//! The format-independent value tree and the merge rules (ARCHITECTURE 6.1).

use std::collections::BTreeMap;

use indexmap::IndexMap;

/// Reserved string value that deletes a key when it appears in a later layer.
pub const DELETE_MARKER: &str = "__dots_delete__";

/// An ordered value tree. Tables keep insertion order.
#[derive(Debug, Clone, PartialEq)]
pub enum Node {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
    Array(Vec<Node>),
    Table(IndexMap<String, Node>),
}

impl Node {
    pub fn type_name(&self) -> &'static str {
        match self {
            Node::Null => "null",
            Node::Bool(_) => "boolean",
            Node::Int(_) => "integer",
            Node::Float(_) => "float",
            Node::Str(_) => "string",
            Node::Array(_) => "array",
            Node::Table(_) => "table",
        }
    }
    fn is_delete(&self) -> bool {
        matches!(self, Node::Str(s) if s == DELETE_MARKER)
    }
    fn compatible(&self, other: &Node) -> bool {
        use Node::*;
        matches!(
            (self, other),
            (Null, Null)
                | (Bool(_), Bool(_))
                | (Int(_) | Float(_), Int(_) | Float(_))
                | (Str(_), Str(_))
                | (Array(_), Array(_))
                | (Table(_), Table(_))
        )
    }
}

/// How an array is combined at one path (`[files.merge]`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Strategy {
    Replace,
    Append,
    /// Append, dropping values already present.
    Union,
    /// Merge array-of-tables elements that share the value of this key.
    By(String),
}

/// A type mismatch between layers at `path`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mismatch {
    pub path: String,
    pub left: &'static str,
    pub right: &'static str,
}

pub type Strategies = BTreeMap<String, Strategy>;

fn join(path: &str, key: &str) -> String {
    if path.is_empty() {
        key.to_string()
    } else {
        format!("{path}.{key}")
    }
}

/// Merge `over` into `base`; the later layer wins per key.
pub fn merge_value(
    base: &mut Node,
    over: Node,
    path: &str,
    strat: &Strategies,
) -> Result<(), Mismatch> {
    if !base.compatible(&over) {
        return Err(Mismatch {
            path: path.to_string(),
            left: base.type_name(),
            right: over.type_name(),
        });
    }
    match (base, over) {
        (Node::Table(b), Node::Table(o)) => {
            for (k, v) in o {
                if v.is_delete() {
                    b.shift_remove(&k);
                    continue;
                }
                let p = join(path, &k);
                match b.get_mut(&k) {
                    Some(bv) => merge_value(bv, v, &p, strat)?,
                    None => {
                        b.insert(k, strip_deletes(v));
                    }
                }
            }
        }
        (Node::Array(b), Node::Array(o)) => match strat.get(path).unwrap_or(&Strategy::Replace) {
            Strategy::Replace => *b = o,
            Strategy::Append => b.extend(o),
            Strategy::Union => {
                for v in o {
                    if !b.contains(&v) {
                        b.push(v);
                    }
                }
            }
            Strategy::By(key) => {
                for v in o {
                    let id = match &v {
                        Node::Table(t) => t.get(key).cloned(),
                        _ => None,
                    };
                    let slot = id.and_then(|id| {
                        b.iter_mut()
                            .find(|e| matches!(e, Node::Table(t) if t.get(key) == Some(&id)))
                    });
                    match slot {
                        Some(existing) => merge_value(existing, v, path, strat)?,
                        None => b.push(v),
                    }
                }
            }
        },
        (b, o) => *b = o,
    }
    Ok(())
}

/// Remove every delete marker below `n` (used on the first layer and on inserted values).
pub fn strip_deletes(n: Node) -> Node {
    match n {
        Node::Table(t) => Node::Table(
            t.into_iter()
                .filter(|(_, v)| !v.is_delete())
                .map(|(k, v)| (k, strip_deletes(v)))
                .collect(),
        ),
        Node::Array(a) => Node::Array(a.into_iter().map(strip_deletes).collect()),
        other => other,
    }
}

/// Parse a `[files.merge]` value.
pub fn parse_strategy(v: &serde_json::Value) -> Result<Strategy, String> {
    match v {
        serde_json::Value::String(s) => match s.as_str() {
            "replace" => Ok(Strategy::Replace),
            "append" => Ok(Strategy::Append),
            "union" => Ok(Strategy::Union),
            other => Err(format!("unknown merge strategy `{other}`")),
        },
        serde_json::Value::Object(m) => match m.get("by").and_then(|b| b.as_str()) {
            Some(k) => Ok(Strategy::By(k.to_string())),
            None => Err("a table strategy needs `by = \"<key>\"`".into()),
        },
        _ => Err("expected \"append\", \"replace\", \"union\" or { by = \"<key>\" }".into()),
    }
}
