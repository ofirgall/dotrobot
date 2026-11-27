//! A transient one-line indicator for the phases a command goes through before it has
//! anything to show (`dots plan` asking brew, cargo and the checks what is installed):
//! `⠼ Checking installed packages (brew, cargo)  4s`.
//!
//! Drawn on stderr only, only when stderr is a terminal and motion is allowed (the same rule
//! as the live region of [`crate::Progress`]); otherwise it does nothing, so pipes, `--json`
//! consumers and CI see no extra bytes. It never draws in its first 200 ms, so a fast command
//! does not flash it, and it erases itself when finished, when dropped and on Ctrl-C.
//!
//! One indicator is active per process: [`start`] makes it, [`note`] changes its wording from
//! anywhere (the provider code does not have to know about the UI), [`finish`] ends it.

use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use dots_core::Tone;

use crate::layout::{truncate_end, width};
use crate::progress::format_duration;
use crate::theme::{SPINNER, paint};
use crate::ui::Ui;

const SHOW_AFTER: Duration = Duration::from_millis(200);
const FRAME: Duration = Duration::from_millis(80);

struct Shared {
    label: Mutex<String>,
    started: Instant,
    stop: AtomicBool,
    /// A line is on screen (so it must be erased).
    drawn: AtomicBool,
}

/// A running indicator.
pub struct Spin {
    shared: Arc<Shared>,
    sink: Arc<Mutex<Box<dyn Write + Send>>>,
    handle: Option<JoinHandle<()>>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// The line for `label` after `elapsed`, at most `max` columns.
fn line(ui: &Ui, frame: usize, label: &str, elapsed: Duration) -> String {
    let secs = if elapsed >= Duration::from_secs(1) {
        format!(
            "  {}",
            format_duration(Duration::from_secs(elapsed.as_secs()))
        )
    } else {
        String::new()
    };
    let max = ui.width.saturating_sub(1).max(20);
    let label = truncate_end(label, max.saturating_sub(3 + width(&secs)), ui.ellipsis());
    format!(
        " {} {label}{}",
        paint(
            ui,
            Some(Tone::Info),
            false,
            false,
            SPINNER[frame % SPINNER.len()]
        ),
        paint(ui, Some(Tone::Muted), false, false, &secs)
    )
}

impl Spin {
    /// Draws on `sink` from a background thread, whatever the terminal (callers decide).
    pub fn start(ui: &Ui, label: &str, sink: Box<dyn Write + Send>) -> Spin {
        let shared = Arc::new(Shared {
            label: Mutex::new(label.to_string()),
            started: Instant::now(),
            stop: AtomicBool::new(false),
            drawn: AtomicBool::new(false),
        });
        let sink = Arc::new(Mutex::new(sink));
        let (sh, out, ui) = (shared.clone(), sink.clone(), ui.clone());
        let handle = std::thread::spawn(move || {
            let mut frame = 0usize;
            while !sh.stop.load(Ordering::SeqCst) {
                std::thread::sleep(FRAME);
                let elapsed = sh.started.elapsed();
                if elapsed < SHOW_AFTER || sh.stop.load(Ordering::SeqCst) {
                    continue;
                }
                frame += 1;
                let text = line(&ui, frame, &lock(&sh.label), elapsed);
                let mut o = lock(&out);
                let _ = write!(o, "\r\x1b[2K{text}");
                let _ = o.flush();
                sh.drawn.store(true, Ordering::SeqCst);
            }
        });
        Spin {
            shared,
            sink,
            handle: Some(handle),
        }
    }

    pub fn set_label(&self, label: &str) {
        *lock(&self.shared.label) = label.to_string();
    }

    /// Stop drawing and erase the line.
    pub fn finish(mut self) {
        self.end();
    }

    fn end(&mut self) {
        self.shared.stop.store(true, Ordering::SeqCst);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
        if self.shared.drawn.swap(false, Ordering::SeqCst) {
            let mut o = lock(&self.sink);
            let _ = o.write_all(b"\r\x1b[2K");
            let _ = o.flush();
        }
    }
}

impl Drop for Spin {
    fn drop(&mut self) {
        self.end();
    }
}

static ACTIVE: Mutex<Option<Spin>> = Mutex::new(None);

/// Ends the process's indicator when dropped, so every way out of a command (error return,
/// early exit, panic) erases it.
pub struct PhaseGuard;

impl Drop for PhaseGuard {
    fn drop(&mut self) {
        finish();
    }
}

/// Show `label` on stderr while the command works, if stderr is a terminal and motion is
/// allowed. Replaces an indicator that is already running.
pub fn start(ui: &Ui, label: &str) -> PhaseGuard {
    finish();
    if ui.animate() {
        *lock(&ACTIVE) = Some(Spin::start(ui, label, Box::new(std::io::stderr())));
    }
    PhaseGuard
}

/// Change the wording of the running indicator (no-op when there is none).
pub fn note(label: &str) {
    if let Some(s) = lock(&ACTIVE).as_ref() {
        s.set_label(label);
    }
}

/// End the indicator and erase its line (no-op when there is none).
pub fn finish() {
    let spin = lock(&ACTIVE).take();
    drop(spin);
}

/// Erase the line right before the process exits without unwinding (Ctrl-C).
pub(crate) fn clear_on_exit() {
    let Ok(mut g) = ACTIVE.try_lock() else { return };
    if let Some(s) = g.as_ref()
        && s.shared.drawn.swap(false, Ordering::SeqCst)
    {
        let mut e = std::io::stderr();
        let _ = e.write_all(b"\r\x1b[2K");
        let _ = e.flush();
    }
    *g = None;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Tier;

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

    #[test]
    fn a_fast_phase_draws_nothing() {
        let ui = Ui::fixed(Tier::Basic, 80);
        let buf = Buf::default();
        let s = Spin::start(&ui, "Checking", Box::new(buf.clone()));
        s.finish();
        assert_eq!(buf.text(), "");
    }

    #[test]
    fn a_slow_phase_shows_its_latest_label_and_erases_itself() {
        let ui = Ui::fixed(Tier::Basic, 80);
        let buf = Buf::default();
        let s = Spin::start(&ui, "Checking installed packages", Box::new(buf.clone()));
        std::thread::sleep(Duration::from_millis(400));
        s.set_label("Looking up latest versions (brew: 41 packages)");
        std::thread::sleep(Duration::from_millis(300));
        s.finish();
        let t = buf.text();
        assert!(t.contains("Checking installed packages"), "{t:?}");
        assert!(t.contains("Looking up latest versions (brew: 41 packages)"));
        assert!(t.ends_with("\r\x1b[2K"), "erased at the end: {t:?}");
        assert!(
            !t.contains('\n'),
            "one transient line, never a permanent one"
        );
    }

    #[test]
    fn the_line_fits_a_narrow_terminal() {
        let ui = Ui::fixed(Tier::Basic, 30);
        let l = line(
            &ui,
            0,
            "Looking up latest versions (brew: 41 packages)",
            Duration::from_secs(75),
        );
        assert!(width(&crate::progress::strip_ansi(&l)) <= 29, "{l:?}");
    }

    #[test]
    fn nothing_runs_off_a_terminal() {
        // `Ui::fixed` is not a stderr terminal: no indicator, and `note` is harmless.
        let ui = Ui::fixed(Tier::Plain, 80);
        let _g = start(&ui, "Checking");
        note("Still checking");
        assert!(ACTIVE.lock().unwrap().is_none());
    }
}
