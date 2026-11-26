//! `dots adopt <dst> [--layer <layer>] [--as-patch] [--machine]`: bring an edit made to a
//! managed file back into the repo (ARCHITECTURE 6.3). The merge itself lives in
//! `dots_files::adopt`; this is the lookup of the file, the target and the report.
//! System settings go through `settings_phase::adopt` before this runs.

use std::path::{Path, PathBuf};

use dots_core::{Diagnostic, ExitCode, Report};
use dots_files::{AdoptOpts, Mode};
use serde_json::json;

use crate::app::Context;
use crate::files_phase::RepoFiles;

pub struct Args<'a> {
    pub dst: &'a str,
    pub layer: Option<&'a str>,
    pub as_patch: bool,
    pub machine: bool,
}

fn fail(d: Diagnostic) -> Report {
    Report::failure("adopt", ExitCode::Error, vec![d])
}

/// The run lock, as `apply` and `migrate` take it: `lock-busy` (exit 75) while another run is on.
pub fn lock(ctx: &Context) -> Result<dots_engine::RunLock, Box<Report>> {
    let _ = ctx.dirs.ensure_all();
    dots_engine::RunLock::acquire(&ctx.dirs.run_lock()).map_err(|d| {
        let exit = if d.code == "lock-busy" {
            ExitCode::LockBusy
        } else {
            ExitCode::Error
        };
        Box::new(Report::failure("adopt", exit, vec![d]))
    })
}

fn usage(detail: &str) -> Report {
    let d = Diagnostic::from_code(
        "usage-error",
        &[("detail", detail), ("help", "dots adopt --help")],
    );
    Report::failure("adopt", ExitCode::Usage, vec![d])
}

/// `~/x` and relative paths made absolute (lexically).
fn absolute(arg: &str, home: &Path) -> PathBuf {
    let p = match arg {
        "~" => home.to_path_buf(),
        a => match a.strip_prefix("~/") {
            Some(rest) => home.join(rest),
            None => PathBuf::from(a),
        },
    };
    if p.is_absolute() {
        p
    } else {
        std::env::current_dir().map_or(p.clone(), |c| c.join(p))
    }
}

/// The same file, ignoring symlinked parents (`/var` vs `/private/var`).
fn same_path(a: &Path, b: &Path) -> bool {
    if a == b {
        return true;
    }
    let canon = |p: &Path| {
        let parent = p.parent().and_then(|d| std::fs::canonicalize(d).ok())?;
        Some(parent.join(p.file_name()?))
    };
    matches!((canon(a), canon(b)), (Some(x), Some(y)) if x == y)
}

pub fn run(ctx: &Context, a: &Args) -> Report {
    if a.as_patch {
        let d = Diagnostic::from_code(
            "not-implemented",
            &[(
                "command",
                "adopt --as-patch (patch layers are not supported yet)",
            )],
        );
        return Report::failure("adopt", ExitCode::Error, vec![d]);
    }
    if a.machine && a.layer.is_some() {
        return usage("`--machine` and `--layer` are alternatives; pass one");
    }
    let l = match crate::inspect::load(ctx, "adopt") {
        Ok(l) => l,
        Err(r) => return *r,
    };
    let dirs = &ctx.dirs;
    let rf = RepoFiles {
        dirs,
        rc: &l.rc,
        resolution: &l.resolution,
    };
    let fctx = rf.base_ctx(&dirs.home, &dirs.state, &dots_engine::new_run_id());
    let packages: Vec<String> = l.resolution.active().map(|r| r.name.clone()).collect();
    let resolved = rf.resolved_files(&packages, &fctx);
    let want = absolute(a.dst, &dirs.home);
    let mut hits: Vec<&dots_files::ResolvedFile> = resolved
        .files
        .iter()
        .filter(|f| same_path(&f.dst, &want))
        .collect();
    hits.sort_by_key(|f| f.mode == Mode::Link);
    let Some(file) = hits.first().copied() else {
        let d = Diagnostic::from_code("adopt-not-managed", &[("dst", a.dst)]);
        return fail(d);
    };
    let machine_path = dirs
        .config
        .join("overrides")
        .join(dots_files::override_layer_rel(
            &file.package,
            &file.dst,
            &dirs.home,
        ));
    let opts = AdoptOpts {
        layer: a.layer,
        machine_layer: a.machine.then_some(machine_path.as_path()),
    };
    let done = match dots_files::adopt(file, &fctx, &opts) {
        Ok(d) => d,
        Err(e) => return fail(e),
    };
    let dst = file.dst.display().to_string();
    let mode = match file.mode {
        Mode::Copy => "copy",
        Mode::Inject => "inject",
        _ => "render",
    };
    let rel = |p: &Path| match (p.strip_prefix(&l.rc.m.root), p.strip_prefix(&dirs.home)) {
        (Ok(r), _) => r.display().to_string(),
        (_, Ok(r)) => format!("~/{}", r.display()),
        _ => p.display().to_string(),
    };
    let mut r = Report::ok(
        "adopt",
        json!({
            "dst": dst,
            "package": file.package,
            "mode": mode,
            "changed": done.changed,
            "method": done.method,
            "target": done.target.as_ref().map(|t| t.display().to_string()),
            "layer": done.layer,
            "created": done.created,
            "keys": done.keys,
            "notes": done.notes,
        }),
    );
    let mut lines = vec![match &done.target {
        Some(t) if done.created => format!("{dst}: created {} with your changes", rel(t)),
        Some(t) => format!("{dst}: your changes are now in {}", rel(t)),
        None => format!("{dst} already matches its source; nothing to adopt"),
    }];
    if !done.keys.is_empty() {
        lines.push(format!("keys: {}", done.keys.join(", ")));
    }
    lines.extend(done.notes.iter().cloned());
    r.headline = Some(lines.join("\n"));
    r
}
