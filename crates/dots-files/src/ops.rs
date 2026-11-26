//! `plan`, `apply` and removals.

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use dots_core::report::Diagnostic;

use crate::fsops::{atomic_symlink, atomic_write, ensure_parents, lexists, remove_any, sha256_hex};
use crate::ledger::{Ledger, Record, key};
use crate::model::{
    Action, BackupKind, FileOutcome, FileState, FileStep, FilesCtx, Mode, OutcomeStatus,
    ResolvedFile,
};
use crate::restore::back_up;
use crate::status::{block_markers, build_step, extract_block, observe};

/// Plan every file. Nothing is written.
pub fn plan(files: &[ResolvedFile], ctx: &FilesCtx) -> Vec<FileStep> {
    let ledger = Ledger::load(&ctx.state);
    files
        .iter()
        .map(|f| {
            let mut s = build_step(f, ctx);
            if s.error.is_some() {
                return s;
            }
            let rec = ledger.get(&f.dst, f.mode, &f.package);
            let state = observe(&s, rec, ctx);
            s.from = Some(state);
            let through_link =
                fs::symlink_metadata(&s.dst).is_ok_and(|m| m.file_type().is_symlink());
            (s.action, s.backup) = match state {
                FileState::Ok if rec.is_none() => (Action::Update, BackupKind::None),
                FileState::Ok | FileState::ForeignEntries => (Action::Unchanged, BackupKind::None),
                FileState::Missing => (Action::Create, BackupKind::None),
                FileState::Conflict => (Action::Replace, BackupKind::Move),
                FileState::Modified if s.mode == Mode::Inject => (Action::Update, BackupKind::None),
                FileState::Modified if through_link => (Action::Update, BackupKind::Copy),
                FileState::Modified => (Action::Update, BackupKind::Move),
                FileState::Stale | FileState::Dangling => (Action::Update, BackupKind::None),
            };
            s
        })
        .collect()
}

/// Steps removing owned paths that are no longer desired. `scope` limits them to packages.
pub fn plan_removals(
    desired: &[FileStep],
    ctx: &FilesCtx,
    scope: Option<&[String]>,
) -> Vec<FileStep> {
    let keep: BTreeSet<String> = desired
        .iter()
        .map(|s| key(&s.dst, s.mode, &s.package))
        .collect();
    Ledger::load(&ctx.state)
        .owned
        .into_iter()
        .filter(|(k, r)| !keep.contains(k) && scope.is_none_or(|s| s.contains(&r.package)))
        .map(|(_, r)| FileStep {
            package: r.package,
            dst: r.dst,
            mode: r.mode,
            action: Action::Remove,
            from: None,
            backup: BackupKind::None,
            link_target: r.link_target,
            gen_path: r.gen_path,
            src: None,
            sha256: r.sha256,
            layers: vec![],
            marker: r.marker,
            comment: r.comment,
            error: None,
            content: None,
        })
        .collect()
}

fn outcome(step: &FileStep, status: OutcomeStatus) -> FileOutcome {
    FileOutcome {
        dst: step.dst.clone(),
        status,
        backup: None,
        error: None,
    }
}

fn failed(step: &FileStep, d: Diagnostic) -> FileOutcome {
    FileOutcome {
        error: Some(d),
        ..outcome(step, OutcomeStatus::Failed)
    }
}

fn write_failed(path: &Path, e: &std::io::Error) -> Diagnostic {
    Diagnostic::from_code(
        "write-failed",
        &[
            ("path", &path.display().to_string()),
            ("detail", &e.to_string()),
        ],
    )
}

/// Perform one step. With `ctx.dry_run` nothing is touched.
pub fn apply(step: &FileStep, ctx: &FilesCtx) -> FileOutcome {
    if let Some(d) = &step.error {
        return failed(step, d.clone());
    }
    if step.action == Action::Unchanged {
        return outcome(step, OutcomeStatus::Unchanged);
    }
    if ctx.dry_run {
        return outcome(step, OutcomeStatus::DryRun);
    }
    match step.action {
        Action::Remove => remove(step, ctx),
        Action::Failed | Action::Unchanged => outcome(step, OutcomeStatus::Unchanged),
        _ => write(step, ctx),
    }
}

fn write(step: &FileStep, ctx: &FilesCtx) -> FileOutcome {
    let mut ledger = Ledger::load(&ctx.state);
    let k = key(&step.dst, step.mode, &step.package);
    let existing = ledger.owned.get(&k).cloned();
    let mut out = outcome(step, OutcomeStatus::Applied);

    let mut created_parents = match ensure_parents(&step.dst) {
        Ok(v) => v,
        Err(e) => return failed(step, write_failed(&step.dst, &e)),
    };
    if let Some(r) = &existing {
        for p in &r.created_parents {
            if !created_parents.contains(p) {
                created_parents.insert(0, p.clone());
            }
        }
    }

    if step.backup != BackupKind::None && lexists(&step.dst) {
        match back_up(
            &ctx.state,
            &ctx.run_id,
            &step.dst,
            step.backup,
            step.link_target.as_deref(),
            step.sha256.as_deref(),
        ) {
            Ok(p) => out.backup = Some(p),
            Err(e) => return failed(step, write_failed(&step.dst, &e)),
        }
    }

    let mut created = existing.as_ref().is_some_and(|r| r.created);
    let res: std::io::Result<()> = (|| match step.mode {
        Mode::Dir => {
            created |= !step.dst.is_dir();
            fs::create_dir_all(&step.dst)
        }
        Mode::Link => link(step, ctx),
        Mode::Copy => atomic_write(&step.dst, step.content.as_deref().unwrap_or_default()),
        Mode::Render => {
            let g = step.gen_path.as_deref().unwrap_or(&step.dst);
            ensure_parents(g)?;
            atomic_write(g, step.content.as_deref().unwrap_or_default())?;
            link(step, ctx)
        }
        Mode::Terminfo => {
            let dir = step.dst.parent().unwrap_or(Path::new("."));
            let src = step.src.as_deref().unwrap_or(Path::new(""));
            crate::terminfo::compile(ctx.tools.as_ref(), dir, src).map_err(std::io::Error::other)
        }
        Mode::Inject => {
            let real = fs::canonicalize(&step.dst).unwrap_or_else(|_| step.dst.clone());
            let old = fs::read_to_string(&real);
            created |= old.is_err();
            let new = replace_block(
                old.as_deref().unwrap_or(""),
                step.marker.as_deref().unwrap_or(""),
                step.comment.as_deref().unwrap_or("#"),
                &String::from_utf8_lossy(step.content.as_deref().unwrap_or_default()),
            );
            atomic_write(&real, new.as_bytes())
        }
    })();
    if let Err(e) = res {
        return failed(step, write_failed(&step.dst, &e));
    }

    ledger.owned.insert(
        k,
        Record {
            dst: step.dst.clone(),
            package: step.package.clone(),
            mode: step.mode,
            root_id: ctx.root_id.clone(),
            run_id: ctx.run_id.clone(),
            sha256: step.sha256.clone(),
            link_target: step.link_target.clone(),
            gen_path: step.gen_path.clone(),
            created_parents,
            created,
            marker: step.marker.clone(),
            comment: step.comment.clone(),
        },
    );
    if let Err(e) = ledger.save(&ctx.state) {
        return failed(step, write_failed(&ctx.state.join("files.json"), &e));
    }
    out
}

fn link(step: &FileStep, ctx: &FilesCtx) -> std::io::Result<()> {
    let target = step.link_target.as_deref().unwrap_or(Path::new(""));
    // Fresh sandboxes have no `active` yet: point it at the checkout, once.
    if target.starts_with(ctx.active_dir()) && !lexists(&ctx.active_dir()) {
        fs::create_dir_all(&ctx.state)?;
        atomic_symlink(&ctx.root, &ctx.active_dir())?;
    }
    if fs::read_link(&step.dst).ok().as_deref() == Some(target) {
        return Ok(());
    }
    atomic_symlink(target, &step.dst)
}

/// Insert or replace the managed block in `text`.
pub fn replace_block(text: &str, id: &str, comment: &str, body: &str) -> String {
    let (open, close) = block_markers(id, comment);
    let block = format!("{open} managed by dots, do not edit\n{body}\n{close}\n");
    let lines: Vec<&str> = text.split_inclusive('\n').collect();
    let start = lines.iter().position(|l| l.trim_start().starts_with(&open));
    let end = start.and_then(|s| {
        lines[s + 1..]
            .iter()
            .position(|l| l.trim() == close)
            .map(|e| e + s + 1)
    });
    match (start, end) {
        (Some(s), Some(e)) => {
            let mut out: String = lines[..s].concat();
            out.push_str(&block);
            out.push_str(&lines[e + 1..].concat());
            out
        }
        _ => {
            let mut out = text.to_string();
            if !out.is_empty() && !out.ends_with('\n') {
                out.push('\n');
            }
            out.push_str(&block);
            out
        }
    }
}

fn strip_block(text: &str, id: &str, comment: &str) -> String {
    let (open, close) = block_markers(id, comment);
    let lines: Vec<&str> = text.split_inclusive('\n').collect();
    let start = lines.iter().position(|l| l.trim_start().starts_with(&open));
    let end = start.and_then(|s| {
        lines[s + 1..]
            .iter()
            .position(|l| l.trim() == close)
            .map(|e| e + s + 1)
    });
    match (start, end) {
        (Some(s), Some(e)) => format!("{}{}", lines[..s].concat(), lines[e + 1..].concat()),
        _ => text.to_string(),
    }
}

fn remove(step: &FileStep, ctx: &FilesCtx) -> FileOutcome {
    let mut ledger = Ledger::load(&ctx.state);
    let k = key(&step.dst, step.mode, &step.package);
    let Some(rec) = ledger.owned.get(&k).cloned() else {
        // dots owns nothing here, so it touches nothing.
        return outcome(step, OutcomeStatus::Skipped);
    };
    let dst = &step.dst;
    let res: Result<bool, std::io::Error> = (|| match rec.mode {
        Mode::Inject => {
            let real = fs::canonicalize(dst).unwrap_or_else(|_| dst.clone());
            let Ok(text) = fs::read_to_string(&real) else {
                return Ok(true);
            };
            let id = rec.marker.as_deref().unwrap_or("");
            let c = rec.comment.as_deref().unwrap_or("#");
            if extract_block(&text, id, c).is_none() {
                return Ok(true);
            }
            let new = strip_block(&text, id, c);
            if rec.created && new.trim().is_empty() {
                fs::remove_file(&real)?;
            } else {
                atomic_write(&real, new.as_bytes())?;
            }
            Ok(true)
        }
        Mode::Terminfo => {
            if let (Some(dir), Some(name)) = (dst.parent(), dst.file_name()) {
                crate::terminfo::remove_compiled(dir, &name.to_string_lossy())?;
            }
            Ok(true)
        }
        Mode::Dir => {
            if rec.created && dst.is_dir() && fs::read_dir(dst)?.next().is_none() {
                fs::remove_dir(dst)?;
            }
            Ok(true)
        }
        Mode::Link | Mode::Render => {
            if fs::symlink_metadata(dst).is_ok_and(|m| m.file_type().is_symlink()) {
                fs::remove_file(dst)?;
            } else if lexists(dst) {
                return Ok(false); // replaced by something that is not ours
            }
            if let Some(g) = &rec.gen_path {
                remove_any(g)?;
            }
            Ok(true)
        }
        Mode::Copy => {
            if !lexists(dst) {
                return Ok(true);
            }
            let same = fs::read(dst).ok().map(|b| sha256_hex(&b)) == rec.sha256;
            if same {
                fs::remove_file(dst)?;
            }
            Ok(same)
        }
    })();
    match res {
        Err(e) => failed(step, write_failed(dst, &e)),
        Ok(false) => outcome(step, OutcomeStatus::Skipped),
        Ok(true) => {
            for p in rec.created_parents.iter().rev() {
                if fs::read_dir(p).is_ok_and(|mut d| d.next().is_none()) {
                    let _ = fs::remove_dir(p);
                }
            }
            ledger.owned.remove(&k);
            match ledger.save(&ctx.state) {
                Ok(()) => outcome(step, OutcomeStatus::Applied),
                Err(e) => failed(step, write_failed(&ctx.state, &e)),
            }
        }
    }
}
