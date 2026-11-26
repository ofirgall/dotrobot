//! Version helpers for the diff. The grammar, comparison and intersection live in
//! `dots_model::vspec`; the lock-versus-spec rules in `dots_lock::rules`. This module keeps the
//! small `diff` API the status and outdated views use, built on those.

use dots_lock::rules::{self, Exact, Facts, Kind};
use dots_model::vspec::Spec;

pub use dots_model::vspec::{compare, normalize};

/// A parsed desired version (ARCHITECTURE section 8.1).
pub type Wanted = Spec;

/// Parse a `version`; an omitted or (already reported by the loader) invalid spec is `latest`.
pub fn parse_wanted(spec: Option<&str>) -> Wanted {
    Spec::parse_opt(spec).unwrap_or(Spec::Latest)
}

/// What the diff should do about an installed version, ignoring the lock.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Change {
    None,
    /// Move to this exact version (`None`: whatever the provider calls latest).
    Install(Option<String>),
    Upgrade(Option<String>),
    Downgrade(String),
}

/// Compare `installed` (if any) with the wanted spec.
pub fn diff(wanted: &Wanted, installed: Option<&str>) -> Change {
    let d = rules::decide(&Facts {
        spec: wanted,
        locked: None,
        installed,
        installed_rev: None,
        owned: true,
        offered: None,
        exact: Exact::Yes,
    });
    match d.step {
        None => Change::None,
        Some(s) => match s.kind {
            Kind::Install => Change::Install(s.version),
            Kind::Upgrade => Change::Upgrade(s.version),
            Kind::Downgrade => Change::Downgrade(s.version.unwrap_or_else(|| wanted.to_string())),
        },
    }
}

/// Is `latest` something dots would move `have` to? A newer release only counts when it still
/// satisfies the wanted version, is not a pre-release, and the install is not pinned.
pub fn is_outdated(wanted: &Wanted, have: &str, latest: &str, pinned: bool) -> bool {
    !pinned
        && !wanted.is_git()
        && !dots_model::vspec::is_prerelease(latest)
        && compare(latest, have) == std::cmp::Ordering::Greater
        && diff(wanted, Some(latest)) == Change::None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cmp::Ordering;

    #[test]
    fn compares() {
        assert_eq!(compare("1.2.0", "1.2"), Ordering::Equal);
        assert_eq!(compare("v1.10.0", "1.9.9"), Ordering::Greater);
        assert_eq!(compare("1.2.0-rc1", "1.2.0"), Ordering::Less);
        assert_eq!(compare("1:2.0-1ubuntu1", "2.0"), Ordering::Equal);
    }

    #[test]
    fn diffs() {
        let w = parse_wanted(Some("1.2.3"));
        assert_eq!(diff(&w, None), Change::Install(Some("1.2.3".into())));
        assert_eq!(diff(&w, Some("1.2.3")), Change::None);
        assert_eq!(
            diff(&w, Some("1.0.0")),
            Change::Upgrade(Some("1.2.3".into()))
        );
        assert_eq!(diff(&w, Some("2.0.0")), Change::Downgrade("1.2.3".into()));
        assert_eq!(diff(&parse_wanted(None), Some("9")), Change::None);
        assert_eq!(
            diff(&parse_wanted(Some("1.2")), Some("1.2.9")),
            Change::None
        );
        assert_eq!(
            diff(&parse_wanted(Some("1.2")), Some("1.20.0")),
            Change::Downgrade("1.2".into())
        );
        assert_eq!(
            diff(&parse_wanted(Some(">=1.2")), Some("1.1")),
            Change::Upgrade(None)
        );
    }

    #[test]
    fn outdated() {
        let latest = parse_wanted(None);
        assert!(is_outdated(&latest, "1.0.0", "1.1.0", false));
        assert!(!is_outdated(&latest, "1.0.0", "2.0.0-rc1", false));
        assert!(!is_outdated(&latest, "1.0.0", "1.1.0", true));
        assert!(!is_outdated(
            &parse_wanted(Some("1.0")),
            "1.0.1",
            "1.1.0",
            false
        ));
    }
}
