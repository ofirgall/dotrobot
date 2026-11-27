//! Lock inputs for planning, and the lock rows an apply run produces.
//!
//! The engine never reads or writes `dots.lock` itself: the caller loads it into a
//! [`LockInputs`], and gets [`LockUpdate`]s back in the [`crate::ApplyReport`] to write.

use std::collections::BTreeMap;

use dots_lock::Lock;
use dots_providers::{Installed, Spec};
use dots_resolve::OfferSource;

/// A target `dots upgrade` computed for one package: move it here, whatever the spec rules say.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpgradeTarget {
    /// The version to ask the provider for (`None`: the provider's newest).
    pub version: Option<String>,
    /// The version the lock row will name, when known.
    pub to: Option<String>,
}

/// Everything about the lock that planning reads.
pub struct LockInputs<'a> {
    pub lock: &'a Lock,
    /// `<most specific chain key>/<arch>`: this machine's platform key.
    pub machine_key: &'a str,
    /// What providers offer, for drift notes and "newest that satisfies" targets.
    pub offers: &'a dyn OfferSource,
    /// Packages `dots upgrade` moves explicitly.
    pub upgrade: &'a BTreeMap<String, UpgradeTarget>,
}

/// One planned change to (or note about) a lock row.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LockStep {
    pub package: String,
    pub provider: String,
    pub native: String,
    pub platform: String,
    /// What the row says now.
    pub from: Option<String>,
    /// What the row is expected to say afterwards (`None`: whatever gets installed).
    pub to: Option<String>,
    /// The row is rewritten (false for a pure drift note).
    pub rewrite: bool,
    /// Adopted: already installed outside dots, taken over without reinstalling.
    pub adopt: bool,
    /// `locked X, provider offers Y`.
    pub drift: Option<String>,
    /// A package step produces the new state (otherwise the installed version is recorded).
    pub has_step: bool,
    /// The provider's installed version now, for rows recorded without a step.
    pub installed: Option<String>,
    pub installed_source: Option<String>,
    pub git_ref: Option<String>,
    /// The provider spec the package is installed with (options decide the row's shape).
    pub spec: Option<Spec>,
}

impl LockStep {
    /// Does the plan list this row (the Lock section of the text, table and JSON views)? A row
    /// recorded for the first time is bookkeeping and stays out; a changed row, a row a step
    /// rewrites and a drift note are shown.
    pub fn shown(&self) -> bool {
        (self.rewrite && (self.from.is_some() || self.has_step)) || self.drift.is_some()
    }
}

/// A row for the caller to write into `dots.lock` after a run.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LockUpdate {
    pub package: String,
    pub provider: String,
    pub platform: String,
    pub version: Option<String>,
    pub rev: Option<String>,
    pub git_ref: Option<String>,
    pub source: Option<String>,
    pub adopt: bool,
}

/// The `source` a row records: a git url as `git+<url>`, else the provider's source (tap,
/// ppa, registry).
pub fn source_of(spec: &Spec) -> Option<String> {
    spec.get("git")
        .or_else(|| spec.get("url"))
        .map(|u| format!("git+{u}"))
        .or_else(|| spec.source.clone())
}

/// Split what a provider reports into the row's `version` and `rev`:
/// - a path source (`<crate version>.<n>+<rev>`) keeps the whole string as the version and
///   the part after `+` as the rev;
/// - a git install records its sha as the rev;
/// - a version that is not a version at all (a commit, herdr's `resolved_commit`) is a rev.
pub fn version_and_rev(
    provider: &str,
    spec: &Spec,
    installed: &Installed,
) -> (Option<String>, Option<String>) {
    let v = installed.version.clone();
    if spec.opts.contains_key("path") {
        let rev = v.split_once('+').map(|(_, r)| r.to_string());
        return (Some(v), rev);
    }
    let git_like = provider == "git" || spec.opts.contains_key("git");
    if git_like && let Some(src) = installed.source.clone() {
        let version = (dots_model::vspec::parseable(&v)
            && !v.starts_with(&src[..src.len().min(7)]))
        .then_some(v);
        return (version, Some(src));
    }
    if !dots_model::vspec::parseable(&v) {
        return (None, Some(v));
    }
    (Some(v), None)
}

/// The rows recorded without a package step: versions already installed that the lock now
/// names (first lock, adoption, a spec that moved but is still satisfied).
pub fn recorded_without_step(lock: &[LockStep]) -> Vec<LockUpdate> {
    lock.iter()
        .filter(|l| l.rewrite && !l.has_step)
        .filter_map(|l| {
            let installed = Installed {
                version: l.installed.clone()?,
                pinned: false,
                source: l.installed_source.clone(),
            };
            let spec = l.spec.clone().unwrap_or_else(|| Spec::new(&l.native));
            let (version, rev) = version_and_rev(&l.provider, &spec, &installed);
            Some(LockUpdate {
                package: l.package.clone(),
                provider: l.provider.clone(),
                platform: l.platform.clone(),
                version,
                rev,
                git_ref: l.git_ref.clone(),
                source: source_of(&spec),
                adopt: l.adopt,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inst(v: &str, src: Option<&str>) -> Installed {
        Installed {
            version: v.into(),
            pinned: false,
            source: src.map(str::to_string),
        }
    }

    #[test]
    fn rows_split_versions_and_revs() {
        let plain = Spec::new("bat");
        assert_eq!(
            version_and_rev("brew", &plain, &inst("0.24.0", None)),
            (Some("0.24.0".into()), None)
        );
        let git = Spec::new("kv");
        assert_eq!(
            version_and_rev("git", &git, &inst("9e1c0b7d2f00", Some("9e1c0b7d2f00"))),
            (None, Some("9e1c0b7d2f00".into()))
        );
        assert_eq!(
            version_and_rev("git", &git, &inst("v0.4.1", Some("9e1c0b7d2f00"))),
            (Some("v0.4.1".into()), Some("9e1c0b7d2f00".into()))
        );
        let path = Spec::new("tool").opt("path", "src");
        assert_eq!(
            version_and_rev("cargo", &path, &inst("0.3.1.7+ab12cd", None)),
            (Some("0.3.1.7+ab12cd".into()), Some("ab12cd".into()))
        );
        assert_eq!(
            version_and_rev("herdr", &plain, &inst("5e9c1ec", None)),
            (None, Some("5e9c1ec".into()))
        );
    }
}
