//! User-defined declarative providers (`dots.toml` `[providers.<id>]`, ARCHITECTURE
//! section 5.4; spec `provider-contract` "User-defined providers"): go, gem, pipx, nix-env and
//! anything else without writing Rust.
//!
//! Keys: `available`, `bootstrap`, `installed` (`cmd` + `parse`), `version`, `install`,
//! `batch`. Commands run through `/bin/sh -c` for `available`, `installed` and `version`.
//! `install` is split into an argv when it is a plain command line (so `plan` shows exactly
//! `go install github.com/x/y@latest`) and wrapped in `/bin/sh -c` only when it uses shell
//! syntax.
//!
//! Placeholders (`{name}`, `{source}`, `{version}`, `{version|latest}`, `{bin}`, `{names}`,
//! and the facts `{os}`, `{arch}`, `{distro}`, `{home}`, `{user}`, `{hostname}`) are filled from
//! the spec and the facts. Anything else in braces (an awk body, for one) is left alone.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use dots_facts::Facts;
use serde_json::Value;

use crate::{
    Action, Availability, BinEntry, Caps, Cmd, Ctx, Installed, Latest, Op, OpKind, Outcome,
    Provider, ProviderError, Result, Spec, Support,
};

/// The shell a declarative provider's command lines run in.
fn sh_path() -> PathBuf {
    crate::script::platform_shell().0
}

/// How to read the output of the `installed` command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Parse {
    /// One package per line: `name` or `name<whitespace>version`.
    Lines,
    /// `json:<path>`: a dotted path (empty or `.` for the root) to an array or an object.
    /// Arrays hold names or `{name, version}` objects; objects map name to a version string
    /// or to an object with `version`.
    Json(String),
    /// `regex:<re>`: applied to every line, with named groups `name` and `version` (or group 1
    /// and group 2).
    Regex(String),
}

impl Parse {
    pub fn parse_mode(s: &str) -> Result<Parse> {
        match s.trim() {
            "lines" => Ok(Parse::Lines),
            t if t.starts_with("json:") => Ok(Parse::Json(t["json:".len()..].to_string())),
            t if t.starts_with("regex:") => {
                let re = &t["regex:".len()..];
                regex::Regex::new(re).map_err(|e| {
                    ProviderError::Other(format!("invalid parse regex `{re}`: {e}"))
                })?;
                Ok(Parse::Regex(re.to_string()))
            }
            other => Err(ProviderError::Other(format!(
                "unknown parse mode `{other}` (want lines, json:<path> or regex:<re>)"
            ))),
        }
    }
}

/// A `[providers.<id>]` declaration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CustomDef {
    pub id: String,
    /// Command that succeeds when the backend is usable (`command -v go`).
    pub available: Option<String>,
    /// Package that provides the backend; availability becomes `Bootstrap` when
    /// `available` fails.
    pub bootstrap: Option<String>,
    /// Command listing installed packages.
    pub installed_cmd: String,
    pub parse: Parse,
    /// Per-package version command, used for entries whose listing carries no version.
    pub version: Option<String>,
    pub install: String,
    /// One action for all ops; `install` must then contain `{names}`.
    pub batch: bool,
}

impl CustomDef {
    /// Read a table as written in `dots.toml` (as JSON).
    pub fn from_json(id: &str, table: &Value) -> Result<CustomDef> {
        let err = |m: String| ProviderError::Other(format!("providers.{id}: {m}"));
        let obj = table
            .as_object()
            .ok_or_else(|| err("must be a table".into()))?;
        const KEYS: &[&str] = &[
            "available",
            "bootstrap",
            "installed",
            "version",
            "install",
            "batch",
        ];
        if let Some(k) = obj.keys().find(|k| !KEYS.contains(&k.as_str())) {
            return Err(err(format!("unknown key `{k}`")));
        }
        let text = |k: &str| -> Result<Option<String>> {
            match obj.get(k) {
                None => Ok(None),
                Some(Value::String(s)) => Ok(Some(s.clone())),
                Some(_) => Err(err(format!("`{k}` must be a string"))),
            }
        };
        let install = text("install")?.ok_or_else(|| err("`install` is required".into()))?;
        let (installed_cmd, parse) = match obj.get("installed") {
            None => return Err(err("`installed` is required".into())),
            Some(Value::String(s)) => (s.clone(), Parse::Lines),
            Some(Value::Object(o)) => {
                let cmd = o
                    .get("cmd")
                    .and_then(Value::as_str)
                    .ok_or_else(|| err("`installed.cmd` is required".into()))?;
                let parse = match o.get("parse").and_then(Value::as_str) {
                    Some(p) => Parse::parse_mode(p)?,
                    None => Parse::Lines,
                };
                (cmd.to_string(), parse)
            }
            Some(_) => return Err(err("`installed` must be a string or a table".into())),
        };
        let batch = match obj.get("batch") {
            None => false,
            Some(Value::Bool(b)) => *b,
            Some(_) => return Err(err("`batch` must be a boolean".into())),
        };
        if batch && !install.contains("{names}") {
            return Err(err("`batch = true` needs `{names}` in `install`".into()));
        }
        Ok(CustomDef {
            id: id.to_string(),
            available: text("available")?,
            bootstrap: text("bootstrap")?,
            installed_cmd,
            parse,
            version: text("version")?,
            install,
            batch,
        })
    }
}

/// What `{source}` (and each entry of `{names}`) stands for: the `source` key of the package's
/// `[providers.<id>]` block (it reaches the spec as the `source` option), else the spec-level
/// source, else the native name.
fn source_of(spec: &Spec) -> &str {
    spec.get("source")
        .or(spec.source.as_deref())
        .unwrap_or(&spec.name)
}

/// Fill placeholders. `names` is only set for batched installs.
fn expand(tpl: &str, vars: &BTreeMap<&str, String>, version: Option<&str>) -> Result<String> {
    let mut out = String::new();
    let mut rest = tpl;
    while let Some(i) = rest.find('{') {
        out.push_str(&rest[..i]);
        let after = &rest[i + 1..];
        let Some(j) = after.find('}') else {
            out.push_str(&rest[i..]);
            rest = "";
            break;
        };
        let body = &after[..j];
        let (key, default) = match body.split_once('|') {
            Some((k, d)) => (k.trim(), Some(d.trim())),
            None => (body.trim(), None),
        };
        let value = if key == "version" {
            match (version, default) {
                (Some(v), _) => Some(v.to_string()),
                (None, Some(d)) => Some(d.to_string()),
                (None, None) => {
                    return Err(ProviderError::Other(format!(
                        "`{tpl}` needs a version, but none was given (use `{{version|latest}}` to default)"
                    )));
                }
            }
        } else {
            vars.get(key).cloned()
        };
        match value {
            Some(v) => {
                out.push_str(&v);
                rest = &after[j + 1..];
            }
            None => {
                // Not ours: keep the brace and move on.
                out.push('{');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    Ok(out)
}

fn shell_quote(s: &str) -> String {
    if !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || "@%+=:,./-_".contains(c))
    {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
}

/// A plain command line becomes an argv; anything with shell syntax runs under `sh -c`.
fn to_argv(line: &str) -> Vec<String> {
    let shelly = |c: char| "|&;<>()$`\\\"'*?[]{}~#!\n".contains(c);
    if line.chars().any(shelly) {
        let (sh, flags) = crate::script::platform_shell();
        let mut v = vec![sh.to_string_lossy().into_owned()];
        v.extend(flags.iter().map(|f| f.to_string()));
        v.push(line.to_string());
        v
    } else {
        line.split_whitespace().map(String::from).collect()
    }
}

pub struct CustomProvider {
    def: CustomDef,
    id: &'static str,
    facts: Facts,
    /// The context `available` is probed with when the resolver asks (it has no context of its
    /// own), and the answer, remembered for the run.
    probe: Option<Ctx>,
    probed: std::sync::Mutex<Option<Availability>>,
}

impl CustomProvider {
    /// The provider id comes from user config, so it is leaked once per provider to satisfy
    /// `Provider::id() -> &'static str`; providers live for the whole run.
    pub fn new(def: CustomDef, facts: &Facts) -> Self {
        let id: &'static str = Box::leak(def.id.clone().into_boxed_str());
        CustomProvider {
            def,
            id,
            facts: facts.clone(),
            probe: None,
            probed: std::sync::Mutex::new(None),
        }
    }

    /// Let [`Provider::availability`] run the `available` command with `ctx` (its `PATH` and
    /// runner), so plan-time resolution sees the real answer: `Ready`, `Bootstrap` (the tool
    /// comes from another package in the same run) or `Unsupported`. Without it, only what the
    /// caller recorded in the facts counts.
    pub fn with_probe(mut self, ctx: Ctx) -> Self {
        self.probe = Some(ctx);
        self
    }

    /// The probed availability, run once per provider.
    fn probed_availability(&self, ctx: &Ctx) -> Availability {
        if let Ok(g) = self.probed.lock()
            && let Some(a) = g.as_ref()
        {
            return a.clone();
        }
        let a = self.availability_in(ctx, &self.facts);
        if let Ok(mut g) = self.probed.lock() {
            *g = Some(a.clone());
        }
        a
    }

    fn fact_vars(&self) -> BTreeMap<&'static str, String> {
        let f = &self.facts;
        BTreeMap::from([
            ("os", f.os.clone()),
            ("arch", f.arch.clone()),
            ("distro", f.distro.clone().unwrap_or_default()),
            ("home", f.home.clone()),
            ("user", f.user.clone()),
            ("hostname", f.hostname.clone()),
        ])
    }

    fn op_vars(&self, spec: &Spec) -> BTreeMap<&'static str, String> {
        let mut v = self.fact_vars();
        // `name` is the provider-native name (the block's `name`, else the package's).
        v.insert("name", spec.name.clone());
        v.insert("bin", spec.name.clone());
        v.insert("source", source_of(spec).to_string());
        v
    }

    fn sh(&self, ctx: &Ctx, line: &str) -> Cmd {
        let (sh, flags) = crate::script::platform_shell();
        Cmd::new(sh)
            .args(flags.iter().copied())
            .arg(line)
            .envs(&ctx.env)
    }

    fn log_path(&self, ctx: &Ctx, action: &Action) -> PathBuf {
        let who = match (self.def.batch, action.ops.as_slice()) {
            (false, [op]) => op.spec.name.replace(['/', '\\'], "_"),
            _ => self.id.to_string(),
        };
        let what = match action.kind {
            OpKind::Install => "install",
            OpKind::Upgrade => "upgrade",
            _ => "action",
        };
        ctx.log_dir.join(format!("{who}.{what}.log"))
    }

    fn parse_installed(&self, stdout: &str) -> Result<Vec<(String, Option<String>)>> {
        let mut found: Vec<(String, Option<String>)> = Vec::new();
        match &self.def.parse {
            Parse::Lines => {
                for l in stdout.lines().map(str::trim).filter(|l| !l.is_empty()) {
                    let mut it = l.split_whitespace();
                    let name = it.next().unwrap_or_default().to_string();
                    found.push((name, it.next().map(String::from)));
                }
            }
            Parse::Regex(re) => {
                let re = regex::Regex::new(re)
                    .map_err(|e| ProviderError::Other(format!("invalid regex: {e}")))?;
                let named = re.capture_names().flatten().any(|n| n == "name");
                for line in stdout.lines() {
                    if let Some(c) = re.captures(line) {
                        let (n, v) = if named {
                            (c.name("name"), c.name("version"))
                        } else {
                            (c.get(1), c.get(2))
                        };
                        if let Some(n) = n {
                            found.push((n.as_str().to_string(), v.map(|m| m.as_str().to_string())));
                        }
                    }
                }
            }
            Parse::Json(path) => {
                let root: Value = serde_json::from_str(stdout).map_err(|e| {
                    ProviderError::Other(format!("`{}` printed invalid JSON: {e}", self.def.id))
                })?;
                let mut cur = &root;
                for seg in path.split('.').filter(|s| !s.is_empty()) {
                    cur = cur.get(seg).ok_or_else(|| {
                        ProviderError::Other(format!("json path `{path}`: no `{seg}` in output"))
                    })?;
                }
                let version_of = |v: &Value| match v {
                    Value::String(s) => Some(s.clone()),
                    Value::Object(o) => o.get("version").and_then(Value::as_str).map(String::from),
                    _ => None,
                };
                match cur {
                    Value::Array(items) => {
                        for it in items {
                            match it {
                                Value::String(s) => found.push((s.clone(), None)),
                                Value::Object(o) => {
                                    if let Some(n) = o.get("name").and_then(Value::as_str) {
                                        found.push((n.to_string(), version_of(it)));
                                    }
                                }
                                _ => {}
                            }
                        }
                    }
                    Value::Object(o) => {
                        for (k, v) in o {
                            found.push((k.clone(), version_of(v)));
                        }
                    }
                    _ => {
                        return Err(ProviderError::Other(format!(
                            "json path `{path}` is neither an array nor an object"
                        )));
                    }
                }
            }
        }
        Ok(found)
    }
}

impl Provider for CustomProvider {
    fn id(&self) -> &'static str {
        self.id
    }

    fn caps(&self) -> Caps {
        Caps {
            batch_install: self.def.batch,
            exact_version: if self.def.install.contains("{version") {
                Support::Yes
            } else {
                Support::No
            },
            pin: false,
            needs_root: false,
            uninstall: false,
            exclusive_lock: false,
        }
    }

    /// Without a context nothing can be probed: use what the caller recorded in the facts.
    fn availability(&self, facts: &Facts) -> Availability {
        if let Some(ctx) = &self.probe {
            return self.probed_availability(ctx);
        }
        if facts.providers.get(self.id) == Some(&true) {
            Availability::Ready {
                bin_path: sh_path(),
            }
        } else {
            Availability::Unsupported
        }
    }

    fn availability_in(&self, ctx: &Ctx, _facts: &Facts) -> Availability {
        let Some(cmd) = &self.def.available else {
            return Availability::Ready {
                bin_path: sh_path(),
            };
        };
        match ctx.runner.run(&self.sh(ctx, cmd)) {
            Ok(o) if o.success() => {
                let bin = o.stdout.lines().next().map(str::trim).unwrap_or("");
                let bin_path = if Path::new(bin).is_absolute() {
                    PathBuf::from(bin)
                } else {
                    sh_path()
                };
                Availability::Ready { bin_path }
            }
            _ => match &self.def.bootstrap {
                Some(pkg) => Availability::Bootstrap { pkg: pkg.clone() },
                None => Availability::Unsupported,
            },
        }
    }

    fn installed(&self, ctx: &Ctx) -> Result<BTreeMap<String, Installed>> {
        // A backend that is not there yet (it is installed by a package earlier in the run)
        // has nothing installed; running its listing command would only fail.
        if self.probe.is_some()
            && self.def.available.is_some()
            && !matches!(self.probed_availability(ctx), Availability::Ready { .. })
        {
            return Ok(BTreeMap::new());
        }
        let out = ctx.runner.run(&self.sh(ctx, &self.def.installed_cmd))?;
        if out.timed_out {
            return Err(ProviderError::Coded {
                code: "timeout",
                message: format!("`{}` installed query timed out", self.def.id),
                log: None,
            });
        }
        // A backend that has nothing installed yet may exit non-zero with empty output.
        if !out.success() && out.stdout.trim().is_empty() {
            return Ok(BTreeMap::new());
        }
        let mut map = BTreeMap::new();
        for (name, version) in self.parse_installed(&out.stdout)? {
            let version = match (version, &self.def.version) {
                (Some(v), _) => v,
                (None, Some(tpl)) => {
                    let mut vars = self.fact_vars();
                    vars.insert("name", name.clone());
                    vars.insert("bin", name.clone());
                    // The listing only knows names: that is all a `version` command gets.
                    vars.insert("source", name.clone());
                    let line = expand(tpl, &vars, None)?;
                    let o = ctx.runner.run(&self.sh(ctx, &line))?;
                    o.stdout
                        .lines()
                        .map(str::trim)
                        .find(|l| !l.is_empty())
                        .unwrap_or("unknown")
                        .to_string()
                }
                (None, None) => "unknown".to_string(),
            };
            map.insert(
                name,
                Installed {
                    version,
                    pinned: false,
                    source: None,
                },
            );
        }
        Ok(map)
    }

    fn latest(&self, _ctx: &Ctx, _specs: &[Spec]) -> Result<BTreeMap<String, Latest>> {
        Ok(BTreeMap::new())
    }

    fn plan(&self, ops: &[Op]) -> Result<Vec<Action>> {
        for op in ops {
            match op.kind {
                OpKind::Install | OpKind::Upgrade => {
                    if op.version.is_some() && !self.def.install.contains("{version") {
                        return Err(ProviderError::Other(format!(
                            "`{}` cannot install an exact version: its `install` has no `{{version}}`",
                            self.def.id
                        )));
                    }
                }
                OpKind::Remove => {
                    return Err(ProviderError::Other(format!(
                        "cannot uninstall `{}`: custom provider `{}` declares no uninstall",
                        op.spec.name, self.def.id
                    )));
                }
                OpKind::Pin | OpKind::Unpin => {
                    return Err(ProviderError::Other(format!(
                        "custom provider `{}` cannot pin",
                        self.def.id
                    )));
                }
            }
        }
        if ops.is_empty() {
            return Ok(Vec::new());
        }
        let action = |ops: Vec<Op>, line: String| Action {
            kind: ops[0].kind,
            ops,
            argv: to_argv(&line),
            needs_root: false,
        };
        if self.def.batch {
            let mut vars = self.fact_vars();
            let names: Vec<String> = ops
                .iter()
                .map(|o| shell_quote(source_of(&o.spec)))
                .collect();
            vars.insert("names", names.join(" "));
            let line = expand(&self.def.install, &vars, None)?;
            Ok(vec![action(ops.to_vec(), line)])
        } else {
            ops.iter()
                .map(|op| {
                    let line = expand(
                        &self.def.install,
                        &self.op_vars(&op.spec),
                        op.version.as_deref(),
                    )?;
                    Ok(action(vec![op.clone()], line))
                })
                .collect()
        }
    }

    fn exec(&self, ctx: &Ctx, action: &Action) -> Result<Outcome> {
        if ctx.dry_run {
            return Ok(Outcome {
                success: true,
                log: None,
            });
        }
        let log = self.log_path(ctx, action);
        fs::create_dir_all(&ctx.log_dir)
            .map_err(|e| ProviderError::Other(format!("cannot create log dir: {e}")))?;
        let cmd = Cmd::from_argv(&action.argv)
            .ok_or_else(|| ProviderError::Other("empty argv".into()))?
            .envs(&ctx.env);
        let out = ctx.runner.run(&cmd)?;
        let text = format!(
            "$ {}\n{}{}[dots] {}\n",
            cmd.line(),
            out.stdout,
            out.stderr,
            if out.timed_out {
                "killed: timeout".to_string()
            } else {
                format!("exit status {}", out.status)
            }
        );
        fs::write(&log, text)
            .map_err(|e| ProviderError::Other(format!("cannot write log: {e}")))?;
        if out.timed_out {
            return Err(ProviderError::Coded {
                code: "timeout",
                message: format!("`{}` exceeded its timeout", cmd.line()),
                log: Some(log),
            });
        }
        Ok(Outcome {
            success: out.success(),
            log: Some(log),
        })
    }

    fn bins(&self, _ctx: &Ctx, _installed: &BTreeMap<String, Installed>) -> Result<Vec<BinEntry>> {
        // Declared `bins` of the package are the engine's job; no key in the declaration.
        Ok(Vec::new())
    }
}
