//! The ownership ledger for settings: `<state>/settings.json`, a sibling of dots-files'
//! `files.json`. It records the original value of every key dots changed (or that it did not
//! exist), and a per-run history so `dots restore <run-id>` can undo a run.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::model::Setting;
use crate::sys::Cur;

pub const LEDGER_SCHEMA: u32 = 1;
/// Runs kept in the history.
const KEEP_RUNS: usize = 50;

/// A value that may not have existed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Orig {
    pub exists: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<Value>,
}

impl Orig {
    pub fn absent() -> Orig {
        Orig {
            exists: false,
            value: None,
        }
    }

    pub fn of(v: Value) -> Orig {
        Orig {
            exists: true,
            value: Some(v),
        }
    }

    pub fn from_cur(c: &Cur) -> Orig {
        match c {
            Cur::Absent => Orig::absent(),
            Cur::Value(v) => Orig::of(v.clone()),
        }
    }

    pub fn as_cur(&self) -> Cur {
        match &self.value {
            Some(v) if self.exists => Cur::Value(v.clone()),
            _ => Cur::Absent,
        }
    }
}

/// A key dots changed: who owns it and what it was before dots first touched it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Owned {
    pub package: String,
    pub setting: Setting,
    pub original: Orig,
    pub run: String,
}

/// One change made by a run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Change {
    pub run: String,
    pub id: String,
    pub package: String,
    pub setting: Setting,
    pub before: Orig,
    pub after: Orig,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Ledger {
    pub schema: u32,
    #[serde(default)]
    pub owned: BTreeMap<String, Owned>,
    #[serde(default)]
    pub history: Vec<Change>,
    #[serde(skip)]
    path: PathBuf,
}

impl Ledger {
    pub fn path(state: &Path) -> PathBuf {
        state.join("settings.json")
    }

    /// A missing or unreadable ledger is an empty one.
    pub fn load(state: &Path) -> Ledger {
        let path = Self::path(state);
        let mut l: Ledger = std::fs::read_to_string(&path)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or(Ledger {
                schema: LEDGER_SCHEMA,
                owned: BTreeMap::new(),
                history: Vec::new(),
                path: PathBuf::new(),
            });
        l.path = path;
        l
    }

    pub fn save(&self) -> Result<(), String> {
        if let Some(d) = self.path.parent() {
            std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
        }
        let tmp = self.path.with_extension("json.tmp");
        let mut text = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        text.push('\n');
        std::fs::write(&tmp, text).map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, &self.path).map_err(|e| e.to_string())
    }

    /// Record a change. The first change of a key records its original value; later ones keep it.
    pub fn record(
        &mut self,
        run: &str,
        package: &str,
        setting: &Setting,
        before: Orig,
        after: Orig,
    ) {
        let id = setting.id();
        self.owned.entry(id.clone()).or_insert_with(|| Owned {
            package: package.to_string(),
            setting: setting.clone(),
            original: before.clone(),
            run: run.to_string(),
        });
        if let Some(o) = self.owned.get_mut(&id) {
            o.package = package.to_string();
            o.setting = setting.clone();
        }
        self.push(Change {
            run: run.to_string(),
            id,
            package: package.to_string(),
            setting: setting.clone(),
            before,
            after,
        });
    }

    /// Record that a key went back to `before`-of-its-ownership (its declaration was removed).
    pub fn record_release(&mut self, run: &str, setting: &Setting, before: Orig, after: Orig) {
        let id = setting.id();
        let package = self
            .owned
            .remove(&id)
            .map(|o| o.package)
            .unwrap_or_default();
        self.push(Change {
            run: run.to_string(),
            id,
            package,
            setting: setting.clone(),
            before,
            after,
        });
    }

    fn push(&mut self, c: Change) {
        self.history.push(c);
        let mut runs: Vec<String> = Vec::new();
        for h in &self.history {
            if !runs.contains(&h.run) {
                runs.push(h.run.clone());
            }
        }
        if runs.len() > KEEP_RUNS {
            let drop: Vec<String> = runs[..runs.len() - KEEP_RUNS].to_vec();
            self.history.retain(|h| !drop.contains(&h.run));
        }
    }

    /// The changes of one run, in the order they were made.
    pub fn run_changes(&self, run: &str) -> Vec<&Change> {
        self.history.iter().filter(|c| c.run == run).collect()
    }

    /// Forget a run's history, and the ownership of keys that went back to their original.
    pub fn forget_run(&mut self, run: &str, restored: &[(String, Orig)]) {
        self.history
            .retain(|c| c.run != run || !restored.iter().any(|(id, _)| *id == c.id));
        for (id, now) in restored {
            if self.owned.get(id).is_some_and(|o| &o.original == now) {
                self.owned.remove(id);
            }
        }
    }
}
