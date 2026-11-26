//! Report blocks for `plan` and `apply`: what the human renderers draw. The JSON data of both
//! commands comes from `dots_engine::plan_json` / `ApplyReport::to_json`, untouched.

use dots_core::blocks::plural;
use dots_core::{Block, Cell, Lead, Marker, Report, Row, Section, State, Table, Tone};
use dots_engine::{ApplyReport, Plan, Status, StepKind, StepStatus};

fn count_kind(plan: &Plan, k: StepKind) -> usize {
    plan.steps.iter().filter(|s| s.kind == k).count()
}

fn file_count(plan: &Plan, action: &str) -> usize {
    plan.files.iter().filter(|f| f.action == action).count()
}

/// `2 installs · 1 upgrade · 3 file changes`
pub fn plan_summary(plan: &Plan) -> String {
    let mut parts = Vec::new();
    for (k, one, many) in [
        (StepKind::Install, "install", "installs"),
        (StepKind::Upgrade, "upgrade", "upgrades"),
        (StepKind::Downgrade, "downgrade", "downgrades"),
    ] {
        let n = count_kind(plan, k);
        if n > 0 {
            parts.push(plural(n, one, many));
        }
    }
    let removals = file_count(plan, "remove");
    let changes = plan.files.len() - removals;
    if changes > 0 {
        parts.push(plural(changes, "file change", "file changes"));
    }
    if removals > 0 {
        parts.push(plural(removals, "file removal", "file removals"));
    }
    parts.join(" \u{b7} ")
}

fn pkg_marker(k: StepKind) -> Marker {
    match k {
        StepKind::Install => Marker::Add,
        StepKind::Upgrade => Marker::Upgrade,
        StepKind::Downgrade => Marker::Change,
    }
}

fn file_lead(action: &str) -> Lead {
    match action {
        "create" => Marker::Add.into(),
        "replace" => Marker::Replace.into(),
        "remove" => Marker::Remove.into(),
        "failed" => State::Failed.into(),
        _ => Marker::Change.into(),
    }
}

fn version_cell(from: &Option<String>, to: &Option<String>) -> Cell {
    match (from, to) {
        (Some(f), Some(t)) => Cell::version(f, t),
        (None, Some(t)) => Cell::new(t.clone(), dots_core::Style::Version),
        (Some(f), None) => Cell::new(format!("{f} \u{2192} latest"), dots_core::Style::Plain),
        (None, None) => Cell::muted("latest"),
    }
}

/// The plan body: packages, commands, files, risks, packages nobody can install.
pub fn plan_blocks(plan: &Plan, backup_dir: &str) -> Vec<Block> {
    let mut out = Vec::new();
    if !plan.steps.is_empty() {
        let mut t = Table::new()
            .styled_column("provider", 3, dots_core::Style::Muted)
            .column("version", 4)
            .styled_column("note", 2, dots_core::Style::Muted);
        for s in &plan.steps {
            let mut notes: Vec<&str> = Vec::new();
            if s.bootstrap {
                notes.push("bootstrap");
            }
            if s.needs_root {
                notes.push("needs sudo");
            }
            if s.interactive {
                notes.push("interactive, runs last");
            }
            t.push(
                Row::new(pkg_marker(s.kind), s.package.clone())
                    .cell(Cell::muted(s.provider.clone()))
                    .cell(version_cell(&s.from, &s.to))
                    .cell(Cell::muted(notes.join(", "))),
            );
        }
        out.push(Block::Section(Section::new("Packages").block(t)));
        let lines: Vec<String> = plan
            .batches
            .iter()
            .flat_map(|b| {
                b.actions.iter().map(move |a| {
                    let root = if a.needs_root { "sudo " } else { "" };
                    format!("[{}] {root}{}", b.id + 1, a.argv.join(" "))
                })
            })
            .collect();
        if !lines.is_empty() {
            out.push(Block::Section(
                Section::new("Commands").block(Block::Code { title: None, lines }),
            ));
        }
    }
    if !plan.files.is_empty() {
        let mut t = Table::new()
            .styled_column("mode", 4, dots_core::Style::Muted)
            .styled_column("detail", 3, dots_core::Style::Muted);
        for f in &plan.files {
            let mut row = Row::named(file_lead(&f.action), Cell::path(f.dst.clone()))
                .cell(Cell::muted(f.kind.clone()))
                .cell(Cell::muted(if f.owned {
                    format!("{} (refresh)", f.detail)
                } else {
                    f.detail.clone()
                }));
            if f.action == "failed" {
                row = row.hint(format!("dots lint  # then fix {}", f.package));
            }
            t.push(row);
        }
        out.push(Block::Section(Section::new("Files").block(t)));
    }

    // Heads up: risks come before any confirmation.
    let mut heads: Vec<Block> = Vec::new();
    if plan.needs_root() {
        heads.push(Block::callout("Needs your password once (sudo)"));
    }
    let backups = plan
        .files
        .iter()
        .filter(|f| f.detail.contains("backed up first"))
        .count();
    if backups > 0 {
        heads.push(Block::callout(format!(
            "{} will be backed up to {backup_dir} first",
            plural(backups, "existing file", "existing files")
        )));
    }
    for s in plan.steps.iter().filter(|s| s.interactive) {
        heads.push(Block::callout(format!(
            "{} is interactive and runs last",
            s.package
        )));
    }
    if !heads.is_empty() {
        let mut sec = Section::new("Heads up");
        for h in heads {
            sec = sec.block(h);
        }
        out.push(Block::Section(sec));
    }
    for (p, why) in &plan.unplanned {
        out.push(Block::text(format!("Not planned: {p} ({why})")));
    }
    out
}

/// `dots plan`.
pub fn plan_report(mut report: Report, plan: &Plan, backup_dir: &str) -> Report {
    if plan.is_empty() {
        report = report.with_headline(
            Tone::Ok,
            "Nothing to do: the machine already matches the repo.",
        );
        for (p, why) in &plan.unplanned {
            report.push(Block::text(format!("Not planned: {p} ({why})")));
        }
        return report;
    }
    report = report.with_headline(Tone::Info, format!("Plan   {}", plan_summary(plan)));
    for b in plan_blocks(plan, backup_dir) {
        report.push(b);
    }
    report.push(Block::hint("dots apply"));
    report
}

fn dur(ms: u64) -> String {
    if ms < 1000 {
        format!("{ms}ms")
    } else {
        format!("{:.1}s", ms as f64 / 1000.0)
    }
}

/// `dots apply`: the plan when nothing ran, else what happened.
pub fn apply_report(mut report: Report, r: &ApplyReport, backup_dir: &str) -> Report {
    let plan = &r.plan;
    match r.status {
        Status::PlanFailed => return report,
        Status::DryRun => {
            report = plan_report(report, plan, backup_dir);
            report.push(Block::text("Dry run: nothing was changed."));
            if !plan.is_empty() {
                report.blocks.retain(|b| !matches!(b, Block::Hint(_)));
                report.push(Block::hint("run without --dry-run to apply"));
            }
            return report;
        }
        Status::ConfirmationRequired | Status::Declined => {
            return plan_report(report, plan, backup_dir);
        }
        _ => {}
    }
    if r.steps.is_empty() && r.files.is_empty() && r.status == Status::Done {
        return report.with_headline(
            Tone::Ok,
            "Nothing to do: the machine already matches the repo.",
        );
    }

    let ok = r
        .steps
        .iter()
        .filter(|s| s.status == StepStatus::Ok)
        .count();
    let bad = r
        .steps
        .iter()
        .filter(|s| s.status == StepStatus::Failed)
        .count()
        + r.files.iter().filter(|f| !f.ok).count();
    let total = r.steps.len() + r.files.len();
    let done = ok + r.files.iter().filter(|f| f.ok).count();
    let (tone, text) = if r.status == Status::Interrupted {
        (
            Tone::Warn,
            format!("Interrupted   {done} of {total} done; state is saved"),
        )
    } else if bad > 0 {
        (
            Tone::Err,
            format!(
                "Done with {}   {done} of {total} succeeded",
                plural(bad, "problem", "problems")
            ),
        )
    } else {
        (Tone::Ok, format!("Applied   {done} of {total} succeeded"))
    };
    report = report.with_headline(tone, text);

    if !r.steps.is_empty() {
        let mut t = Table::new()
            .styled_column("provider", 3, dots_core::Style::Muted)
            .column("version", 4)
            .styled_column("time", 2, dots_core::Style::Muted);
        for s in &r.steps {
            let state = match &s.status {
                StepStatus::Ok => State::Done,
                StepStatus::Failed => State::Failed,
                StepStatus::SkippedDueToDep { .. } | StepStatus::NotRun => State::Skipped,
                StepStatus::Deferred => State::Pending,
            };
            let mut row = Row::new(state, s.package.clone())
                .cell(Cell::muted(s.provider.clone()))
                .cell(version_cell(&s.from, &s.to))
                .cell(Cell::muted(match &s.status {
                    StepStatus::SkippedDueToDep { dependency } => format!("needs {dependency}"),
                    StepStatus::Deferred => "interactive: run it in a terminal".into(),
                    StepStatus::NotRun => "not run".into(),
                    _ => dur(s.dur_ms),
                }));
            match &s.status {
                StepStatus::Failed => row = row.hint(format!("dots apply {}", s.package)),
                StepStatus::Deferred => row = row.hint(format!("dots apply {}", s.package)),
                _ => {}
            }
            t.push(row);
        }
        report.push(Block::Section(Section::new("Packages").block(t)));
    }
    if !r.files.is_empty() {
        let mut t = Table::new()
            .styled_column("mode", 3, dots_core::Style::Muted)
            .styled_column("detail", 2, dots_core::Style::Muted);
        for f in &r.files {
            let state = if f.ok { State::Done } else { State::Failed };
            let detail = f.detail.clone().unwrap_or_else(|| f.step.detail.clone());
            t.push(
                Row::named(state, Cell::path(f.step.dst.clone()))
                    .cell(Cell::muted(f.step.kind.clone()))
                    .cell(Cell::muted(detail)),
            );
        }
        report.push(Block::Section(Section::new("Files").block(t)));
    }
    for s in r.steps.iter().filter(|s| s.status == StepStatus::Failed) {
        if let Some(e) = &s.error {
            report.push(Block::Code {
                title: Some(format!("{} failed", s.package)),
                lines: e.lines().map(str::to_string).collect(),
            });
        }
    }
    if !r.files_skipped.is_empty() {
        report.push(Block::text(format!(
            "{} left alone: their package did not install",
            plural(r.files_skipped.len(), "file", "files")
        )));
    }
    if !r.todos.is_empty() {
        report.push(Block::hint("dots todo"));
    }
    if r.status == Status::Interrupted {
        report.push(Block::hint("dots apply"));
    }
    report
}
