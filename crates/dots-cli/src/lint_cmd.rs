//! `dots lint`: check the repo without changing anything.

use dots_core::report::Diagnostic;
use dots_core::{ExitCode, Report, Tone};
use dots_resolve::{LintInputs, lint};
use serde_json::json;

use crate::app::Context;
use crate::context::load_repo;

pub fn run(ctx: &Context) -> Report {
    let (errors, warnings, packages, bundles) = match load_repo(ctx, None) {
        Err((errors, warnings)) => (errors, warnings, 0, 0),
        Ok(rc) => {
            let mut rep = lint(&LintInputs {
                repo: &rc.repo,
                machine: Some(&rc.m.machine),
                chain: &rc.chain,
            });
            let mut warnings = rc.m.warnings.clone();
            warnings.append(&mut rep.warnings);
            (
                rep.errors,
                warnings,
                rc.repo.packages.len(),
                rc.repo.bundles.len(),
            )
        }
    };
    finish(errors, warnings, packages, bundles)
}

fn finish(
    errors: Vec<Diagnostic>,
    warnings: Vec<Diagnostic>,
    packages: usize,
    bundles: usize,
) -> Report {
    let plural = |n: usize, w: &str| format!("{n} {w}{}", if n == 1 { "" } else { "s" });
    let headline = if errors.is_empty() && warnings.is_empty() {
        format!(
            "lint: ok ({}, {} checked)",
            plural(packages, "package"),
            plural(bundles, "bundle")
        )
    } else {
        format!(
            "lint: {}, {}",
            plural(errors.len(), "error"),
            plural(warnings.len(), "warning")
        )
    };
    let mut report = Report::ok(
        "lint",
        json!({
            "packages": packages,
            "bundles": bundles,
            "errors": errors.len(),
            "warnings": warnings.len(),
        }),
    );
    let tone = if !errors.is_empty() {
        Tone::Err
    } else if !warnings.is_empty() {
        Tone::Warn
    } else {
        Tone::Ok
    };
    report = report.with_headline(tone, headline);
    report.warnings = warnings;
    if !errors.is_empty() {
        report.errors = errors;
        report.exit = ExitCode::Error;
    }
    report
}
