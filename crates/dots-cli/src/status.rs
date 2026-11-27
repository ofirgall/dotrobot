//! `dots status`: the summary (only what needs attention), `--all`, and the deep view of one
//! package. Reads only: installed state from the providers, cached latest versions, the files'
//! drift states, pending todos and git sources. Never touches the network.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use dots_core::blocks::AttentionLine;
use dots_core::blocks::plural;
use dots_core::{
    Block, Cell, Diagnostic, Dirs, ExitCode, Report, Row, Section, State, Style, Table, Tone,
};
use dots_engine::{Change, State as OwnedState, diff, parse_wanted};
use dots_facts::suggest;
use dots_files::FileState;
use serde_json::{Map, Value, json};

use crate::app::Context;
use crate::bins::find_copies;
use crate::files_phase::RepoFiles;
use crate::inspect::{LatestCache, Loaded, cached_latest, load, tilde};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Area {
    Packages,
    Files,
    Settings,
    Services,
    Todos,
    Git,
    Bins,
}

impl Area {
    const ALL: [Area; 7] = [
        Area::Packages,
        Area::Files,
        Area::Settings,
        Area::Services,
        Area::Todos,
        Area::Git,
        Area::Bins,
    ];

    fn key(self) -> &'static str {
        match self {
            Area::Packages => "packages",
            Area::Files => "files",
            Area::Settings => "settings",
            Area::Services => "services",
            Area::Todos => "todos",
            Area::Git => "git",
            Area::Bins => "bins",
        }
    }

    fn title(self) -> &'static str {
        match self {
            Area::Packages => "PACKAGES",
            Area::Files => "FILES",
            Area::Settings => "SETTINGS",
            Area::Services => "SERVICES",
            Area::Todos => "TODO",
            Area::Git => "GIT",
            Area::Bins => "BINS",
        }
    }
}

/// One row of the inventory.
#[derive(Debug, Clone)]
pub struct Item {
    pub area: Area,
    /// A package name, a destination path (files) or a binary name.
    pub name: String,
    pub package: String,
    pub state: State,
    /// The provider id, or the file mode.
    pub provider: Option<String>,
    pub want: Option<String>,
    pub have: Option<String>,
    pub latest: Option<String>,
    pub detail: String,
    pub hints: Vec<String>,
    /// Override scopes that change this item (`repo`, `profile`, `machine`).
    pub overridden: Vec<String>,
}

impl Item {
    fn new(area: Area, name: &str, package: &str, state: State) -> Item {
        Item {
            area,
            name: name.to_string(),
            package: package.to_string(),
            state,
            provider: None,
            want: None,
            have: None,
            latest: None,
            detail: String::new(),
            hints: Vec::new(),
            overridden: Vec::new(),
        }
    }

    /// Warn and error states need the user; muted ones (n/a, excluded, orphan, ...) do not.
    pub fn needs_attention(&self) -> bool {
        matches!(self.state.tone(), Tone::Warn | Tone::Err) && self.state != State::Pending
    }

    /// Listed in the summary without `--all`: everything that is not healthy or inert.
    fn in_summary(&self) -> bool {
        !matches!(
            self.state,
            State::Ok
                | State::NotApplicable
                | State::Excluded
                | State::Unmanaged
                | State::Done
                | State::Skipped
                | State::Auto
        )
    }

    fn to_json(&self) -> Value {
        let mut v = json!({
            "area": self.area.key(),
            "name": self.name,
            "package": self.package,
            "state": self.state.word(),
            "provider": self.provider,
            "want": self.want,
            "have": self.have,
            "latest": self.latest,
            "detail": self.detail,
            "hints": self.hints,
        });
        // Only customized items carry the key, so plain output stays unchanged.
        if !self.overridden.is_empty() {
            v["overridden"] = json!(self.overridden);
        }
        v
    }
}

fn mode_word(m: dots_files::Mode) -> &'static str {
    match m {
        dots_files::Mode::Link => "link",
        dots_files::Mode::Copy => "copy",
        dots_files::Mode::Render => "render",
        dots_files::Mode::Inject => "inject",
        dots_files::Mode::Dir => "dir",
        dots_files::Mode::Terminfo => "terminfo",
    }
}

fn file_state(s: FileState) -> State {
    match s {
        FileState::Ok => State::Ok,
        FileState::Missing => State::Missing,
        FileState::Conflict => State::Conflict,
        FileState::Modified => State::Modified,
        FileState::Stale => State::Stale,
        FileState::Dangling => State::Dangling,
        FileState::ForeignEntries => State::ForeignEntries,
    }
}

pub fn file_items(l: &Loaded, dirs: &Dirs) -> Vec<Item> {
    let rf = RepoFiles {
        dirs,
        rc: &l.rc,
        resolution: &l.resolution,
    };
    let ctx = rf.base_ctx(&dirs.home, &dirs.state, "status");
    let packages: Vec<String> = l.resolution.active().map(|r| r.name.clone()).collect();
    let r = rf.resolved_files(&packages, &ctx);
    let mut items = Vec::new();
    for (package, entry, d) in &r.failures {
        let name = match &entry.dst {
            Some(dots_model::PlatformString::Plain(s)) => s.clone(),
            _ => "(unresolved)".to_string(),
        };
        let mut it = Item::new(Area::Files, &name, package, State::Failed);
        it.detail = d.message.clone();
        it.hints = if d.code == "setting-required" {
            vec![d.hint.clone()]
        } else {
            vec![format!("dots lint  # then fix {package}")]
        };
        items.push(it);
    }
    for fs in dots_files::status(&r.files, &ctx) {
        let state = file_state(fs.state);
        let mut it = Item::new(Area::Files, &tilde(&fs.dst, &dirs.home), &fs.package, state);
        it.provider = Some(mode_word(fs.mode).to_string());
        it.detail = match (&fs.error, state) {
            (Some(e), _) => e.message.clone(),
            (None, State::Missing) => "not created yet".into(),
            (None, State::Conflict) => "exists and is not managed by dots".into(),
            (None, State::Modified) => "changed since dots wrote it".into(),
            (None, State::Stale) => "a source changed".into(),
            (None, State::Dangling) => "the link target is gone".into(),
            (None, State::ForeignEntries) => "entries created by the app live in your repo".into(),
            _ => String::new(),
        };
        let mut hints: Vec<String> = Vec::new();
        for r in &fs.resolutions {
            let cmd = r.command.replace(&dirs.home.display().to_string(), "~");
            if !hints.contains(&cmd) && hints.len() < 2 {
                hints.push(cmd);
            }
        }
        if hints.is_empty() {
            match state {
                State::Missing | State::Stale if fs.package == dots_files::FRAGMENT_OWNER => {
                    hints.push("dots apply".into())
                }
                State::Missing | State::Stale => hints.push(format!("dots apply {}", fs.package)),
                State::Dangling => hints.push("dots roots".into()),
                State::ForeignEntries => {
                    hints.push("link individual files instead of the whole folder".into())
                }
                _ => {}
            }
        }
        it.hints = hints;
        items.push(it);
    }
    items
}

// ---------------------------------------------------------------- settings and services

fn setting_state(word: &str) -> State {
    match word {
        "ok" => State::Ok,
        "drift" => State::Drift,
        "missing" => State::Missing,
        "n/a" => State::NotApplicable,
        "unknown" => State::Unknown,
        _ => State::Failed,
    }
}

/// `[[defaults]]`, `[[registry]]` and `[[xdg]]`: one batched read per domain, key or mime type.
pub fn settings_items(l: &Loaded, dirs: &Dirs) -> Vec<Item> {
    crate::settings_phase::status_rows(&l.rc, &l.resolution, dirs, &l.pctx)
        .into_iter()
        .map(|r| {
            let mut it = Item::new(Area::Settings, &r.name, &r.package, setting_state(r.state));
            it.provider = (!r.kind.is_empty()).then(|| r.kind.clone());
            it.want = (!r.want.is_empty()).then(|| r.want.clone());
            it.have = r.have.clone();
            it.detail = r.detail.clone();
            it.hints = match (&r.adopt, r.state) {
                (Some(a), _) => vec![a.clone(), format!("dots apply {}", r.package)],
                (None, "missing" | "drift") => vec![format!("dots apply {}", r.package)],
                _ => vec![],
            };
            it
        })
        .collect()
}

/// `[service]`: `running`, `stopped` or `failed`, one call per manager.
pub fn service_items(l: &Loaded, dirs: &Dirs) -> Vec<Item> {
    use dots_services::Health;
    let (rows, inert, error) =
        crate::services_phase::status_rows(&l.rc, &l.resolution, dirs, &l.pctx);
    let mut items = Vec::new();
    for r in rows {
        let (state, detail) = match r.health {
            Health::Ok => (State::Ok, String::new()),
            Health::NotEnabled => (State::Missing, "not enabled".to_string()),
            Health::Stopped => (State::Drift, "not running".to_string()),
            Health::Failed => (State::Failed, "crashed".to_string()),
        };
        let mut it = Item::new(Area::Services, &r.name, &r.package, state);
        it.provider = Some(r.manager.as_str().to_string());
        it.have = Some(r.state.run.as_str().to_string());
        it.want = Some("running".into());
        it.detail = detail;
        if state != State::Ok {
            it.hints = vec![format!("dots apply {}", r.package)];
        }
        items.push(it);
    }
    for package in inert {
        let mut it = Item::new(Area::Services, &package, &package, State::NotApplicable);
        it.detail = "n/a on this platform".into();
        items.push(it);
    }
    if let Some(e) = error {
        let mut it = Item::new(Area::Services, "services", "", State::Failed);
        it.detail = e;
        items.push(it);
    }
    items
}

// ---------------------------------------------------------------- packages

/// What `dots.lock` records for `name` on this machine (the row of its chosen provider).
fn lock_row(
    lock: &dots_lock::Lock,
    machine_key: &str,
    name: &str,
    provider: &str,
) -> Option<dots_lock::PkgRow> {
    lock.pkg(name, &dots_lock::platform_for(provider, machine_key))
        .cloned()
}

fn package_items(l: &Loaded, dirs: &Dirs, all: bool) -> Vec<Item> {
    let lock = dots_lock::Lock::load(&crate::lock_cmd::path(&l.rc.m.root)).unwrap_or_default();
    let machine_key = crate::lock_cmd::machine_key(&l.rc);
    let cache = LatestCache::new(dirs).load_all();
    let owned = OwnedState::load(&dirs.state_json()).0;
    let mut items = Vec::new();
    let mut claimed: BTreeSet<(String, String)> = BTreeSet::new();

    for (name, res) in &l.resolution.packages {
        let pkg = l.rc.repo.packages.get(name);
        let mut it = Item::new(Area::Packages, name, name, State::Ok);
        match &res.state {
            dots_resolve::PackageState::Inert { condition } => {
                it.state = State::NotApplicable;
                let _ = condition;
                it.detail = "n/a on this platform".into();
                items.push(it);
                continue;
            }
            dots_resolve::PackageState::Excluded => {
                it.state = State::Excluded;
                it.detail = "excluded (machine.toml `exclude`)".into();
                items.push(it);
                continue;
            }
            dots_resolve::PackageState::Replaced { by } => {
                it.state = State::Excluded;
                it.detail = format!("replaced by `{by}`");
                items.push(it);
                continue;
            }
            dots_resolve::PackageState::Active => {}
        }
        let wanted = pkg.and_then(|p| p.version.clone());
        it.want = Some(wanted.clone().unwrap_or_else(|| "latest".into()));
        let Some(choice) = res.chosen_provider() else {
            let files_only = res.provider.as_ref().is_some_and(|s| {
                s.candidates.is_empty() && s.ranking == dots_resolve::Ranking::Unordered
            });
            if files_only {
                it.detail = "files only".into();
            } else {
                it.state = State::Missing;
                it.detail = "no provider can install it".into();
                it.hints = vec![format!("dots why {name}")];
            }
            items.push(it);
            continue;
        };
        claimed.insert((choice.id.clone(), choice.native_name.clone()));
        it.provider = Some(choice.id.clone());
        if choice.id == "git" {
            it.area = Area::Git;
        }
        if let Some(why) = l.failed.get(&choice.id) {
            it.state = State::Failed;
            it.detail = format!("{} could not be queried: {why}", choice.id);
            it.hints = vec!["dots doctor".into()];
            items.push(it);
            continue;
        }
        let inst = l
            .installed
            .get(&choice.id)
            .and_then(|m| m.get(&choice.native_name));
        let Some(inst) = inst else {
            it.state = State::Missing;
            it.detail = "not installed".into();
            it.hints = vec![format!("dots apply {name}")];
            items.push(it);
            continue;
        };
        it.have = Some(inst.version.clone());
        // A declared shim that is missing or dangling is drift on its package.
        let shim = dots_engine::shim_declared_for(&l.rc.repo, &l.resolution, name, &dirs.home)
            .and_then(|d| match dots_engine::shim_state(&d.path) {
                dots_engine::ShimState::Ok => None,
                dots_engine::ShimState::Missing => Some((d, "is missing")),
                dots_engine::ShimState::Dangling => Some((d, "points at nothing")),
                dots_engine::ShimState::Occupied => Some((d, "is occupied by a real file")),
            });
        let latest = cached_latest(&cache, &choice.id, &choice.native_name);
        it.latest = latest.map(str::to_string);
        match diff(&parse_wanted(wanted.as_deref()), Some(&inst.version)) {
            Change::None => {
                // The provider holds a version the lock does not name, though the spec allows
                // it: drift (apt and brew cannot install an exact version).
                let drifted = lock_row(&lock, &machine_key, name, &choice.id).filter(|row| {
                    let lk = dots_lock::rules::Locked::from(row);
                    dots_lock::rules::lock_satisfies(&res.wanted, &lk)
                        && !dots_lock::rules::matches_lock(
                            &lk,
                            &inst.version,
                            inst.source.as_deref(),
                        )
                });
                if let Some(row) = drifted {
                    it.state = State::Drift;
                    it.detail = format!("locked {}, installed {}", row.shown(), inst.version);
                    it.hints = vec![format!("dots apply {name}")];
                } else if let Some(latest) = latest
                    && crate::inspect::is_outdated(
                        wanted.as_deref(),
                        &inst.version,
                        latest,
                        inst.pinned,
                    )
                {
                    it.state = State::Outdated;
                    it.hints = vec![format!("dots upgrade {name}")];
                }
            }
            _ => {
                it.state = State::Drift;
                it.detail = format!("wants {}", wanted.as_deref().unwrap_or("latest"));
                it.hints = vec![format!("dots apply {name}")];
            }
        }
        if let Some((d, what)) = shim
            && matches!(it.state, State::Ok | State::Outdated)
        {
            it.state = State::Drift;
            it.detail = format!("shim {} {what}", tilde(&d.path, &dirs.home));
            it.hints = vec![format!("dots apply {name}")];
        }
        items.push(it);
    }
    mark_overridden(l, &mut items);

    // Installed by dots, no longer declared.
    for (name, o) in &owned.packages {
        if l.resolution.get(name).is_some_and(|p| p.is_active()) {
            continue;
        }
        let Some(inst) = l.installed.get(&o.provider).and_then(|m| m.get(&o.name)) else {
            continue;
        };
        claimed.insert((o.provider.clone(), o.name.clone()));
        let mut it = Item::new(Area::Packages, name, name, State::Orphan);
        it.provider = Some(o.provider.clone());
        it.have = Some(inst.version.clone());
        it.detail = "installed by dots, not declared anymore".into();
        it.hints = vec![format!("dots remove {name}")];
        items.push(it);
    }

    // Installed by a provider and unknown to dots: only listed with `--all`.
    if all {
        let deps = crate::bins::dependencies(l);
        for (provider, map) in &l.installed {
            if matches!(provider.as_str(), "script" | "git") {
                continue;
            }
            for (native, inst) in map {
                if claimed.contains(&(provider.clone(), native.clone())) {
                    continue;
                }
                let mut it = Item::new(Area::Packages, native, native, State::Unmanaged);
                it.provider = Some(provider.clone());
                it.have = Some(inst.version.clone());
                it.detail =
                    if let Some(pkg) = crate::bins::superseded_by(l, &owned, provider, native) {
                        format!("old copy installed by dots; `{pkg}` now uses another provider")
                    } else if deps.get(provider).is_some_and(|d| d.contains(native)) {
                        "installed as a dependency of another package".into()
                    } else {
                        "installed outside dots".into()
                    };
                items.push(it);
            }
        }
    }
    items
}

/// `✎ overridden (repo, machine)`: the scopes whose overrides change a package.
fn mark_overridden(l: &Loaded, items: &mut [Item]) {
    let Some(info) = l.rc.repo.effective.as_ref() else {
        return;
    };
    for it in items.iter_mut().filter(|i| i.area == Area::Packages) {
        let scopes = info.scopes_of(&format!("pkg:{}", it.name));
        if scopes.is_empty() {
            continue;
        }
        it.overridden = scopes.iter().map(|s| s.short().to_string()).collect();
        let mark = format!("✎ overridden ({})", it.overridden.join(", "));
        it.detail = if it.detail.is_empty() {
            mark
        } else {
            format!("{} · {mark}", it.detail)
        };
    }
}

/// A git source with local changes is `modified`, whatever its revision.
fn git_dirty(l: &Loaded, dirs: &Dirs, items: &mut [Item]) {
    for it in items.iter_mut().filter(|i| i.area == Area::Git) {
        if it.state != State::Ok {
            continue;
        }
        let Some(res) = l.resolution.get(&it.package) else {
            continue;
        };
        let Some(c) = res.chosen_provider() else {
            continue;
        };
        let dest = dirs.data.join("git").join(&c.native_name);
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(&dest)
            .args(["status", "--porcelain"])
            .output();
        if let Ok(o) = out
            && o.status.success()
        {
            let n = String::from_utf8_lossy(&o.stdout).lines().count();
            if n > 0 {
                it.state = State::Modified;
                it.detail = format!("{} changed", plural(n, "file", "files"));
                it.hints = vec![format!("git -C {} status", tilde(&dest, &dirs.home))];
            }
        }
    }
}

/// Pending manual actions. A `logout` / `reboot` action whose setting reads as declared now
/// is not pending any more (the next `apply` also drops it from the state).
fn todo_items(l: &Loaded, dirs: &Dirs) -> Vec<Item> {
    let todos = OwnedState::load(&dirs.state_json()).0.todos;
    let settled = if todos.is_empty() {
        Vec::new()
    } else {
        crate::settings_phase::settled_todos(&l.rc, &l.resolution, &l.pctx)
    };
    todos
        .iter()
        .filter(|t| {
            !settled
                .iter()
                .any(|(p, r)| *p == t.package && *r == t.reason)
        })
        .map(|t| {
            let mut it = Item::new(Area::Todos, &t.package, &t.package, State::Pending);
            it.detail = t.reason.clone();
            it.hints = vec!["dots todo".into()];
            it
        })
        .collect()
}

/// Declared executables of installed packages: cheap, static checks against PATH.
fn bin_items(l: &Loaded, dirs: &Dirs) -> Vec<Item> {
    let path = l.path_dirs(dirs);
    let mut items = Vec::new();
    for a in l.active() {
        if a.installed.is_none() {
            continue;
        }
        let Some(pkg) = l.rc.repo.packages.get(a.name) else {
            continue;
        };
        let names: BTreeSet<&String> = pkg.bin.iter().chain(&pkg.bins).collect();
        for bin in names {
            let copies = find_copies(&path, bin);
            let mut it = Item::new(Area::Bins, bin, a.name, State::Ok);
            match copies.len() {
                0 => {
                    it.state = State::Missing;
                    it.detail = "not on PATH".into();
                    it.hints = vec!["dots doctor".into()];
                    items.push(it);
                }
                1 => {}
                n => {
                    it.state = State::Drift;
                    it.detail = format!("{n} different copies on PATH");
                    it.hints = vec![format!("dots which {bin}")];
                    items.push(it);
                }
            }
        }
    }
    items
}

pub struct Snapshot {
    pub items: Vec<Item>,
}

pub fn compute(l: &Loaded, dirs: &Dirs, all: bool) -> Snapshot {
    let mut items = package_items(l, dirs, all);
    git_dirty(l, dirs, &mut items);
    items.extend(file_items(l, dirs));
    items.extend(settings_items(l, dirs));
    items.extend(service_items(l, dirs));
    items.extend(todo_items(l, dirs));
    items.extend(bin_items(l, dirs));
    Snapshot { items }
}

impl Snapshot {
    fn attention(&self) -> usize {
        self.items.iter().filter(|i| i.needs_attention()).count()
    }

    fn counts(&self, area: Area) -> Vec<(State, usize)> {
        State::ALL
            .iter()
            .filter_map(|s| {
                let n = self
                    .items
                    .iter()
                    .filter(|i| i.area == area && i.state == *s)
                    .count();
                (n > 0).then_some((*s, n))
            })
            .collect()
    }

    fn total(&self, area: Area) -> usize {
        self.items.iter().filter(|i| i.area == area).count()
    }
}

// ---------------------------------------------------------------- the command

pub fn run(ctx: &Context, pkg: Option<&str>, all: bool, strict: bool) -> Report {
    run_view(ctx, pkg, all, strict, false)
}

/// `dots` with no arguments: the grouped dashboard (UX 4.1) over the same data as `status`.
pub fn dashboard_cmd(ctx: &Context) -> Report {
    run_view(ctx, None, false, false, true)
}

fn run_view(ctx: &Context, pkg: Option<&str>, all: bool, strict: bool, dash: bool) -> Report {
    let l = match load(ctx, "status") {
        Ok(l) => l,
        Err(r) => return *r,
    };
    let snap = compute(&l, &ctx.dirs, all || pkg.is_some());
    let mut report = match pkg {
        Some(p) => match deep(&l, &ctx.dirs, &snap, p) {
            Ok(r) => r,
            Err(d) => {
                let mut r = Report::failure("status", ExitCode::Error, vec![d]);
                r.warnings = l.warnings.clone();
                return r;
            }
        },
        None if dash => dashboard(&l, &ctx.dirs, &snap),
        None => summary(&l, &ctx.dirs, &snap, all),
    };
    report.warnings = l.warnings.clone();
    let scoped: Vec<&Item> = match pkg {
        Some(p) => snap.items.iter().filter(|i| i.package == p).collect(),
        None => snap.items.iter().collect(),
    };
    if scoped.iter().any(|i| i.needs_attention()) {
        let drift_is_error = strict && scoped.iter().any(|i| i.state == State::Drift);
        report.exit = if drift_is_error {
            ExitCode::Error
        } else {
            ExitCode::Drift
        };
    }
    report
}

fn header(l: &Loaded, dirs: &Dirs) -> (Value, Vec<String>) {
    let rc = &l.rc;
    let root = tilde(&rc.m.root, &dirs.home);
    let mut root_part = root.clone();
    if let Some(c) = &l.git.commit {
        let state = if l.git.dirty { "dirty" } else { "clean" };
        root_part = match &l.git.branch {
            Some(b) => format!("{root} \u{b7} {b} @ {c} ({state})"),
            None => format!("{root} \u{b7} @ {c} ({state})"),
        };
    }
    let mut parts = vec![
        root_part,
        rc.m.facts.hostname.clone(),
        format!("{} {}", os_display(&rc.m.facts.os), rc.m.facts.arch),
    ];
    if !rc.m.machine.tags.is_empty() {
        parts.push(format!("tags {}", rc.m.machine.tags.join(", ")));
    }
    let data = json!({
        "root": root,
        "branch": l.git.branch,
        "commit": l.git.commit,
        "dirty": l.git.dirty,
        "machine": rc.m.facts.hostname,
        "platform": rc.chain.levels(),
        "os": rc.m.facts.os,
        "arch": rc.m.facts.arch,
        "tags": rc.m.machine.tags,
    });
    (data, parts)
}

fn version_cell(it: &Item) -> Cell {
    match (it.state, &it.have, &it.latest) {
        (State::Outdated, Some(h), Some(n)) => Cell::version(h, n),
        (_, Some(h), _) => Cell::new(h.clone(), Style::Plain),
        _ => Cell::muted("-"),
    }
}

fn row_for(it: &Item) -> Row {
    let lead = it.state;
    let mut row = match it.area {
        Area::Files => Row::named(lead, Cell::path(it.name.clone())),
        _ => Row::new(lead, it.name.clone()),
    };
    match it.area {
        Area::Files => {
            row = row
                .cell(Cell::muted(it.provider.clone().unwrap_or_default()))
                .cell(Cell::muted(it.detail.clone()));
        }
        Area::Todos | Area::Bins => {
            row = row.cell(Cell::muted(it.detail.clone()));
        }
        _ => {
            row = row
                .cell(Cell::muted(it.provider.clone().unwrap_or_default()))
                .cell(version_cell(it))
                .cell(Cell::muted(it.detail.clone()));
        }
    }
    for h in &it.hints {
        row = row.hint(h.clone());
    }
    row
}

fn table_for(area: Area) -> Table {
    match area {
        Area::Files => Table::new()
            .styled_column("mode", 4, Style::Muted)
            .styled_column("note", 3, Style::Muted),
        Area::Todos | Area::Bins => Table::new().styled_column("note", 3, Style::Muted),
        _ => Table::new()
            .styled_column("provider", 2, Style::Muted)
            .column("version", 4)
            .styled_column("note", 3, Style::Muted),
    }
}

/// How the OS is named in headers.
fn os_display(os: &str) -> String {
    match os {
        "macos" => "macOS".into(),
        "linux" => "Linux".into(),
        "windows" => "Windows".into(),
        o => o.to_string(),
    }
}

/// The widest list of names a dashboard line shows before it ends in `…`.
const NAMES_BUDGET: usize = 24;

fn short_names(names: &[String]) -> String {
    let mut out = String::new();
    for (i, n) in names.iter().enumerate() {
        let sep = if i == 0 { "" } else { ", " };
        if out.chars().count() + sep.len() + n.chars().count() > NAMES_BUDGET {
            if out.is_empty() {
                let keep: String = n.chars().take(NAMES_BUDGET - 1).collect();
                return format!("{keep}\u{2026}");
            }
            return format!("{out}, \u{2026}");
        }
        out.push_str(sep);
        out.push_str(n);
    }
    out
}

/// One dashboard line per state that needs a look: the count, the names, the fix.
fn attention_lines(snap: &Snapshot) -> Vec<AttentionLine> {
    let mut groups: Vec<((Area, State), Vec<&Item>)> = Vec::new();
    for it in snap.items.iter().filter(|i| i.in_summary()) {
        match groups.iter_mut().find(|(k, _)| *k == (it.area, it.state)) {
            Some((_, v)) => v.push(it),
            None => groups.push(((it.area, it.state), vec![it])),
        }
    }
    groups.sort_by_key(|((a, _), _)| *a);
    groups
        .into_iter()
        .map(|((area, state), items)| {
            let n = items.len();
            let (text, stem) = match (area, state) {
                (_, State::Outdated) => (
                    plural(n, "update available", "updates available"),
                    "dots upgrade",
                ),
                (_, State::Modified) if area == Area::Files => (
                    plural(n, "config changed by app", "configs changed by app"),
                    "dots adopt",
                ),
                (_, State::Pending) => (
                    plural(n, "manual step left", "manual steps left"),
                    "dots todo",
                ),
                (Area::Bins, _) => (format!("{n} {}", state.word()), "dots bins"),
                (_, State::Missing | State::Drift | State::Stale) => {
                    (format!("{n} {}", state.word()), "dots apply")
                }
                _ => (format!("{n} {}", state.word()), "dots status"),
            };
            let names: Vec<String> = items
                .iter()
                .map(|i| match area {
                    Area::Todos if !i.detail.is_empty() => i.detail.clone(),
                    _ => i.name.clone(),
                })
                .collect();
            let hint = match items[0].hints.first() {
                Some(h) if n == 1 => h.clone(),
                _ => stem.to_string(),
            };
            AttentionLine {
                state,
                text,
                detail: short_names(&names),
                hint,
            }
        })
        .collect()
}

/// The bare `dots` screen (UX 4.1): header, one line per kind of problem, one collapsed count.
fn dashboard(l: &Loaded, dirs: &Dirs, snap: &Snapshot) -> Report {
    let mut report = summary(l, dirs, snap, false);
    let rc = &l.rc;
    let mut root = tilde(&rc.m.root, &dirs.home);
    if let Some(c) = &l.git.commit {
        root = match &l.git.branch {
            Some(b) => format!("{root} \u{b7} {b} @ {c}"),
            None => format!("{root} \u{b7} @ {c}"),
        };
        if l.git.dirty {
            root.push_str(" \u{b7} dirty");
        }
    }
    let mut parts = vec![
        root,
        rc.m.facts.hostname.clone(),
        format!("{} {}", os_display(&rc.m.facts.os), rc.m.facts.arch),
    ];
    if !rc.m.machine.tags.is_empty() {
        parts.push(format!("tags {}", rc.m.machine.tags.join(", ")));
    }
    let lines = attention_lines(snap);
    if lines.is_empty() {
        // Healthy: the summary's "All good" headline already says it.
        report.blocks[0] = Block::Context {
            title: "dots".into(),
            parts,
        };
        return report;
    }
    let ok = |a: Area| {
        snap.items
            .iter()
            .filter(|i| i.area == a && i.state == State::Ok)
            .count()
    };
    let mut bits = Vec::new();
    for (a, one, many) in [
        (Area::Packages, "package", "packages"),
        (Area::Files, "file", "files"),
        (Area::Git, "git source", "git sources"),
    ] {
        if ok(a) > 0 {
            bits.push(plural(ok(a), one, many));
        }
    }
    let mut blocks = vec![
        Block::Context {
            title: "dots".into(),
            parts,
        },
        Block::headline(
            Tone::Warn,
            plural(
                lines.len(),
                "thing needs attention",
                "things need attention",
            ),
        ),
        Block::Attention(lines),
    ];
    if !bits.is_empty() {
        blocks.push(Block::Collapsed {
            state: State::Ok,
            text: format!("{} ok", bits.join(" \u{b7} ")),
            aside: None,
        });
    }
    report.blocks = blocks;
    report
}

fn summary(l: &Loaded, dirs: &Dirs, snap: &Snapshot, all: bool) -> Report {
    let (header_json, parts) = header(l, dirs);
    let attention = snap.attention();
    let visible: Vec<&Item> = snap
        .items
        .iter()
        .filter(|i| all || i.in_summary())
        .collect();

    let mut counts = Map::new();
    for a in Area::ALL {
        let m: Map<String, Value> = snap
            .counts(a)
            .into_iter()
            .map(|(s, n)| (s.word().to_string(), json!(n)))
            .collect();
        counts.insert(a.key().into(), Value::Object(m));
    }
    let data = json!({
        "view": if all { "all" } else { "summary" },
        "header": header_json,
        "attention": attention,
        "counts": counts,
        "items": visible.iter().map(|i| i.to_json()).collect::<Vec<_>>(),
    });
    let mut report = Report::ok("status", data);

    let healthy = |a: Area| {
        snap.items
            .iter()
            .filter(|i| i.area == a && i.state == State::Ok)
            .count()
    };
    if attention == 0 {
        let mut bits = Vec::new();
        for (a, one, many) in [
            (Area::Packages, "package", "packages"),
            (Area::Files, "file", "files"),
            (Area::Git, "git source", "git sources"),
        ] {
            let n = healthy(a);
            if n > 0 {
                bits.push(plural(n, one, many));
            }
        }
        let text = if bits.is_empty() {
            "Nothing declared yet.".to_string()
        } else {
            format!("All good. {}", bits.join(" \u{b7} "))
        };
        report = report.with_headline(Tone::Ok, text);
    } else {
        report = report.with_headline(
            Tone::Warn,
            plural(attention, "thing needs attention", "things need attention"),
        );
    }
    report.blocks.insert(
        0,
        Block::Context {
            title: "dots status".into(),
            parts,
        },
    );

    for a in Area::ALL {
        let rows: Vec<&&Item> = visible.iter().filter(|i| i.area == a).collect();
        let show = !rows.is_empty() || (attention > 0 && snap.total(a) > 0);
        if !show {
            continue;
        }
        let mut sec = Section::new(a.title());
        for (s, n) in snap.counts(a) {
            sec = sec.pill(s, n);
        }
        if !rows.is_empty() {
            let mut t = table_for(a);
            for it in &rows {
                t.push(row_for(it));
            }
            sec = sec.block(t);
        }
        report.push(Block::Section(sec));
    }
    report
}

// ---------------------------------------------------------------- deep view

fn list_files(dir: &Path, base: &Path, out: &mut Vec<String>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<PathBuf> = rd.flatten().map(|e| e.path()).collect();
    entries.sort();
    for p in entries {
        if p.is_dir() {
            list_files(&p, base, out);
        } else if let Ok(rel) = p.strip_prefix(base) {
            out.push(rel.display().to_string());
        }
    }
}

/// Override files of a package, by scope.
fn overrides(l: &Loaded, dirs: &Dirs, name: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for (scope, base) in [
        ("repo", l.rc.m.root.join("overrides/packages").join(name)),
        ("machine", dirs.config.join("overrides/packages").join(name)),
    ] {
        let mut files = Vec::new();
        list_files(&base, &base, &mut files);
        for f in files {
            out.push((scope.to_string(), f));
        }
    }
    out
}

/// The newest `n` journal entries of one package, newest first.
fn history(dirs: &Dirs, name: &str, n: usize) -> Vec<Value> {
    let Ok(text) = std::fs::read_to_string(dirs.journal()) else {
        return Vec::new();
    };
    let mut out: Vec<Value> = text
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter(|v| v["pkg"] == name)
        .collect();
    out.reverse();
    out.truncate(n);
    out.into_iter()
        .map(|v| {
            json!({
                "ts": v["ts"], "op": v["op"], "provider": v["provider"],
                "from": v["from"], "to": v["to"], "exit": v["exit"], "run": v["run"],
            })
        })
        .collect()
}

fn deep(l: &Loaded, dirs: &Dirs, snap: &Snapshot, name: &str) -> Result<Report, Diagnostic> {
    let Some(pkg) = l.rc.repo.packages.get(name) else {
        let sug = suggest(name, l.rc.repo.packages.keys().map(String::as_str))
            .map(|s| format!(" Did you mean `{s}`?"))
            .unwrap_or_default();
        return Err(Diagnostic::from_code(
            "unknown-package",
            &[("name", name), ("suggestion", &sug)],
        ));
    };
    let res = l.resolution.get(name);
    let items: Vec<&Item> = snap.items.iter().filter(|i| i.package == name).collect();
    let main = items
        .iter()
        .find(|i| i.area == Area::Packages || i.area == Area::Git)
        .copied();
    let state = main.map_or(State::NotApplicable, |i| i.state);
    let files: Vec<&Item> = items
        .iter()
        .filter(|i| i.area == Area::Files)
        .copied()
        .collect();
    let todos: Vec<&Item> = items
        .iter()
        .filter(|i| i.area == Area::Todos)
        .copied()
        .collect();
    let ovr = overrides(l, dirs, name);
    let hist = history(dirs, name, 5);
    let (header_json, _) = header(l, dirs);
    let provider_json = res.and_then(|r| r.provider.as_ref()).map(|p| p.to_json());
    let bundle = res.and_then(|r| {
        r.inclusions
            .iter()
            .find(|i| i.active)
            .or(r.inclusions.first())
    });

    let data = json!({
        "view": "package",
        "header": header_json,
        "package": name,
        "description": pkg.description,
        "state": state.word(),
        "detail": main.map(|i| i.detail.clone()),
        "provider": provider_json,
        "versions": {
            "wanted": pkg.version.clone().unwrap_or_else(|| "latest".into()),
            "installed": main.and_then(|i| i.have.clone()),
            "locked": lock_row(
                &dots_lock::Lock::load(&crate::lock_cmd::path(&l.rc.m.root)).unwrap_or_default(),
                &crate::lock_cmd::machine_key(&l.rc),
                name,
                res.and_then(|r| r.chosen_provider()).map_or("", |c| c.id.as_str()),
            )
            .map(|r| r.shown()),
            "latest": main.and_then(|i| i.latest.clone()),
        },
        "inclusion": bundle.map(|i| i.display()),
        "files": files.iter().map(|i| i.to_json()).collect::<Vec<_>>(),
        "todos": todos.iter().map(|i| i.to_json()).collect::<Vec<_>>(),
        "env": {
            "path": pkg.env.path,
            "vars": pkg.env.vars,
            "source": pkg.env.source,
        },
        "overrides": ovr.iter().map(|(s, f)| json!({"scope": s, "file": f})).collect::<Vec<_>>(),
        "history": hist,
    });
    let mut report = Report::ok("status", data);
    let head = match &pkg.description {
        Some(d) => format!("{name}   {d}"),
        None => name.to_string(),
    };
    report = report.with_headline(state.tone(), format!("{head}   {}", state.word()));

    // Provider and why.
    if let Some(sel) = res.and_then(|r| r.provider.as_ref()) {
        let mut sec = Section::new("Provider");
        let line = match &sel.chosen {
            Some(c) => format!("provider: {}", c.id),
            None if sel.candidates.is_empty() => "provider: none needed (files only)".to_string(),
            None => "provider: none usable".to_string(),
        };
        sec = sec.block(Block::text(line));
        let mut lines: Vec<String> = sel.reasons();
        for c in &sel.candidates {
            if matches!(c.outcome, dots_resolve::CandidateOutcome::Standby) {
                lines.push(c.line());
            }
        }
        if !lines.is_empty() {
            sec = sec.block(Block::Code { title: None, lines });
        }
        report.push(Block::Section(sec));
    } else if let Some(i) = main {
        let text = match res.map(|r| &r.state) {
            Some(dots_resolve::PackageState::Inert { condition }) if !condition.is_empty() => {
                format!("{}: `{condition}` is false", i.detail)
            }
            _ => i.detail.clone(),
        };
        report.push(Block::text(text));
    }

    // Versions.
    let ver = |v: Option<&String>| v.cloned().unwrap_or_else(|| "-".into());
    let mut facts = vec![
        (
            "wanted".to_string(),
            Cell::from(pkg.version.clone().unwrap_or_else(|| "latest".into())),
        ),
        (
            "installed".to_string(),
            Cell::from(ver(main.and_then(|i| i.have.as_ref()))),
        ),
        (
            "locked".to_string(),
            match lock_row(
                &dots_lock::Lock::load(&crate::lock_cmd::path(&l.rc.m.root)).unwrap_or_default(),
                &crate::lock_cmd::machine_key(&l.rc),
                name,
                res.and_then(|r| r.chosen_provider())
                    .map_or("", |c| c.id.as_str()),
            ) {
                Some(r) => Cell::from(r.shown()),
                None => Cell::muted("-"),
            },
        ),
        (
            "latest".to_string(),
            Cell::from(ver(main.and_then(|i| i.latest.as_ref()))),
        ),
    ];
    if let Some(b) = bundle {
        facts.push(("path".to_string(), Cell::muted(b.display())));
    }
    report.push(Block::Section(
        Section::new("Version").block(Block::Facts(facts)),
    ));

    if !files.is_empty() {
        let mut t = table_for(Area::Files);
        for it in &files {
            t.push(row_for(it));
        }
        let mut sec = Section::new("Files");
        for (s, n) in snap_counts(&files) {
            sec = sec.pill(s, n);
        }
        report.push(Block::Section(sec.block(t)));
    }
    let env = &pkg.env;
    if !env.path.is_empty() || !env.vars.is_empty() || !env.source.is_empty() {
        let mut f: Vec<(String, Cell)> = Vec::new();
        if !env.path.is_empty() {
            f.push(("path".into(), Cell::from(env.path.join(":"))));
        }
        for (k, v) in &env.vars {
            f.push((k.clone(), Cell::from(v.clone())));
        }
        for s in &env.source {
            f.push(("source".into(), Cell::from(s.clone())));
        }
        report.push(Block::Section(Section::new("Env").block(Block::Facts(f))));
    }
    if !todos.is_empty() {
        let mut t = table_for(Area::Todos);
        for it in &todos {
            t.push(row_for(it));
        }
        report.push(Block::Section(Section::new("Todos").block(t)));
    }
    if !ovr.is_empty() {
        let f: Vec<(String, Cell)> = ovr
            .iter()
            .map(|(s, file)| (s.clone(), Cell::from(file.clone())))
            .collect();
        report.push(Block::Section(
            Section::new("Overrides").block(Block::Facts(f)),
        ));
    }
    if !hist.is_empty() {
        let lines: Vec<String> = hist
            .iter()
            .map(|h| {
                let ts = h["ts"].as_str().unwrap_or("").get(..10).unwrap_or("");
                let op = h["op"].as_str().unwrap_or("");
                let change = match (h["from"].as_str(), h["to"].as_str()) {
                    (Some(f), Some(t)) => format!("{f} -> {t}"),
                    (None, Some(t)) => t.to_string(),
                    (Some(f), None) => f.to_string(),
                    _ => String::new(),
                };
                format!("{ts}  {op} {change}").trim_end().to_string()
            })
            .collect();
        report.push(Block::Section(
            Section::new("History").block(Block::Code { title: None, lines }),
        ));
    }
    if let Some(i) = main
        && let Some(h) = i.hints.first()
    {
        report.push(Block::hint(h.clone()));
    }
    Ok(report)
}

fn snap_counts(items: &[&Item]) -> Vec<(State, usize)> {
    State::ALL
        .iter()
        .filter_map(|s| {
            let n = items.iter().filter(|i| i.state == *s).count();
            (n > 0).then_some((*s, n))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use dots_providers::{Caps, Provider, Registry};
    use dots_testkit::FakeProvider;
    use dots_ui::{Blocks, Tier, Ui};

    use super::*;
    use crate::inspect::{CacheFile, LatestCache, testing::Fixture};

    fn text(r: &Report) -> String {
        Blocks::new(&Ui::fixed(Tier::Plain, 200)).report(r)
    }

    fn state_of(snap: &Snapshot, name: &str) -> State {
        snap.items
            .iter()
            .find(|i| i.package == name && matches!(i.area, Area::Packages | Area::Git))
            .unwrap_or_else(|| panic!("no row for {name}"))
            .state
    }

    fn cache(dirs: &Dirs, provider: &str, latest: &[(&str, &str)]) {
        let f = CacheFile {
            fetched: crate::inspect::now_secs(),
            versions: latest
                .iter()
                .map(|(k, v)| (k.to_string(), Some(v.to_string())))
                .collect(),
        };
        LatestCache::new(dirs).store(provider, &f).unwrap();
    }

    #[test]
    fn deep_view_says_why_apt_was_skipped() {
        let fx = Fixture::new(&[(
            "fd",
            "description = \"A fast find\"\n[providers.apt]\n[providers.cargo]\n",
        )]);
        let root_only = Caps {
            needs_root: true,
            ..FakeProvider::new("x").caps()
        };
        let reg = Registry::new()
            .with(Box::new(FakeProvider::new("apt").with_caps(root_only)))
            .with(Box::new(
                FakeProvider::new("cargo").with_installed("fd", "10.1.0"),
            ));
        let l = fx.load(reg, Fixture::facts(false), "");
        cache(&fx.dirs, "cargo", &[("fd", "10.2.0")]);
        let snap = compute(&l, &fx.dirs, true);
        let r = deep(&l, &fx.dirs, &snap, "fd").unwrap();
        let out = text(&r);
        assert!(out.contains("apt skipped: needs root"), "{out}");
        assert!(out.contains("provider: cargo"), "{out}");
        assert!(out.contains("A fast find"), "{out}");
        assert!(
            out.contains("10.1.0") && out.contains("10.2.0") && out.contains("wanted"),
            "{out}"
        );
        assert_eq!(r.data["state"], "outdated");
        assert_eq!(r.data["versions"]["latest"], "10.2.0");
        assert_eq!(r.data["provider"]["chosen"]["id"], "cargo");
    }

    #[test]
    fn every_package_state_is_reported() {
        let fx = Fixture::new(&[
            ("fine", "[providers.cargo]\n"),
            ("old", "[providers.cargo]\n"),
            ("gone", "[providers.cargo]\n"),
            ("pinned", "version = \"2.0.0\"\n[providers.cargo]\n"),
            (
                "winonly",
                "when = 'platform = \"windows\"'\n[providers.cargo]\n",
            ),
        ]);
        let cargo = FakeProvider::new("cargo")
            .with_installed("fine", "1.0.0")
            .with_installed("old", "1.0.0")
            .with_installed("pinned", "1.0.0")
            .with_installed("orphaned", "3.0.0")
            .with_installed("stray", "9.9.9");
        let l = fx.load(
            Registry::new().with(Box::new(cargo)),
            Fixture::facts(true),
            "",
        );
        cache(
            &fx.dirs,
            "cargo",
            &[("fine", "1.0.0"), ("old", "1.5.0"), ("pinned", "3.0.0")],
        );
        let mut owned = OwnedState::default();
        owned.packages.insert(
            "orphaned".into(),
            dots_engine::OwnedPackage {
                provider: "cargo".into(),
                name: "orphaned".into(),
                version: Some("3.0.0".into()),
                run: "r-1".into(),
                ts: "t".into(),
            },
        );
        owned.todos.push(dots_engine::Todo {
            package: "fine".into(),
            reason: "grant access".into(),
            run: "r-1".into(),
        });
        owned.save(&fx.dirs.state_json()).unwrap();

        let snap = compute(&l, &fx.dirs, false);
        assert_eq!(state_of(&snap, "fine"), State::Ok);
        assert_eq!(state_of(&snap, "old"), State::Outdated);
        assert_eq!(state_of(&snap, "gone"), State::Missing);
        // wants 2.0.0, has 1.0.0: drift, and the newer latest is not "outdated".
        assert_eq!(state_of(&snap, "pinned"), State::Drift);
        assert_eq!(state_of(&snap, "winonly"), State::NotApplicable);
        assert_eq!(state_of(&snap, "orphaned"), State::Orphan);
        assert!(
            snap.items.iter().all(|i| i.package != "stray"),
            "unmanaged only with --all"
        );
        let all = compute(&l, &fx.dirs, true);
        assert_eq!(state_of(&all, "stray"), State::Unmanaged);

        let r = summary(&l, &fx.dirs, &snap, false);
        let out = text(&r);
        // outdated + missing + drift = 3 things; orphan and todo are shown but are not attention.
        assert_eq!(r.data["attention"], 3, "{out}");
        assert!(out.contains("3 things need attention"), "{out}");
        assert!(
            out.contains("old") && out.contains("1.0.0 -> 1.5.0"),
            "{out}"
        );
        assert!(
            out.contains("orphaned") && out.contains("grant access"),
            "{out}"
        );
        assert!(!out.contains("winonly") && !out.contains("[ok]"), "{out}");
        assert_eq!(r.data["counts"]["packages"]["ok"], 1);
        // --all adds the inert and unmanaged rows.
        let out_all = text(&summary(&l, &fx.dirs, &all, true));
        assert!(
            out_all.contains("winonly") && out_all.contains("n/a on this platform"),
            "{out_all}"
        );
        assert!(out_all.contains("stray"), "{out_all}");
    }

    /// 100 packages over three fake providers, a warm latest cache: the whole inspection
    /// (installed queries, resolution, files, status) stays under the 1.5 s target.
    #[test]
    fn hundred_fake_packages_under_the_budget() {
        let ids = ["brew", "cargo", "apt"];
        let bodies: Vec<(String, String)> = (0..100)
            .map(|i| {
                (
                    format!("pkg{i:03}"),
                    format!("[providers.{}]\n", ids[i % 3]),
                )
            })
            .collect();
        let pkgs: Vec<(&str, &str)> = bodies
            .iter()
            .map(|(n, b)| (n.as_str(), b.as_str()))
            .collect();
        let fx = Fixture::new(&pkgs);
        let providers: Vec<FakeProvider> = ids.iter().map(|i| FakeProvider::new(i)).collect();
        for (i, (name, _)) in bodies.iter().enumerate() {
            if i % 10 != 0 {
                providers[i % 3].set_installed(name, "1.0.0");
            }
        }
        for (k, id) in ids.iter().enumerate() {
            let latest: Vec<(String, &str)> = bodies
                .iter()
                .enumerate()
                .filter(|(i, _)| i % 3 == k)
                .map(|(i, (n, _))| (n.clone(), if i % 7 == 0 { "2.0.0" } else { "1.0.0" }))
                .collect();
            let refs: Vec<(&str, &str)> = latest.iter().map(|(n, v)| (n.as_str(), *v)).collect();
            cache(&fx.dirs, id, &refs);
        }
        let mut samples = Vec::new();
        for _ in 0..5 {
            let mut reg = Registry::new();
            for p in &providers {
                reg.register(Box::new(p.clone()));
            }
            let start = std::time::Instant::now();
            let l = fx.load(reg, Fixture::facts(true), "");
            let snap = compute(&l, &fx.dirs, false);
            let r = summary(&l, &fx.dirs, &snap, false);
            samples.push(start.elapsed());
            assert_eq!(r.data["counts"]["packages"]["missing"], 10);
            assert!(r.data["counts"]["packages"]["outdated"].as_u64().unwrap() > 5);
        }
        samples.sort();
        let median = samples[2];
        eprintln!("status over 100 fake packages: median {median:?}");
        assert!(
            median < std::time::Duration::from_millis(1500),
            "{samples:?}"
        );
    }
}
