//! The download seam. Providers that fetch a file (an apt repository key, a script
//! `fetch` installer) go through [`Fetcher`] on `Ctx::fetcher`, so tests serve canned bytes and
//! never touch the network. The real implementation shells out to `curl`.

use std::fmt;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use crate::{ProviderError, Result};

pub trait Fetcher: Send + Sync + fmt::Debug {
    /// The body at `url` (http or https only).
    fn fetch(&self, url: &str) -> Result<Vec<u8>>;
}

/// Downloads with `curl -fsSL`, found in its known locations (never via `PATH`).
#[derive(Debug, Default, Clone, Copy)]
pub struct CurlFetcher;

const CURL: &[&str] = &[
    "/usr/bin/curl",
    "/bin/curl",
    "/opt/homebrew/bin/curl",
    "/usr/local/bin/curl",
];

/// Only plain web urls: no `file://`, no option-looking strings.
pub fn valid_url(url: &str) -> bool {
    (url.starts_with("https://") || url.starts_with("http://"))
        && !url.chars().any(|c| c.is_whitespace() || c.is_control())
}

impl Fetcher for CurlFetcher {
    fn fetch(&self, url: &str) -> Result<Vec<u8>> {
        if !valid_url(url) {
            return Err(ProviderError::Other(format!("refusing to fetch `{url}`")));
        }
        // `curl.exe` ships with Windows 10 and later, in System32.
        #[cfg(windows)]
        let system32 = crate::cargo::windows_dirs(&["SystemRoot"], r"System32\curl");
        #[cfg(not(windows))]
        let system32: Vec<PathBuf> = Vec::new();
        let curl = CURL
            .iter()
            .map(PathBuf::from)
            .chain(system32)
            .find_map(|p| crate::cargo::first_file([p]))
            .ok_or_else(|| ProviderError::Unavailable {
                provider: "fetch".into(),
                reason: "curl not found".into(),
            })?;
        let out = Command::new(curl)
            .args([
                "-fsSL",
                "--max-time",
                "300",
                "--proto",
                "=https,http",
                "--",
                url,
            ])
            .stdin(Stdio::null())
            .output()
            .map_err(|e| ProviderError::Other(format!("cannot run curl: {e}")))?;
        if !out.status.success() {
            return Err(ProviderError::Other(format!(
                "download of {url} failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            )));
        }
        Ok(out.stdout)
    }
}
