//! `dots lock --fix`: rebuild a lock that fails to parse or carries merge-conflict markers,
//! keeping every row that is still valid.

use crate::{Lock, SCHEMA};

/// What the repair did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FixReport {
    pub had_markers: bool,
    pub kept: usize,
    /// One line per row that could not be kept.
    pub dropped: Vec<String>,
    /// Rows that appeared on both sides of a conflict with different contents (the newer
    /// `locked_at` won; each loser is also listed in `dropped`).
    pub duplicates: usize,
}

/// Split a conflicted file into the full "ours" and the full "theirs" text: shared lines go to
/// both, each conflict hunk's sides go to their own text, and a diff3 base section is dropped.
/// Without markers, both texts are the input.
fn split_sides(text: &str) -> (String, String, bool) {
    #[derive(PartialEq)]
    enum Part {
        Common,
        Ours,
        Base,
        Theirs,
    }
    let (mut ours, mut theirs) = (String::new(), String::new());
    let mut had = false;
    let mut part = Part::Common;
    for line in text.lines() {
        if line.starts_with("<<<<<<<") {
            had = true;
            part = Part::Ours;
        } else if line.starts_with("|||||||") && part == Part::Ours {
            part = Part::Base;
        } else if line == "=======" && matches!(part, Part::Ours | Part::Base) {
            part = Part::Theirs;
        } else if line.starts_with(">>>>>>>") && part != Part::Common {
            part = Part::Common;
        } else {
            let line = format!("{line}\n");
            if matches!(part, Part::Common | Part::Ours) {
                ours.push_str(&line);
            }
            if matches!(part, Part::Common | Part::Theirs) {
                theirs.push_str(&line);
            }
        }
    }
    (ours, theirs, had)
}

/// Split into `(kind, text)` blocks at `[[...]]` headers; anything before the first header
/// (comments, `schema`) is dropped.
fn blocks(text: &str) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for line in text.lines() {
        let t = line.trim();
        if t.starts_with("[[") && t.ends_with("]]") {
            out.push((t[2..t.len() - 2].trim().to_string(), String::new()));
        } else if t.starts_with('[') {
            // A plain table is not part of the lock format; end the current block.
            out.push((String::new(), String::new()));
        } else if let Some((_, body)) = out.last_mut() {
            body.push_str(line);
            body.push('\n');
        }
    }
    out
}

/// Is `a` a better row to keep than `b`: newer `locked_at`, a tie going to the higher version.
fn newer(a: &crate::PkgRow, b: &crate::PkgRow) -> bool {
    match a.locked_at.cmp(&b.locked_at) {
        std::cmp::Ordering::Equal => match (&a.version, &b.version) {
            (Some(x), Some(y)) => dots_model::vspec::compare(x, y).is_gt(),
            (Some(_), None) => true,
            _ => false,
        },
        o => o.is_gt(),
    }
}

/// Fold `from` into `into`; a key present in both keeps one row and reports the other.
fn merge(into: &mut Lock, from: Lock, report: &mut FixReport) {
    for r in from.pkgs {
        match into
            .pkgs
            .iter_mut()
            .find(|x| x.name == r.name && x.platform == r.platform)
        {
            Some(old) if *old == r => {}
            Some(old) => {
                report.duplicates += 1;
                let (keep, lose) = if newer(&r, old) {
                    (r.shown(), old.shown())
                } else {
                    (old.shown(), r.shown())
                };
                report.dropped.push(format!(
                    "collision on `{}` ({}): kept {keep} (newer), dropped {lose}",
                    old.name, old.platform
                ));
                if newer(&r, old) {
                    *old = r;
                }
            }
            None => into.pkgs.push(r),
        }
    }
    for r in from.upstream {
        match into.upstream.iter_mut().find(|x| x.alias == r.alias) {
            Some(old) if *old == r => {}
            Some(old) => {
                report.duplicates += 1;
                report.dropped.push(format!(
                    "collision on upstream `{}`: kept rev {}, dropped rev {}",
                    r.alias, r.rev, old.rev
                ));
                *old = r;
            }
            None => into.upstream.push(r),
        }
    }
    for r in from.content {
        match into
            .content
            .iter_mut()
            .find(|x| x.item == r.item && x.rev == r.rev)
        {
            Some(old) if *old == r => {}
            Some(old) => {
                report.duplicates += 1;
                report
                    .dropped
                    .push(format!("collision on content `{}`: kept one row", r.item));
                *old = r;
            }
            None => into.content.push(r),
        }
    }
}

/// Parse one side leniently: rows that fail are reported and skipped.
fn parse_side(text: &str, report: &mut FixReport) -> Lock {
    let mut lock = Lock::default();
    for (kind, body) in blocks(text) {
        if !matches!(kind.as_str(), "pkg" | "upstream" | "content") {
            if !body.trim().is_empty() || !kind.is_empty() {
                let msg = format!("unknown block `{kind}`");
                if !report.dropped.contains(&msg) {
                    report.dropped.push(msg);
                }
            }
            continue;
        }
        match Lock::parse(&format!("schema = {SCHEMA}\n[[{kind}]]\n{body}")) {
            Ok(l) => merge(&mut lock, l, report),
            Err(e) => {
                let name = body
                    .lines()
                    .find_map(|l| l.strip_prefix("name = ").or(l.strip_prefix("alias = ")))
                    .unwrap_or("?")
                    .trim_matches('"')
                    .to_string();
                let msg = format!("[[{kind}]] `{name}`: {e}");
                if !report.dropped.contains(&msg) {
                    report.dropped.push(msg);
                }
            }
        }
    }
    lock
}

/// Repair `text`: both sides of every conflict are reconstructed and parsed whole, then their
/// rows are unioned by `(name, platform)`.
pub fn fix_text(text: &str) -> (Lock, FixReport) {
    let (ours_text, theirs_text, had_markers) = split_sides(text);
    let mut report = FixReport {
        had_markers,
        ..FixReport::default()
    };
    let mut lock = parse_side(&ours_text, &mut report);
    if had_markers {
        let theirs = parse_side(&theirs_text, &mut report);
        merge(&mut lock, theirs, &mut report);
    }
    lock.normalize();
    report.kept = lock.pkgs.len() + lock.upstream.len() + lock.content.len();
    (lock, report)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_both_sides_of_a_conflict() {
        let text = "\
schema = 1

[[pkg]]
name = \"bat\"
platform = \"*\"
provider = \"cargo\"
version = \"1.0.0\"
locked_at = \"2026-01-01T00:00:00Z\"

<<<<<<< HEAD
[[pkg]]
name = \"fd\"
platform = \"*\"
provider = \"cargo\"
version = \"9.0.0\"
locked_at = \"2026-01-02T00:00:00Z\"
=======
[[pkg]]
name = \"rg\"
platform = \"*\"
provider = \"cargo\"
version = \"14.0.0\"
locked_at = \"2026-01-03T00:00:00Z\"
>>>>>>> feature
";
        let (lock, rep) = fix_text(text);
        assert!(rep.had_markers);
        assert_eq!(lock.pkgs.len(), 3);
        assert!(rep.dropped.is_empty());
    }

    #[test]
    fn conflict_inside_a_block_prefers_the_newer_side() {
        let text = "\
schema = 1

[[pkg]]
name = \"bat\"
platform = \"*\"
provider = \"cargo\"
<<<<<<< HEAD
version = \"1.1.0\"
locked_at = \"2026-02-01T00:00:00Z\"
=======
version = \"1.0.0\"
locked_at = \"2026-01-01T00:00:00Z\"
>>>>>>> x
";
        let (lock, _) = fix_text(text);
        assert_eq!(lock.pkgs[0].version.as_deref(), Some("1.1.0"));
    }

    #[test]
    fn drops_invalid_rows_and_keeps_the_rest() {
        let text = "\
schema = 1

[[pkg]]
name = \"good\"
platform = \"*\"
provider = \"cargo\"
version = \"1.0.0\"

[[pkg]]
name = \"bad\"
platform = \"*\"

[[pkg]]
name = \"garbled
";
        let (lock, rep) = fix_text(text);
        assert_eq!(lock.pkgs.len(), 1);
        assert_eq!(rep.dropped.len(), 2);
    }
}
