//! `<state>/journal.jsonl`: append-only, one JSON object per action, each line one `write`.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

/// The per-run facts every journal line repeats.
#[derive(Debug, Clone, Default)]
pub struct JournalMeta {
    pub host: String,
    pub root: String,
    pub repo_rev: Option<String>,
    pub repo_dirty: bool,
    pub upstream_rev: Option<String>,
    pub dots_version: String,
}

/// One action's record.
#[derive(Debug, Clone, Default)]
pub struct Entry {
    pub op: String,
    pub pkg: String,
    pub provider: Option<String>,
    pub from: Option<String>,
    pub to: Option<String>,
    pub exit: i32,
    pub dur_ms: u64,
    pub log: Option<String>,
}

pub struct Journal {
    path: PathBuf,
    run: String,
    meta: JournalMeta,
}

impl Journal {
    pub fn new(path: &Path, run: &str, meta: JournalMeta) -> Self {
        Journal {
            path: path.to_path_buf(),
            run: run.to_string(),
            meta,
        }
    }

    pub fn line(&self, e: &Entry) -> Value {
        json!({
            "ts": rfc3339(SystemTime::now()),
            "run": self.run,
            "host": self.meta.host,
            "op": e.op,
            "pkg": e.pkg,
            "provider": e.provider,
            "from": e.from,
            "to": e.to,
            "root": self.meta.root,
            "repo_rev": self.meta.repo_rev,
            "repo_dirty": self.meta.repo_dirty,
            "upstream_rev": self.meta.upstream_rev,
            "dots": self.meta.dots_version,
            "exit": e.exit,
            "dur_ms": e.dur_ms,
            "log": e.log,
        })
    }

    /// Append one line with a single `write` on an `O_APPEND` file.
    pub fn append(&self, e: &Entry) -> std::io::Result<()> {
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let mut text = serde_json::to_string(&self.line(e)).map_err(std::io::Error::other)?;
        text.push('\n');
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        f.write_all(text.as_bytes())
    }
}

/// `2026-11-25T20:11:03Z`.
pub fn rfc3339(t: SystemTime) -> String {
    let secs = t.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs()) as i64;
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    // Civil-from-days (Howard Hinnant).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

/// A fresh run id (`r-` and 8 hex digits).
pub fn new_run_id() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.subsec_nanos() as u64 ^ d.as_secs().rotate_left(20));
    let mixed = nanos ^ (u64::from(std::process::id()) << 16);
    format!("r-{:08x}", mixed as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_time() {
        assert_eq!(rfc3339(UNIX_EPOCH), "1970-01-01T00:00:00Z");
        assert_eq!(
            rfc3339(UNIX_EPOCH + std::time::Duration::from_secs(1_784_000_000)),
            "2026-07-14T03:33:20Z"
        );
    }
}
