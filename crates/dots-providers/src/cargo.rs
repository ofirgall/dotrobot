//! The `cargo` provider: `cargo install` for crates (registry or git).
//!
//! - `installed()` parses `$CARGO_HOME/.crates2.json` with zero processes (it is exact and
//!   faster than `cargo install --list`); only when that file is missing does it make ONE
//!   `cargo install --list` call. [`Provider::bins`] reads the same file.
//! - `exec()` resolves a bare `cargo` in `argv[0]` (from [`CargoProvider::new`]) through the
//!   same known-locations lookup, never `PATH`.
//! - Per-package options are `Spec::opts` keys: `git=<url>`, `rev=<sha>`, `features=a,b`,
//!   `locked=false`. `locked` defaults to true.
//! - Because `plan` is pure and has no `Ctx`, the provider carries the cargo binary it plans
//!   for: build it with [`CargoProvider::for_ctx`]. [`CargoProvider::new`] plans a bare `cargo`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use dots_facts::Facts;

use crate::pathsrc;
use crate::{
    Action, Availability, BinEntry, Caps, Cmd, Ctx, Installed, Latest, Op, OpKind, Outcome,
    Provider, ProviderError, Result, Spec, Support,
};

const ENV: &str = "/usr/bin/env";
const FETCH_WITH_CLI: &str = "CARGO_NET_GIT_FETCH_WITH_CLI";
const REVS_FILE: &str = ".dots-path-revs.json";

pub(crate) const SYSTEM_PATH: &str =
    "/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin";

/// First existing file among `candidates`.
pub(crate) fn first_file(candidates: impl IntoIterator<Item = PathBuf>) -> Option<PathBuf> {
    candidates.into_iter().find(|p| p.is_file())
}

/// A version requirement as a command-line suffix: `1.2.3` becomes `==1.2.3`; anything that
/// already starts with an operator is kept.
pub(crate) fn pep440_req(v: &str) -> String {
    if v.starts_with(['=', '<', '>', '~', '!']) {
        v.to_string()
    } else {
        format!("=={v}")
    }
}

/// The environment commands run with: the context's, plus a usable `PATH` and `HOME`.
pub(crate) fn command_env(ctx: &Ctx, prefix: Option<&Path>) -> BTreeMap<String, String> {
    let mut env = ctx.env.clone();
    let mut parts: Vec<String> = Vec::new();
    if let Some(p) = prefix {
        parts.push(p.to_string_lossy().into_owned());
    }
    if let Some(p) = env.get("PATH").filter(|p| !p.is_empty()) {
        parts.push(p.clone());
    }
    parts.push(SYSTEM_PATH.to_string());
    env.insert("PATH".into(), parts.join(":"));
    if !env.contains_key("HOME") && !ctx.home.as_os_str().is_empty() {
        env.insert("HOME".into(), ctx.home.to_string_lossy().into_owned());
    }
    env
}

/// Run exactly `action.argv`, write `<pkg>.<action>.log` under the run's log dir.
pub(crate) fn exec_action(
    ctx: &Ctx,
    action: &Action,
    prefix: Option<&Path>,
    extra_env: &[(&str, &str)],
) -> Result<Outcome> {
    if ctx.dry_run {
        return Ok(Outcome {
            success: true,
            log: None,
        });
    }
    let mut cmd = Cmd::from_argv(&action.argv)
        .ok_or_else(|| ProviderError::Other("empty argv".into()))?
        .envs(&command_env(ctx, prefix));
    for (k, v) in extra_env {
        cmd = cmd.env(*k, *v);
    }
    let out = ctx.runner.run(&cmd)?;
    let stem = match action.ops.as_slice() {
        [one] => one.spec.name.replace(['/', '\\'], "_"),
        _ => "batch".to_string(),
    };
    let kind = format!("{:?}", action.kind).to_lowercase();
    let path = ctx.log_dir.join(format!("{stem}.{kind}.log"));
    let text = format!(
        "$ {}\n{}{}# exit status {}\n",
        action.argv.join(" "),
        out.stdout,
        out.stderr,
        out.status
    );
    std::fs::create_dir_all(&ctx.log_dir)
        .and_then(|()| std::fs::write(&path, text))
        .map_err(|e| ProviderError::Other(format!("cannot write {}: {e}", path.display())))?;
    Ok(Outcome {
        success: out.success(),
        log: Some(path),
    })
}

fn cargo_home(home: &Path, env: &BTreeMap<String, String>) -> PathBuf {
    match env.get("CARGO_HOME").filter(|v| !v.is_empty()) {
        Some(c) => PathBuf::from(c),
        None => home.join(".cargo"),
    }
}

fn resolve_bin(home: &Path, env: &BTreeMap<String, String>) -> Option<PathBuf> {
    first_file([
        cargo_home(home, env).join("bin/cargo"),
        home.join(".cargo/bin/cargo"),
        PathBuf::from("/opt/homebrew/bin/cargo"),
        PathBuf::from("/usr/local/bin/cargo"),
        PathBuf::from("/usr/bin/cargo"),
    ])
}

#[derive(Debug, Clone)]
pub struct CargoProvider {
    bin: PathBuf,
}

impl Default for CargoProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl CargoProvider {
    /// Plans a bare `cargo` (not absolute): fine for `plan -v` previews, not for `exec`.
    pub fn new() -> Self {
        CargoProvider {
            bin: PathBuf::from("cargo"),
        }
    }

    pub fn with_bin(bin: impl Into<PathBuf>) -> Self {
        CargoProvider { bin: bin.into() }
    }

    /// Resolve cargo from the context (never `PATH`); falls back to a bare `cargo`.
    pub fn for_ctx(ctx: &Ctx) -> Self {
        match resolve_bin(&ctx.home, &ctx.env) {
            Some(b) => Self::with_bin(b),
            None => Self::new(),
        }
    }
}

impl Provider for CargoProvider {
    fn id(&self) -> &'static str {
        "cargo"
    }

    fn caps(&self) -> Caps {
        Caps {
            batch_install: false,
            exact_version: Support::Yes,
            pin: false,
            needs_root: false,
            uninstall: true,
            exclusive_lock: true,
        }
    }

    fn availability(&self, facts: &Facts) -> Availability {
        availability(Path::new(&facts.home), &BTreeMap::new())
    }

    fn availability_in(&self, ctx: &Ctx, _facts: &Facts) -> Availability {
        availability(&ctx.home, &ctx.env)
    }

    fn installed(&self, ctx: &Ctx) -> Result<BTreeMap<String, Installed>> {
        if let Ok(text) =
            std::fs::read_to_string(cargo_home(&ctx.home, &ctx.env).join(".crates2.json"))
            && let Some(map) = parse_crates2(&text)
        {
            return Ok(with_path_revs(ctx, map));
        }
        let bin = resolve_bin(&ctx.home, &ctx.env).ok_or_else(|| ProviderError::Unavailable {
            provider: "cargo".into(),
            reason: "cargo not found".into(),
        })?;
        let mut cmd = Cmd::new(&bin)
            .args(["install", "--list"])
            .envs(&command_env(ctx, bin.parent()));
        cmd.env.insert(
            "CARGO_HOME".into(),
            cargo_home(&ctx.home, &ctx.env)
                .to_string_lossy()
                .into_owned(),
        );
        let out = ctx.runner.run(&cmd)?;
        if !out.success() {
            return Err(ProviderError::CommandFailed {
                command: cmd.line(),
                status: out.status,
            });
        }
        Ok(with_path_revs(ctx, parse_install_list(&out.stdout)))
    }

    fn latest(&self, ctx: &Ctx, specs: &[Spec]) -> Result<BTreeMap<String, Latest>> {
        let mut map = BTreeMap::new();
        // A local crate's "latest" is its working tree: no network, no cargo.
        for s in specs {
            if let Some(dir) = pathsrc::source_dir(s)
                && let Some(version) = pathsrc::source_version(ctx, &dir)
            {
                map.insert(s.name.clone(), Latest { version });
            }
        }
        let Some(bin) = resolve_bin(&ctx.home, &ctx.env) else {
            return Ok(map);
        };
        if ctx.offline {
            return Ok(map);
        }
        for s in specs {
            if s.opts.contains_key("git") || s.opts.contains_key("path") {
                continue;
            }
            let cmd = Cmd::new(&bin)
                .args(["search", "--limit", "1", &s.name])
                .envs(&command_env(ctx, bin.parent()));
            let out = ctx.runner.run(&cmd)?;
            if !out.success() {
                continue;
            }
            // `name = "1.2.3"    # description`
            if let Some(line) = out.stdout.lines().next()
                && let Some((n, rest)) = line.split_once(" = \"")
                && n == s.name
                && let Some((v, _)) = rest.split_once('"')
            {
                map.insert(
                    s.name.clone(),
                    Latest {
                        version: v.to_string(),
                    },
                );
            }
        }
        Ok(map)
    }

    fn plan(&self, ops: &[Op]) -> Result<Vec<Action>> {
        let bin = self.bin.to_string_lossy().into_owned();
        ops.iter()
            .map(|op| {
                let name = &op.spec.name;
                let argv = match op.kind {
                    OpKind::Install | OpKind::Upgrade => {
                        let o = &op.spec.opts;
                        let mut a = Vec::new();
                        if op.spec.flag("git_fetch_with_cli", false) {
                            a.extend([ENV.to_string(), format!("{FETCH_WITH_CLI}=true")]);
                        }
                        a.extend([bin.clone(), "install".to_string()]);
                        if o.contains_key("git") && o.contains_key("path") {
                            return Err(ProviderError::Other(format!(
                                "cargo: `{name}` sets both `git` and `path`; pick one"
                            )));
                        }
                        if o.get("locked").map(String::as_str) != Some("false") {
                            a.push("--locked".into());
                        }
                        let target = if let Some(dir) = pathsrc::source_dir(&op.spec) {
                            if op.version.is_some() {
                                return Err(ProviderError::Other(format!(
                                    "cargo: `{name}` is installed from a local path; its version comes from the crate"
                                )));
                            }
                            if op.kind == OpKind::Upgrade {
                                a.push("--force".into());
                            }
                            a.extend(["--path".into(), dir.to_string_lossy().into_owned()]);
                            String::new()
                        } else if let Some(git) = o.get("git") {
                            if op.version.is_some() {
                                return Err(ProviderError::Other(format!(
                                    "cargo: `{name}` is installed from git; pin it with rev=, not a version"
                                )));
                            }
                            a.extend(["--git".into(), git.clone()]);
                            if let Some(rev) = o.get("rev") {
                                a.extend(["--rev".into(), rev.clone()]);
                            }
                            name.clone()
                        } else {
                            match &op.version {
                                Some(v) => format!("{name}@{v}"),
                                None => name.clone(),
                            }
                        };
                        if let Some(f) = o.get("features").filter(|f| !f.is_empty()) {
                            a.extend(["--features".into(), f.clone()]);
                        }
                        if !target.is_empty() {
                            a.push(target);
                        }
                        a
                    }
                    OpKind::Remove => vec![bin.clone(), "uninstall".into(), name.clone()],
                    OpKind::Pin | OpKind::Unpin => {
                        return Err(ProviderError::Other(
                            "cargo cannot pin packages".to_string(),
                        ));
                    }
                };
                Ok(Action {
                    kind: op.kind,
                    ops: vec![op.clone()],
                    argv,
                    needs_root: false,
                })
            })
            .collect()
    }

    fn exec(&self, ctx: &Ctx, action: &Action) -> Result<Outcome> {
        let home = cargo_home(&ctx.home, &ctx.env);
        // `env VAR=1 cargo ...` carries the program at index 2.
        let idx = if action.argv.first().map(String::as_str) == Some(ENV) {
            action
                .argv
                .iter()
                .skip(1)
                .position(|a| !a.contains('='))
                .map_or(0, |i| i + 1)
        } else {
            0
        };
        let action = &resolve_bare(action, idx, "cargo", resolve_bin(&ctx.home, &ctx.env))?;
        let bin_dir = Path::new(&action.argv[idx]).parent().map(Path::to_path_buf);
        let out = exec_action(
            ctx,
            action,
            bin_dir.as_deref(),
            &[("CARGO_HOME", &home.to_string_lossy())],
        )?;
        if !out.success && !ctx.dry_run && action.argv.iter().any(|a| a == "--locked") {
            let text = out
                .log
                .as_ref()
                .and_then(|l| std::fs::read_to_string(l).ok())
                .unwrap_or_default();
            if is_compile_error(&text) {
                return Err(ProviderError::Other(format!(
                    "`{}` failed to compile with the crate's locked dependencies; an old \
                     lockfile may not build with the current rustc. Try `locked = false` in \
                     [providers.cargo]{}",
                    action.argv.join(" "),
                    out.log
                        .map(|l| format!(" (log: {})", l.display()))
                        .unwrap_or_default()
                )));
            }
        }
        if out.success && !ctx.dry_run {
            let revs = home.join(REVS_FILE);
            for op in &action.ops {
                match op.kind {
                    OpKind::Remove => pathsrc::forget_rev(&revs, &op.spec.name),
                    _ => {
                        if let Some(dir) = pathsrc::source_dir(&op.spec)
                            && let Some(v) = pathsrc::source_version(ctx, &dir)
                        {
                            pathsrc::record_rev(&revs, &op.spec.name, &v);
                        }
                    }
                }
            }
        }
        Ok(out)
    }

    fn bins(&self, ctx: &Ctx, installed: &BTreeMap<String, Installed>) -> Result<Vec<BinEntry>> {
        let home = cargo_home(&ctx.home, &ctx.env);
        let Ok(text) = std::fs::read_to_string(home.join(".crates2.json")) else {
            return Ok(Vec::new());
        };
        let json: serde_json::Value = serde_json::from_str(&text)
            .map_err(|e| ProviderError::Other(format!(".crates2.json is not valid JSON: {e}")))?;
        let mut out = Vec::new();
        if let Some(installs) = json.get("installs").and_then(|v| v.as_object()) {
            for (key, entry) in installs {
                let Some(name) = key.split(' ').next() else {
                    continue;
                };
                if !installed.contains_key(name) {
                    continue;
                }
                for b in entry
                    .get("bins")
                    .and_then(|b| b.as_array())
                    .into_iter()
                    .flatten()
                    .filter_map(|b| b.as_str())
                {
                    out.push(BinEntry {
                        package: name.to_string(),
                        bin: b.to_string(),
                        path: home.join("bin").join(b),
                    });
                }
            }
        }
        out.sort_by(|a, b| (&a.package, &a.bin).cmp(&(&b.package, &b.bin)));
        Ok(out)
    }
}

/// Does cargo output show a rustc compile failure (as opposed to a network or resolve error)?
fn is_compile_error(text: &str) -> bool {
    text.contains("could not compile") || text.lines().any(|l| l.starts_with("error[E"))
}

/// Path-installed crates report `<crate version>.<n>+<rev>` as of their last install.
fn with_path_revs(ctx: &Ctx, mut map: BTreeMap<String, Installed>) -> BTreeMap<String, Installed> {
    let revs = pathsrc::read_revs(&cargo_home(&ctx.home, &ctx.env).join(REVS_FILE));
    for (name, i) in map.iter_mut() {
        if i.source.as_deref().is_some_and(|s| s.starts_with('/'))
            && let Some(v) = revs.get(name)
        {
            i.version = v.clone();
        }
    }
    map
}

fn availability(home: &Path, env: &BTreeMap<String, String>) -> Availability {
    match resolve_bin(home, env) {
        Some(bin_path) => Availability::Ready { bin_path },
        None => Availability::Bootstrap {
            pkg: "rustup".into(),
        },
    }
}

/// Parse `.crates2.json`: keys are `name version (source)`.
fn parse_crates2(text: &str) -> Option<BTreeMap<String, Installed>> {
    let json: serde_json::Value = serde_json::from_str(text).ok()?;
    let installs = json.get("installs")?.as_object()?;
    let mut map = BTreeMap::new();
    for key in installs.keys() {
        let mut it = key.splitn(3, ' ');
        let (Some(name), Some(version)) = (it.next(), it.next()) else {
            continue;
        };
        let source = it.next().map(|s| s.trim_matches(['(', ')'])).and_then(|s| {
            if s.starts_with("registry+") || s.starts_with("sparse+") {
                None
            } else {
                Some(
                    s.strip_prefix("git+")
                        .or_else(|| s.strip_prefix("path+file://"))
                        .unwrap_or(s)
                        .to_string(),
                )
            }
        });
        map.insert(
            name.to_string(),
            Installed {
                version: version.to_string(),
                pinned: false,
                source,
            },
        );
    }
    Some(map)
}

/// A copy of `action` whose `argv[idx]`, when it is the bare program name `bare`, is replaced
/// by the resolved absolute binary. Errors when the binary cannot be found.
pub(crate) fn resolve_bare(
    action: &Action,
    idx: usize,
    bare: &str,
    resolved: Option<PathBuf>,
) -> Result<Action> {
    let mut a = action.clone();
    if a.argv.get(idx).map(String::as_str) == Some(bare) {
        let bin = resolved.ok_or_else(|| ProviderError::Unavailable {
            provider: bare.to_string(),
            reason: format!("{bare} not found in its known locations"),
        })?;
        a.argv[idx] = bin.to_string_lossy().into_owned();
    }
    Ok(a)
}

/// Parse `cargo install --list`:
/// `name vX.Y.Z:` or `name vX.Y.Z (source):`, followed by indented binary names.
pub(crate) fn parse_install_list(text: &str) -> BTreeMap<String, Installed> {
    let mut map = BTreeMap::new();
    for line in text.lines() {
        if line.starts_with(char::is_whitespace) || !line.ends_with(':') {
            continue;
        }
        let head = line.trim_end_matches(':');
        let mut it = head.splitn(3, ' ');
        let (Some(name), Some(ver)) = (it.next(), it.next()) else {
            continue;
        };
        let Some(version) = ver.strip_prefix('v') else {
            continue;
        };
        let source = it
            .next()
            .map(|s| s.trim_matches(['(', ')']).to_string())
            .filter(|s| !s.is_empty());
        map.insert(
            name.to_string(),
            Installed {
                version: version.to_string(),
                pinned: false,
                source,
            },
        );
    }
    map
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_install_list() {
        let m = parse_install_list(
            "bottom v0.14.4:\n    btm\nfoo v0.1.0 (https://github.com/x/y#abc123):\n    foo\n",
        );
        assert_eq!(m["bottom"].version, "0.14.4");
        assert_eq!(m["bottom"].source, None);
        assert_eq!(
            m["foo"].source.as_deref(),
            Some("https://github.com/x/y#abc123")
        );
    }
}
