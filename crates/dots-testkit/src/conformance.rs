//! The provider conformance suite (design D5, spec `provider-contract`). Every provider runs
//! it; real providers against sandboxed real backends, the fake in memory.
//!
//! ```ignore
//! dots_testkit::conformance_test!(brew_conforms, {
//!     let p = BrewProvider::new();
//!     let f = ConformanceFixture::new("jq", "tree").backend(Arc::new(RealRunner));
//!     (p, f)
//! });
//! ```

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use dots_facts::Facts;
use dots_providers::{Action, Availability, Ctx, Op, OpKind, Provider, Runner, Spec, Support};
use tempfile::TempDir;

use crate::RecordingRunner;

/// One failed contract point.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failure {
    /// Short check id, e.g. `installed-is-batched`.
    pub check: &'static str,
    pub message: String,
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[{}] {}", self.check, self.message)
    }
}

/// What the suite needs to know about the environment a provider is tested in.
pub struct ConformanceFixture {
    /// A package that is installed in the backend.
    pub installed_pkg: String,
    /// A package that is not installed (and installable).
    pub missing_pkg: String,
    /// Exact version to install `missing_pkg` at; required when `caps.exact_version != No`
    /// for the version checks (without it they install the latest).
    pub exact_version: Option<String>,
    /// Facts handed to `availability`; `home` is the sandbox home.
    pub facts: Facts,
    /// The provider instance manages one package only (per-package providers such as
    /// `script`): the suite uses `missing_pkg` alone, which must be absent until installed.
    pub single_package: bool,
    /// Records every command the provider runs.
    pub recorder: Arc<RecordingRunner>,
    sandbox: TempDir,
}

impl ConformanceFixture {
    /// A fixture with a sandboxed home, cache and log dir, and a recorder that answers with
    /// empty successful output until a backend is attached.
    pub fn new(installed_pkg: &str, missing_pkg: &str) -> Self {
        let sandbox = tempfile::tempdir().expect("conformance sandbox");
        let facts = Facts {
            home: sandbox.path().join("home").to_string_lossy().into_owned(),
            os: std::env::consts::OS.to_string(),
            arch: std::env::consts::ARCH.to_string(),
            ..Facts::default()
        };
        ConformanceFixture {
            installed_pkg: installed_pkg.into(),
            missing_pkg: missing_pkg.into(),
            exact_version: None,
            facts,
            single_package: false,
            recorder: Arc::new(RecordingRunner::new()),
            sandbox,
        }
    }

    /// For per-package providers: only `missing_pkg` is used (see [`Self::single_package`]).
    pub fn single_package(mut self) -> Self {
        self.single_package = true;
        self
    }

    pub fn exact_version(mut self, v: &str) -> Self {
        self.exact_version = Some(v.into());
        self
    }

    /// Run commands against `backend` (recorded): an in-memory fake, scripted outputs, or
    /// `Arc::new(RealRunner)` for the real thing.
    pub fn backend(mut self, backend: Arc<dyn Runner>) -> Self {
        self.recorder = Arc::new(RecordingRunner::wrapping(backend));
        self
    }

    /// Use a recorder you pre-scripted with `RecordingRunner::on`.
    pub fn recorder(mut self, r: RecordingRunner) -> Self {
        self.recorder = Arc::new(r);
        self
    }

    pub fn sandbox_dir(&self) -> &Path {
        self.sandbox.path()
    }

    /// The `Ctx` the suite uses: recorded runner, sandboxed dirs, and the given `PATH`.
    pub fn ctx_with_path(&self, path: &str) -> Ctx {
        let root = self.sandbox.path();
        let mut env = BTreeMap::new();
        env.insert("PATH".to_string(), path.to_string());
        env.insert("HOME".to_string(), self.facts.home.clone());
        let log_dir = root.join("state/logs/run-1");
        let _ = std::fs::create_dir_all(&log_dir);
        Ctx {
            runner: self.recorder.clone(),
            env,
            home: PathBuf::from(&self.facts.home),
            cache_dir: root.join("cache"),
            log_dir,
            ..Ctx::default()
        }
    }

    /// The default suite context: `PATH` is empty, so nothing may rely on it.
    pub fn ctx(&self) -> Ctx {
        self.ctx_with_path("")
    }
}

fn op(kind: OpKind, name: &str, version: Option<&str>) -> Op {
    Op {
        kind,
        spec: Spec::new(name),
        version: version.map(str::to_string),
    }
}

fn fail(out: &mut Vec<Failure>, check: &'static str, message: impl Into<String>) {
    out.push(Failure {
        check,
        message: message.into(),
    });
}

/// Recursive (path, len, mtime) listing, to prove `plan` wrote nothing.
fn fs_snapshot(dir: &Path) -> Vec<(PathBuf, u64, Option<std::time::SystemTime>)> {
    let mut v = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in rd.flatten() {
            let p = e.path();
            if let Ok(m) = e.metadata() {
                // A directory's own size and mtime say nothing a new or removed entry does
                // not (and on Windows they are refreshed lazily, so they flap).
                if m.is_dir() {
                    v.push((p.clone(), 0, None));
                } else {
                    v.push((p.clone(), m.len(), m.modified().ok()));
                }
                if m.is_dir() {
                    stack.push(p);
                }
            }
        }
    }
    v.sort();
    v
}

/// Run every contract check. An empty result means the provider conforms. Checks that mutate
/// the backend (install) run last.
pub fn run(provider: &dyn Provider, fx: &ConformanceFixture) -> Vec<Failure> {
    let mut f = Vec::new();
    let caps = provider.caps();
    let ctx = fx.ctx();

    // 4. own-binary resolution: independent of PATH.
    fx.recorder.clear();
    let a_empty = provider.availability_in(&ctx, &fx.facts);
    let a_other = provider.availability_in(&fx.ctx_with_path("/usr/bin:/bin"), &fx.facts);
    if a_empty != a_other {
        fail(
            &mut f,
            "binary-resolution",
            format!(
                "availability changes with PATH: empty PATH gave {a_empty:?}, /usr/bin:/bin gave {a_other:?}; resolve the binary from known locations or facts"
            ),
        );
    }
    match &a_empty {
        Availability::Ready { bin_path } if !bin_path.is_absolute() => fail(
            &mut f,
            "binary-resolution",
            format!("Ready.bin_path {bin_path:?} is not absolute (would depend on PATH)"),
        ),
        Availability::Ready { .. } => {}
        other => {
            fail(
                &mut f,
                "fixture",
                format!(
                    "provider is {other:?}, not Ready, in the fixture environment; runtime checks skipped"
                ),
            );
        }
    }
    for c in fx.recorder.commands() {
        if !c.program.is_absolute() {
            fail(
                &mut f,
                "binary-resolution",
                format!(
                    "availability ran `{}` via PATH lookup; use an absolute program path",
                    c.line()
                ),
            );
        }
    }
    let ready = matches!(a_empty, Availability::Ready { .. });

    // 2. plan is pure.
    let other = if fx.single_package {
        &fx.missing_pkg
    } else {
        &fx.installed_pkg
    };
    let both: Vec<Op> = if fx.single_package {
        vec![op(OpKind::Install, &fx.missing_pkg, None)]
    } else {
        vec![
            op(OpKind::Install, &fx.missing_pkg, None),
            op(OpKind::Install, other, None),
        ]
    };
    fx.recorder.clear();
    let before = fs_snapshot(fx.sandbox_dir());
    let p1 = provider.plan(&both);
    let p2 = provider.plan(&both);
    if fx.recorder.count() != 0 {
        fail(
            &mut f,
            "plan-pure",
            format!("plan ran commands: {:?}", fx.recorder.lines()),
        );
    }
    let after = fs_snapshot(fx.sandbox_dir());
    if after != before {
        let changed: Vec<_> = after
            .iter()
            .filter(|e| !before.contains(e))
            .chain(before.iter().filter(|e| !after.contains(e)))
            .collect();
        fail(
            &mut f,
            "plan-pure",
            format!("plan changed files in the sandbox: {changed:?}"),
        );
    }
    match (&p1, &p2) {
        (Ok(a), Ok(b)) if a == b => {}
        (Ok(_), Ok(_)) => fail(
            &mut f,
            "plan-pure",
            "same ops produced different actions on two calls",
        ),
        (Err(e), _) | (_, Err(e)) => fail(
            &mut f,
            "plan-pure",
            format!("plan of two installs failed: {e}"),
        ),
    }

    // 3. capabilities are honored.
    if let Ok(actions) = &p1 {
        if !caps.batch_install && actions.len() != both.len() {
            fail(
                &mut f,
                "caps-batch",
                format!(
                    "batch_install is false but {} install ops produced {} actions (want one per op)",
                    both.len(),
                    actions.len()
                ),
            );
        }
        if caps.batch_install && actions.len() != 1 {
            fail(
                &mut f,
                "caps-batch",
                format!(
                    "batch_install is true but 2 install ops produced {} actions (want one batched action)",
                    actions.len()
                ),
            );
        }
        if actions.iter().any(|a| a.argv.is_empty()) {
            fail(&mut f, "plan-argv", "an action has an empty argv");
        }
        if actions.iter().any(|a| a.needs_root != caps.needs_root) {
            fail(
                &mut f,
                "caps-root",
                format!(
                    "caps.needs_root is {} but an action's needs_root differs; mark every action",
                    caps.needs_root
                ),
            );
        }
        if actions.iter().flat_map(|a| &a.ops).count() != both.len() {
            fail(
                &mut f,
                "plan-ops",
                "actions do not carry exactly the planned ops",
            );
        }
    }
    if caps.exact_version == Support::No
        && provider
            .plan(&[op(OpKind::Install, &fx.missing_pkg, Some("1.2.3"))])
            .is_ok()
    {
        fail(
            &mut f,
            "caps-version",
            "exact_version is No: planning an install with a version must return Err (it may not silently ignore the version)",
        );
    }
    if !caps.uninstall {
        match provider.plan(&[op(OpKind::Remove, other, None)]) {
            Err(e) if e.to_string().contains("cannot uninstall") => {}
            Err(e) => fail(
                &mut f,
                "caps-uninstall",
                format!("Remove error must say \"cannot uninstall\", got: {e}"),
            ),
            Ok(_) => fail(
                &mut f,
                "caps-uninstall",
                "uninstall is false but plan accepted a Remove",
            ),
        }
    }
    if caps.pin && provider.plan(&[op(OpKind::Pin, other, None)]).is_err() {
        fail(&mut f, "caps-pin", "pin is true but plan rejected a Pin op");
    }

    if !ready {
        return f;
    }

    // 1. installed() is at most one batched call (zero when read from disk), and reports the fixture state.
    fx.recorder.clear();
    match provider.installed(&ctx) {
        Ok(map) => {
            if !fx.single_package && !map.contains_key(&fx.installed_pkg) {
                fail(
                    &mut f,
                    "installed-content",
                    format!(
                        "installed() lacks `{}`; got {:?}",
                        fx.installed_pkg,
                        map.keys().collect::<Vec<_>>()
                    ),
                );
            }
            if map.contains_key(&fx.missing_pkg) {
                fail(
                    &mut f,
                    "installed-content",
                    format!(
                        "installed() reports `{}`, which must be absent",
                        fx.missing_pkg
                    ),
                );
            }
        }
        Err(e) => fail(
            &mut f,
            "installed-content",
            format!("installed() failed: {e}"),
        ),
    }
    let n = fx.recorder.count();
    if n > 1 {
        fail(
            &mut f,
            "installed-batched",
            format!(
                "installed() made {n} backend calls (want at most 1: zero when state is read from disk): {:?}",
                fx.recorder.lines()
            ),
        );
    }
    for c in fx.recorder.commands() {
        if !c.program.is_absolute() {
            fail(
                &mut f,
                "binary-resolution",
                format!("installed() ran `{}` via PATH lookup", c.line()),
            );
        }
    }

    // 5 + 6. exec runs Action.argv (plus any follow-up such as a check), logs, and installed() then reports the new version.
    let ver = if caps.exact_version == Support::No {
        None
    } else {
        fx.exact_version.as_deref()
    };
    let install = op(OpKind::Install, &fx.missing_pkg, ver);
    let actions = match provider.plan(std::slice::from_ref(&install)) {
        Ok(a) if !a.is_empty() => a,
        Ok(_) => {
            fail(&mut f, "exec", "plan of an install returned no actions");
            return f;
        }
        Err(e) => {
            fail(&mut f, "exec", format!("plan of an install failed: {e}"));
            return f;
        }
    };
    for action in &actions {
        exec_checks(provider, fx, &ctx, action, &mut f);
    }
    match provider.installed(&ctx) {
        Ok(map) => match map.get(&fx.missing_pkg) {
            None => fail(
                &mut f,
                "roundtrip",
                format!("after install, installed() lacks `{}`", fx.missing_pkg),
            ),
            Some(i) => {
                if i.version.is_empty() {
                    fail(&mut f, "roundtrip", "installed version is empty");
                }
                if let Some(v) = ver
                    && i.version != v
                {
                    fail(
                        &mut f,
                        "roundtrip",
                        format!("installed {v}, but installed() reports {}", i.version),
                    );
                }
            }
        },
        Err(e) => fail(
            &mut f,
            "roundtrip",
            format!("installed() after install failed: {e}"),
        ),
    }
    f
}

fn exec_checks(
    provider: &dyn Provider,
    fx: &ConformanceFixture,
    ctx: &Ctx,
    action: &Action,
    f: &mut Vec<Failure>,
) {
    // exec never runs anything under dry_run.
    fx.recorder.clear();
    let dry = Ctx {
        dry_run: true,
        ..ctx.clone()
    };
    match provider.exec(&dry, action) {
        Ok(o) if !o.success => fail(f, "exec-dry-run", "exec under dry_run reported failure"),
        Err(e) => fail(f, "exec-dry-run", format!("exec under dry_run failed: {e}")),
        Ok(_) => {}
    }
    if fx.recorder.count() != 0 {
        fail(
            f,
            "exec-dry-run",
            format!(
                "exec ran commands under dry_run (must run none): {:?}",
                fx.recorder.lines()
            ),
        );
    }

    fx.recorder.clear();
    let outcome = match provider.exec(ctx, action) {
        Ok(o) => o,
        Err(e) => {
            fail(f, "exec", format!("exec failed: {e}"));
            return;
        }
    };
    if !outcome.success {
        fail(f, "exec", "exec reported failure for a valid install");
    }
    let cmds = fx.recorder.commands();
    if !cmds.iter().any(|c| c.argv() == action.argv) {
        fail(
            f,
            "exec-argv",
            format!(
                "exec must run at least one command equal to Action.argv {:?}; ran {:?}",
                action.argv,
                cmds.iter().map(|c| c.argv()).collect::<Vec<_>>()
            ),
        );
    }
    match &outcome.log {
        None => fail(f, "exec-log", "exec returned no log path"),
        Some(p) if !p.starts_with(&ctx.log_dir) => fail(
            f,
            "exec-log",
            format!("log {p:?} is not under the run's log dir {:?}", ctx.log_dir),
        ),
        Some(p) if !p.is_file() => fail(f, "exec-log", format!("log {p:?} does not exist")),
        Some(_) => {}
    }
}

/// Panic with every failure listed; for use in `#[test]`s.
pub fn assert_conforms(provider: &dyn Provider, fx: &ConformanceFixture) {
    let failures = run(provider, fx);
    assert!(
        failures.is_empty(),
        "provider `{}` violates the provider contract:\n{}",
        provider.id(),
        failures
            .iter()
            .map(|f| format!("  - {f}"))
            .collect::<Vec<_>>()
            .join("\n")
    );
}

/// Generate a `#[test]` that runs the conformance suite. The block yields
/// `(provider, ConformanceFixture)`.
#[macro_export]
macro_rules! conformance_test {
    ($name:ident, $setup:block) => {
        #[test]
        fn $name() {
            let (provider, fixture) = $setup;
            $crate::conformance::assert_conforms(&provider, &fixture);
        }
    };
}
