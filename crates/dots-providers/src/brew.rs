//! The Homebrew provider (ARCHITECTURE.md section 5; spec `provider-contract`).
//!
//! Design notes:
//!
//! * **Own binary.** `brew` is found at the known prefixes (`HOMEBREW_PREFIX` from `ctx.env`
//!   first), never via `PATH`. Availability is re-evaluated on every call (nothing is cached), so
//!   after Homebrew is bootstrapped later brew actions in the same run succeed.
//! * **One batched query.** `installed()` is a single `brew info --json=v2 --installed`
//!   (formulae and casks). `latest()` is one `brew info --json=v2 <names>` per kind (formula
//!   call plus, only when casks are asked for, one `--cask` call). `bins()` reuses the one
//!   installed query and reads keg directories from the filesystem.
//! * **Spec.** `Spec::opts["cask"] = "true"` selects `--cask`; `Spec::source` is the tap
//!   (`user/repo`), see [`spec`]. Casks and formulae share one name space in
//!   `installed()` (formulae win a collision); casks are marked with `Installed::source`
//!   starting with `cask`.
//! * **Limited exact versions.** brew only offers its current version plus `name@X` formulae.
//!   `plan` therefore installs what brew offers for a plain name and keeps the requested version
//!   on the `Op` so the engine reports drift; for `name@X` formulae a version outside `X` is an
//!   error. `plan` is pure and cannot compare against the current stable version.
//! * **Determinism.** Every brew command runs with `HOMEBREW_NO_AUTO_UPDATE=1`,
//!   `HOMEBREW_NO_INSTALL_CLEANUP=1`, `HOMEBREW_NO_ENV_HINTS=1` and `NONINTERACTIVE=1`.
//! * **Tap actions.** A `brew tap <tap>` action precedes the installs that need it. It has no
//!   `ops` (it carries no user-visible package).

use std::collections::{BTreeMap, BTreeSet};
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};

use dots_facts::Facts;
use serde_json::Value;

use crate::{
    Action, Availability, BinEntry, Caps, Cmd, Ctx, Installed, Latest, Op, OpKind, Outcome,
    Provider, ProviderError, Result, Spec, Support,
};

const ID: &str = "brew";
const DEFAULT_PATH: &str = "/usr/bin:/bin:/usr/sbin:/sbin";

/// Build a brew [`Spec`]: `cask` selects `--cask`, `tap` is the `user/repo` the package needs.
pub fn spec(name: &str, cask: bool, tap: Option<&str>) -> Spec {
    let mut s = Spec::new(name);
    if cask {
        s = s.opt("cask", "true");
    }
    if let Some(t) = tap {
        s = s.source(t);
    }
    s
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct Kind {
    cask: bool,
    tap: Option<String>,
}

fn parse_source(spec: &Spec) -> Result<Kind> {
    let mut k = Kind {
        cask: spec.flag("cask", false),
        tap: None,
    };
    if let Some(tap) = spec.source.as_deref() {
        if tap.split('/').count() == 2 && !tap.starts_with('-') && !tap.is_empty() {
            k.tap = Some(tap.to_string());
        } else {
            return Err(ProviderError::Other(format!(
                "brew: unknown source `{tap}` for `{}` (expected a tap `user/repo`)",
                spec.name
            )));
        }
    }
    Ok(k)
}

/// The name brew is given: tap packages are fully qualified.
fn brew_name(spec: &Spec, kind: &Kind) -> Result<String> {
    if spec.name.is_empty() || spec.name.starts_with('-') || spec.name.contains(char::is_whitespace)
    {
        return Err(ProviderError::Other(format!(
            "brew: invalid package name `{}`",
            spec.name
        )));
    }
    Ok(match &kind.tap {
        Some(t) if !spec.name.contains('/') => format!("{t}/{}", spec.name),
        _ => spec.name.clone(),
    })
}

/// For `name@X` formulae, the requested version must lie inside `X`.
fn check_version(name: &str, version: Option<&str>) -> Result<()> {
    let Some(v) = version else { return Ok(()) };
    let base = name.rsplit('/').next().unwrap_or(name);
    if let Some((_, want)) = base.rsplit_once('@')
        && v != want
        && !v.starts_with(&format!("{want}."))
    {
        return Err(ProviderError::Other(format!(
            "brew cannot install version {v} of `{base}`: this formula only provides {want}.x"
        )));
    }
    Ok(())
}

#[derive(Debug, Clone)]
pub struct BrewProvider {
    /// Known brew binary locations, in preference order.
    candidates: Vec<PathBuf>,
}

impl Default for BrewProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl BrewProvider {
    pub fn new() -> Self {
        Self::with_candidates(vec![
            "/opt/homebrew/bin/brew".into(),
            "/usr/local/bin/brew".into(),
            "/home/linuxbrew/.linuxbrew/bin/brew".into(),
        ])
    }

    /// Look for brew only at these paths (tests use a fake prefix).
    pub fn with_candidates(candidates: Vec<PathBuf>) -> Self {
        BrewProvider { candidates }
    }

    /// Where brew is, if installed: `HOMEBREW_PREFIX/bin/brew` from `env`, then the known paths.
    fn locate(&self, env: &BTreeMap<String, String>) -> Option<PathBuf> {
        env.get("HOMEBREW_PREFIX")
            .filter(|p| !p.is_empty())
            .map(|p| Path::new(p).join("bin/brew"))
            .into_iter()
            .chain(self.candidates.iter().cloned())
            .find(|p| p.is_file())
    }

    /// The path `plan` puts in `argv[0]`: the installed brew, or where brew will land on this
    /// platform when it is not installed yet (fresh machine, before bootstrap).
    fn plan_bin(&self) -> PathBuf {
        if let Some(p) = self.candidates.iter().find(|p| p.is_file()) {
            return p.clone();
        }
        let preferred = if cfg!(target_os = "linux") {
            self.candidates.iter().find(|p| p.starts_with("/home"))
        } else if cfg!(target_arch = "aarch64") {
            self.candidates.iter().find(|p| p.starts_with("/opt"))
        } else {
            self.candidates.iter().find(|p| p.starts_with("/usr/local"))
        };
        preferred
            .or(self.candidates.first())
            .cloned()
            .unwrap_or_else(|| "/opt/homebrew/bin/brew".into())
    }

    fn require(&self, ctx: &Ctx) -> Result<PathBuf> {
        self.locate(&ctx.env)
            .ok_or_else(|| ProviderError::Unavailable {
                provider: ID.into(),
                reason: "Homebrew is not installed".into(),
            })
    }

    /// Environment for every brew command.
    fn brew_env(&self, ctx: &Ctx, bin: &Path) -> BTreeMap<String, String> {
        let mut env = ctx.env.clone();
        for (k, v) in [
            ("HOMEBREW_NO_AUTO_UPDATE", "1"),
            ("HOMEBREW_NO_INSTALL_CLEANUP", "1"),
            ("HOMEBREW_NO_ENV_HINTS", "1"),
            ("NONINTERACTIVE", "1"),
        ] {
            env.insert(k.into(), v.into());
        }
        if !env.contains_key("HOME") && !ctx.home.as_os_str().is_empty() {
            env.insert("HOME".into(), ctx.home.to_string_lossy().into_owned());
        }
        let mut path = env
            .get("PATH")
            .filter(|p| !p.is_empty())
            .cloned()
            .unwrap_or_else(|| DEFAULT_PATH.into());
        if let Some(prefix) = bin.parent().and_then(Path::parent) {
            for d in [prefix.join("sbin"), prefix.join("bin")] {
                let d = d.to_string_lossy().into_owned();
                if !path.split(':').any(|p| p == d) {
                    path = format!("{d}:{path}");
                }
            }
        }
        env.insert("PATH".into(), path);
        env
    }

    /// Run `brew info --json=v2 <args>` and parse its JSON.
    fn info(&self, ctx: &Ctx, bin: &Path, args: &[&str]) -> Result<Value> {
        let cmd = Cmd::new(bin)
            .args(["info", "--json=v2"])
            .args(args.iter().copied())
            .envs(&self.brew_env(ctx, bin));
        let out = ctx.runner.run(&cmd)?;
        if !out.success() {
            let msg = out.stderr.lines().find(|l| !l.trim().is_empty());
            return Err(ProviderError::Other(match msg {
                Some(m) => format!("`{}` failed (status {}): {m}", cmd.line(), out.status),
                None => format!("`{}` failed with exit status {}", cmd.line(), out.status),
            }));
        }
        serde_json::from_str(&out.stdout)
            .map_err(|e| ProviderError::Other(format!("`{}`: bad JSON: {e}", cmd.line())))
    }

    fn prefixes(&self, ctx: &Ctx) -> BTreeSet<PathBuf> {
        let mut set: BTreeSet<PathBuf> = self
            .candidates
            .iter()
            .filter_map(|c| c.parent()?.parent().map(Path::to_path_buf))
            .collect();
        if let Some(p) = ctx.env.get("HOMEBREW_PREFIX").filter(|p| !p.is_empty()) {
            set.insert(PathBuf::from(p));
        }
        // Paths are compared after symlink resolution, so include the resolved prefixes too.
        let resolved: Vec<PathBuf> = set
            .iter()
            .filter_map(|p| std::fs::canonicalize(p).ok())
            .collect();
        set.extend(resolved);
        set
    }
}

fn arr<'a>(v: &'a Value, key: &str) -> &'a [Value] {
    v.get(key)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
}

fn s<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key).and_then(Value::as_str).filter(|s| !s.is_empty())
}

/// The version of an installed formula: the linked keg, else the newest installed keg.
fn formula_installed_version(f: &Value) -> Option<String> {
    s(f, "linked_keg").map(str::to_string).or_else(|| {
        arr(f, "installed")
            .iter()
            .filter_map(|i| s(i, "version"))
            .next_back()
            .map(str::to_string)
    })
}

fn is_default_tap(tap: &str) -> bool {
    matches!(tap, "homebrew/core" | "homebrew/cask")
}

fn parse_installed(doc: &Value) -> BTreeMap<String, Installed> {
    let mut map = BTreeMap::new();
    for f in arr(doc, "formulae") {
        let (Some(name), Some(version)) = (s(f, "name"), formula_installed_version(f)) else {
            continue;
        };
        map.insert(
            name.to_string(),
            Installed {
                version,
                pinned: f.get("pinned").and_then(Value::as_bool).unwrap_or(false),
                source: s(f, "tap")
                    .filter(|t| !is_default_tap(t))
                    .map(str::to_string),
            },
        );
    }
    for c in arr(doc, "casks") {
        let (Some(token), Some(version)) = (s(c, "token"), s(c, "installed")) else {
            continue;
        };
        map.entry(token.to_string()).or_insert_with(|| Installed {
            version: version.to_string(),
            pinned: c.get("pinned").and_then(Value::as_bool).unwrap_or(false),
            source: Some(match s(c, "tap").filter(|t| !is_default_tap(t)) {
                Some(t) => format!("cask:{t}"),
                None => "cask".to_string(),
            }),
        });
    }
    map
}

/// Names installed on request: every cask, and each formula brew did not install only as a
/// dependency. A formula whose JSON lacks the flag counts as a root.
fn parse_roots(doc: &Value) -> BTreeSet<String> {
    let mut roots = BTreeSet::new();
    for f in arr(doc, "formulae") {
        let Some(name) = s(f, "name") else { continue };
        let kegs = arr(f, "installed");
        let on_request = kegs.is_empty()
            || kegs.iter().any(|k| {
                k.get("installed_on_request")
                    .and_then(Value::as_bool)
                    .unwrap_or(true)
            });
        if on_request {
            roots.insert(name.to_string());
        }
    }
    for c in arr(doc, "casks") {
        if let Some(token) = s(c, "token") {
            roots.insert(token.to_string());
        }
    }
    roots
}

/// brew's own version string for a formula: `stable` plus `_<revision>`.
fn formula_latest_version(f: &Value) -> Option<String> {
    let stable = f.get("versions").and_then(|v| s(v, "stable"))?;
    Some(match f.get("revision").and_then(Value::as_u64) {
        Some(r) if r > 0 => format!("{stable}_{r}"),
        _ => stable.to_string(),
    })
}

/// Every name a formula/cask answers to, for matching the specs of a `latest` query.
fn names_of(v: &Value, keys: &[&str]) -> Vec<String> {
    let mut out = Vec::new();
    for k in keys {
        match v.get(*k) {
            Some(Value::String(x)) => out.push(x.clone()),
            Some(Value::Array(xs)) => {
                out.extend(xs.iter().filter_map(Value::as_str).map(str::to_string))
            }
            _ => {}
        }
    }
    out
}

impl Provider for BrewProvider {
    fn id(&self) -> &'static str {
        ID
    }

    fn caps(&self) -> Caps {
        Caps {
            batch_install: true,
            exact_version: Support::Limited,
            pin: true,
            needs_root: false,
            uninstall: true,
            exclusive_lock: true,
        }
    }

    fn availability(&self, facts: &Facts) -> Availability {
        self.availability_for(&BTreeMap::new(), facts)
    }

    fn availability_in(&self, ctx: &Ctx, facts: &Facts) -> Availability {
        self.availability_for(&ctx.env, facts)
    }

    fn installed(&self, ctx: &Ctx) -> Result<BTreeMap<String, Installed>> {
        let bin = self.require(ctx)?;
        Ok(parse_installed(&self.info(ctx, &bin, &["--installed"])?))
    }

    fn roots(&self, ctx: &Ctx) -> Result<Option<BTreeSet<String>>> {
        let bin = self.require(ctx)?;
        Ok(Some(parse_roots(&self.info(
            ctx,
            &bin,
            &["--installed"],
        )?)))
    }

    fn latest(&self, ctx: &Ctx, specs: &[Spec]) -> Result<BTreeMap<String, Latest>> {
        // Offline: no fresh data to offer; the caller falls back to its cache.
        if ctx.offline || specs.is_empty() {
            return Ok(BTreeMap::new());
        }
        let bin = self.require(ctx)?;
        let mut wanted: [Vec<(&Spec, String)>; 2] = [Vec::new(), Vec::new()];
        for sp in specs {
            let kind = parse_source(sp)?;
            wanted[usize::from(kind.cask)].push((sp, brew_name(sp, &kind)?));
        }
        let mut out = BTreeMap::new();
        for (cask, group) in wanted.iter().enumerate() {
            if group.is_empty() {
                continue;
            }
            let mut args: Vec<&str> = if cask == 1 { vec!["--cask"] } else { vec![] };
            args.extend(group.iter().map(|(_, n)| n.as_str()));
            let doc = self.info(ctx, &bin, &args)?;
            let mut versions: BTreeMap<String, String> = BTreeMap::new();
            if cask == 1 {
                for c in arr(&doc, "casks") {
                    if let Some(v) = s(c, "version") {
                        for n in names_of(c, &["token", "full_token", "old_tokens"]) {
                            versions.entry(n).or_insert_with(|| v.to_string());
                        }
                    }
                }
            } else {
                for f in arr(&doc, "formulae") {
                    if let Some(v) = formula_latest_version(f) {
                        for n in names_of(f, &["name", "full_name", "aliases", "oldnames"]) {
                            versions.entry(n).or_insert_with(|| v.clone());
                        }
                    }
                }
            }
            for (sp, full) in group {
                if let Some(v) = versions.get(full).or_else(|| versions.get(&sp.name)) {
                    out.insert(sp.name.clone(), Latest { version: v.clone() });
                }
            }
        }
        Ok(out)
    }

    fn plan(&self, ops: &[Op]) -> Result<Vec<Action>> {
        let bin = self.plan_bin().to_string_lossy().into_owned();
        struct Item<'a> {
            op: &'a Op,
            cask: bool,
            name: String,
            tap: Option<String>,
        }
        let mut items = Vec::new();
        for op in ops {
            let kind = parse_source(&op.spec)?;
            let name = brew_name(&op.spec, &kind)?;
            if matches!(op.kind, OpKind::Pin | OpKind::Unpin) && kind.cask {
                return Err(ProviderError::Other(format!(
                    "brew cannot pin casks (`{}`); pin applies to formulae only",
                    op.spec.name
                )));
            }
            if matches!(op.kind, OpKind::Install | OpKind::Upgrade) {
                check_version(&name, op.version.as_deref())?;
            }
            items.push(Item {
                op,
                cask: kind.cask,
                name,
                tap: kind.tap,
            });
        }

        let mut actions = Vec::new();
        let mut taps = BTreeSet::new();
        for it in &items {
            if matches!(it.op.kind, OpKind::Install | OpKind::Upgrade)
                && let Some(t) = &it.tap
                && taps.insert(t.clone())
            {
                actions.push(Action {
                    kind: OpKind::Install,
                    ops: Vec::new(),
                    argv: vec![bin.clone(), "tap".into(), t.clone()],
                    needs_root: false,
                });
            }
        }
        let order = [
            (OpKind::Unpin, "unpin"),
            (OpKind::Remove, "uninstall"),
            (OpKind::Install, "install"),
            (OpKind::Upgrade, "upgrade"),
            (OpKind::Pin, "pin"),
        ];
        for (kind, verb) in order {
            for cask in [false, true] {
                let group: Vec<&Item> = items
                    .iter()
                    .filter(|i| i.op.kind == kind && i.cask == cask)
                    .collect();
                if group.is_empty() {
                    continue;
                }
                let mut argv = vec![bin.clone(), verb.to_string()];
                if cask {
                    argv.push("--cask".into());
                }
                let mut seen = BTreeSet::new();
                argv.extend(
                    group
                        .iter()
                        .filter(|i| seen.insert(&i.name))
                        .map(|i| i.name.clone()),
                );
                actions.push(Action {
                    kind,
                    ops: group.iter().map(|i| i.op.clone()).collect(),
                    argv,
                    needs_root: false,
                });
            }
        }
        Ok(actions)
    }

    fn exec(&self, ctx: &Ctx, action: &Action) -> Result<Outcome> {
        let mut argv = action.argv.clone();
        if argv.is_empty() {
            return Err(ProviderError::Other(
                "brew: action has an empty argv".into(),
            ));
        }
        // A plan made before Homebrew existed names the default location; use the real one.
        if !Path::new(&argv[0]).is_file()
            && let Some(found) = self.locate(&ctx.env)
        {
            argv[0] = found.to_string_lossy().into_owned();
        }
        if ctx.dry_run {
            return Ok(Outcome {
                success: true,
                log: None,
            });
        }
        let env = self.brew_env(ctx, Path::new(&argv[0]));
        let cmd = Cmd::from_argv(&argv)
            .ok_or_else(|| ProviderError::Other("brew: empty argv".into()))?
            .envs(&env);
        let started = std::time::Instant::now();
        let out = ctx.runner.run(&cmd);
        let (mut file, log) = open_log(&ctx.log_dir)?;
        let write_err = |e: std::io::Error| ProviderError::Other(format!("brew: writing log: {e}"));
        writeln!(file, "$ {}", cmd.line()).map_err(write_err)?;
        let out = match out {
            Ok(o) => o,
            Err(e) => {
                let _ = writeln!(file, "failed to run: {e}");
                return Err(e);
            }
        };
        let status = if out.timed_out {
            "timed out".to_string()
        } else {
            format!("exit status {}", out.status)
        };
        writeln!(
            file,
            "# {status} after {:.1}s",
            started.elapsed().as_secs_f32()
        )
        .and_then(|_| file.write_all(out.stdout.as_bytes()))
        .and_then(|_| {
            if out.stderr.is_empty() {
                Ok(())
            } else {
                writeln!(file, "--- stderr ---").and_then(|_| file.write_all(out.stderr.as_bytes()))
            }
        })
        .map_err(write_err)?;
        Ok(Outcome {
            success: out.success(),
            log: Some(log),
        })
    }

    fn bins(&self, ctx: &Ctx, installed: &BTreeMap<String, Installed>) -> Result<Vec<BinEntry>> {
        if installed.is_empty() {
            return Ok(Vec::new());
        }
        let bin = self.require(ctx)?;
        let prefix = bin
            .parent()
            .and_then(Path::parent)
            .map(Path::to_path_buf)
            .unwrap_or_default();
        let doc = self.info(ctx, &bin, &["--installed"])?;
        let mut out = Vec::new();
        for f in arr(&doc, "formulae") {
            let Some(name) = s(f, "name").filter(|n| installed.contains_key(*n)) else {
                continue;
            };
            let Some(ver) = formula_installed_version(f) else {
                continue;
            };
            let linked = s(f, "linked_keg").is_some();
            let keg = prefix.join("Cellar").join(name).join(&ver);
            for sub in ["bin", "sbin"] {
                let Ok(rd) = std::fs::read_dir(keg.join(sub)) else {
                    continue;
                };
                let mut names: Vec<String> = rd
                    .flatten()
                    .filter(|e| e.path().is_file() || e.file_type().is_ok_and(|t| t.is_symlink()))
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .collect();
                names.sort();
                for n in names {
                    // Linked kegs expose their executables under the prefix; keg-only ones do not.
                    let path = if linked {
                        prefix.join(sub).join(&n)
                    } else {
                        keg.join(sub).join(&n)
                    };
                    out.push(BinEntry {
                        package: name.to_string(),
                        bin: n,
                        path,
                    });
                }
            }
        }
        for c in arr(&doc, "casks") {
            let Some(token) = s(c, "token").filter(|t| installed.contains_key(*t)) else {
                continue;
            };
            for art in arr(c, "artifacts") {
                let Some(b) = art.get("binary").and_then(Value::as_array) else {
                    continue;
                };
                let path = match s(art, "target") {
                    Some(t) => PathBuf::from(t),
                    None => {
                        let Some(src) = b.first().and_then(Value::as_str) else {
                            continue;
                        };
                        let name = b
                            .get(1)
                            .and_then(|o| s(o, "target"))
                            .or_else(|| Path::new(src).file_name()?.to_str())
                            .unwrap_or(src);
                        prefix.join("bin").join(name)
                    }
                };
                // Only executables on the prefix's PATH; completions land elsewhere.
                let in_bin = path
                    .parent()
                    .is_some_and(|p| p == prefix.join("bin") || p == prefix.join("sbin"));
                if let (true, Some(bin_name)) = (in_bin, path.file_name().and_then(|n| n.to_str()))
                {
                    out.push(BinEntry {
                        package: token.to_string(),
                        bin: bin_name.to_string(),
                        path,
                    });
                }
            }
        }
        out.sort_by(|a, b| (&a.package, &a.bin).cmp(&(&b.package, &b.bin)));
        out.dedup();
        Ok(out)
    }

    /// Resolves each path through its symlinks and reads the `Cellar/<name>` (or
    /// `Caskroom/<token>`) segment under a known brew prefix. No commands are run.
    fn owner_of(&self, ctx: &Ctx, paths: &[PathBuf]) -> Result<BTreeMap<PathBuf, String>> {
        let prefixes = self.prefixes(ctx);
        let mut out = BTreeMap::new();
        for p in paths {
            let real = std::fs::canonicalize(p).unwrap_or_else(|_| p.clone());
            for cand in [&real, p] {
                if let Some(name) = owner_in(cand, &prefixes) {
                    out.insert(p.clone(), name);
                    break;
                }
            }
        }
        Ok(out)
    }
}

fn owner_in(path: &Path, prefixes: &BTreeSet<PathBuf>) -> Option<String> {
    for prefix in prefixes {
        for dir in ["Cellar", "Caskroom"] {
            if let Ok(rest) = path.strip_prefix(prefix.join(dir))
                && let Some(name) = rest.components().next()
            {
                return Some(name.as_os_str().to_string_lossy().into_owned());
            }
        }
    }
    None
}

impl BrewProvider {
    fn availability_for(&self, env: &BTreeMap<String, String>, facts: &Facts) -> Availability {
        if let Some(bin_path) = self.locate(env) {
            return Availability::Ready { bin_path };
        }
        match facts.os.as_str() {
            "macos" | "linux" => Availability::Bootstrap {
                pkg: "homebrew".into(),
            },
            _ => Availability::Unsupported,
        }
    }
}

/// Create `<log_dir>/brew-<n>.log` with the first unused `n`.
fn open_log(dir: &Path) -> Result<(std::fs::File, PathBuf)> {
    let err = |e: std::io::Error| ProviderError::Other(format!("brew: creating log: {e}"));
    std::fs::create_dir_all(dir).map_err(err)?;
    for n in 1.. {
        let path = dir.join(format!("{ID}-{n}.log"));
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(f) => return Ok((f, path)),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(err(e)),
        }
    }
    unreachable!()
}
