//! The one merge grammar (ARCHITECTURE §18.2), shared by `[platform."<key>"]` tables and
//! overrides. It works on raw `serde_json::Value` objects so it never depends on the typed
//! structs, and it records provenance as it goes.
//!
//! | Source | Effect |
//! |---|---|
//! | scalar | replace |
//! | table | deep merge |
//! | array | replace |
//! | `{ add, remove, prepend }` | edit the list below |
//! | keyed entry (files by `dst`, settings, providers, members, ...) | merge by key; new key adds |
//! | `remove = true` in a keyed entry | delete it |

use serde_json::{Map, Value};

use crate::effective::{Prov, RawErr, RawMiss};
use crate::spans::Spans;

/// Top-level arrays whose entries are addressed by a key and merged entry by entry.
fn keyed_array(path: &str) -> bool {
    matches!(path, "files" | "defaults" | "registry" | "xdg")
}

/// Tables whose entries are keyed objects that `remove = true` deletes.
fn keyed_map(path: &str) -> bool {
    matches!(
        path,
        "providers" | "settings" | "members" | "tags" | "paths" | "platforms" | "provider"
    )
}

pub(crate) fn join(prefix: &str, key: &str) -> String {
    if prefix.is_empty() {
        key.to_string()
    } else {
        format!("{prefix}.{key}")
    }
}

/// Is `path` (or an ancestor of it) inside a platform table? Those do not get provenance:
/// the merged value does.
fn in_platform(path: &str) -> bool {
    path == "platform"
        || path.starts_with("platform.")
        || path.contains(".platform.")
        || path.ends_with(".platform")
}

/// `{ add, remove, prepend }` and nothing else.
pub(crate) fn is_edit(m: &Map<String, Value>) -> bool {
    !m.is_empty()
        && m.keys()
            .all(|k| matches!(k.as_str(), "add" | "remove" | "prepend"))
}

/// What identifies a list item: the string itself, or `name`/`dst`/`alias`/`key` of an entry.
pub(crate) fn item_key(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Object(m) => ["name", "dst", "alias", "key", "domain"]
            .iter()
            .find_map(|k| m.get(*k))
            .map(|k| match k {
                Value::String(s) => s.clone(),
                other => other.to_string(),
            })
            .unwrap_or_else(|| v.to_string()),
        other => other.to_string(),
    }
}

/// The key of an entry of keyed array `path`.
pub(crate) fn entry_key(path: &str, e: &Value) -> String {
    let field = |k: &str| match e.get(k) {
        Some(Value::String(s)) => s.clone(),
        Some(other) => other.to_string(),
        None => String::new(),
    };
    match path {
        "files" => field("dst"),
        "defaults" => format!("{}|{}", field("domain"), field("key")),
        "registry" => format!("{}|{}", field("key"), field("name")),
        "xdg" => match e.get("setting") {
            Some(Value::String(s)) => s.clone(),
            _ => e
                .get("mime")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join(",")
                })
                .unwrap_or_default(),
        },
        _ => item_key(e),
    }
}

pub(crate) struct Merger<'a> {
    pub prov: &'a mut Prov,
    pub misses: &'a mut Vec<RawMiss>,
    pub errors: &'a mut Vec<RawErr>,
    pub spans: &'a Spans,
    /// Provenance source id of the layer being merged.
    pub src: u16,
    /// False for the definition's own base content (it is the default, so it is not stored).
    pub record: bool,
}

impl Merger<'_> {
    fn line(&self, spath: &str) -> u32 {
        self.spans.get(spath).map_or(0, |p| p.0)
    }

    fn mark(&mut self, path: &str, spath: &str) {
        if self.record && !path.is_empty() && !in_platform(path) {
            let line = self.line(spath);
            self.prov.prune(path);
            self.prov.set(path.to_string(), self.src, line);
        }
    }

    fn mark_leaves(&mut self, v: &Value, path: &str, spath: &str) {
        match v {
            Value::Object(m) if !m.is_empty() => {
                for (k, x) in m {
                    self.mark_leaves(x, &join(path, k), &join(spath, k));
                }
            }
            _ => self.mark(path, spath),
        }
    }

    fn miss(&mut self, path: &str, spath: &str) {
        if self.record {
            let line = self.line(spath);
            self.misses.push(RawMiss {
                path: path.to_string(),
                src: self.src,
                line,
            });
        }
    }

    fn mismatch(&mut self, path: &str, spath: &str, expected: &str) {
        let line = self.line(spath);
        self.errors.push(RawErr {
            path: path.to_string(),
            src: self.src,
            line,
            expected: expected.to_string(),
        });
    }

    /// Merge `src` on top of `dst`. `path` is the effective key path, `spath` the position
    /// path inside the layer's file (see [`Spans`]).
    pub fn merge(&mut self, dst: &mut Value, src: &Value, path: &str, spath: &str) {
        match (&mut *dst, src) {
            (Value::Object(d), Value::Object(s)) => self.merge_obj(d, s, path, spath),
            (Value::Array(d), Value::Object(s)) => {
                if is_edit(s) {
                    self.list_edit(d, s, path, spath);
                } else if keyed_array(path) {
                    self.keyed_table(d, s, path, spath);
                } else {
                    self.replace(dst, src, path, spath);
                }
            }
            (Value::Array(d), Value::Array(s)) if keyed_array(path) => {
                for (i, e) in s.iter().enumerate() {
                    self.upsert(d, e, path, &join(spath, &i.to_string()));
                }
            }
            _ => self.replace(dst, src, path, spath),
        }
    }

    fn replace(&mut self, dst: &mut Value, src: &Value, path: &str, spath: &str) {
        *dst = src.clone();
        self.prov.prune(path);
        self.mark_leaves(src, path, spath);
    }

    fn merge_obj(
        &mut self,
        d: &mut Map<String, Value>,
        s: &Map<String, Value>,
        path: &str,
        spath: &str,
    ) {
        for (k, v) in s {
            if path.is_empty() && k == "schema" {
                continue;
            }
            let (cpath, cspath) = (join(path, k), join(spath, k));
            if let Value::Object(vo) = v
                && keyed_map(path)
                && vo.get("remove") == Some(&Value::Bool(true))
            {
                if d.remove(k).is_none() {
                    self.miss(&cpath, &cspath);
                }
                self.prov.prune(&cpath);
                continue;
            }
            if let Some(dv) = d.get_mut(k) {
                self.merge(dv, v, &cpath, &cspath);
                continue;
            }
            let list_like = match v {
                Value::Object(m) => is_edit(m) || keyed_array(&cpath),
                Value::Array(_) => keyed_array(&cpath),
                _ => false,
            };
            if list_like {
                let mut arr = Value::Array(Vec::new());
                self.merge(&mut arr, v, &cpath, &cspath);
                d.insert(k.clone(), arr);
            } else {
                d.insert(k.clone(), v.clone());
                self.mark_leaves(v, &cpath, &cspath);
            }
        }
    }

    /// `{ add, remove, prepend }` on a list: remove, then prepend, then add.
    fn list_edit(&mut self, d: &mut Vec<Value>, s: &Map<String, Value>, path: &str, spath: &str) {
        let list = |k: &str| -> Vec<Value> {
            match s.get(k) {
                Some(Value::Array(a)) => a.clone(),
                _ => Vec::new(),
            }
        };
        for r in list("remove") {
            let key = item_key(&r);
            d.retain(|e| item_key(e) != key);
        }
        for p in list("prepend").into_iter().rev() {
            let key = item_key(&p);
            d.retain(|e| item_key(e) != key);
            d.insert(0, p);
        }
        for a in list("add") {
            let key = item_key(&a);
            match d.iter().position(|e| item_key(e) == key) {
                Some(i) if a.is_object() => d[i] = a,
                Some(_) => {}
                None => d.push(a),
            }
        }
        self.mark(path, spath);
    }

    fn find(d: &[Value], path: &str, key: &str) -> Option<usize> {
        d.iter().position(|e| entry_key(path, e) == key)
    }

    /// Merge `e` into the entry with the same key, or append it.
    fn upsert(&mut self, d: &mut Vec<Value>, e: &Value, path: &str, spath: &str) {
        let key = entry_key(path, e);
        let epath = join(path, &key);
        let removing = e.get("remove") == Some(&Value::Bool(true));
        match (Self::find(d, path, &key), removing) {
            (Some(i), true) => {
                d.remove(i);
                self.prov.prune(&epath);
            }
            (None, true) => self.miss(&epath, spath),
            (Some(i), false) => self.merge(&mut d[i], e, &epath, spath),
            (None, false) => {
                d.push(e.clone());
                self.mark_leaves(e, &epath, spath);
            }
        }
    }

    /// `files = { "<dst>" = { ... }, add = [...], remove = [...], prepend = [...] }`.
    fn keyed_table(&mut self, d: &mut Vec<Value>, s: &Map<String, Value>, path: &str, spath: &str) {
        for (k, v) in s {
            let cspath = join(spath, k);
            match (k.as_str(), v) {
                ("add", Value::Array(a)) => {
                    for (i, e) in a.iter().enumerate() {
                        self.upsert(d, e, path, &join(&cspath, &i.to_string()));
                    }
                }
                ("prepend", Value::Array(a)) => {
                    for e in a.iter().rev() {
                        let key = entry_key(path, e);
                        d.retain(|x| entry_key(path, x) != key);
                        d.insert(0, e.clone());
                        self.mark_leaves(e, &join(path, &key), &cspath);
                    }
                }
                ("remove", Value::Array(a)) => {
                    for r in a {
                        let key = item_key(r);
                        let epath = join(path, &key);
                        match Self::find(d, path, &key) {
                            Some(i) => {
                                d.remove(i);
                                self.prov.prune(&epath);
                            }
                            None => self.miss(&epath, &cspath),
                        }
                    }
                }
                (key, Value::Object(vo)) => {
                    let epath = join(path, key);
                    let found = Self::find(d, path, key);
                    if vo.get("remove") == Some(&Value::Bool(true)) {
                        match found {
                            Some(i) => {
                                d.remove(i);
                                self.prov.prune(&epath);
                            }
                            None => self.miss(&epath, &cspath),
                        }
                    } else if let Some(i) = found {
                        self.merge(&mut d[i], v, &epath, &cspath);
                    } else if is_new_entry(vo) {
                        let mut e = vo.clone();
                        let field = if path == "files" { "dst" } else { "name" };
                        e.entry(field)
                            .or_insert_with(|| Value::String(key.to_string()));
                        d.push(Value::Object(e));
                        self.mark_leaves(v, &epath, &cspath);
                    } else {
                        // A partial copy of an entry that does not exist.
                        self.miss(&epath, &cspath);
                    }
                }
                _ => self.mismatch(&join(path, k), &cspath, "a table"),
            }
        }
    }
}

/// Does a keyed override entry carry enough to be a new entry, rather than a patch?
fn is_new_entry(m: &Map<String, Value>) -> bool {
    ["src", "layers", "url", "template", "kind"]
        .iter()
        .any(|k| m.contains_key(*k))
}

/// The entry of `root` reached by `segs` (object keys, or the key of a keyed array entry).
pub(crate) fn descend<'v>(root: &'v mut Value, segs: &[&str]) -> Option<&'v mut Value> {
    let mut cur = root;
    let mut path = String::new();
    for seg in segs {
        cur = match cur {
            Value::Object(m) => m.get_mut(*seg)?,
            Value::Array(a) => {
                let i = a.iter().position(|e| entry_key(&path, e) == *seg)?;
                &mut a[i]
            }
            _ => return None,
        };
        path = join(&path, seg);
    }
    Some(cur)
}
