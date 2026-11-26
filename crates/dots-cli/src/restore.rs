//! `dots restore <run-id>`: put back the files a run backed up, and the system settings it
//! changed (from the settings ledger).

use dots_core::{ExitCode, Report};
use dots_providers::{Cmd, Stdin};
use serde_json::json;

use crate::app::Context;

/// Restore the settings `run_id` changed. `Ok(None)`: the run changed none.
fn restore_settings(
    ctx: &Context,
    run_id: &str,
) -> Result<Option<dots_settings::Restored>, dots_core::Diagnostic> {
    let mut ledger = dots_settings::Ledger::load(&ctx.dirs.state);
    if ledger.run_changes(run_id).is_empty() {
        return Ok(None);
    }
    let pctx = crate::providers::exec_ctx(&ctx.dirs, ctx.opts.offline, false, run_id);
    let sys = crate::settings_phase::sys(&pctx, false);
    // A root setting needs the password; ask once, before touching anything. When that
    // fails (or dots is not allowed sudo) root settings are left alone and reported, they
    // never run as the user.
    let root = ledger.run_changes(run_id).iter().any(|c| c.setting.root());
    let sys = if root && !crate::settings_phase::running_as_root() {
        let cmd = Cmd::new(sys.tools.path("sudo"))
            .arg("-v")
            .stdin(Stdin::Inherit)
            .timeout(None);
        let ok = sys.runner.run(&cmd).is_ok_and(|o| o.success());
        dots_settings::Sys {
            sudo: crate::settings_phase::sudo_mode(ok),
            ..sys
        }
    } else {
        sys
    };
    dots_settings::restore_run(&sys, &mut ledger, run_id, false).map_err(|e| {
        dots_core::Diagnostic::from_code("setting-failed", &[("setting", run_id), ("detail", &e)])
    })
}

pub fn run(ctx: &Context, run_id: &str) -> Report {
    let files = dots_files::restore(&ctx.dirs.state, run_id, false);
    let settings = restore_settings(ctx, run_id);
    let (restored, skipped, files_found) = match files {
        Ok(r) => (
            r.restored.iter().map(|p| p.display().to_string()).collect(),
            r.skipped,
            true,
        ),
        Err(d) if d.code == "backup-not-found" => (Vec::<String>::new(), Vec::new(), false),
        Err(d) => return Report::failure("restore", ExitCode::Error, vec![d]),
    };
    let settings = match settings {
        Ok(s) => s,
        Err(d) => return Report::failure("restore", ExitCode::Error, vec![d]),
    };
    if !files_found && settings.is_none() {
        let d = dots_core::Diagnostic::from_code("backup-not-found", &[("run_id", run_id)]);
        return Report::failure("restore", ExitCode::Error, vec![d]);
    }
    let (set_restored, set_skipped) = settings
        .map(|s| (s.restored, s.skipped))
        .unwrap_or_default();
    let mut report = Report::ok(
        "restore",
        json!({
            "run_id": run_id,
            "restored": restored,
            "skipped": skipped.len() + set_skipped.len(),
            "settings": set_restored,
        }),
    );
    let mut lines: Vec<String> = restored.iter().map(|p| format!("restored {p}")).collect();
    lines.extend(set_restored.iter().map(|s| format!("restored setting {s}")));
    if lines.is_empty() {
        lines.push(format!("nothing restored from {run_id}"));
    }
    report.headline = Some(lines.join("\n"));
    let mut errors = skipped;
    errors.extend(set_skipped.iter().map(|(_, why)| {
        dots_core::Diagnostic::from_code(
            "restore-skipped",
            &[
                ("path", why.trim_end_matches(" changed after this run")),
                ("run_id", run_id),
            ],
        )
    }));
    if !errors.is_empty() {
        // Left-as-is files and settings mean the run was not fully undone.
        report.errors = errors;
        report.exit = ExitCode::Error;
    }
    report
}
