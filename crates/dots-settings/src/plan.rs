//! Read-before-write planning, applying, status and restoring of settings.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Map, Value};

use crate::ledger::{Ledger, Orig, Owned};
use crate::model::{Declared, Setting};
use crate::sys::{Cur, Observed, Sudo, Sys, Write, commands, dkey, exec, resolve_key};

/// What a step does to a key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    /// Write the declared value.
    Set,
    /// The declaration is gone: write the recorded original value back.
    Restore,
    /// The declaration is gone and the key did not exist before: delete it.
    Delete,
    /// The declaration is gone but nothing can undo the change (`xdg` has no unset): the value
    /// stays as it is and dots stops owning it.
    Keep,
}

impl Op {
    pub fn as_str(self) -> &'static str {
        match self {
            Op::Set => "set",
            Op::Restore => "restore",
            Op::Delete => "delete",
            Op::Keep => "keep",
        }
    }
}

/// One planned change of one key.
#[derive(Debug, Clone, PartialEq)]
pub struct Step {
    pub package: String,
    pub setting: Setting,
    pub op: Op,
    /// The value now.
    pub before: Orig,
    /// The value once the step has run.
    pub after: Orig,
    /// The exact commands.
    pub argv: Vec<Vec<String>>,
    /// A root entry could not be read at plan time (no sudo yet): `before` and `argv` are
    /// worked out when the step is applied, under the run's sudo.
    pub deferred: bool,
}

impl Step {
    pub fn id(&self) -> String {
        self.setting.id()
    }

    pub fn root(&self) -> bool {
        self.setting.root()
    }

    /// `6 -> 2`, `unset -> 2`, `2 -> deleted`.
    pub fn detail(&self) -> String {
        if self.op == Op::Keep {
            let tool = match self.setting {
                Setting::XdgMime { .. } => "xdg-mime",
                _ => "xdg-settings",
            };
            return format!("left as is: {tool} has no unset");
        }
        let show = |o: &Orig| match &o.value {
            Some(v) if o.exists => self.setting.show(v),
            _ => "unset".to_string(),
        };
        let after = if self.op == Op::Delete {
            "deleted".to_string()
        } else {
            show(&self.after)
        };
        let before = if self.deferred {
            "?".to_string()
        } else {
            show(&self.before)
        };
        format!("{before} -> {after}")
    }
}

/// `a == b` for plist-like values: numbers keep their kind (integer or float), floats compare
/// with a tolerance.
pub fn same(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => match (x.is_f64(), y.is_f64()) {
            (true, true) => (x.as_f64().unwrap_or(0.0) - y.as_f64().unwrap_or(1.0)).abs() < 1e-9,
            (false, false) => x == y,
            _ => false,
        },
        (Value::Array(x), Value::Array(y)) => {
            x.len() == y.len() && x.iter().zip(y).all(|(p, q)| same(p, q))
        }
        (Value::Object(x), Value::Object(y)) => {
            x.len() == y.len() && x.iter().all(|(k, v)| y.get(k).is_some_and(|w| same(v, w)))
        }
        _ => a == b,
    }
}

/// Every key of `want` is in `have` with the same value (dict-add semantics).
fn subset(want: &Value, have: &Value) -> bool {
    match (want, have) {
        (Value::Object(w), Value::Object(h)) => w
            .iter()
            .all(|(k, v)| h.get(k).is_some_and(|x| subset(v, x))),
        _ => same(want, have),
    }
}

/// Is this a dotted edit of one dict entry (so a dict value merges instead of replacing)?
fn dotted(setting: &Setting, observed: &Observed) -> bool {
    match setting {
        Setting::Defaults {
            domain, key, root, ..
        } => !resolve_key(&observed.domain(domain, *root), key)
            .path
            .is_empty(),
        _ => false,
    }
}

/// Does `cur` already match what `setting` wants?
pub fn satisfied(setting: &Setting, cur: &Cur, observed: &Observed) -> bool {
    let Cur::Value(have) = cur else { return false };
    let want = setting.wanted();
    if matches!(setting, Setting::Defaults { .. }) && dotted(setting, observed) {
        return subset(&want, have);
    }
    same(&want, have)
}

/// The steps that bring the machine to `declared`, plus the restore steps for ledger entries
/// whose declaration is gone (`orphans`). `declared` is read once per domain, key or mime type.
pub fn plan(
    sys: &Sys,
    declared: &[Declared],
    orphans: &[Owned],
) -> Result<(Vec<Step>, Observed), String> {
    let observed = Observed::read(
        sys,
        declared
            .iter()
            .map(|d| &d.setting)
            .chain(orphans.iter().map(|o| &o.setting)),
    );
    let mut working: BTreeMap<String, Map<String, Value>> = BTreeMap::new();
    for s in declared
        .iter()
        .map(|d| &d.setting)
        .chain(orphans.iter().map(|o| &o.setting))
    {
        if let Setting::Defaults { domain, root, .. } = s {
            working
                .entry(dkey(domain, *root))
                .or_insert_with(|| observed.domain(domain, *root));
        }
    }
    // A root entry that cannot be read yet (sudo is not primed at plan time) is planned as
    // deferred: it is read, and its commands built, when the step runs.
    // An xdg entry whose tool is not installed yet is deferred too: the tool comes with a
    // package of this run, so it is read and set once the installs are done.
    let deferrable = |s: &Setting, e: &str| {
        (s.root() && sys.sudo == Sudo::Unavailable)
            || (no_unset(s) && crate::sys::is_tool_missing(e))
    };
    let mut steps = Vec::new();
    for d in declared {
        let cur = match observed.get(&d.setting) {
            Ok(c) => c,
            Err(e) if deferrable(&d.setting, &e) => {
                steps.push(Step {
                    package: d.package.clone(),
                    setting: d.setting.clone(),
                    op: Op::Set,
                    before: Orig::absent(),
                    after: Orig::of(d.setting.wanted()),
                    argv: Vec::new(),
                    deferred: true,
                });
                continue;
            }
            Err(e) => return Err(format!("{}: {e}", d.setting.label())),
        };
        if satisfied(&d.setting, &cur, &observed) {
            continue;
        }
        let want = d.setting.wanted();
        let merge = dotted(&d.setting, &observed);
        let argv = commands(
            sys,
            &Write {
                setting: &d.setting,
                value: Some(&want),
                merge,
            },
            &mut working,
        );
        // What the key will hold: the merged entry for a dotted dict edit.
        let after = match (&d.setting, &cur, merge) {
            (Setting::Defaults { .. }, Cur::Value(Value::Object(have)), true) => {
                let mut m = have.clone();
                if let Value::Object(w) = &want {
                    m.extend(w.clone());
                }
                Orig::of(Value::Object(m))
            }
            _ => Orig::of(want),
        };
        steps.push(Step {
            package: d.package.clone(),
            setting: d.setting.clone(),
            op: Op::Set,
            before: Orig::from_cur(&cur),
            after,
            argv,
            deferred: false,
        });
    }
    for o in orphans {
        let (op, value) = match &o.original.value {
            Some(v) if o.original.exists => (Op::Restore, Some(v)),
            _ => (Op::Delete, None),
        };
        let cur = match observed.get(&o.setting) {
            Ok(c) => c,
            Err(e) if deferrable(&o.setting, &e) => {
                steps.push(Step {
                    package: o.package.clone(),
                    setting: o.setting.clone(),
                    op,
                    before: Orig::absent(),
                    after: o.original.clone(),
                    argv: Vec::new(),
                    deferred: true,
                });
                continue;
            }
            Err(e) => return Err(format!("{}: {e}", o.setting.label())),
        };
        let original = o.original.as_cur();
        if cur == original {
            continue;
        }
        if op == Op::Delete && no_unset(&o.setting) {
            // Nothing to run: the value stays, and dots stops claiming it.
            steps.push(Step {
                package: o.package.clone(),
                setting: o.setting.clone(),
                op: Op::Keep,
                before: Orig::from_cur(&cur),
                after: Orig::from_cur(&cur),
                argv: Vec::new(),
                deferred: false,
            });
            continue;
        }
        let argv = commands(
            sys,
            &Write {
                setting: &o.setting,
                value,
                merge: false,
            },
            &mut working,
        );
        steps.push(Step {
            package: o.package.clone(),
            setting: o.setting.clone(),
            op,
            before: Orig::from_cur(&cur),
            after: o.original.clone(),
            argv,
            deferred: false,
        });
    }
    Ok((steps, observed))
}

/// `xdg` defaults can be set but never unset.
fn no_unset(s: &Setting) -> bool {
    matches!(s, Setting::XdgMime { .. } | Setting::XdgSetting { .. })
}

/// Run one step and record it in the ledger (saved right away, so a crash leaves accurate
/// ownership).
pub fn apply_step(sys: &Sys, ledger: &mut Ledger, run: &str, step: &Step) -> Result<(), String> {
    apply_step_noted(sys, ledger, run, step).map(|_| ())
}

/// [`apply_step`], returning a note when the step was skipped rather than applied: an xdg
/// tool that is still missing after the packages installed.
pub fn apply_step_noted(
    sys: &Sys,
    ledger: &mut Ledger,
    run: &str,
    step: &Step,
) -> Result<Option<String>, String> {
    if step.deferred {
        if step.root() && sys.sudo == Sudo::Unavailable {
            return Err(format!(
                "`{}` needs root, but sudo is not available (the password prompt failed or was declined)",
                step.setting.label()
            ));
        }
        // Now that sudo is primed: read it for real, and do what is still needed.
        let fresh = if step.op == Op::Set {
            plan(
                sys,
                &[Declared {
                    package: step.package.clone(),
                    setting: step.setting.clone(),
                }],
                &[],
            )
        } else {
            plan(
                sys,
                &[],
                &[Owned {
                    package: step.package.clone(),
                    setting: step.setting.clone(),
                    original: step.after.clone(),
                    run: run.to_string(),
                }],
            )
        }?;
        return match fresh.0.into_iter().next() {
            // Still deferred under a primed sudo: the tool is still missing.
            Some(s) if s.deferred => Ok(Some(format!(
                "skipped: {} is still not installed",
                match s.setting {
                    Setting::XdgMime { .. } => "xdg-mime",
                    _ => "xdg-settings",
                }
            ))),
            Some(s) => apply_step_noted(sys, ledger, run, &s),
            None if step.op == Op::Set => Ok(None),
            None => {
                // Already back to its original: only the ownership is left to drop.
                ledger.record_release(run, &step.setting, step.after.clone(), step.after.clone());
                ledger.save().map(|()| None)
            }
        };
    }
    for a in &step.argv {
        exec(sys, a, step.root())?;
    }
    match step.op {
        Op::Keep => {
            ledger.owned.remove(&step.id());
            return ledger.save().map(|()| None);
        }
        Op::Set => ledger.record(
            run,
            &step.package,
            &step.setting,
            step.before.clone(),
            step.after.clone(),
        ),
        Op::Restore | Op::Delete => {
            ledger.record_release(run, &step.setting, step.before.clone(), step.after.clone())
        }
    }
    ledger.save().map(|()| None)
}

/// Apps to restart, from the steps that were applied: each named once, in first-seen order.
/// `requires = ["restart-app:<name>"]` feeds the same list.
pub fn restarts(applied: &[&Step]) -> Vec<String> {
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for s in applied {
        let named = s.setting.restart().iter().map(String::as_str);
        let required = s
            .setting
            .requires()
            .iter()
            .filter_map(|r| r.strip_prefix("restart-app:"));
        for app in named.chain(required) {
            if seen.insert(app.to_string()) {
                out.push(app.to_string());
            }
        }
    }
    out
}

/// What a change needs from the user before it takes effect (`logout`, `reboot`), once each.
pub fn needs(applied: &[&Step]) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for s in applied {
        for r in s.setting.requires() {
            if (r == "logout" || r == "reboot")
                && !out.iter().any(|(p, k)| p == &s.package && k == r)
            {
                out.push((s.package.clone(), r.clone()));
            }
        }
    }
    out
}

/// The text of the manual action a `logout` / `reboot` requirement raises. The engine and
/// [`settled`] must agree on it: it is how the action is found again to close it.
pub fn todo_reason(need: &str, label: &str) -> String {
    format!("{need}: {label} only takes effect after it")
}

/// A `logout` / `reboot` requirement whose check now passes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settled {
    pub package: String,
    pub need: String,
    pub label: String,
    /// [`todo_reason`] of the manual action to close.
    pub reason: String,
}

/// Which of the declared entries that need a logout or reboot are `ok` now (the machine reads
/// as declared), so the manual actions raised for them can close. One batched read.
pub fn settled(sys: &Sys, declared: &[Declared]) -> Vec<Settled> {
    let waiting: Vec<&Declared> = declared
        .iter()
        .filter(|d| {
            d.setting
                .requires()
                .iter()
                .any(|r| r == "logout" || r == "reboot")
        })
        .collect();
    let observed = Observed::read(sys, waiting.iter().map(|d| &d.setting));
    let mut out = Vec::new();
    for d in waiting {
        let ok = observed
            .get(&d.setting)
            .is_ok_and(|cur| satisfied(&d.setting, &cur, &observed));
        if !ok {
            continue;
        }
        let label = d.setting.label();
        for need in d
            .setting
            .requires()
            .iter()
            .filter(|r| *r == "logout" || *r == "reboot")
        {
            out.push(Settled {
                package: d.package.clone(),
                need: need.clone(),
                reason: todo_reason(need, &label),
                label: label.clone(),
            });
        }
    }
    out
}

/// The state of a declared setting, as `status` shows it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Health {
    Ok,
    /// dots changed it and someone changed it since.
    Drift,
    /// It differs and dots has never applied it.
    Missing,
    /// Reading it failed.
    Failed,
    /// It could not be read for lack of a cached sudo credential (a `root` entry): neither
    /// healthy nor known to be broken.
    Unknown,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    pub package: String,
    pub setting: Setting,
    pub health: Health,
    pub observed: Option<String>,
    pub wanted: String,
    /// The raw observed value, for `adopt`.
    pub observed_value: Option<Value>,
    pub error: Option<String>,
}

/// One batched read, then a row per declared setting.
pub fn status(sys: &Sys, ledger: &Ledger, declared: &[Declared]) -> Vec<Row> {
    let observed = Observed::read(sys, declared.iter().map(|d| &d.setting));
    declared
        .iter()
        .map(|d| {
            let s = &d.setting;
            let wanted = s.show(&s.wanted());
            match observed.get(s) {
                Err(e) => Row {
                    package: d.package.clone(),
                    setting: s.clone(),
                    health: if e == crate::sys::NEEDS_SUDO_READ || crate::sys::is_tool_missing(&e) {
                        Health::Unknown
                    } else {
                        Health::Failed
                    },
                    observed: None,
                    wanted,
                    observed_value: None,
                    error: Some(e),
                },
                Ok(cur) => {
                    let ok = satisfied(s, &cur, &observed);
                    let health = if ok {
                        Health::Ok
                    } else if ledger.owned.contains_key(&s.id()) {
                        Health::Drift
                    } else {
                        Health::Missing
                    };
                    Row {
                        package: d.package.clone(),
                        setting: s.clone(),
                        health,
                        observed: Some(
                            cur.value()
                                .map_or_else(|| "unset".to_string(), |v| s.show(v)),
                        ),
                        wanted,
                        observed_value: cur.value().cloned(),
                        error: None,
                    }
                }
            }
        })
        .collect()
}

/// The result of `dots restore <run-id>` for settings.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Restored {
    /// Labels of the settings that went back.
    pub restored: Vec<String>,
    /// Settings left alone because they changed after the run, with the reason.
    pub skipped: Vec<(String, String)>,
}

/// Undo the settings a run changed. `None` when the run changed none. A setting that someone
/// changed after the run is left alone and reported.
pub fn restore_run(
    sys: &Sys,
    ledger: &mut Ledger,
    run: &str,
    dry_run: bool,
) -> Result<Option<Restored>, String> {
    let changes: Vec<_> = ledger.run_changes(run).into_iter().cloned().collect();
    if changes.is_empty() {
        return Ok(None);
    }
    let observed = Observed::read(sys, changes.iter().map(|c| &c.setting));
    let mut working: BTreeMap<String, Map<String, Value>> = BTreeMap::new();
    for c in &changes {
        if let Setting::Defaults { domain, root, .. } = &c.setting {
            working
                .entry(dkey(domain, *root))
                .or_insert_with(|| observed.domain(domain, *root));
        }
    }
    let mut out = Restored::default();
    let mut done: Vec<(String, Orig)> = Vec::new();
    for c in changes.iter().rev() {
        let cur = observed.get(&c.setting)?;
        let unchanged = match (&cur, &c.after.value) {
            (Cur::Absent, _) => !c.after.exists,
            (Cur::Value(have), Some(want)) => c.after.exists && same(want, have),
            _ => false,
        };
        // Two changes of one key in one run: the earlier restore sees the later one's value.
        let unchanged = unchanged || done.iter().any(|(id, _)| *id == c.id);
        if !unchanged {
            out.skipped.push((
                c.setting.label(),
                format!("`{}` changed after this run", c.setting.label()),
            ));
            continue;
        }
        let value = c.before.value.as_ref().filter(|_| c.before.exists);
        if value.is_none() && no_unset(&c.setting) {
            out.skipped.push((
                c.setting.label(),
                format!(
                    "`{}` stays as it is: it did not exist before and xdg has no unset",
                    c.setting.label()
                ),
            ));
            continue;
        }
        if !dry_run {
            let argv = commands(
                sys,
                &Write {
                    setting: &c.setting,
                    value,
                    merge: false,
                },
                &mut working,
            );
            let mut failed = None;
            for a in &argv {
                if let Err(e) = exec(sys, a, c.setting.root()) {
                    failed = Some(e);
                    break;
                }
            }
            if let Some(e) = failed {
                out.skipped.push((c.setting.label(), e));
                continue;
            }
        }
        out.restored.push(c.setting.label());
        done.push((c.id.clone(), c.before.clone()));
    }
    if !dry_run {
        ledger.forget_run(run, &done);
        ledger.save()?;
    }
    Ok(Some(out))
}
