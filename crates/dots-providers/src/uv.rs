//! The `uv` provider: `uv tool` for CLIs, `uv pip` for libraries (library mode).
//!
//! Per-package options are `Spec::opts` keys:
//! `kind=tool` (default) | `kind=lib` with `python=system|<interpreter>` | `kind=venv` with an
//! optional `requirements` file (ARCHITECTURE 5.2): the package name is the venv's name, it
//! lives at `~/.local/share/dots/venvs/<name>`, is created with `uv venv` and filled with
//! `uv pip install -r <requirements>` (a relative path is relative to the package directory).
//! A venv counts as installed when its `pyvenv.cfg` exists; `upgrade` re-runs the pip install
//! with `--upgrade`, which is how a changed requirements file is applied.
//!
//! `installed()` makes ONE `uv tool list` call, plus, only when lib packages are registered
//! ([`UvProvider::lib`]), ONE `uv pip list --format json` call per distinct python target
//! (`--system` or `--python <path>`): at most one call per backend target, never one per
//! package. Registered libs are reported under their registered name, so a second apply is a
//! no-op. `exec()` resolves a bare `uv` in `argv[0]` through the known locations, never `PATH`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use dots_facts::Facts;

use crate::cargo::{command_env, exec_action, first_file, pep440_req, resolve_bare};
use crate::pathsrc;
use crate::{
    Action, Availability, BinEntry, Caps, Cmd, Ctx, Installed, Latest, Op, OpKind, Outcome,
    Provider, ProviderError, Result, Spec, Support,
};

fn resolve_bin(home: &Path) -> Option<PathBuf> {
    first_file([
        home.join(".local/bin/uv"),
        home.join(".cargo/bin/uv"),
        PathBuf::from("/opt/homebrew/bin/uv"),
        PathBuf::from("/usr/local/bin/uv"),
        PathBuf::from("/usr/bin/uv"),
    ])
}

fn availability(home: &Path) -> Availability {
    match resolve_bin(home) {
        Some(bin_path) => Availability::Ready { bin_path },
        None => Availability::Bootstrap { pkg: "uv".into() },
    }
}

/// The command that deletes a venv directory: `rm -rf` on unix, PowerShell's `Remove-Item` on
/// Windows (there is no `rm`).
fn remove_dir_argv(dir: &str) -> Vec<String> {
    if cfg!(windows) {
        let (sh, flags) = crate::script::platform_shell();
        let mut v = vec![sh.to_string_lossy().into_owned()];
        v.extend(flags.iter().map(|f| f.to_string()));
        v.push(format!(
            "Remove-Item -LiteralPath '{}' -Recurse -Force",
            dir.replace('\'', "''")
        ));
        v
    } else {
        ["/bin/rm", "-rf", dir].map(String::from).to_vec()
    }
}

fn default_venv_root(home: &Path) -> PathBuf {
    crate::cargo::native(&home.join(".local/share/dots/venvs"))
}

fn revs_file(ctx: &Ctx) -> PathBuf {
    let tools = tool_dir(ctx);
    tools.parent().unwrap_or(&tools).join("dots-path-revs.json")
}

/// True when the tool's source is a local directory or file: its `tool list` requirement is a
/// path/`file://` URL, or its receipt names a `directory`, `editable` or `path` requirement.
fn is_path_tool(ctx: &Ctx, name: &str, source: Option<&str>) -> bool {
    if source.is_some_and(|s| s.starts_with('/') || s.starts_with("file://")) {
        return true;
    }
    let Ok(text) = std::fs::read_to_string(tool_dir(ctx).join(name).join("uv-receipt.toml")) else {
        return false;
    };
    let marker = format!("name = \"{name}\"");
    text.lines().any(|l| {
        l.contains(&marker)
            && ["directory = \"", "editable = \"", ", path = \""]
                .iter()
                .any(|k| l.contains(k))
    })
}

fn tool_dir(ctx: &Ctx) -> PathBuf {
    if let Some(d) = ctx.env.get("UV_TOOL_DIR").filter(|d| !d.is_empty()) {
        return PathBuf::from(d);
    }
    if let Some(x) = ctx.env.get("XDG_DATA_HOME").filter(|d| !d.is_empty()) {
        return PathBuf::from(x).join("uv/tools");
    }
    ctx.home.join(".local/share/uv/tools")
}

#[derive(Debug, Clone)]
pub struct UvProvider {
    bin: PathBuf,
    /// Where `kind=venv` packages live (`~/.local/share/dots/venvs`).
    venv_root: PathBuf,
    /// Registered library packages: name -> python target (`system` or an interpreter path).
    libs: BTreeMap<String, String>,
    /// Test seam: overrides whether a `break_system_packages` install needs root.
    system_root: Option<bool>,
}

impl Default for UvProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl UvProvider {
    /// Plans a bare `uv` (not absolute): fine for previews, not for `exec`.
    pub fn new() -> Self {
        UvProvider {
            bin: PathBuf::from("uv"),
            venv_root: default_venv_root(Path::new(&std::env::var("HOME").unwrap_or_default())),
            libs: BTreeMap::new(),
            system_root: None,
        }
    }

    pub fn with_bin(bin: impl Into<PathBuf>) -> Self {
        UvProvider {
            bin: bin.into(),
            venv_root: default_venv_root(Path::new(&std::env::var("HOME").unwrap_or_default())),
            libs: BTreeMap::new(),
            system_root: None,
        }
    }

    /// Where `kind=venv` packages are created.
    pub fn with_venv_root(mut self, root: impl Into<PathBuf>) -> Self {
        self.venv_root = root.into();
        self
    }

    /// Register a `kind=lib` package so `installed()` reports it. `python` is `system` or an
    /// interpreter path.
    pub fn lib(mut self, name: &str, python: &str) -> Self {
        self.libs.insert(name.to_string(), python.to_string());
        self
    }

    /// Force whether opted-in system lib installs need root (otherwise probed from the system
    /// python's site-packages).
    pub fn with_system_root(mut self, needs_root: bool) -> Self {
        self.system_root = Some(needs_root);
        self
    }

    /// Does installing into the system python's site-packages need root? Not when we are root
    /// or the directory is writable.
    fn system_needs_root(&self) -> bool {
        if let Some(r) = self.system_root {
            return r;
        }
        if is_root() {
            return false;
        }
        let out = std::process::Command::new("python3")
            .args([
                "-c",
                "import sysconfig; print(sysconfig.get_path('purelib'))",
            ])
            .output();
        match out {
            Ok(o) if o.status.success() => {
                !dir_writable(Path::new(String::from_utf8_lossy(&o.stdout).trim()))
            }
            _ => false,
        }
    }

    /// Resolve uv from the context (never `PATH`); falls back to a bare `uv`.
    pub fn for_ctx(ctx: &Ctx) -> Self {
        let p = match resolve_bin(&ctx.home) {
            Some(b) => Self::with_bin(b),
            None => Self::new(),
        };
        p.with_venv_root(default_venv_root(&ctx.home))
    }
}

impl UvProvider {
    /// `kind=venv`: `uv venv` (a preparatory action, no ops) then `uv pip install -r` for the
    /// package's `requirements`; removal deletes the directory.
    fn plan_venv(&self, bin: &str, op: &Op) -> Result<Vec<Action>> {
        let v = |a: &[&str]| -> Vec<String> { a.iter().map(|s| (*s).to_string()).collect() };
        let name = &op.spec.name;
        let o = &op.spec.opts;
        if name.is_empty() || name.contains(['/', '\\']) || name.starts_with('.') {
            return Err(ProviderError::Other(format!(
                "uv: `{name}` is not usable as a venv name"
            )));
        }
        let dir = self.venv_root.join(name);
        let dir_s = dir.to_string_lossy().into_owned();
        let act = |ops: Vec<Op>, argv: Vec<String>| Action {
            kind: op.kind,
            ops,
            argv,
            needs_root: false,
        };
        let reqs = o.get("requirements").map(|r| {
            let p = Path::new(r);
            match o.get("pkg_dir") {
                Some(d) if p.is_relative() => Path::new(d).join(p),
                _ => p.to_path_buf(),
            }
            .to_string_lossy()
            .into_owned()
        });
        match op.kind {
            OpKind::Pin | OpKind::Unpin => Err(no_pin()),
            OpKind::Remove => Ok(vec![act(vec![op.clone()], remove_dir_argv(&dir_s))]),
            OpKind::Install | OpKind::Upgrade => {
                let mut create = v(&[bin, "venv", "--allow-existing"]);
                if let Some(py) = o.get("python").filter(|p| *p != "system") {
                    create.extend(v(&["--python", py]));
                }
                create.push(dir_s.clone());
                let Some(reqs) = reqs else {
                    return Ok(vec![act(vec![op.clone()], create)]);
                };
                // A venv keeps its interpreter in `bin/` on unix and `Scripts\python.exe` on
                // Windows.
                let python = if cfg!(windows) {
                    dir.join("Scripts").join("python.exe")
                } else {
                    dir.join("bin/python")
                }
                .to_string_lossy()
                .into_owned();
                let mut pip = v(&[bin, "pip", "install", "--python", &python]);
                if op.kind == OpKind::Upgrade {
                    pip.push("--upgrade".into());
                }
                pip.extend(v(&["-r", &reqs]));
                Ok(vec![act(Vec::new(), create), act(vec![op.clone()], pip)])
            }
        }
    }
}

impl UvProvider {
    /// PEP 668 guard: `kind=lib, python=system` on an externally-managed python fails with a
    /// clear error unless the package opted in with `break_system_packages = true`.
    fn refuse_externally_managed(&self, ctx: &Ctx, action: &Action) -> Result<()> {
        for op in &action.ops {
            if !is_system_lib(&op.spec) || break_system(&op.spec) {
                continue;
            }
            if let Some((stdlib, _)) = system_python_dirs(ctx)
                && externally_managed(&stdlib)
            {
                return Err(ProviderError::Other(format!(
                    "uv: the system python is externally managed (PEP 668: {}/EXTERNALLY-MANAGED), \
                     so `{}` cannot be installed into it. Use `kind = \"venv\"` (an isolated venv \
                     under ~/.local/share/dots/venvs), or opt in with \
                     `break_system_packages = true` in [providers.uv] to pass \
                     --break-system-packages (uses sudo when the target is not writable)",
                    stdlib.display(),
                    op.spec.name
                )));
            }
        }
        Ok(())
    }
}

impl Provider for UvProvider {
    fn id(&self) -> &'static str {
        "uv"
    }

    fn caps(&self) -> Caps {
        Caps {
            batch_install: false,
            exact_version: Support::Yes,
            pin: false,
            needs_root: false,
            uninstall: true,
            exclusive_lock: false,
        }
    }

    fn availability(&self, facts: &Facts) -> Availability {
        availability(Path::new(&facts.home))
    }

    fn availability_in(&self, ctx: &Ctx, _facts: &Facts) -> Availability {
        availability(&ctx.home)
    }

    fn installed(&self, ctx: &Ctx) -> Result<BTreeMap<String, Installed>> {
        let bin = resolve_bin(&ctx.home).ok_or_else(|| ProviderError::Unavailable {
            provider: "uv".into(),
            reason: "uv not found".into(),
        })?;
        let cmd = Cmd::new(&bin)
            .args(["tool", "list", "--show-version-specifiers"])
            .envs(&command_env(ctx, bin.parent()));
        let out = ctx.runner.run(&cmd)?;
        if !out.success() {
            return Err(ProviderError::CommandFailed {
                command: cmd.line(),
                status: out.status,
            });
        }
        let mut map = parse_tool_list(&out.stdout);
        // Path sources report the version they had when dots installed them.
        // Only for tools that are still path installs: a tool later reinstalled from a
        // registry keeps a stale sidecar entry, which must not override its real version.
        for (name, version) in pathsrc::read_revs(&revs_file(ctx)) {
            if let Some(i) = map.get_mut(&name)
                && is_path_tool(ctx, &name, i.source.as_deref())
            {
                i.version = version;
            }
        }
        // `kind=venv` packages: one per directory with a `pyvenv.cfg`. Their version is not
        // tracked (`venv`): the package is installed or it is not.
        if let Ok(rd) = std::fs::read_dir(&self.venv_root) {
            for e in rd.flatten() {
                if e.path().join("pyvenv.cfg").is_file()
                    && let Some(name) = e.file_name().to_str()
                {
                    map.entry(name.to_string()).or_insert(Installed {
                        version: "venv".into(),
                        pinned: false,
                        source: None,
                    });
                }
            }
        }
        let targets: std::collections::BTreeSet<&String> = self.libs.values().collect();
        for py in targets {
            let mut cmd = Cmd::new(&bin).args(["pip", "list"]);
            cmd = if py == "system" {
                cmd.arg("--system")
            } else {
                cmd.args(["--python", py])
            };
            let cmd = cmd
                .args(["--format", "json"])
                .envs(&command_env(ctx, bin.parent()));
            let out = ctx.runner.run(&cmd)?;
            if !out.success() {
                return Err(ProviderError::CommandFailed {
                    command: cmd.line(),
                    status: out.status,
                });
            }
            let json: serde_json::Value = serde_json::from_str(&out.stdout)
                .map_err(|e| ProviderError::Other(format!("uv pip list: bad JSON: {e}")))?;
            let pkgs: BTreeMap<String, String> = json
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|p| {
                    Some((
                        norm(p.get("name")?.as_str()?),
                        p.get("version")?.as_str()?.to_string(),
                    ))
                })
                .collect();
            for (name, _) in self.libs.iter().filter(|(_, p)| *p == py) {
                if let Some(version) = pkgs.get(&norm(name)) {
                    map.entry(name.clone()).or_insert(Installed {
                        version: version.clone(),
                        pinned: false,
                        source: None,
                    });
                }
            }
        }
        Ok(map)
    }

    fn latest(&self, ctx: &Ctx, specs: &[Spec]) -> Result<BTreeMap<String, Latest>> {
        // `uv` has no "latest version" query; a dry-run upgrade would install. PyPI's JSON API
        // is the planned source; until an HTTP client exists, report nothing.
        // Only local sources have an answer: their working tree.
        let mut map = BTreeMap::new();
        for s in specs {
            if let Some(dir) = pathsrc::source_dir(s)
                && let Some(version) = pathsrc::source_version(ctx, &dir)
            {
                map.insert(s.name.clone(), Latest { version });
            }
        }
        Ok(map)
    }

    fn plan(&self, ops: &[Op]) -> Result<Vec<Action>> {
        let bin = self.bin.to_string_lossy().into_owned();
        let mut venv_actions: BTreeMap<usize, Vec<Action>> = BTreeMap::new();
        for (i, op) in ops.iter().enumerate() {
            if op.spec.opts.get("kind").map(String::as_str) == Some("venv") {
                venv_actions.insert(i, self.plan_venv(&bin, op)?);
            }
        }
        let planned: Vec<Result<Vec<Action>>> = ops
            .iter()
            .enumerate()
            .map(|(i, op)| {
                if let Some(a) = venv_actions.remove(&i) {
                    return Ok(a);
                }
                let name = &op.spec.name;
                let o = &op.spec.opts;
                let local = match (pathsrc::source_dir(&op.spec), o.get("git")) {
                    (Some(_), Some(_)) => {
                        return Err(ProviderError::Other(format!(
                            "uv: `{name}` sets both `git` and `path`; pick one"
                        )));
                    }
                    (Some(dir), None) => Some(dir.to_string_lossy().into_owned()),
                    (None, Some(git)) => {
                        let git = if git.starts_with("git+") {
                            git.clone()
                        } else {
                            format!("git+{git}")
                        };
                        Some(match o.get("rev") {
                            Some(rev) => format!("{git}@{rev}"),
                            None => git,
                        })
                    }
                    (None, None) => None,
                };
                if local.is_some() && op.version.is_some() {
                    return Err(ProviderError::Other(format!(
                        "uv: `{name}` is installed from a path or git source; pin it with `rev`, not a version"
                    )));
                }
                let req = match (&local, &op.version) {
                    (Some(src), _) => src.clone(),
                    (None, Some(v)) => format!("{name}{}", pep440_req(v)),
                    (None, None) => name.clone(),
                };
                let argv: Vec<String> = match o.get("kind").map(String::as_str).unwrap_or("tool") {
                    "tool" => match op.kind {
                        OpKind::Install => v(&[&bin, "tool", "install", &req]),
                        OpKind::Upgrade if op.version.is_some() || local.is_some() => {
                            v(&[&bin, "tool", "install", "--force", &req])
                        }
                        OpKind::Upgrade => v(&[&bin, "tool", "upgrade", name]),
                        OpKind::Remove => v(&[&bin, "tool", "uninstall", name]),
                        OpKind::Pin | OpKind::Unpin => return Err(no_pin()),
                    },
                    "lib" => {
                        let target = match o.get("python").map(String::as_str) {
                            None | Some("system") => v(&["--system"]),
                            Some(py) => v(&["--python", py]),
                        };
                        let mut a = v(&[&bin, "pip"]);
                        let brk = break_system(&op.spec);
                        let brk_flag = |a: &mut Vec<String>| {
                            if brk {
                                a.push("--break-system-packages".into());
                            }
                        };
                        match op.kind {
                            OpKind::Install => {
                                a.push("install".into());
                                a.extend(target);
                                brk_flag(&mut a);
                                a.push(req);
                            }
                            OpKind::Upgrade => {
                                a.push("install".into());
                                a.extend(target);
                                brk_flag(&mut a);
                                a.push("--upgrade".into());
                                a.push(req);
                            }
                            OpKind::Remove => {
                                a.push("uninstall".into());
                                a.extend(target);
                                brk_flag(&mut a);
                                a.push(name.clone());
                            }
                            OpKind::Pin | OpKind::Unpin => return Err(no_pin()),
                        }
                        a
                    }
                    other => {
                        return Err(ProviderError::Other(format!(
                            "uv: kind `{other}` is not supported (use `tool`, `lib` or `venv`)"
                        )));
                    }
                };
                // An opted-in system-wide install writes where the user cannot: root, but only
                // when the target is not writable (and we are not root already).
                let needs_root = break_system(&op.spec) && self.system_needs_root();
                Ok(vec![Action {
                    kind: op.kind,
                    ops: vec![op.clone()],
                    argv,
                    needs_root,
                }])
            })
            .collect();
        Ok(planned
            .into_iter()
            .collect::<Result<Vec<_>>>()?
            .into_iter()
            .flatten()
            .collect())
    }

    fn exec(&self, ctx: &Ctx, action: &Action) -> Result<Outcome> {
        let action = &resolve_bare(action, 0, "uv", resolve_bin(&ctx.home))?;
        if !ctx.dry_run {
            self.refuse_externally_managed(ctx, action)?;
        }
        let bin_dir = Path::new(&action.argv[0]).parent().map(Path::to_path_buf);
        let out = exec_action(ctx, action, bin_dir.as_deref(), &[])?;
        if out.success && !ctx.dry_run {
            let revs = revs_file(ctx);
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
        let dir = tool_dir(ctx);
        let mut out = Vec::new();
        for name in installed.keys() {
            let Ok(text) = std::fs::read_to_string(dir.join(name).join("uv-receipt.toml")) else {
                continue;
            };
            for line in text.lines() {
                if let (Some(bin), Some(path)) = (
                    quoted_after(line, "{ name = \""),
                    quoted_after(line, "install-path = \""),
                ) {
                    out.push(BinEntry {
                        package: name.clone(),
                        bin,
                        path: PathBuf::from(path),
                    });
                }
            }
        }
        out.sort_by(|a, b| (&a.package, &a.bin).cmp(&(&b.package, &b.bin)));
        Ok(out)
    }
}

/// `kind=lib, python=system` with the explicit `break_system_packages = true` opt-in.
fn break_system(spec: &Spec) -> bool {
    let o = &spec.opts;
    o.get("kind").map(String::as_str) == Some("lib")
        && matches!(o.get("python").map(String::as_str), None | Some("system"))
        && spec.flag("break_system_packages", false)
}

fn is_system_lib(spec: &Spec) -> bool {
    let o = &spec.opts;
    o.get("kind").map(String::as_str) == Some("lib")
        && matches!(o.get("python").map(String::as_str), None | Some("system"))
}

/// Ask the system python where its stdlib and site-packages are:
/// `(stdlib, purelib)` from one `python3 -c` call.
fn system_python_dirs(ctx: &Ctx) -> Option<(PathBuf, PathBuf)> {
    let cmd = Cmd::new("python3")
        .args([
            "-c",
            "import sysconfig; print(sysconfig.get_path('stdlib')); print(sysconfig.get_path('purelib'))",
        ])
        .envs(&command_env(ctx, None));
    let out = ctx.runner.run(&cmd).ok().filter(|o| o.success())?;
    let mut l = out.stdout.lines();
    Some((
        PathBuf::from(l.next()?.trim()),
        PathBuf::from(l.next()?.trim()),
    ))
}

/// PEP 668: the stdlib dir holds an `EXTERNALLY-MANAGED` marker.
fn externally_managed(stdlib: &Path) -> bool {
    stdlib.join("EXTERNALLY-MANAGED").is_file()
}

/// Can this process create files in `dir` (or its nearest existing ancestor)?
fn dir_writable(dir: &Path) -> bool {
    let Some(base) = dir.ancestors().find(|p| p.is_dir()) else {
        return false;
    };
    let probe = base.join(format!(".dots-write-probe-{}", std::process::id()));
    let ok = std::fs::File::create(&probe).is_ok();
    let _ = std::fs::remove_file(&probe);
    ok
}

fn is_root() -> bool {
    std::process::Command::new("id")
        .arg("-u")
        .output()
        .ok()
        .is_some_and(|o| String::from_utf8_lossy(&o.stdout).trim() == "0")
}

/// PEP 503 name normalisation.
fn norm(n: &str) -> String {
    n.to_lowercase().replace(['_', '.'], "-")
}

fn v(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|s| s.to_string()).collect()
}

fn no_pin() -> ProviderError {
    ProviderError::Other("uv cannot pin packages".into())
}

fn quoted_after(line: &str, marker: &str) -> Option<String> {
    let rest = &line[line.find(marker)? + marker.len()..];
    Some(rest[..rest.find('"')?].to_string())
}

/// Parse `uv tool list --show-version-specifiers`: `name vX.Y.Z [required: spec]` lines,
/// each followed by `- bin` lines.
pub(crate) fn parse_tool_list(text: &str) -> BTreeMap<String, Installed> {
    let mut map = BTreeMap::new();
    for line in text.lines() {
        if line.starts_with(['-', ' ']) || line.trim().is_empty() {
            continue;
        }
        let mut it = line.splitn(3, ' ');
        let (Some(name), Some(ver)) = (it.next(), it.next()) else {
            continue;
        };
        let Some(version) = ver.strip_prefix('v') else {
            continue;
        };
        let source = it
            .next()
            .and_then(|r| r.trim().strip_prefix("[required: "))
            .and_then(|r| r.strip_suffix(']'))
            .map(str::to_string);
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
    fn parses_tool_list() {
        let m = parse_tool_list(
            "agents-status v0.1.0 [required: file:///x]\n- agents-status-server\nruff v0.4.0\n- ruff\n",
        );
        assert_eq!(m["ruff"].version, "0.4.0");
        assert_eq!(m["agents-status"].source.as_deref(), Some("file:///x"));
        assert_eq!(m.len(), 2);
    }
}
