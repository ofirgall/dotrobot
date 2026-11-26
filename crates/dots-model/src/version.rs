//! `requires_dots` (`>=X.Y[.Z]`) parsing and the "dots too old" check.

use std::cmp::Ordering;

use dots_core::{Diagnostic, Provenance};

/// A parsed `major.minor.patch` triple (missing components are 0).
pub type Triple = (u64, u64, u64);

fn parse_triple(s: &str) -> Option<Triple> {
    let core = s.split(['-', '+']).next()?;
    let mut parts = core.split('.');
    let major = parts.next()?.trim().parse().ok()?;
    let minor = match parts.next() {
        Some(p) => p.trim().parse().ok()?,
        None => 0,
    };
    let patch = match parts.next() {
        Some(p) => p.trim().parse().ok()?,
        None => 0,
    };
    if parts.next().is_some() {
        return None;
    }
    Some((major, minor, patch))
}

/// Parse `>=X.Y[.Z]` into the minimum version.
pub fn parse_requirement(s: &str) -> Option<Triple> {
    parse_triple(s.trim().strip_prefix(">=")?.trim())
}

/// Is `current` (the running dots version) at or above the requirement?
pub fn satisfies(requirement: &str, current: &str) -> Option<bool> {
    let min = parse_requirement(requirement)?;
    let cur = parse_triple(current)?;
    Some(cur.cmp(&min) != Ordering::Less)
}

/// `dots-too-old` when `requires_dots` is not met by `current`; `None` when it is (or when
/// either side does not parse; the loader already reports unparseable requirements).
pub fn check_requires_dots(requires: &str, current: &str, file: &str) -> Option<Diagnostic> {
    if satisfies(requires, current)? {
        return None;
    }
    Some(
        Diagnostic::from_code(
            "dots-too-old",
            &[("required", requires), ("current", current), ("file", file)],
        )
        .with_provenance(Provenance {
            source: "dots.toml".into(),
            file: Some(file.to_string()),
            line: None,
            col: None,
        }),
    )
}
