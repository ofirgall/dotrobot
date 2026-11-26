//! Config, state, cache and data directories and the state-dir layout (design D6).

use std::io;
use std::path::{Path, PathBuf};

use crate::env::Env;

/// The four dots directories plus the (possibly redirected) home.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dirs {
    pub home: PathBuf,
    pub config: PathBuf,
    pub state: PathBuf,
    pub cache: PathBuf,
    pub data: PathBuf,
}

impl Dirs {
    /// Resolve the directories.
    ///
    /// `--home <dir>` wins over everything: it redirects `~` and all four dirs under `<dir>`
    /// (flags beat environment variables). Otherwise each dir is `DOTS_<KIND>_DIR`, then
    /// `$XDG_<KIND>_HOME/dots` (absolute values only), then the default under `~`.
    pub fn resolve(home_flag: Option<&Path>, env: &dyn Env) -> Dirs {
        if let Some(home) = home_flag {
            return Dirs::under(home.to_path_buf());
        }
        let home = env
            .get("HOME")
            .or_else(|| env.get("USERPROFILE"))
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/"));
        let pick = |dots_var: &str, xdg_var: &str, default: &str| -> PathBuf {
            if let Some(p) = absolute(env.get(dots_var)) {
                return p;
            }
            if let Some(p) = absolute(env.get(xdg_var)) {
                return p.join("dots");
            }
            home.join(default).join("dots")
        };
        Dirs {
            config: pick("DOTS_CONFIG_DIR", "XDG_CONFIG_HOME", ".config"),
            state: pick("DOTS_STATE_DIR", "XDG_STATE_HOME", ".local/state"),
            cache: pick("DOTS_CACHE_DIR", "XDG_CACHE_HOME", ".cache"),
            data: pick("DOTS_DATA_DIR", "XDG_DATA_HOME", ".local/share"),
            home,
        }
    }

    fn under(home: PathBuf) -> Dirs {
        Dirs {
            config: home.join(".config/dots"),
            state: home.join(".local/state/dots"),
            cache: home.join(".cache/dots"),
            data: home.join(".local/share/dots"),
            home,
        }
    }

    /// Create all four directories. Nothing else in dots creates them implicitly.
    pub fn ensure_all(&self) -> io::Result<()> {
        for d in [&self.config, &self.state, &self.cache, &self.data] {
            std::fs::create_dir_all(d)?;
        }
        Ok(())
    }

    // Config dir.
    pub fn machine_toml(&self, override_path: Option<&Path>) -> PathBuf {
        override_path
            .map(Path::to_path_buf)
            .unwrap_or_else(|| self.config.join("machine.toml"))
    }
    pub fn ui_toml(&self) -> PathBuf {
        self.config.join("ui.toml")
    }

    // State dir.
    pub fn state_json(&self) -> PathBuf {
        self.state.join("state.json")
    }
    pub fn journal(&self) -> PathBuf {
        self.state.join("journal.jsonl")
    }
    pub fn run_lock(&self) -> PathBuf {
        self.state.join("run.lock")
    }
    pub fn logs(&self, run_id: &str) -> PathBuf {
        self.state.join("logs").join(run_id)
    }
    pub fn backup(&self, run_id: &str) -> PathBuf {
        self.state.join("backup").join(run_id)
    }
    pub fn gen_dir(&self, root_id: &str) -> PathBuf {
        self.state.join("gen").join(root_id)
    }
    /// The active-root symlink.
    pub fn active(&self) -> PathBuf {
        self.state.join("active")
    }
    /// Generated env file for a shell: `sh`, `fish` or `ps1`.
    pub fn env_file(&self, shell: &str) -> PathBuf {
        self.state.join(format!("env.{shell}"))
    }

    // Data dir.
    pub fn upstream(&self, alias: &str, rev: &str) -> PathBuf {
        self.data.join("upstreams").join(alias).join(rev)
    }

    // Cache dir.
    pub fn remotes(&self) -> PathBuf {
        self.cache.join("remotes")
    }
    pub fn latest_cache(&self) -> PathBuf {
        self.cache.join("latest")
    }
}

fn absolute(v: Option<std::ffi::OsString>) -> Option<PathBuf> {
    v.map(PathBuf::from).filter(|p| p.is_absolute())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::env::MapEnv;

    #[test]
    fn defaults_under_home() {
        let d = Dirs::resolve(None, &MapEnv::new().with("HOME", "/h"));
        assert_eq!(d.config, Path::new("/h/.config/dots"));
        assert_eq!(d.state, Path::new("/h/.local/state/dots"));
        assert_eq!(d.cache, Path::new("/h/.cache/dots"));
        assert_eq!(d.data, Path::new("/h/.local/share/dots"));
    }

    #[test]
    fn xdg_then_dots_vars() {
        let env = MapEnv::new()
            .with("HOME", "/h")
            .with("XDG_STATE_HOME", "/x/state")
            .with("XDG_CONFIG_HOME", "relative-is-ignored")
            .with("DOTS_CACHE_DIR", "/c");
        let d = Dirs::resolve(None, &env);
        assert_eq!(d.state, Path::new("/x/state/dots"));
        assert_eq!(d.config, Path::new("/h/.config/dots"));
        assert_eq!(d.cache, Path::new("/c"));
        let env = env.with("DOTS_STATE_DIR", "/s");
        assert_eq!(Dirs::resolve(None, &env).state, Path::new("/s"));
    }

    #[test]
    fn home_flag_beats_env() {
        let env = MapEnv::new()
            .with("HOME", "/h")
            .with("DOTS_STATE_DIR", "/s");
        let d = Dirs::resolve(Some(Path::new("/tmp/x")), &env);
        assert_eq!(d.home, Path::new("/tmp/x"));
        for p in [&d.config, &d.state, &d.cache, &d.data] {
            assert!(p.starts_with("/tmp/x"), "{p:?}");
        }
    }

    #[test]
    fn ensure_all_stays_inside_home() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("h");
        let d = Dirs::resolve(Some(&home), &MapEnv::new().with("HOME", "/nonexistent"));
        d.ensure_all().unwrap();
        assert!(d.state.is_dir() && d.config.is_dir() && d.cache.is_dir() && d.data.is_dir());
        let top: Vec<_> = std::fs::read_dir(tmp.path()).unwrap().collect();
        assert_eq!(top.len(), 1);
    }

    #[test]
    fn layout_names() {
        let d = Dirs::resolve(Some(Path::new("/h")), &MapEnv::new());
        assert_eq!(d.state_json(), Path::new("/h/.local/state/dots/state.json"));
        assert_eq!(
            d.env_file("fish"),
            Path::new("/h/.local/state/dots/env.fish")
        );
        assert_eq!(
            d.upstream("a", "r"),
            Path::new("/h/.local/share/dots/upstreams/a/r")
        );
        assert_eq!(d.latest_cache(), Path::new("/h/.cache/dots/latest"));
    }
}
