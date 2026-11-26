//! Parsers and serializers: JSON/JSONC, YAML, INI/gitconfig, and the TOML in-place merge.

use indexmap::IndexMap;
use toml_edit::{Array, ArrayOfTables, DocumentMut, InlineTable, Item, Table, TableLike, Value};

use crate::merge::{DELETE_MARKER, Mismatch, Node, Strategies, Strategy, merge_value};

/// A parse failure: 1-based line and a description.
pub type ParseErr = (u32, String);

// ------------------------------------------------------------------ JSON / JSONC

pub fn parse_json(text: &str, jsonc: bool) -> Result<Node, ParseErr> {
    let mut p = JsonParser {
        s: text.as_bytes(),
        i: 0,
        jsonc,
    };
    p.ws()?;
    let v = p.value(0)?;
    p.ws()?;
    if p.i < p.s.len() {
        return Err(p.err("trailing characters after the JSON value"));
    }
    Ok(v)
}

struct JsonParser<'a> {
    s: &'a [u8],
    i: usize,
    jsonc: bool,
}

impl JsonParser<'_> {
    fn line(&self) -> u32 {
        1 + self.s[..self.i.min(self.s.len())]
            .iter()
            .filter(|b| **b == b'\n')
            .count() as u32
    }
    fn err(&self, m: &str) -> ParseErr {
        (self.line(), m.to_string())
    }
    fn peek(&self) -> Option<u8> {
        self.s.get(self.i).copied()
    }
    fn ws(&mut self) -> Result<(), ParseErr> {
        loop {
            match self.peek() {
                Some(b' ' | b'\t' | b'\r' | b'\n') => self.i += 1,
                Some(b'/') if self.jsonc => match self.s.get(self.i + 1) {
                    Some(b'/') => {
                        while !matches!(self.peek(), None | Some(b'\n')) {
                            self.i += 1;
                        }
                    }
                    Some(b'*') => {
                        self.i += 2;
                        loop {
                            match self.peek() {
                                None => return Err(self.err("unterminated block comment")),
                                Some(b'*') if self.s.get(self.i + 1) == Some(&b'/') => {
                                    self.i += 2;
                                    break;
                                }
                                _ => self.i += 1,
                            }
                        }
                    }
                    _ => return Err(self.err("unexpected `/`")),
                },
                _ => return Ok(()),
            }
        }
    }
    fn value(&mut self, depth: usize) -> Result<Node, ParseErr> {
        if depth > 128 {
            return Err(self.err("nesting is too deep"));
        }
        match self.peek() {
            None => Err(self.err("unexpected end of input")),
            Some(b'{') => {
                self.i += 1;
                let mut t = IndexMap::new();
                loop {
                    self.ws()?;
                    if self.peek() == Some(b'}') {
                        self.i += 1;
                        break;
                    }
                    if self.peek() != Some(b'"') {
                        return Err(self.err("expected a string key"));
                    }
                    let k = self.string()?;
                    self.ws()?;
                    if self.peek() != Some(b':') {
                        return Err(self.err("expected `:`"));
                    }
                    self.i += 1;
                    self.ws()?;
                    let v = self.value(depth + 1)?;
                    t.insert(k, v);
                    self.ws()?;
                    match self.peek() {
                        Some(b',') => {
                            self.i += 1;
                            self.ws()?;
                            if self.peek() == Some(b'}') {
                                if !self.jsonc {
                                    return Err(self.err("trailing comma"));
                                }
                                self.i += 1;
                                break;
                            }
                        }
                        Some(b'}') => {
                            self.i += 1;
                            break;
                        }
                        _ => return Err(self.err("expected `,` or `}`")),
                    }
                }
                Ok(Node::Table(t))
            }
            Some(b'[') => {
                self.i += 1;
                let mut a = Vec::new();
                loop {
                    self.ws()?;
                    if self.peek() == Some(b']') {
                        self.i += 1;
                        break;
                    }
                    a.push(self.value(depth + 1)?);
                    self.ws()?;
                    match self.peek() {
                        Some(b',') => {
                            self.i += 1;
                            self.ws()?;
                            if self.peek() == Some(b']') {
                                if !self.jsonc {
                                    return Err(self.err("trailing comma"));
                                }
                                self.i += 1;
                                break;
                            }
                        }
                        Some(b']') => {
                            self.i += 1;
                            break;
                        }
                        _ => return Err(self.err("expected `,` or `]`")),
                    }
                }
                Ok(Node::Array(a))
            }
            Some(b'"') => Ok(Node::Str(self.string()?)),
            Some(b't') => self.lit("true", Node::Bool(true)),
            Some(b'f') => self.lit("false", Node::Bool(false)),
            Some(b'n') => self.lit("null", Node::Null),
            Some(c) if c == b'-' || c.is_ascii_digit() => {
                let st = self.i;
                while matches!(
                    self.peek(),
                    Some(b'-' | b'+' | b'.' | b'e' | b'E' | b'0'..=b'9')
                ) {
                    self.i += 1;
                }
                let t = std::str::from_utf8(&self.s[st..self.i]).unwrap_or("");
                if let Ok(n) = t.parse::<i64>() {
                    Ok(Node::Int(n))
                } else if let Ok(f) = t.parse::<f64>() {
                    Ok(Node::Float(f))
                } else {
                    Err(self.err("invalid number"))
                }
            }
            Some(_) => Err(self.err("unexpected character")),
        }
    }
    fn lit(&mut self, word: &str, n: Node) -> Result<Node, ParseErr> {
        if self.s[self.i..].starts_with(word.as_bytes()) {
            self.i += word.len();
            Ok(n)
        } else {
            Err(self.err("invalid literal"))
        }
    }
    fn hex4(&mut self) -> Result<u32, ParseErr> {
        let h = self
            .s
            .get(self.i..self.i + 4)
            .and_then(|b| std::str::from_utf8(b).ok());
        let v = h.and_then(|h| u32::from_str_radix(h, 16).ok());
        match v {
            Some(v) => {
                self.i += 4;
                Ok(v)
            }
            None => Err(self.err("invalid \\u escape")),
        }
    }
    fn string(&mut self) -> Result<String, ParseErr> {
        self.i += 1;
        let mut out: Vec<u8> = Vec::new();
        loop {
            match self.peek() {
                None | Some(b'\n') => return Err(self.err("unterminated string")),
                Some(b'"') => {
                    self.i += 1;
                    break;
                }
                Some(b'\\') => {
                    self.i += 1;
                    let c = self.peek().ok_or_else(|| self.err("unterminated string"))?;
                    self.i += 1;
                    let ch = match c {
                        b'n' => '\n',
                        b't' => '\t',
                        b'r' => '\r',
                        b'b' => '\u{8}',
                        b'f' => '\u{c}',
                        b'/' => '/',
                        b'\\' => '\\',
                        b'"' => '"',
                        b'u' => {
                            let mut cp = self.hex4()?;
                            if (0xD800..0xDC00).contains(&cp)
                                && self.s.get(self.i..self.i + 2) == Some(b"\\u")
                            {
                                self.i += 2;
                                let lo = self.hex4()?;
                                cp = 0x10000
                                    + ((cp - 0xD800) << 10)
                                    + (lo.wrapping_sub(0xDC00) & 0x3ff);
                            }
                            char::from_u32(cp).unwrap_or('\u{fffd}')
                        }
                        _ => return Err(self.err("invalid escape")),
                    };
                    let mut buf = [0u8; 4];
                    out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
                }
                Some(c) => {
                    out.push(c);
                    self.i += 1;
                }
            }
        }
        String::from_utf8(out).map_err(|_| self.err("invalid UTF-8 in string"))
    }
}

pub fn write_json(n: &Node) -> String {
    let mut s = String::new();
    json_node(n, 0, &mut s);
    s.push('\n');
    s
}

fn json_node(n: &Node, ind: usize, out: &mut String) {
    let pad = |k: usize| "  ".repeat(k);
    match n {
        Node::Null => out.push_str("null"),
        Node::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Node::Int(i) => out.push_str(&i.to_string()),
        Node::Float(f) => out.push_str(
            &serde_json::Number::from_f64(*f).map_or_else(|| "null".into(), |n| n.to_string()),
        ),
        Node::Str(s) => out.push_str(&serde_json::to_string(s).unwrap_or_default()),
        Node::Array(a) if a.is_empty() => out.push_str("[]"),
        Node::Array(a) => {
            out.push_str("[\n");
            for (i, v) in a.iter().enumerate() {
                out.push_str(&pad(ind + 1));
                json_node(v, ind + 1, out);
                out.push_str(if i + 1 < a.len() { ",\n" } else { "\n" });
            }
            out.push_str(&pad(ind));
            out.push(']');
        }
        Node::Table(t) if t.is_empty() => out.push_str("{}"),
        Node::Table(t) => {
            out.push_str("{\n");
            for (i, (k, v)) in t.iter().enumerate() {
                out.push_str(&pad(ind + 1));
                out.push_str(&serde_json::to_string(k).unwrap_or_default());
                out.push_str(": ");
                json_node(v, ind + 1, out);
                out.push_str(if i + 1 < t.len() { ",\n" } else { "\n" });
            }
            out.push_str(&pad(ind));
            out.push('}');
        }
    }
}

// ------------------------------------------------------------------ YAML

pub fn parse_yaml(text: &str) -> Result<Node, ParseErr> {
    if text.trim().is_empty() {
        return Ok(Node::Table(IndexMap::new()));
    }
    let v: serde_yaml_ng::Value = serde_yaml_ng::from_str(text)
        .map_err(|e| (e.location().map_or(1, |l| l.line() as u32), e.to_string()))?;
    Ok(yaml_to_node(v))
}

fn yaml_to_node(v: serde_yaml_ng::Value) -> Node {
    use serde_yaml_ng::Value as Y;
    match v {
        Y::Null => Node::Null,
        Y::Bool(b) => Node::Bool(b),
        Y::Number(n) => match n.as_i64() {
            Some(i) => Node::Int(i),
            None => Node::Float(n.as_f64().unwrap_or(0.0)),
        },
        Y::String(s) => Node::Str(s),
        Y::Sequence(s) => Node::Array(s.into_iter().map(yaml_to_node).collect()),
        Y::Mapping(m) => Node::Table(
            m.into_iter()
                .map(|(k, v)| {
                    let key = match k {
                        Y::String(s) => s,
                        other => serde_yaml_ng::to_string(&other)
                            .unwrap_or_default()
                            .trim()
                            .to_string(),
                    };
                    (key, yaml_to_node(v))
                })
                .collect(),
        ),
        Y::Tagged(t) => yaml_to_node(t.value),
    }
}

fn node_to_yaml(n: &Node) -> serde_yaml_ng::Value {
    use serde_yaml_ng::Value as Y;
    match n {
        Node::Null => Y::Null,
        Node::Bool(b) => Y::Bool(*b),
        Node::Int(i) => Y::Number((*i).into()),
        Node::Float(f) => Y::Number(serde_yaml_ng::Number::from(*f)),
        Node::Str(s) => Y::String(s.clone()),
        Node::Array(a) => Y::Sequence(a.iter().map(node_to_yaml).collect()),
        Node::Table(t) => {
            let mut m = serde_yaml_ng::Mapping::new();
            for (k, v) in t {
                m.insert(Y::String(k.clone()), node_to_yaml(v));
            }
            Y::Mapping(m)
        }
    }
}

pub fn write_yaml(n: &Node) -> String {
    serde_yaml_ng::to_string(&node_to_yaml(n)).unwrap_or_default()
}

// ------------------------------------------------------------------ INI / gitconfig

/// Sections become tables keyed by their bracket text (`remote "origin"`); keys before the first
/// section stay at the top level. A repeated key becomes an array; a bare key is `Null`.
pub fn parse_ini(text: &str) -> Result<Node, ParseErr> {
    let mut root: IndexMap<String, Node> = IndexMap::new();
    let mut section: Option<String> = None;
    for (n, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        if let Some(rest) = line.strip_prefix('[') {
            let Some(end) = rest.rfind(']') else {
                return Err((n as u32 + 1, "unterminated section header".into()));
            };
            let name = rest[..end].trim().to_string();
            root.entry(name.clone())
                .or_insert_with(|| Node::Table(IndexMap::new()));
            section = Some(name);
            continue;
        }
        let (k, v) = match line.split_once('=') {
            Some((k, v)) => (k.trim().to_string(), Node::Str(v.trim().to_string())),
            None => (line.to_string(), Node::Null),
        };
        let target = match &section {
            Some(s) => match root.get_mut(s) {
                Some(Node::Table(t)) => t,
                _ => return Err((n as u32 + 1, "section clashes with a key".into())),
            },
            None => &mut root,
        };
        match target.get_mut(&k) {
            None => {
                target.insert(k, v);
            }
            Some(Node::Array(a)) => a.push(v),
            Some(existing) => {
                let first = existing.clone();
                *existing = Node::Array(vec![first, v]);
            }
        }
    }
    Ok(Node::Table(root))
}

pub fn write_ini(n: &Node, indent: &str) -> String {
    let Node::Table(root) = n else {
        return String::new();
    };
    let mut out = String::new();
    let entry = |out: &mut String, pad: &str, k: &str, v: &Node| {
        let one = |out: &mut String, v: &Node| match v {
            Node::Null => out.push_str(&format!("{pad}{k}\n")),
            Node::Str(s) => out.push_str(&format!("{pad}{k} = {s}\n")),
            Node::Bool(b) => out.push_str(&format!("{pad}{k} = {b}\n")),
            Node::Int(i) => out.push_str(&format!("{pad}{k} = {i}\n")),
            Node::Float(f) => out.push_str(&format!("{pad}{k} = {f}\n")),
            _ => {}
        };
        match v {
            Node::Array(a) => a.iter().for_each(|x| one(out, x)),
            other => one(out, other),
        }
    };
    for (k, v) in root {
        if !matches!(v, Node::Table(_)) {
            entry(&mut out, "", k, v);
        }
    }
    for (k, v) in root {
        if let Node::Table(t) = v {
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(&format!("[{k}]\n"));
            for (kk, vv) in t {
                entry(&mut out, indent, kk, vv);
            }
        }
    }
    out
}

// ------------------------------------------------------------------ TOML (in place)

pub fn parse_toml(text: &str) -> Result<DocumentMut, ParseErr> {
    text.parse::<DocumentMut>().map_err(|e| {
        let line = e.span().map_or(1, |s| {
            1 + text.as_bytes()[..s.start.min(text.len())]
                .iter()
                .filter(|b| **b == b'\n')
                .count() as u32
        });
        (line, e.message().to_string())
    })
}

fn is_delete(i: &Item) -> bool {
    i.as_str() == Some(DELETE_MARKER)
}

fn item_to_node(i: &Item) -> Node {
    match i {
        Item::None => Node::Null,
        Item::Value(v) => value_to_node(v),
        Item::Table(t) => table_to_node(t),
        Item::ArrayOfTables(a) => Node::Array(a.iter().map(|t| table_to_node(t)).collect()),
    }
}

fn table_to_node(t: &dyn TableLike) -> Node {
    Node::Table(
        t.iter()
            .map(|(k, v)| (k.to_string(), item_to_node(v)))
            .collect(),
    )
}

fn value_to_node(v: &Value) -> Node {
    match v {
        Value::String(s) => Node::Str(s.value().clone()),
        Value::Integer(i) => Node::Int(*i.value()),
        Value::Float(f) => Node::Float(*f.value()),
        Value::Boolean(b) => Node::Bool(*b.value()),
        Value::Datetime(d) => Node::Str(d.value().to_string()),
        Value::Array(a) => Node::Array(a.iter().map(value_to_node).collect()),
        Value::InlineTable(t) => table_to_node(t),
    }
}

fn node_to_value(n: &Node) -> Value {
    match n {
        Node::Null => Value::from(""),
        Node::Bool(b) => Value::from(*b),
        Node::Int(i) => Value::from(*i),
        Node::Float(f) => Value::from(*f),
        Node::Str(s) => Value::from(s.as_str()),
        Node::Array(a) => {
            let mut arr = Array::new();
            a.iter().for_each(|x| arr.push(node_to_value(x)));
            Value::Array(arr)
        }
        Node::Table(t) => {
            let mut it = InlineTable::new();
            for (k, v) in t {
                it.insert(k, node_to_value(v));
            }
            Value::InlineTable(it)
        }
    }
}

pub fn node_to_item(n: &Node) -> Item {
    match n {
        Node::Array(a) if !a.is_empty() && a.iter().all(|x| matches!(x, Node::Table(_))) => {
            let mut aot = ArrayOfTables::new();
            for x in a {
                if let Node::Table(t) = x {
                    let mut tb = Table::new();
                    for (k, v) in t {
                        tb.insert(k, node_to_item(v));
                    }
                    aot.push(tb);
                }
            }
            Item::ArrayOfTables(aot)
        }
        other => Item::Value(node_to_value(other)),
    }
}

fn strip_toml_deletes(t: &mut dyn TableLike) {
    let dead: Vec<String> = t
        .iter()
        .filter(|(_, v)| is_delete(v))
        .map(|(k, _)| k.to_string())
        .collect();
    for k in dead {
        t.remove(&k);
    }
    for (_, v) in t.iter_mut() {
        if let Some(sub) = v.as_table_like_mut() {
            strip_toml_deletes(sub);
        }
    }
}

/// Merge `over` into `base` in place. The base layer keeps its comments and order.
pub fn merge_toml(
    base: &mut dyn TableLike,
    over: &dyn TableLike,
    path: &str,
    strat: &Strategies,
) -> Result<(), Mismatch> {
    for (k, ov) in over.iter() {
        let p = if path.is_empty() {
            k.to_string()
        } else {
            format!("{path}.{k}")
        };
        if is_delete(ov) {
            base.remove(k);
            continue;
        }
        let Some(bv) = base.get_mut(k) else {
            let mut item = ov.clone();
            if let Some(t) = item.as_table_like_mut() {
                strip_toml_deletes(t);
            }
            base.insert(k, item);
            continue;
        };
        if let (Some(bt), Some(ot)) = (bv.as_table_like_mut(), ov.as_table_like()) {
            merge_toml(bt, ot, &p, strat)?;
            continue;
        }
        let mut merged = item_to_node(bv);
        merge_value(&mut merged, item_to_node(ov), &p, strat)?;
        let arrays_combined = matches!(merged, Node::Array(_))
            && !matches!(strat.get(&p), None | Some(Strategy::Replace));
        *bv = if arrays_combined {
            node_to_item(&merged)
        } else {
            ov.clone()
        };
    }
    Ok(())
}

pub fn finish_toml(doc: &mut DocumentMut) {
    strip_toml_deletes(doc.as_table_mut());
}

/// The whole document as a value tree (comments dropped), for comparing and linting.
pub fn toml_to_node(doc: &DocumentMut) -> Node {
    table_to_node(doc.as_table())
}

/// A JSON value as a tree node.
pub fn json_to_node(v: &serde_json::Value) -> Node {
    use serde_json::Value as J;
    match v {
        J::Null => Node::Null,
        J::Bool(b) => Node::Bool(*b),
        J::Number(n) => n
            .as_i64()
            .map(Node::Int)
            .unwrap_or_else(|| Node::Float(n.as_f64().unwrap_or(0.0))),
        J::String(s) => Node::Str(s.clone()),
        J::Array(a) => Node::Array(a.iter().map(json_to_node).collect()),
        J::Object(o) => Node::Table(
            o.iter()
                .map(|(k, v)| (k.clone(), json_to_node(v)))
                .collect(),
        ),
    }
}

/// Set (`Some`) or delete (`None`) the key at `path` in place. A path that crosses a value that
/// is not a table fails with the path walked so far and the type found there.
pub fn patch_toml(
    doc: &mut DocumentMut,
    path: &[String],
    value: Option<&Node>,
) -> Result<(), (String, &'static str)> {
    let Some((last, parents)) = path.split_last() else {
        return Ok(());
    };
    let mut cur: &mut dyn TableLike = doc.as_table_mut();
    for (i, seg) in parents.iter().enumerate() {
        if cur.get(seg).is_none() {
            if value.is_none() {
                return Ok(());
            }
            let mut t = Table::new();
            t.set_implicit(true);
            cur.insert(seg, Item::Table(t));
        }
        let item = cur.get_mut(seg).expect("just inserted");
        let found = item.type_name();
        cur = item
            .as_table_like_mut()
            .ok_or_else(|| (path[..=i].join("."), found))?;
    }
    match value {
        Some(v) => {
            cur.insert(last, node_to_item(v));
        }
        None => {
            cur.remove(last);
        }
    }
    Ok(())
}
