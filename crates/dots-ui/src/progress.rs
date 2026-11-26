//! Progress and the live region for long operations (terminal-ui "Progress and
//! responsiveness"; UX §4.5). The engine reports task events; this draws them on stderr.
//!
//! - Animated (Basic/Rich tier, stderr is a TTY, no `DOTS_REDUCE_MOTION`): a live region with
//!   one spinner line per running task and a footer, redrawn at most 15 times a second.
//!   Finished tasks scroll up as permanent single lines. Rich adds synchronized output
//!   (mode 2026) and the terminal's own progress indicator (OSC 9;4).
//! - Otherwise (Plain, reduced motion, not a TTY): no spinner frames, one line per state change.

use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use dots_core::{State, Tone};

use crate::Tier;
use crate::config::IconSet;
use crate::layout::{truncate_end, width};
use crate::theme::{SPINNER, paint, state_symbol};
use crate::ui::Ui;

pub trait ProgressSink: Write + Send {}
impl<T: Write + Send> ProgressSink for T {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TaskId(usize);

struct Task {
    id: TaskId,
    label: String,
    detail: String,
    last_output: String,
    started: Instant,
}

struct Inner {
    ui: Ui,
    animate: bool,
    sink: Box<dyn ProgressSink>,
    running: Vec<Task>,
    next_id: usize,
    total: usize,
    done: usize,
    failed: usize,
    live_lines: usize,
    frame: usize,
    started: Instant,
    last_draw: Option<Instant>,
    last_osc: Option<Instant>,
    osc_active: bool,
}

pub struct Progress {
    inner: Arc<Mutex<Inner>>,
    ticker: Option<(Arc<AtomicBool>, JoinHandle<()>)>,
}

const FRAME_INTERVAL: Duration = Duration::from_millis(66);

impl Progress {
    /// Progress on stderr. Animates only when `ui.animate()`.
    pub fn stderr(ui: &Ui) -> Progress {
        Progress::with_sink(ui, Box::new(std::io::stderr()), ui.animate(), true)
    }

    /// For tests and embedders: an explicit sink and animation flag, no ticker thread (call
    /// [`Progress::tick`] to advance the spinner).
    pub fn manual(ui: &Ui, sink: Box<dyn ProgressSink>, animate: bool) -> Progress {
        Progress::with_sink(ui, sink, animate && ui.tier != Tier::Plain, false)
    }

    fn with_sink(ui: &Ui, sink: Box<dyn ProgressSink>, animate: bool, ticker: bool) -> Progress {
        let inner = Arc::new(Mutex::new(Inner {
            ui: ui.clone(),
            animate,
            sink,
            running: Vec::new(),
            next_id: 0,
            total: 0,
            done: 0,
            failed: 0,
            live_lines: 0,
            frame: 0,
            started: Instant::now(),
            last_draw: None,
            last_osc: None,
            osc_active: false,
        }));
        let ticker = (animate && ticker).then(|| {
            let stop = Arc::new(AtomicBool::new(false));
            let (flag, shared) = (stop.clone(), inner.clone());
            let handle = std::thread::spawn(move || {
                while !flag.load(Ordering::Relaxed) {
                    std::thread::sleep(Duration::from_millis(80));
                    lock(&shared).tick();
                }
            });
            (stop, handle)
        });
        Progress { inner, ticker }
    }

    /// Whether frames are drawn (false means one line per state change).
    pub fn animated(&self) -> bool {
        lock(&self.inner).animate
    }

    /// How many tasks the run has in total, for the footer and the terminal progress bar.
    pub fn set_total(&self, total: usize) {
        let mut i = lock(&self.inner);
        i.total = total;
        i.draw(false);
    }

    pub fn start(&self, label: &str, detail: &str) -> TaskId {
        let mut i = lock(&self.inner);
        let id = TaskId(i.next_id);
        i.next_id += 1;
        let task = Task {
            id,
            label: label.to_string(),
            detail: detail.to_string(),
            last_output: String::new(),
            started: Instant::now(),
        };
        if !i.animate {
            let line = i.event_line(State::Running, label, detail, None);
            i.print_line(&line);
        }
        i.running.push(task);
        i.draw(true);
        id
    }

    /// The tool's latest output line, shown dimmed next to the spinner (live region only).
    pub fn output(&self, id: TaskId, line: &str) {
        let mut i = lock(&self.inner);
        if let Some(t) = i.running.iter_mut().find(|t| t.id == id) {
            t.last_output = line.trim().to_string();
        }
        i.draw(false);
    }

    /// A task changed state for good: it leaves the live region as a permanent line.
    pub fn finish(&self, id: TaskId, state: State, detail: &str, elapsed: Option<Duration>) {
        let mut i = lock(&self.inner);
        let Some(pos) = i.running.iter().position(|t| t.id == id) else {
            return;
        };
        let task = i.running.remove(pos);
        i.done += 1;
        if matches!(state, State::Failed | State::Missing | State::Conflict) {
            i.failed += 1;
        }
        let elapsed = elapsed.unwrap_or_else(|| task.started.elapsed());
        let detail = if matches!(state, State::Ok | State::Done) || detail.is_empty() {
            if matches!(state, State::Ok | State::Done) {
                detail.to_string()
            } else {
                state.word().to_string()
            }
        } else {
            format!("{}: {detail}", state.word())
        };
        let line = i.event_line(state, &task.label, &detail, Some(elapsed));
        i.print_line(&line);
        i.draw(true);
    }

    /// Print a permanent line above the live region.
    pub fn println(&self, line: &str) {
        lock(&self.inner).print_line(line);
    }

    /// Clear the live region (before a sudo prompt or an interactive task).
    pub fn suspend(&self) {
        lock(&self.inner).clear_live();
    }

    /// Draw the live region again after [`Progress::suspend`].
    pub fn resume(&self) {
        lock(&self.inner).draw(true);
    }

    /// Advance the spinner one frame and redraw (throttled).
    pub fn tick(&self) {
        lock(&self.inner).tick();
    }

    /// Tear everything down: the live region, the terminal progress indicator. Also done on drop.
    pub fn finish_all(&self) {
        lock(&self.inner).teardown();
    }
}

impl Drop for Progress {
    fn drop(&mut self) {
        if let Some((stop, handle)) = self.ticker.take() {
            stop.store(true, Ordering::Relaxed);
            let _ = handle.join();
        }
        lock(&self.inner).teardown();
    }
}

fn lock(m: &Mutex<Inner>) -> MutexGuard<'_, Inner> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// `850ms`, `12s`, `3m 05s` (UX §3.3). Never raw seconds past 60.
pub fn format_duration(d: Duration) -> String {
    let ms = d.as_millis();
    if ms < 1000 {
        format!("{ms}ms")
    } else if d.as_secs() < 60 {
        let s = d.as_secs_f64();
        if s < 10.0 {
            format!("{s:.1}s")
        } else {
            format!("{}s", d.as_secs())
        }
    } else {
        format!("{}m {:02}s", d.as_secs() / 60, d.as_secs() % 60)
    }
}

impl Inner {
    fn p(&self, tone: Option<Tone>, bold: bool, dim: bool, text: &str) -> String {
        paint(&self.ui, tone, bold, dim, text)
    }

    /// A permanent event line: `✓ name  detail   4.2s`.
    fn event_line(
        &self,
        state: State,
        label: &str,
        detail: &str,
        elapsed: Option<Duration>,
    ) -> String {
        let sym = if state == State::Running && self.animate {
            SPINNER[self.frame % SPINNER.len()]
        } else {
            state_symbol(self.ui.icons, state)
        };
        let mut line = format!(
            " {} {}",
            self.p(Some(state.tone()), false, false, sym),
            label
        );
        if !detail.is_empty() {
            line.push_str("  ");
            line.push_str(&self.p(Some(Tone::Muted), false, false, detail));
        }
        if let Some(e) = elapsed {
            line.push_str("  ");
            line.push_str(&self.p(Some(Tone::Muted), false, false, &format_duration(e)));
        }
        line
    }

    fn print_line(&mut self, line: &str) {
        self.clear_live();
        let _ = writeln!(self.sink, "{line}");
        let _ = self.sink.flush();
    }

    fn clear_live(&mut self) {
        if self.live_lines > 0 {
            let _ = write!(self.sink, "\x1b[{}A\r\x1b[J", self.live_lines);
            self.live_lines = 0;
            let _ = self.sink.flush();
        }
    }

    fn tick(&mut self) {
        self.frame += 1;
        self.draw(false);
    }

    fn live_lines(&self) -> Vec<String> {
        let w = self.ui.width;
        let sp = SPINNER[self.frame % SPINNER.len()];
        let mut lines = Vec::new();
        for t in &self.running {
            let elapsed = format_duration(t.started.elapsed());
            let head = format!(" {sp} {}", t.label);
            let detail = if t.detail.is_empty() {
                String::new()
            } else {
                format!("  {}", t.detail)
            };
            let fixed = width(&head) + width(&detail) + 2 + width(&elapsed);
            let room = w.saturating_sub(fixed + 2);
            let out = if t.last_output.is_empty() || room < 8 {
                String::new()
            } else {
                format!(
                    "  {}",
                    truncate_end(&t.last_output, room, self.ui.ellipsis())
                )
            };
            lines.push(format!(
                " {} {}{}{}  {}",
                self.p(Some(Tone::Info), false, false, sp),
                t.label,
                self.p(Some(Tone::Muted), false, false, &detail),
                self.p(Some(Tone::Muted), false, true, &out),
                self.p(Some(Tone::Muted), false, false, &elapsed),
            ));
        }
        let rule = if self.ui.icons == IconSet::Ascii {
            "-"
        } else {
            "─"
        };
        lines.push(format!(
            " {}",
            self.p(
                Some(Tone::Muted),
                false,
                false,
                &rule.repeat(w.saturating_sub(2).min(60))
            )
        ));
        let mut footer = format!("{} of {} done", self.done, self.total.max(self.done));
        if self.failed > 0 {
            footer.push_str(&format!(" · {} failed", self.failed));
        }
        footer.push_str(&format!(" · {}", format_duration(self.started.elapsed())));
        lines.push(format!(" {footer}"));
        lines
    }

    fn draw(&mut self, force: bool) {
        if !self.animate {
            return;
        }
        if !force && self.last_draw.is_some_and(|t| t.elapsed() < FRAME_INTERVAL) {
            return;
        }
        self.last_draw = Some(Instant::now());
        let rich = self.ui.tier == Tier::Rich;
        let mut buf = String::new();
        if rich {
            buf.push_str("\x1b[?2026h");
        }
        if self.live_lines > 0 {
            buf.push_str(&format!("\x1b[{}A\r\x1b[J", self.live_lines));
        }
        let lines = if self.running.is_empty() {
            Vec::new()
        } else {
            self.live_lines()
        };
        for l in &lines {
            buf.push_str(l);
            buf.push('\n');
        }
        if rich {
            buf.push_str("\x1b[?2026l");
            // The terminal's own progress indicator, at most twice a second.
            if self.total > 0
                && self
                    .last_osc
                    .is_none_or(|t| t.elapsed() >= Duration::from_millis(500))
            {
                let pct = self.done * 100 / self.total.max(1);
                buf.push_str(&format!("\x1b]9;4;1;{pct}\x07"));
                self.last_osc = Some(Instant::now());
                self.osc_active = true;
            }
        }
        self.live_lines = lines.len();
        let _ = self.sink.write_all(buf.as_bytes());
        let _ = self.sink.flush();
    }

    fn teardown(&mut self) {
        self.running.clear();
        self.clear_live();
        if self.osc_active {
            let _ = write!(self.sink, "\x1b]9;4;0;\x07");
            let _ = self.sink.flush();
            self.osc_active = false;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, Default)]
    struct Buf(Arc<Mutex<Vec<u8>>>);
    impl Write for Buf {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    impl Buf {
        fn text(&self) -> String {
            String::from_utf8(self.0.lock().unwrap().clone()).unwrap()
        }
    }

    fn run(ui: &Ui, animate: bool) -> String {
        let buf = Buf::default();
        {
            let p = Progress::manual(ui, Box::new(buf.clone()), animate);
            p.set_total(2);
            let a = p.start("ripgrep, fd", "brew install");
            let b = p.start("herdr", "script");
            p.output(a, "Pouring ripgrep");
            p.tick();
            p.finish(
                b,
                State::Failed,
                "script failed",
                Some(Duration::from_secs(12)),
            );
            p.finish(a, State::Ok, "installed", Some(Duration::from_millis(4200)));
        }
        buf.text()
    }

    #[test]
    fn plain_prints_one_ascii_line_per_state_change_and_no_escapes() {
        let ui = Ui::fixed(Tier::Plain, 80);
        let text = run(&ui, true);
        assert!(!text.contains('\x1b'), "{text:?}");
        assert_eq!(
            text,
            " [..] ripgrep, fd  brew install\n \
             [..] herdr  script\n \
             [FAIL] herdr  failed: script failed  12s\n \
             [ok] ripgrep, fd  installed  4.2s\n"
        );
    }

    #[test]
    fn reduced_motion_prints_no_spinner_frames() {
        let ui = Ui::fixed(Tier::Basic, 80);
        let text = run(&ui, false);
        for frame in SPINNER {
            assert!(!text.contains(frame), "spinner frame in {text:?}");
        }
        assert!(text.contains("✓") && text.contains("installed"));
        assert!(!text.contains("\x1b[J"), "no live region");
    }

    #[test]
    fn animated_draws_a_live_region_and_finished_lines_scroll_up() {
        let ui = Ui::fixed(Tier::Basic, 80);
        let text = run(&ui, true);
        assert!(text.contains("\x1b[J"), "redraws the region");
        assert!(text.contains("1 of 2 done"));
        assert!(text.contains("1 failed"));
        assert!(text.contains("✓"));
        assert!(!text.contains("\x1b]9;4"), "OSC 9;4 is Rich only");
    }

    #[test]
    fn rich_uses_synchronized_output_and_clears_terminal_progress() {
        let ui = Ui::fixed(Tier::Rich, 80);
        let text = run(&ui, true);
        assert!(text.contains("\x1b[?2026h") && text.contains("\x1b[?2026l"));
        assert!(text.contains("\x1b]9;4;1;"));
        assert!(
            text.ends_with("\x1b]9;4;0;\x07"),
            "cleared on drop: {text:?}"
        );
    }

    #[test]
    fn durations_follow_the_style_guide() {
        assert_eq!(format_duration(Duration::from_millis(850)), "850ms");
        assert_eq!(format_duration(Duration::from_secs(12)), "12s");
        assert_eq!(format_duration(Duration::from_secs(185)), "3m 05s");
    }
}
