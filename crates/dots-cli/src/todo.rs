//! `dots todo`: the manual actions of `packages/<pkg>/manual/*.md` (spec `manual-actions`).
//!
//! What is pending is decided here from three things: the actions of the packages that are
//! active on this machine (and whose `when` holds), the per-machine record in `state.json`
//! (`dots_engine::State::manual_status`: a record only counts for its revision), and the
//! actions' own `check`s. Checks run in two places only, as the spec says: at the end of
//! `apply` and on `dots todo check`. `status`, `plan` and `dots todo` just read the record, so
//! they stay instant and never run user commands.
//!
//! Left for later: `interactive` actions are parsed and badged but `apply` does not yet walk
//! them one by one, and `show` prints the markdown as plain text (no termimad rendering, no
//! "open this link" key).

use std::collections::BTreeSet;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime};

use dots_core::report::Diagnostic;
use dots_core::{Block, Cell, ExitCode, Report, Row, Section, State as UiState, Table, Tone};
use dots_engine::{ManualStatus, State, rfc3339};
use dots_model::{Address, After, ManualAction, Requires, Sub};
use dots_providers::Ctx;
use dots_resolve::{InstalledMap, Resolution};
use serde_json::{Value, json};

use crate::app::Context;
use crate::cli::TodoAction;
use crate::context::{RepoCtx, load_repo};
use crate::inspect::{Loaded, git_info, load};

/// How long one `check` may run before it counts as "not yet".
const CHECK_TIMEOUT: Duration = Duration::from_secs(10);

/// One manual action that applies to this machine, with where it stands.
pub struct Entry<'a> {
    pub action: &'a ManualAction,
    pub status: ManualStatus,
}

fn condition_holds(rc: &RepoCtx, when: &str) -> bool {
    let tags: BTreeSet<String> = rc.m.machine.tags.iter().cloned().collect();
    let declared: BTreeSet<String> = rc.repo.manifest.tags.keys().cloned().collect();
    // A condition that cannot be evaluated (lint reports it) must not hide the action.
    dots_facts::Condition::parse(when)
        .and_then(|c| {
            c.eval(&dots_facts::EvalCtx {
                facts: &rc.m.facts,
                chain: &rc.chain,
                tags: &tags,
                declared_tags: &declared,
                settings: &dots_facts::NoSettings,
            })
        })
        .unwrap_or(true)
}

/// The actions that exist on this machine: the package is active, `when` holds, and for
/// `after: install` the package is installed (the provider says so, or dots owns it).
pub fn entries<'a>(
    rc: &'a RepoCtx,
    resolution: &Resolution,
    installed: &InstalledMap,
    state: &State,
) -> Vec<Entry<'a>> {
    let mut out = Vec::new();
    for res in resolution.active() {
        let Some(pkg) = rc.repo.packages.get(&res.name) else {
            continue;
        };
        let is_installed = match res.chosen_provider() {
            // Files only: nothing to install, so "installed" is moot.
            None => true,
            Some(c) => {
                installed
                    .get(&c.id)
                    .is_some_and(|m| m.contains_key(&c.native_name))
                    || state.packages.contains_key(&res.name)
            }
        };
        for a in &pkg.manual {
            if a.after == After::Install && !is_installed {
                continue;
            }
            if a.when.as_deref().is_some_and(|w| !condition_holds(rc, w)) {
                continue;
            }
            out.push(Entry {
                action: a,
                status: state.manual_status(&a.full_id(), a.revision),
            });
        }
    }
    out
}

fn entries_of<'a>(l: &'a Loaded, state: &State) -> Vec<Entry<'a>> {
    entries(&l.rc, &l.resolution, &l.installed, state)
}

/// Run `check` the way a hook runs: `/bin/sh -c` in the package directory, stdin closed, output
/// dropped, killed after [`CHECK_TIMEOUT`]. Exit 0 is "done".
pub fn run_check(action: &ManualAction, env: &std::collections::BTreeMap<String, String>) -> bool {
    let Some(check) = &action.check else {
        return false;
    };
    let dir = action.path.parent().and_then(Path::parent);
    let mut cmd = Command::new("/bin/sh");
    cmd.args(["-c", check])
        .env_clear()
        .envs(env)
        .env("DOTS_PKG", &action.package)
        .env("DOTS_MANUAL", &action.id)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if let Some(d) = dir {
        cmd.current_dir(d);
    }
    let Ok(mut child) = cmd.spawn() else {
        return false;
    };
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.success(),
            Ok(None) if started.elapsed() < CHECK_TIMEOUT => {
                std::thread::sleep(Duration::from_millis(15));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return false;
            }
        }
    }
}

/// The environment a check runs in: the provider environment (so `PATH` includes what dots
/// installed) with `HOME` set to the home dots writes to, as for hooks (`--home` redirects it).
pub fn check_env(pctx: &Ctx, dirs: &dots_core::Dirs) -> std::collections::BTreeMap<String, String> {
    let mut env = pctx.env.clone();
    env.insert("HOME".into(), dirs.home.to_string_lossy().into_owned());
    env
}

/// What a round of checks changed.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Checked {
    /// Closed as `auto` because the check passed.
    pub closed: Vec<String>,
    /// An `after: always` action that had been closed by its check and no longer passes.
    pub reopened: Vec<String>,
    /// Checks that ran and did not pass.
    pub not_yet: Vec<String>,
}

/// Run the checks of the pending actions, and re-run the checks of `after: always` actions an
/// earlier check closed. Updates `state`; returns what changed.
pub fn check_all(
    entries: &[Entry<'_>],
    state: &mut State,
    env: &std::collections::BTreeMap<String, String>,
    repo_rev: Option<&str>,
) -> Checked {
    let mut out = Checked::default();
    for e in entries {
        let a = e.action;
        if a.check.is_none() {
            continue;
        }
        let id = a.full_id();
        match e.status {
            ManualStatus::Pending => {
                if run_check(a, env) {
                    state.close_manual(
                        &id,
                        a.revision,
                        ManualStatus::Auto,
                        rfc3339(SystemTime::now()),
                        repo_rev.map(str::to_string),
                        None,
                    );
                    out.closed.push(id);
                } else {
                    out.not_yet.push(id);
                }
            }
            ManualStatus::Auto if a.after == After::Always && !run_check(a, env) => {
                state.reopen_manual(&id);
                out.reopened.push(id);
            }
            _ => {}
        }
    }
    out
}

/// `2 manual actions pending, 1 requires logout`.
pub fn summary_line(pending: &[&ManualAction]) -> Option<String> {
    if pending.is_empty() {
        return None;
    }
    let mut line = format!(
        "{} {}",
        pending.len(),
        if pending.len() == 1 {
            "manual action pending"
        } else {
            "manual actions pending"
        }
    );
    let count =
        |f: &dyn Fn(&Requires) -> bool| pending.iter().filter(|a| a.requires.iter().any(f)).count();
    let mut needs = Vec::new();
    let (lo, rb) = (
        count(&|r| *r == Requires::Logout),
        count(&|r| *r == Requires::Reboot),
    );
    if lo > 0 {
        needs.push(format!("{lo} {} logout", verb(lo)));
    }
    if rb > 0 {
        needs.push(format!("{rb} {} reboot", verb(rb)));
    }
    let apps: BTreeSet<&str> = pending
        .iter()
        .flat_map(|a| &a.requires)
        .filter_map(|r| match r {
            Requires::RestartApp(n) => Some(n.as_str()),
            _ => None,
        })
        .collect();
    if !apps.is_empty() {
        let list: Vec<&str> = apps.into_iter().collect();
        needs.push(format!("restart {}", list.join(", ")));
    }
    if !needs.is_empty() {
        line.push_str(&format!(", {}", needs.join(", ")));
    }
    Some(line)
}

fn verb(n: usize) -> &'static str {
    if n == 1 { "requires" } else { "require" }
}

fn pending_of<'a>(entries: &[Entry<'a>]) -> Vec<&'a ManualAction> {
    entries
        .iter()
        .filter(|e| e.status == ManualStatus::Pending)
        .map(|e| e.action)
        .collect()
}

/// The note `apply` and `plan` end with, and the data that goes with it.
pub fn pending_block(pending: &[&ManualAction]) -> Option<Block> {
    summary_line(pending).map(Block::callout)
}

pub fn pending_json(pending: &[&ManualAction]) -> Value {
    json!({
        "count": pending.len(),
        "ids": pending.iter().map(|a| a.full_id()).collect::<Vec<_>>(),
        "requires_logout": pending.iter().filter(|a| a.requires.contains(&Requires::Logout)).count(),
        "requires_reboot": pending.iter().filter(|a| a.requires.contains(&Requires::Reboot)).count(),
    })
}

/// `dots plan`: the pending manual actions go into the Heads up section (made if the plan has
/// none) and into the JSON, so nobody is surprised that a clean plan is not the whole story.
pub fn plan_heads_up(report: &mut Report, pending: &[&ManualAction]) {
    let Some(line) = summary_line(pending) else {
        return;
    };
    if let Some(obj) = report.data.as_object_mut() {
        obj.insert("manual".into(), pending_json(pending));
    }
    let text = format!("{line} (dots todo)");
    for b in report.blocks.iter_mut() {
        if let Block::Section(s) = b
            && s.title == "Heads up"
        {
            s.body.push(Block::callout(text));
            return;
        }
    }
    // Before the closing `→ dots apply` hint, so the hint stays last.
    let at = match report.blocks.last() {
        Some(Block::Hint(_)) => report.blocks.len() - 1,
        _ => report.blocks.len(),
    };
    report.blocks.insert(
        at,
        Block::Section(Section::new("Heads up").block(Block::callout(text))),
    );
}

/// The pending manual actions of a prepared run, read from the state (no checks run).
pub fn pending_for(
    rc: &RepoCtx,
    resolution: &Resolution,
    installed: &InstalledMap,
    dirs: &dots_core::Dirs,
) -> Vec<ManualAction> {
    let (state, _) = State::load(&dirs.state_json());
    pending_of(&entries(rc, resolution, installed, &state))
        .into_iter()
        .cloned()
        .collect()
}

/// End of `apply`: run the checks of what is pending, record what passed, and say what is left.
/// Called with the run lock still held by the caller's scope (the engine has finished).
pub fn after_apply(
    ctx: &Context,
    rc: &RepoCtx,
    resolution: &Resolution,
    installed: &InstalledMap,
    pctx: &Ctx,
    report: &mut Report,
) {
    let path = ctx.dirs.state_json();
    let (mut state, _) = State::load(&path);
    let es = entries(rc, resolution, installed, &state);
    let rev = git_info(&rc.m.root).commit;
    let checked = check_all(&es, &mut state, &check_env(pctx, &ctx.dirs), rev.as_deref());
    if (!checked.closed.is_empty() || !checked.reopened.is_empty())
        && let Err(e) = state.save(&path)
    {
        report.warnings.push(Diagnostic::from_code(
            "write-failed",
            &[
                ("path", &path.display().to_string()),
                ("detail", &e.to_string()),
            ],
        ));
    }
    let es = entries(rc, resolution, installed, &state);
    let pending = pending_of(&es);
    if let Some(obj) = report.data.as_object_mut() {
        let mut m = pending_json(&pending);
        m["closed"] = json!(checked.closed);
        m["reopened"] = json!(checked.reopened);
        obj.insert("manual".into(), m);
    }
    if !checked.closed.is_empty() {
        report.push(Block::text(format!(
            "{} closed by its check: {}",
            plural_actions(checked.closed.len()),
            checked.closed.join(", ")
        )));
    }
    if let Some(b) = pending_block(&pending) {
        report.push(b);
        report.push(Block::hint("dots todo"));
    }
}

fn plural_actions(n: usize) -> String {
    format!("{n} manual {}", if n == 1 { "action" } else { "actions" })
}

// ------------------------------------------------------------------- the command

pub fn run(ctx: &Context, action: Option<&TodoAction>) -> Report {
    match action {
        None => list(ctx),
        Some(TodoAction::Show { id }) => show(ctx, id),
        Some(TodoAction::Done { id, note }) => close(ctx, id, ManualStatus::Done, note.clone()),
        Some(TodoAction::Skip { id }) => close(ctx, id, ManualStatus::Skipped, None),
        Some(TodoAction::Check) => check(ctx),
    }
}

fn failure(errors: Vec<Diagnostic>, warnings: Vec<Diagnostic>) -> Report {
    let mut r = Report::failure("todo", ExitCode::Error, errors);
    r.warnings = warnings;
    r
}

fn usage(detail: &str) -> Report {
    let d = Diagnostic::from_code(
        "usage-error",
        &[("detail", detail), ("help", "dots todo --help")],
    );
    Report::failure("todo", ExitCode::Usage, vec![d])
}

/// `karabiner/permissions` or the address `pkg:karabiner/manual:permissions`.
fn parse_id(id: &str) -> Result<(String, String), String> {
    if let Ok(Address::Pkg {
        name,
        sub: Some(Sub::Manual(m)),
    }) = Address::parse(id)
    {
        return Ok((name, m));
    }
    match id.split_once('/') {
        Some((p, m)) if !p.is_empty() && !m.is_empty() && !m.contains('/') => {
            Ok((p.to_string(), m.to_string()))
        }
        _ => Err(format!(
            "`{id}` is not a manual action id like `karabiner/permissions`"
        )),
    }
}

fn find<'a>(rc: &'a RepoCtx, id: &str) -> Result<&'a ManualAction, Box<Report>> {
    let (pkg, name) = parse_id(id).map_err(|e| Box::new(usage(&e)))?;
    let found = rc
        .repo
        .packages
        .get(&pkg)
        .and_then(|p| p.manual.iter().find(|a| a.id == name));
    found.ok_or_else(|| {
        let d = Diagnostic::from_code("manual-unknown", &[("id", id)]);
        Box::new(failure(vec![d], vec![]))
    })
}

fn badges(a: &ManualAction) -> String {
    let mut parts: Vec<String> = a.requires.iter().map(Requires::badge).collect();
    if a.interactive {
        parts.push("needs a terminal".into());
    }
    parts.join(", ")
}

fn action_json(a: &ManualAction, status: ManualStatus, state: &State) -> Value {
    let rec = state.manual.get(&a.full_id());
    json!({
        "id": a.full_id(),
        "package": a.package,
        "title": a.title,
        "status": status.word(),
        "revision": a.revision,
        "after": a.after.word(),
        "check": a.check.is_some(),
        "requires": a.requires.iter().map(Requires::badge).collect::<Vec<_>>(),
        "interactive": a.interactive,
        "at": rec.map(|r| r.at.clone()),
        "note": rec.and_then(|r| r.note.clone()),
    })
}

fn list(ctx: &Context) -> Report {
    let l = match load(ctx, "todo") {
        Ok(l) => l,
        Err(r) => return *r,
    };
    let (state, _) = State::load(&ctx.dirs.state_json());
    let es = entries_of(&l, &state);
    let pending = pending_of(&es);
    // The logout / reboot requirements that settings raised in an earlier run: they close
    // themselves once the setting reads as declared (the next `apply` drops them).
    let settled = if state.todos.is_empty() {
        Vec::new()
    } else {
        crate::settings_phase::settled_todos(&l.rc, &l.resolution, &l.pctx)
    };
    let setting_todos: Vec<&dots_engine::Todo> = state
        .todos
        .iter()
        .filter(|t| {
            !settled
                .iter()
                .any(|(p, r)| *p == t.package && *r == t.reason)
        })
        .collect();

    let data = json!({
        "pending": pending.len() + setting_todos.len(),
        "actions": es.iter().map(|e| action_json(e.action, e.status, &state)).collect::<Vec<_>>(),
        "settings": setting_todos.iter().map(|t| json!({"package": t.package, "reason": t.reason})).collect::<Vec<_>>(),
    });
    let mut report = Report::ok("todo", data);
    report.warnings = l.warnings.clone();
    let total = pending.len() + setting_todos.len();
    if total == 0 {
        let closed = es.len();
        let text = if closed == 0 {
            "no manual actions on this machine".to_string()
        } else {
            format!("nothing to do by hand ({closed} closed)")
        };
        return report.with_headline(Tone::Ok, text);
    }
    let mut table =
        Table::new()
            .column("action", 5)
            .styled_column("note", 3, dots_core::Style::Muted);
    let mut es_sorted: Vec<&Entry<'_>> = es
        .iter()
        .filter(|e| e.status == ManualStatus::Pending)
        .collect();
    es_sorted.sort_by_key(|e| (e.action.package.clone(), e.action.id.clone()));
    for e in es_sorted {
        let a = e.action;
        let note = match badges(a) {
            b if b.is_empty() => String::new(),
            b => format!("[{b}]"),
        };
        table.push(
            Row::new(UiState::Pending, a.full_id())
                .cell(Cell::new(a.title.clone(), dots_core::Style::Plain))
                .cell(Cell::muted(note))
                .hint(format!("dots todo show {}", a.full_id())),
        );
    }
    for t in setting_todos {
        table.push(
            Row::new(UiState::Pending, t.package.clone())
                .cell(Cell::new(t.reason.clone(), dots_core::Style::Plain))
                .cell(Cell::muted("[from a setting]"))
                .hint("dots apply"),
        );
    }
    let line = summary_line(&pending).unwrap_or_else(|| format!("{total} manual steps pending"));
    report = report.with_headline(Tone::Warn, line);
    report.push(Block::Section(Section::new("Manual actions").block(table)));
    report
}

fn show(ctx: &Context, id: &str) -> Report {
    let rc = match load_repo(ctx, None) {
        Ok(rc) => rc,
        Err((e, w)) => return failure(e, w),
    };
    let a = match find(&rc, id) {
        Ok(a) => a,
        Err(r) => return *r,
    };
    let (state, _) = State::load(&ctx.dirs.state_json());
    let status = state.manual_status(&a.full_id(), a.revision);
    let mut report = Report::ok("todo", action_json(a, status, &state));
    report.data["body"] = json!(a.body);
    let tone = match status {
        ManualStatus::Pending => Tone::Warn,
        _ => Tone::Ok,
    };
    report = report.with_headline(tone, format!("{}: {}", a.full_id(), a.title));
    let mut facts: Vec<(String, Cell)> = vec![("status".into(), Cell::tone(status.word(), tone))];
    let b = badges(a);
    if !b.is_empty() {
        facts.push(("requires".into(), Cell::muted(b)));
    }
    if a.check.is_some() {
        facts.push((
            "check".into(),
            Cell::muted("run by `dots todo check` and at the end of `apply`"),
        ));
    }
    report.push(Block::facts(facts));
    report.push(Block::Code {
        title: Some(a.title.clone()),
        lines: a.body.trim_end().lines().map(str::to_string).collect(),
    });
    if status == ManualStatus::Pending {
        report.push(Block::hint(format!("dots todo done {}", a.full_id())));
        if a.check.is_some() {
            report.push(Block::hint("dots todo check"));
        }
    }
    report
}

fn close(ctx: &Context, id: &str, status: ManualStatus, note: Option<String>) -> Report {
    let rc = match load_repo(ctx, None) {
        Ok(rc) => rc,
        Err((e, w)) => return failure(e, w),
    };
    let a = match find(&rc, id) {
        Ok(a) => a,
        Err(r) => return *r,
    };
    let _lock = match crate::adopt::lock(ctx) {
        Ok(l) => l,
        Err(r) => {
            let mut r = *r;
            r.command = "todo".into();
            return r;
        }
    };
    let path = ctx.dirs.state_json();
    let (mut state, _) = State::load(&path);
    let full = a.full_id();
    let rev = git_info(&rc.m.root).commit;
    state.close_manual(
        &full,
        a.revision,
        status,
        rfc3339(SystemTime::now()),
        rev,
        note,
    );
    if let Err(e) = state.save(&path) {
        let d = Diagnostic::from_code(
            "write-failed",
            &[
                ("path", &path.display().to_string()),
                ("detail", &e.to_string()),
            ],
        );
        return failure(vec![d], vec![]);
    }
    let data = action_json(a, status, &state);
    let (word, tail) = match status {
        ManualStatus::Skipped => ("skipped", " (it comes back when its revision increases)"),
        _ => ("done", ""),
    };
    Report::ok("todo", data).with_headline(Tone::Ok, format!("{full} marked {word}{tail}"))
}

fn check(ctx: &Context) -> Report {
    let l = match load(ctx, "todo") {
        Ok(l) => l,
        Err(r) => return *r,
    };
    let _lock = match crate::adopt::lock(ctx) {
        Ok(l) => l,
        Err(r) => {
            let mut r = *r;
            r.command = "todo".into();
            return r;
        }
    };
    let path = ctx.dirs.state_json();
    let (mut state, _) = State::load(&path);
    let es = entries_of(&l, &state);
    let rev = git_info(&l.rc.m.root).commit;
    let checked = check_all(
        &es,
        &mut state,
        &check_env(&l.pctx, &ctx.dirs),
        rev.as_deref(),
    );
    if !checked.closed.is_empty() || !checked.reopened.is_empty() {
        let _ = state.save(&path);
    }
    let with_check = es.iter().filter(|e| e.action.check.is_some()).count();
    let data = json!({
        "checked": with_check,
        "closed": checked.closed,
        "reopened": checked.reopened,
        "not_yet": checked.not_yet,
    });
    let mut report = Report::ok("todo", data);
    report.warnings = l.warnings.clone();
    let mut table = Table::new().styled_column("result", 3, dots_core::Style::Muted);
    for id in &checked.closed {
        table.push(Row::new(UiState::Auto, id.clone()).cell(Cell::muted("\u{2713} verified")));
    }
    for id in &checked.reopened {
        table.push(
            Row::new(UiState::Pending, id.clone()).cell(Cell::muted("no longer true, open again")),
        );
    }
    for id in &checked.not_yet {
        table.push(Row::new(UiState::Pending, id.clone()).cell(Cell::muted("\u{2717} not yet")));
    }
    let headline = match (with_check, checked.closed.len()) {
        (0, _) => (Tone::Info, "no pending action has a check".to_string()),
        (_, 0) if checked.not_yet.is_empty() => (Tone::Ok, "nothing changed".into()),
        (_, 0) => (Tone::Warn, "no check passed yet".into()),
        (_, n) => (Tone::Ok, format!("{n} closed by their check")),
    };
    report = report.with_headline(headline.0, headline.1);
    if !table.rows.is_empty() {
        report.push(Block::Table(table));
    }
    report
}
