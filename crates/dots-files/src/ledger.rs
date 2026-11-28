//! Ownership records, kept in `<state>/files.json` (separate from the engine's `state.json`).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::fsops::{LinkKind, atomic_write};
use crate::model::Mode;

/// What dots wrote for one destination.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Record {
    pub dst: PathBuf,
    pub package: String,
    pub mode: Mode,
    pub root_id: String,
    pub run_id: String,
    /// Hash of the rendered or copied content (the inject block body for `inject`).
    pub sha256: Option<String>,
    pub link_target: Option<PathBuf>,
    pub gen_path: Option<PathBuf>,
    /// Directories dots created for this destination, outermost first.
    #[serde(default)]
    pub created_parents: Vec<PathBuf>,
    /// For `inject` and `dir`: dots created the file or directory itself.
    #[serde(default)]
    pub created: bool,
    pub marker: Option<String>,
    pub comment: Option<String>,
    /// What `link`/`render` created at `dst`: a symlink, or on Windows without the privilege a
    /// hardlink (files) or junction (directories). Absent in older ledgers: a symlink.
    #[serde(default, skip_serializing_if = "LinkKind::is_symlink")]
    pub link_kind: LinkKind,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Ledger {
    #[serde(default)]
    pub schema: u32,
    #[serde(default)]
    pub owned: BTreeMap<String, Record>,
}

/// Ledger key: the destination, plus the package for `inject` (several may share one file).
pub fn key(dst: &Path, mode: Mode, package: &str) -> String {
    match mode {
        Mode::Inject => format!("{}::inject::{package}", dst.display()),
        _ => dst.display().to_string(),
    }
}

impl Ledger {
    pub fn path(state: &Path) -> PathBuf {
        state.join("files.json")
    }
    pub fn load(state: &Path) -> Ledger {
        std::fs::read(Self::path(state))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or(Ledger {
                schema: 1,
                owned: BTreeMap::new(),
            })
    }
    pub fn save(&self, state: &Path) -> std::io::Result<()> {
        std::fs::create_dir_all(state)?;
        let mut s = serde_json::to_string_pretty(self).unwrap_or_default();
        s.push('\n');
        atomic_write(&Self::path(state), s.as_bytes())
    }
    pub fn get(&self, dst: &Path, mode: Mode, package: &str) -> Option<&Record> {
        self.owned.get(&key(dst, mode, package))
    }
}
