//! Text and JSON views of a plan and of an apply run. The CLI wraps these in a `Report`.

use serde_json::{Value, json};

use crate::apply::{ApplyReport, StepStatus};
use crate::plan::{Plan, StepKind};

fn arrow(from: &Option<String>, to: &Option<String>) -> String {
    match (from, to) {
        (Some(f), Some(t)) => format!("{f} -> {t}"),
        (None, Some(t)) => t.clone(),
        (Some(f), None) => format!("{f} -> latest"),
        (None, None) => String::new(),
    }
}

/// The plan as the user reads it before confirming: steps, the exact commands, and files.
pub fn plan_text(plan: &Plan) -> String {
    if plan.is_empty() {
        return "Nothing to do: the machine already matches the repo.".to_string();
    }
    let mut out = String::new();
    if !plan.steps.is_empty() {
        out.push_str(&format!("Packages ({})\n", plan.steps.len()));
        for s in &plan.steps {
            let mut notes: Vec<&str> = Vec::new();
            if s.bootstrap {
                notes.push("bootstrap");
            }
            if s.needs_root {
                notes.push("sudo");
            }
            if s.interactive {
                notes.push("interactive, runs last");
            }
            let notes = if notes.is_empty() {
                String::new()
            } else {
                format!("  ({})", notes.join(", "))
            };
            out.push_str(&format!(
                "  {:<9} {} {}  [{}]{}\n",
                s.kind.as_str(),
                s.package,
                arrow(&s.from, &s.to),
                s.provider,
                notes
            ));
        }
        out.push_str("Commands\n");
        for b in &plan.batches {
            for a in &b.actions {
                let root = if a.needs_root { "sudo " } else { "" };
                out.push_str(&format!("  [{}] {root}{}\n", b.id + 1, a.argv.join(" ")));
            }
        }
    }
    if !plan.files.is_empty() {
        out.push_str(&format!("Files ({})\n", plan.files.len()));
        for f in &plan.files {
            let owned = if f.owned { "  (owned)" } else { "" };
            out.push_str(&format!(
                "  {:<8} {}  {}{}\n",
                f.kind, f.dst, f.detail, owned
            ));
        }
    }
    for (p, why) in &plan.unplanned {
        out.push_str(&format!("Not planned: {p} ({why})\n"));
    }
    out.truncate(out.trim_end().len());
    out
}

pub fn plan_json(plan: &Plan) -> Value {
    json!({
        "empty": plan.is_empty(),
        "steps": plan.steps.iter().map(|s| json!({
            "package": s.package,
            "provider": s.provider,
            "name": s.native,
            "action": s.kind.as_str(),
            "from": s.from,
            "to": s.to,
            "needs_root": s.needs_root,
            "interactive": s.interactive,
            "bootstrap": s.bootstrap,
            "batch": s.batch,
            "after": s.deps,
        })).collect::<Vec<_>>(),
        "batches": plan.batches.iter().map(|b| json!({
            "id": b.id,
            "provider": b.provider,
            "packages": b.packages,
            "interactive": b.interactive,
            "after": b.deps,
            "commands": b.actions.iter().map(|a| json!({
                "argv": a.argv,
                "needs_root": a.needs_root,
            })).collect::<Vec<_>>(),
        })).collect::<Vec<_>>(),
        "files": plan.files.iter().map(|f| json!({
            "package": f.package,
            "dst": f.dst,
            "kind": f.kind,
            "action": f.action,
            "detail": f.detail,
            "owned": f.owned,
        })).collect::<Vec<_>>(),
        "unplanned": plan.unplanned.iter().map(|(p, w)| json!({ "package": p, "reason": w })).collect::<Vec<_>>(),
        "summary": {
            "packages": plan.steps.len(),
            "files": plan.files.len(),
            "downgrades": plan.steps.iter().filter(|s| s.kind == StepKind::Downgrade).count(),
            "root_actions": plan.batches.iter().flat_map(|b| &b.actions).filter(|a| a.needs_root).count(),
        },
    })
}

impl ApplyReport {
    /// `apply` data: the plan plus what happened.
    pub fn to_json(&self) -> Value {
        let count = |s: &str| self.steps.iter().filter(|r| r.status.as_str() == s).count();
        json!({
            "status": self.status.as_str(),
            "run": self.run_id,
            "dry_run": self.status == crate::apply::Status::DryRun,
            "plan": plan_json(&self.plan),
            "results": self.steps.iter().map(|r| {
                let mut v = json!({
                    "package": r.package,
                    "provider": r.provider,
                    "action": r.action,
                    "from": r.from,
                    "to": r.to,
                    "status": r.status.as_str(),
                    "log": r.log.as_ref().map(|p| p.display().to_string()),
                    "error": r.error,
                    "dur_ms": r.dur_ms,
                });
                if let StepStatus::SkippedDueToDep { dependency } = &r.status {
                    v["dependency"] = json!(dependency);
                }
                v
            }).collect::<Vec<_>>(),
            "files": self.files.iter().map(|f| json!({
                "package": f.step.package,
                "dst": f.step.dst,
                "kind": f.step.kind,
                "action": f.step.action,
                "ok": f.ok,
                "detail": f.detail,
            })).collect::<Vec<_>>(),
            "todos": self.todos.iter().map(|t| json!({
                "package": t.package, "reason": t.reason,
            })).collect::<Vec<_>>(),
            "summary": {
                "ok": count("ok"),
                "failed": count("failed"),
                "skipped": count("skipped-due-to-dep"),
                "deferred": count("deferred"),
                "not_run": count("not-run"),
                "files_ok": self.files.iter().filter(|f| f.ok).count(),
                "files_failed": self.files.iter().filter(|f| !f.ok).count(),
            },
        })
    }

    /// The end-of-run summary for the terminal.
    pub fn summary_text(&self) -> String {
        let mut out = String::new();
        let n = |s: &str| self.steps.iter().filter(|r| r.status.as_str() == s).count();
        let fok = self.files.iter().filter(|f| f.ok).count();
        let ffail = self.files.len() - fok;
        out.push_str(&format!(
            "{} package(s) ok, {} failed, {} skipped, {} deferred; {} file(s) ok, {} failed",
            n("ok"),
            n("failed"),
            n("skipped-due-to-dep"),
            n("deferred"),
            fok,
            ffail
        ));
        for r in &self.steps {
            match &r.status {
                StepStatus::Failed => {
                    out.push_str(&format!(
                        "\n  failed   {} ({} via {})",
                        r.package, r.action, r.provider
                    ));
                    if let Some(e) = &r.error {
                        for l in e.lines() {
                            out.push_str(&format!("\n             {l}"));
                        }
                    }
                }
                StepStatus::SkippedDueToDep { dependency } => out.push_str(&format!(
                    "\n  skipped  {} (skipped-due-to-dep: {dependency})",
                    r.package
                )),
                StepStatus::Deferred => out.push_str(&format!(
                    "\n  todo     {} runs interactively; run `dots apply {}` in a terminal",
                    r.package, r.package
                )),
                StepStatus::NotRun => out.push_str(&format!("\n  not run  {}", r.package)),
                StepStatus::Ok => {}
            }
        }
        for f in self.files.iter().filter(|f| !f.ok) {
            out.push_str(&format!(
                "\n  failed   {} ({})",
                f.step.dst,
                f.detail.as_deref().unwrap_or("file action failed")
            ));
        }
        out
    }
}
