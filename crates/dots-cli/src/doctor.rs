//! `dots doctor`: environment problems, each with a fix. Read-only.
//!
//! Checks: providers (unavailable, unqueryable, nothing can install a package), dangling
//! links, PATH order that shadows managed binaries, the generated env file and its rc line, the
//! active-root link, the lock file, sudo, symlink capability (Windows), unknown platform keys.

use std::collections::BTreeSet;
use std::path::Path;

use dots_core::blocks::plural;
use dots_core::{Block, Cell, Dirs, ExitCode, Report, Section, State, Tone};
use dots_env::{MARKER_OPEN, Shell, default_rc};
use dots_resolve::{CandidateOutcome, LintInputs, SkipReason, lint};
use serde_json::json;

use crate::app::Context;
use crate::bins::{Class, inventory};
use crate::inspect::{Loaded, load, tilde};
use crate::status::{Area, file_items};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Sev {
    Warn,
    Error,
}

impl Sev {
    fn word(self) -> &'static str {
        match self {
            Sev::Warn => "warn",
            Sev::Error => "error",
        }
    }
}

struct Finding {
    subject: String,
    sev: Sev,
    message: String,
    fix: String,
}

fn finding(subject: &str, sev: Sev, message: impl Into<String>, fix: impl Into<String>) -> Finding {
    Finding {
        subject: subject.to_string(),
        sev,
        message: message.into(),
        fix: fix.into(),
    }
}

struct Check {
    id: &'static str,
    title: &'static str,
    findings: Vec<Finding>,
}

fn providers(l: &Loaded) -> Vec<Finding> {
    let mut out = Vec::new();
    let mut seen: BTreeSet<String> = BTreeSet::new();
    for res in l.resolution.active() {
        let Some(sel) = &res.provider else { continue };
        let files_only = sel.candidates.is_empty();
        if sel.chosen.is_none() && !files_only {
            out.push(finding(
                &res.name,
                Sev::Error,
                "no provider can install it",
                format!("dots why {}", res.name),
            ));
        }
        for c in &sel.candidates {
            let CandidateOutcome::Skipped(r) = &c.outcome else {
                continue;
            };
            let reason = match r {
                SkipReason::Unavailable(d) => format!("is unavailable: {d}"),
                SkipReason::Bootstrap { package, why } => {
                    format!("needs bootstrap package `{package}`, but {why}")
                }
                _ => continue,
            };
            if seen.insert(format!("{}:{reason}", c.id)) {
                let sev = if sel.chosen.is_none() {
                    Sev::Error
                } else {
                    Sev::Warn
                };
                out.push(finding(
                    &c.id,
                    sev,
                    format!("{} (needed by {})", reason, res.name),
                    "install it, or prefer another provider in the package's `prefer` list",
                ));
            }
        }
        if let Some(c) = &sel.chosen
            && let Some(why) = l.failure(&c.id, &res.name)
            && seen.insert(format!("{}:{}:failed", c.id, why))
        {
            out.push(finding(
                &c.id,
                Sev::Error,
                format!("could not be queried: {why}"),
                format!("run the {} command by hand to see why it fails", c.id),
            ));
        }
    }
    out
}

fn links(l: &Loaded, dirs: &Dirs) -> Vec<Finding> {
    file_items(l, dirs)
        .into_iter()
        .filter(|i| i.area == Area::Files && i.state == State::Dangling)
        .map(|i| {
            finding(
                &i.name,
                Sev::Error,
                format!("dangling link ({})", i.package),
                i.hints
                    .first()
                    .cloned()
                    .unwrap_or_else(|| "dots apply".into()),
            )
        })
        .collect()
}

fn path_order(l: &Loaded, dirs: &Dirs) -> Vec<Finding> {
    let copies = inventory(l, dirs, None);
    let mut out = Vec::new();
    for c in copies
        .iter()
        .filter(|c| c.shadowed && c.class == Class::Managed)
    {
        let Some(w) = copies.iter().find(|w| w.bin == c.bin && !w.shadowed) else {
            continue;
        };
        let dir = |p: &Path| p.parent().map_or(String::new(), |d| tilde(d, &dirs.home));
        out.push(finding(
            &c.bin,
            Sev::Warn,
            format!(
                "{} ({}) is shadowed by {}",
                tilde(&c.path, &dirs.home),
                c.package.clone().unwrap_or_default(),
                tilde(&w.path, &dirs.home)
            ),
            format!(
                "put {} before {} on PATH; dots' env file does that: source {}",
                dir(&c.path),
                dir(&w.path),
                tilde(&dirs.env_file("sh"), &dirs.home)
            ),
        ));
    }
    out
}

fn env(l: &Loaded, dirs: &Dirs) -> Vec<Finding> {
    let contribs = l.contributions();
    let sh = dirs.env_file("sh");
    if contribs.is_empty() && !sh.exists() {
        return Vec::new();
    }
    let mut out = Vec::new();
    if !sh.exists() {
        out.push(finding(
            &tilde(&sh, &dirs.home),
            Sev::Warn,
            "the generated env file does not exist",
            "dots apply",
        ));
    }
    let shell = l
        .pctx
        .env
        .get("SHELL")
        .and_then(|s| Shell::detect(s))
        .or_else(|| Shell::detect(&l.rc.m.facts.shell));
    if let Some(s) = shell {
        let rc = default_rc(s, &dirs.home);
        let target = std::fs::canonicalize(&rc).unwrap_or_else(|_| rc.clone());
        let text = std::fs::read_to_string(target).unwrap_or_default();
        if !text.contains(MARKER_OPEN) {
            out.push(finding(
                &tilde(&rc, &dirs.home),
                Sev::Warn,
                "the env file is not sourced from your shell rc",
                "dots apply",
            ));
        }
    }
    let live: BTreeSet<&str> = l
        .pctx
        .env
        .get("PATH")
        .map(|p| p.split(':').collect())
        .unwrap_or_default();
    let missing: Vec<String> = l
        .env_path_entries(dirs)
        .into_iter()
        .filter(|d| !live.contains(d.as_str()))
        .collect();
    if !missing.is_empty() {
        out.push(finding(
            "PATH",
            Sev::Warn,
            format!("not in this shell's PATH: {}", missing.join(", ")),
            format!("start a new shell, or run `. {}`", tilde(&sh, &dirs.home)),
        ));
    }
    out
}

fn active_link(dirs: &Dirs) -> Vec<Finding> {
    let a = dirs.active();
    let is_link = std::fs::symlink_metadata(&a).is_ok_and(|m| m.file_type().is_symlink());
    if is_link && !a.exists() {
        return vec![finding(
            &tilde(&a, &dirs.home),
            Sev::Error,
            "the active-root link points at a missing directory",
            "dots roots   # then `dots use <root>`",
        )];
    }
    Vec::new()
}

fn lock(l: &Loaded) -> Vec<Finding> {
    let p = l.rc.m.root.join(dots_lock::FILE);
    let Ok(text) = std::fs::read_to_string(&p) else {
        return Vec::new();
    };
    if dots_lock::has_conflict_markers(&text) {
        return vec![finding(
            "dots.lock",
            Sev::Error,
            "the lock has unresolved merge conflict markers",
            "resolve the conflict in dots.lock, then run `dots lock --fix`",
        )];
    }
    let lock = match dots_lock::Lock::parse(&text) {
        Ok(k) => k,
        Err(e) => {
            return vec![finding(
                "dots.lock",
                Sev::Error,
                format!("the lock cannot be read: {e}"),
                "dots lock --fix",
            )];
        }
    };
    let mut out = Vec::new();
    // Rows for packages the repo no longer has.
    let mut gone: Vec<&str> = lock
        .pkgs
        .iter()
        .filter(|r| !l.rc.repo.packages.contains_key(&r.name))
        .map(|r| r.name.as_str())
        .collect();
    gone.sort_unstable();
    gone.dedup();
    if !gone.is_empty() {
        out.push(finding(
            "dots.lock",
            Sev::Warn,
            format!(
                "stale rows for packages the repo no longer has: {}",
                gone.join(", ")
            ),
            "dots lock --fix   # drops them",
        ));
    }
    // Rows this machine's spec has moved away from: apply re-resolves them.
    let key = crate::lock_cmd::machine_key(&l.rc);
    for res in l.resolution.active() {
        let Some(c) = res.chosen_provider() else {
            continue;
        };
        let Some(row) = lock.pkg(&res.name, &dots_lock::platform_for(&c.id, &key)) else {
            continue;
        };
        if !dots_lock::rules::lock_satisfies(&res.wanted, &dots_lock::rules::Locked::from(row)) {
            out.push(finding(
                &res.name,
                Sev::Warn,
                format!(
                    "stale lock row: locked {}, but the spec now wants {}",
                    row.shown(),
                    res.wanted
                ),
                format!("dots apply {}   # re-resolves the row", res.name),
            ));
        }
    }
    out
}

fn sudo(l: &Loaded) -> Vec<Finding> {
    if l.rc.m.facts.has_sudo {
        return Vec::new();
    }
    let mut by_provider: std::collections::BTreeMap<String, Vec<String>> = Default::default();
    for res in l.resolution.active() {
        let Some(sel) = &res.provider else { continue };
        for c in &sel.candidates {
            if matches!(c.outcome, CandidateOutcome::Skipped(SkipReason::NeedsRoot)) {
                by_provider
                    .entry(c.id.clone())
                    .or_default()
                    .push(res.name.clone());
            }
        }
    }
    by_provider
        .into_iter()
        .map(|(id, pkgs)| {
            finding(
                &id,
                Sev::Warn,
                format!(
                    "needs root but sudo is not available; skipped for {}",
                    pkgs.join(", ")
                ),
                "run as a user with sudo, set `[facts] sudo = true` in machine.toml if detection is wrong, or prefer a user-level provider",
            )
        })
        .collect()
}

#[cfg(windows)]
fn can_symlink(dir: &Path) -> bool {
    let src = dir.join(".dots-symlink-src");
    let dst = dir.join(".dots-symlink-test");
    let _ = std::fs::create_dir_all(dir);
    let ok =
        std::fs::write(&src, b"").is_ok() && std::os::windows::fs::symlink_file(&src, &dst).is_ok();
    let _ = std::fs::remove_file(&dst);
    let _ = std::fs::remove_file(&src);
    ok
}

#[cfg(not(windows))]
fn can_symlink(_: &Path) -> bool {
    true
}

fn symlinks(l: &Loaded, dirs: &Dirs) -> Vec<Finding> {
    if l.rc.m.facts.os == "windows" && !can_symlink(&dirs.cache) {
        return vec![finding(
            "symlinks",
            Sev::Error,
            "this account cannot create symbolic links",
            "turn on Developer Mode in Windows settings, or run from an elevated shell",
        )];
    }
    Vec::new()
}

fn platform_keys(l: &Loaded) -> Vec<Finding> {
    let rep = lint(&LintInputs {
        repo: &l.rc.repo,
        machine: Some(&l.rc.m.machine),
        chain: &l.rc.chain,
    });
    rep.errors
        .iter()
        .chain(&rep.warnings)
        .filter(|d| d.code == "unknown-platform")
        .map(|d| finding("platform", Sev::Warn, d.message.clone(), d.hint.clone()))
        .collect()
}

pub fn run(ctx: &Context) -> Report {
    match load(ctx, "doctor") {
        Ok(l) => report(&l, &ctx.dirs),
        Err(r) => *r,
    }
}

pub fn report(l: &Loaded, dirs: &Dirs) -> Report {
    let checks = vec![
        Check {
            id: "providers",
            title: "Providers",
            findings: providers(l),
        },
        Check {
            id: "links",
            title: "Links",
            findings: links(l, dirs),
        },
        Check {
            id: "path-order",
            title: "PATH order",
            findings: path_order(l, dirs),
        },
        Check {
            id: "env",
            title: "Environment",
            findings: env(l, dirs),
        },
        Check {
            id: "active-link",
            title: "Active root",
            findings: active_link(dirs),
        },
        Check {
            id: "lock",
            title: "Lock",
            findings: lock(l),
        },
        Check {
            id: "sudo",
            title: "Sudo",
            findings: sudo(l),
        },
        Check {
            id: "symlinks",
            title: "Symlinks",
            findings: symlinks(l, dirs),
        },
        Check {
            id: "platform-keys",
            title: "Platform keys",
            findings: platform_keys(l),
        },
    ];
    let errors: usize = checks
        .iter()
        .flat_map(|c| &c.findings)
        .filter(|f| f.sev == Sev::Error)
        .count();
    let warnings: usize = checks
        .iter()
        .flat_map(|c| &c.findings)
        .filter(|f| f.sev == Sev::Warn)
        .count();
    let passed: Vec<&Check> = checks.iter().filter(|c| c.findings.is_empty()).collect();

    let data = json!({
        "checks": checks.iter().map(|c| json!({
            "id": c.id,
            "status": match c.findings.iter().map(|f| f.sev).max_by_key(|s| *s as u8) {
                None => "ok",
                Some(s) => s.word(),
            },
            "findings": c.findings.iter().map(|f| json!({
                "subject": f.subject,
                "severity": f.sev.word(),
                "message": f.message,
                "fix": f.fix,
            })).collect::<Vec<_>>(),
        })).collect::<Vec<_>>(),
        "summary": { "errors": errors, "warnings": warnings, "passed": passed.len() },
    });
    let mut report = Report::ok("doctor", data);
    report.warnings = l.warnings.clone();
    if errors + warnings == 0 {
        report = report.with_headline(
            Tone::Ok,
            format!("All good. {} checks passed", checks.len()),
        );
    } else {
        let tone = if errors > 0 { Tone::Err } else { Tone::Warn };
        report = report.with_headline(
            tone,
            format!("{} found", plural(errors + warnings, "problem", "problems")),
        );
    }
    for c in checks.iter().filter(|c| !c.findings.is_empty()) {
        // Key/value blocks stay whole on a narrow terminal, where table columns would drop.
        let mut sec = Section::new(c.title);
        for f in &c.findings {
            let tone = match f.sev {
                Sev::Error => Tone::Err,
                Sev::Warn => Tone::Warn,
            };
            sec = sec
                .block(Block::Facts(vec![(
                    f.subject.clone(),
                    Cell::tone(f.message.clone(), tone),
                )]))
                .block(Block::hint(f.fix.clone()));
        }
        report.push(Block::Section(sec));
    }
    if !passed.is_empty() && errors + warnings > 0 {
        let names: Vec<&str> = passed.iter().map(|c| c.id).collect();
        report.push(Block::collapsed(
            State::Ok,
            format!(
                "{} ok: {}",
                plural(passed.len(), "check", "checks"),
                names.join(", ")
            ),
        ));
    }
    if errors > 0 {
        report.exit = ExitCode::Error;
    }
    report
}

#[cfg(test)]
mod tests {
    use dots_providers::{Availability, Provider, Registry};
    use dots_testkit::FakeProvider;
    use dots_ui::{Blocks, Tier, Ui};

    use super::*;
    use crate::inspect::testing::{BinsProvider, Fixture, exe};

    fn text(r: &Report) -> String {
        Blocks::new(&Ui::fixed(Tier::Plain, 260)).report(r)
    }

    fn findings<'a>(r: &'a Report, check: &str) -> &'a serde_json::Value {
        r.data["checks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["id"] == check)
            .unwrap_or_else(|| panic!("no check {check}"))
    }

    #[test]
    fn a_system_dir_before_cargo_shadows_the_managed_binary() {
        let fx = Fixture::new(&[("fd", "[providers.cargo]\n")]);
        let t = fx.tmp.path();
        let (usr, cargo) = (t.join("usr/bin"), fx.dirs.home.join(".cargo/bin"));
        exe(&usr.join("fd"));
        exe(&cargo.join("fd"));
        let reg = Registry::new().with(Box::new(BinsProvider {
            inner: FakeProvider::new("cargo").with_installed("fd", "10.2.0"),
            bins: vec![("fd".into(), cargo.join("fd"))],
        }));
        // `/usr/bin` first: the managed copy loses.
        let path = format!("{}:{}", usr.display(), cargo.display());
        let l = fx.load(reg, Fixture::facts(true), &path);
        let r = report(&l, &fx.dirs);
        let c = findings(&r, "path-order");
        assert_eq!(c["status"], "warn", "{c}");
        let f = &c["findings"][0];
        assert_eq!(f["subject"], "fd");
        assert!(
            f["message"].as_str().unwrap().contains("shadowed by"),
            "{f}"
        );
        let fix = f["fix"].as_str().unwrap();
        assert!(fix.contains("before") && fix.contains("env.sh"), "{fix}");
        assert!(text(&r).contains("PATH order"), "{}", text(&r));
        // A warning is not a failure.
        assert_eq!(r.exit, ExitCode::Success);
    }

    #[test]
    fn a_missing_provider_is_a_warning_with_a_fallback_and_an_error_without() {
        let both = Fixture::new(&[("fd", "[providers.apt]\n[providers.cargo]\n")]);
        let reg = Registry::new()
            .with(Box::new(
                FakeProvider::new("apt").with_availability(Availability::Unsupported),
            ))
            .with(Box::new(FakeProvider::new("cargo")));
        let l = both.load(reg, Fixture::facts(true), "");
        let r = report(&l, &both.dirs);
        let c = findings(&r, "providers");
        assert_eq!(c["status"], "warn", "{c}");
        assert_eq!(c["findings"][0]["subject"], "apt");
        assert_eq!(r.exit, ExitCode::Success);

        let only = Fixture::new(&[("fd", "[providers.apt]\n")]);
        let reg = Registry::new().with(Box::new(
            FakeProvider::new("apt").with_availability(Availability::Unsupported),
        ));
        let l = only.load(reg, Fixture::facts(true), "");
        let r = report(&l, &only.dirs);
        assert_eq!(findings(&r, "providers")["status"], "error");
        assert_eq!(r.exit, ExitCode::Error);
        assert!(text(&r).contains("dots why fd"), "{}", text(&r));
    }

    #[test]
    fn sudo_problems_name_the_skipped_provider() {
        let fx = Fixture::new(&[("fd", "[providers.apt]\n[providers.cargo]\n")]);
        let root_only = dots_providers::Caps {
            needs_root: true,
            ..FakeProvider::new("x").caps()
        };
        let reg = Registry::new()
            .with(Box::new(FakeProvider::new("apt").with_caps(root_only)))
            .with(Box::new(FakeProvider::new("cargo")));
        let l = fx.load(reg, Fixture::facts(false), "");
        let r = report(&l, &fx.dirs);
        let c = findings(&r, "sudo");
        assert_eq!(c["status"], "warn");
        assert_eq!(c["findings"][0]["subject"], "apt");
        assert!(c["findings"][0]["message"].as_str().unwrap().contains("fd"));
    }

    #[test]
    fn a_healthy_machine_passes_every_check() {
        let fx = Fixture::new(&[("fd", "[providers.cargo]\n")]);
        let l = fx.load(
            Registry::new().with(Box::new(
                FakeProvider::new("cargo").with_installed("fd", "1.0.0"),
            )),
            Fixture::facts(true),
            "",
        );
        let r = report(&l, &fx.dirs);
        assert_eq!(r.data["summary"]["errors"], 0);
        assert_eq!(r.data["summary"]["warnings"], 0);
        assert!(text(&r).contains("All good"), "{}", text(&r));
    }

    #[test]
    fn a_conflicted_lock_and_a_dangling_active_link_are_errors() {
        let fx = Fixture::new(&[("fd", "[providers.cargo]\n")]);
        std::fs::write(
            fx.root.join("dots.lock"),
            "<<<<<<< HEAD\nx\n=======\ny\n>>>>>>> b\n",
        )
        .unwrap();
        std::fs::create_dir_all(&fx.dirs.state).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(fx.tmp.path().join("gone"), fx.dirs.active()).unwrap();
        let l = fx.load(
            Registry::new().with(Box::new(
                FakeProvider::new("cargo").with_installed("fd", "1.0.0"),
            )),
            Fixture::facts(true),
            "",
        );
        let r = report(&l, &fx.dirs);
        assert_eq!(findings(&r, "lock")["status"], "error");
        #[cfg(unix)]
        assert_eq!(findings(&r, "active-link")["status"], "error");
        assert_eq!(r.exit, ExitCode::Error);
    }
}
