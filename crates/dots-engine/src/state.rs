//! `<state>/state.json`: what dots owns. Schema-versioned, rewritten atomically (write a temp
//! file, then rename) after every completed action, so an interrupted run leaves an accurate
//! record.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub const STATE_SCHEMA: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OwnedPackage {
    pub provider: String,
    /// The provider-native name.
    pub name: String,
    pub version: Option<String>,
    pub run: String,
    pub ts: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OwnedFile {
    pub package: String,
    pub kind: String,
    pub run: String,
    pub ts: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Todo {
    pub package: String,
    pub reason: String,
    pub run: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct State {
    pub schema: u32,
    #[serde(default)]
    pub packages: BTreeMap<String, OwnedPackage>,
    /// Keyed by destination.
    #[serde(default)]
    pub files: BTreeMap<String, OwnedFile>,
    #[serde(default)]
    pub todos: Vec<Todo>,
}

impl Default for State {
    fn default() -> Self {
        State {
            schema: STATE_SCHEMA,
            packages: BTreeMap::new(),
            files: BTreeMap::new(),
            todos: Vec::new(),
        }
    }
}

impl State {
    /// Load `path`. A missing file is an empty state. A file that does not parse is moved to
    /// `state.json.corrupt` and an empty state is returned, with the reason in the second value.
    pub fn load(path: &Path) -> (State, Option<String>) {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(_) => return (State::default(), None),
        };
        match serde_json::from_str::<State>(&text) {
            Ok(s) if s.schema <= STATE_SCHEMA => (s, None),
            Ok(s) => (
                State::default(),
                Some(format!(
                    "state.json has schema {} (newer than this dots)",
                    s.schema
                )),
            ),
            Err(e) => {
                let mut bad = path.to_path_buf().into_os_string();
                bad.push(".corrupt");
                let _ = std::fs::rename(path, PathBuf::from(bad));
                (
                    State::default(),
                    Some(format!("state.json was unreadable ({e})")),
                )
            }
        }
    }

    /// Atomic write: temp file in the same directory, fsync, rename.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        let dir = path.parent().unwrap_or_else(|| Path::new("."));
        std::fs::create_dir_all(dir)?;
        let tmp = dir.join(format!(".state.json.{}.tmp", std::process::id()));
        let mut text = serde_json::to_string_pretty(self).map_err(std::io::Error::other)?;
        text.push('\n');
        {
            let mut f = std::fs::File::create(&tmp)?;
            f.write_all(text.as_bytes())?;
            f.sync_all()?;
        }
        std::fs::rename(&tmp, path).inspect_err(|_| {
            let _ = std::fs::remove_file(&tmp);
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_and_corrupt() {
        let d = tempfile_dir();
        let p = d.join("state.json");
        let mut s = State::default();
        s.todos.push(Todo {
            package: "a".into(),
            reason: "interactive".into(),
            run: "r-1".into(),
        });
        s.save(&p).unwrap();
        assert_eq!(State::load(&p).0, s);
        std::fs::write(&p, "{ nope").unwrap();
        let (fresh, why) = State::load(&p);
        assert_eq!(fresh, State::default());
        assert!(why.is_some());
        assert!(d.join("state.json.corrupt").exists());
    }

    fn tempfile_dir() -> PathBuf {
        let d = std::env::temp_dir().join(format!("dots-state-{}-{}", std::process::id(), line!()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }
}
