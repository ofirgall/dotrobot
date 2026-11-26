//! Environment access behind a trait, so tests never touch the process environment.

use std::collections::BTreeMap;
use std::ffi::OsString;

pub trait Env {
    fn get(&self, key: &str) -> Option<OsString>;

    /// Value as a string, treating empty as unset.
    fn get_str(&self, key: &str) -> Option<String> {
        self.get(key)
            .and_then(|v| v.into_string().ok())
            .filter(|v| !v.is_empty())
    }
}

/// The real process environment.
pub struct ProcessEnv;

impl Env for ProcessEnv {
    fn get(&self, key: &str) -> Option<OsString> {
        std::env::var_os(key)
    }
}

/// An in-memory environment for tests.
#[derive(Default, Clone)]
pub struct MapEnv(BTreeMap<String, OsString>);

impl MapEnv {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with(mut self, key: &str, value: impl Into<OsString>) -> Self {
        self.0.insert(key.to_string(), value.into());
        self
    }
}

impl Env for MapEnv {
    fn get(&self, key: &str) -> Option<OsString> {
        self.0.get(key).cloned()
    }
}
