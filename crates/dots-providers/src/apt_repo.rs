//! apt repositories (ARCHITECTURE section 5.2, spec `provider-contract` "apt repositories").
//!
//! A package names its repository with `Spec::opts` keys:
//!
//! | key | meaning |
//! |-----|---------|
//! | `repo.ppa` | `owner/name` of a Launchpad PPA (a `Spec::source` is read the same way) |
//! | `repo.url`, `repo.suite`, `repo.components` (comma list), `repo.flat`, `repo.key`, `repo.fingerprint` | a third-party repository; `repo.flat = true` (or a suite path ending in `/` with no components) is a flat repository: no `Components` line |
//! | `pin.origin`, `pin.priority` | optional `apt_preferences` stanza |
//! | `repo_keep` | `true` on a `Remove` op when another wanted package still names the repository |
//!
//! dots owns three files per repository, named after the repository id (never `apt-key`):
//! `/etc/apt/keyrings/dots-<id>.asc` (`.gpg` for a binary key), `/etc/apt/sources.list.d/dots-<id>.sources` (deb822 with
//! `Signed-By`) and, with a pin, `/etc/apt/preferences.d/dots-<id>`.

use std::collections::BTreeMap;

use crate::{ProviderError, Result, Spec, fetch};

pub const KEYRING_DIR: &str = "/etc/apt/keyrings";
pub const SOURCES_DIR: &str = "/etc/apt/sources.list.d";
pub const PREFS_DIR: &str = "/etc/apt/preferences.d";
/// First line of every file dots writes; removal only touches files that carry it.
pub const OWNER_MARK: &str = "# Managed by dots. Do not edit.";
/// Placeholder in a PPA's sources text for the release codename, resolved at exec time.
pub const SUITE_TOKEN: &str = "@SUITE@";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    Ppa {
        owner: String,
        name: String,
    },
    Url {
        url: String,
        suite: String,
        /// Empty for a flat repository.
        components: Vec<String>,
        /// A flat repository: `suite` is a path ending in `/` and there are no components
        /// (`deb https://download.sublimetext.com/ apt/stable/`).
        flat: bool,
        key: String,
        /// Upper-case hex, no spaces.
        fingerprint: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pin {
    pub origin: String,
    pub priority: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AptRepo {
    pub id: String,
    pub source: Source,
    pub pin: Option<Pin>,
}

fn slug(s: &str) -> String {
    let mut out = String::new();
    for c in s.to_ascii_lowercase().chars() {
        if c.is_ascii_alphanumeric() || c == '.' {
            out.push(c);
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    out.trim_matches('-').to_string()
}

fn plain(label: &str, v: &str) -> Result<()> {
    if v.is_empty() || v.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return Err(ProviderError::Other(format!(
            "apt repository: `{label}` must be a non-empty value without spaces"
        )));
    }
    Ok(())
}

fn ppa_part(label: &str, v: &str) -> Result<()> {
    if v.is_empty()
        || !v
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-._+".contains(c))
    {
        return Err(ProviderError::Other(format!(
            "apt repository: invalid PPA {label} `{v}`"
        )));
    }
    Ok(())
}

impl AptRepo {
    /// The repository a spec names, if any.
    pub fn from_spec(spec: &Spec) -> Result<Option<AptRepo>> {
        let ppa = spec.get("repo.ppa").or(spec.source.as_deref());
        let url = spec.get("repo.url");
        let source = match (ppa, url) {
            (Some(_), Some(_)) => {
                return Err(ProviderError::Other(format!(
                    "apt: `{}` sets both a PPA and a repository url; pick one",
                    spec.name
                )));
            }
            (Some(p), None) => {
                let p = p.strip_prefix("ppa:").unwrap_or(p);
                let (owner, name) = p.split_once('/').ok_or_else(|| {
                    ProviderError::Other(format!("apt: PPA `{p}` must look like owner/name"))
                })?;
                ppa_part("owner", owner)?;
                ppa_part("name", name)?;
                Source::Ppa {
                    owner: owner.into(),
                    name: name.into(),
                }
            }
            (None, Some(url)) => {
                let need = |k: &str| {
                    spec.get(k).ok_or_else(|| {
                        ProviderError::Other(format!(
                            "apt: `{}` declares a repository url but no `{}`",
                            spec.name,
                            k.trim_start_matches("repo.")
                        ))
                    })
                };
                if !fetch::valid_url(url) {
                    return Err(ProviderError::Other(format!(
                        "apt: repository url `{url}` must be http(s)"
                    )));
                }
                let suite = need("repo.suite")?;
                plain("suite", suite)?;
                let key = need("repo.key")?;
                if !fetch::valid_url(key) {
                    return Err(ProviderError::Other(format!(
                        "apt: key url `{key}` must be http(s)"
                    )));
                }
                let fp: String = need("repo.fingerprint")?
                    .chars()
                    .filter(|c| !c.is_whitespace())
                    .collect::<String>()
                    .to_ascii_uppercase();
                if fp.len() == 16 && fp.chars().all(|c| c.is_ascii_hexdigit()) {
                    return Err(ProviderError::Other(format!(
                        "apt: `{fp}` is a 16-character key id, not a fingerprint; \
                         use the full 40-character fingerprint"
                    )));
                }
                if fp.len() != 40 || !fp.chars().all(|c| c.is_ascii_hexdigit()) {
                    return Err(ProviderError::Other(format!(
                        "apt: `{fp}` is not a key fingerprint (hex, exactly 40 characters)"
                    )));
                }
                let listed: Vec<String> = spec
                    .get("repo.components")
                    .map(|c| {
                        c.split(',')
                            .map(|c| c.trim().to_string())
                            .filter(|c| !c.is_empty())
                            .collect()
                    })
                    .unwrap_or_default();
                // Flat: asked for (`flat = true` or `components = []`), or implied by a suite
                // that is a path ending in `/` with no components.
                let flat = spec.flag("repo.flat", false)
                    || (spec.get("repo.components").is_none() && suite.ends_with('/'));
                let (suite, components) = if flat {
                    if !listed.is_empty() {
                        return Err(ProviderError::Other(format!(
                            "apt: `{}` is a flat repository (no components) but lists components",
                            spec.name
                        )));
                    }
                    let s = if suite.ends_with('/') {
                        suite.to_string()
                    } else {
                        format!("{suite}/")
                    };
                    (s, Vec::new())
                } else {
                    if suite.ends_with('/') {
                        return Err(ProviderError::Other(format!(
                            "apt: suite `{suite}` is a path (a flat repository): drop `components`"
                        )));
                    }
                    let comps = if spec.get("repo.components").is_some() {
                        listed
                    } else {
                        vec!["main".to_string()]
                    };
                    if comps.is_empty() {
                        return Err(ProviderError::Other(
                            "apt: `components` must name at least one component (or set `flat = true`)"
                                .into(),
                        ));
                    }
                    for c in &comps {
                        plain("components", c)?;
                    }
                    (suite.to_string(), comps)
                };
                plain("url", url)?;
                Source::Url {
                    url: url.into(),
                    suite,
                    components,
                    flat,
                    key: key.into(),
                    fingerprint: fp,
                }
            }
            (None, None) => return Ok(None),
        };
        let pin = match spec.get("pin.origin") {
            Some(origin) => {
                plain("pin.origin", origin)?;
                let priority = match spec.get("pin.priority") {
                    Some(p) => p.parse::<i64>().map_err(|_| {
                        ProviderError::Other(format!("apt: pin priority `{p}` is not a number"))
                    })?,
                    None => 1000,
                };
                Some(Pin {
                    origin: origin.into(),
                    priority,
                })
            }
            None => None,
        };
        let id = match &source {
            Source::Ppa { owner, name } => slug(&format!("ppa-{owner}-{name}")),
            Source::Url { url, suite, .. } => {
                let bare = url.split_once("://").map_or(url.as_str(), |(_, r)| r);
                slug(&format!("{bare}-{suite}"))
            }
        };
        Ok(Some(AptRepo { id, source, pin }))
    }

    /// The keyring of an ASCII-armored key (`.asc`).
    pub fn keyring_path(&self) -> String {
        format!("{KEYRING_DIR}/dots-{}.asc", self.id)
    }
    /// The keyring of a binary (dearmored) key (`.gpg`).
    pub fn binary_keyring_path(&self) -> String {
        format!("{KEYRING_DIR}/dots-{}.gpg", self.id)
    }
    pub fn sources_path(&self) -> String {
        format!("{SOURCES_DIR}/dots-{}.sources", self.id)
    }
    pub fn prefs_path(&self) -> String {
        format!("{PREFS_DIR}/dots-{}", self.id)
    }

    /// Every file dots owns for this repository (the preferences file only with a pin).
    pub fn owned_paths(&self) -> Vec<String> {
        let mut v = vec![self.keyring_path(), self.sources_path()];
        if self.pin.is_some() {
            v.push(self.prefs_path());
        }
        v
    }

    /// The deb822 sources file. A PPA's suite is [`SUITE_TOKEN`] until exec time.
    pub fn sources_text(&self, suite: Option<&str>) -> String {
        self.sources_text_for(suite, &self.keyring_path())
    }

    /// [`Self::sources_text`] signed by `keyring` (the `.asc` or the `.gpg` file).
    pub fn sources_text_for(&self, suite: Option<&str>, keyring: &str) -> String {
        let (uri, suite, components) = match &self.source {
            Source::Ppa { owner, name } => (
                format!("https://ppa.launchpadcontent.net/{owner}/{name}/ubuntu"),
                suite.unwrap_or(SUITE_TOKEN).to_string(),
                "main".to_string(),
            ),
            Source::Url {
                url,
                suite,
                components,
                ..
            } => (url.clone(), suite.clone(), components.join(" ")),
        };
        // A flat repository has no `Components` field.
        let components = if components.is_empty() {
            String::new()
        } else {
            format!("Components: {components}\n")
        };
        format!(
            "{OWNER_MARK}\nTypes: deb\nURIs: {uri}\nSuites: {suite}\n{components}Signed-By: {keyring}\n"
        )
    }

    pub fn prefs_text(&self) -> Option<String> {
        self.pin.as_ref().map(|p| {
            format!(
                "{OWNER_MARK}\nPackage: *\nPin: origin {}\nPin-Priority: {}\n",
                p.origin, p.priority
            )
        })
    }

    /// `key=value` tokens that carry the repository through an `Action.argv` and back.
    pub fn to_tokens(&self) -> Vec<String> {
        let mut t = vec![format!("id={}", self.id)];
        match &self.source {
            Source::Ppa { owner, name } => t.push(format!("ppa={owner}/{name}")),
            Source::Url {
                url,
                suite,
                components,
                flat,
                key,
                fingerprint,
            } => {
                t.push(format!("url={url}"));
                t.push(format!("suite={suite}"));
                if *flat {
                    t.push("flat=true".to_string());
                } else {
                    t.push(format!("components={}", components.join(",")));
                }
                t.push(format!("key={key}"));
                t.push(format!("fingerprint={fingerprint}"));
            }
        }
        if let Some(p) = &self.pin {
            t.push(format!("pin.origin={}", p.origin));
            t.push(format!("pin.priority={}", p.priority));
        }
        t
    }

    pub fn from_tokens(tokens: &[String]) -> Result<AptRepo> {
        let map: BTreeMap<&str, &str> = tokens.iter().filter_map(|t| t.split_once('=')).collect();
        let mut spec = Spec::new("repo");
        let renames = [
            ("ppa", "repo.ppa"),
            ("url", "repo.url"),
            ("suite", "repo.suite"),
            ("components", "repo.components"),
            ("flat", "repo.flat"),
            ("key", "repo.key"),
            ("fingerprint", "repo.fingerprint"),
            ("pin.origin", "pin.origin"),
            ("pin.priority", "pin.priority"),
        ];
        for (from, to) in renames {
            if let Some(v) = map.get(from) {
                spec = spec.opt(to, v);
            }
        }
        Self::from_spec(&spec)?.ok_or_else(|| {
            ProviderError::Other("apt: repository action without a repository".into())
        })
    }
}

/// Primary-key fingerprints in `gpg --with-colons --show-keys` output.
pub fn primary_fingerprints(colons: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut after_pub = false;
    for line in colons.lines() {
        let rec = line.split(':').next().unwrap_or("");
        match rec {
            "pub" => after_pub = true,
            "sub" | "sec" | "ssb" => after_pub = false,
            "fpr" if after_pub => {
                if let Some(f) = line.split(':').nth(9).filter(|f| !f.is_empty()) {
                    out.push(f.to_ascii_uppercase());
                }
                after_pub = false;
            }
            _ => {}
        }
    }
    out
}

/// `signing_key_fingerprint` of a Launchpad archive JSON document.
pub fn launchpad_fingerprint(json: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    let fp = v.get("signing_key_fingerprint")?.as_str()?;
    (!fp.is_empty()).then(|| fp.to_ascii_uppercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ppa_ids_and_files() {
        let r = AptRepo::from_spec(&Spec::new("git").opt("repo.ppa", "git-core/ppa"))
            .unwrap()
            .unwrap();
        assert_eq!(r.id, "ppa-git-core-ppa");
        assert_eq!(
            r.keyring_path(),
            "/etc/apt/keyrings/dots-ppa-git-core-ppa.asc"
        );
        assert!(r.sources_text(None).contains("Suites: @SUITE@"));
        assert!(r.sources_text(Some("noble")).contains("Suites: noble"));
        assert!(r.prefs_text().is_none());
    }

    #[test]
    fn tokens_round_trip() {
        let spec = Spec::new("code")
            .opt("repo.url", "https://packages.microsoft.com/repos/code")
            .opt("repo.suite", "stable")
            .opt("repo.components", "main,contrib")
            .opt(
                "repo.key",
                "https://packages.microsoft.com/keys/microsoft.asc",
            )
            .opt(
                "repo.fingerprint",
                "bc52 8686 b50d 79e3 39d3 721c eb3e 94ad be12 29cf",
            )
            .opt("pin.origin", "packages.microsoft.com")
            .opt("pin.priority", "900");
        let r = AptRepo::from_spec(&spec).unwrap().unwrap();
        assert_eq!(r.id, "packages.microsoft.com-repos-code-stable");
        assert_eq!(AptRepo::from_tokens(&r.to_tokens()).unwrap(), r);
        assert_eq!(r.owned_paths().len(), 3);
        assert!(r.prefs_text().unwrap().contains("Pin-Priority: 900"));
    }

    fn sublime(extra: &[(&str, &str)]) -> Spec {
        let mut s = Spec::new("sublime-text")
            .opt("repo.url", "https://download.sublimetext.com/")
            .opt("repo.suite", "apt/stable/")
            .opt(
                "repo.key",
                "https://download.sublimetext.com/sublimehq-pub.gpg",
            )
            .opt(
                "repo.fingerprint",
                "1EDDE2CDFC025D17F6DA9EC0ADAE6AD28A8F901A",
            );
        for (k, v) in extra {
            s = s.opt(k, v);
        }
        s
    }

    #[test]
    fn flat_repository_has_no_components_line() {
        // The suite path alone says it is flat; `flat = true` and `components = []` (the
        // resolver turns it into `repo.flat`) say so explicitly.
        for extra in [&[][..], &[("repo.flat", "true")][..]] {
            let r = AptRepo::from_spec(&sublime(extra)).unwrap().unwrap();
            let text = r.sources_text(None);
            assert!(
                text.contains("URIs: https://download.sublimetext.com/\n"),
                "{text}"
            );
            assert!(text.contains("Suites: apt/stable/\n"), "{text}");
            assert!(!text.contains("Components"), "{text}");
            assert_eq!(r.id, "download.sublimetext.com-apt-stable");
            assert_eq!(AptRepo::from_tokens(&r.to_tokens()).unwrap(), r);
        }
        // `flat = true` on a suite without the trailing slash makes it a path.
        let r = AptRepo::from_spec(&sublime(&[
            ("repo.suite", "apt/stable"),
            ("repo.flat", "true"),
        ]))
        .unwrap()
        .unwrap();
        assert!(r.sources_text(None).contains("Suites: apt/stable/\n"));
        // A binary key's keyring is the `.gpg` file.
        let t = r.sources_text_for(None, &r.binary_keyring_path());
        assert!(
            t.contains("Signed-By: /etc/apt/keyrings/dots-download.sublimetext.com-apt-stable.gpg"),
            "{t}"
        );
    }

    #[test]
    fn flat_and_components_conflict() {
        let e = AptRepo::from_spec(&sublime(&[("repo.components", "main")]))
            .unwrap_err()
            .to_string();
        assert!(e.contains("flat repository"), "{e}");
        let e = AptRepo::from_spec(&sublime(&[
            ("repo.suite", "stable"),
            ("repo.flat", "true"),
            ("repo.components", "main"),
        ]))
        .unwrap_err()
        .to_string();
        assert!(e.contains("flat repository"), "{e}");
        // A non-flat repository still needs a component (and defaults to `main`).
        let mut s = sublime(&[("repo.suite", "stable")]);
        let r = AptRepo::from_spec(&s).unwrap().unwrap();
        assert!(r.sources_text(None).contains("Components: main\n"));
        s = s.opt("repo.components", "");
        assert!(
            AptRepo::from_spec(&s)
                .unwrap_err()
                .to_string()
                .contains("flat = true")
        );
    }

    #[test]
    fn rejects_bad_input() {
        let bad = |s: Spec| AptRepo::from_spec(&s).unwrap_err().to_string();
        assert!(bad(Spec::new("x").opt("repo.ppa", "nope")).contains("owner/name"));
        assert!(bad(Spec::new("x").opt("repo.url", "file:///etc")).contains("http"));
        assert!(
            bad(Spec::new("x")
                .opt("repo.url", "https://a.b/c")
                .opt("repo.suite", "stable"))
            .contains("key")
        );
        assert!(AptRepo::from_spec(&Spec::new("x")).unwrap().is_none());
    }

    #[test]
    fn reads_gpg_colons() {
        let out = "tru::1:1:1:0:3:1:5\npub:-:4096:1:EB3E94ADBE1229CF:1:::-:::scESC:::::::\nfpr:::::::::BC528686B50D79E339D3721CEB3E94ADBE1229CF:\nsub:-:4096:1:AAAA:1::::::s::::::\nfpr:::::::::1111111111111111111111111111111111111111:\n";
        assert_eq!(
            primary_fingerprints(out),
            vec!["BC528686B50D79E339D3721CEB3E94ADBE1229CF"]
        );
        assert_eq!(
            launchpad_fingerprint("{\"signing_key_fingerprint\": \"abcd\"}").as_deref(),
            Some("ABCD")
        );
    }

    #[test]
    fn fingerprint_must_be_exactly_forty_hex_digits() {
        let with = |fp: &str| {
            AptRepo::from_spec(
                &Spec::new("x")
                    .opt("repo.url", "https://a.b/c")
                    .opt("repo.suite", "stable")
                    .opt("repo.key", "https://a.b/k.asc")
                    .opt("repo.fingerprint", fp),
            )
        };
        let long_id = with("EB3E94ADBE1229CF").unwrap_err().to_string();
        assert!(long_id.contains("16-character key id"), "{long_id}");
        assert!(
            with("BC528686B50D79E339D3721CEB3E94ADBE1229CF0").is_err(),
            "41 digits"
        );
        assert!(
            with("BC528686B50D79E339D3721CEB3E94ADBE1229C").is_err(),
            "39 digits"
        );
        assert!(
            with("BC528686B50D79E339D3721CEB3E94ADBE1229CG").is_err(),
            "non-hex"
        );
        assert!(with("BC52 8686 B50D 79E3 39D3 721C EB3E 94AD BE12 29CF").is_ok());
    }
}
