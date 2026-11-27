//! Streamed progress of `dots apply`: one line when a step starts and one when it finishes, on
//! stderr, as it happens, and the same lines in the run's live log (`logs/<run-id>/run.log`).
//!
//! - Not a terminal (a pipe, ssh without a tty, CI): plain ASCII lines, flushed one by one, no
//!   colors, spinners or cursor movement. With a terminal the live region of `dots-ui` is kept
//!   and the finished lines still scroll up as permanent lines.
//! - `--quiet` draws nothing; the log is still written. With `--json` the lines go to stderr
//!   and stdout stays the one JSON document.
//! - A failure prints its error and the log path at once, not only in the final summary. Packages
//!   that failed with the same error (one batched command) are listed under it once.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;

use dots_core::State;
use dots_engine::{Batch, Observer, Plan, StepResult, StepStatus};
use dots_ui::{IconSet, Progress, TaskId, Tier, Ui};

/// How many package names a step line shows before `+N more`.
const MAX_NAMES: usize = 6;

pub struct LiveProgress {
    p: Progress,
    log_path: PathBuf,
    batches: Mutex<BTreeMap<usize, TaskId>>,
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
        LiveProgress {
            p,
            log_path,
            batches: Mutex::default(),
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
    }
}

impl Drop for LiveProgress {
    fn drop(&mut self) {
        self.p.finish_all();
    }
}

/// `a, b, c` or `a, b, c, d, e, f +4 more`.
fn names(packages: &[String]) -> String {
    if packages.len() <= MAX_NAMES {
        packages.join(", ")
    } else {
        format!(
            "{} +{} more",
            packages[..MAX_NAMES].join(", "),
            packages.len() - MAX_NAMES
        )
    }
}

/// `brew install`, `cargo install/upgrade`: the provider and what its actions do.
fn what(b: &Batch) -> String {
    let mut kinds: Vec<String> = Vec::new();
    for a in b.actions.iter().filter(|a| !a.ops.is_empty()) {
        let k = format!("{:?}", a.kind).to_lowercase();
        if !kinds.contains(&k) {
            kinds.push(k);
        }
    }
    format!("{} {}", b.provider, kinds.join("/"))
        .trim_end()
        .to_string()
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

impl Observer for LiveProgress {
    fn planned(&self, plan: &Plan) {
        self.open_log();
        self.p.set_total(
            plan.steps.len()
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
        let id = self.p.start(&names(&b.packages), &what(b));
        self.batches.lock().unwrap().insert(b.id, id);
    }

    fn batch_finished(&self, b: &Batch, results: &[StepResult]) {
        let Some(id) = self.batches.lock().unwrap().remove(&b.id) else {
            return;
        };
        let failed: Vec<&StepResult> = results
            .iter()
            .filter(|r| r.status == StepStatus::Failed)
            .collect();
        let dur = Duration::from_millis(results.iter().map(|r| r.dur_ms).sum());
        let (state, detail) = if !failed.is_empty() {
            let detail = match failed.as_slice() {
                [one] => one
                    .error
                    .as_deref()
                    .and_then(|e| e.lines().next())
                    .unwrap_or("failed")
                    .to_string(),
                _ => format!("{} of {} failed", failed.len(), results.len()),
            };
            (State::Failed, detail)
        } else if results.iter().all(|r| r.status == StepStatus::Ok) {
            let action = results.first().map_or("install", |r| r.action.as_str());
            (State::Done, past(action))
        } else {
            (State::Skipped, "not run".to_string())
        };
        self.p.finish(id, state, &detail, Some(dur));

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

    fn file_done(&self, r: &dots_engine::FileStepResult) {
        let detail = if r.ok {
            r.step.action.clone()
        } else {
            r.detail.clone().unwrap_or_default()
        };
        self.record(
            &format!("{}  {}", r.step.kind, r.step.dst),
            r.ok,
            &detail,
            r.dur_ms,
        );
    }

    fn setting_done(&self, r: &dots_engine::SettingStepResult) {
        let detail = if r.ok {
            // An ok step with a note was skipped (its tool is still missing).
            r.detail.clone().unwrap_or_else(|| r.step.detail.clone())
        } else {
            r.detail.clone().unwrap_or_default()
        };
        self.record(
            &format!("{}  {}", r.step.kind, r.step.label),
            r.ok,
            &detail,
            r.dur_ms,
        );
    }

    fn shim_done(&self, r: &dots_engine::ShimStepResult) {
        let detail = if r.ok {
            r.step.action.clone()
        } else {
            r.detail.clone().unwrap_or_default()
        };
        self.record(&format!("shim  {}", r.step.dst), r.ok, &detail, r.dur_ms);
    }

    fn hook_done(&self, r: &dots_engine::HookStepResult) {
        let detail = if r.ok {
            r.step.short()
        } else {
            r.detail.clone().unwrap_or_default()
        };
        self.record(
            &format!("{}  {}", r.step.package, r.step.event),
            r.ok,
            &detail,
            r.dur_ms,
        );
        if let (false, Some(log)) = (r.ok, &r.log) {
            self.p.println(&format!("        log: {}", log.display()));
        }
    }

    fn service_done(&self, r: &dots_engine::ServiceStepResult) {
        let detail = if r.ok {
            r.step.action.clone()
        } else {
            r.detail.clone().unwrap_or_default()
        };
        self.record(
            &format!("{}  {}", r.step.manager, r.step.name),
            r.ok,
            &detail,
            r.dur_ms,
        );
    }
}
