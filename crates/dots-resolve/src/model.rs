//! The resolution result: what is in the set, why, and with which provider.

use std::collections::BTreeMap;
use std::fmt;

use dots_core::Diagnostic;
use serde_json::{Value, json};

use crate::constraints::ConstraintSet;

/// One hop of an inclusion path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    /// The machine's own bundle list; always the first step.
    Machine,
    /// A bundle, reached from the machine or through `include`.
    Bundle(String),
    /// A package listed by the bundle before it (or the requirer of the next step).
    Package(String),
    /// Reached through the previous package's `requires`.
    Requires {
        package: String,
        version: Option<String>,
    },
    /// Injected because `provider` is in the `Bootstrap` state and `for_package` uses it.
    Bootstrap {
        provider: String,
        for_package: String,
    },
}

impl Step {
    /// Short text used in `machine → cli → jless → requires rust >=1.80`.
    pub fn label(&self) -> String {
        match self {
            Step::Machine => "machine".into(),
            Step::Bundle(n) | Step::Package(n) => n.clone(),
            Step::Requires { package, version } => match version {
                Some(v) => format!("requires {package} {v}"),
                None => format!("requires {package}"),
            },
            Step::Bootstrap {
                provider,
                for_package,
            } => format!("bootstrap for {provider} (used by {for_package})"),
        }
    }

    pub fn kind(&self) -> &'static str {
        match self {
            Step::Machine => "machine",
            Step::Bundle(_) => "bundle",
            Step::Package(_) => "package",
            Step::Requires { .. } => "requires",
            Step::Bootstrap { .. } => "bootstrap",
        }
    }

    pub fn to_json(&self) -> Value {
        let mut v = json!({ "kind": self.kind(), "label": self.label() });
        match self {
            Step::Bundle(n) | Step::Package(n) => v["name"] = json!(n),
            Step::Requires { package, version } => {
                v["name"] = json!(package);
                if let Some(ver) = version {
                    v["version"] = json!(ver);
                }
            }
            Step::Bootstrap { provider, .. } => v["name"] = json!(provider),
            Step::Machine => {}
        }
        v
    }
}

/// A `when` met on the way, and what it evaluated to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathCondition {
    /// The step (label) the condition is attached to.
    pub on: String,
    pub when: String,
    pub result: bool,
}

/// One way a package got into the set (or would have).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Inclusion {
    pub steps: Vec<Step>,
    /// True when every condition along the path holds.
    pub active: bool,
    pub conditions: Vec<PathCondition>,
}

impl Inclusion {
    /// The first condition that was false, if any.
    pub fn blocked_by(&self) -> Option<&PathCondition> {
        self.conditions.iter().find(|c| !c.result)
    }

    /// `machine → cli → jless → requires rust >=1.80`.
    pub fn display(&self) -> String {
        self.steps
            .iter()
            .map(Step::label)
            .collect::<Vec<_>>()
            .join(" → ")
    }

    pub fn to_json(&self) -> Value {
        json!({
            "text": self.display(),
            "active": self.active,
            "steps": self.steps.iter().map(Step::to_json).collect::<Vec<_>>(),
            "conditions": self.conditions.iter().map(|c| json!({
                "on": c.on, "when": c.when, "result": c.result,
            })).collect::<Vec<_>>(),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PackageState {
    /// In the set; has a provider (unless it is a files-only package) and a place in `order`.
    Active,
    /// Not wanted here: its own `when` (or every inclusion path's) is false. `n/a`.
    Inert { condition: String },
    /// Listed in the machine's `exclude`.
    Excluded,
    /// Dropped because another selected package declares `replaces`.
    Replaced { by: String },
}

impl PackageState {
    pub fn as_str(&self) -> &'static str {
        match self {
            PackageState::Active => "active",
            PackageState::Inert { .. } => "inert",
            PackageState::Excluded => "excluded",
            PackageState::Replaced { .. } => "replaced",
        }
    }
}

/// Why a candidate provider was not used.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkipReason {
    /// The provider table's `when` is false.
    WhenFalse(String),
    /// The provider is not usable here (`detail` says why).
    Unavailable(String),
    NeedsRoot,
    /// The provider cannot deliver the intersected version constraint (task 5.8).
    CannotSatisfy(String),
    /// The provider needs a bootstrap package that cannot be added.
    Bootstrap {
        package: String,
        why: String,
    },
}

impl fmt::Display for SkipReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SkipReason::WhenFalse(w) => write!(f, "its `when` is false ({w})"),
            SkipReason::Unavailable(d) => f.write_str(d),
            SkipReason::NeedsRoot => f.write_str("needs root"),
            SkipReason::CannotSatisfy(d) => f.write_str(d),
            SkipReason::Bootstrap { package, why } => {
                write!(f, "needs bootstrap package `{package}`, but {why}")
            }
        }
    }
}

impl SkipReason {
    fn kind(&self) -> &'static str {
        match self {
            SkipReason::WhenFalse(_) => "when-false",
            SkipReason::Unavailable(_) => "unavailable",
            SkipReason::NeedsRoot => "needs-root",
            SkipReason::CannotSatisfy(_) => "cannot-satisfy",
            SkipReason::Bootstrap { .. } => "bootstrap",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CandidateOutcome {
    Chosen,
    /// Usable, but a better-ranked candidate won.
    Standby,
    Skipped(SkipReason),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CandidateReport {
    pub id: String,
    pub outcome: CandidateOutcome,
    /// True when this provider already has the package installed.
    pub installed: Option<String>,
}

impl CandidateReport {
    /// `apt skipped: needs root`.
    pub fn line(&self) -> String {
        match &self.outcome {
            CandidateOutcome::Chosen => format!("{} chosen", self.id),
            CandidateOutcome::Standby => format!("{} available (not needed)", self.id),
            CandidateOutcome::Skipped(r) => format!("{} skipped: {r}", self.id),
        }
    }
}

/// How the candidates were ranked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ranking {
    /// The package's `prefer` list (then the priority list for the rest).
    Prefer,
    /// The `[priority]` list of this platform key.
    Priority(String),
    /// Neither exists; candidates in alphabetical order.
    Unordered,
}

impl Ranking {
    pub fn describe(&self) -> String {
        match self {
            Ranking::Prefer => "package `prefer` list".into(),
            Ranking::Priority(k) => format!("priority list for `{k}`"),
            Ranking::Unordered => "no priority list".into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderChoice {
    pub id: String,
    /// The package's name for this provider (`name` in `[providers.<id>]`, else the package name).
    pub native_name: String,
    /// The package that makes this provider usable, when its state is `Bootstrap`.
    pub bootstrap: Option<String>,
    /// Kept because the package is already installed by this provider.
    pub sticky: bool,
}

/// The outcome of provider selection for one package.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderSelection {
    /// `None` when nothing could be chosen (see the `no-provider-satisfies` error) or the
    /// package is files-only.
    pub chosen: Option<ProviderChoice>,
    /// Every candidate, best-ranked first.
    pub candidates: Vec<CandidateReport>,
    pub ranking: Ranking,
    /// `dots migrate <pkg> --to <preferred>` when sticky choice overrode the ranking.
    pub hint: Option<String>,
}

impl ProviderSelection {
    /// Human reasons, one per line: skips, the choice and the sticky hint.
    pub fn reasons(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for c in &self.candidates {
            if let CandidateOutcome::Skipped(_) = c.outcome {
                out.push(c.line());
            }
        }
        if let Some(ch) = &self.chosen {
            let mut s = if ch.sticky {
                format!(
                    "{} chosen: already installed (sticky, ranking would prefer another)",
                    ch.id
                )
            } else {
                format!(
                    "{} chosen: first usable in the {}",
                    ch.id,
                    self.ranking.describe()
                )
            };
            if let Some(b) = &ch.bootstrap {
                s.push_str(&format!("; needs `{b}` installed first"));
            }
            out.push(s);
        }
        if let Some(h) = &self.hint {
            out.push(format!("hint: {h}"));
        }
        out
    }

    pub fn to_json(&self) -> Value {
        json!({
            "chosen": self.chosen.as_ref().map(|c| json!({
                "id": c.id,
                "name": c.native_name,
                "sticky": c.sticky,
                "bootstrap": c.bootstrap,
            })),
            "ranking": self.ranking.describe(),
            "candidates": self.candidates.iter().map(|c| {
                let mut v = json!({
                    "id": c.id,
                    "outcome": match &c.outcome {
                        CandidateOutcome::Chosen => "chosen",
                        CandidateOutcome::Standby => "standby",
                        CandidateOutcome::Skipped(_) => "skipped",
                    },
                    "installed": c.installed,
                });
                if let CandidateOutcome::Skipped(r) = &c.outcome {
                    v["skip"] = json!(r.kind());
                    v["reason"] = json!(r.to_string());
                }
                v
            }).collect::<Vec<_>>(),
            "reasons": self.reasons(),
            "hint": self.hint,
        })
    }
}

#[derive(Debug, Clone)]
pub struct PackageResolution {
    pub name: String,
    pub state: PackageState,
    /// Every inclusion path, active or not.
    pub inclusions: Vec<Inclusion>,
    /// Present for active packages.
    pub provider: Option<ProviderSelection>,
    pub constraints: ConstraintSet,
}

impl PackageResolution {
    pub fn is_active(&self) -> bool {
        self.state == PackageState::Active
    }

    pub fn chosen_provider(&self) -> Option<&ProviderChoice> {
        self.provider.as_ref().and_then(|p| p.chosen.as_ref())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EdgeKind {
    Requires,
    After,
    Bootstrap,
}

/// `package` must come after `dependency`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Edge {
    pub package: String,
    pub dependency: String,
    pub kind: EdgeKind,
}

/// A package dropped because `by` replaces it. The plan uninstalls it if dots owns it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Replacement {
    pub package: String,
    pub by: String,
}

#[derive(Debug, Clone, Default)]
pub struct Resolution {
    /// Every package that was reached, in any state.
    pub packages: BTreeMap<String, PackageResolution>,
    /// Active packages, dependencies first.
    pub order: Vec<String>,
    pub edges: Vec<Edge>,
    pub replaced: Vec<Replacement>,
    pub errors: Vec<Diagnostic>,
    pub warnings: Vec<Diagnostic>,
}

impl Resolution {
    pub fn get(&self, name: &str) -> Option<&PackageResolution> {
        self.packages.get(name)
    }

    pub fn active(&self) -> impl Iterator<Item = &PackageResolution> {
        self.order.iter().filter_map(|n| self.packages.get(n))
    }

    pub fn is_ok(&self) -> bool {
        self.errors.is_empty()
    }

    pub fn error_codes(&self) -> Vec<&str> {
        self.errors.iter().map(|d| d.code.as_str()).collect()
    }
}
