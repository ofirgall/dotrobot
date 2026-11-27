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
    if plan.is_empty() && !plan.has_lock_changes() {
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
            if let Some(n) = &s.note {
                notes.push(n);
            }
            if s.interactive {
                notes.push("interactive, runs last");
            }
            let switch_note = s
                .switch
                .as_ref()
                .filter(|w| w.has_copy())
                .map(|w| match &w.keep {
                    Some(why) => format!("keeps the old {} copy: {why}", w.provider),
                    None => format!("then removes the old {} copy", w.provider),
                });
            if let Some(n) = &switch_note {
                notes.push(n);
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
                match s.switch.as_ref().filter(|w| w.has_copy()) {
                    Some(w) => format!("{} -> {}", w.provider, s.provider),
                    None => s.provider.clone(),
                },
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
        for w in plan.steps.iter().filter_map(|s| s.switch.as_ref()) {
            for a in &w.remove {
                let root = if a.needs_root { "sudo " } else { "" };
                out.push_str(&format!("  [then] {root}{}\n", a.argv.join(" ")));
            }
        }
    }
    for s in plan.steps.iter().filter(|s| s.kind == StepKind::Downgrade) {
        out.push_str(&format!(
            "\u{26a0} downgrade {} {}\n",
            s.package,
            arrow(&s.from, &s.to)
        ));
    }
    let lock: Vec<_> = plan.lock.iter().filter(|l| l.shown()).collect();
    if !lock.is_empty() {
        out.push_str(&format!("Lock ({})\n", lock.len()));
        for l in lock {
            let change = match (&l.from, &l.to) {
                (Some(f), Some(t)) => format!("{f} -> {t}"),
                (None, Some(t)) => t.clone(),
                (Some(f), None) => format!("{f} -> (installed)"),
                (None, None) => "(installed)".into(),
            };
            let note = l
                .drift
                .as_deref()
                .map(|d| format!("  drift: {d}"))
                .unwrap_or_default();
            out.push_str(&format!(
                "  {:<8} {} {}  [{}]{}\n",
                if l.rewrite { "lock" } else { "drift" },
                l.package,
                change,
                l.platform,
                note
            ));
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
    if !plan.settings.is_empty() {
        out.push_str(&format!("Settings ({})\n", plan.settings.len()));
        for st in &plan.settings {
            let root = if st.root { "  (sudo)" } else { "" };
            out.push_str(&format!(
                "  {:<8} {}  {}{}\n",
                st.action, st.label, st.detail, root
            ));
        }
    }
    if !plan.services.is_empty() {
        out.push_str(&format!("Services ({})\n", plan.services.len()));
        for sv in &plan.services {
            out.push_str(&format!(
                "  {:<12} {}  [{}] {}\n",
                sv.action, sv.name, sv.manager, sv.detail
            ));
        }
    }
    if !plan.shims.is_empty() {
        out.push_str(&format!("Shims ({})\n", plan.shims.len()));
        for sh in &plan.shims {
            let to = match (&sh.target, sh.action.as_str()) {
                (_, "remove") => String::new(),
                (Some(t), _) => format!("  -> {}", t.display()),
                (None, _) => format!("  -> {}", sh.bin),
            };
            out.push_str(&format!("  {:<8} {}{to}\n", sh.action, sh.dst));
        }
    }
    if !plan.hooks.is_empty() {
        out.push_str(&format!("Hooks ({})\n", plan.hooks.len()));
        for h in &plan.hooks {
            out.push_str(&format!(
                "  {:<12} {}  {}  ({})\n",
                h.event,
                h.package,
                h.short(),
                h.why
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
    let mut v = plan_json_base(plan);
    let lock: Vec<Value> = plan
        .lock
        .iter()
        .filter(|l| l.shown())
        .map(|l| {
            json!({
                "package": l.package,
                "provider": l.provider,
                "platform": l.platform,
                "from": l.from,
                "to": l.to,
                "rewrite": l.rewrite,
                "drift": l.drift,
            })
        })
        .collect();
    if !lock.is_empty() {
        v["lock"] = Value::Array(lock);
    }
    v
}

fn plan_json_base(plan: &Plan) -> Value {
    json!({
        "empty": plan.is_empty() && !plan.has_lock_changes(),
        "steps": plan.steps.iter().map(|s| {
            let mut v = json!({
                "package": s.package,
                "provider": s.provider,
                "name": s.native,
                "action": s.kind.as_str(),
                "from": s.from,
                "to": s.to,
                "needs_root": s.needs_root,
                "sudo": s.sudo,
                "interactive": s.interactive,
                "bootstrap": s.bootstrap,
                "note": s.note,
                "batch": s.batch,
                "after": s.deps,
            });
            if let Some(w) = s.switch.as_ref().filter(|w| w.has_copy()) {
                v["replaces"] = json!({
                    "provider": w.provider,
                    "name": w.native,
                    "version": w.version,
                    "removes": w.keep.is_none(),
                    "keep": w.keep,
                    "commands": w.remove.iter().map(|a| json!({
                        "argv": a.argv,
                        "needs_root": a.needs_root,
                    })).collect::<Vec<_>>(),
                });
            }
            v
        }).collect::<Vec<_>>(),
        "batches": plan.batches.iter().map(|b| json!({
            "id": b.id,
            "provider": b.provider,
            "packages": b.packages,
            "interactive": b.interactive,
            "sudo": b.sudo,
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
        "settings": plan.settings.iter().map(|s| json!({
            "package": s.package,
            "id": s.id,
            "label": s.label,
            "kind": s.kind,
            "action": s.action,
            "detail": s.detail,
            "root": s.root,
            "restart": s.restart,
            "requires": s.requires,
        })).collect::<Vec<_>>(),
        "services": plan.services.iter().map(|s| json!({
            "package": s.package,
            "name": s.name,
            "manager": s.manager,
            "action": s.action,
            "detail": s.detail,
            "commands": s.commands,
        })).collect::<Vec<_>>(),
        "shims": plan.shims.iter().map(|s| json!({
            "package": s.package,
            "name": s.name,
            "dst": s.dst,
            "action": s.action,
            "target": s.target.as_ref().map(|t| t.display().to_string()),
        })).collect::<Vec<_>>(),
        "hooks": plan.hooks.iter().map(|h| json!({
            "package": h.package,
            "event": h.event,
            "command": h.command,
            "why": h.why,
        })).collect::<Vec<_>>(),
        "unplanned": plan.unplanned.iter().map(|(p, w)| json!({ "package": p, "reason": w })).collect::<Vec<_>>(),
        "summary": {
            "packages": plan.steps.len(),
            "files": plan.files.len(),
            "settings": plan.settings.len(),
            "services": plan.services.len(),
            "shims": plan.shims.len(),
            "hooks": plan.hooks.len(),
            "downgrades": plan.steps.iter().filter(|s| s.kind == StepKind::Downgrade).count(),
            "sudo_steps": plan.sudo_count(),
            "root_actions": plan.batches.iter().flat_map(|b| &b.actions).filter(|a| a.needs_root).count()
                + plan.settings.iter().filter(|s| s.root).count(),
        },
    })
}

impl ApplyReport {
    /// `apply` data: the plan plus what happened.
    pub fn to_json(&self) -> Value {
        let count = |s: &str| self.steps.iter().filter(|r| r.status.as_str() == s).count();
        let mut v = json!({
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
            "settings": self.settings.iter().map(|s| json!({
                "package": s.step.package,
                "id": s.step.id,
                "label": s.step.label,
                "action": s.step.action,
                "ok": s.ok,
                "detail": s.detail,
            })).collect::<Vec<_>>(),
            "restarts": self.restarts.iter().map(|r| json!({
                "app": r.app,
                "ok": r.ok,
                "detail": r.detail,
            })).collect::<Vec<_>>(),
            "services": self.services.iter().map(|s| json!({
                "package": s.step.package,
                "name": s.step.name,
                "manager": s.step.manager,
                "action": s.step.action,
                "ok": s.ok,
                "detail": s.detail,
            })).collect::<Vec<_>>(),
            "shims": self.shims.iter().map(|s| json!({
                "package": s.step.package,
                "name": s.step.name,
                "dst": s.step.dst,
                "action": s.step.action,
                "ok": s.ok,
                "detail": s.detail,
            })).collect::<Vec<_>>(),
            "hooks": self.hooks.iter().map(|h| json!({
                "package": h.step.package,
                "event": h.step.event,
                "ok": h.ok,
                "detail": h.detail,
                "log": h.log.as_ref().map(|p| p.display().to_string()),
            })).collect::<Vec<_>>(),
            "hooks_skipped": self.hooks_skipped.iter().map(|(h, why)| json!({
                "package": h.package, "event": h.event, "reason": why,
            })).collect::<Vec<_>>(),
            "todos": self.todos.iter().map(|t| json!({
                "package": t.package, "reason": t.reason,
            })).collect::<Vec<_>>(),
            "sudo": {
                "mode": self.sudo_policy.as_str(),
                "skipped": self.sudo_skipped.iter().map(|i| json!({
                    "name": i.name,
                    "kind": i.what,
                    "provider": i.kind,
                    "reason": i.reason,
                })).collect::<Vec<_>>(),
                "not_applied_ordinary": self.sudo_unselected,
            },
            "summary": {
                "ok": count("ok"),
                "failed": count("failed"),
                "skipped": count("skipped-due-to-dep"),
                "skipped_sudo": self.sudo_skipped.len(),
                "deferred": count("deferred"),
                "not_run": count("not-run"),
                "files_ok": self.files.iter().filter(|f| f.ok).count(),
                "files_failed": self.files.iter().filter(|f| !f.ok).count(),
                "settings_ok": self.settings.iter().filter(|s| s.ok).count(),
                "settings_failed": self.settings.iter().filter(|s| !s.ok).count(),
                "services_ok": self.services.iter().filter(|s| s.ok).count(),
                "services_failed": self.services.iter().filter(|s| !s.ok).count(),
                "shims_ok": self.shims.iter().filter(|s| s.ok).count(),
                "shims_failed": self.shims.iter().filter(|s| !s.ok).count(),
                "hooks_ok": self.hooks.iter().filter(|h| h.ok).count(),
                "hooks_failed": self.hooks.iter().filter(|h| !h.ok).count(),
            },
        });
        // Only a provider switch has old copies to report.
        if !self.removals.is_empty() {
            v["removals"] = self
                .removals
                .iter()
                .map(|x| {
                    json!({
                        "package": x.package,
                        "provider": x.provider,
                        "name": x.native,
                        "version": x.version,
                        "removed": x.removed,
                        "detail": x.detail,
                    })
                })
                .collect();
        }
        v
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
                StepStatus::SkippedSudo { reason } => out.push_str(&format!(
                    "\n  skipped  {} (needs sudo: {reason})",
                    r.package
                )),
                StepStatus::NotSelected | StepStatus::Ok => {}
            }
        }
        for f in self.files.iter().filter(|f| !f.ok) {
            out.push_str(&format!(
                "\n  failed   {} ({})",
                f.step.dst,
                f.detail.as_deref().unwrap_or("file action failed")
            ));
        }
        if !self.settings.is_empty() || !self.services.is_empty() {
            let sok = self.settings.iter().filter(|s| s.ok).count();
            let vok = self.services.iter().filter(|s| s.ok).count();
            out.push_str(&format!(
                "\n{} setting(s) changed, {} failed; {} service action(s) ok, {} failed",
                sok,
                self.settings.len() - sok,
                vok,
                self.services.len() - vok
            ));
        }
        for s in self.settings.iter().filter(|s| !s.ok) {
            out.push_str(&format!(
                "\n  failed   {} ({})",
                s.step.label,
                s.detail.as_deref().unwrap_or("the change failed")
            ));
        }
        for s in self.services.iter().filter(|s| !s.ok) {
            out.push_str(&format!(
                "\n  failed   {} {} ({})",
                s.step.name,
                s.step.action,
                s.detail.as_deref().unwrap_or("the action failed")
            ));
        }
        for s in self.shims.iter().filter(|s| !s.ok) {
            out.push_str(&format!(
                "\n  failed   shim {} ({})",
                s.step.dst,
                s.detail.as_deref().unwrap_or("the shim failed")
            ));
        }
        if !self.hooks.is_empty() {
            let hok = self.hooks.iter().filter(|h| h.ok).count();
            out.push_str(&format!(
                "\n{} hook(s) ok, {} failed",
                hok,
                self.hooks.len() - hok
            ));
        }
        for h in self.hooks.iter().filter(|h| !h.ok) {
            out.push_str(&format!(
                "\n  failed   {} {} hook ({})",
                h.step.package,
                h.step.event,
                h.detail.as_deref().unwrap_or("the hook failed")
            ));
        }
        for (h, why) in &self.hooks_skipped {
            out.push_str(&format!(
                "\n  skipped  {} {} hook ({why})",
                h.package, h.event
            ));
        }
        for t in &self.todos {
            out.push_str(&format!("\n  todo     {}: {}", t.package, t.reason));
        }
        out
    }
}
