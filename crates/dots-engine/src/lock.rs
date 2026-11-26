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
        match file.try_lock() {
            Ok(()) => {
                let _ = file.set_len(0);
                let _ = file.rewind();
                let _ = write!(file, "{}", std::process::id());
                let _ = file.flush();
                Ok(RunLock { _file: file })
            }
            Err(TryLockError::WouldBlock) => {
                let mut text = String::new();
                let _ = file.read_to_string(&mut text);
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
