//! Progress and the live region for long operations (terminal-ui "Progress and
//! responsiveness"; UX §4.5). The engine reports task events; this draws them on stderr.
//!
//! - Animated (Basic/Rich tier, stderr is a TTY, no `DOTS_REDUCE_MOTION`): a live region with
//!   one spinner line per running task and a footer, redrawn at most 15 times a second.
//!   Finished tasks scroll up as permanent single lines. Rich adds synchronized output
//!   (mode 2026) and the terminal's own progress indicator (OSC 9;4).
//! - Otherwise (Plain, reduced motion, not a TTY): no spinner frames, one line per state change.
//!
//! Every line is written whole (one `write_all`, then a flush) under one lock, so tasks running
//! in parallel never interleave fragments. An optional log (`set_log`) receives every event
//! line too, without colors or cursor movement, so `tail -f` works from another terminal.

use std::io::Write;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
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
    /// Counts in the overall bar when it finishes (false for a phase note).
    counted: bool,
    /// When a plain-mode "still running" line was last printed for it.
    last_beat: Instant,
}

/// Rows of the live region and whether OSC 9;4 is set, for [`clear_on_exit`] (a second
/// Ctrl-C ends the process without running destructors).
static LIVE_ROWS: AtomicUsize = AtomicUsize::new(0);
static LIVE_OSC: AtomicBool = AtomicBool::new(false);

/// Wipe the live region and the terminal progress indicator of a stderr [`Progress`], and a
/// running phase spinner, right before the process exits without unwinding (second Ctrl-C).
pub fn clear_on_exit() {
    let rows = LIVE_ROWS.swap(0, Ordering::SeqCst);
    let mut out = String::new();
    if rows > 0 {
        out.push_str(&format!("\x1b[{rows}A\r\x1b[J"));
    }
    if LIVE_OSC.swap(false, Ordering::SeqCst) {
        out.push_str("\x1b]9;4;0;\x07");
    }
    if !out.is_empty() {
        let mut e = std::io::stderr();
        let _ = e.write_all(out.as_bytes());
        let _ = e.flush();
    }
    crate::phase::clear_on_exit();
}

/// How long a plain-mode step may run before a "still running" line, and how often after.
pub const HEARTBEAT: Duration = Duration::from_secs(30);

/// The overall bar of a run: `Applying  [█████░░░░░]  9/23 steps · 1 failed · 3m 12s`.
#[derive(Debug, Clone)]
pub struct Overall<'a> {
    pub title: &'a str,
    pub done: usize,
    pub total: usize,
    pub failed: usize,
    pub elapsed: Duration,
}

/// `[██████░░░░]`'s inside: `width` cells, full only when `done >= total`.
pub fn bar(done: usize, total: usize, width: usize, ascii: bool) -> String {
    let (on, off) = if ascii { ('#', '-') } else { ('█', '░') };
    let filled = if done >= total.max(1) {
        width
    } else {
        (done * width / total.max(1)).min(width.saturating_sub(1))
    };
    let mut s: String = std::iter::repeat_n(on, filled).collect();
    s.extend(std::iter::repeat_n(off, width - filled));
    s
}

impl Overall<'_> {
    const BAR: usize = 20;

    fn count(&self) -> String {
        let total = self.total.max(self.done);
        let unit = if total == 1 { "step" } else { "steps" };
        format!("{}/{total} {unit}", self.done)
    }

    /// One line of at most `max` columns. The bar shrinks, then goes, before anything is cut.
    /// `bar_style` and `failed_style` color their parts (identity for plain text).
    pub fn render(
        &self,
        max: usize,
        ascii: bool,
        bar_style: &dyn Fn(&str) -> String,
        failed_style: &dyn Fn(&str) -> String,
    ) -> String {
        let count = self.count();
        let failed = (self.failed > 0).then(|| format!("{} failed", self.failed));
        let clock = format_duration(self.elapsed);
        let counts_w =
            width(&count) + failed.as_ref().map_or(0, |f| 3 + width(f)) + 3 + width(&clock);
        let fixed = width(self.title) + 2 + counts_w;
        let mut bar_w = Self::BAR;
        while bar_w >= 6 && fixed + bar_w + 4 > max {
            bar_w -= 2;
        }
        let with_bar = bar_w >= 6;
        let total_w = fixed + if with_bar { bar_w + 4 } else { 0 };
        if total_w > max {
            let mut t = format!("{}  {count}", self.title);
            if let Some(f) = &failed {
                t.push_str(&format!(" · {f}"));
            }
            t.push_str(&format!(" · {clock}"));
            return truncate_end(&t, max, if ascii { "..." } else { "…" });
        }
        let mut out = self.title.to_string();
        if with_bar {
            let b = bar(self.done, self.total.max(self.done), bar_w, ascii);
            out.push_str(&format!("  {}", bar_style(&format!("[{b}]"))));
        }
        out.push_str(&format!("  {count}"));
        if let Some(f) = &failed {
            out.push_str(&format!(" · {}", failed_style(f)));
        }
        out.push_str(&format!(" · {clock}"));
        out
    }

    /// Unstyled (tests, logs).
    pub fn plain(&self, max: usize, ascii: bool) -> String {
        self.render(max, ascii, &|s| s.to_string(), &|s| s.to_string())
    }
}

/// An output line made safe for one terminal row: no escapes, tabs or control characters.
fn tidy_output(line: &str) -> String {
    strip_ansi(line)
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect::<String>()
        .trim()
        .to_string()
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
    /// Nothing is drawn between [`Progress::suspend`] and [`Progress::resume`].
    suspended: bool,
    /// Draws on the process's stderr (see [`clear_on_exit`]).
    is_stderr: bool,
    title: String,
    heartbeat: Option<Duration>,
    /// A second copy of every event line, plain text with the run's elapsed time in front.
    log: Option<Box<dyn Write + Send>>,
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
/// Running rows the live region shows at most (the rest is counted).
const MAX_ROWS: usize = 5;

impl Progress {
    /// Progress on stderr. Animates only when `ui.animate()`.
    pub fn stderr(ui: &Ui) -> Progress {
        let p = Progress::with_sink(ui, Box::new(std::io::stderr()), ui.animate(), true);
        lock(&p.inner).is_stderr = true;
        p
    }

    /// Progress that draws nothing (`--quiet`): only the log, when one is set, gets the lines.
    pub fn silent(ui: &Ui) -> Progress {
        Progress::with_sink(ui, Box::new(std::io::sink()), false, true)
    }

    /// Also write every event line to `log`: plain text, one line each, flushed at once.
    pub fn set_log(&self, log: Box<dyn Write + Send>) {
        lock(&self.inner).log = Some(log);
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
            suspended: false,
            is_stderr: false,
            title: "Applying".into(),
            heartbeat: Some(HEARTBEAT),
            log: None,
            frame: 0,
            started: Instant::now(),
            last_draw: None,
            last_osc: None,
            osc_active: false,
        }));
        // The ticker also runs without animation: it prints the plain "still running" lines.
        let ticker = ticker.then(|| {
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

    /// The word the overall bar starts with (`Applying`, the default).
    pub fn set_title(&self, title: &str) {
        lock(&self.inner).title = title.to_string();
    }

    /// Plain mode: a step running longer than `every` gets a `still running` line each `every`.
    /// `None` turns it off. Default [`HEARTBEAT`].
    pub fn set_heartbeat(&self, every: Option<Duration>) {
        lock(&self.inner).heartbeat = every;
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
        self.start_task(label, detail, true)
    }

    /// A running line that is not a step of the bar (the note of a phase that reports its
    /// steps one by one): end it with [`Progress::dismiss`].
    pub fn start_note(&self, label: &str) -> TaskId {
        self.start_task(label, "", false)
    }

    fn start_task(&self, label: &str, detail: &str, counted: bool) -> TaskId {
        let mut i = lock(&self.inner);
        let id = TaskId(i.next_id);
        i.next_id += 1;
        let task = Task {
            id,
            label: label.to_string(),
            detail: detail.to_string(),
            last_output: String::new(),
            started: Instant::now(),
            counted,
            last_beat: Instant::now(),
        };
        if !i.animate {
            let line = i.event_line(State::Running, label, detail, None);
            i.print_line(&line);
        } else {
            let line = i.event_line(State::Running, label, detail, None);
            i.log_line(&line);
        }
        i.running.push(task);
        i.draw(true);
        id
    }

    /// The tool's latest output line, shown dimmed next to the spinner (live region only).
    pub fn output(&self, id: TaskId, line: &str) {
        note_output(&self.inner, id, line);
    }

    /// [`Progress::output`] for `id` as a callback that owns what it needs, to hand to the
    /// thread that runs the step.
    pub fn output_sink(&self, id: TaskId) -> Arc<dyn Fn(&str) + Send + Sync> {
        let inner = self.inner.clone();
        Arc::new(move |line| note_output(&inner, id, line))
    }

    /// A task changed state for good: it leaves the live region as a permanent line.
    pub fn finish(&self, id: TaskId, state: State, detail: &str, elapsed: Option<Duration>) {
        self.finish_as(id, state, None, detail, elapsed);
    }

    /// Like [`Progress::finish`], and the permanent line says `label` instead of the running
    /// label (`Installing x (brew)` becomes `Installed x`).
    pub fn finish_as(
        &self,
        id: TaskId,
        state: State,
        label: Option<&str>,
        detail: &str,
        elapsed: Option<Duration>,
    ) {
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
        let line = i.event_line(state, label.unwrap_or(&task.label), &detail, Some(elapsed));
        i.print_line(&line);
        i.draw(true);
    }

    /// A task that is over without a line of its own (a phase note).
    pub fn dismiss(&self, id: TaskId) {
        let mut i = lock(&self.inner);
        i.running.retain(|t| t.id != id);
        i.draw(true);
    }

    /// A step that is over by the time anyone hears of it: one permanent line, no start line.
    pub fn record(&self, label: &str, state: State, detail: &str, elapsed: Option<Duration>) {
        let mut i = lock(&self.inner);
        i.done += 1;
        if matches!(state, State::Failed | State::Missing | State::Conflict) {
            i.failed += 1;
        }
        let detail = if matches!(state, State::Ok | State::Done) || detail.is_empty() {
            if matches!(state, State::Ok | State::Done) {
                detail.to_string()
            } else {
                state.word().to_string()
            }
        } else {
            format!("{}: {detail}", state.word())
        };
        let line = i.event_line(state, label, &detail, elapsed);
        i.print_line(&line);
        i.draw(true);
    }

    /// Print a permanent line above the live region.
    pub fn println(&self, line: &str) {
        lock(&self.inner).print_line(line);
    }

    /// Clear the live region (before a sudo prompt or an interactive task).
    pub fn suspend(&self) {
        let mut i = lock(&self.inner);
        i.suspended = true;
        i.clear_live();
    }

    /// Draw the live region again after [`Progress::suspend`].
    pub fn resume(&self) {
        let mut i = lock(&self.inner);
        i.suspended = false;
        i.draw(true);
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

fn note_output(inner: &Mutex<Inner>, id: TaskId, line: &str) {
    let mut i = lock(inner);
    if let Some(t) = i.running.iter_mut().find(|t| t.id == id) {
        t.last_output = tidy_output(line);
    }
    i.draw(false);
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

/// `mm:ss` since the run started (`hh:mm:ss` past an hour), the log's line prefix.
fn format_clock(d: Duration) -> String {
    let s = d.as_secs();
    if s >= 3600 {
        format!("{:02}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
    } else {
        format!("{:02}:{:02}", s / 60, s % 60)
    }
}

/// `text` without escape sequences (colors, cursor movement, OSC).
pub(crate) fn strip_ansi(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\x1b' {
            out.push(c);
            continue;
        }
        match chars.next() {
            // CSI: parameters, then a final byte in @..~
            Some('[') => {
                for n in chars.by_ref() {
                    if ('@'..='~').contains(&n) {
                        break;
                    }
                }
            }
            // OSC: up to BEL or ST
            Some(']') => {
                while let Some(n) = chars.next() {
                    if n == '\x07' || (n == '\x1b' && chars.next() == Some('\\')) {
                        break;
                    }
                }
            }
            _ => {}
        }
    }
    out
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

    /// One whole line, in one write, so nothing else can land in the middle of it.
    fn print_line(&mut self, line: &str) {
        self.clear_live();
        let mut buf = String::with_capacity(line.len() + 1);
        buf.push_str(line);
        buf.push('\n');
        let _ = self.sink.write_all(buf.as_bytes());
        let _ = self.sink.flush();
        self.log_line(line);
    }

    /// The line, without styling, in the log.
    fn log_line(&mut self, line: &str) {
        let stamp = format_clock(self.started.elapsed());
        let Some(log) = self.log.as_mut() else { return };
        let text = format!("{stamp} {}\n", strip_ansi(line).trim_start());
        let _ = log.write_all(text.as_bytes());
        let _ = log.flush();
    }

    fn clear_live(&mut self) {
        if self.live_lines > 0 {
            let _ = write!(self.sink, "\x1b[{}A\r\x1b[J", self.live_lines);
            self.set_rows(0);
            let _ = self.sink.flush();
        }
    }

    fn set_rows(&mut self, n: usize) {
        self.live_lines = n;
        if self.is_stderr {
            LIVE_ROWS.store(n, Ordering::SeqCst);
        }
    }

    fn tick(&mut self) {
        self.frame += 1;
        self.heartbeat_lines();
        self.draw(false);
    }

    /// Plain mode has no spinner: a long step says it is still running now and then.
    fn heartbeat_lines(&mut self) {
        let Some(every) = self.heartbeat.filter(|_| !self.animate) else {
            return;
        };
        let mut lines = Vec::new();
        for t in self.running.iter_mut().filter(|t| t.counted) {
            if t.started.elapsed() >= every && t.last_beat.elapsed() >= every {
                t.last_beat = Instant::now();
                lines.push((t.label.clone(), t.started.elapsed()));
            }
        }
        for (label, elapsed) in lines {
            let sym = state_symbol(self.ui.icons, State::Running);
            let line = format!(
                " {} still running: {label} ({})",
                self.p(Some(State::Running.tone()), false, false, sym),
                format_duration(elapsed)
            );
            self.print_line(&line);
        }
    }

    /// A running step: `⠹ Installing plantuml (brew)  1m 04s  ==> Pouring deno...`, never
    /// wider than `w` columns.
    fn running_row(&self, t: &Task, sp: &str, w: usize) -> String {
        let ell = self.ui.ellipsis();
        let elapsed = format_duration(t.started.elapsed());
        let tail_w = 2 + width(&elapsed);
        let head = if t.detail.is_empty() {
            t.label.clone()
        } else {
            format!("{}  {}", t.label, t.detail)
        };
        let head = truncate_end(&head, w.saturating_sub(3 + tail_w), ell);
        let room = w.saturating_sub(3 + width(&head) + tail_w + 2);
        let out = if t.last_output.is_empty() || room < 8 {
            String::new()
        } else {
            format!("  {}", truncate_end(&t.last_output, room, ell))
        };
        format!(
            " {} {}  {}{}",
            self.p(Some(Tone::Info), false, false, sp),
            head,
            self.p(Some(Tone::Muted), false, false, &elapsed),
            self.p(Some(Tone::Muted), false, true, &out),
        )
    }

    fn live_lines(&self) -> Vec<String> {
        // Never the last column: a full row would wrap on some terminals.
        let w = self.ui.width.saturating_sub(1).max(20);
        let sp = SPINNER[self.frame % SPINNER.len()];
        let mut lines = Vec::new();
        for t in self.running.iter().take(MAX_ROWS) {
            lines.push(self.running_row(t, sp, w));
        }
        if self.running.len() > MAX_ROWS {
            let more = format!("   {} more running", self.running.len() - MAX_ROWS);
            lines.push(self.p(Some(Tone::Muted), false, false, &more));
        }
        if self.total > 0 {
            let o = Overall {
                title: &self.title,
                done: self.done,
                total: self.total,
                failed: self.failed,
                elapsed: self.started.elapsed(),
            };
            let ascii = self.ui.icons == IconSet::Ascii;
            let line = o.render(
                w.saturating_sub(1),
                ascii,
                &|s| self.p(Some(Tone::Info), false, false, s),
                &|s| self.p(Some(Tone::Err), false, false, s),
            );
            lines.push(format!(" {line}"));
        }
        lines
    }

    fn draw(&mut self, force: bool) {
        if !self.animate || self.suspended {
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
        // The bar stays between steps too, until the last one is done.
        let show = !self.running.is_empty() || (self.total > 0 && self.done < self.total);
        let lines = if show { self.live_lines() } else { Vec::new() };
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
                LIVE_OSC.store(self.is_stderr, Ordering::SeqCst);
            }
        }
        self.set_rows(lines.len());
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
            LIVE_OSC.store(false, Ordering::SeqCst);
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
        assert!(text.contains("1/2 steps"));
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
    fn the_log_gets_every_event_as_plain_text_even_when_the_screen_animates() {
        let ui = Ui::fixed(Tier::Basic, 80);
        let (screen, log) = (Buf::default(), Buf::default());
        {
            let p = Progress::manual(&ui, Box::new(screen.clone()), true);
            p.set_log(Box::new(log.clone()));
            let a = p.start("ripgrep", "brew install");
            p.finish(a, State::Failed, "boom", Some(Duration::from_secs(2)));
            p.record(
                "fd",
                State::Done,
                "installed",
                Some(Duration::from_millis(50)),
            );
        }
        let text = log.text();
        assert!(!text.contains('\x1b'), "{text:?}");
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 3, "{text:?}");
        assert!(lines[0].starts_with("00:00 ") && lines[0].contains("ripgrep"));
        assert!(lines[1].contains("failed: boom") && lines[1].contains("2.0s"));
        assert!(lines[2].contains("fd") && lines[2].contains("installed"));
    }

    #[test]
    fn parallel_tasks_write_whole_lines() {
        let ui = Ui::fixed(Tier::Plain, 80);
        let (screen, log) = (Buf::default(), Buf::default());
        let p = Arc::new(Progress::manual(&ui, Box::new(screen.clone()), false));
        p.set_log(Box::new(log.clone()));
        let threads: Vec<_> = (0..8)
            .map(|t| {
                let p = p.clone();
                std::thread::spawn(move || {
                    for n in 0..50 {
                        let id = p.start(&format!("pkg-{t}-{n}"), "brew install");
                        p.finish(id, State::Ok, "installed", None);
                    }
                })
            })
            .collect();
        for t in threads {
            t.join().unwrap();
        }
        for text in [screen.text(), log.text()] {
            let lines: Vec<&str> = text.lines().collect();
            assert_eq!(lines.len(), 8 * 50 * 2);
            for l in lines {
                assert!(
                    l.contains("pkg-") && (l.contains("brew install") || l.contains("installed")),
                    "fragment: {l:?}"
                );
            }
        }
    }

    #[test]
    fn silent_draws_nothing_but_still_logs() {
        let ui = Ui::fixed(Tier::Plain, 80);
        let log = Buf::default();
        let p = Progress::silent(&ui);
        p.set_log(Box::new(log.clone()));
        p.println("run log: x");
        assert_eq!(log.text(), "00:00 run log: x\n");
    }

    #[test]
    fn strips_styling_from_log_lines() {
        assert_eq!(strip_ansi("\x1b[32m✓\x1b[0m ok\x1b]9;4;0;\x07!"), "✓ ok!");
    }

    #[test]
    fn durations_follow_the_style_guide() {
        assert_eq!(format_duration(Duration::from_millis(850)), "850ms");
        assert_eq!(format_duration(Duration::from_secs(12)), "12s");
        assert_eq!(format_duration(Duration::from_secs(185)), "3m 05s");
    }

    fn overall(done: usize, total: usize, failed: usize, secs: u64) -> Overall<'static> {
        Overall {
            title: "Applying",
            done,
            total,
            failed,
            elapsed: Duration::from_secs(secs),
        }
    }

    #[test]
    fn the_bar_has_a_fixed_width_and_is_full_only_when_done() {
        for (done, total) in [(0, 23), (1, 23), (9, 23), (22, 23), (23, 23), (5, 0)] {
            assert_eq!(width(&bar(done, total, 20, false)), 20, "{done}/{total}");
            assert_eq!(bar(done, total, 20, true).len(), 20);
        }
        assert_eq!(bar(9, 23, 20, true), "#######-------------");
        assert_eq!(bar(0, 23, 10, true), "----------");
        assert_eq!(bar(23, 23, 10, true), "##########");
        assert_eq!(
            bar(22, 23, 10, true),
            "#########-",
            "never full before the end"
        );
        assert_eq!(bar(9, 23, 8, false), "███░░░░░");
    }

    #[test]
    fn the_overall_line_has_counts_failures_and_time() {
        assert_eq!(
            overall(9, 23, 0, 192).plain(80, false),
            "Applying  [███████░░░░░░░░░░░░░]  9/23 steps · 3m 12s"
        );
        assert_eq!(
            overall(9, 23, 1, 192).plain(80, true),
            "Applying  [#######-------------]  9/23 steps · 1 failed · 3m 12s"
        );
        assert_eq!(
            overall(1, 1, 0, 4).plain(80, true),
            "Applying  [####################]  1/1 step · 4.0s"
        );
        // The styled parts are the bar and the failures, nothing else.
        let s = overall(2, 4, 1, 5).render(80, true, &|b| format!("<{b}>"), &|f| format!("!{f}!"));
        assert!(
            s.contains("<[##########----------]>") && s.contains("!1 failed!"),
            "{s}"
        );
    }

    #[test]
    fn the_overall_line_never_exceeds_the_width() {
        for max in [10, 20, 30, 40, 50, 60, 80] {
            for o in [overall(9, 23, 0, 192), overall(120, 300, 12, 4000)] {
                for ascii in [false, true] {
                    let l = o.plain(max, ascii);
                    assert!(width(&l) <= max, "{max}: {l:?} ({})", width(&l));
                }
            }
        }
        // Narrow: the bar goes before the counts are cut.
        assert_eq!(
            overall(9, 23, 0, 192).plain(30, true),
            "Applying  9/23 steps · 3m 12s"
        );
    }

    #[test]
    fn a_running_step_shows_elapsed_time_and_its_last_output_cut_to_the_width() {
        let ui = Ui::fixed(Tier::Basic, 60);
        let buf = Buf::default();
        {
            let p = Progress::manual(&ui, Box::new(buf.clone()), true);
            p.set_total(3);
            let a = p.start("Installing plantuml, deno, zsh (brew)", "");
            p.output(
                a,
                "\x1b[32m==>\x1b[0m Pouring deno--2.9.8.arm64_sonoma.bottle.tar.gz\r",
            );
            p.suspend();
            p.resume();
        }
        let t = buf.text();
        let last = t
            .split("\x1b[J")
            .filter(|s| s.contains("Installing"))
            .last()
            .unwrap();
        let rows: Vec<String> = last
            .lines()
            .map(strip_ansi)
            .filter(|l| l.contains("Installing"))
            .collect();
        let row = rows.last().expect("a running row");
        assert!(
            row.contains("Installing plantuml, deno, zsh (brew)"),
            "{row:?}"
        );
        assert!(row.contains("==> Pouring"), "{row:?}");
        assert!(!row.contains('\r'));
        assert!(width(row) <= 59, "{row:?}");
        assert!(t.contains("0/3 steps"), "the bar is drawn from the start");
    }

    #[test]
    fn the_bar_is_not_drawn_while_suspended_and_comes_back_on_resume() {
        let ui = Ui::fixed(Tier::Basic, 80);
        let buf = Buf::default();
        let p = Progress::manual(&ui, Box::new(buf.clone()), true);
        p.set_total(2);
        p.suspend();
        let before = buf.text().len();
        p.tick();
        p.set_total(3);
        assert_eq!(buf.text().len(), before, "nothing drawn while suspended");
        p.resume();
        assert!(buf.text().len() > before);
    }

    #[test]
    fn plain_mode_says_still_running_for_a_long_step_and_logs_it() {
        let ui = Ui::fixed(Tier::Plain, 80);
        let (screen, log) = (Buf::default(), Buf::default());
        let p = Progress::manual(&ui, Box::new(screen.clone()), false);
        p.set_log(Box::new(log.clone()));
        p.set_heartbeat(Some(Duration::from_millis(40)));
        let a = p.start("Installing kv (cargo)", "");
        p.tick();
        assert!(
            !screen.text().contains("still running"),
            "not before the interval"
        );
        std::thread::sleep(Duration::from_millis(60));
        p.tick();
        p.tick();
        std::thread::sleep(Duration::from_millis(60));
        p.tick();
        p.finish(a, State::Ok, "", None);
        for text in [screen.text(), log.text()] {
            let beats: Vec<&str> = text
                .lines()
                .filter(|l| l.contains("still running"))
                .collect();
            assert_eq!(beats.len(), 2, "once per interval: {text:?}");
            assert!(
                beats[0].contains("[..] still running: Installing kv (cargo) ("),
                "{text:?}"
            );
            assert!(!text.contains('\x1b'));
        }
    }

    #[test]
    fn the_heartbeat_can_be_off_and_skips_phase_notes() {
        let ui = Ui::fixed(Tier::Plain, 80);
        let screen = Buf::default();
        let p = Progress::manual(&ui, Box::new(screen.clone()), false);
        p.set_heartbeat(Some(Duration::from_millis(10)));
        let n = p.start_note("Applying 3 settings");
        std::thread::sleep(Duration::from_millis(30));
        p.tick();
        p.dismiss(n);
        assert!(
            !screen.text().contains("still running"),
            "{:?}",
            screen.text()
        );
        p.set_heartbeat(None);
        let _a = p.start("x", "");
        std::thread::sleep(Duration::from_millis(30));
        p.tick();
        assert!(!screen.text().contains("still running"));
    }

    #[test]
    fn a_finished_line_can_use_its_own_wording() {
        let ui = Ui::fixed(Tier::Plain, 80);
        let screen = Buf::default();
        let p = Progress::manual(&ui, Box::new(screen.clone()), false);
        let a = p.start("Installing deno (brew)", "");
        p.finish_as(
            a,
            State::Done,
            Some("Installed deno"),
            "",
            Some(Duration::from_secs(43)),
        );
        assert_eq!(
            screen.text(),
            " [..] Installing deno (brew)\n [x] Installed deno  43s\n"
        );
    }
}
