//! Version helpers for the diff: normalisation, comparison and the wanted-version grammar
//! (ARCHITECTURE section 8.1).

use std::cmp::Ordering;

/// Drop a leading `v` and a Debian epoch, and cut at the packaging revision (`+`, `_`, `~`).
pub fn normalize(v: &str) -> String {
    let v = v.trim();
    let v = v.strip_prefix('v').unwrap_or(v);
    let v = match v.split_once(':') {
        Some((e, rest)) if e.chars().all(|c| c.is_ascii_digit()) => rest,
        _ => v,
    };
    let v = v.split(['+', '_', '~']).next().unwrap_or(v);
    // A Debian-style packaging revision: `-1`, `-0ubuntu1`, `-2build1`.
    for (i, _) in v.match_indices('-') {
        let rest = &v[i + 1..];
        let digits = rest.chars().take_while(char::is_ascii_digit).count();
        let tail = &rest[digits..];
        if digits > 0
            && (tail.is_empty()
                || ["ubuntu", "deb", "build"]
                    .iter()
                    .any(|p| tail.starts_with(p)))
        {
            return v[..i].to_string();
        }
    }
    v.to_string()
}

fn components(v: &str) -> Vec<String> {
    normalize(v).split(['.', '-']).map(str::to_string).collect()
}

/// Component-wise comparison; an alphanumeric component is lower than a numeric one.
pub fn compare(a: &str, b: &str) -> Ordering {
    let (ca, cb) = (components(a), components(b));
    for i in 0..ca.len().max(cb.len()) {
        let (x, y) = (ca.get(i), cb.get(i));
        let ord = match (x, y) {
            (Some(x), Some(y)) => match (x.parse::<u64>(), y.parse::<u64>()) {
                (Ok(x), Ok(y)) => x.cmp(&y),
                (Ok(_), Err(_)) => Ordering::Greater,
                (Err(_), Ok(_)) => Ordering::Less,
                (Err(_), Err(_)) => x.cmp(y),
            },
            (Some(x), None) => {
                if x.parse::<u64>() == Ok(0) {
                    Ordering::Equal
                } else if x.parse::<u64>().is_ok() {
                    Ordering::Greater
                } else {
                    Ordering::Less
                }
            }
            (None, Some(y)) => {
                if y.parse::<u64>() == Ok(0) {
                    Ordering::Equal
                } else if y.parse::<u64>().is_ok() {
                    Ordering::Less
                } else {
                    Ordering::Greater
                }
            }
            (None, None) => Ordering::Equal,
        };
        if ord != Ordering::Equal {
            return ord;
        }
    }
    Ordering::Equal
}

/// A parsed desired version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Wanted {
    Latest,
    Exact(String),
    Prefix(String),
    Floor(String),
    Git(String),
}

fn numeric(s: &str) -> bool {
    !s.is_empty()
        && s.split('.')
            .all(|c| !c.is_empty() && c.chars().all(|ch| ch.is_ascii_digit()))
}

pub fn parse_wanted(spec: Option<&str>) -> Wanted {
    let s = spec.map(str::trim).unwrap_or("");
    if s.is_empty() || s == "latest" {
        return Wanted::Latest;
    }
    if let Some(r) = s.strip_prefix("git:") {
        return Wanted::Git(r.to_string());
    }
    if let Some(f) = s.strip_prefix(">=") {
        return Wanted::Floor(f.trim().to_string());
    }
    let bare = s.strip_prefix('v').unwrap_or(s);
    if numeric(bare) {
        return if bare.split('.').count() >= 3 {
            Wanted::Exact(bare.to_string())
        } else {
            Wanted::Prefix(bare.to_string())
        };
    }
    // Not in the grammar (a lint matter): treat as exact so the user gets what they wrote.
    Wanted::Exact(s.to_string())
}

fn matches_prefix(installed: &str, prefix: &str) -> bool {
    let have = components(installed);
    let want: Vec<String> = prefix.split('.').map(str::to_string).collect();
    want.len() <= have.len() && want.iter().zip(&have).all(|(w, h)| w == h)
}

/// What the diff should do about an installed version.
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
    let Some(have) = installed else {
        return Change::Install(match wanted {
            Wanted::Exact(v) => Some(v.clone()),
            _ => None,
        });
    };
    match wanted {
        Wanted::Latest | Wanted::Git(_) => Change::None,
        Wanted::Exact(v) => match compare(have, v) {
            Ordering::Equal => Change::None,
            Ordering::Less => Change::Upgrade(Some(v.clone())),
            Ordering::Greater => Change::Downgrade(v.clone()),
        },
        Wanted::Prefix(p) => {
            if matches_prefix(have, p) {
                Change::None
            } else {
                Change::Upgrade(None)
            }
        }
        Wanted::Floor(f) => {
            if compare(have, f) == Ordering::Less {
                Change::Upgrade(None)
            } else {
                Change::None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
            Change::Upgrade(None)
        );
        assert_eq!(
            diff(&parse_wanted(Some(">=1.2")), Some("1.1")),
            Change::Upgrade(None)
        );
    }
}
