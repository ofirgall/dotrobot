//! The `script` provider: the escape hatch, kept honest (ARCHITECTURE section 5.3, spec
//! `provider-contract` "Script provider contract", "Script runtime environment",
//! "Script timeout").
//!
//! One [`ScriptProvider`] is built per package's `[providers.script]` table. Every command it
//! runs gets the same runtime contract: the package dir as working directory, stdin at
//! `/dev/null` unless `interactive`, `PATH` extended with the provider bins, the `DOTS_*`
//! variables, a timeout that kills the whole process tree, and output in
//! `<log_dir>/<pkg>.<action>.log`. After `install`/`upgrade` exits 0, `check` must pass;
//! otherwise the action fails with `check-failed-after-install`.
//!
//! `exec` runs `Action.argv` and then `check` (two commands); it runs nothing under
//! `ctx.dry_run`.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use dots_facts::Facts;

use crate::{
    Action, Availability, BinEntry, Caps, Cmd, Ctx, DEFAULT_TIMEOUT, Installed, Latest, Op, OpKind,
    Outcome, Output, Provider, ProviderError, Result, Spec, Stdin, Support,
};

/// The `[providers.script]` keys.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScriptConfig {
    /// REQUIRED. Exit 0 means installed.
    pub check: Option<String>,
    /// REQUIRED unless `versionless`. Prints the installed version.
    pub version: Option<String>,
    pub latest: Option<String>,
    /// REQUIRED.
    pub install: Option<String>,
    /// Defaults to `install`.
    pub upgrade: Option<String>,
    /// Absent means "cannot uninstall".
    pub uninstall: Option<String>,
    /// `bash`, `sh` or `pwsh`; default per OS.
    pub shell: Option<String>,
    pub needs_root: bool,
    pub interactive: bool,
    pub versionless: bool,
    /// `20m`, `30s`, `1h30m`, `500ms`.
    pub timeout: Option<String>,
    /// Declared executables the package puts in `DOTS_BIN`.
    pub bins: Vec<String>,
}

impl ScriptConfig {
    /// Enforce the required keys and parse the timeout.
    pub fn validate(&self) -> Result<()> {
        if self.check.is_none() {
            return Err(ProviderError::Other(
                "script provider requires `check`".into(),
            ));
        }
        if self.install.is_none() {
            return Err(ProviderError::Other(
                "script provider requires `install`".into(),
            ));
        }
        if self.version.is_none() && !self.versionless {
            return Err(ProviderError::Other(
                "script provider requires `version` unless `versionless = true`".into(),
            ));
        }
        if let Some(s) = &self.shell
            && !matches!(s.as_str(), "bash" | "sh" | "pwsh")
        {
            return Err(ProviderError::Other(format!(
                "unknown shell `{s}` (want bash, sh or pwsh)"
            )));
        }
        self.timeout()?;
        Ok(())
    }

    /// The configured timeout, or the 20 minute default.
    pub fn timeout(&self) -> Result<Duration> {
        match &self.timeout {
            Some(t) => parse_duration(t),
            None => Ok(DEFAULT_TIMEOUT),
        }
    }
}

/// Parse `20m`, `30s`, `1h30m`, `500ms`, or a bare number of seconds.
pub fn parse_duration(s: &str) -> Result<Duration> {
    let bad = || ProviderError::Other(format!("invalid timeout `{s}` (use e.g. 30s, 20m, 1h)"));
    let s = s.trim();
    if s.is_empty() {
        return Err(bad());
    }
    let mut total = Duration::ZERO;
    let mut rest = s;
    while !rest.is_empty() {
        let digits = rest.chars().take_while(char::is_ascii_digit).count();
        if digits == 0 {
            return Err(bad());
        }
        let n: u64 = rest[..digits].parse().map_err(|_| bad())?;
        rest = &rest[digits..];
        let unit = rest.chars().take_while(char::is_ascii_alphabetic).count();
        let (u, tail) = rest.split_at(unit);
        rest = tail;
        total += match u {
            "ms" => Duration::from_millis(n),
            "s" | "" => Duration::from_secs(n),
            "m" => Duration::from_secs(n * 60),
            "h" => Duration::from_secs(n * 3600),
            _ => return Err(bad()),
        };
    }
    Ok(total)
}

/// The per-package inputs of the `DOTS_*` variables.
#[derive(Debug, Clone, Default)]
pub struct ScriptEnv {
    pub pkg_dir: PathBuf,
    /// `DOTS_ROOT`: the active repo root.
    pub root: PathBuf,
    /// `DOTS_STATE`: the state dir.
    pub state: PathBuf,
    /// `DOTS_BIN` (`~/.local/bin`).
    pub bin_dir: PathBuf,
    /// `DOTS_LIB`: shared helpers.
    pub lib_dir: PathBuf,
    pub tags: Vec<String>,
    /// Resolved settings by name; exposed as `DOTS_SETTING_<NAME>`.
    pub settings: BTreeMap<String, String>,
    /// Directories placed in front of `PATH`: the generated env plus provider bins.
    pub path_prefix: Vec<PathBuf>,
}

/// `font-size` -> `FONT_SIZE`.
fn env_name(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect()
}

fn flag(b: bool) -> String {
    if b { "1" } else { "0" }.to_string()
}

pub struct ScriptProvider {
    pkg: String,
    cfg: ScriptConfig,
    env: ScriptEnv,
    facts: Facts,
}

impl ScriptProvider {
    /// Build the provider for one package. Fails when required keys are missing.
    pub fn new(pkg: &str, cfg: ScriptConfig, env: ScriptEnv, facts: &Facts) -> Result<Self> {
        cfg.validate()?;
        Ok(ScriptProvider {
            pkg: pkg.to_string(),
            cfg,
            env,
            facts: facts.clone(),
        })
    }

    pub fn package(&self) -> &str {
        &self.pkg
    }

    /// The `DOTS_*` variables plus `PATH`, on top of `ctx.env`.
    pub fn script_env(&self, ctx: &Ctx, version_wanted: Option<&str>) -> BTreeMap<String, String> {
        let mut e = ctx.env.clone();
        let f = &self.facts;
        let p = |x: &Path| x.to_string_lossy().into_owned();
        let mut set = |k: &str, v: String| {
            e.insert(k.to_string(), v);
        };
        set("DOTS_OS", f.os.clone());
        set("DOTS_ARCH", f.arch.clone());
        set("DOTS_DISTRO", f.distro.clone().unwrap_or_default());
        set("DOTS_WSL", flag(f.is_wsl));
        set("DOTS_SUDO", flag(f.has_sudo));
        set("DOTS_TAGS", self.env.tags.join(" "));
        for t in &self.env.tags {
            set(&format!("DOTS_TAG_{}", env_name(t)), "1".into());
        }
        set("DOTS_PKG", self.pkg.clone());
        set("DOTS_PKG_DIR", p(&self.env.pkg_dir));
        set("DOTS_ROOT", p(&self.env.root));
        set("DOTS_STATE", p(&self.env.state));
        set("DOTS_BIN", p(&self.env.bin_dir));
        set("DOTS_LIB", p(&self.env.lib_dir));
        set("DOTS_VERSION_WANTED", version_wanted.unwrap_or("").into());
        set("DOTS_DRY_RUN", flag(ctx.dry_run));
        for (k, v) in &self.env.settings {
            set(&format!("DOTS_SETTING_{}", env_name(k)), v.clone());
        }
        if !self.env.path_prefix.is_empty() {
            let mut parts: Vec<String> = self.env.path_prefix.iter().map(|d| p(d)).collect();
            if let Some(old) = ctx.env.get("PATH").filter(|s| !s.is_empty()) {
                parts.push(old.clone());
            }
            set("PATH", parts.join(":"));
        }
        e
    }

    fn shell_kind(&self) -> &str {
        match self.cfg.shell.as_deref() {
            Some(s) => s,
            None if self.facts.os == "windows" => "pwsh",
            None => "bash",
        }
    }

    /// Absolute path of the shell, from known locations (never via `PATH`).
    fn shell_path(&self) -> Option<PathBuf> {
        let kind = self.shell_kind();
        let cands: &[&str] = match kind {
            "bash" => &[
                "/bin/bash",
                "/usr/bin/bash",
                "/opt/homebrew/bin/bash",
                "/usr/local/bin/bash",
            ],
            "sh" => &["/bin/sh", "/usr/bin/sh"],
            _ => &[
                "/usr/local/bin/pwsh",
                "/opt/homebrew/bin/pwsh",
                "/usr/bin/pwsh",
                "C:\\Program Files\\PowerShell\\7\\pwsh.exe",
            ],
        };
        let found = cands.iter().map(PathBuf::from).find(|c| c.is_file());
        // A box without bash still runs a script written for plain sh.
        found.or_else(|| {
            (kind == "bash")
                .then(|| ["/bin/sh", "/usr/bin/sh"].map(PathBuf::from))
                .and_then(|c| c.into_iter().find(|p| p.is_file()))
        })
    }

    fn shell_argv(&self, shell: &Path, text: &str) -> Vec<String> {
        let (flag, pre) = match self.shell_kind() {
            "pwsh" => (
                vec!["-NoProfile", "-NonInteractive", "-Command"],
                "$ErrorActionPreference = 'Stop'\n",
            ),
            "sh" => (vec!["-c"], "set -eu\n"),
            _ => (vec!["-c"], "set -euo pipefail\n"),
        };
        let mut v = vec![shell.to_string_lossy().into_owned()];
        v.extend(flag.into_iter().map(String::from));
        v.push(format!("{pre}{text}"));
        v
    }

    fn cmd(&self, ctx: &Ctx, argv: &[String], version_wanted: Option<&str>) -> Result<Cmd> {
        let mut c =
            Cmd::from_argv(argv).ok_or_else(|| ProviderError::Other("empty argv".into()))?;
        c.env = self.script_env(ctx, version_wanted);
        c.cwd = Some(self.env.pkg_dir.clone());
        c.stdin = if self.cfg.interactive {
            Stdin::Inherit
        } else {
            Stdin::Null
        };
        c.timeout = Some(self.cfg.timeout()?);
        Ok(c)
    }

    fn command_for(&self, kind: OpKind) -> Option<&str> {
        let c = &self.cfg;
        match kind {
            OpKind::Install => c.install.as_deref(),
            OpKind::Upgrade => c.upgrade.as_deref().or(c.install.as_deref()),
            OpKind::Remove => c.uninstall.as_deref(),
            OpKind::Pin | OpKind::Unpin => None,
        }
    }

    fn log_path(&self, ctx: &Ctx, kind: OpKind) -> PathBuf {
        let action = match kind {
            OpKind::Install => "install",
            OpKind::Upgrade => "upgrade",
            OpKind::Remove => "uninstall",
            OpKind::Pin => "pin",
            OpKind::Unpin => "unpin",
        };
        ctx.log_dir.join(format!(
            "{}.{action}.log",
            self.pkg.replace(['/', '\\'], "_")
        ))
    }

    /// Run `check` (not via `installed`, which also asks for the version).
    fn run_check(&self, ctx: &Ctx, shell: &Path, version_wanted: Option<&str>) -> Result<Output> {
        let text = self.cfg.check.as_deref().unwrap_or("false");
        ctx.runner
            .run(&self.cmd(ctx, &self.shell_argv(shell, text), version_wanted)?)
    }
}

fn append_log(path: &Path, header: &str, out: &Output) {
    let mut s = String::new();
    s.push_str(&format!("$ {header}\n"));
    s.push_str(&out.stdout);
    if !out.stdout.is_empty() && !out.stdout.ends_with('\n') {
        s.push('\n');
    }
    s.push_str(&out.stderr);
    if !out.stderr.is_empty() && !out.stderr.ends_with('\n') {
        s.push('\n');
    }
    if out.timed_out {
        s.push_str("[dots] killed: timeout\n");
    } else {
        s.push_str(&format!("[dots] exit status {}\n", out.status));
    }
    let old = fs::read_to_string(path).unwrap_or_default();
    let _ = fs::write(path, format!("{old}{s}"));
}

/// Display form of the script text for logs: its first line is the `set` prelude.
fn short(argv: &[String]) -> String {
    argv.iter()
        .skip(1)
        .map(String::as_str)
        .collect::<Vec<_>>()
        .join(" ")
        .replace('\n', "; ")
}

const INSTALLED: &str = "__dots_installed__";

impl Provider for ScriptProvider {
    fn id(&self) -> &'static str {
        "script"
    }

    fn caps(&self) -> Caps {
        Caps {
            batch_install: false,
            exact_version: Support::Limited,
            pin: false,
            needs_root: self.cfg.needs_root,
            uninstall: self.cfg.uninstall.is_some(),
            exclusive_lock: self.cfg.interactive,
        }
    }

    fn availability(&self, _facts: &Facts) -> Availability {
        match self.shell_path() {
            Some(bin_path) => Availability::Ready { bin_path },
            None => Availability::Unsupported,
        }
    }

    fn installed(&self, ctx: &Ctx) -> Result<BTreeMap<String, Installed>> {
        let shell = self
            .shell_path()
            .ok_or_else(|| ProviderError::Unavailable {
                provider: "script".into(),
                reason: format!("no `{}` found", self.shell_kind()),
            })?;
        let check = self.cfg.check.as_deref().unwrap_or("false");
        let version = self
            .cfg
            .version
            .as_deref()
            .filter(|_| !self.cfg.versionless);
        let (installed, version_out) = if self.shell_kind() == "pwsh" {
            let c = self.run_check(ctx, &shell, None)?;
            if c.timed_out {
                return Err(self.timeout_error(ctx, OpKind::Install));
            }
            let v = match (c.success(), version) {
                (true, Some(v)) => Some(
                    ctx.runner
                        .run(&self.cmd(ctx, &self.shell_argv(&shell, v), None)?)?
                        .stdout,
                ),
                _ => None,
            };
            (c.success(), v.unwrap_or_default())
        } else {
            // One backend call for both: check, then version when it passes.
            let mut text =
                format!("if (\n{check}\n) >/dev/null 2>&1; then printf '%s\\n' {INSTALLED}\n");
            if let Some(v) = version {
                text.push_str(&format!("( {v}\n) 2>/dev/null || true\n"));
            }
            text.push_str("fi\n");
            let argv = vec![shell.to_string_lossy().into_owned(), "-c".to_string(), text];
            let out = ctx.runner.run(&self.cmd(ctx, &argv, None)?)?;
            if out.timed_out {
                return Err(self.timeout_error(ctx, OpKind::Install));
            }
            if !out.success() {
                return Err(ProviderError::CommandFailed {
                    command: format!("script check for `{}`", self.pkg),
                    status: out.status,
                });
            }
            let mut lines = out.stdout.lines();
            let first = lines.by_ref().map(str::trim).find(|l| !l.is_empty());
            (
                first == Some(INSTALLED),
                lines.collect::<Vec<_>>().join("\n"),
            )
        };
        let mut map = BTreeMap::new();
        if installed {
            let v = version_out.lines().map(str::trim).find(|l| !l.is_empty());
            map.insert(
                self.pkg.clone(),
                Installed {
                    // `versionless`, or a version command that printed nothing: installed
                    // but unverified.
                    version: v.unwrap_or("unknown").to_string(),
                    pinned: false,
                    source: None,
                },
            );
        }
        Ok(map)
    }

    fn latest(&self, ctx: &Ctx, specs: &[Spec]) -> Result<BTreeMap<String, Latest>> {
        let mut map = BTreeMap::new();
        let (Some(cmd), Some(shell)) = (self.cfg.latest.as_deref(), self.shell_path()) else {
            return Ok(map);
        };
        if !specs.iter().any(|s| s.name == self.pkg) {
            return Ok(map);
        }
        let out = ctx
            .runner
            .run(&self.cmd(ctx, &self.shell_argv(&shell, cmd), None)?)?;
        if out.timed_out {
            return Err(self.timeout_error(ctx, OpKind::Upgrade));
        }
        if out.success()
            && let Some(v) = out.stdout.lines().map(str::trim).find(|l| !l.is_empty())
        {
            map.insert(
                self.pkg.clone(),
                Latest {
                    version: v.to_string(),
                },
            );
        }
        Ok(map)
    }

    fn plan(&self, ops: &[Op]) -> Result<Vec<Action>> {
        let shell = self
            .shell_path()
            .ok_or_else(|| ProviderError::Unavailable {
                provider: "script".into(),
                reason: format!("no `{}` found", self.shell_kind()),
            })?;
        ops.iter()
            .map(|op| {
                if op.spec.name != self.pkg {
                    return Err(ProviderError::Other(format!(
                        "script provider for `{}` cannot act on `{}`",
                        self.pkg, op.spec.name
                    )));
                }
                let Some(text) = self.command_for(op.kind) else {
                    return Err(ProviderError::Other(match op.kind {
                        OpKind::Remove => format!(
                            "cannot uninstall `{}`: its script provider declares no `uninstall`",
                            self.pkg
                        ),
                        _ => format!("script packages cannot {:?} (no such operation)", op.kind),
                    }));
                };
                Ok(Action {
                    kind: op.kind,
                    ops: vec![op.clone()],
                    argv: self.shell_argv(&shell, text),
                    needs_root: self.cfg.needs_root,
                })
            })
            .collect()
    }

    fn exec(&self, ctx: &Ctx, action: &Action) -> Result<Outcome> {
        if ctx.dry_run {
            return Ok(Outcome {
                success: true,
                log: None,
            });
        }
        let log = self.log_path(ctx, action.kind);
        fs::create_dir_all(&ctx.log_dir)
            .map_err(|e| ProviderError::Other(format!("cannot create log dir: {e}")))?;
        let _ = fs::write(&log, "");
        let wanted = action.ops.first().and_then(|o| o.version.as_deref());
        let cmd = self.cmd(ctx, &action.argv, wanted)?;
        let out = ctx.runner.run(&cmd)?;
        append_log(&log, &short(&action.argv), &out);
        if out.timed_out {
            return Err(self.timeout_error(ctx, action.kind));
        }
        if !out.success() {
            return Ok(Outcome {
                success: false,
                log: Some(log),
            });
        }
        if matches!(action.kind, OpKind::Install | OpKind::Upgrade) {
            let shell = PathBuf::from(&action.argv[0]);
            let check = self.run_check(ctx, &shell, wanted)?;
            append_log(
                &log,
                &format!("check: {}", self.cfg.check.as_deref().unwrap_or("")),
                &check,
            );
            if check.timed_out {
                return Err(self.timeout_error(ctx, action.kind));
            }
            if !check.success() {
                return Err(ProviderError::Coded {
                    code: "check-failed-after-install",
                    message: format!(
                        "install of `{}` succeeded but its check fails (log: {})",
                        self.pkg,
                        log.display()
                    ),
                    log: Some(log),
                });
            }
        }
        Ok(Outcome {
            success: true,
            log: Some(log),
        })
    }

    fn bins(&self, _ctx: &Ctx, installed: &BTreeMap<String, Installed>) -> Result<Vec<BinEntry>> {
        if !installed.contains_key(&self.pkg) {
            return Ok(Vec::new());
        }
        Ok(self
            .cfg
            .bins
            .iter()
            .map(|b| BinEntry {
                package: self.pkg.clone(),
                bin: b.clone(),
                path: self.env.bin_dir.join(b),
            })
            .collect())
    }
}

impl ScriptProvider {
    fn timeout_error(&self, ctx: &Ctx, kind: OpKind) -> ProviderError {
        let t = self.cfg.timeout.clone().unwrap_or_else(|| "20m".into());
        let log = self.log_path(ctx, kind);
        ProviderError::Coded {
            code: "timeout",
            message: format!(
                "`{}` {} exceeded its timeout of {t}",
                self.pkg,
                match kind {
                    OpKind::Remove => "uninstall",
                    OpKind::Upgrade => "upgrade",
                    _ => "install",
                }
            ),
            log: log.is_file().then_some(log),
        }
    }
}
