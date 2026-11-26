//! The condition language (task 2.5): `all() any() not()`, `key = "v"`, `!=`, ordered
//! comparisons and bare boolean keys.

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};

use dots_core::report::{Diagnostic, Provenance};

use crate::chain::Chain;
use crate::facts::{FACT_NAMES, Facts, Value, suggest};

/// A condition error with a stable registry code and, for syntax problems, a 1-based column.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CondError {
    pub code: &'static str,
    pub message: String,
    pub col: Option<usize>,
    pub suggestion: Option<String>,
    args: Vec<(String, String)>,
}

impl CondError {
    fn new(code: &'static str, col: Option<usize>, args: &[(&str, &str)]) -> Self {
        let mut a: Vec<(String, String)> = args
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        let suggestion = a
            .iter()
            .find(|(k, _)| k == "suggestion")
            .map(|(_, v)| v.clone());
        if suggestion.is_none() {
            a.push(("suggestion".into(), String::new()));
        }
        let borrowed: Vec<(&str, &str)> = a.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
        let message = Diagnostic::from_code(code, &borrowed).message;
        CondError {
            code,
            message,
            col,
            suggestion: suggestion.filter(|s| !s.is_empty()),
            args: a,
        }
    }

    fn syntax(col: usize, detail: &str) -> Self {
        Self::new("condition-syntax", Some(col), &[("detail", detail)])
    }

    fn typ(col: usize, detail: &str) -> Self {
        Self::new("condition-type", Some(col), &[("detail", detail)])
    }

    fn unknown_fact(col: usize, key: &str) -> Self {
        let sug = suggest(key, FACT_NAMES.iter().copied().chain(["tag", "platform"]));
        Self::new(
            "unknown-fact",
            Some(col),
            &[("key", key), ("suggestion", sug.as_deref().unwrap_or(""))],
        )
    }

    fn unknown_tag(col: usize, tag: &str, declared: &BTreeSet<String>) -> Self {
        let sug = suggest(tag, declared.iter().map(String::as_str));
        Self::new(
            "unknown-tag",
            Some(col),
            &[("tag", tag), ("suggestion", sug.as_deref().unwrap_or(""))],
        )
    }

    fn setting_unresolved(col: usize, reference: &str) -> Self {
        Self::new(
            "setting-ref-unresolved",
            Some(col),
            &[("reference", reference)],
        )
    }

    /// Convert to a report diagnostic. `file`/`line` locate the string holding the condition;
    /// the column is within that string.
    pub fn to_diagnostic(&self, file: Option<&str>, line: Option<u32>) -> Diagnostic {
        let hint_sug = self
            .suggestion
            .as_ref()
            .map_or(String::new(), |s| format!(" Did you mean `{s}`?"));
        let borrowed: Vec<(&str, &str)> = self
            .args
            .iter()
            .map(|(k, v)| {
                if self.code == "unknown-fact" && k == "suggestion" {
                    (k.as_str(), hint_sug.as_str())
                } else {
                    (k.as_str(), v.as_str())
                }
            })
            .collect();
        let mut d = Diagnostic::from_code(self.code, &borrowed);
        if self.suggestion.is_none() && self.code == "unknown-tag" {
            d.hint = "Declare it under `tags` in dots.toml.".into();
        }
        if file.is_some() || line.is_some() || self.col.is_some() {
            d = d.with_provenance(Provenance {
                source: "condition".into(),
                file: file.map(str::to_string),
                line,
                col: self.col.map(|c| c as u32),
            });
        }
        d
    }
}

impl std::fmt::Display for CondError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.col {
            Some(c) => write!(f, "{} (column {c})", self.message),
            None => f.write_str(&self.message),
        }
    }
}

impl std::error::Error for CondError {}

/// A reference to a package setting.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct SettingRef {
    /// `None` for `setting.<k>` (the enclosing package's own setting).
    pub package: Option<String>,
    pub key: String,
}

impl SettingRef {
    pub fn display(&self) -> String {
        match &self.package {
            Some(p) => format!("settings.{p}.{}", self.key),
            None => format!("setting.{}", self.key),
        }
    }
}

/// What a condition touches (for lint and implicit ordering edges).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct References {
    pub facts: BTreeSet<String>,
    pub tags: BTreeSet<String>,
    pub platforms: BTreeSet<String>,
    pub settings: BTreeSet<SettingRef>,
}

/// Caller-supplied setting resolution.
pub trait SettingLookup {
    /// Is the setting declared? `package = None` means the enclosing package's own.
    fn resolves(&self, package: Option<&str>, key: &str) -> bool;
    /// Its current value (only called when `resolves` is true).
    fn value(&self, package: Option<&str>, key: &str) -> Option<Value>;
}

/// No settings at all (every reference is unresolved).
pub struct NoSettings;

impl SettingLookup for NoSettings {
    fn resolves(&self, _: Option<&str>, _: &str) -> bool {
        false
    }
    fn value(&self, _: Option<&str>, _: &str) -> Option<Value> {
        None
    }
}

/// A fixed set of settings, keyed `(package, key)`; `package` `""` is the own package.
#[derive(Debug, Clone, Default)]
pub struct MapSettings(pub BTreeMap<(String, String), Value>);

impl MapSettings {
    pub fn with(mut self, package: Option<&str>, key: &str, v: Value) -> Self {
        self.0
            .insert((package.unwrap_or("").to_string(), key.to_string()), v);
        self
    }
}

impl SettingLookup for MapSettings {
    fn resolves(&self, package: Option<&str>, key: &str) -> bool {
        self.value(package, key).is_some()
    }
    fn value(&self, package: Option<&str>, key: &str) -> Option<Value> {
        self.0
            .get(&(package.unwrap_or("").to_string(), key.to_string()))
            .cloned()
    }
}

/// Everything evaluation reads.
pub struct EvalCtx<'a> {
    pub facts: &'a Facts,
    pub chain: &'a Chain,
    /// Tags active on this machine.
    pub tags: &'a BTreeSet<String>,
    /// Tags declared in dots.toml; any other tag in a condition is an error.
    pub declared_tags: &'a BTreeSet<String>,
    pub settings: &'a dyn SettingLookup,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Key {
    Fact(String),
    Provider(String),
    Tag,
    Platform,
    Setting(SettingRef),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Op {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

#[derive(Debug, Clone, PartialEq)]
enum Lit {
    Str(String),
    Num(String),
    Bool(bool),
}

impl Lit {
    fn text(&self) -> String {
        match self {
            Lit::Str(s) | Lit::Num(s) => s.clone(),
            Lit::Bool(b) => b.to_string(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
enum Node {
    Const(bool),
    All(Vec<Node>),
    Any(Vec<Node>),
    Not(Box<Node>),
    Bare(Key, usize),
    Cmp {
        key: Key,
        key_col: usize,
        op: Op,
        lit: Lit,
        lit_col: usize,
    },
}

/// A parsed condition.
#[derive(Debug, Clone, PartialEq)]
pub struct Condition {
    root: Node,
}

impl Condition {
    pub fn parse(src: &str) -> Result<Condition, CondError> {
        let mut p = Parser {
            chars: src.chars().collect(),
            pos: 0,
        };
        p.ws();
        if p.eof() {
            return Err(CondError::syntax(1, "empty condition"));
        }
        let root = p.expr()?;
        p.ws();
        if !p.eof() {
            return Err(CondError::syntax(
                p.col(),
                &format!("unexpected `{}` after the condition", p.chars[p.pos]),
            ));
        }
        Ok(Condition { root })
    }

    pub fn eval(&self, ctx: &EvalCtx) -> Result<bool, CondError> {
        eval(&self.root, ctx)
    }

    /// Everything the condition reads.
    pub fn references(&self) -> References {
        let mut r = References::default();
        collect(&self.root, &mut r);
        r
    }

    /// Lint-time check: undeclared tags and unresolvable settings, without evaluating.
    pub fn check(
        &self,
        declared_tags: &BTreeSet<String>,
        settings: &dyn SettingLookup,
    ) -> Vec<CondError> {
        let mut errs = Vec::new();
        check(&self.root, declared_tags, settings, &mut errs);
        errs
    }
}

impl std::str::FromStr for Condition {
    type Err = CondError;
    fn from_str(s: &str) -> Result<Self, CondError> {
        Condition::parse(s)
    }
}

// ---- parser -------------------------------------------------------------------------------

struct Parser {
    chars: Vec<char>,
    pos: usize,
}

impl Parser {
    fn eof(&self) -> bool {
        self.pos >= self.chars.len()
    }
    fn col(&self) -> usize {
        self.pos + 1
    }
    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }
    fn ws(&mut self) {
        while self.peek().is_some_and(char::is_whitespace) {
            self.pos += 1;
        }
    }

    fn ident(&mut self) -> Option<String> {
        let start = self.pos;
        if !self
            .peek()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        {
            return None;
        }
        while self
            .peek()
            .is_some_and(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'))
        {
            self.pos += 1;
        }
        Some(self.chars[start..self.pos].iter().collect())
    }

    fn expr(&mut self) -> Result<Node, CondError> {
        self.ws();
        let col = self.col();
        let Some(name) = self.ident() else {
            return Err(match self.peek() {
                Some(c) => CondError::syntax(
                    col,
                    &format!("unexpected `{c}`; expected a key or all/any/not"),
                ),
                None => CondError::syntax(col, "unexpected end of condition"),
            });
        };
        self.ws();
        if self.peek() == Some('(') && matches!(name.as_str(), "all" | "any" | "not") {
            self.pos += 1;
            let args = self.args()?;
            return match name.as_str() {
                "all" => Ok(Node::All(args)),
                "any" => Ok(Node::Any(args)),
                _ => {
                    let mut it = args.into_iter();
                    match (it.next(), it.next()) {
                        (Some(n), None) => Ok(Node::Not(Box::new(n))),
                        _ => Err(CondError::syntax(col, "`not` takes exactly one condition")),
                    }
                }
            };
        }
        if self.peek() == Some('(') {
            return Err(CondError::syntax(
                col,
                &format!("unknown function `{name}`; use all, any or not"),
            ));
        }
        match name.as_str() {
            "true" | "false" if self.op_ahead().is_none() => {
                return Ok(Node::Const(name == "true"));
            }
            _ => {}
        }
        let key = resolve_key(&name, col)?;
        match self.op()? {
            None => Ok(Node::Bare(key, col)),
            Some(op) => {
                self.ws();
                let lit_col = self.col();
                let lit = self.literal()?;
                Ok(Node::Cmp {
                    key,
                    key_col: col,
                    op,
                    lit,
                    lit_col,
                })
            }
        }
    }

    fn args(&mut self) -> Result<Vec<Node>, CondError> {
        let mut out = Vec::new();
        loop {
            self.ws();
            match self.peek() {
                Some(')') => {
                    self.pos += 1;
                    return Ok(out);
                }
                None => return Err(CondError::syntax(self.col(), "missing `)`")),
                _ => {}
            }
            out.push(self.expr()?);
            self.ws();
            match self.peek() {
                Some(',') => self.pos += 1,
                Some(')') => {}
                Some(c) => {
                    return Err(CondError::syntax(
                        self.col(),
                        &format!("expected `,` or `)`, found `{c}`"),
                    ));
                }
                None => return Err(CondError::syntax(self.col(), "missing `)`")),
            }
        }
    }

    fn op_ahead(&mut self) -> Option<Op> {
        let save = self.pos;
        self.ws();
        let r = self.op().ok().flatten();
        self.pos = save;
        r
    }

    fn op(&mut self) -> Result<Option<Op>, CondError> {
        self.ws();
        let c = self.peek();
        let next = self.chars.get(self.pos + 1).copied();
        let (op, len) = match (c, next) {
            (Some('!'), Some('=')) => (Op::Ne, 2),
            (Some('<'), Some('=')) => (Op::Le, 2),
            (Some('>'), Some('=')) => (Op::Ge, 2),
            (Some('='), Some('=')) => (Op::Eq, 2),
            (Some('='), _) => (Op::Eq, 1),
            (Some('<'), _) => (Op::Lt, 1),
            (Some('>'), _) => (Op::Gt, 1),
            (Some('!'), _) => return Err(CondError::syntax(self.col(), "expected `!=`")),
            _ => return Ok(None),
        };
        self.pos += len;
        Ok(Some(op))
    }

    fn literal(&mut self) -> Result<Lit, CondError> {
        let col = self.col();
        match self.peek() {
            Some('"') => {
                self.pos += 1;
                let mut s = String::new();
                loop {
                    match self.peek() {
                        None => return Err(CondError::syntax(col, "unterminated string")),
                        Some('"') => {
                            self.pos += 1;
                            return Ok(Lit::Str(s));
                        }
                        Some('\\') => {
                            self.pos += 1;
                            match self.peek() {
                                Some(c @ ('"' | '\\')) => {
                                    s.push(c);
                                    self.pos += 1;
                                }
                                _ => {
                                    return Err(CondError::syntax(
                                        self.col(),
                                        "invalid escape; use \\\" or \\\\",
                                    ));
                                }
                            }
                        }
                        Some(c) => {
                            s.push(c);
                            self.pos += 1;
                        }
                    }
                }
            }
            Some(c) if c.is_ascii_digit() || c == '-' => {
                let start = self.pos;
                self.pos += 1;
                while self.peek().is_some_and(|c| c.is_ascii_digit() || c == '.') {
                    self.pos += 1;
                }
                let t: String = self.chars[start..self.pos].iter().collect();
                if t.parse::<f64>().is_err() {
                    return Err(CondError::syntax(col, &format!("`{t}` is not a number")));
                }
                Ok(Lit::Num(t))
            }
            Some(c) if c.is_ascii_alphabetic() => {
                let w = self.ident().unwrap_or_default();
                match w.as_str() {
                    "true" => Ok(Lit::Bool(true)),
                    "false" => Ok(Lit::Bool(false)),
                    _ => Err(CondError::syntax(
                        col,
                        &format!("`{w}` must be quoted: \"{w}\""),
                    )),
                }
            }
            Some(c) => Err(CondError::syntax(
                col,
                &format!("expected a value, found `{c}`"),
            )),
            None => Err(CondError::syntax(
                col,
                "expected a value after the operator",
            )),
        }
    }
}

fn resolve_key(name: &str, col: usize) -> Result<Key, CondError> {
    if FACT_NAMES.contains(&name) {
        return Ok(Key::Fact(name.to_string()));
    }
    match name {
        "tag" => return Ok(Key::Tag),
        "platform" => return Ok(Key::Platform),
        _ => {}
    }
    if let Some(id) = name.strip_prefix("provider.").filter(|s| !s.is_empty()) {
        return Ok(Key::Provider(id.to_string()));
    }
    if let Some(k) = name.strip_prefix("setting.").filter(|s| !s.is_empty()) {
        return Ok(Key::Setting(SettingRef {
            package: None,
            key: k.to_string(),
        }));
    }
    if let Some(rest) = name.strip_prefix("settings.") {
        if let Some((pkg, k)) = rest
            .split_once('.')
            .filter(|(p, k)| !p.is_empty() && !k.is_empty())
        {
            return Ok(Key::Setting(SettingRef {
                package: Some(pkg.to_string()),
                key: k.to_string(),
            }));
        }
        return Err(CondError::syntax(
            col,
            "use `settings.<package>.<key>` to read another package's setting",
        ));
    }
    Err(CondError::unknown_fact(col, name))
}

// ---- evaluation ---------------------------------------------------------------------------

fn eval(n: &Node, ctx: &EvalCtx) -> Result<bool, CondError> {
    match n {
        Node::Const(b) => Ok(*b),
        // No short-circuiting: an error in any branch must surface, never turn into false.
        Node::All(v) => v.iter().try_fold(true, |acc, c| Ok(eval(c, ctx)? && acc)),
        Node::Any(v) => v.iter().try_fold(false, |acc, c| Ok(eval(c, ctx)? || acc)),
        Node::Not(c) => Ok(!eval(c, ctx)?),
        Node::Bare(key, col) => match key {
            Key::Fact(name) => match ctx.facts.get(name) {
                Some(Value::Bool(b)) => Ok(b),
                _ => Err(CondError::typ(
                    *col,
                    &format!("`{name}` is not a boolean; compare it, e.g. `{name} = \"...\"`"),
                )),
            },
            Key::Provider(id) => provider(ctx, id, *col),
            Key::Tag | Key::Platform => Err(CondError::typ(
                *col,
                "`tag` and `platform` need a comparison, e.g. `tag = \"work\"`",
            )),
            Key::Setting(r) => match setting(ctx, r, *col)? {
                Value::Bool(b) => Ok(b),
                _ => Err(CondError::typ(
                    *col,
                    &format!("`{}` is not a boolean; compare it", r.display()),
                )),
            },
        },
        Node::Cmp {
            key,
            key_col,
            op,
            lit,
            lit_col,
        } => match key {
            Key::Tag => {
                let t = lit.text();
                if !ctx.declared_tags.contains(&t) {
                    return Err(CondError::unknown_tag(*lit_col, &t, ctx.declared_tags));
                }
                set_op(*op, ctx.tags.contains(&t), *key_col)
            }
            Key::Platform => set_op(*op, ctx.chain.contains(&lit.text()), *key_col),
            Key::Fact(name) => {
                let v = ctx.facts.get(name).unwrap_or(Value::Str(String::new()));
                compare(&v, *op, lit, *lit_col)
            }
            Key::Provider(id) => {
                let b = provider(ctx, id, *key_col)?;
                compare(&Value::Bool(b), *op, lit, *lit_col)
            }
            Key::Setting(r) => {
                let v = setting(ctx, r, *key_col)?;
                compare(&v, *op, lit, *lit_col)
            }
        },
    }
}

fn set_op(op: Op, member: bool, col: usize) -> Result<bool, CondError> {
    match op {
        Op::Eq => Ok(member),
        Op::Ne => Ok(!member),
        _ => Err(CondError::typ(
            col,
            "`tag` and `platform` only support `=` and `!=`",
        )),
    }
}

fn provider(ctx: &EvalCtx, id: &str, col: usize) -> Result<bool, CondError> {
    match ctx.facts.get(&format!("provider.{id}")) {
        Some(Value::Bool(b)) => Ok(b),
        _ => {
            let sug = suggest(id, ctx.facts.providers.keys().map(String::as_str))
                .map(|s| format!("provider.{s}"));
            Err(CondError::new(
                "unknown-fact",
                Some(col),
                &[
                    ("key", &format!("provider.{id}")),
                    ("suggestion", sug.as_deref().unwrap_or("")),
                ],
            ))
        }
    }
}

fn setting(ctx: &EvalCtx, r: &SettingRef, col: usize) -> Result<Value, CondError> {
    let pkg = r.package.as_deref();
    if !ctx.settings.resolves(pkg, &r.key) {
        return Err(CondError::setting_unresolved(col, &r.display()));
    }
    ctx.settings
        .value(pkg, &r.key)
        .ok_or_else(|| CondError::setting_unresolved(col, &r.display()))
}

fn compare(v: &Value, op: Op, lit: &Lit, lit_col: usize) -> Result<bool, CondError> {
    let left = v.render();
    let right = lit.text();
    match op {
        Op::Eq | Op::Ne => {
            let eq = left == right
                || matches!((v, lit), (Value::Num(n), Lit::Num(s)) if s.parse::<f64>().is_ok_and(|x| x == *n))
                || matches!((v, lit), (Value::Str(s), Lit::Num(t)) if matches!(
                    (s.parse::<f64>(), t.parse::<f64>()), (Ok(a), Ok(b)) if a == b));
            Ok(if op == Op::Eq { eq } else { !eq })
        }
        _ => {
            if left.is_empty() {
                // An absent fact (`distro_version` on macOS) satisfies no ordered test.
                return Ok(false);
            }
            let ord = compare_versions(&left, &right)
                .or_else(|| {
                    let (a, b) = (left.parse::<f64>().ok()?, right.parse::<f64>().ok()?);
                    a.partial_cmp(&b)
                })
                .ok_or_else(|| {
                    CondError::typ(
                        lit_col,
                        &format!(
                            "cannot order `{left}` and `{right}`; both must be versions or numbers"
                        ),
                    )
                })?;
            Ok(match op {
                Op::Lt => ord == Ordering::Less,
                Op::Le => ord != Ordering::Greater,
                Op::Gt => ord == Ordering::Greater,
                _ => ord != Ordering::Less,
            })
        }
    }
}

/// Compare dotted numeric versions segment by segment (`3.10 > 3.9`, `24.04 == 24.4`,
/// missing segments count as 0). `None` when either side is not a dotted number.
pub fn compare_versions(a: &str, b: &str) -> Option<Ordering> {
    fn segs(s: &str) -> Option<Vec<u64>> {
        s.split('.').map(|p| p.parse::<u64>().ok()).collect()
    }
    let (a, b) = (segs(a)?, segs(b)?);
    for i in 0..a.len().max(b.len()) {
        let o = a.get(i).unwrap_or(&0).cmp(b.get(i).unwrap_or(&0));
        if o != Ordering::Equal {
            return Some(o);
        }
    }
    Some(Ordering::Equal)
}

// ---- references / check -------------------------------------------------------------------

fn collect(n: &Node, r: &mut References) {
    match n {
        Node::Const(_) => {}
        Node::All(v) | Node::Any(v) => v.iter().for_each(|c| collect(c, r)),
        Node::Not(c) => collect(c, r),
        Node::Bare(k, _) => note_key(k, None, r),
        Node::Cmp { key, lit, .. } => note_key(key, Some(lit), r),
    }
}

fn note_key(k: &Key, lit: Option<&Lit>, r: &mut References) {
    match k {
        Key::Fact(n) => {
            r.facts.insert(n.clone());
        }
        Key::Provider(id) => {
            r.facts.insert(format!("provider.{id}"));
        }
        Key::Tag => {
            if let Some(l) = lit {
                r.tags.insert(l.text());
            }
        }
        Key::Platform => {
            if let Some(l) = lit {
                r.platforms.insert(l.text());
            }
        }
        Key::Setting(s) => {
            r.settings.insert(s.clone());
        }
    }
}

fn check(
    n: &Node,
    declared: &BTreeSet<String>,
    settings: &dyn SettingLookup,
    out: &mut Vec<CondError>,
) {
    match n {
        Node::Const(_) => {}
        Node::All(v) | Node::Any(v) => v.iter().for_each(|c| check(c, declared, settings, out)),
        Node::Not(c) => check(c, declared, settings, out),
        Node::Bare(Key::Setting(s), col) => check_setting(s, *col, settings, out),
        Node::Bare(..) => {}
        Node::Cmp {
            key,
            key_col,
            lit,
            lit_col,
            ..
        } => match key {
            Key::Tag if !declared.contains(&lit.text()) => {
                out.push(CondError::unknown_tag(*lit_col, &lit.text(), declared));
            }
            Key::Setting(s) => check_setting(s, *key_col, settings, out),
            _ => {}
        },
    }
}

fn check_setting(s: &SettingRef, col: usize, lookup: &dyn SettingLookup, out: &mut Vec<CondError>) {
    if !lookup.resolves(s.package.as_deref(), &s.key) {
        out.push(CondError::setting_unresolved(col, &s.display()));
    }
}
