//! The run lock: `<state>/run.lock`, an advisory exclusive file lock (released by the OS if the
//! holder dies) with the holder's pid written inside so the loser can name it.

use std::fs::{File, OpenOptions, TryLockError};
use std::io::{Read, Seek, Write};
use std::path::Path;

use dots_core::Diagnostic;

/// Holding this means no other mutating dots run is active for the state dir.
#[derive(Debug)]
pub struct RunLock {
    _file: File,
}

/// `run.lock.pid`, next to the lock file (Windows only; see `acquire`).
#[cfg(windows)]
fn pid_file(lock: &Path) -> std::path::PathBuf {
    let mut p = lock.as_os_str().to_owned();
    p.push(".pid");
    p.into()
}

impl RunLock {
    /// Take the lock, or return the `lock-busy` diagnostic naming the holder's pid.
    pub fn acquire(path: &Path) -> Result<RunLock, Diagnostic> {
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)
            .map_err(|e| {
                Diagnostic::from_code(
                    "write-failed",
                    &[
                        ("path", &path.display().to_string()),
                        ("detail", &e.to_string()),
                    ],
                )
            })?;
        // A child that another thread is starting holds a copy of this file's descriptor
        // between fork and exec (close-on-exec only closes it at exec), so a lock just released
        // can read as held for a moment. Retry briefly before calling it busy.
        let mut tries = 0;
        let locked = loop {
            match file.try_lock() {
                Err(TryLockError::WouldBlock) if tries < 25 => {
                    tries += 1;
                    std::thread::sleep(std::time::Duration::from_millis(20));
                }
                other => break other,
            }
        };
        match locked {
            Ok(()) => {
                let _ = file.set_len(0);
                let _ = file.rewind();
                let _ = write!(file, "{}", std::process::id());
                let _ = file.flush();
                #[cfg(windows)]
                let _ = std::fs::write(pid_file(path), std::process::id().to_string());
                Ok(RunLock { _file: file })
            }
            Err(TryLockError::WouldBlock) => {
                let mut text = String::new();
                let _ = file.read_to_string(&mut text);
                // A Windows lock is mandatory: the holder's lock covers the whole file, so
                // the pid cannot be read from it. The holder also wrote it beside the lock.
                #[cfg(windows)]
                if text.trim().is_empty() {
                    text = std::fs::read_to_string(pid_file(path)).unwrap_or_default();
                }
                let pid = text.trim();
                let pid = if pid.is_empty() { "unknown" } else { pid };
                Err(Diagnostic::from_code("lock-busy", &[("pid", pid)]))
            }
            Err(TryLockError::Error(e)) => Err(Diagnostic::from_code(
                "write-failed",
                &[
                    ("path", &path.display().to_string()),
                    ("detail", &e.to_string()),
                ],
            )),
        }
    }
}
