//! A small declarative schema and the strict validator that walks a parsed TOML document
//! against it. Serde alone cannot give "did you mean" suggestions or `file:line:col`, so every
//! file is validated here first and only then deserialized into the typed structs.

use toml_edit::Item;

use crate::source::{Diags, SourceFile, suggest};

/// The expected shape of a value.
#[derive(Debug, Clone, Copy)]
pub enum Ty {
    Any,
    Str,
    Int,
    Bool,
    /// A float; integers are accepted too.
    Float,
    /// A string restricted to a set of values.
    Enum(&'static [&'static str]),
    /// A package-relative path (checked for escapes in package files).
    Path,
    /// A shell command or script reference (first token checked for `..` escapes).
    Cmd,
    /// `>=X.Y[.Z]`.
    DotsReq,
    /// A version spec: `latest`, `X.Y.Z`, `X[.Y]`, `>=X[.Y[.Z]]` or `git:<ref>`.
    VersionSpec,
    /// A list. Inside `[platform.*]` tables a `{ add, remove }` edit table is accepted too.
    List(&'static Ty),
    /// A list that always accepts the `{ add, remove }` edit form (machine files).
    ListEdit(&'static Ty),
    /// A table with a fixed key set, and no `platform` tables.
    Table(&'static [(&'static str, Ty)]),
    /// Like `Table`, but also accepts `[platform."<key>"]` partial copies of itself.
    Obj(&'static [(&'static str, Ty)]),
    /// A table with arbitrary keys whose values share a type.
    Map(&'static Ty),
    /// A table with arbitrary content.
    Open,
    /// `[providers.<id>]`: known keys for built-in ids, an open table for any other id.
    Providers,
    /// Any of several shapes. Errors are reported against the alternative that matches the
    /// value's kind.
    Alt(&'static [Ty]),
}

impl Ty {
    fn describe(&self) -> String {
        match self {
            Ty::Any => "any value".into(),
            Ty::Str | Ty::Path | Ty::Cmd | Ty::DotsReq | Ty::VersionSpec => "a string".into(),
            Ty::Int => "an integer".into(),
            Ty::Bool => "a boolean".into(),
            Ty::Float => "a number".into(),
            Ty::Enum(vs) => format!("one of {}", vs.join(", ")),
            Ty::List(_) | Ty::ListEdit(_) => "a list".into(),
            Ty::Table(_) | Ty::Obj(_) | Ty::Map(_) | Ty::Open | Ty::Providers => "a table".into(),
            Ty::Alt(alts) => {
                let mut v: Vec<String> = alts.iter().map(Ty::describe).collect();
                v.dedup();
                v.join(" or ")
            }
        }
    }

    fn kind(&self) -> Kind {
        match self {
            Ty::Any => Kind::Any,
            Ty::List(_) | Ty::ListEdit(_) => Kind::List,
            Ty::Table(_) | Ty::Obj(_) | Ty::Map(_) | Ty::Open | Ty::Providers => Kind::Table,
            _ => Kind::Scalar,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Any,
    Scalar,
    List,
    Table,
}

fn item_kind(item: &Item) -> Kind {
    if item.as_table_like().is_some() {
        Kind::Table
    } else if item.is_array() || item.is_array_of_tables() {
        Kind::List
    } else {
        Kind::Scalar
    }
}

fn found(item: &Item) -> &'static str {
    match item {
        Item::Value(v) => match v {
            toml_edit::Value::String(_) => "a string",
            toml_edit::Value::Integer(_) => "an integer",
            toml_edit::Value::Float(_) => "a number",
            toml_edit::Value::Boolean(_) => "a boolean",
            toml_edit::Value::Datetime(_) => "a datetime",
            toml_edit::Value::Array(_) => "a list",
            toml_edit::Value::InlineTable(_) => "a table",
        },
        Item::Table(_) => "a table",
        Item::ArrayOfTables(_) => "a list of tables",
        Item::None => "nothing",
    }
}

/// Immutable context for one validation run.
pub struct Env<'a> {
    pub src: &'a SourceFile,
    /// Check package-relative paths (package files only).
    pub check_paths: bool,
    /// Validating an override file: a partial copy of its target. Lists accept
    /// `{ add, remove, prepend }` everywhere, keyed entries may be tables, and any entry
    /// accepts `remove = true`.
    pub partial: bool,
    /// Built-in provider ids and their tables, for `Ty::Providers`.
    pub provider_ty: fn(&str) -> Option<&'static Ty>,
}

/// Mutable results of one validation run.
#[derive(Default)]
pub struct State {
    pub diags: Diags,
    /// `//`-prefixed paths seen (accepted, but they block `add --copy`).
    pub escape_hatch: Vec<String>,
    /// Every `[platform."<key>"]` key seen, with the key's byte span.
    pub platform_keys: Vec<(String, Option<std::ops::Range<usize>>)>,
}

/// Validate a whole document against `ty`.
pub fn validate(env: &Env, state: &mut State, ty: &Ty, root: &Item) {
    check(env, state, ty, "", root, false);
}

fn span_of(item: &Item) -> Option<std::ops::Range<usize>> {
    item.span()
}

fn wrong_type(env: &Env, st: &mut State, ty: &Ty, key: &str, item: &Item) {
    st.diags.error(
        "schema-wrong-type",
        &[
            ("key", key),
            ("expected", &ty.describe()),
            ("found", found(item)),
        ],
        env.src,
        span_of(item),
    );
}

fn invalid(env: &Env, st: &mut State, key: &str, item: &Item, detail: &str, hint: String) {
    st.diags.report(
        false,
        "schema-invalid-value",
        &[("key", key), ("detail", detail)],
        env.src,
        span_of(item),
        Some(hint),
    );
}

fn list_items(item: &Item) -> Option<Vec<Item>> {
    match item {
        Item::Value(toml_edit::Value::Array(a)) => {
            Some(a.iter().map(|v| Item::Value(v.clone())).collect())
        }
        Item::ArrayOfTables(a) => Some(a.iter().map(|t| Item::Table(t.clone())).collect()),
        _ => None,
    }
}

fn check(env: &Env, st: &mut State, ty: &Ty, key: &str, item: &Item, plat: bool) {
    match ty {
        Ty::Any => {}
        Ty::Str => {
            if item.as_str().is_none() {
                wrong_type(env, st, ty, key, item);
            }
        }
        Ty::Int => {
            if item.as_integer().is_none() {
                wrong_type(env, st, ty, key, item);
            }
        }
        Ty::Bool => {
            if item.as_bool().is_none() {
                wrong_type(env, st, ty, key, item);
            }
        }
        Ty::Float => {
            if item.as_float().is_none() && item.as_integer().is_none() {
                wrong_type(env, st, ty, key, item);
            }
        }
        Ty::Enum(vs) => match item.as_str() {
            None => wrong_type(env, st, ty, key, item),
            Some(s) if !vs.contains(&s) => {
                let mut hint = format!("Use one of: {}.", vs.join(", "));
                if let Some(sug) = suggest(s, vs.iter().copied()) {
                    hint = format!("Did you mean `{sug}`? {hint}");
                }
                invalid(env, st, key, item, &format!("`{s}` is not allowed"), hint);
            }
            Some(_) => {}
        },
        Ty::VersionSpec => match item.as_str() {
            None => wrong_type(env, st, ty, key, item),
            Some(s) => {
                if crate::vspec::Spec::parse(s).is_err() {
                    st.diags.report(
                        false,
                        "version-spec-invalid",
                        &[("spec", s)],
                        env.src,
                        span_of(item),
                        None,
                    );
                }
            }
        },
        Ty::DotsReq => match item.as_str() {
            None => wrong_type(env, st, ty, key, item),
            Some(s) => {
                if crate::version::parse_requirement(s).is_none() {
                    invalid(
                        env,
                        st,
                        key,
                        item,
                        &format!("`{s}` is not a version requirement"),
                        "Use the form `>=X.Y` or `>=X.Y.Z`.".into(),
                    );
                }
            }
        },
        Ty::Path => match item.as_str() {
            None => wrong_type(env, st, ty, key, item),
            Some(s) => {
                if env.check_paths {
                    check_path(env, st, item, s, true);
                }
            }
        },
        Ty::Cmd => match item.as_str() {
            None => wrong_type(env, st, ty, key, item),
            Some(s) => {
                if env.check_paths {
                    let first = s.split_whitespace().next().unwrap_or("");
                    check_path(env, st, item, first, false);
                }
            }
        },
        Ty::List(inner) | Ty::ListEdit(inner) => {
            if let Some(items) = list_items(item) {
                for it in &items {
                    check(env, st, inner, key, it, plat);
                }
                return;
            }
            let edit_ok = plat || env.partial || matches!(ty, Ty::ListEdit(_));
            if edit_ok && let Some(t) = item.as_table_like() {
                let keyed = env.partial && matches!(inner, Ty::Obj(_) | Ty::Table(_));
                for (k, v) in t.iter() {
                    if k == "add" || k == "remove" || (k == "prepend" && env.partial) {
                        check(env, st, &Ty::List(inner), key, v, plat);
                    } else if keyed {
                        // `files = { "<dst>" = { ... } }`: an entry addressed by its key.
                        check(env, st, inner, k, v, plat);
                    } else {
                        unknown_key(env, st, t, k, &["add", "remove"]);
                    }
                }
                return;
            }
            wrong_type(env, st, ty, key, item);
        }
        Ty::Table(fields) | Ty::Obj(fields) => {
            let Some(t) = item.as_table_like() else {
                wrong_type(env, st, ty, key, item);
                return;
            };
            let allow_platform = matches!(ty, Ty::Obj(_)) && !plat;
            for (k, v) in t.iter() {
                if allow_platform && k == "platform" {
                    check_platform(env, st, fields, v);
                } else if let Some((_, fty)) = fields.iter().find(|(n, _)| *n == k) {
                    check(env, st, fty, k, v, plat);
                } else if env.partial && k == "remove" {
                    check(env, st, &Ty::Bool, k, v, plat);
                } else {
                    let mut names: Vec<&str> = fields.iter().map(|(n, _)| *n).collect();
                    if allow_platform {
                        names.push("platform");
                    }
                    unknown_key(env, st, t, k, &names);
                }
            }
        }
        Ty::Map(inner) => {
            let Some(t) = item.as_table_like() else {
                wrong_type(env, st, ty, key, item);
                return;
            };
            for (k, v) in t.iter() {
                check(env, st, inner, k, v, plat);
            }
        }
        Ty::Open => {
            if item.as_table_like().is_none() {
                wrong_type(env, st, ty, key, item);
            }
        }
        Ty::Providers => {
            let Some(t) = item.as_table_like() else {
                wrong_type(env, st, ty, key, item);
                return;
            };
            for (id, v) in t.iter() {
                let pty = (env.provider_ty)(id).unwrap_or(&Ty::Open);
                check(env, st, pty, id, v, plat);
            }
        }
        Ty::Alt(alts) => {
            let k = item_kind(item);
            let matching: Vec<&Ty> = alts
                .iter()
                .filter(|a| a.kind() == k || a.kind() == Kind::Any)
                .collect();
            if matching.is_empty() {
                wrong_type(env, st, ty, key, item);
                return;
            }
            // Accept as soon as one alternative is clean; otherwise report the first match's
            // errors (the shape the user most likely meant).
            let mut first_failure: Option<State> = None;
            for a in matching {
                let mut trial = State::default();
                check(env, &mut trial, a, key, item, plat);
                if trial.diags.errors.is_empty() {
                    st.diags.warnings.extend(trial.diags.warnings);
                    st.escape_hatch.extend(trial.escape_hatch);
                    return;
                }
                first_failure.get_or_insert(trial);
            }
            if let Some(f) = first_failure {
                st.diags.extend(f.diags);
                st.escape_hatch.extend(f.escape_hatch);
            }
        }
    }
}

/// `[platform."<key>"]`: each entry is a partial copy of the enclosing object.
fn check_platform(env: &Env, st: &mut State, fields: &'static [(&'static str, Ty)], v: &Item) {
    let Some(t) = v.as_table_like() else {
        wrong_type(env, st, &Ty::Open, "platform", v);
        return;
    };
    for (pk, pv) in t.iter() {
        let span = t
            .get_key_value(pk)
            .and_then(|(k, v)| k.span().or_else(|| v.span()));
        st.platform_keys.push((pk.to_string(), span));
        check(env, st, &Ty::Table(fields), pk, pv, true);
    }
}

fn unknown_key(
    env: &Env,
    st: &mut State,
    table: &dyn toml_edit::TableLike,
    key: &str,
    valid: &[&str],
) {
    let span = table
        .get_key_value(key)
        .and_then(|(k, v)| k.span().or_else(|| v.span()));
    let hint = match suggest(key, valid.iter().copied()) {
        Some(s) => format!("Did you mean `{s}`?"),
        None => {
            let mut v: Vec<&str> = valid.to_vec();
            v.sort_unstable();
            format!("Valid keys here: {}.", v.join(", "))
        }
    };
    st.diags.report(
        false,
        "schema-unknown-key",
        &[("key", key)],
        env.src,
        span,
        Some(hint),
    );
}

/// Does `p` stay inside the package? Lexical check: `..` may not climb above the start.
pub fn escapes_package(p: &str) -> bool {
    if p.starts_with('/') || p.starts_with('~') || p.starts_with('\\') {
        return true;
    }
    let b = p.as_bytes();
    if b.len() >= 2 && b[1] == b':' && b[0].is_ascii_alphabetic() {
        return true;
    }
    let mut depth = 0i32;
    for c in p.split(['/', '\\']) {
        match c {
            "" | "." => {}
            ".." => {
                depth -= 1;
                if depth < 0 {
                    return true;
                }
            }
            _ => depth += 1,
        }
    }
    false
}

fn check_path(env: &Env, st: &mut State, item: &Item, p: &str, absolute_is_escape: bool) {
    if p.starts_with("//") {
        st.escape_hatch.push(p.to_string());
        st.diags.report(
            true,
            "escape-hatch-path",
            &[("path", p)],
            env.src,
            span_of(item),
            None,
        );
        return;
    }
    let bad = if absolute_is_escape {
        escapes_package(p)
    } else {
        // Commands may legitimately name absolute tools; only climbing out is an error.
        !p.starts_with('/') && !p.starts_with('~') && escapes_package(p)
    };
    if bad {
        st.diags.error(
            "path-escapes-package",
            &[("path", p)],
            env.src,
            span_of(item),
        );
    }
}
