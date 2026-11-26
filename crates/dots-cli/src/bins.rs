//! `dots bins` and `dots which`: every executable on PATH, and who owns it (ARCHITECTURE 7.4).
//!
//! The steps: walk PATH in order (first match wins, later copies are shadowed), attribute each
//! file to a provider (the providers' own `bins()` first, then a Cellar-path heuristic, then the
//! providers' `owner_of()` reverse lookup), and join the owner with dots' model (declared,
//! orphan, unmanaged, unknown).

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use dots_core::{
    Block, Cell, Diagnostic, Dirs, ExitCode, Lead, Marker, Report, Row, Section, State, Style,
    Table, Tone,
};
use dots_engine::State as OwnedState;
use dots_facts::suggest;
use serde_json::{Value, json};

use crate::app::Context;
use crate::inspect::{Loaded, load, par_map, tilde};

pub fn is_executable(p: &Path) -> bool {
    let Ok(m) = std::fs::metadata(p) else {
        return false;
    };
    if !m.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        m.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

/// Copies of `name` on `path`, in PATH order, one per distinct real file.
pub fn find_copies(path: &[PathBuf], name: &str) -> Vec<PathBuf> {
    let mut seen: BTreeSet<PathBuf> = BTreeSet::new();
    let mut out = Vec::new();
    for dir in path {
        let p = dir.join(name);
        if !is_executable(&p) {
            continue;
        }
        let real = std::fs::canonicalize(&p).unwrap_or_else(|_| p.clone());
        if seen.insert(real) {
            out.push(p);
        }
    }
    out
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Class {
    Managed,
    Orphan,
    Unmanaged,
    Unknown,
}

impl Class {
    fn word(self) -> &'static str {
        match self {
            Class::Managed => "managed",
            Class::Orphan => "orphan",
            Class::Unmanaged => "unmanaged",
            Class::Unknown => "unknown",
        }
    }
}

/// One executable file found on PATH.
#[derive(Debug, Clone)]
pub struct BinCopy {
    pub bin: String,
    pub path: PathBuf,
    pub real: PathBuf,
    pub shim_to: Option<PathBuf>,
    pub provider: Option<String>,
    /// The provider-native package name.
    pub native: Option<String>,
    /// The declared dots package (managed and orphan copies).
    pub package: Option<String>,
    /// A declared package with the same name as `native`, owned by another provider.
    pub same_name_as: Option<String>,
    pub version: Option<String>,
    pub class: Class,
    pub shadowed: bool,
    /// Another provider owns the winning copy of this name.
    pub duplicate_of: Option<(String, String)>,
    /// An unmanaged copy whose package the package manager installed only as a dependency of
    /// another one (brew `installed_on_request` false, apt auto-installed).
    pub dependency: bool,
}

fn walk_path(dirs: &[PathBuf], only: Option<&str>) -> Vec<(String, PathBuf)> {
    let mut out = Vec::new();
    for dir in dirs {
        if let Some(name) = only {
            let p = dir.join(name);
            if is_executable(&p) {
                out.push((name.to_string(), p));
            }
            continue;
        }
        let Ok(rd) = std::fs::read_dir(dir) else {
            continue;
        };
        let mut names: Vec<String> = rd
            .flatten()
            .filter_map(|e| e.file_name().into_string().ok())
            .collect();
        names.sort();
        for n in names {
            let p = dir.join(&n);
            if is_executable(&p) {
                out.push((n, p));
            }
        }
    }
    out
}

/// `…/Cellar/<formula>/<version>/…` belongs to brew's `<formula>`.
fn cellar_owner(real: &Path) -> Option<String> {
    let comps: Vec<_> = real.components().collect();
    let i = comps.iter().position(|c| c.as_os_str() == "Cellar")?;
    Some(comps.get(i + 1)?.as_os_str().to_string_lossy().into_owned())
}

/// Every copy of every executable (or only `only`) on the user's PATH, attributed and joined
/// with dots' model.
pub fn inventory(l: &Loaded, dirs: &Dirs, only: Option<&str>) -> Vec<BinCopy> {
    let path = l.path_dirs(dirs);
    // 1. walk
    let mut seen_real: BTreeSet<(String, PathBuf)> = BTreeSet::new();
    let mut copies: Vec<BinCopy> = Vec::new();
    for (bin, p) in walk_path(&path, only) {
        let real = std::fs::canonicalize(&p).unwrap_or_else(|_| p.clone());
        if !seen_real.insert((bin.clone(), real.clone())) {
            continue;
        }
        let is_link = std::fs::symlink_metadata(&p).is_ok_and(|m| m.file_type().is_symlink());
        let shim_to =
            (is_link && real.file_name().is_some_and(|f| f != bin.as_str())).then(|| real.clone());
        copies.push(BinCopy {
            bin,
            path: p,
            real,
            shim_to,
            provider: None,
            native: None,
            package: None,
            same_name_as: None,
            version: None,
            class: Class::Unknown,
            shadowed: false,
            duplicate_of: None,
            dependency: false,
        });
    }

    // 2. forward scan: each provider's own claim on what it put on disk.
    let jobs: Vec<(&str, &dyn dots_providers::Provider)> = l
        .registry
        .ids()
        .flat_map(|id| l.registry.instances(id).map(move |(_, p)| (id, p)))
        .collect();
    let empty = BTreeMap::new();
    let results = par_map(&jobs, |(id, p)| {
        p.bins(&l.pctx, l.installed.get(*id).unwrap_or(&empty))
    });
    let mut table: BTreeMap<PathBuf, (String, String)> = BTreeMap::new();
    for ((id, _), r) in jobs.iter().zip(results) {
        for e in r.unwrap_or_default() {
            let real = std::fs::canonicalize(&e.path).unwrap_or_else(|_| e.path.clone());
            table
                .entry(e.path.clone())
                .or_insert((id.to_string(), e.package.clone()));
            table.entry(real).or_insert((id.to_string(), e.package));
        }
    }
    for c in &mut copies {
        if let Some((prov, native)) = table.get(&c.real).or_else(|| table.get(&c.path)) {
            c.provider = Some(prov.clone());
            c.native = Some(native.clone());
        } else if let Some(f) = cellar_owner(&c.real) {
            c.provider = Some("brew".into());
            c.native = Some(f);
        }
    }

    // 3. reverse lookup for what is left, one batched call per provider.
    let unmatched: Vec<PathBuf> = copies
        .iter()
        .filter(|c| c.provider.is_none())
        .map(|c| c.real.clone())
        .collect();
    if !unmatched.is_empty() {
        let ids: Vec<&str> = l.registry.ids().collect();
        let owners = par_map(&ids, |id| {
            l.registry
                .get(id)
                .and_then(|p| p.owner_of(&l.pctx, &unmatched).ok())
                .unwrap_or_default()
        });
        for c in copies.iter_mut().filter(|c| c.provider.is_none()) {
            for (id, m) in ids.iter().zip(&owners) {
                if let Some(native) = m.get(&c.real) {
                    c.provider = Some((*id).to_string());
                    c.native = Some(native.clone());
                    break;
                }
            }
        }
    }

    // 4. join with dots' model.
    let owned = OwnedState::load(&dirs.state_json()).0;
    let deps = dependencies(l);
    for c in &mut copies {
        let (Some(prov), Some(native)) = (c.provider.clone(), c.native.clone()) else {
            continue;
        };
        c.version = l
            .installed
            .get(&prov)
            .and_then(|m| m.get(&native))
            .map(|i| i.version.clone());
        let declared = l.active().find(|a| {
            a.choice
                .is_some_and(|ch| ch.id == prov && ch.native_name == native)
        });
        if let Some(a) = declared {
            c.package = Some(a.name.to_string());
            c.class = Class::Managed;
        } else if let Some((name, _)) = owned.packages.iter().find(|(name, o)| {
            // A package that is still declared is not an orphan, whatever copy this is.
            o.provider == prov
                && o.name == native
                && !l.resolution.get(name).is_some_and(|p| p.is_active())
        }) {
            c.package = Some(name.clone());
            c.class = Class::Orphan;
        } else {
            c.class = Class::Unmanaged;
            c.dependency = deps.get(&prov).is_some_and(|d| d.contains(&native));
            c.same_name_as = l
                .resolution
                .get(&native)
                .filter(|p| p.is_active())
                .map(|p| p.name.clone());
        }
    }

    // 5. shadowing: the first copy of a name wins.
    let mut winner: BTreeMap<String, usize> = BTreeMap::new();
    for i in 0..copies.len() {
        let bin = copies[i].bin.clone();
        match winner.get(&bin) {
            None => {
                winner.insert(bin, i);
            }
            Some(&w) => {
                copies[i].shadowed = true;
                let (wp, wn) = (copies[w].provider.clone(), copies[w].package.clone());
                if let (Some(wp), Some(cp)) = (wp, copies[i].provider.clone())
                    && wp != cp
                {
                    let name = wn
                        .or_else(|| copies[w].native.clone())
                        .unwrap_or_else(|| bin_name(&copies[i]));
                    copies[i].duplicate_of = Some((name, wp));
                }
            }
        }
    }
    copies.sort_by(|a, b| a.bin.cmp(&b.bin));
    copies
}

/// Per provider, the installed packages that were not asked for: the package manager pulled
/// them in as dependencies (`Provider::roots`). A provider that cannot tell contributes
/// nothing, so its installs all count as asked for. Those are never orphans (an orphan is
/// something dots installed), and `status --all` and `bins` label them as dependencies.
pub fn dependencies(l: &Loaded) -> BTreeMap<String, BTreeSet<String>> {
    let ids: Vec<&str> = l.registry.ids().collect();
    let results = par_map(&ids, |id| {
        let roots = l.registry.get(id)?.roots(&l.pctx).ok().flatten()?;
        let installed = l.installed.get(*id)?;
        Some(
            installed
                .keys()
                .filter(|k| !roots.contains(*k))
                .cloned()
                .collect::<BTreeSet<String>>(),
        )
    });
    ids.iter()
        .zip(results)
        .filter_map(|(id, r)| Some((id.to_string(), r?)))
        .collect()
}

fn bin_name(c: &BinCopy) -> String {
    c.bin.clone()
}

impl BinCopy {
    fn notes(&self, home: &Path) -> Vec<String> {
        let mut n = Vec::new();
        if let Some(t) = &self.shim_to {
            n.push(format!("shim -> {}", tilde(t, home)));
        }
        if self.shadowed {
            n.push("shadowed".into());
        }
        if self.duplicate_of.is_some() {
            n.push("duplicate install".into());
        }
        match self.class {
            Class::Managed => {}
            Class::Orphan => n.push("orphan (installed by dots, no longer declared)".into()),
            Class::Unmanaged if self.dependency => {
                n.push("unmanaged (installed as a dependency of another package)".into());
            }
            Class::Unmanaged => n.push("unmanaged (installed outside dots)".into()),
            Class::Unknown => n.push("unknown owner".into()),
        }
        n
    }

    fn hint(&self) -> Option<String> {
        self.duplicate_of
            .as_ref()
            .map(|(pkg, to)| format!("dots migrate {pkg} --to {to}"))
    }

    fn package_cell(&self) -> Cell {
        match (&self.package, self.class) {
            (Some(p), _) => Cell::accent(p.clone()),
            (None, Class::Unknown) => Cell::muted("?"),
            (None, _) => match &self.same_name_as {
                Some(n) => Cell::muted(format!("({n})")),
                None => Cell::muted("-"),
            },
        }
    }

    fn to_json(&self, home: &Path, wins: bool) -> Value {
        json!({
            "bin": self.bin,
            "path": tilde(&self.path, home),
            "real_path": tilde(&self.real, home),
            "package": self.package,
            "native": self.native,
            "provider": self.provider,
            "version": self.version,
            "class": self.class.word(),
            "dependency": self.dependency,
            "wins": wins,
            "shadowed": self.shadowed,
            "shim": self.shim_to.as_ref().map(|t| tilde(t, home)),
            "notes": self.notes(home),
            "hint": self.hint(),
        })
    }
}

// ---------------------------------------------------------------- bins

pub struct BinsArgs<'a> {
    pub target: Option<&'a str>,
    pub managed: bool,
    pub unmanaged: bool,
    pub unknown: bool,
    pub shadowed: bool,
}

pub fn bins_cmd(ctx: &Context, a: &BinsArgs) -> Report {
    match load(ctx, "bins") {
        Ok(l) => bins_report(&l, &ctx.dirs, a),
        Err(r) => *r,
    }
}

pub fn bins_report(l: &Loaded, dirs: &Dirs, a: &BinsArgs) -> Report {
    if let Some(t) = a.target {
        let is_provider = l.registry.ids().any(|id| id == t);
        if !is_provider && !l.rc.repo.packages.contains_key(t) {
            let sug = suggest(t, l.rc.repo.packages.keys().map(String::as_str))
                .map(|s| format!(" Did you mean `{s}`?"))
                .unwrap_or_default();
            let d = Diagnostic::from_code("unknown-package", &[("name", t), ("suggestion", &sug)]);
            return crate::inspect::failure("bins", vec![d], l.warnings.clone(), ExitCode::Error);
        }
    }
    let home = &dirs.home;
    let all = inventory(l, dirs, None);
    let winners: BTreeSet<usize> = (0..all.len()).filter(|&i| !all[i].shadowed).collect();
    let rows: Vec<(usize, &BinCopy)> = all
        .iter()
        .enumerate()
        .filter(|(_, c)| match a.target {
            Some(t) => {
                c.package.as_deref() == Some(t)
                    || c.native.as_deref() == Some(t)
                    || c.provider.as_deref() == Some(t)
            }
            None => true,
        })
        .filter(|(_, c)| !a.managed || c.class == Class::Managed)
        .filter(|(_, c)| !a.unmanaged || matches!(c.class, Class::Unmanaged | Class::Orphan))
        .filter(|(_, c)| !a.unknown || c.class == Class::Unknown)
        .filter(|(_, c)| !a.shadowed || c.shadowed)
        .collect();

    let count = |f: &dyn Fn(&BinCopy) -> bool| rows.iter().filter(|(_, c)| f(c)).count();
    let summary = json!({
        "total": rows.len(),
        "managed": count(&|c| c.class == Class::Managed),
        "unmanaged": count(&|c| matches!(c.class, Class::Unmanaged | Class::Orphan)),
        "unknown": count(&|c| c.class == Class::Unknown),
        "shadowed": count(&|c| c.shadowed),
    });
    let data = json!({
        "target": a.target,
        "bins": rows.iter().map(|(i, c)| c.to_json(home, winners.contains(i))).collect::<Vec<_>>(),
        "summary": summary,
    });
    let mut report = Report::ok("bins", data);
    if rows.is_empty() {
        return report.with_headline(Tone::Muted, "No executables match.");
    }
    report = report.with_headline(
        Tone::Info,
        format!(
            "{} on PATH   {} managed \u{b7} {} unmanaged \u{b7} {} unknown \u{b7} {} shadowed",
            dots_core::blocks::plural(rows.len(), "executable", "executables"),
            summary["managed"],
            summary["unmanaged"],
            summary["unknown"],
            summary["shadowed"],
        ),
    );
    let mut t = Table::new()
        .styled_column("path", 5, Style::Path)
        .column("package", 4)
        .styled_column("provider", 3, Style::Muted)
        .styled_column("version", 2, Style::Muted)
        .styled_column("note", 4, Style::Muted);
    for (_, c) in &rows {
        report_row(&mut t, c, home);
    }
    report.push(Block::Section(Section::new("BINS").block(t)));
    report
}

fn report_row(t: &mut Table, c: &BinCopy, home: &Path) {
    let lead: Lead = if c.shadowed {
        Marker::Shadow.into()
    } else {
        match c.class {
            Class::Managed => Lead::None,
            Class::Orphan => State::Orphan.into(),
            Class::Unmanaged | Class::Unknown => State::Unmanaged.into(),
        }
    };
    let mut row = Row::new(lead, c.bin.clone())
        .cell(Cell::path(tilde(&c.path, home)))
        .cell(c.package_cell())
        .cell(Cell::muted(
            c.provider.clone().unwrap_or_else(|| "-".into()),
        ))
        .cell(Cell::muted(c.version.clone().unwrap_or_else(|| "-".into())))
        .cell(Cell::muted(c.notes(home).join("; ")));
    if let Some(h) = c.hint() {
        row = row.hint(h);
    }
    t.push(row);
}

// ---------------------------------------------------------------- which

pub fn which_cmd(ctx: &Context, bin: &str) -> Report {
    match load(ctx, "which") {
        Ok(l) => which_report(&l, &ctx.dirs, bin),
        Err(r) => *r,
    }
}

pub fn which_report(l: &Loaded, dirs: &Dirs, bin: &str) -> Report {
    let home = &dirs.home;
    let copies = inventory(l, dirs, Some(bin));
    if copies.is_empty() {
        let d = Diagnostic::from_code("bin-not-found", &[("bin", bin)]);
        return crate::inspect::failure("which", vec![d], l.warnings.clone(), ExitCode::Error);
    }
    let data = json!({
        "bin": bin,
        "copies": copies.iter().map(|c| c.to_json(home, !c.shadowed)).collect::<Vec<_>>(),
    });
    let n = copies.len();
    let mut report = Report::ok("which", data).with_headline(
        if n > 1 { Tone::Warn } else { Tone::Ok },
        if n > 1 {
            format!("{bin}: {n} copies on PATH")
        } else {
            format!("{bin}: 1 copy on PATH")
        },
    );
    let mut t = Table::new()
        .column("owner", 5)
        .styled_column("provider", 3, Style::Muted)
        .styled_column("version", 2, Style::Muted)
        .styled_column("note", 4, Style::Muted);
    for c in &copies {
        let mark = if c.shadowed { "  " } else { "\u{25b6} " };
        let mut row = Row::named(
            Lead::None,
            Cell::path(format!("{mark}{}", tilde(&c.path, home))),
        )
        .cell(c.package_cell())
        .cell(Cell::muted(
            c.provider.clone().unwrap_or_else(|| "-".into()),
        ))
        .cell(Cell::muted(c.version.clone().unwrap_or_else(|| "-".into())))
        .cell(Cell::muted(c.notes(home).join("; ")));
        if let Some(h) = c.hint() {
            row = row.hint(h);
        }
        t.push(row);
    }
    report.push(Block::Section(Section::new("PATH order").block(t)));
    report
}

#[cfg(test)]
mod tests {
    use dots_providers::Registry;
    use dots_testkit::FakeProvider;
    use dots_ui::{Blocks, Tier, Ui};

    use super::*;
    use crate::inspect::testing::{BinsProvider, Fixture, exe};

    fn text(r: &Report) -> String {
        Blocks::new(&Ui::fixed(Tier::Plain, 260)).report(r)
    }

    fn args() -> BinsArgs<'static> {
        BinsArgs {
            target: None,
            managed: false,
            unmanaged: false,
            unknown: false,
            shadowed: false,
        }
    }

    /// bat from cargo (declared, preferred) and from apt (not what dots chose).
    fn duplicate() -> (Fixture, Loaded) {
        let fx = Fixture::new(&[(
            "bat",
            "prefer = [\"cargo\"]\n[providers.cargo]\n[providers.apt]\n",
        )]);
        let t = fx.tmp.path();
        let (cargo_bin, usr_bin) = (t.join("home/.cargo/bin"), t.join("usr/bin"));
        exe(&cargo_bin.join("bat"));
        exe(&usr_bin.join("bat"));
        let reg = Registry::new()
            .with(Box::new(BinsProvider {
                inner: FakeProvider::new("cargo").with_installed("bat", "0.24.0"),
                bins: vec![("bat".into(), cargo_bin.join("bat"))],
            }))
            .with(Box::new(BinsProvider {
                inner: FakeProvider::new("apt").with_installed("bat", "0.19.0"),
                bins: vec![("bat".into(), usr_bin.join("bat"))],
            }));
        let path = format!("{}:{}", cargo_bin.display(), usr_bin.display());
        let l = fx.load(reg, Fixture::facts(true), &path);
        (fx, l)
    }

    #[test]
    fn which_marks_the_winner_and_suggests_migrate_on_the_other_copy() {
        let (fx, l) = duplicate();
        let r = which_report(&l, &fx.dirs, "bat");
        let out = text(&r);
        let lines: Vec<&str> = out.lines().collect();
        let winner = lines
            .iter()
            .position(|l| l.trim_start().starts_with("> "))
            .unwrap();
        assert!(lines[winner].contains(".cargo/bin/bat"), "{out}");
        assert!(lines[winner].contains("cargo") && lines[winner].contains("0.24.0"));
        let other = lines
            .iter()
            .position(|l| l.contains("usr/bin/bat"))
            .unwrap();
        assert!(other > winner);
        assert!(
            lines[other].contains("shadowed") && lines[other].contains("apt"),
            "{out}"
        );
        assert!(out.contains("dots migrate bat --to cargo"), "{out}");
        let d = &r.data["copies"];
        assert_eq!(d[0]["wins"], true);
        assert_eq!(d[1]["wins"], false);
        assert_eq!(d[1]["hint"], "dots migrate bat --to cargo");
        assert_eq!(d[1]["class"], "unmanaged");
    }

    #[test]
    fn bins_lists_owner_provider_version_and_filters() {
        let (fx, l) = duplicate();
        let r = bins_report(&l, &fx.dirs, &args());
        assert_eq!(r.data["summary"]["total"], 2);
        assert_eq!(r.data["summary"]["managed"], 1);
        assert_eq!(r.data["summary"]["shadowed"], 1);
        let sh = bins_report(
            &l,
            &fx.dirs,
            &BinsArgs {
                shadowed: true,
                ..args()
            },
        );
        assert_eq!(sh.data["bins"].as_array().unwrap().len(), 1);
        assert_eq!(sh.data["bins"][0]["provider"], "apt");
        let by_provider = bins_report(
            &l,
            &fx.dirs,
            &BinsArgs {
                target: Some("cargo"),
                ..args()
            },
        );
        assert_eq!(by_provider.data["bins"].as_array().unwrap().len(), 1);
        let by_pkg = bins_report(
            &l,
            &fx.dirs,
            &BinsArgs {
                target: Some("bat"),
                ..args()
            },
        );
        assert_eq!(
            by_pkg.data["bins"].as_array().unwrap().len(),
            2,
            "both copies of bat"
        );
        let bad = bins_report(
            &l,
            &fx.dirs,
            &BinsArgs {
                target: Some("nope"),
                ..args()
            },
        );
        assert_eq!(bad.errors[0].code, "unknown-package");
    }

    #[test]
    fn a_file_no_provider_owns_is_an_unknown_owner() {
        let fx = Fixture::new(&[("bat", "[providers.cargo]\n")]);
        let local = fx.dirs.home.join(".local/bin");
        exe(&local.join("foo"));
        let reg = Registry::new().with(Box::new(FakeProvider::new("cargo")));
        let l = fx.load(reg, Fixture::facts(true), &local.display().to_string());
        let r = bins_report(
            &l,
            &fx.dirs,
            &BinsArgs {
                unknown: true,
                ..args()
            },
        );
        let out = text(&r);
        assert!(
            out.contains("foo") && out.contains("unknown owner"),
            "{out}"
        );
        assert!(out.contains(" ? "), "package column is `?`: {out}");
        let b = &r.data["bins"][0];
        assert_eq!(b["class"], "unknown");
        assert!(b["package"].is_null());
        assert_eq!(b["path"], "~/.local/bin/foo");
        // Not managed: the `--managed` filter drops it.
        let m = bins_report(
            &l,
            &fx.dirs,
            &BinsArgs {
                managed: true,
                ..args()
            },
        );
        assert_eq!(m.data["bins"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn a_symlink_to_another_name_is_a_shim() {
        let fx = Fixture::new(&[("fd", "[providers.apt]\n")]);
        let (usr, local) = (
            fx.tmp.path().join("usr/bin"),
            fx.dirs.home.join(".local/bin"),
        );
        exe(&usr.join("fdfind"));
        std::fs::create_dir_all(&local).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(usr.join("fdfind"), local.join("fd")).unwrap();
        let reg = Registry::new().with(Box::new(BinsProvider {
            inner: FakeProvider::new("apt").with_installed("fd", "8.7.0"),
            bins: vec![("fd".into(), usr.join("fdfind"))],
        }));
        let l = fx.load(reg, Fixture::facts(true), &local.display().to_string());
        let r = bins_report(&l, &fx.dirs, &args());
        let b = &r.data["bins"][0];
        assert_eq!(b["package"], "fd");
        assert_eq!(b["provider"], "apt");
        assert!(
            b["shim"].as_str().unwrap().ends_with("usr/bin/fdfind"),
            "{b}"
        );
        assert!(text(&r).contains("shim -> "), "{}", text(&r));
    }

    #[test]
    fn which_of_a_missing_binary_is_an_error() {
        let fx = Fixture::new(&[("bat", "[providers.cargo]\n")]);
        let l = fx.load(
            Registry::new().with(Box::new(FakeProvider::new("cargo"))),
            Fixture::facts(true),
            "",
        );
        let r = which_report(&l, &fx.dirs, "nothing-here");
        assert_eq!(r.errors[0].code, "bin-not-found");
        assert_eq!(r.exit, ExitCode::Error);
    }
}
