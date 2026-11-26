//! Version constraints per package, with provenance, their intersection, and the
//! constraint-aware provider check (ARCHITECTURE section 8.3 (a)-(c)).
//!
//! Every package collects one constraint per source: its own spec (or the override that
//! replaced it) and every dependent's `requires`. Dependents' constraints are never replaced by
//! an override of the package itself; that is the dependents' business.

use dots_core::{Diagnostic, Provenance};
use dots_model::vspec::{self, Spec};
use dots_providers::Support;

use crate::model::Step;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConstraintSource {
    /// The package's own `version`.
    Own { file: String },
    /// A dependent's `requires = [{ name, version }]`.
    Requires {
        from: String,
        file: String,
        path: Vec<Step>,
    },
    /// An override replacing the package's own spec.
    Override { file: String },
}

impl ConstraintSource {
    pub fn file(&self) -> &str {
        match self {
            ConstraintSource::Own { file }
            | ConstraintSource::Requires { file, .. }
            | ConstraintSource::Override { file } => file,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Constraint {
    /// The raw spec as written.
    pub spec: String,
    pub source: ConstraintSource,
    /// 1-based line of the spec in `source`'s file, when known.
    pub line: Option<u32>,
    /// How it got here, for messages: an inclusion path (`machine → cli → kv → requires rust
    /// >=1.80`) or the override layer (`repo override`).
    pub via: String,
}

impl Constraint {
    fn kind_label(&self) -> String {
        match &self.source {
            ConstraintSource::Own { .. } => "package".into(),
            ConstraintSource::Requires { from, .. } => format!("package:{from}"),
            ConstraintSource::Override { .. } => "override".into(),
        }
    }

    /// `">=1.80" from packages/kv/package.toml:12 (machine → cli → kv → requires rust >=1.80)`.
    pub fn describe(&self) -> String {
        let at = match self.line {
            Some(l) => format!("{}:{l}", self.source.file()),
            None => self.source.file().to_string(),
        };
        if self.via.is_empty() {
            format!("\"{}\" from {at}", self.spec)
        } else {
            format!("\"{}\" from {at} ({})", self.spec, self.via)
        }
    }

    fn provenance(&self) -> Provenance {
        Provenance {
            source: self.kind_label(),
            file: Some(self.source.file().to_string()),
            line: self.line,
            col: None,
        }
    }
}

/// Every constraint on one package.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ConstraintSet {
    pub constraints: Vec<Constraint>,
}

impl ConstraintSet {
    pub fn is_empty(&self) -> bool {
        self.constraints.is_empty()
    }

    /// Own spec (or override) first, then dependents by name, so messages are stable.
    pub fn sort(&mut self) {
        self.constraints.sort_by_key(|c| match &c.source {
            ConstraintSource::Own { .. } | ConstraintSource::Override { .. } => {
                (0, String::new(), c.line)
            }
            ConstraintSource::Requires { from, .. } => (1, from.clone(), c.line),
        });
    }

    /// Intersect every constraint. Unparseable specs are skipped (the loader reports them).
    /// `Err` is the `version-conflict` diagnostic naming the two sides.
    pub fn effective(&self, package: &str) -> Result<Spec, Diagnostic> {
        let parsed: Vec<(&Constraint, Spec)> = self
            .constraints
            .iter()
            .filter_map(|c| Spec::parse(&c.spec).ok().map(|s| (c, s)))
            .collect();
        let mut acc = Spec::Latest;
        for (i, (c, s)) in parsed.iter().enumerate() {
            match acc.intersect(s) {
                Ok(next) => acc = next,
                Err(why) => {
                    // Prefer a single earlier constraint that conflicts with this one on its
                    // own; otherwise the accumulated constraints together are the other side.
                    let single = parsed[..i]
                        .iter()
                        .find(|(_, o)| o.intersect(s).is_err())
                        .map(|(o, _)| *o);
                    return Err(conflict(
                        package,
                        single,
                        &parsed[..i].iter().map(|(c, _)| *c).collect::<Vec<_>>(),
                        c,
                        &why,
                    ));
                }
            }
        }
        Ok(acc)
    }

    /// The constraints whose spec does not admit `version`.
    pub fn rejecting(&self, version: &str) -> Vec<&Constraint> {
        self.constraints
            .iter()
            .filter(|c| Spec::parse(&c.spec).is_ok_and(|s| !s.admits(version)))
            .collect()
    }
}

fn conflict(
    package: &str,
    left: Option<&Constraint>,
    earlier: &[&Constraint],
    right: &Constraint,
    why: &str,
) -> Diagnostic {
    let left_text = match (left, earlier) {
        (Some(l), _) => l.describe(),
        (None, [one]) => one.describe(),
        (None, many) => many
            .iter()
            .map(|c| c.describe())
            .collect::<Vec<_>>()
            .join(" and "),
    };
    let right_text = right.describe();
    let mut d = Diagnostic::from_code(
        "version-conflict",
        &[
            ("package", package),
            ("left", &left_text),
            ("right", &right_text),
        ],
    );
    d.hint = format!("{why}. {}", d.hint);
    for c in left.into_iter().chain(if left.is_none() {
        earlier.to_vec()
    } else {
        vec![]
    }) {
        d.provenance.push(c.provenance());
    }
    d.provenance.push(right.provenance());
    d
}

/// What a provider offers, looked up lazily because asking costs a provider call.
pub trait OfferSource {
    /// The newest version `provider` offers for package `pkg`'s `native` name (`spec` carries tap, source and
    /// options). `None` when unknown (offline, query failed, nothing cached).
    fn offered(
        &self,
        provider: &str,
        pkg: &str,
        native: &str,
        spec: &dots_providers::Spec,
    ) -> Option<String>;
}

/// Offers nothing: every provider is assumed able to deliver.
pub struct NoOffers;

impl OfferSource for NoOffers {
    fn offered(&self, _: &str, _: &str, _: &str, _: &dots_providers::Spec) -> Option<String> {
        None
    }
}

/// Providers that can install a git ref.
fn git_capable(provider: &str) -> bool {
    !matches!(provider, "brew" | "apt" | "script")
}

/// Providers that only offer their current version (brew formulae, apt's candidate), so a
/// numeric constraint can be checked against it. Everything else is taken at its word: a
/// script or user-defined provider installs what its commands say.
pub fn offers_only_current(provider: &str) -> bool {
    matches!(provider, "brew" | "apt")
}

/// Can `provider` deliver a version satisfying `spec`? `Err` is the skip reason shown to the
/// user, such as `offers 10.2.0, need 8.7`.
///
/// Providers that install any exact version (`Support::Yes`) always can. brew and apt offer
/// only their current version (plus what is installed); with neither known, the check cannot
/// decide and lets the provider through. Other providers are not second-guessed. `git:` needs a git-capable provider.
pub fn can_satisfy(
    provider: &str,
    exact: Option<Support>,
    spec: &Spec,
    installed: Option<&str>,
    offered: Option<&str>,
) -> Result<(), String> {
    match spec {
        Spec::Latest => Ok(()),
        Spec::Git(_) => {
            if git_capable(provider) {
                Ok(())
            } else {
                Err(format!("cannot install a git ref (need {spec})"))
            }
        }
        _ => {
            if exact == Some(Support::Yes) || !offers_only_current(provider) {
                return Ok(());
            }
            let candidates: Vec<&str> = offered.into_iter().chain(installed).collect();
            if candidates.is_empty() || candidates.iter().any(|c| spec.admits(c)) {
                return Ok(());
            }
            let have = match (offered, installed) {
                (Some(o), Some(i)) if !vspec::same(o, i) => format!("offers {o}, has {i}"),
                (Some(o), _) => format!("offers {o}"),
                (None, Some(i)) => format!("has {i}"),
                (None, None) => unreachable!(),
            };
            Err(format!("{have}, need {spec}"))
        }
    }
}
