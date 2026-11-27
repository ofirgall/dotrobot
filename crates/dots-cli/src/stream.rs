//! Streamed progress of `dots apply`: a line when a step starts and one when it finishes, on
//! stderr, as it happens, and the same lines in the run's live log (`logs/<run-id>/run.log`).
//!
//! - A terminal: the live region of `dots-ui` shows the running steps (spinner, elapsed time
//!   and the last line of the tool's own output) and an overall bar; finished steps scroll up
//!   as permanent lines.
//! - Not a terminal (a pipe, ssh without a tty, CI): plain ASCII lines, flushed one by one, no
//!   colors, spinners or cursor movement. A step running longer than 30 s adds a
//!   `still running` line every 30 s (`DOTS_HEARTBEAT_SECS` changes the interval, `0` turns
//!   it off), also in the run log.
//! - `--quiet` draws nothing; the log is still written. With `--json` the lines go to stderr
//!   and stdout stays the one JSON document.
//! - A failure prints its error and the log path at once, not only in the final summary. Packages
//!   that failed with the same error (one batched command) are listed under it once.
//!
//! The wording (`Installing plantuml (brew)`, `Linked ~/.zshrc`) lives in [`crate::wording`].

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use dots_core::State;
use dots_engine::{Batch, HookStep, Observer, Plan, StepRef, StepResult, StepStatus};
use dots_providers::LineSink;
use dots_ui::{IconSet, Progress, TaskId, Tier, Ui};

use crate::wording as w;

/// The heartbeat interval override, in seconds (fractions allowed; `0` turns it off).
const HEARTBEAT_ENV: &str = "DOTS_HEARTBEAT_SECS";

/// A phase that reports its steps one by one: its live note goes away after the last one.
struct Note {
    id: TaskId,
    remaining: usize,
}

pub struct LiveProgress {
    p: Progress,
    log_path: PathBuf,
    /// A running batch: its line and when it started (its duration is wall clock, not the sum
    /// of its packages, which share one command's time).
    batches: Mutex<BTreeMap<usize, (TaskId, Instant)>>,
    /// Running hooks, by `package/event`.
    hooks: Mutex<BTreeMap<String, TaskId>>,
    note: Mutex<Option<Note>>,
}

impl LiveProgress {
    /// `stream`: draw on stderr (false for `--quiet`). The log at `log_path` is opened when the
    /// run starts, so a run with nothing to do leaves no file behind.
    pub fn new(ui: &Ui, stream: bool, log_path: PathBuf) -> Self {
        let mut ui = ui.clone();
        if !ui.stderr_tty {
            // Piped, redirected or over ssh without a tty: lines only, in plain ASCII.
            ui.tier = Tier::Plain;
            ui.icons = IconSet::Ascii;
            ui.reduce_motion = true;
        }
        let p = if stream {
            Progress::stderr(&ui)
        } else {
            Progress::silent(&ui)
        };
        if let Some(v) = std::env::var(HEARTBEAT_ENV)
            .ok()
            .and_then(|v| v.trim().parse::<f64>().ok())
        {
            p.set_heartbeat((v > 0.0).then(|| Duration::from_secs_f64(v)));
        }
        LiveProgress {
            p,
            log_path,
            batches: Mutex::default(),
            hooks: Mutex::default(),
            note: Mutex::new(None),
        }
    }

    fn open_log(&self) {
        let opened = self
            .log_path
            .parent()
            .map_or(Ok(()), std::fs::create_dir_all)
            .and_then(|()| {
                std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&self.log_path)
            });
        match opened {
            Ok(f) => {
                self.p.set_log(Box::new(f));
                self.p.println(&format!(
                    " run log: {}  (follow it with `tail -f`)",
                    self.log_path.display()
                ));
            }
            Err(e) => self.p.println(&format!(
                " cannot write the run log {}: {e}",
                self.log_path.display()
            )),
        }
    }

    /// A step that is over: one line.
    fn record(&self, label: &str, ok: bool, detail: &str, dur_ms: u64) {
        let state = if ok { State::Done } else { State::Failed };
        self.p
            .record(label, state, detail, Some(Duration::from_millis(dur_ms)));
        self.note_step_done();
    }

    /// A phase starts: its note replaces the previous phase's.
    fn phase_started(&self, what: &str, n: usize) {
        self.end_note();
        if n == 0 {
            return;
        }
        let id = self.p.start_note(&w::phase_note(what, n));
        *self.note.lock().unwrap() = Some(Note { id, remaining: n });
    }

    fn note_step_done(&self) {
        let mut g = self.note.lock().unwrap();
        if let Some(n) = g.as_mut() {
            n.remaining = n.remaining.saturating_sub(1);
            if n.remaining == 0 {
                self.p.dismiss(n.id);
                *g = None;
            }
        }
    }

    fn end_note(&self) {
        if let Some(n) = self.note.lock().unwrap().take() {
            self.p.dismiss(n.id);
        }
    }
}

impl Drop for LiveProgress {
    fn drop(&mut self) {
        self.p.finish_all();
    }
}

fn past(action: &str) -> String {
    match action {
        "install" => "installed".into(),
        "upgrade" => "upgraded".into(),
        "downgrade" => "downgraded".into(),
        "remove" => "removed".into(),
        other => other.to_string(),
    }
}

fn hook_key(h: &HookStep) -> String {
    format!("{}/{}", h.package, h.event)
}

impl Observer for LiveProgress {
    fn planned(&self, plan: &Plan) {
        // The plan is confirmed: the "working it out" indicator is over.
        dots_ui::phase::finish();
        self.open_log();
        self.p.set_total(
            plan.batches.len()
                + plan.files.len()
                + plan.settings.len()
                + plan.services.len()
                + plan.shims.len()
                + plan.hooks.len(),
        );
    }

    fn sudo_prompt(&self, begin: bool) {
        if begin {
            self.p.suspend();
        } else {
            self.p.resume();
        }
    }

    fn batch_started(&self, b: &Batch) {
        self.end_note();
        let id = self.p.start(&w::batch_running(b), "");
        self.batches
            .lock()
            .unwrap()
            .insert(b.id, (id, Instant::now()));
        if b.interactive {
            // The tool talks to the user itself: the live region would be drawn over it.
            self.p.suspend();
        }
    }

    fn output_sink(&self, step: StepRef<'_>) -> Option<LineSink> {
        let id = match step {
            StepRef::Batch(b) => self.batches.lock().unwrap().get(&b.id).map(|(id, _)| *id),
            StepRef::Hook(h) => self.hooks.lock().unwrap().get(&hook_key(h)).copied(),
        }?;
        Some(self.p.output_sink(id))
    }

    fn batch_finished(&self, b: &Batch, results: &[StepResult]) {
        let Some((id, started)) = self.batches.lock().unwrap().remove(&b.id) else {
            return;
        };
        if b.interactive {
            self.p.resume();
        }
        let failed: Vec<&StepResult> = results
            .iter()
            .filter(|r| r.status == StepStatus::Failed)
            .collect();
        let dur = started.elapsed();
        let ok: Vec<String> = results
            .iter()
            .filter(|r| r.status == StepStatus::Ok)
            .map(|r| r.package.clone())
            .collect();
        // The finished line: `Installed a, b` when it all went through, otherwise the running
        // wording with what went wrong.
        let (state, label, detail) = if !failed.is_empty() {
            let mut detail = match failed.as_slice() {
                [one] => format!(
                    "{}: {}",
                    one.package,
                    one.error
                        .as_deref()
                        .and_then(|e| e.lines().next())
                        .unwrap_or("failed")
                ),
                _ => format!("{} of {} failed", failed.len(), results.len()),
            };
            // A partial failure: say which packages did go through, so a batch line naming
            // them all does not read as all of them failing.
            if !ok.is_empty() {
                let action = results
                    .iter()
                    .find(|r| r.status == StepStatus::Ok)
                    .map_or("install", |r| r.action.as_str());
                detail.push_str(&format!(" ({} {})", past(action), w::names(&ok)));
            }
            (State::Failed, None, detail)
        } else if results.iter().all(|r| r.status == StepStatus::Ok) {
            (State::Done, Some(w::batch_done(b, &ok)), String::new())
        } else {
            (State::Skipped, None, "not run".to_string())
        };
        self.p
            .finish_as(id, state, label.as_deref(), &detail, Some(dur));
        self.note_step_done();

        // The error and the log, now: each distinct error once, naming its packages.
        let mut groups: Vec<(Vec<&str>, &StepResult)> = Vec::new();
        for r in &failed {
            match groups
                .iter_mut()
                .find(|(_, g)| g.error == r.error && g.log == r.log)
            {
                Some((pkgs, _)) => pkgs.push(&r.package),
                None => groups.push((vec![&r.package], r)),
            }
        }
        for (pkgs, r) in groups {
            let mut lines = r.error.as_deref().unwrap_or("failed").lines();
            self.p.println(&format!(
                "        {}: {}",
                pkgs.join(", "),
                lines.next().unwrap_or("failed")
            ));
            for l in lines {
                self.p.println(&format!("          {l}"));
            }
            if let Some(log) = &r.log {
                self.p.println(&format!("        log: {}", log.display()));
            }
        }
    }

    fn files_started(&self, total: usize) {
        self.phase_started("file change", total);
    }

    fn file_done(&self, r: &dots_engine::FileStepResult) {
        let detail = if r.ok {
            r.step.action.clone()
        } else {
            r.detail.clone().unwrap_or_default()
        };
        self.record(
            &w::file(&r.step.kind, &r.step.dst, r.ok),
            r.ok,
            &detail,
            r.dur_ms,
        );
    }

    fn settings_started(&self, total: usize) {
        self.phase_started("setting", total);
    }

    fn setting_done(&self, r: &dots_engine::SettingStepResult) {
        let detail = if r.ok {
            // An ok step with a note was skipped (its tool is still missing).
            r.detail.clone().unwrap_or_else(|| r.step.detail.clone())
        } else {
            r.detail.clone().unwrap_or_default()
        };
        self.record(
            &w::setting(&r.step.action, &r.step.label, r.ok),
            r.ok,
            &detail,
            r.dur_ms,
        );
    }

    fn shim_done(&self, r: &dots_engine::ShimStepResult) {
        let detail = if r.ok {
            String::new()
        } else {
            r.detail.clone().unwrap_or_default()
        };
        self.record(
            &w::shim(&r.step.action, &r.step.dst, r.ok),
            r.ok,
            &detail,
            r.dur_ms,
        );
    }

    fn hook_started(&self, h: &HookStep) {
        self.end_note();
        let id = self.p.start(&w::hook(&h.event, &h.package, false), "");
        self.hooks.lock().unwrap().insert(hook_key(h), id);
    }

    fn hook_done(&self, r: &dots_engine::HookStepResult) {
        let detail = if r.ok {
            r.step.short()
        } else {
            r.detail.clone().unwrap_or_default()
        };
        let running = self.hooks.lock().unwrap().remove(&hook_key(&r.step));
        let dur = Some(Duration::from_millis(r.dur_ms));
        match running {
            Some(id) => {
                let state = if r.ok { State::Done } else { State::Failed };
                let label = r.ok.then(|| w::hook(&r.step.event, &r.step.package, true));
                self.p.finish_as(id, state, label.as_deref(), &detail, dur);
                self.note_step_done();
            }
            // Ran inside its package's batch (a `pre_install` hook): only the result is known.
            None => self.record(
                &w::hook(&r.step.event, &r.step.package, r.ok),
                r.ok,
                &detail,
                r.dur_ms,
            ),
        }
        if let (false, Some(log)) = (r.ok, &r.log) {
            self.p.println(&format!("        log: {}", log.display()));
        }
    }

    fn services_started(&self, total: usize) {
        self.phase_started("service change", total);
    }

    fn service_done(&self, r: &dots_engine::ServiceStepResult) {
        let detail = if r.ok {
            r.step.manager.clone()
        } else {
            r.detail.clone().unwrap_or_default()
        };
        self.record(
            &w::service(&r.step.action, &r.step.name, r.ok),
            r.ok,
            &detail,
            r.dur_ms,
        );
    }
}
