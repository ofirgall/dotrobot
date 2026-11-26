//! The version grammar (ARCHITECTURE section 8.1): parse, compare, match and intersect.
//!
//! Five forms and nothing else: `latest`, exact `X.Y.Z`, prefix `X` / `X.Y`, floor `>=X[.Y[.Z]]`
//! and `git:<ref>`. Everything here is pure; provenance and provider choice live in the
//! resolver, the lock rules in `dots-lock`.

use std::cmp::Ordering;
use std::fmt;

/// What the `version-spec-invalid` hint lists.
pub const FORMS_HINT: &str = "Use one of: `latest`, an exact version (`1.2.3`), a prefix (`1` or `1.2`), a floor (`>=1.2`), or `git:<ref>`.";

/// Drop a leading `v` and a Debian epoch, and cut at the packaging revision (`+`, `_`, `~`,
/// `-0ubuntu1`, `-1`). The raw string stays the display and lock value.
pub fn normalize(v: &str) -> String {
    let v = v.trim();
    let v = v.strip_prefix('v').unwrap_or(v);
    let v = match v.split_once(':') {
        Some((e, rest)) if !e.is_empty() && e.chars().all(|c| c.is_ascii_digit()) => rest,
        _ => v,
    };
    let v = v.split(['+', '_', '~']).next().unwrap_or(v);
    // One trailing package revision (`-1`, `-0ubuntu1`, `-1build2`): the core must have dots
    // (so a date such as `2024-01-15` stays whole) and the revision is a small integer (so a
    // build stamp such as `2.0.0-20240101` stays whole).
    if let Some((core, rest)) = v.rsplit_once('-') {
        let digits = rest.chars().take_while(char::is_ascii_digit).count();
        let tail = &rest[digits..];
        if core.contains('.')
            && (1..=2).contains(&digits)
            && (tail.is_empty()
                || ["ubuntu", "deb", "build"]
                    .iter()
                    .any(|p| tail.starts_with(p)))
        {
            return core.to_string();
        }
    }
    v.to_string()
}

fn components(v: &str) -> Vec<String> {
    normalize(v).split(['.', '-']).map(str::to_string).collect()
}

/// A component's leading number and what follows it (`5a` is `(5, "a")`).
fn split_comp(c: &str) -> Option<(u64, &str)> {
    let digits = c.chars().take_while(char::is_ascii_digit).count();
    let n = c[..digits].parse().ok()?;
    Some((n, &c[digits..]))
}

/// A bare trailing letter (`3.5a`, `1.1.1w`) is a later release; any longer tail (`0rc1`) is a
/// pre-release of that number.
fn is_letter_release(suffix: &str) -> bool {
    suffix.len() == 1 && suffix.chars().all(|c| c.is_ascii_alphabetic())
}

/// Orders suffixes of one number: a pre-release tail, then none, then `a`, `b`, ...
fn suffix_rank(suffix: &str) -> (u8, &str) {
    if suffix.is_empty() {
        (1, "")
    } else if is_letter_release(suffix) {
        (2, suffix)
    } else {
        (0, suffix)
    }
}

/// Component-wise comparison; an alphanumeric component is lower than a numeric one, a bare
/// letter suffix orders after the plain number (`3.5 < 3.5a < 3.5b < 3.6`), and trailing zeros
/// do not matter (`1.2.0 == 1.2`).
pub fn compare(a: &str, b: &str) -> Ordering {
    let (ca, cb) = (components(a), components(b));
    for i in 0..ca.len().max(cb.len()) {
        let ord = match (ca.get(i), cb.get(i)) {
            (Some(x), Some(y)) => match (split_comp(x), split_comp(y)) {
                (Some((x, xs)), Some((y, ys))) => x
                    .cmp(&y)
                    .then_with(|| suffix_rank(xs).cmp(&suffix_rank(ys))),
                (Some(_), None) => Ordering::Greater,
                (None, Some(_)) => Ordering::Less,
                (None, None) => x.cmp(y),
            },
            (Some(x), None) => match split_comp(x) {
                Some((0, s)) => match suffix_rank(s).0 {
                    0 => Ordering::Less,
                    1 => Ordering::Equal,
                    _ => Ordering::Greater,
                },
                Some(_) => Ordering::Greater,
                None => Ordering::Less,
            },
            (None, Some(y)) => match split_comp(y) {
                Some((0, s)) => match suffix_rank(s).0 {
                    0 => Ordering::Greater,
                    1 => Ordering::Equal,
                    _ => Ordering::Less,
                },
                Some(_) => Ordering::Less,
                None => Ordering::Greater,
            },
            (None, None) => Ordering::Equal,
        };
        if ord != Ordering::Equal {
            return ord;
        }
    }
    Ordering::Equal
}

/// Can prefix and floor reason about this version at all? A date stamp parses (it is numeric);
/// a hash does not.
pub fn parseable(v: &str) -> bool {
    components(v)
        .first()
        .is_some_and(|c| !c.is_empty() && c.chars().all(|ch| ch.is_ascii_digit()))
}

/// A pre-release: a separator-led tag (`-rc1`, `-beta`, `.alpha2`, `-pre`, `-dev`) or a number
/// followed by a longer letter tail (`0rc1`). A bare trailing letter (`3.5a`, `1.1.1w`) and a
/// numeric `-` segment (a date or build stamp) are releases.
pub fn is_prerelease(v: &str) -> bool {
    let n = normalize(v);
    let mut dashes = n.split('-');
    let head = dashes.next().unwrap_or("");
    if dashes.any(|seg| seg.starts_with(|c: char| c.is_ascii_alphabetic())) {
        return true;
    }
    head.split('.').skip(1).any(|c| {
        c.starts_with(|ch: char| ch.is_ascii_alphabetic())
            || split_comp(c).is_some_and(|(_, s)| !s.is_empty() && !is_letter_release(s))
    })
}

/// Do two raw version strings name the same upstream version?
pub fn same(a: &str, b: &str) -> bool {
    compare(a, b) == Ordering::Equal
}

/// A version spec outside the grammar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpecError;

impl fmt::Display for SpecError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("not a valid version spec")
    }
}

impl std::error::Error for SpecError {}

/// A parsed version spec, or (only from [`Spec::intersect`]) a bounded prefix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Spec {
    Latest,
    /// `X.Y.Z` (three or more components, optionally a pre-release suffix).
    Exact(String),
    /// `X` or `X.Y`, matched component by component.
    Prefix(Vec<u64>),
    /// `>=X[.Y[.Z]]`.
    Floor(Vec<u64>),
    /// `git:<ref>`.
    Git(String),
    /// A prefix whose floor lies inside it: `1.4` intersected with `>=1.4.5`.
    Bounded {
        prefix: Vec<u64>,
        floor: Vec<u64>,
    },
}

fn nums(s: &str) -> Option<Vec<u64>> {
    if s.is_empty() {
        return None;
    }
    s.split('.')
        .map(|c| {
            (!c.is_empty() && c.chars().all(|ch| ch.is_ascii_digit()))
                .then(|| c.parse().ok())
                .flatten()
        })
        .collect()
}

fn join(v: &[u64]) -> String {
    v.iter().map(u64::to_string).collect::<Vec<_>>().join(".")
}

/// Compare numeric component lists, padding with zeros.
fn cmp_nums(a: &[u64], b: &[u64]) -> Ordering {
    for i in 0..a.len().max(b.len()) {
        let o = a.get(i).unwrap_or(&0).cmp(b.get(i).unwrap_or(&0));
        if o != Ordering::Equal {
            return o;
        }
    }
    Ordering::Equal
}

impl Spec {
    /// Parse a spec. Surrounding whitespace is ignored; anything outside the grammar is `Err`.
    pub fn parse(s: &str) -> Result<Spec, SpecError> {
        let s = s.trim();
        if s == "latest" {
            return Ok(Spec::Latest);
        }
        if let Some(r) = s.strip_prefix("git:") {
            return if !r.is_empty() && !r.contains(char::is_whitespace) {
                Ok(Spec::Git(r.to_string()))
            } else {
                Err(SpecError)
            };
        }
        if let Some(f) = s.strip_prefix(">=") {
            let n = nums(f.trim()).ok_or(SpecError)?;
            return if n.len() <= 3 {
                Ok(Spec::Floor(n))
            } else {
                Err(SpecError)
            };
        }
        let (core, pre) = match s.split_once('-') {
            Some((c, p)) => (c, Some(p)),
            None => (s, None),
        };
        let n = nums(core).ok_or(SpecError)?;
        match (n.len(), pre) {
            (1 | 2, None) => Ok(Spec::Prefix(n)),
            (3.., None) => Ok(Spec::Exact(s.to_string())),
            (3.., Some(p))
                if !p.is_empty()
                    && p.chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-') =>
            {
                Ok(Spec::Exact(s.to_string()))
            }
            _ => Err(SpecError),
        }
    }

    /// An omitted `version` means `latest`.
    pub fn parse_opt(s: Option<&str>) -> Result<Spec, SpecError> {
        s.map_or(Ok(Spec::Latest), Spec::parse)
    }

    pub fn is_latest(&self) -> bool {
        matches!(self, Spec::Latest)
    }

    pub fn is_git(&self) -> bool {
        matches!(self, Spec::Git(_))
    }

    /// A numeric constraint (prefix, floor, or both).
    pub fn is_range(&self) -> bool {
        matches!(
            self,
            Spec::Prefix(_) | Spec::Floor(_) | Spec::Bounded { .. }
        )
    }

    /// Does `version` satisfy the spec? `latest` accepts anything; prefix and floor never accept
    /// a pre-release or a version they cannot parse; exact compares upstream versions; `git:`
    /// accepts a version equal to the ref (a tag), and `satisfied_by_rev` handles shas.
    pub fn admits(&self, version: &str) -> bool {
        match self {
            Spec::Latest => true,
            Spec::Exact(e) => same(version, e),
            Spec::Git(r) => {
                version == r || version.trim_start_matches('v') == r.trim_start_matches('v')
            }
            Spec::Prefix(p) => prefix_matches(version, p),
            Spec::Floor(f) => floor_matches(version, f),
            Spec::Bounded { prefix, floor } => {
                prefix_matches(version, prefix) && floor_matches(version, floor)
            }
        }
    }

    /// The newest of `candidates` that satisfies the spec. Pre-releases are only ever chosen by
    /// an exact spec; `latest` skips them too.
    pub fn newest<'a>(&self, candidates: impl IntoIterator<Item = &'a str>) -> Option<&'a str> {
        candidates
            .into_iter()
            .filter(|c| {
                self.admits(c)
                    && (matches!(self, Spec::Exact(_) | Spec::Git(_)) || !is_prerelease(c))
            })
            .max_by(|a, b| compare(a, b))
    }

    /// Intersect two specs; `Err` carries why they cannot both hold.
    pub fn intersect(&self, other: &Spec) -> Result<Spec, String> {
        use Spec::*;
        match (self, other) {
            (Latest, x) | (x, Latest) => Ok(x.clone()),
            (Git(a), Git(b)) => {
                if a == b {
                    Ok(Git(a.clone()))
                } else {
                    Err(format!("`git:{a}` and `git:{b}` name different refs"))
                }
            }
            (Git(r), x) | (x, Git(r)) => Err(format!(
                "`git:{r}` is a ref, and `{x}` is a version: a ref's version cannot be reasoned about"
            )),
            (Exact(a), x) | (x, Exact(a)) => {
                if x.admits(a) {
                    Ok(Exact(a.clone()))
                } else {
                    Err(format!("`{a}` does not satisfy `{x}`"))
                }
            }
            (a, b) => {
                let (pa, fa) = a.parts();
                let (pb, fb) = b.parts();
                let prefix = match (pa, pb) {
                    (Some(x), Some(y)) => {
                        let (short, long) = if x.len() <= y.len() { (x, y) } else { (y, x) };
                        if long.starts_with(short) {
                            Some(long.to_vec())
                        } else {
                            return Err(format!(
                                "prefix `{}` and prefix `{}` do not overlap",
                                join(x),
                                join(y)
                            ));
                        }
                    }
                    (Some(x), None) | (None, Some(x)) => Some(x.to_vec()),
                    (None, None) => None,
                };
                let floor = match (fa, fb) {
                    (Some(x), Some(y)) => Some(
                        if cmp_nums(x, y) == Ordering::Less {
                            y
                        } else {
                            x
                        }
                        .to_vec(),
                    ),
                    (Some(x), None) | (None, Some(x)) => Some(x.to_vec()),
                    (None, None) => None,
                };
                Ok(match (prefix, floor) {
                    (Some(p), Some(f)) => {
                        // The highest version under the prefix is `p.∞`: it reaches the floor
                        // iff `p >= f` over the prefix's components.
                        let head: Vec<u64> =
                            (0..p.len()).map(|i| *f.get(i).unwrap_or(&0)).collect();
                        if cmp_nums(&p, &head) == Ordering::Less {
                            return Err(format!(
                                "prefix `{}` is below the floor `>={}`",
                                join(&p),
                                join(&f)
                            ));
                        }
                        // A floor at or below the prefix's own start adds nothing.
                        let start: Vec<u64> = p.clone();
                        if cmp_nums(&f, &start) != Ordering::Greater {
                            Prefix(p)
                        } else {
                            Bounded {
                                prefix: p,
                                floor: f,
                            }
                        }
                    }
                    (Some(p), None) => Prefix(p),
                    (None, Some(f)) => Floor(f),
                    (None, None) => Latest,
                })
            }
        }
    }

    fn parts(&self) -> (Option<&[u64]>, Option<&[u64]>) {
        match self {
            Spec::Prefix(p) => (Some(p), None),
            Spec::Floor(f) => (None, Some(f)),
            Spec::Bounded { prefix, floor } => (Some(prefix), Some(floor)),
            _ => (None, None),
        }
    }
}

fn prefix_matches(version: &str, p: &[u64]) -> bool {
    if !parseable(version) || is_prerelease(version) {
        return false;
    }
    let have = components(version);
    p.len() <= have.len()
        && p.iter()
            .zip(&have)
            .all(|(w, h)| split_comp(h).map(|(n, _)| n) == Some(*w))
}

fn floor_matches(version: &str, f: &[u64]) -> bool {
    parseable(version) && !is_prerelease(version) && compare(version, &join(f)) != Ordering::Less
}

impl fmt::Display for Spec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Spec::Latest => f.write_str("latest"),
            Spec::Exact(e) => f.write_str(e),
            Spec::Prefix(p) => f.write_str(&join(p)),
            Spec::Floor(p) => write!(f, ">={}", join(p)),
            Spec::Git(r) => write!(f, "git:{r}"),
            Spec::Bounded { prefix, floor } => {
                write!(f, "{} (>={})", join(prefix), join(floor))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> Spec {
        Spec::parse(s).unwrap()
    }

    #[test]
    fn grammar() {
        assert_eq!(p("latest"), Spec::Latest);
        assert_eq!(p("1.2.3"), Spec::Exact("1.2.3".into()));
        assert_eq!(p("1.2.0-rc1"), Spec::Exact("1.2.0-rc1".into()));
        assert_eq!(p("0.11"), Spec::Prefix(vec![0, 11]));
        assert_eq!(p("22"), Spec::Prefix(vec![22]));
        assert_eq!(p(">=1.2"), Spec::Floor(vec![1, 2]));
        assert_eq!(p(">= 1"), Spec::Floor(vec![1]));
        assert_eq!(p("git:v0.4.1"), Spec::Git("v0.4.1".into()));
        for bad in [
            "~1.2",
            "^1.2",
            ">=1.2,<2",
            "1.2.x",
            "",
            "v1.2",
            ">1.2",
            "git:",
            ">=1.2.3.4",
            "1.",
            "*",
            "1.2-rc1",
        ] {
            assert!(Spec::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn matching() {
        assert!(p("1.2").admits("1.2.17"));
        assert!(!p("1.2").admits("1.20.0"));
        assert!(!p("1.2").admits("1.3.0"));
        assert!(p(">=1.2").admits("2.0"));
        assert!(!p(">=1.2").admits("1.1.9"));
        // pre-releases are never chosen by a prefix, floor or latest
        assert!(!p("1.2").admits("1.2.0-rc1"));
        assert!(!p(">=1.0").admits("1.2.0-beta2"));
        assert_eq!(p("latest").newest(["1.0", "2.0.0-rc1", "1.9"]), Some("1.9"));
        assert!(p("1.2.0-rc1").admits("1.2.0-rc1"));
        // unparseable versions: only latest and exact apply
        assert!(!p("1").admits("9e1c0b7"));
        assert!(!p(">=1").admits("9e1c0b7"));
        assert!(p("latest").admits("9e1c0b7"));
        assert!(p("1.2.3").admits("1:1.2.3-0ubuntu1"));
        assert_eq!(
            p("0.11").newest(["0.10.9", "0.11.0", "0.11.4", "0.12.0"]),
            Some("0.11.4")
        );
    }

    #[test]
    fn git_ref_v_prefix_is_ignored_on_both_sides() {
        assert!(p("git:v0.4.1").admits("0.4.1"));
        assert!(p("git:0.4.1").admits("v0.4.1"));
        assert!(!p("git:v0.4.1").admits("0.4.2"));
        assert!(!p("git:main").admits("0.4.1"));
        assert!(!p("git:v0.4.1").admits("main"));
    }

    #[test]
    fn letter_suffix_releases_are_not_prereleases() {
        for v in ["3.5a", "1.1.1w", "3.5", "2024-01-15", "2.0.0-20240101"] {
            assert!(!is_prerelease(v), "{v}");
        }
        for v in [
            "1.0-rc1",
            "1.0.0-beta",
            "1.0.alpha2",
            "1.0-pre",
            "1.0-dev",
            "2.0rc1",
        ] {
            assert!(is_prerelease(v), "{v}");
        }
        assert!(p(">=3.2").admits("3.5a"));
        assert!(p("3.5").admits("3.5a"));
        assert_eq!(p("latest").newest(["3.5", "3.5a", "3.4"]), Some("3.5a"));
    }

    #[test]
    fn letter_suffix_ordering() {
        let ord = ["3.5", "3.5a", "3.5b", "3.6"];
        for w in ord.windows(2) {
            assert_eq!(compare(w[0], w[1]), Ordering::Less, "{} < {}", w[0], w[1]);
            assert_eq!(compare(w[1], w[0]), Ordering::Greater);
        }
        assert_eq!(compare("1.1.1w", "1.1.1"), Ordering::Greater);
        assert_eq!(compare("3.5-rc1", "3.5"), Ordering::Less);
    }

    #[test]
    fn normalize_keeps_dates_and_build_stamps_distinct() {
        assert!(!same("2024-01-15", "2024-01-20"));
        assert!(!same("2.0.0-20240101", "2.0.0"));
        assert!(!same("2.0.0-20240101", "2.0.0-20240102"));
        assert_eq!(normalize("1.2.3-1"), "1.2.3");
        assert_eq!(normalize("1:1.2.3-0ubuntu1"), "1.2.3");
        assert_eq!(normalize("1.2.3-1ubuntu2"), "1.2.3");
        assert_eq!(normalize("1.2.3_1"), "1.2.3");
        assert_eq!(normalize("1.2.3-rc1"), "1.2.3-rc1");
        assert!(same("v1.2.3-1", "1.2.3"));
    }

    #[test]
    fn intersections() {
        assert_eq!(p("latest").intersect(&p("1.4")), Ok(p("1.4")));
        assert_eq!(p(">=1.2").intersect(&p(">=1.5")), Ok(p(">=1.5")));
        assert_eq!(p("1").intersect(&p("1.4")), Ok(p("1.4")));
        assert!(p("1.4").intersect(&p("1.5")).is_err());
        assert!(p("1.4").intersect(&p("2")).is_err());
        assert_eq!(p("1.4").intersect(&p(">=1.2")), Ok(p("1.4")));
        assert_eq!(p(">=1.2").intersect(&p("1.4")), Ok(p("1.4")));
        assert!(p("1.4").intersect(&p(">=1.5")).is_err());
        assert_eq!(p("1.4").intersect(&p(">=1.4")), Ok(p("1.4")));
        assert_eq!(
            p("1.4").intersect(&p(">=1.4.5")),
            Ok(Spec::Bounded {
                prefix: vec![1, 4],
                floor: vec![1, 4, 5]
            })
        );
        assert_eq!(p("1.2.3").intersect(&p("1.2")), Ok(p("1.2.3")));
        assert!(p("1.2.3").intersect(&p(">=1.3")).is_err());
        assert!(p("1.2.3").intersect(&p("1.2.4")).is_err());
        assert_eq!(p("git:main").intersect(&p("git:main")), Ok(p("git:main")));
        assert!(p("git:main").intersect(&p("git:dev")).is_err());
        assert!(p("git:main").intersect(&p("1.2")).is_err());
        assert_eq!(p("git:main").intersect(&p("latest")), Ok(p("git:main")));
    }
}
