//! Reading the effective model back (task 5.7): the annotated `show --effective` text, the
//! structural diff behind `override diff`, and the stale/redundant override lint.

use serde_json::{Map, Value};

use dots_core::{Diagnostic, Provenance};

use crate::address::{Address, get_path};
use crate::effective::{EffObject, EffectiveInfo, OverrideFile, Scope};
use crate::merge::{entry_key, is_edit, join};
use crate::source::suggest;

/// Who set one key.
#[derive(Debug, Clone, PartialEq)]
pub struct Origin {
    /// Provenance key of the value.
    pub path: String,
    pub scope: Scope,
    /// `repo override (overrides/packages/x/override.toml)`.
    pub source: String,
    pub file: Option<String>,
    pub line: Option<u32>,
}

impl Origin {
    /// The annotation shown next to a key: the source, then the line when known.
    pub fn note(&self) -> String {
        match self.line {
            Some(n) => format!("{}, line {n}", self.source),
            None => self.source.clone(),
        }
    }
}

/// One line of the TOML-like text `show` prints.
#[derive(Debug, Clone, PartialEq)]
pub struct RenderLine {
    pub text: String,
    /// Set on `key = value` lines of an effective view.
    pub origin: Option<Origin>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Rendered {
    pub value: Value,
    pub lines: Vec<RenderLine>,
}

/// One difference between the definition and the effective object.
#[derive(Debug, Clone, PartialEq)]
pub struct Change {
    pub path: String,
    /// `added`, `removed` or `changed`.
    pub kind: &'static str,
    pub before: Option<Value>,
    pub after: Option<Value>,
    pub source: Option<String>,
}

/// The file an object's definition lives in.
fn definition_file(object: &str) -> String {
    if let Some(n) = object.strip_prefix("pkg:") {
        format!("packages/{n}/package.toml")
    } else if let Some(n) = object.strip_prefix("bundle:") {
        format!("bundles/{n}.toml")
    } else {
        "dots.toml".to_string()
    }
}

fn bare(k: &str) -> bool {
    !k.is_empty()
        && k.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

fn tk(k: &str) -> String {
    if bare(k) {
        k.to_string()
    } else {
        format!("{k:?}")
    }
}

fn inline(v: &Value) -> String {
    match v {
        Value::Array(a) => format!("[{}]", a.iter().map(inline).collect::<Vec<_>>().join(", ")),
        Value::Object(m) if m.is_empty() => "{}".into(),
        Value::Object(m) => format!(
            "{{ {} }}",
            m.iter()
                .map(|(k, x)| format!("{} = {}", tk(k), inline(x)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        other => other.to_string(),
    }
}

fn is_table(v: &Value) -> bool {
    matches!(v, Value::Object(m) if !m.is_empty())
}

fn is_table_array(v: &Value) -> bool {
    matches!(v, Value::Array(a) if !a.is_empty() && a.iter().all(Value::is_object))
}

struct Renderer<'a> {
    object: &'a str,
    obj: &'a EffObject,
    effective: bool,
    out: Vec<RenderLine>,
}

impl Renderer<'_> {
    fn origin(&self, path: &str) -> Option<Origin> {
        if !self.effective {
            return None;
        }
        Some(match self.obj.prov.get(path) {
            Some(l) => Origin {
                path: path.to_string(),
                scope: l.source.scope,
                source: l.source.describe(),
                file: Some(l.source.file.clone()),
                line: l.line,
            },
            None => {
                let file = definition_file(self.object);
                Origin {
                    path: path.to_string(),
                    scope: Scope::Definition,
                    source: format!("definition ({file})"),
                    file: Some(file),
                    line: None,
                }
            }
        })
    }

    fn header(&mut self, text: String) {
        if !self.out.is_empty() {
            self.out.push(RenderLine {
                text: String::new(),
                origin: None,
            });
        }
        self.out.push(RenderLine { text, origin: None });
    }

    fn object(&mut self, m: &Map<String, Value>, path: &str, hdr: &str) {
        let hidden = |k: &str| self.effective && k == "platform";
        let keys: Vec<&String> = m.keys().filter(|k| !hidden(k)).collect();
        for k in &keys {
            let v = &m[*k];
            if is_table(v) || is_table_array(v) {
                continue;
            }
            let origin = self.origin(&join(path, k));
            self.out.push(RenderLine {
                text: format!("{} = {}", tk(k), inline(v)),
                origin,
            });
        }
        for k in keys {
            match &m[k] {
                Value::Object(sub) if !sub.is_empty() => {
                    let h = join(hdr, &tk(k));
                    self.header(format!("[{h}]"));
                    self.object(sub, &join(path, k), &h);
                }
                Value::Array(a) if is_table_array(&m[k]) => {
                    let h = join(hdr, &tk(k));
                    for e in a {
                        self.header(format!("[[{h}]]"));
                        let key = entry_key(k, e);
                        if let Value::Object(em) = e {
                            self.object(em, &join(&join(path, k), &key), &h);
                        }
                    }
                }
                _ => {}
            }
        }
    }
}

/// The address of a keyed entry given its key path (`files.~/.zshrc`).
fn entry_address(object: &str, path: &str) -> String {
    let (head, rest) = path.split_once('.').unwrap_or((path, ""));
    let kind = match head {
        "files" => "file",
        "settings" => "setting",
        "providers" => "provider",
        "manual" => "manual",
        "hooks" => "hook",
        "packages" => "member",
        _ => return format!("{object}/{path}"),
    };
    if rest.is_empty() {
        object.to_string()
    } else {
        format!("{object}/{kind}:{rest}")
    }
}

fn identity_fields(coll: &str) -> &'static [&'static str] {
    match coll {
        "files" => &["dst"],
        "defaults" => &["domain", "key"],
        "registry" => &["key", "name"],
        "xdg" => &["setting", "mime"],
        _ => &[],
    }
}

/// Settable leaves of an override file: `(key path, position path, value)`. List edits and
/// `remove = true` entries are not comparable and are skipped.
fn leaves(
    v: &Value,
    segs: &mut Vec<String>,
    spath: &str,
    out: &mut Vec<(Vec<String>, String, Value)>,
) {
    match v {
        Value::Object(m) => {
            if is_edit(m) || m.get("remove") == Some(&Value::Bool(true)) {
                return;
            }
            for (k, x) in m {
                if k == "platform"
                    || (segs.is_empty() && matches!(k.as_str(), "schema" | "members"))
                {
                    continue;
                }
                segs.push(k.clone());
                leaves(x, segs, &join(spath, k), out);
                segs.pop();
            }
        }
        Value::Array(a)
            if segs.len() == 1 && is_table_array(v) && !identity_fields(&segs[0]).is_empty() =>
        {
            let coll = segs[0].clone();
            for (i, e) in a.iter().enumerate() {
                let Value::Object(m) = e else { continue };
                if m.get("remove") == Some(&Value::Bool(true)) {
                    continue;
                }
                let key = entry_key(&coll, e);
                for (k, x) in m {
                    if k == "platform" || identity_fields(&coll).contains(&k.as_str()) {
                        continue;
                    }
                    segs.push(key.clone());
                    segs.push(k.clone());
                    leaves(x, segs, &join(&join(spath, &i.to_string()), k), out);
                    segs.pop();
                    segs.pop();
                }
            }
        }
        _ => out.push((segs.clone(), spath.to_string(), v.clone())),
    }
}

fn diff_values(
    before: Option<&Value>,
    after: Option<&Value>,
    path: &str,
    coll: &str,
    obj: &EffObject,
    out: &mut Vec<Change>,
) {
    let source = |p: &str| obj.prov.get(p).map(|l| l.source.describe());
    let push = |out: &mut Vec<Change>, kind, p: &str, b: Option<&Value>, a: Option<&Value>| {
        out.push(Change {
            path: p.to_string(),
            kind,
            before: b.cloned(),
            after: a.cloned(),
            source: source(p),
        });
    };
    match (before, after) {
        (None, None) => {}
        (Some(b), Some(a)) if b == a => {}
        (None, Some(a)) => push(out, "added", path, None, Some(a)),
        (Some(b), None) => push(out, "removed", path, Some(b), None),
        (Some(Value::Object(b)), Some(Value::Object(a))) => {
            let mut keys: Vec<&String> = b.keys().chain(a.keys()).collect();
            keys.sort();
            keys.dedup();
            for k in keys {
                if k == "platform" || (path.is_empty() && k == "schema") {
                    continue;
                }
                diff_values(b.get(k), a.get(k), &join(path, k), k, obj, out);
            }
        }
        (Some(Value::Array(b)), Some(Value::Array(a))) => {
            let key = |e: &Value| entry_key(coll, e);
            let mut keys: Vec<String> = b.iter().chain(a.iter()).map(key).collect();
            keys.dedup();
            let mut seen = std::collections::BTreeSet::new();
            keys.retain(|k| seen.insert(k.clone()));
            for k in keys {
                let find = |l: &'_ [Value]| l.iter().find(|e| entry_key(coll, e) == k).cloned();
                let (x, y) = (find(b), find(a));
                diff_values(x.as_ref(), y.as_ref(), &join(path, &k), &k, obj, out);
            }
        }
        (Some(b), Some(a)) => push(out, "changed", path, Some(b), Some(a)),
    }
}

impl EffectiveInfo {
    /// The text of an address, one line per key. `effective` annotates every key with the
    /// layer that set it; otherwise the definition is printed as written. `None` when
    /// nothing exists at the address.
    pub fn render(&self, addr: &Address, effective: bool) -> Option<Rendered> {
        let found = self.lookup(addr)?;
        let object = addr.object();
        let obj = self.objects.get(&object)?;
        let (value, effective) = match (&found.definition, effective) {
            (Some(d), false) => (d.clone(), false),
            _ => (found.effective.clone(), effective),
        };
        let mut r = Renderer {
            object: &object,
            obj,
            effective,
            out: Vec::new(),
        };
        match &value {
            Value::Object(m) => r.object(m, &found.path, ""),
            other => {
                let origin = r.origin(&found.path);
                r.out.push(RenderLine {
                    text: format!("value = {}", inline(other)),
                    origin,
                });
            }
        }
        Some(Rendered {
            value,
            lines: r.out,
        })
    }

    /// Definition against effective at an address. `None` when nothing exists there.
    pub fn diff(&self, addr: &Address) -> Option<Vec<Change>> {
        let found = self.lookup(addr)?;
        let obj = self.objects.get(&addr.object())?;
        let coll = found.path.rsplit('.').next().unwrap_or("");
        let mut out = Vec::new();
        diff_values(
            found.definition.as_ref(),
            Some(&found.effective),
            &found.path,
            coll,
            obj,
            &mut out,
        );
        Some(out)
    }

    /// Every address the CLI could point at, for "did you mean".
    pub fn object_addresses(&self) -> Vec<String> {
        self.objects.keys().cloned().collect()
    }

    /// Stale overrides (errors: a keyed entry that no longer exists, with the closest address
    /// as the hint) and redundant ones (warnings: a value the layers beneath already set).
    /// Whole-object staleness is already an error at load.
    pub fn lint(&self) -> (Vec<Diagnostic>, Vec<Diagnostic>) {
        let mut errors = Vec::new();
        let mut warnings = Vec::new();
        for m in &self.misses {
            let address = entry_address(&m.address, &m.path);
            let mut d = Diagnostic::from_code(
                "stale-override",
                &[("address", &address), ("file", &m.file)],
            )
            .with_provenance(Provenance {
                source: format!("override:{}", m.scope.short()),
                file: Some(m.file.clone()),
                line: m.line,
                col: None,
            });
            if let Some(near) = self.closest(&m.address, &m.path) {
                d.hint = format!("Did you mean `{near}`? Move the override there, or remove it.");
            }
            errors.push(d);
        }
        for (i, o) in self.overrides.iter().enumerate() {
            if !self.objects.contains_key(&o.target) {
                continue;
            }
            let mut found = Vec::new();
            leaves(&o.value, &mut Vec::new(), "", &mut found);
            for (segs, spath, value) in found {
                // What the layers below this override give, platform tables applied. If that
                // cannot be computed, the override is not called redundant.
                let lower: Vec<&OverrideFile> = self.overrides[..i]
                    .iter()
                    .filter(|l| l.target == o.target)
                    .collect();
                let Some(merged) = self.merged_below(&o.target, &lower) else {
                    continue;
                };
                let beneath = get_path(&merged, &segs);
                if beneath.as_ref() != Some(&value) {
                    continue;
                }
                let key = segs.join(".");
                let address = entry_address(&o.target, &key);
                let line = o.spans.get(&spath).map(|p| p.0);
                warnings.push(
                    Diagnostic::from_code(
                        "redundant-override",
                        &[
                            ("address", &address),
                            ("path", &key),
                            ("file", &o.file),
                            ("line", &line.unwrap_or(0).to_string()),
                        ],
                    )
                    .with_provenance(Provenance {
                        source: format!("override:{}", o.scope.short()),
                        file: Some(o.file.clone()),
                        line,
                        col: None,
                    }),
                );
            }
        }
        (errors, warnings)
    }

    /// The existing sibling nearest to a missing keyed entry.
    fn closest(&self, object: &str, path: &str) -> Option<String> {
        let (head, rest) = path.split_once('.')?;
        let obj = self.objects.get(object)?;
        let cands: Vec<String> = match obj.effective.get(head)? {
            Value::Object(m) => m.keys().cloned().collect(),
            Value::Array(a) => a.iter().map(|e| entry_key(head, e)).collect(),
            _ => return None,
        };
        let pick = suggest(rest, cands.iter().map(String::as_str)).or_else(|| {
            // One entry left over: the rename is not ambiguous.
            (cands.len() == 1).then(|| cands[0].clone())
        })?;
        Some(entry_address(object, &join(head, &pick)))
    }

    /// The object names of one kind for suggestions: `pkg:` names, `bundle:` names.
    pub fn suggest_object(&self, address: &str) -> Option<String> {
        suggest(address, self.objects.keys().map(String::as_str))
    }
}
