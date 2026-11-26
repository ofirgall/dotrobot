//! Object addresses (ARCHITECTURE §18.1): `pkg:<name>`, `pkg:<name>/file:<dst>`,
//! `bundle:<name>/member:<item>`, `provider:<id>`, `priority:<key>`, `repo:tags`,
//! `upstream:<alias>`, and `<alias>:<name>` for an upstream item.

use std::fmt;

use serde_json::Value;

use crate::effective::EffectiveInfo;
use crate::merge::entry_key;

/// The part of a package or bundle an address points into.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Sub {
    File(String),
    Setting(String),
    Provider(String),
    Manual(String),
    Hook(String),
    Member(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Address {
    Pkg {
        name: String,
        sub: Option<Sub>,
    },
    /// `<alias>:<name>`: an item of a followed upstream.
    Item {
        alias: String,
        name: String,
        sub: Option<Sub>,
    },
    Bundle {
        name: String,
        sub: Option<Sub>,
    },
    /// `provider:<id>`: options in `dots.toml`.
    Provider(String),
    /// `priority:<platform-key>`.
    Priority(String),
    /// `repo:tags`, `repo:paths`, `repo:platforms` or `repo:env`.
    Repo(String),
    /// `upstream:<alias>`.
    Upstream(String),
}

impl Sub {
    fn parse(s: &str) -> Result<Sub, String> {
        let (kind, value) = s
            .split_once(':')
            .filter(|(_, v)| !v.is_empty())
            .ok_or_else(|| format!("`{s}` is not `<kind>:<name>`"))?;
        let v = value.to_string();
        Ok(match kind {
            "file" => Sub::File(v),
            "setting" => Sub::Setting(v),
            "provider" => Sub::Provider(v),
            "manual" => Sub::Manual(v),
            "hook" => Sub::Hook(v),
            "member" => Sub::Member(v),
            _ => return Err(format!("unknown address part `{kind}:`")),
        })
    }

    fn path(&self) -> [&str; 2] {
        match self {
            Sub::File(v) => ["files", v],
            Sub::Setting(v) => ["settings", v],
            Sub::Provider(v) => ["providers", v],
            Sub::Manual(v) => ["manual", v],
            Sub::Hook(v) => ["hooks", v],
            Sub::Member(v) => ["packages", v],
        }
    }
}

impl fmt::Display for Sub {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (k, v) = match self {
            Sub::File(v) => ("file", v),
            Sub::Setting(v) => ("setting", v),
            Sub::Provider(v) => ("provider", v),
            Sub::Manual(v) => ("manual", v),
            Sub::Hook(v) => ("hook", v),
            Sub::Member(v) => ("member", v),
        };
        write!(f, "{k}:{v}")
    }
}

impl Address {
    pub fn parse(s: &str) -> Result<Address, String> {
        let (head, rest) = match s.split_once('/') {
            Some((h, r)) => (h, Some(r)),
            None => (s, None),
        };
        let (kind, name) = head
            .split_once(':')
            .filter(|(k, n)| !k.is_empty() && !n.is_empty())
            .ok_or_else(|| format!("`{s}` is not an address like `pkg:<name>`"))?;
        let sub = rest.map(Sub::parse).transpose()?;
        let n = name.to_string();
        let no_sub = |a: Address| match sub {
            None => Ok(a),
            Some(_) => Err(format!("`{head}` has no parts to address")),
        };
        match kind {
            "pkg" => check_sub(&sub, false).map(|()| Address::Pkg { name: n, sub }),
            "bundle" => check_sub(&sub, true).map(|()| Address::Bundle { name: n, sub }),
            "provider" => no_sub(Address::Provider(n)),
            "priority" => no_sub(Address::Priority(n)),
            "upstream" => no_sub(Address::Upstream(n)),
            "repo" => match name {
                "tags" | "paths" | "platforms" | "env" => no_sub(Address::Repo(n)),
                _ => Err(format!(
                    "`repo:{name}` is not an address; use repo:tags, repo:paths, repo:platforms or repo:env"
                )),
            },
            alias => check_sub(&sub, false).map(|()| Address::Item {
                alias: alias.to_string(),
                name: n,
                sub,
            }),
        }
    }

    /// The object that holds this address: `pkg:<name>`, `bundle:<name>`, `<alias>:<name>`
    /// or `repo:manifest` (the `dots.toml` sections).
    pub fn object(&self) -> String {
        match self {
            Address::Pkg { name, .. } => format!("pkg:{name}"),
            Address::Item { alias, name, .. } => format!("{alias}:{name}"),
            Address::Bundle { name, .. } => format!("bundle:{name}"),
            _ => "repo:manifest".to_string(),
        }
    }

    /// Where the addressed value sits inside its object. A segment that follows a keyed list
    /// (`files`, ...) names the entry by its key. The same segments are the provenance key.
    /// `provider:<id>` lives under `provider` (options) or `providers` (custom): see
    /// [`Address::candidates`].
    pub fn path(&self) -> Vec<String> {
        self.candidates().remove(0)
    }

    /// Every path the address could be at.
    pub fn candidates(&self) -> Vec<Vec<String>> {
        let own = |sub: &Option<Sub>| -> Vec<String> {
            sub.as_ref()
                .map(|s| s.path().iter().map(|x| x.to_string()).collect())
                .unwrap_or_default()
        };
        let p = |a: &str, b: &str| vec![a.to_string(), b.to_string()];
        match self {
            Address::Pkg { sub, .. } | Address::Item { sub, .. } | Address::Bundle { sub, .. } => {
                vec![own(sub)]
            }
            Address::Provider(id) => vec![p("provider", id), p("providers", id)],
            Address::Priority(k) => vec![p("priority", k)],
            Address::Repo(section) => vec![vec![
                if section == "env" { "env" } else { section }.to_string(),
            ]],
            Address::Upstream(alias) => vec![p("upstream", alias)],
        }
    }

    /// The dotted provenance key of the addressed value (empty for a whole object).
    pub fn prov_path(&self) -> String {
        self.path().join(".")
    }
}

fn check_sub(sub: &Option<Sub>, bundle: bool) -> Result<(), String> {
    match (sub, bundle) {
        (None, _) => Ok(()),
        (Some(Sub::Member(_)), true) => Ok(()),
        (Some(s), true) => Err(format!("a bundle has no `{s}`; use member:<item>")),
        (Some(Sub::Member(_)), false) => Err("a package has no members".into()),
        (Some(_), false) => Ok(()),
    }
}

impl fmt::Display for Address {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let sub = |f: &mut fmt::Formatter<'_>, s: &Option<Sub>| match s {
            Some(s) => write!(f, "/{s}"),
            None => Ok(()),
        };
        match self {
            Address::Pkg { name, sub: s } => {
                write!(f, "pkg:{name}")?;
                sub(f, s)
            }
            Address::Item {
                alias,
                name,
                sub: s,
            } => {
                write!(f, "{alias}:{name}")?;
                sub(f, s)
            }
            Address::Bundle { name, sub: s } => {
                write!(f, "bundle:{name}")?;
                sub(f, s)
            }
            Address::Provider(id) => write!(f, "provider:{id}"),
            Address::Priority(k) => write!(f, "priority:{k}"),
            Address::Repo(s) => write!(f, "repo:{s}"),
            Address::Upstream(a) => write!(f, "upstream:{a}"),
        }
    }
}

/// An addressed value.
#[derive(Debug, Clone, PartialEq)]
pub struct Found {
    /// The key path that matched (also the provenance key).
    pub path: String,
    pub effective: Value,
    /// The same spot in the definition, `None` when the definition has nothing there.
    pub definition: Option<Value>,
}

/// The value at `segs` of `v`; a segment after a keyed list names the entry by its key.
pub(crate) fn get_path(v: &Value, segs: &[String]) -> Option<Value> {
    let mut cur = v;
    let mut coll = String::new();
    for s in segs {
        cur = match cur {
            Value::Object(m) => m.get(s)?,
            Value::Array(a) => a.iter().find(|e| entry_key(&coll, e) == *s)?,
            _ => return None,
        };
        coll = s.clone();
    }
    Some(cur.clone())
}

impl EffectiveInfo {
    /// Look an address up in the effective model. `None` when the object or the part does
    /// not exist.
    pub fn lookup(&self, addr: &Address) -> Option<Found> {
        let obj = self.objects.get(&addr.object())?;
        addr.candidates().into_iter().find_map(|segs| {
            let effective = get_path(&obj.effective, &segs)?;
            Some(Found {
                path: segs.join("."),
                effective,
                definition: get_path(&obj.definition, &segs),
            })
        })
    }
}
