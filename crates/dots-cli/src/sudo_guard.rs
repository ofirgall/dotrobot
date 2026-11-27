//! Refuse to run under `sudo`.
//!
//! `sudo dots apply --with-sudo` runs all of dots as root with the user's `HOME` (macOS keeps
//! it), so the user's lock, state and log directories are used by root: every `check` and
//! `version` command runs as root (git over ssh as root waits on a prompt nobody sees), and
//! state, logs and installed files end up root-owned. dots asks for the password itself and
//! runs only the steps that need root as root, so it must start as the user.
//!
//! A real root login (a container or CI job as root: uid 0 with no `SUDO_USER`) is not sudo
//! and keeps working, as does `DOTS_ALLOW_SUDO=1` for anyone who means it.

use dots_core::Env;

/// Where this process stands; injected so the decision is testable without being root.
pub struct Started<'a> {
    /// The effective uid is 0.
    pub root: bool,
    pub env: &'a dyn Env,
}

/// The user `sudo` was started by, when dots was started through sudo and should refuse.
///
/// `None`: run normally. Root with `SUDO_USER` set is sudo; `SUDO_USER=root` (root using
/// sudo) is not worth refusing, since root's own `HOME` is then the right one.
pub fn refuse_for(s: &Started) -> Option<String> {
    if !s.root {
        return None;
    }
    let user = s.env.get_str("SUDO_USER").filter(|u| u != "root")?;
    let allowed = s
        .env
        .get_str("DOTS_ALLOW_SUDO")
        .is_some_and(|v| !matches!(v.as_str(), "0" | "false" | "no"));
    (!allowed).then_some(user)
}

#[cfg(test)]
mod tests {
    use super::*;
    use dots_core::MapEnv;

    fn decide(root: bool, env: MapEnv) -> Option<String> {
        refuse_for(&Started { root, env: &env })
    }

    #[test]
    fn root_started_by_sudo_is_refused() {
        let env = MapEnv::new().with("SUDO_USER", "ofir");
        assert_eq!(decide(true, env), Some("ofir".into()));
    }

    #[test]
    fn a_real_root_login_keeps_working() {
        assert_eq!(decide(true, MapEnv::new()), None);
        // An empty SUDO_USER is not set.
        assert_eq!(decide(true, MapEnv::new().with("SUDO_USER", "")), None);
    }

    #[test]
    fn a_normal_user_is_never_refused() {
        // SUDO_USER can linger in an environment (`sudo -u me`, `su -`): without root it
        // means nothing.
        assert_eq!(decide(false, MapEnv::new().with("SUDO_USER", "ofir")), None);
        assert_eq!(decide(false, MapEnv::new()), None);
    }

    #[test]
    fn root_using_sudo_is_not_refused() {
        assert_eq!(decide(true, MapEnv::new().with("SUDO_USER", "root")), None);
    }

    #[test]
    fn the_escape_hatch_allows_it() {
        let env = |v: &str| {
            MapEnv::new()
                .with("SUDO_USER", "ofir")
                .with("DOTS_ALLOW_SUDO", v)
        };
        assert_eq!(decide(true, env("1")), None);
        assert!(decide(true, env("0")).is_some());
        assert!(decide(true, env("")).is_some());
    }
}
