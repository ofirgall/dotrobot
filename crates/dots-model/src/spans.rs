//! Source positions of the keys lint and the resolver point at.
//!
//! The typed structs are lenient and carry no positions, so the loader walks the parsed
//! document once and records a `(line, col)` per key path. Paths join keys and array indexes
//! with `.`: `when`, `requires.0.when`, `providers.apt`, `packages.2.name`, `priority.debian`.
//! Strings and other scalars map to the position of their value; tables and arrays map to
//! the position of their key (or header).

use std::collections::BTreeMap;
use std::ops::Range;

use toml_edit::{Item, Table, Value};

use crate::source::SourceFile;

/// Key path to 1-based `(line, col)`.
pub type Spans = BTreeMap<String, (u32, u32)>;

pub(crate) fn collect(src: &SourceFile, root: &Item) -> Spans {
    let mut w = Walker {
        src,
        out: Spans::new(),
    };
    if let Some(t) = root.as_table() {
        w.table(t, "");
    }
    w.out
}

struct Walker<'a> {
    src: &'a SourceFile,
    out: Spans,
}

fn join(prefix: &str, key: &str) -> String {
    if prefix.is_empty() {
        key.to_string()
    } else {
        format!("{prefix}.{key}")
    }
}

impl Walker<'_> {
    fn record(&mut self, path: String, span: Option<Range<usize>>) {
        if let Some(s) = span {
            self.out.insert(path, self.src.pos(s.start));
        }
    }

    fn table(&mut self, t: &Table, prefix: &str) {
        for (k, item) in t.iter() {
            let key_span = t.get_key_value(k).and_then(|(k, _)| k.span());
            self.item(item, join(prefix, k), key_span);
        }
    }

    fn item(&mut self, item: &Item, path: String, key_span: Option<Range<usize>>) {
        match item {
            Item::Value(v) => self.value(v, path, key_span),
            Item::Table(t) => {
                self.record(path.clone(), key_span.or_else(|| t.span()));
                self.table(t, &path);
            }
            Item::ArrayOfTables(a) => {
                self.record(path.clone(), key_span);
                for (i, t) in a.iter().enumerate() {
                    let p = join(&path, &i.to_string());
                    self.record(p.clone(), t.span());
                    self.table(t, &p);
                }
            }
            Item::None => {}
        }
    }

    fn value(&mut self, v: &Value, path: String, key_span: Option<Range<usize>>) {
        match v {
            Value::Array(a) => {
                self.record(path.clone(), key_span);
                for (i, e) in a.iter().enumerate() {
                    self.value(e, join(&path, &i.to_string()), None);
                }
            }
            Value::InlineTable(t) => {
                self.record(path.clone(), key_span.or_else(|| v.span()));
                for (k, e) in t.iter() {
                    let ks = t.get_key_value(k).and_then(|(k, _)| k.span());
                    self.value(e, join(&path, k), ks);
                }
            }
            scalar => self.record(path, scalar.span()),
        }
    }
}
