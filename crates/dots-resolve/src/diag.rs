//! Diagnostic helpers: provenance with a position, and condition errors pointed at the
//! offending token inside the string they came from.

use dots_core::{Diagnostic, Provenance};
use dots_facts::CondError;

/// A 1-based `(line, col)` in a file, when the loader recorded one.
pub(crate) type At = Option<(u32, u32)>;

pub(crate) fn prov_at(source: &str, file: &str, at: At) -> Provenance {
    Provenance {
        source: source.to_string(),
        file: Some(file.to_string()),
        line: at.map(|a| a.0),
        col: at.map(|a| a.1),
    }
}

/// `at` is where the string value starts (its opening quote); the error's column is relative
/// to the string's content, which starts one column later.
pub(crate) fn cond_diag(e: &CondError, file: &str, at: At) -> Diagnostic {
    let mut d = e.to_diagnostic(Some(file), at.map(|a| a.0));
    if let (Some(p), Some((_, c))) = (d.provenance.first_mut(), at) {
        p.col = Some(c + e.col.map_or(0, |x| x as u32));
    }
    d
}

/// Position of `name` in a list-valued key (`requires`, `after`, ...): the entry itself,
/// or its `name` when it is a table.
pub(crate) fn entry_at(
    spans: &std::collections::BTreeMap<String, (u32, u32)>,
    list: &str,
    index: usize,
) -> At {
    spans
        .get(&format!("{list}.{index}.name"))
        .or_else(|| spans.get(&format!("{list}.{index}")))
        .copied()
}

/// Where the first package of a cycle names the second: its `requires` or `after` entry.
pub(crate) fn cycle_at(repo: &dots_model::Repo, cycle: &[String]) -> At {
    let pkg = repo.packages.get(&cycle[0])?;
    let next = cycle.get(1)?;
    if let Some(i) = pkg.requires.iter().position(|r| r.name() == next) {
        return entry_at(&pkg.spans, "requires", i);
    }
    let i = pkg.after.iter().position(|a| a == next)?;
    pkg.spans.get(&format!("after.{i}")).copied()
}
