//! An in-memory provider. Drives engine, status and preview tests without real package managers.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use dots_facts::Facts;
use dots_providers::{
    Action, Availability, BinEntry, Caps, Cmd, Ctx, Installed, Latest, Op, OpKind, Outcome, Output,
    Provider, ProviderError, Result, Runner, Spec, Support,
};

use crate::conformance::ConformanceFixture;

#[derive(Default)]
struct State {
    installed: BTreeMap<String, Installed>,
    latest: BTreeMap<String, String>,
    /// Every action `exec` ran, in order.
    executed: Vec<Action>,
    /// How many times `installed` was queried (batching assertions).
    installed_calls: usize,
    /// Package names whose install/upgrade fails.
    failing: Vec<String>,
    /// Installed packages `stale_reason` reports (until an install or upgrade of them runs).
    stale: BTreeMap<String, String>,
}

/// Cloneable handle; clones share state, so tests keep one to inspect after handing another over.
#[derive(Clone)]
pub struct FakeProvider {
    id: &'static str,
    caps: Caps,
    availability: Availability,
    state: Arc<Mutex<State>>,
}

/// The fake's in-memory package manager, speaking the argv the fake plans (`<id> list`,
/// `<id> install a b@1.0`, ...). Commands reach it through the `Ctx` runner, so the conformance
/// suite sees the same recorded calls it sees for real providers.
#[derive(Debug, Clone)]
struct FakeBackend(Arc<Mutex<State>>);

impl std::fmt::Debug for State {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("State").finish_non_exhaustive()
    }
}

impl Runner for FakeBackend {
    fn run(&self, cmd: &Cmd) -> Result<Output> {
        let mut st = self.0.lock().unwrap();
        let verb = cmd.args.first().map(String::as_str).unwrap_or_default();
        if verb == "list" {
            st.installed_calls += 1;
            let lines: String = st
                .installed
                .iter()
                .map(|(n, i)| format!("{n}\t{}\t{}\n", i.version, i.pinned))
                .collect();
            return Ok(Output::ok(&lines));
        }
        let kind = match verb {
            "install" => OpKind::Install,
            "upgrade" => OpKind::Upgrade,
            "remove" => OpKind::Remove,
            "pin" => OpKind::Pin,
            "unpin" => OpKind::Unpin,
            other => {
                return Ok(Output::fail(
                    127,
                    &format!("fake: unknown command `{other}`"),
                ));
            }
        };
        let targets: Vec<(String, Option<String>)> = cmd.args[1..]
            .iter()
            .map(|a| match a.split_once('@') {
                Some((n, v)) => (n.to_string(), Some(v.to_string())),
                None => (a.clone(), None),
            })
            .collect();
        if let Some((n, _)) = targets.iter().find(|(n, _)| st.failing.contains(n)) {
            return Ok(Output::fail(1, &format!("fake: {n} failed to {verb}")));
        }
        for (name, version) in targets {
            match kind {
                OpKind::Install | OpKind::Upgrade => {
                    st.stale.remove(&name);
                    let version = version
                        .or_else(|| st.latest.get(&name).cloned())
                        .unwrap_or_else(|| "1.0.0".to_string());
                    let pinned = st.installed.get(&name).is_some_and(|i| i.pinned);
                    st.installed.insert(
                        name,
                        Installed {
                            version,
                            pinned,
                            source: None,
                        },
                    );
                }
                OpKind::Remove => {
                    st.installed.remove(&name);
                }
                OpKind::Pin | OpKind::Unpin => {
                    if let Some(i) = st.installed.get_mut(&name) {
                        i.pinned = kind == OpKind::Pin;
                    }
                }
            }
        }
        Ok(Output::ok(&format!("fake: {verb} done\n")))
    }
}

impl FakeProvider {
    /// The runner commands go through: the context's, unless it is the real process runner
    /// (a default `Ctx`), in which case the in-memory backend is used directly.
    fn runner(&self, ctx: &Ctx) -> Arc<dyn Runner> {
        if ctx.runner.is_real() {
            Arc::new(FakeBackend(self.state.clone()))
        } else {
            ctx.runner.clone()
        }
    }

    fn bin(&self) -> PathBuf {
        PathBuf::from(format!("/fake/bin/{}", self.id))
    }

    /// A conformance fixture matching this fake: `installed_pkg` is installed, `missing_pkg`
    /// is not, and commands run against this fake's backend (recorded).
    pub fn conformance_fixture(&self) -> ConformanceFixture {
        self.set_installed("conf-installed", "1.0.0");
        let mut f = ConformanceFixture::new("conf-installed", "conf-missing");
        if self.caps.exact_version != Support::No {
            f = f.exact_version("2.3.4");
        }
        f.backend(Arc::new(FakeBackend(self.state.clone())))
    }
}

impl FakeProvider {
    pub fn new(id: &'static str) -> Self {
        FakeProvider {
            id,
            caps: Caps {
                batch_install: true,
                exact_version: Support::Yes,
                pin: true,
                needs_root: false,
                uninstall: true,
                exclusive_lock: false,
            },
            availability: Availability::Ready {
                bin_path: PathBuf::from(format!("/fake/bin/{id}")),
            },
            state: Arc::default(),
        }
    }

    pub fn with_caps(mut self, caps: Caps) -> Self {
        self.caps = caps;
        self
    }

    pub fn with_availability(mut self, a: Availability) -> Self {
        self.availability = a;
        self
    }

    pub fn with_installed(self, name: &str, version: &str) -> Self {
        self.set_installed(name, version);
        self
    }

    pub fn with_latest(self, name: &str, version: &str) -> Self {
        self.state
            .lock()
            .unwrap()
            .latest
            .insert(name.into(), version.into());
        self
    }

    /// `stale_reason` answers `reason` for `name` until it is installed or upgraded.
    pub fn with_stale(self, name: &str, reason: &str) -> Self {
        self.state
            .lock()
            .unwrap()
            .stale
            .insert(name.into(), reason.into());
        self
    }

    pub fn failing_on(self, name: &str) -> Self {
        self.state.lock().unwrap().failing.push(name.into());
        self
    }

    pub fn set_installed(&self, name: &str, version: &str) {
        self.state.lock().unwrap().installed.insert(
            name.into(),
            Installed {
                version: version.into(),
                pinned: false,
                source: None,
            },
        );
    }

    /// Actions executed so far.
    pub fn executed(&self) -> Vec<Action> {
        self.state.lock().unwrap().executed.clone()
    }

    pub fn installed_calls(&self) -> usize {
        self.state.lock().unwrap().installed_calls
    }

    pub fn snapshot_installed(&self) -> BTreeMap<String, Installed> {
        self.state.lock().unwrap().installed.clone()
    }
}

impl Provider for FakeProvider {
    fn id(&self) -> &'static str {
        self.id
    }

    fn caps(&self) -> Caps {
        self.caps
    }

    fn availability(&self, _facts: &Facts) -> Availability {
        self.availability.clone()
    }

    fn installed(&self, ctx: &Ctx) -> Result<BTreeMap<String, Installed>> {
        let out = self
            .runner(ctx)
            .run(&Cmd::new(self.bin()).arg("list").envs(&ctx.env))?;
        if !out.success() {
            return Err(ProviderError::CommandFailed {
                command: format!("{} list", self.id),
                status: out.status,
            });
        }
        Ok(out
            .stdout
            .lines()
            .filter_map(|l| {
                let mut it = l.split('\t');
                let (n, v, p) = (it.next()?, it.next()?, it.next()?);
                Some((
                    n.to_string(),
                    Installed {
                        version: v.to_string(),
                        pinned: p == "true",
                        source: None,
                    },
                ))
            })
            .collect())
    }

    fn stale_reason(&self, _ctx: &Ctx, spec: &Spec) -> Option<String> {
        self.state.lock().unwrap().stale.get(&spec.name).cloned()
    }

    fn latest(&self, _ctx: &Ctx, specs: &[Spec]) -> Result<BTreeMap<String, Latest>> {
        let st = self.state.lock().unwrap();
        Ok(specs
            .iter()
            .filter_map(|s| {
                st.latest
                    .get(&s.name)
                    .map(|v| (s.name.clone(), Latest { version: v.clone() }))
            })
            .collect())
    }

    fn plan(&self, ops: &[Op]) -> Result<Vec<Action>> {
        // Group consecutive-by-kind ops into one action when the provider batches.
        let mut actions: Vec<Action> = Vec::new();
        for op in ops {
            if op.kind == OpKind::Remove && !self.caps.uninstall {
                return Err(ProviderError::Other(format!(
                    "{} cannot uninstall",
                    self.id
                )));
            }
            if op.version.is_some() && self.caps.exact_version == Support::No {
                return Err(ProviderError::Other(format!(
                    "{} cannot install an exact version",
                    self.id
                )));
            }
            let batchable = self.caps.batch_install
                && matches!(op.kind, OpKind::Install | OpKind::Upgrade | OpKind::Remove);
            match actions.iter_mut().find(|a| batchable && a.kind == op.kind) {
                Some(a) => {
                    a.ops.push(op.clone());
                    a.argv.push(arg_for(op));
                }
                None => actions.push(Action {
                    kind: op.kind,
                    ops: vec![op.clone()],
                    argv: vec![self.id.to_string(), verb(op.kind).to_string(), arg_for(op)],
                    needs_root: self.caps.needs_root,
                }),
            }
        }
        Ok(actions)
    }

    fn exec(&self, ctx: &Ctx, action: &Action) -> Result<Outcome> {
        if ctx.dry_run {
            return Ok(Outcome {
                success: true,
                log: None,
            });
        }
        self.state.lock().unwrap().executed.push(action.clone());
        let cmd = Cmd::from_argv(&action.argv)
            .ok_or_else(|| ProviderError::Other("empty argv".into()))?
            .envs(&ctx.env);
        let out = self.runner(ctx).run(&cmd)?;
        let log = if ctx.log_dir.as_os_str().is_empty() {
            None
        } else {
            let pkg = action.ops.first().map_or("batch", |o| o.spec.name.as_str());
            let path = ctx.log_dir.join(format!("{pkg}.{}.log", verb(action.kind)));
            std::fs::create_dir_all(&ctx.log_dir)
                .and_then(|()| {
                    std::fs::write(
                        &path,
                        format!("$ {}\n{}{}", cmd.line(), out.stdout, out.stderr),
                    )
                })
                .map_err(|e| ProviderError::Other(format!("cannot write log: {e}")))?;
            Some(path)
        };
        Ok(Outcome {
            success: out.success(),
            log,
        })
    }

    fn bins(&self, _ctx: &Ctx, installed: &BTreeMap<String, Installed>) -> Result<Vec<BinEntry>> {
        Ok(installed
            .keys()
            .map(|n| BinEntry {
                package: n.clone(),
                bin: n.clone(),
                path: PathBuf::from(format!("/fake/bin/{n}")),
            })
            .collect())
    }
}

fn verb(k: OpKind) -> &'static str {
    match k {
        OpKind::Install => "install",
        OpKind::Upgrade => "upgrade",
        OpKind::Remove => "remove",
        OpKind::Pin => "pin",
        OpKind::Unpin => "unpin",
    }
}

fn arg_for(op: &Op) -> String {
    match &op.version {
        Some(v) => format!("{}@{v}", op.spec.name),
        None => op.spec.name.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    crate::conformance_test!(fake_conforms_batched, {
        let p = FakeProvider::new("fake");
        let f = p.conformance_fixture();
        (p, f)
    });

    crate::conformance_test!(fake_conforms_unbatched_root_no_uninstall_no_version, {
        let base = FakeProvider::new("fake").caps();
        let p = FakeProvider::new("fake").with_caps(Caps {
            batch_install: false,
            needs_root: true,
            uninstall: false,
            exact_version: Support::No,
            pin: false,
            ..base
        });
        let f = p.conformance_fixture();
        (p, f)
    });

    #[test]
    fn suite_catches_violations() {
        // A provider that ignores needs_root marking must be reported.
        struct Liar(FakeProvider);
        impl Provider for Liar {
            fn id(&self) -> &'static str {
                "fake"
            }
            fn caps(&self) -> Caps {
                Caps {
                    needs_root: true,
                    ..self.0.caps()
                }
            }
            fn availability(&self, f: &Facts) -> Availability {
                self.0.availability(f)
            }
            fn installed(&self, c: &Ctx) -> Result<BTreeMap<String, Installed>> {
                self.0.installed(c)?;
                self.0.installed(c)
            }
            fn latest(&self, c: &Ctx, s: &[Spec]) -> Result<BTreeMap<String, Latest>> {
                self.0.latest(c, s)
            }
            fn plan(&self, o: &[Op]) -> Result<Vec<Action>> {
                self.0.plan(o)
            }
            fn exec(&self, c: &Ctx, a: &Action) -> Result<Outcome> {
                self.0.exec(c, a)
            }
            fn bins(&self, c: &Ctx, i: &BTreeMap<String, Installed>) -> Result<Vec<BinEntry>> {
                self.0.bins(c, i)
            }
        }
        let inner = FakeProvider::new("fake");
        let fx = inner.conformance_fixture();
        let checks: Vec<_> = crate::conformance::run(&Liar(inner), &fx)
            .into_iter()
            .map(|f| f.check)
            .collect();
        assert!(checks.contains(&"installed-batched"), "{checks:?}");
        assert!(checks.contains(&"caps-root"), "{checks:?}");
    }

    fn op(kind: OpKind, name: &str) -> Op {
        Op {
            kind,
            spec: Spec::new(name),
            version: None,
        }
    }

    #[test]
    fn plan_is_pure_and_batches() {
        let p = FakeProvider::new("fake");
        let actions = p
            .plan(&[op(OpKind::Install, "a"), op(OpKind::Install, "b")])
            .unwrap();
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0].argv, ["fake", "install", "a", "b"]);
        assert!(p.executed().is_empty() && p.snapshot_installed().is_empty());
    }

    #[test]
    fn exec_records_and_mutates() {
        let p = FakeProvider::new("fake")
            .with_installed("a", "1.0.0")
            .with_latest("a", "2.0.0");
        let ctx = Ctx::default();
        let acts = p
            .plan(&[op(OpKind::Upgrade, "a"), op(OpKind::Install, "b")])
            .unwrap();
        for a in &acts {
            assert!(p.exec(&ctx, a).unwrap().success);
        }
        assert_eq!(p.executed().len(), 2);
        let inst = p.installed(&ctx).unwrap();
        assert_eq!(inst["a"].version, "2.0.0");
        assert!(inst.contains_key("b"));
        assert_eq!(p.installed_calls(), 1);
    }

    #[test]
    fn failure_and_capabilities() {
        let p = FakeProvider::new("fake").failing_on("bad");
        let a = p.plan(&[op(OpKind::Install, "bad")]).unwrap();
        assert!(!p.exec(&Ctx::default(), &a[0]).unwrap().success);
        let no_rm = FakeProvider::new("x").with_caps(Caps {
            uninstall: false,
            ..p.caps()
        });
        assert!(no_rm.plan(&[op(OpKind::Remove, "a")]).is_err());
    }
}
