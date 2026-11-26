//! `dots restore <run-id>`: put back the files a run backed up.

use dots_core::{ExitCode, Report};
use serde_json::json;

use crate::app::Context;

pub fn run(ctx: &Context, run_id: &str) -> Report {
    match dots_files::restore(&ctx.dirs.state, run_id, false) {
        Err(d) => Report::failure("restore", ExitCode::Error, vec![d]),
        Ok(r) => {
            let restored: Vec<String> =
                r.restored.iter().map(|p| p.display().to_string()).collect();
            let mut report = Report::ok(
                "restore",
                json!({ "run_id": run_id, "restored": restored, "skipped": r.skipped.len() }),
            );
            let mut lines: Vec<String> = restored.iter().map(|p| format!("restored {p}")).collect();
            if lines.is_empty() {
                lines.push(format!("nothing restored from {run_id}"));
            }
            report.headline = Some(lines.join("\n"));
            if !r.skipped.is_empty() {
                // Left-as-is files mean the run was not fully undone.
                report.errors = r.skipped;
                report.exit = ExitCode::Error;
            }
            report
        }
    }
}
