//! The lock-versus-spec rules (ARCHITECTURE section 8.3 (g), `specs/lockfile`): given the
//! intersected spec, the lock row, what is installed and what the provider offers, decide what
//! the plan does and what happens to the lock row.
//!
//! | situation | decision |
//! |-----------|----------|
//! | not installed, lock row satisfies the spec | install the locked version (exact-capable providers) or what the provider offers, with `drift` if that differs |
//! | not installed, no row, or the spec moved away from the row | install the newest version that satisfies the spec; the row is rewritten |
//! | installed satisfies the spec, no row (or a row the spec no longer admits) | keep it, record it in the lock; adopt it if dots did not install it |
//! | installed equals the locked version | nothing |
//! | installed differs from the lock, installed outside dots | adopt; `decide_package` also rewrites the row to what is installed (a lock-only update) |
//! | installed older than the lock | upgrade to the locked version |
//! | installed newer than the lock, exact-capable provider | downgrade to the locked version (`⚠ downgrade`, always confirmed) |
//! | installed newer than the lock, limited provider (brew, apt) | no step; the row is rewritten to what is installed (`dots_engine::decide_package`), so `status`'s drift clears with one `apply` |
//! | installed version unknown (script without a version command), nothing else to do | nothing: the row is left alone and `status` shows ok |
//! | installed does not satisfy the spec | upgrade or downgrade into the spec; the row is rewritten |

use std::cmp::Ordering;

use dots_model::vspec::{self, Spec, compare, same};

use crate::PkgRow;

/// What a provider reports when it cannot tell the installed version (a script with no
/// `version` command).
pub const UNKNOWN_VERSION: &str = "unknown";

/// Is this the placeholder for a version the provider could not read?
pub fn version_unknown(installed: &str) -> bool {
    installed == UNKNOWN_VERSION
}

/// How well a provider installs an exact version (`Support` in the provider contract).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exact {
    Yes,
    Limited,
    No,
}

/// The parts of a lock row the rules read.
#[derive(Debug, Clone, Copy, Default)]
pub struct Locked<'a> {
    pub version: Option<&'a str>,
    pub rev: Option<&'a str>,
    pub git_ref: Option<&'a str>,
}

impl<'a> From<&'a PkgRow> for Locked<'a> {
    fn from(r: &'a PkgRow) -> Self {
        Locked {
            version: r.version.as_deref(),
            rev: r.rev.as_deref(),
            git_ref: r.git_ref.as_deref(),
        }
    }
}

impl Locked<'_> {
    /// What the row locks, for display.
    pub fn shown(&self) -> String {
        self.version.or(self.rev).unwrap_or_default().to_string()
    }
}

pub struct Facts<'a> {
    pub spec: &'a Spec,
    pub locked: Option<Locked<'a>>,
    pub installed: Option<&'a str>,
    /// The revision an install came from (a git sha), when the provider reports one.
    pub installed_rev: Option<&'a str>,
    /// Did dots install it (per `state.json`)?
    pub owned: bool,
    /// The newest version the provider offers.
    pub offered: Option<&'a str>,
    pub exact: Exact,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Install,
    Upgrade,
    Downgrade,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Step {
    pub kind: Kind,
    /// The version to ask the provider for (`None`: whatever it calls current).
    pub version: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Decision {
    pub step: Option<Step>,
    /// The version is already installed outside dots and satisfies the spec: take it over
    /// without reinstalling (journal `op: adopt`).
    pub adopt: bool,
    /// The lock row must be set to what ends up installed.
    pub rewrite: bool,
    /// What the row is expected to say afterwards, when known now (for the plan).
    pub expected: Option<String>,
    /// The provider cannot produce the locked version: `locked X, provider offers Y`.
    pub drift: Option<String>,
}

fn rev_eq(a: &str, b: &str) -> bool {
    a == b || (a.len().min(b.len()) >= 7 && (a.starts_with(b) || b.starts_with(a)))
}

fn is_sha(s: &str) -> bool {
    s.len() >= 7 && s.chars().all(|c| c.is_ascii_hexdigit())
}

/// Does the lock row still satisfy the spec?
pub fn lock_satisfies(spec: &Spec, l: &Locked<'_>) -> bool {
    match spec {
        Spec::Latest => true,
        Spec::Git(r) => l.git_ref == Some(r.as_str()) && l.rev.is_some(),
        _ => l.version.is_some_and(|v| spec.admits(v)),
    }
}

/// Is `installed` what the row locks?
pub fn matches_lock(l: &Locked<'_>, installed: &str, installed_rev: Option<&str>) -> bool {
    let by_version = l
        .version
        .is_some_and(|v| v == installed || same(installed, v));
    let by_rev = l
        .rev
        .is_some_and(|r| rev_eq(installed, r) || installed_rev.is_some_and(|ir| rev_eq(ir, r)));
    by_version || by_rev
}

fn installed_satisfies(f: &Facts<'_>, lock_ok: Option<&Locked<'_>>, inst: &str) -> bool {
    match f.spec {
        Spec::Git(r) => {
            if inst == r || f.installed_rev.is_some_and(|ir| rev_eq(ir, r)) || rev_eq(inst, r) {
                return true;
            }
            if lock_ok.is_some_and(|l| matches_lock(l, inst, f.installed_rev)) {
                return true;
            }
            // A branch name cannot be checked against a detached checkout; tags and shas can.
            let versionish = r
                .trim_start_matches('v')
                .starts_with(|c: char| c.is_ascii_digit());
            !(is_sha(r) || versionish)
        }
        s => s.admits(inst),
    }
}

/// Is `inst` (which does not satisfy `spec`) provably below the spec's range? When the direction
/// cannot be proven upward (an unparseable version, a pre-release above the floor, a ref) the
/// move counts as a downgrade, which asks for confirmation.
fn below(spec: &Spec, inst: &str) -> bool {
    if !vspec::parseable(inst) {
        return false;
    }
    let less = |s: &str| vspec::parseable(s) && compare(inst, s) == Ordering::Less;
    match spec {
        Spec::Exact(v) => less(v),
        Spec::Floor(f) => less(&join(f)),
        Spec::Prefix(p) => less(&join(p)),
        Spec::Bounded { prefix, floor } => {
            // Inside the prefix but under the floor, or below the prefix.
            if Spec::Prefix(prefix.clone()).admits(inst) {
                less(&join(floor))
            } else {
                less(&join(prefix))
            }
        }
        Spec::Git(r) => less(r.trim_start_matches('v')),
        Spec::Latest => false,
    }
}

fn join(v: &[u64]) -> String {
    v.iter().map(u64::to_string).collect::<Vec<_>>().join(".")
}

/// The version to ask the provider for when moving to the spec, and what the lock row is
/// expected to say.
fn target(f: &Facts<'_>, lock_ok: Option<&Locked<'_>>) -> (Option<String>, Option<String>) {
    match f.spec {
        Spec::Exact(v) => (Some(v.clone()), Some(v.clone())),
        Spec::Git(r) => match lock_ok {
            Some(l) => {
                let rev = l.rev.map(str::to_string);
                (rev.clone(), rev)
            }
            None => (Some(r.clone()), None),
        },
        spec => {
            if let Some(l) = lock_ok {
                let shown = Some(l.shown());
                let ask = (f.exact == Exact::Yes).then(|| shown.clone()).flatten();
                return (ask, shown);
            }
            let offered_ok = f
                .offered
                .filter(|o| spec.admits(o) && !vspec::is_prerelease(o));
            match offered_ok {
                Some(o) => (
                    (f.exact == Exact::Yes).then(|| o.to_string()),
                    Some(o.to_string()),
                ),
                // The newest version is unknown: an exact-capable provider can still be held to
                // the prefix (the provider resolves the newest match).
                None => match spec {
                    Spec::Prefix(p) if f.exact == Exact::Yes => (Some(join(p)), None),
                    _ => (None, None),
                },
            }
        }
    }
}

fn drift_note(f: &Facts<'_>, l: &Locked<'_>) -> Option<String> {
    if f.exact == Exact::Yes {
        return None;
    }
    let off = f.offered?;
    let locked = l.version?;
    (!same(off, locked) && f.spec.admits(off))
        .then(|| format!("locked {locked}, provider offers {off}"))
}

/// Decide what to do about one package.
pub fn decide(f: &Facts<'_>) -> Decision {
    let lock_ok = f.locked.filter(|l| lock_satisfies(f.spec, l));
    let lock_ok = lock_ok.as_ref();
    let mut d = Decision::default();

    let Some(inst) = f.installed else {
        // Not installed.
        let (ask, expected) = target(f, lock_ok);
        d.step = Some(Step {
            kind: Kind::Install,
            version: ask,
        });
        d.rewrite = lock_ok.is_none();
        d.expected = expected;
        if let Some(l) = lock_ok {
            d.drift = drift_note(f, l);
        }
        return d;
    };

    if !installed_satisfies(f, lock_ok, inst) {
        // The installed version is outside the spec: move into it.
        let (ask, expected) = target(f, lock_ok);
        let kind = if below(f.spec, inst) {
            Kind::Upgrade
        } else {
            Kind::Downgrade
        };
        // A provider that cannot install an older version cannot go down to a range; only an
        // exact spec is passed through (the provider may still know the version).
        if kind == Kind::Downgrade && f.exact != Exact::Yes && !matches!(f.spec, Spec::Exact(_)) {
            d.drift = Some(format!(
                "installed {inst} is outside {}; the provider cannot install an older version",
                f.spec
            ));
            return d;
        }
        d.step = Some(Step { kind, version: ask });
        d.rewrite = true;
        d.expected = expected;
        return d;
    }

    // Installed satisfies the spec.
    match lock_ok {
        None => {
            // No row, or a row the spec moved away from: keep what is installed and record it.
            d.rewrite = true;
            d.adopt = !f.owned;
            d.expected = Some(inst.to_string());
        }
        Some(l) if matches_lock(l, inst, f.installed_rev) => {}
        Some(l) => {
            if !f.owned {
                // Outside dots: adopt it, leave the lock, drift is reported by `status`.
                d.adopt = true;
                return d;
            }
            // Only a provable "installed is older" is an upgrade; a rev-only row, or a version
            // that cannot be compared, might be older or newer, so it is treated as a downgrade
            // (which asks for confirmation).
            let ord = match l.version {
                Some(v) if vspec::parseable(v) && vspec::parseable(inst) => compare(inst, v),
                _ => Ordering::Greater,
            };
            match (ord, f.exact) {
                (Ordering::Less, Exact::Yes) => {
                    d.step = Some(Step {
                        kind: Kind::Upgrade,
                        version: l.version.or(l.rev).map(str::to_string),
                    });
                }
                (Ordering::Less, _) => {
                    d.step = Some(Step {
                        kind: Kind::Upgrade,
                        version: None,
                    });
                    d.drift = drift_note(f, l);
                }
                (Ordering::Greater, Exact::Yes) => {
                    d.step = Some(Step {
                        kind: Kind::Downgrade,
                        version: l.version.or(l.rev).map(str::to_string),
                    });
                }
                // A limited provider cannot go down; `status` reports the drift.
                _ => {}
            }
        }
    }
    d
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(s: &str) -> Spec {
        Spec::parse(s).unwrap()
    }

    fn locked(v: &str) -> Locked<'_> {
        Locked {
            version: Some(v),
            ..Locked::default()
        }
    }

    fn facts<'a>(
        spec: &'a Spec,
        locked: Option<Locked<'a>>,
        installed: Option<&'a str>,
        offered: Option<&'a str>,
        exact: Exact,
    ) -> Facts<'a> {
        Facts {
            spec,
            locked,
            installed,
            installed_rev: None,
            owned: true,
            offered,
            exact,
        }
    }

    #[test]
    fn keeps_the_locked_version_under_latest() {
        let s = spec("latest");
        let d = decide(&facts(
            &s,
            Some(locked("0.9.6")),
            None,
            Some("0.10.2"),
            Exact::Yes,
        ));
        assert_eq!(
            d.step,
            Some(Step {
                kind: Kind::Install,
                version: Some("0.9.6".into())
            })
        );
        assert!(!d.rewrite);
    }

    #[test]
    fn spec_tightened_moves_the_row_to_the_newest_match() {
        let s = spec(">=0.10");
        let d = decide(&facts(
            &s,
            Some(locked("0.9.6")),
            Some("0.9.6"),
            Some("0.10.2"),
            Exact::Yes,
        ));
        assert_eq!(d.step.as_ref().unwrap().kind, Kind::Upgrade);
        assert_eq!(d.step.unwrap().version.as_deref(), Some("0.10.2"));
        assert!(d.rewrite);
        assert_eq!(d.expected.as_deref(), Some("0.10.2"));
    }

    #[test]
    fn limited_provider_installs_what_it_offers_and_reports_drift() {
        let s = spec("latest");
        let d = decide(&facts(
            &s,
            Some(locked("8.7.0")),
            None,
            Some("8.7.1"),
            Exact::Limited,
        ));
        let step = d.step.unwrap();
        assert_eq!((step.kind, step.version), (Kind::Install, None));
        assert_eq!(
            d.drift.as_deref(),
            Some("locked 8.7.0, provider offers 8.7.1")
        );
        assert!(!d.rewrite);
    }

    #[test]
    fn newer_than_the_lock_is_a_downgrade_only_where_exact_versions_work() {
        let s = spec("latest");
        let yes = decide(&facts(
            &s,
            Some(locked("1.0.0")),
            Some("1.2.0"),
            None,
            Exact::Yes,
        ));
        assert_eq!(yes.step.unwrap().kind, Kind::Downgrade);
        let limited = decide(&facts(
            &s,
            Some(locked("1.0.0")),
            Some("1.2.0"),
            None,
            Exact::Limited,
        ));
        assert_eq!(limited.step, None);
    }

    #[test]
    fn older_than_the_lock_upgrades_to_it() {
        let s = spec("latest");
        let d = decide(&facts(
            &s,
            Some(locked("1.2.0")),
            Some("1.0.0"),
            None,
            Exact::Yes,
        ));
        let st = d.step.unwrap();
        assert_eq!(
            (st.kind, st.version.as_deref()),
            (Kind::Upgrade, Some("1.2.0"))
        );
    }

    #[test]
    fn installed_outside_dots_is_adopted() {
        let s = spec("1.2");
        let mut f = facts(&s, None, Some("1.2.9"), None, Exact::Limited);
        f.owned = false;
        let d = decide(&f);
        assert!(d.adopt && d.rewrite && d.step.is_none());
        assert_eq!(d.expected.as_deref(), Some("1.2.9"));
        // with a differing lock row it is adopted but the row stays
        let mut g = facts(
            &s,
            Some(locked("1.2.3")),
            Some("1.2.9"),
            None,
            Exact::Limited,
        );
        g.owned = false;
        let d = decide(&g);
        assert!(d.adopt && !d.rewrite);
    }

    #[test]
    fn installed_outside_the_spec_moves_in() {
        let s = spec("0.11");
        let d = decide(&facts(&s, None, Some("0.12.1"), Some("0.11.4"), Exact::Yes));
        let st = d.step.unwrap();
        assert_eq!(
            (st.kind, st.version.as_deref()),
            (Kind::Downgrade, Some("0.11.4"))
        );
        let d = decide(&facts(&s, None, Some("0.10.9"), Some("0.11.4"), Exact::Yes));
        assert_eq!(d.step.unwrap().kind, Kind::Upgrade);
        let e = spec("1.2.3");
        let d = decide(&facts(&e, None, Some("2.0.0"), None, Exact::Limited));
        let st = d.step.unwrap();
        assert_eq!(
            (st.kind, st.version.as_deref()),
            (Kind::Downgrade, Some("1.2.3"))
        );
    }

    #[test]
    fn git_specs_use_the_locked_rev() {
        let s = spec("git:main");
        let l = Locked {
            version: None,
            rev: Some("9e1c0b7d2f"),
            git_ref: Some("main"),
        };
        let d = decide(&facts(&s, Some(l), None, None, Exact::Yes));
        assert_eq!(d.step.unwrap().version.as_deref(), Some("9e1c0b7d2f"));
        let mut f = facts(&s, Some(l), Some("1234567"), None, Exact::Yes);
        f.installed_rev = Some("1234567890");
        let d = decide(&f);
        let st = d.step.unwrap();
        // A rev cannot say which way it moves: confirmed like a downgrade.
        assert_eq!(
            (st.kind, st.version.as_deref()),
            (Kind::Downgrade, Some("9e1c0b7d2f"))
        );
        let mut f = facts(&s, Some(l), Some("9e1c0b7"), None, Exact::Yes);
        f.installed_rev = Some("9e1c0b7d2f");
        assert_eq!(decide(&f), Decision::default());
    }

    #[test]
    fn a_prerelease_above_the_floor_moving_to_an_older_offer_is_a_downgrade() {
        let s = spec(">=1.5");
        let d = decide(&facts(
            &s,
            None,
            Some("2.0.0-rc1"),
            Some("1.9.0"),
            Exact::Yes,
        ));
        assert_eq!(d.step.unwrap().kind, Kind::Downgrade);
        // An installed version that cannot be compared is never assumed to be older.
        let d = decide(&facts(&s, None, Some("nightly"), Some("1.9.0"), Exact::Yes));
        assert_eq!(d.step.unwrap().kind, Kind::Downgrade);
        // A plainly older one is still an upgrade.
        let d = decide(&facts(&s, None, Some("1.2.0"), Some("1.9.0"), Exact::Yes));
        assert_eq!(d.step.unwrap().kind, Kind::Upgrade);
    }

    #[test]
    fn a_differing_locked_rev_is_not_an_upgrade() {
        let s = spec("latest");
        let l = Locked {
            version: None,
            rev: Some("aaaaaaaa11"),
            git_ref: None,
        };
        let d = decide(&facts(&s, Some(l), Some("1.4.0"), None, Exact::Yes));
        assert_eq!(d.step.unwrap().kind, Kind::Downgrade);
    }
}
