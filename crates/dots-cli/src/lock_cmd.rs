//! `dots.lock` plumbing for the commands: where it lives, writing the rows an `apply` produced,
//! and `dots lock --fix`.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::SystemTime;

use dots_core::{Diagnostic, ExitCode, Report, Tone};
use dots_engine::{LockUpdate, rfc3339};
use dots_lock::{FILE, Lock, PkgRow, Upsert, fix_text, has_conflict_markers};
use serde_json::json;

use crate::app::Context;
use crate::context::{RepoCtx, load_machine};

pub fn path(root: &Path) -> PathBuf {
    root.join(FILE)
}

/// This machine's platform key: `<most specific chain key>/<arch>`.
pub fn machine_key(rc: &RepoCtx) -> String {
    dots_lock::machine_key(rc.chain.levels(), rc.chain.arch())
}

/// The repo's short HEAD commit, which a lock row records as `by_rev`.
pub fn repo_rev(root: &Path) -> Option<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["rev-parse", "--short=8", "HEAD"])
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Commit exactly `paths` in the repo at `root` with `message`; returns the short commit id.
pub fn commit(root: &Path, paths: &[&str], message: &str) -> Result<String, Diagnostic> {
    let fail = |detail: String| Diagnostic::from_code("commit-failed", &[("detail", &detail)]);
    let git = |args: &[&str]| -> Result<String, String> {
        let out = Command::new("git")
            .arg("-C")
            .arg(root)
            .args(args)
            .output()
            .map_err(|e| e.to_string())?;
        if out.status.success() {
            Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
        } else {
            let err = String::from_utf8_lossy(&out.stderr);
            let msg = err.trim();
            Err(if msg.is_empty() {
                String::from_utf8_lossy(&out.stdout).trim().to_string()
            } else {
                msg.to_string()
            })
        }
    };
    let mut add = vec!["add", "--"];
    add.extend_from_slice(paths);
    git(&add).map_err(&fail)?;
    let mut commit = vec!["commit", "-q", "-m", message, "--"];
    commit.extend_from_slice(paths);
    git(&commit).map_err(&fail)?;
    git(&["rev-parse", "--short=8", "HEAD"]).map_err(fail)
}

/// One lock row that a run added or moved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Written {
    pub package: String,
    pub platform: String,
    pub from: Option<String>,
    pub to: String,
    pub added: bool,
}

/// Merge `updates` into `<root>/dots.lock`. Rows that lock exactly what they locked before are
/// left alone, so a no-op run changes no bytes.
pub fn write_updates(
    root: &Path,
    updates: &[LockUpdate],
    drops: &[dots_engine::LockDrop],
    content: &[dots_lock::ContentRow],
) -> Result<Vec<Written>, Diagnostic> {
    if updates.is_empty() && drops.is_empty() && content.is_empty() {
        return Ok(Vec::new());
    }
    let file = path(root);
    let mut lock = Lock::load(&file)?;
    let now = rfc3339(SystemTime::now());
    let by_rev = repo_rev(root);
    let mut written = Vec::new();
    let mut dropped = false;
    // Rows of the provider a package moved away from: the lock follows the new provider.
    for d in drops {
        if lock
            .pkg(&d.package, &d.platform)
            .is_some_and(|r| r.provider == d.provider)
        {
            dropped |= lock.remove_pkg(&d.package, &d.platform);
        }
    }
    for u in updates {
        let mut row = PkgRow::new(&u.package, &u.platform, &u.provider);
        row.version = u.version.clone();
        row.rev = u.rev.clone();
        row.git_ref = u.git_ref.clone();
        row.source = u.source.clone();
        row.locked_at = now.clone();
        row.by_rev = by_rev.clone();
        if row.version.is_none() && row.rev.is_none() {
            continue;
        }
        let from = lock.pkg(&u.package, &u.platform).map(PkgRow::shown);
        let to = row.shown();
        match lock.upsert(row) {
            Upsert::Unchanged => {}
            r => written.push(Written {
                package: u.package.clone(),
                platform: u.platform.clone(),
                from,
                to,
                added: r == Upsert::Added,
            }),
        }
    }
    let mut changed = dropped || !written.is_empty();
    for row in content {
        if lock.content_for(&row.item) != Some(row) {
            lock.set_content(row.clone());
            changed = true;
        }
    }
    if changed {
        lock.save(&file)?;
    }
    Ok(written)
}

/// Hashes of script `fetch` files that were downloaded for the first time in this run: when
/// neither the author nor the lock pinned the file, the hash it had is recorded (and checked
/// by every later download).
pub fn first_use_hashes(
    scripts: &crate::providers::ScriptHandles,
    repo: &dots_model::Repo,
    lock: &Lock,
    report: &dots_engine::ApplyReport,
) -> Vec<dots_lock::ContentRow> {
    let mut rows = Vec::new();
    for (pkg, handle) in &scripts.0 {
        let item = crate::providers::fetch_item(pkg);
        let ok = report
            .steps
            .iter()
            .any(|s| s.package == *pkg && s.status == dots_engine::StepStatus::Ok);
        let Some(hash) = handle.fetched_sha256().filter(|_| ok) else {
            continue;
        };
        let Some(dots_model::ProviderConfig::Script(sc)) = repo
            .packages
            .get(pkg)
            .and_then(|p| p.providers.get("script"))
        else {
            continue;
        };
        let Some(fetch) = &sc.fetch else { continue };
        if fetch.sha256.is_some() || lock.content_for(&item).is_some_and(|c| c.sha256.is_some()) {
            continue;
        }
        let mut row = dots_lock::ContentRow::new(&item);
        row.url = fetch.url.clone();
        row.sha256 = Some(hash);
        rows.push(row);
    }
    rows
}

/// `dots lock --fix`.
pub fn run(ctx: &Context) -> Report {
    // Read the machine config for the repo root only: the repo itself may refuse to load while
    // the lock is broken, and this command must still work.
    let m = match load_machine(ctx, None) {
        Ok(m) => m,
        Err(errors) => return Report::failure("lock", ExitCode::Error, errors),
    };
    let file = path(&m.root);
    let text = match std::fs::read_to_string(&file) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Report::ok(
                "lock",
                json!({ "path": FILE, "status": "missing", "fixed": false, "kept": 0,
                        "dropped": [], "had_markers": false }),
            )
            .with_headline(
                Tone::Ok,
                "No dots.lock yet; `dots apply` creates it.".to_string(),
            );
        }
        Err(e) => {
            let d = Diagnostic::from_code(
                "lock-invalid",
                &[("detail", &e.to_string()), ("file", FILE)],
            );
            return Report::failure("lock", ExitCode::Error, vec![d]);
        }
    };
    let broken = has_conflict_markers(&text) || Lock::parse(&text).is_err();
    // Rows for packages the repo no longer has go too, when the repo can be read.
    let known: Option<std::collections::BTreeSet<String>> = dots_model::Repo::load(&m.root)
        .ok()
        .map(|l| l.value.packages.keys().cloned().collect());
    let stale_in = |lock: &Lock| -> Vec<String> {
        known.as_ref().map_or(Vec::new(), |k| {
            let mut v: Vec<String> = lock
                .pkgs
                .iter()
                .filter(|r| !k.contains(&r.name))
                .map(|r| r.name.clone())
                .collect();
            v.sort();
            v.dedup();
            v
        })
    };
    if !broken {
        let mut lock = Lock::parse(&text).unwrap_or_default();
        let stale = stale_in(&lock);
        if stale.is_empty() {
            return Report::ok(
                "lock",
                json!({ "path": FILE, "status": "valid", "fixed": false, "kept": lock.pkgs.len(),
                        "dropped": [], "had_markers": false }),
            )
            .with_headline(
                Tone::Ok,
                "dots.lock is valid; nothing to repair.".to_string(),
            );
        }
        lock.pkgs.retain(|r| !stale.contains(&r.name));
        if let Err(d) = lock.save(&file) {
            return Report::failure("lock", ExitCode::Error, vec![d]);
        }
        return Report::ok(
            "lock",
            json!({ "path": FILE, "status": "pruned", "fixed": true, "kept": lock.pkgs.len(),
                    "dropped": stale, "had_markers": false }),
        )
        .with_headline(
            Tone::Ok,
            format!("Dropped stale dots.lock rows: {}", stale.join(", ")),
        );
    }
    let (mut lock, mut rep) = fix_text(&text);
    let stale = stale_in(&lock);
    if !stale.is_empty() {
        lock.pkgs.retain(|r| !stale.contains(&r.name));
        rep.dropped.extend(
            stale
                .iter()
                .map(|n| format!("stale row `{n}` (no such package)")),
        );
        rep.kept = lock.pkgs.len() + lock.upstream.len() + lock.content.len();
    }
    if let Err(d) = lock.save(&file) {
        return Report::failure("lock", ExitCode::Error, vec![d]);
    }
    let mut report = Report::ok(
        "lock",
        json!({
            "path": FILE,
            "status": "repaired",
            "fixed": true,
            "kept": rep.kept,
            "dropped": rep.dropped,
            "had_markers": rep.had_markers,
            "duplicates": rep.duplicates,
        }),
    )
    .with_headline(
        if rep.dropped.is_empty() {
            Tone::Ok
        } else {
            Tone::Warn
        },
        format!(
            "Repaired dots.lock: kept {} row{}{}",
            rep.kept,
            if rep.kept == 1 { "" } else { "s" },
            if rep.dropped.is_empty() {
                String::new()
            } else {
                format!(", dropped {}", rep.dropped.len())
            }
        ),
    );
    for d in &rep.dropped {
        report.push(dots_core::Block::text(format!("dropped {d}")));
    }
    report.push(dots_core::Block::hint(
        "dots apply  # re-resolves anything that was dropped",
    ));
    report
}
