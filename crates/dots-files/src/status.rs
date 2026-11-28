//! Desired-state computation and drift observation.

use std::fs;
use std::path::Path;

use dots_core::report::Diagnostic;

use crate::fsops::{LinkKind, lexists, sha256_hex, sha256_path};
use crate::ledger::{Ledger, Record};
use crate::model::{
    Action, BackupKind, FileState, FileStatus, FileStep, FilesCtx, Mode, Resolution, ResolvedFile,
};
use crate::render::render_layers;

/// Marker lines of an inject block.
pub fn block_markers(id: &str, comment: &str) -> (String, String) {
    (
        format!("{comment} >>> dots:{id} >>>"),
        format!("{comment} <<< dots:{id} <<<"),
    )
}

/// The body of the managed block, if present.
pub fn extract_block(text: &str, id: &str, comment: &str) -> Option<String> {
    let (open, close) = block_markers(id, comment);
    let lines: Vec<&str> = text.lines().collect();
    let s = lines
        .iter()
        .position(|l| l.trim_start().starts_with(&open))?;
    let e = lines[s + 1..].iter().position(|l| l.trim() == close)? + s + 1;
    Some(lines[s + 1..e].join("\n"))
}

fn gen_path(file: &ResolvedFile, ctx: &FilesCtx) -> std::path::PathBuf {
    let rel = file
        .dst
        .strip_prefix(&ctx.home)
        .map(Path::to_path_buf)
        .unwrap_or_else(|_| {
            file.dst
                .components()
                .filter(|c| matches!(c, std::path::Component::Normal(_)))
                .collect()
        });
    ctx.gen_dir().join(&file.package).join(rel)
}

/// Compute the desired state of one file. On failure the step carries `error`.
pub fn build_step(file: &ResolvedFile, ctx: &FilesCtx) -> FileStep {
    let mut step = FileStep {
        package: file.package.clone(),
        dst: file.dst.clone(),
        mode: file.mode,
        action: Action::Unchanged,
        from: None,
        backup: BackupKind::None,
        link_target: None,
        gen_path: None,
        src: file.src.clone(),
        sha256: None,
        layers: file.layers.iter().map(|l| l.label.clone()).collect(),
        marker: None,
        comment: None,
        error: None,
        content: None,
    };
    let fail = |step: &mut FileStep, d: Diagnostic| {
        step.action = Action::Failed;
        step.error = Some(d);
    };
    let read = |p: &Path| {
        fs::read(p).map_err(|e| {
            Diagnostic::from_code(
                "file-unreadable",
                &[
                    ("file", &p.display().to_string()),
                    ("detail", &e.to_string()),
                ],
            )
        })
    };
    match file.mode {
        Mode::Dir => {}
        Mode::Link => {
            let src = file.src.clone().unwrap_or_default();
            step.link_target = Some(match src.strip_prefix(&ctx.root) {
                Ok(rel) => ctx.active_dir().join(rel),
                Err(_) => src,
            });
        }
        Mode::Copy => match read(file.src.as_deref().unwrap_or(Path::new(""))) {
            Ok(b) => {
                step.sha256 = Some(sha256_hex(&b));
                step.content = Some(b);
            }
            Err(d) => fail(&mut step, d),
        },
        Mode::Render => match render_layers(file, ctx) {
            Ok(text) => {
                let g = gen_path(file, ctx);
                step.sha256 = Some(sha256_hex(text.as_bytes()));
                step.content = Some(text.into_bytes());
                step.link_target = Some(g.clone());
                step.gen_path = Some(g);
            }
            Err(d) => fail(&mut step, d),
        },
        Mode::Terminfo => match read(file.src.as_deref().unwrap_or(Path::new(""))) {
            Ok(b) => step.sha256 = Some(sha256_hex(&b)),
            Err(d) => fail(&mut step, d),
        },
        Mode::Inject => match read(file.src.as_deref().unwrap_or(Path::new(""))) {
            Ok(b) => {
                let body = String::from_utf8_lossy(&b)
                    .trim_end_matches('\n')
                    .to_string();
                step.sha256 = Some(sha256_hex(body.as_bytes()));
                step.content = Some(body.into_bytes());
                step.marker = Some(file.package.clone());
                step.comment = Some(file.comment.clone());
            }
            Err(d) => fail(&mut step, d),
        },
    }
    step
}

/// State of a destination that is a hardlink of `target` (see `LinkKind::Hardlink`).
fn observe_hardlink(step: &FileStep, rec: &Record, target: &Path) -> FileState {
    let Some(h) = sha256_path(&step.dst) else {
        return FileState::Dangling;
    };
    if step.mode == Mode::Link {
        return if crate::fsops::same_content(&step.dst, target) {
            FileState::Ok
        } else {
            FileState::Modified
        };
    }
    match &rec.sha256 {
        Some(r) if *r != h => FileState::Modified,
        Some(r) if Some(r) != step.sha256.as_ref() => FileState::Stale,
        // The rendered file was rewritten (same content or not): the hardlink must be remade
        // to follow it.
        _ if !crate::fsops::same_content(&step.dst, target) => FileState::Stale,
        _ => FileState::Ok,
    }
}

/// Observe the on-disk state of a step's destination.
pub fn observe(step: &FileStep, rec: Option<&Record>, ctx: &FilesCtx) -> FileState {
    let dst = &step.dst;
    match step.mode {
        Mode::Terminfo => {
            let name = dst.file_name().map(|n| n.to_string_lossy().into_owned());
            let (Some(name), Some(dir), Some(src)) = (name, dst.parent(), step.src.as_deref())
            else {
                return FileState::Missing;
            };
            let scratch = ctx
                .state
                .join(format!("terminfo-check-{}", std::process::id()));
            crate::terminfo::observe(ctx.tools.as_ref(), dir, &name, src, &scratch)
        }
        Mode::Dir => {
            if dst.is_dir() {
                FileState::Ok
            } else if lexists(dst) {
                FileState::Conflict
            } else {
                FileState::Missing
            }
        }
        Mode::Inject => {
            let id = step.marker.as_deref().unwrap_or("");
            let c = step.comment.as_deref().unwrap_or("#");
            let Some(block) = fs::read_to_string(dst)
                .ok()
                .and_then(|t| extract_block(&t, id, c))
            else {
                return FileState::Missing;
            };
            let h = sha256_hex(block.as_bytes());
            if Some(&h) == step.sha256.as_ref() {
                FileState::Ok
            } else if rec.is_none_or(|r| r.sha256.as_ref() == Some(&h)) {
                FileState::Stale
            } else {
                FileState::Modified
            }
        }
        Mode::Link | Mode::Render => {
            let Ok(meta) = fs::symlink_metadata(dst) else {
                return FileState::Missing;
            };
            let target = step.link_target.as_deref().unwrap_or(Path::new(""));
            if meta.file_type().is_symlink() {
                if fs::read_link(dst).ok().as_deref() != Some(target) {
                    return FileState::Conflict;
                }
                if step.mode == Mode::Link {
                    return if dst.exists() {
                        FileState::Ok
                    } else {
                        FileState::Dangling
                    };
                }
                let Some(h) = sha256_path(dst) else {
                    return FileState::Dangling;
                };
                match rec {
                    Some(r) if r.sha256.as_ref() != Some(&h) => FileState::Modified,
                    Some(r) if r.sha256 != step.sha256 => FileState::Stale,
                    Some(_) => FileState::Ok,
                    None if Some(&h) == step.sha256.as_ref() => FileState::Ok,
                    None => FileState::Stale,
                }
            } else if let Some(r) =
                rec.filter(|r| r.link_kind == LinkKind::Hardlink && meta.is_file())
            {
                // A hardlink made where symlinks are refused: it is the link while its
                // content is the target's. (A write by rename leaves it on the old file.)
                observe_hardlink(step, r, target)
            } else if meta.is_dir() || rec.is_none() {
                FileState::Conflict
            } else {
                FileState::Modified
            }
        }
        Mode::Copy => {
            let Ok(meta) = fs::symlink_metadata(dst) else {
                return FileState::Missing;
            };
            if !meta.is_file() {
                return FileState::Conflict;
            }
            let h = sha256_path(dst);
            match rec {
                None if h == step.sha256 => FileState::Ok,
                None => FileState::Conflict,
                Some(r) if r.sha256 != h => FileState::Modified,
                Some(r) if r.sha256 != step.sha256 => FileState::Stale,
                Some(_) => FileState::Ok,
            }
        }
    }
}

fn resolutions(step: &FileStep, state: FileState) -> (Option<String>, Vec<Resolution>) {
    let dst = step.dst.display();
    let res = |id: &str, command: String, description: &str| Resolution {
        id: id.into(),
        command,
        description: description.into(),
    };
    let adoptable = matches!(step.mode, Mode::Render | Mode::Copy);
    match state {
        FileState::Modified if adoptable => (
            Some(format!("run `dots adopt {dst}`")),
            vec![
                res(
                    "adopt",
                    format!("dots adopt {dst}"),
                    "write the app's changes back into a layer",
                ),
                res(
                    "adopt-machine",
                    format!("dots adopt {dst} --machine"),
                    "keep the changes for this machine only",
                ),
                res(
                    "overwrite",
                    "dots apply".into(),
                    "regenerate the file; your version is backed up first",
                ),
            ],
        ),
        FileState::Modified => (
            Some("replaced by a regular file; run `dots apply` to relink".into()),
            vec![res(
                "relink",
                "dots apply".into(),
                "relink; the current file is backed up first",
            )],
        ),
        FileState::Conflict => (
            Some("exists and is not managed by dots; `dots apply` backs it up first".into()),
            vec![res(
                "replace",
                "dots apply".into(),
                "back it up to backup/<run-id>/ and replace it",
            )],
        ),
        FileState::Stale => (Some("a source changed; run `dots apply`".into()), vec![]),
        FileState::Dangling => (
            Some("the link target is gone; run `dots roots` or `dots use`".into()),
            vec![],
        ),
        _ => (None, vec![]),
    }
}

/// Drift status of every file.
pub fn status(files: &[ResolvedFile], ctx: &FilesCtx) -> Vec<FileStatus> {
    let ledger = Ledger::load(&ctx.state);
    files
        .iter()
        .map(|f| {
            let step = build_step(f, ctx);
            let rec = ledger.get(&f.dst, f.mode, &f.package);
            if step.error.is_some() {
                return FileStatus {
                    package: step.package,
                    dst: step.dst,
                    mode: step.mode,
                    state: FileState::Stale,
                    hint: Some("the source cannot be rendered".into()),
                    resolutions: vec![],
                    error: step.error,
                };
            }
            let state = observe(&step, rec, ctx);
            let (hint, resolutions) = resolutions(&step, state);
            FileStatus {
                package: step.package,
                dst: step.dst,
                mode: step.mode,
                state,
                hint,
                resolutions,
                error: None,
            }
        })
        .collect()
}
