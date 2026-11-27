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
    if !plan.settings.is_empty() {
        parts.push(plural(
            plan.settings.len(),
            "setting change",
            "setting changes",
        ));
    }
    if !plan.services.is_empty() {
        parts.push(plural(
            plan.services.len(),
            "service action",
            "service actions",
        ));
    }
    if !plan.shims.is_empty() {
        parts.push(plural(plan.shims.len(), "shim", "shims"));
    }
    if !plan.hooks.is_empty() {
        parts.push(plural(plan.hooks.len(), "hook", "hooks"));
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
            if let Some(n) = &s.note {
                notes.push(n);
            }
            if s.interactive {
                notes.push("interactive, runs last");
            }
            let (marker, provider, switch_note) = match s.switch.as_ref().filter(|w| w.has_copy()) {
                Some(w) => (
                    Marker::Change,
                    format!("{} -> {}", w.provider, s.provider),
                    Some(match &w.keep {
                        Some(why) => format!("keeps the old {} copy: {why}", w.provider),
                        None => format!("then removes the old {} copy", w.provider),
                    }),
                ),
                None => (pkg_marker(s.kind), s.provider.clone(), None),
            };
            if let Some(n) = &switch_note {
                notes.push(n);
            }
            t.push(
                Row::new(marker, s.package.clone())
                    .cell(Cell::muted(provider))
                    .cell(version_cell(&s.from, &s.to))
                    .cell(Cell::muted(notes.join(", "))),
            );
        }
        out.push(Block::Section(Section::new("Packages").block(t)));
        let mut lines: Vec<String> = plan
            .batches
            .iter()
            .flat_map(|b| {
                b.actions.iter().map(move |a| {
                    let root = if a.needs_root { "sudo " } else { "" };
                    format!("[{}] {root}{}", b.id + 1, a.argv.join(" "))
                })
            })
            .collect();
        for w in plan.steps.iter().filter_map(|s| s.switch.as_ref()) {
            for a in &w.remove {
                let root = if a.needs_root { "sudo " } else { "" };
                lines.push(format!("[then] {root}{}", a.argv.join(" ")));
            }
        }
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
                // `message → command`: a blocked render names the command that unblocks it.
                row = match f.detail.split_once(" \u{2192} ") {
                    Some((_, cmd)) => row.hint(cmd.to_string()),
                    None => row.hint(format!("dots lint  # then fix {}", f.package)),
                };
            }
            t.push(row);
        }
        out.push(Block::Section(Section::new("Files").block(t)));
    }

    if !plan.settings.is_empty() {
        let mut t = Table::new()
            .styled_column("kind", 3, dots_core::Style::Muted)
            .styled_column("change", 4, dots_core::Style::Muted);
        for s in &plan.settings {
            let lead: Lead = match s.action.as_str() {
                "restore" => Marker::Replace.into(),
                "delete" => Marker::Remove.into(),
                _ => Marker::Change.into(),
            };
            let note = if s.root {
                format!("{} (sudo)", s.detail)
            } else {
                s.detail.clone()
            };
            t.push(
                Row::new(lead, s.label.clone())
                    .cell(Cell::muted(s.kind.clone()))
                    .cell(Cell::muted(note)),
            );
        }
        out.push(Block::Section(Section::new("Settings").block(t)));
    }
    if !plan.services.is_empty() {
        let mut t = Table::new()
            .styled_column("manager", 3, dots_core::Style::Muted)
            .styled_column("why", 3, dots_core::Style::Muted);
        for s in &plan.services {
            t.push(
                Row::new(Marker::Change, format!("{} {}", s.action, s.name))
                    .cell(Cell::muted(s.manager.clone()))
                    .cell(Cell::muted(s.detail.clone())),
            );
        }
        out.push(Block::Section(Section::new("Services").block(t)));
        let lines: Vec<String> = plan
            .services
            .iter()
            .flat_map(|s| s.commands.iter().cloned())
            .collect();
        if !lines.is_empty() {
            out.push(Block::Section(
                Section::new("Service commands").block(Block::Code { title: None, lines }),
            ));
        }
    }

    if !plan.shims.is_empty() {
        let mut t = Table::new()
            .styled_column("package", 3, dots_core::Style::Muted)
            .styled_column("target", 3, dots_core::Style::Muted);
        for sh in &plan.shims {
            let target = match (&sh.target, sh.action.as_str()) {
                (_, "remove") => String::new(),
                (Some(t), _) => t.display().to_string(),
                (None, _) => sh.bin.clone(),
            };
            let marker = if sh.action == "remove" {
                Marker::Remove
            } else {
                Marker::Add
            };
            t.push(
                Row::new(marker, format!("{} {}", sh.action, sh.dst))
                    .cell(Cell::muted(sh.package.clone()))
                    .cell(Cell::muted(target)),
            );
        }
        out.push(Block::Section(Section::new("Shims").block(t)));
    }
    if !plan.hooks.is_empty() {
        let mut t = Table::new()
            .styled_column("package", 3, dots_core::Style::Muted)
            .styled_column("why", 3, dots_core::Style::Muted);
        for h in &plan.hooks {
            t.push(
                Row::new(Marker::Change, format!("{}  {}", h.event, h.short()))
                    .cell(Cell::muted(h.package.clone()))
                    .cell(Cell::muted(h.why.clone())),
            );
        }
        out.push(Block::Section(Section::new("Hooks").block(t)));
    }

    // dots.lock rows that change, and what a provider cannot hold to.
    let shown: Vec<_> = plan
        .lock
        .iter()
        .filter(|l| (l.rewrite && (l.from.is_some() || l.has_step)) || l.drift.is_some())
        .collect();
    if !shown.is_empty() {
        let mut t = Table::new()
            .styled_column("platform", 3, dots_core::Style::Muted)
            .column("lock", 4)
            .styled_column("note", 2, dots_core::Style::Muted);
        for l in shown {
            let lead: Lead = if l.drift.is_some() && !l.rewrite {
                State::Drift.into()
            } else if l.from.is_none() {
                Marker::Add.into()
            } else {
                Marker::Change.into()
            };
            let cell = match (&l.from, &l.to) {
                (Some(f), Some(t)) => Cell::version(f, t),
                (None, Some(t)) => Cell::new(t.clone(), dots_core::Style::Version),
                (Some(f), None) => {
                    Cell::new(format!("{f} \u{2192} (installed)"), dots_core::Style::Plain)
                }
                (None, None) => Cell::muted("(installed)"),
            };
            t.push(
                Row::new(lead, l.package.clone())
                    .cell(Cell::muted(l.platform.clone()))
                    .cell(cell)
                    .cell(Cell::muted(l.drift.clone().unwrap_or_default())),
            );
        }
        out.push(Block::Section(Section::new("Lock").block(t)));
    }

    // Heads up: risks come before any confirmation.
    let mut heads: Vec<Block> = Vec::new();
    let apps: std::collections::BTreeSet<&str> = plan
        .settings
        .iter()
        .flat_map(|s| s.restart.iter().map(String::as_str))
        .chain(plan.settings.iter().flat_map(|s| {
            s.requires
                .iter()
                .filter_map(|r| r.strip_prefix("restart-app:"))
        }))
        .collect();
    if !apps.is_empty() {
        heads.push(Block::callout(format!(
            "{} will be restarted once, at the end",
            apps.into_iter().collect::<Vec<_>>().join(", ")
        )));
    }
    let needs = |what: &str| {
        plan.settings
            .iter()
            .any(|s| s.requires.iter().any(|r| r == what))
    };
    match (needs("logout"), needs("reboot")) {
        (true, true) => heads.push(Block::callout(
            "Some settings only take effect after you log out, and some after a reboot",
        )),
        (true, false) => heads.push(Block::callout(
            "Some settings only take effect after you log out",
        )),
        (false, true) => heads.push(Block::callout(
            "Some settings only take effect after a reboot",
        )),
        (false, false) => {}
    }
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
    for s in plan.steps.iter().filter(|s| s.kind == StepKind::Downgrade) {
        heads.push(Block::callout(format!(
            "\u{26a0} downgrade {} {} \u{2192} {}: apply asks first, even with --yes (--allow-downgrade skips the question)",
            s.package,
            s.from.as_deref().unwrap_or("?"),
            s.to.as_deref().unwrap_or("?")
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
    if r.steps.is_empty()
        && r.files.is_empty()
        && r.settings.is_empty()
        && r.services.is_empty()
        && r.hooks.is_empty()
        && r.shims.is_empty()
        && r.status == Status::Done
    {
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
        + r.files.iter().filter(|f| !f.ok).count()
        + r.settings.iter().filter(|s| !s.ok).count()
        + r.services.iter().filter(|s| !s.ok).count()
        + r.hooks.iter().filter(|h| !h.ok).count()
        + r.shims.iter().filter(|s| !s.ok).count();
    let total = r.steps.len()
        + r.files.len()
        + r.settings.len()
        + r.services.len()
        + r.hooks.len()
        + r.shims.len();
    let done = ok
        + r.files.iter().filter(|f| f.ok).count()
        + r.settings.iter().filter(|s| s.ok).count()
        + r.services.iter().filter(|s| s.ok).count()
        + r.hooks.iter().filter(|h| h.ok).count()
        + r.shims.iter().filter(|s| s.ok).count();
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
    if !r.removals.is_empty() {
        let mut t = Table::new()
            .styled_column("provider", 3, dots_core::Style::Muted)
            .styled_column("detail", 2, dots_core::Style::Muted);
        for x in &r.removals {
            let (state, detail) = match (x.removed, &x.detail) {
                (true, _) => (State::Done, "removed after the switch".to_string()),
                (false, Some(why)) => (State::Skipped, format!("kept: {why}")),
                (false, None) => (State::Skipped, "kept".to_string()),
            };
            t.push(
                Row::new(state, x.package.clone())
                    .cell(Cell::muted(x.provider.clone()))
                    .cell(Cell::muted(detail)),
            );
        }
        report.push(Block::Section(Section::new("Old copies").block(t)));
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
    if !r.settings.is_empty() {
        let mut t = Table::new()
            .styled_column("kind", 3, dots_core::Style::Muted)
            .styled_column("change", 3, dots_core::Style::Muted);
        for s in &r.settings {
            let state = if s.ok { State::Done } else { State::Failed };
            let detail = if s.ok {
                s.step.detail.clone()
            } else {
                s.detail.clone().unwrap_or_default()
            };
            t.push(
                Row::new(state, s.step.label.clone())
                    .cell(Cell::muted(s.step.kind.clone()))
                    .cell(Cell::muted(detail)),
            );
        }
        report.push(Block::Section(Section::new("Settings").block(t)));
    }
    if !r.restarts.is_empty() {
        let names: Vec<String> = r.restarts.iter().map(|x| x.app.clone()).collect();
        report.push(Block::text(format!("Restarted {}", names.join(", "))));
    }
    if !r.services.is_empty() {
        let mut t = Table::new()
            .styled_column("manager", 3, dots_core::Style::Muted)
            .styled_column("detail", 3, dots_core::Style::Muted);
        for s in &r.services {
            let state = if s.ok { State::Done } else { State::Failed };
            let detail = if s.ok {
                s.step.detail.clone()
            } else {
                s.detail.clone().unwrap_or_default()
            };
            t.push(
                Row::new(state, format!("{} {}", s.step.action, s.step.name))
                    .cell(Cell::muted(s.step.manager.clone()))
                    .cell(Cell::muted(detail)),
            );
        }
        report.push(Block::Section(Section::new("Services").block(t)));
    }
    if !r.shims.is_empty() {
        let mut t = Table::new()
            .styled_column("package", 3, dots_core::Style::Muted)
            .styled_column("detail", 3, dots_core::Style::Muted);
        for sh in &r.shims {
            let state = if sh.ok { State::Done } else { State::Failed };
            let detail = if sh.ok {
                sh.step
                    .target
                    .as_ref()
                    .map_or_else(String::new, |t| t.display().to_string())
            } else {
                sh.detail.clone().unwrap_or_default()
            };
            t.push(
                Row::new(state, format!("{} {}", sh.step.action, sh.step.dst))
                    .cell(Cell::muted(sh.step.package.clone()))
                    .cell(Cell::muted(detail)),
            );
        }
        report.push(Block::Section(Section::new("Shims").block(t)));
    }
    if !r.hooks.is_empty() {
        let mut t = Table::new()
            .styled_column("package", 3, dots_core::Style::Muted)
            .styled_column("detail", 3, dots_core::Style::Muted);
        for h in &r.hooks {
            let state = if h.ok { State::Done } else { State::Failed };
            let detail = if h.ok {
                dur(h.dur_ms)
            } else {
                h.detail.clone().unwrap_or_default()
            };
            t.push(
                Row::new(state, format!("{}  {}", h.step.event, h.step.short()))
                    .cell(Cell::muted(h.step.package.clone()))
                    .cell(Cell::muted(detail)),
            );
        }
        report.push(Block::Section(Section::new("Hooks").block(t)));
    }
    if !r.hooks_skipped.is_empty() {
        report.push(Block::text(format!(
            "{} not run: {}",
            plural(r.hooks_skipped.len(), "hook", "hooks"),
            r.hooks_skipped
                .iter()
                .map(|(h, why)| format!("{} {} ({why})", h.package, h.event))
                .collect::<Vec<_>>()
                .join("; ")
        )));
    }
    // One batched command that failed for several packages gives them all the same error:
    // show it once, naming the packages.
    let mut failures: Vec<(&str, Vec<&str>)> = Vec::new();
    for s in r.steps.iter().filter(|s| s.status == StepStatus::Failed) {
        if let Some(e) = &s.error {
            match failures.iter_mut().find(|(text, _)| *text == e.as_str()) {
                Some((_, pkgs)) => pkgs.push(&s.package),
                None => failures.push((e, vec![&s.package])),
            }
        }
    }
    for (text, pkgs) in failures {
        report.push(Block::Code {
            title: Some(format!("{} failed", pkgs.join(", "))),
            lines: text.lines().map(str::to_string).collect(),
        });
    }
    if !r.files_skipped.is_empty() {
        report.push(Block::text(format!(
            "{} left alone: their package did not install",
            plural(r.files_skipped.len(), "file", "files")
        )));
    }
    if !r.settings_skipped.is_empty() || !r.services_skipped.is_empty() {
        report.push(Block::text(format!(
            "{} left alone: their package did not install or configure",
            plural(
                r.settings_skipped.len() + r.services_skipped.len(),
                "setting or service",
                "settings and services"
            )
        )));
    }
    for t in &r.todos {
        if t.reason.starts_with("logout:") || t.reason.starts_with("reboot:") {
            report.push(Block::text(format!("{}: {}", t.package, t.reason)));
        }
    }
    if !r.todos.is_empty() {
        report.push(Block::hint("dots todo"));
    }
    if r.status == Status::Interrupted {
        report.push(Block::hint("dots apply"));
    }
    report
}
