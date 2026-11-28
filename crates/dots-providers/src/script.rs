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
use std::sync::{Arc, Mutex};
use std::time::Duration;

use dots_facts::Facts;
use sha2::{Digest, Sha256};

use crate::{
    Action, Availability, BinEntry, Caps, Cmd, Ctx, DEFAULT_TIMEOUT, Installed, Latest, Op, OpKind,
    Outcome, Output, Provider, ProviderError, Result, Spec, Stdin, Support,
};

/// `fetch = { url, sha256 }`: a file dots downloads, verifies and hands to `install` as
/// `{fetched}`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FetchSpec {
    pub url: String,
    /// The author's pin (`sha256:` prefix allowed). Without it, the lock's pin or the first
    /// download decides.
    pub sha256: Option<String>,
}

/// The placeholder in `install`/`upgrade` for the verified download.
pub const FETCHED: &str = "{fetched}";

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
    /// Serialization key: script packages with the same key never run concurrently.
    pub lock: Option<String>,
    /// Declared executables the package puts in `DOTS_BIN`.
    pub bins: Vec<String>,
    pub fetch: Option<FetchSpec>,
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
        if let Some(f) = &self.fetch {
            if !crate::fetch::valid_url(&f.url) {
                return Err(ProviderError::Other(format!(
                    "script `fetch` url `{}` must be http(s)",
                    f.url
                )));
            }
            if let Some(h) = &f.sha256
                && normalize_hash(h).is_none()
            {
                return Err(ProviderError::Other(format!(
                    "script `fetch` sha256 `{h}` is not a 64-digit hex hash"
                )));
            }
            if !self.install.as_deref().unwrap_or("").contains(FETCHED) {
                return Err(ProviderError::Other(
                    "script `fetch` needs `{fetched}` in `install` (for example `sh {fetched}`)"
                        .into(),
                ));
            }
        }
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

/// The first line of `text` that pipes a download into a shell (`curl ... | sh`,
/// `bash -c "$(curl ...)"`, `sh <(curl ...)`), trimmed. These run whatever the server sends
/// next; `fetch = { url, sha256 }` pins the installer instead.
pub fn unpinned_curl_pipe(text: &str) -> Option<String> {
    use std::sync::OnceLock;
    static RES: OnceLock<[regex::Regex; 3]> = OnceLock::new();
    let res = RES.get_or_init(|| {
        [
            r"\b(curl|wget)\b[^\n]*\|\s*(sudo\s+(-\S+\s+)*)?(env\s+\S+=\S+\s+)*(ba|z|da|k)?sh\b",
            r#"\b(ba|z|da|k)?sh\s+(-\w+\s+)*-c\s+["']?\$\(\s*(curl|wget)\b"#,
            r"\b(ba|z|da|k)?sh\s+<\(\s*(curl|wget)\b",
        ]
        .map(|r| regex::Regex::new(r).expect("static regex"))
    });
    text.lines()
        .map(str::trim)
        .filter(|l| !l.starts_with('#'))
        .find(|l| res.iter().any(|r| r.is_match(l)))
        .map(str::to_string)
}

/// PowerShell on Windows, from known locations (never via `PATH`): PowerShell 7 installed
/// machine-wide, the Store/winget app-execution alias in `WindowsApps`, then the Windows
/// PowerShell 5.1 that ships with the OS (it takes the same `-NoProfile -NonInteractive
/// -Command` flags).
#[cfg(windows)]
pub fn windows_pwsh() -> Option<PathBuf> {
    let mut c =
        crate::cargo::windows_dirs(&["ProgramFiles", "ProgramW6432"], r"PowerShell\7\pwsh.exe");
    c.extend(crate::cargo::windows_dirs(
        &["LOCALAPPDATA"],
        r"Microsoft\WindowsApps\pwsh.exe",
    ));
    c.extend(crate::cargo::windows_dirs(
        &["SystemRoot"],
        r"System32\WindowsPowerShell\v1.0\powershell.exe",
    ));
    c.into_iter().find(|p| p.is_file())
}

/// `bash` or `sh` on Windows: the ones Git for Windows installs (`shell = "bash"` in a
/// package runs its scripts there; the default shell on Windows is PowerShell).
#[cfg(windows)]
fn windows_git_shell(kind: &str) -> Option<PathBuf> {
    let exe = format!("{kind}.exe");
    let mut c = Vec::new();
    for sub in [r"Git\bin", r"Git\usr\bin"] {
        c.extend(crate::cargo::windows_dirs(
            &["ProgramFiles", "ProgramW6432"],
            &format!(r"{sub}\{exe}"),
        ));
    }
    c.extend(crate::cargo::windows_dirs(
        &["LOCALAPPDATA"],
        &format!(r"Programs\Git\bin\{exe}"),
    ));
    let found = c.into_iter().find(|p| p.is_file());
    // A box without bash still runs a script written for plain sh.
    found.or_else(|| (kind == "bash").then(|| windows_git_shell("sh")).flatten())
}

/// The shell that runs a command line, as `(program, flags before the line)`: `/bin/sh -c`
/// on unix, PowerShell on Windows (the same lookup as the script provider). For callers that
/// have a line of shell text and no package shell setting (hooks, declarative providers).
pub fn platform_shell() -> (PathBuf, &'static [&'static str]) {
    #[cfg(unix)]
    return (PathBuf::from("/bin/sh"), &["-c"]);
    #[cfg(windows)]
    return (
        windows_pwsh().unwrap_or_else(|| PathBuf::from("powershell.exe")),
        &["-NoProfile", "-NonInteractive", "-Command"],
    );
}

/// Lower-case 64-digit hex of a `sha256:`-optional hash.
pub fn normalize_hash(h: &str) -> Option<String> {
    let h = h.trim().strip_prefix("sha256:").unwrap_or(h.trim());
    (h.len() == 64 && h.chars().all(|c| c.is_ascii_hexdigit())).then(|| h.to_ascii_lowercase())
}

/// Quote `p` for the script's shell when it needs it.
fn shell_quote(p: &str, pwsh: bool) -> String {
    if p.chars()
        .all(|c| c.is_ascii_alphanumeric() || "/._-+:@".contains(c))
    {
        return p.to_string();
    }
    if pwsh {
        format!("'{}'", p.replace('\'', "''"))
    } else {
        format!("'{}'", p.replace('\'', "'\\''"))
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
    /// `HOME` for the script's commands. Scripts act on the user's files, so under
    /// `--home <dir>` they see that dir as `~`, unlike the package manager providers, which
    /// keep the real HOME. Empty leaves `HOME` as `ctx.env` has it.
    pub home: PathBuf,
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

fn hex_sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Create a new file; fails if the path exists (so a planted symlink is never followed).
/// A stale regular file or symlink is unlinked first.
fn write_exclusive(path: &Path, bytes: &[u8], mode: u32) -> std::io::Result<()> {
    use std::io::Write;
    if fs::symlink_metadata(path).is_ok() {
        fs::remove_file(path)?;
    }
    let mut o = fs::OpenOptions::new();
    o.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(mode);
    }
    #[cfg(not(unix))]
    let _ = mode;
    o.open(path)?.write_all(bytes)
}

/// A fresh private (0700) directory, created exclusively and removed on drop.
struct PrivateDir(PathBuf);

impl PrivateDir {
    fn create() -> std::io::Result<Self> {
        use std::sync::atomic::{AtomicU64, Ordering};
        static N: AtomicU64 = AtomicU64::new(0);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        for _ in 0..16 {
            let name = format!(
                "dots-fetch-{}-{nanos:x}-{}",
                std::process::id(),
                N.fetch_add(1, Ordering::Relaxed)
            );
            let dir = std::env::temp_dir().join(name);
            #[allow(unused_mut)] // only the unix branch sets a mode
            let mut b = fs::DirBuilder::new();
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                b.mode(0o700);
            }
            match b.create(&dir) {
                Ok(()) => return Ok(PrivateDir(dir)),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(e),
            }
        }
        Err(std::io::Error::other("no free temporary directory name"))
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for PrivateDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

pub struct ScriptProvider {
    pkg: String,
    cfg: ScriptConfig,
    env: ScriptEnv,
    facts: Facts,
    /// The hash `dots.lock` pinned for the `fetch` file, if any.
    lock_pin: Option<String>,
    /// The hash of the last verified download (for the lock to record on first use).
    fetched: Arc<Mutex<Option<String>>>,
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
            lock_pin: None,
            fetched: Arc::default(),
        })
    }

    /// The lock's hash for the `fetch` file; the download must match it as well as the
    /// author's pin.
    pub fn with_lock_pin(mut self, sha256: Option<String>) -> Self {
        self.lock_pin = sha256.as_deref().and_then(normalize_hash);
        self
    }

    /// The sha256 of the file the last install or upgrade fetched. The caller records it in
    /// the lock when nothing pinned it.
    pub fn fetched_sha256(&self) -> Option<String> {
        self.fetched.lock().ok().and_then(|g| g.clone())
    }

    /// Download the `fetch` file, check it against the author's and the lock's pins, and put
    /// it in the package's cache. Nothing is run before this passes.
    fn fetch_verified(&self, ctx: &Ctx, spec: &FetchSpec) -> Result<(PrivateDir, PathBuf)> {
        if ctx.offline {
            return Err(ProviderError::Other(format!(
                "`{}` needs {} downloaded, but dots is offline",
                self.pkg, spec.url
            )));
        }
        let bytes = ctx.fetcher.fetch(&spec.url)?;
        let got = hex_sha256(&bytes);
        let pins = [
            spec.sha256.as_deref().and_then(normalize_hash),
            self.lock_pin.clone(),
        ];
        for expected in pins.into_iter().flatten() {
            if expected != got {
                return Err(ProviderError::Coded {
                    code: "integrity-mismatch",
                    message: format!(
                        "checksum mismatch for `{}`: expected {expected}, got {got}",
                        spec.url
                    ),
                    log: None,
                });
            }
        }
        let file = spec
            .url
            .split(['?', '#'])
            .next()
            .and_then(|u| u.rsplit('/').next())
            .filter(|n| !n.is_empty() && *n != "." && *n != "..")
            .unwrap_or("installer")
            .replace(['\\', ':'], "_");
        // The cache copy is for the user's reference only and is never what runs: it sits
        // in a user-writable place, so it is written without following symlinks and the
        // installer runs from a private copy below.
        if !ctx.cache_dir.as_os_str().is_empty() {
            let dir = ctx
                .cache_dir
                .join("fetch")
                .join(self.pkg.replace(['/', '\\'], "_"));
            let path = dir.join(&file);
            fs::create_dir_all(&dir)
                .and_then(|()| {
                    if fs::symlink_metadata(&dir)?.is_dir() {
                        write_exclusive(&path, &bytes, 0o644)
                    } else {
                        Err(std::io::Error::other("cache directory is a symlink"))
                    }
                })
                .map_err(|e| {
                    ProviderError::Other(format!("cannot write {}: {e}", path.display()))
                })?;
        }
        // The file that runs: a fresh 0700 directory (created exclusively), a copy of the
        // verified bytes, and a re-hash of that copy.
        let private = PrivateDir::create()
            .map_err(|e| ProviderError::Other(format!("cannot create a private directory: {e}")))?;
        let path = private.path().join(&file);
        write_exclusive(&path, &bytes, 0o700)
            .map_err(|e| ProviderError::Other(format!("cannot write {}: {e}", path.display())))?;
        let again = fs::read(&path)
            .map_err(|e| ProviderError::Other(format!("cannot read {}: {e}", path.display())))?;
        if hex_sha256(&again) != got {
            return Err(ProviderError::Coded {
                code: "integrity-mismatch",
                message: format!("`{}` changed after it was verified", spec.url),
                log: None,
            });
        }
        if let Ok(mut g) = self.fetched.lock() {
            *g = Some(got);
        }
        Ok((private, path))
    }

    pub fn package(&self) -> &str {
        &self.pkg
    }

    /// A command that runs `text` with this provider's runtime contract (the shell and its
    /// `set` prelude, the package directory as working directory, the `DOTS_*` variables,
    /// stdin at `/dev/null`, the timeout). Used for package `[hooks]`.
    pub fn hook_cmd(&self, ctx: &Ctx, text: &str) -> Result<Cmd> {
        let shell = self
            .shell_path()
            .ok_or_else(|| ProviderError::Unavailable {
                provider: "script".into(),
                reason: format!("no `{}` found", self.shell_kind()),
            })?;
        self.cmd(
            ctx,
            &self.shell_argv(&shell, &self.resolve_text(text)),
            None,
        )
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
        if !self.env.home.as_os_str().is_empty() {
            set("HOME", p(&self.env.home));
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
            let mut parts: Vec<PathBuf> = self.env.path_prefix.clone();
            if let Some(old) = ctx.env.get("PATH").filter(|s| !s.is_empty()) {
                parts.extend(std::env::split_paths(old));
            }
            // `;` on Windows, `:` elsewhere.
            if let Ok(joined) = std::env::join_paths(&parts) {
                set("PATH", joined.to_string_lossy().into_owned());
            }
        }
        e
    }

    /// A root action goes through `sudo -n`, whose `env_reset` drops everything the script
    /// would get unprivileged: `HOME` becomes `/root` and the `DOTS_*` variables (settings,
    /// secrets included) and the `[env]` `PATH` directories vanish. The values must not go
    /// into argv, where any local user can read them (`ps`, `/proc/*/cmdline`), and
    /// `--preserve-env` is rejected by sudoers without `SETENV`. So they go into a file in a
    /// private 0700 directory, created with mode 0600, and the script text is prefixed with
    /// `set -a; . '<file>'; set +a`. Root reads the file whatever its mode. Only `HOME`, `PATH`
    /// and the `DOTS_*` variables are written. The returned directory removes the file when
    /// dropped: hold it until the action has finished.
    fn carry_env(
        &self,
        ctx: &Ctx,
        argv: &mut [String],
        version_wanted: Option<&str>,
    ) -> Result<PrivateDir> {
        if self.shell_kind() == "pwsh" {
            return Err(ProviderError::Other(format!(
                "`{}` has needs_root = true with the pwsh shell; root scripts need `shell = \"bash\"` or `\"sh\"`",
                self.pkg
            )));
        }
        let env = self.script_env(ctx, version_wanted);
        let mut text = String::new();
        for (k, v) in &env {
            let name_ok = k
                .chars()
                .enumerate()
                .all(|(i, c)| c == '_' || c.is_ascii_alphabetic() || (i > 0 && c.is_ascii_digit()));
            if name_ok && (matches!(k.as_str(), "HOME" | "PATH") || k.starts_with("DOTS_")) {
                text.push_str(&format!("{k}={}\n", shell_quote(v, false)));
            }
        }
        let io = |e: std::io::Error| ProviderError::Other(format!("cannot write env file: {e}"));
        let dir = PrivateDir::create().map_err(io)?;
        let file = dir.path().join("env");
        write_exclusive(&file, text.as_bytes(), 0o600).map_err(io)?;
        let prelude = format!(
            "set -a; . {}; set +a\n",
            shell_quote(&file.to_string_lossy(), false)
        );
        if let Some(t) = argv.last_mut() {
            *t = format!("{prelude}{t}");
        }
        Ok(dir)
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
        #[cfg(windows)]
        if kind == "pwsh" {
            windows_pwsh()
        } else {
            windows_git_shell(kind)
        }
        #[cfg(not(windows))]
        self.unix_shell_path(kind)
    }

    #[cfg(not(windows))]
    fn unix_shell_path(&self, kind: &str) -> Option<PathBuf> {
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

    /// A value that is one token naming an existing file in the package directory (`install =
    /// "install.sh"`) runs that file with the script's shell; anything else is shell text, as
    /// written. Applies to `check`, `version`, `latest`, `install`, `upgrade`, `uninstall` and
    /// the package hooks.
    fn resolve_text(&self, text: &str) -> String {
        let t = text.trim();
        let single = !t.is_empty()
            && !t
                .chars()
                .any(|c| c.is_whitespace() || "|&;<>()$`\\\"'*?[]{}~#!".contains(c));
        if !single || !self.env.pkg_dir.join(t).is_file() {
            return text.to_string();
        }
        let path = if t.starts_with('/') || t.starts_with("./") || t.starts_with("../") {
            t.to_string()
        } else {
            format!("./{t}")
        };
        let pwsh = self.shell_kind() == "pwsh";
        let q = shell_quote(&path, pwsh);
        match self.shell_kind() {
            "pwsh" => format!("& {q}"),
            kind => format!("{kind} {q}"),
        }
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

    /// Like [`Self::cmd`] for a read-only question: the timeout is the shorter of the script's
    /// own and `ctx.query_timeout`.
    fn query_cmd(&self, ctx: &Ctx, argv: &[String], version_wanted: Option<&str>) -> Result<Cmd> {
        let mut c = self.cmd(ctx, argv, version_wanted)?;
        c.timeout = Some(
            c.timeout
                .map_or(ctx.query_timeout, |t| t.min(ctx.query_timeout)),
        );
        Ok(c)
    }

    /// The limit a question hit, as the user would write it.
    fn query_limit(&self, ctx: &Ctx) -> Duration {
        self.cfg
            .timeout()
            .map_or(ctx.query_timeout, |t| t.min(ctx.query_timeout))
    }

    /// `check` ran out of time: an error for this package only. Names the command, since
    /// what hung is usually something the command waits for.
    fn check_timeout_error(&self, ctx: &Ctx) -> ProviderError {
        let limit = fmt_limit(self.query_limit(ctx));
        let text = self.cfg.check.as_deref().unwrap_or("false");
        ProviderError::Coded {
            code: "query-timeout",
            message: format!(
                "the check of `{}` did not finish within {limit}: `{}`",
                self.pkg,
                one_line(text)
            ),
            log: None,
        }
    }

    /// `version` or `latest` ran out of time: the answer is unknown, the plan goes on.
    fn query_timed_out(&self, ctx: &Ctx, what: &'static str) {
        crate::record_query_timeout(crate::QueryTimeout {
            package: self.pkg.clone(),
            what,
            timeout: fmt_limit(self.query_limit(ctx)),
        });
    }

    fn command_for(&self, kind: OpKind) -> Option<String> {
        let c = &self.cfg;
        match kind {
            OpKind::Install => c.install.as_deref(),
            OpKind::Upgrade => c.upgrade.as_deref().or(c.install.as_deref()),
            OpKind::Remove => c.uninstall.as_deref(),
            OpKind::Pin | OpKind::Unpin => None,
        }
        .map(|t| self.resolve_text(t))
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
        let text = self.resolve_text(self.cfg.check.as_deref().unwrap_or("false"));
        ctx.runner
            .run(&self.query_cmd(ctx, &self.shell_argv(shell, &text), version_wanted)?)
    }
}

/// `60s`, `500ms`.
fn fmt_limit(d: Duration) -> String {
    if !d.as_millis().is_multiple_of(1000) {
        format!("{}ms", d.as_millis())
    } else {
        format!("{}s", d.as_secs())
    }
}

/// A command text on one line, cut to a length that fits a message.
fn one_line(text: &str) -> String {
    let t = text.split_whitespace().collect::<Vec<_>>().join(" ");
    match t.char_indices().nth(100) {
        Some((i, _)) => format!("{}...", &t[..i]),
        None => t,
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
    // `sudo -n <shell> -c <text>` (a root action) or `<shell> -c <text>`.
    let skip = if argv.first().is_some_and(|a| a.ends_with("/sudo")) {
        3
    } else {
        1
    };
    argv.iter()
        .skip(skip)
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

    fn lock_key(&self) -> Option<String> {
        self.cfg.lock.clone().filter(|k| !k.trim().is_empty())
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
        let check = self.resolve_text(self.cfg.check.as_deref().unwrap_or("false"));
        let version = self
            .cfg
            .version
            .as_deref()
            .filter(|_| !self.cfg.versionless)
            .map(|v| self.resolve_text(v));
        let version = version.as_deref();
        let (installed, version_out) = if self.shell_kind() == "pwsh" {
            let c = self.run_check(ctx, &shell, None)?;
            if c.timed_out {
                return Err(self.check_timeout_error(ctx));
            }
            let v = match (c.success(), version) {
                (true, Some(v)) => {
                    let o = ctx.runner.run(&self.query_cmd(
                        ctx,
                        &self.shell_argv(&shell, v),
                        None,
                    )?)?;
                    if o.timed_out {
                        self.query_timed_out(ctx, "version");
                    }
                    Some(o.stdout)
                }
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
            let out = ctx.runner.run(&self.query_cmd(ctx, &argv, None)?)?;
            if out.timed_out {
                // The marker is printed once `check` passed: without it `check` itself hung
                // (an error for this package); with it only `version` did (installed, version
                // unknown).
                if !out.stdout.lines().any(|l| l.trim() == INSTALLED) {
                    return Err(self.check_timeout_error(ctx));
                }
                self.query_timed_out(ctx, "version");
            } else if !out.success() {
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
        let cmd = self.resolve_text(cmd);
        if !specs.iter().any(|s| s.name == self.pkg) {
            return Ok(map);
        }
        let out = ctx
            .runner
            .run(&self.query_cmd(ctx, &self.shell_argv(&shell, &cmd), None)?)?;
        if out.timed_out {
            self.query_timed_out(ctx, "latest");
            return Ok(map);
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
                    argv: self.shell_argv(&shell, &text),
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
        let mut action = action.clone();
        // Held until the end of `exec`: dropping it deletes the private copy.
        let mut _private = None;
        if let Some(spec) = &self.cfg.fetch
            && matches!(action.kind, OpKind::Install | OpKind::Upgrade)
        {
            let (dir, path) = self.fetch_verified(ctx, spec)?;
            _private = Some(dir);
            let quoted = shell_quote(&path.to_string_lossy(), self.shell_kind() == "pwsh");
            if let Some(text) = action.argv.last_mut() {
                *text = text.replace(FETCHED, &quoted);
            }
        }
        // Held until the end of `exec`: dropping it deletes the env file.
        let _env_file = if action.needs_root {
            Some(self.carry_env(ctx, &mut action.argv, wanted)?)
        } else {
            None
        };
        let action = &action;
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
            // The check always runs unprivileged, through this provider's own shell. It must
            // not take the shell from `argv[0]`: the engine prefixes a `needs_root` action with
            // `sudo -n`, which made this run `sudo -c <text>` (sudo's usage error, so the
            // check of every root script failed). `installed()` checks unprivileged too.
            let shell = self
                .shell_path()
                .ok_or_else(|| ProviderError::Unavailable {
                    provider: "script".into(),
                    reason: format!("no `{}` found", self.shell_kind()),
                })?;
            let check = self.run_check(ctx, &shell, wanted)?;
            append_log(
                &log,
                &format!("check: {}", self.cfg.check.as_deref().unwrap_or("")),
                &check,
            );
            if check.timed_out {
                return Err(self.check_timeout_error(ctx));
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
