//! `[[files.set]]`: key-path assignments applied after the layers merge (ARCHITECTURE 17.4).

use dots_core::report::{Diagnostic, Provenance};
use serde_json::Value as Json;
use toml_edit::DocumentMut;

use crate::directives;
use crate::merge::Node;
use crate::model::ResolvedFile;
use crate::scope::Scope;
use crate::structured::{json_to_node, patch_toml};

/// A patch whose `when` held, with its value's `@dots()@` references resolved.
pub(crate) struct Active {
    pub path: Vec<String>,
    /// `None` deletes the key.
    pub value: Option<Node>,
}

fn bad(file: &ResolvedFile, detail: String) -> Diagnostic {
    Diagnostic::from_code(
        "schema-invalid-value",
        &[
            ("key", "files.set"),
            ("file", &file.dst.display().to_string()),
            ("line", "1"),
            ("detail", &detail),
        ],
    )
}

/// `a.b."c.d"` into segments. A quoted segment may hold dots and `\"`.
fn split_path(p: &str) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    let mut had_quote = false;
    let mut chars = p.chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => {
                quoted = !quoted;
                had_quote = true;
            }
            '\\' if quoted => match chars.next() {
                Some(n) => cur.push(n),
                None => return Err("a trailing backslash".into()),
            },
            '.' if !quoted => {
                if cur.is_empty() && !had_quote {
                    return Err("an empty segment".into());
                }
                out.push(std::mem::take(&mut cur));
                had_quote = false;
            }
            c => cur.push(c),
        }
    }
    if quoted {
        return Err("an unterminated quote".into());
    }
    if cur.is_empty() && !had_quote {
        return Err("an empty segment".into());
    }
    out.push(cur);
    Ok(out)
}

/// A value with every `@dots(..)@` replaced. A string that is exactly one expression keeps the
/// setting's own type.
fn resolve_value(v: &Json, file: &str, scope: &Scope) -> Result<Json, Diagnostic> {
    match v {
        Json::String(s) => {
            let t = s.trim();
            if let Some(expr) = t
                .strip_prefix("@dots(")
                .and_then(|r| r.strip_suffix(")@"))
                .filter(|e| !e.contains(")@") && !e.contains("@dots("))
            {
                let (val, filters) = directives::evaluate(expr, file, 1, scope)?;
                if filters.is_empty() {
                    return Ok(val);
                }
                return directives::render_with(&val, &filters, file, 1).map(Json::String);
            }
            directives::inline_text(s, file, 1, scope).map(Json::String)
        }
        Json::Array(a) => a
            .iter()
            .map(|x| resolve_value(x, file, scope))
            .collect::<Result<_, _>>()
            .map(Json::Array),
        Json::Object(o) => o
            .iter()
            .map(|(k, x)| Ok((k.clone(), resolve_value(x, file, scope)?)))
            .collect::<Result<_, Diagnostic>>()
            .map(Json::Object),
        other => Ok(other.clone()),
    }
}

/// The patches of `file` that apply here, in declaration order.
pub(crate) fn active(file: &ResolvedFile, scope: &Scope) -> Result<Vec<Active>, Diagnostic> {
    let label = file.dst.display().to_string();
    let mut out = Vec::new();
    for (i, p) in file.extras.set.iter().enumerate() {
        if let Some(c) = &p.when
            && !scope
                .eval_cond(c)
                .map_err(|e| e.to_diagnostic(Some(&label), None))?
        {
            continue;
        }
        let path = split_path(&p.path)
            .map_err(|e| bad(file, format!("files.set.{i}.path `{}` has {e}", p.path)))?;
        let value = match (&p.value, p.delete) {
            (_, true) => None,
            (Some(v), false) => Some(json_to_node(&resolve_value(v, &label, scope)?)),
            (None, false) => {
                return Err(bad(
                    file,
                    format!(
                        "files.set.{i} (`{}`) needs `value` or `delete = true`",
                        p.path
                    ),
                ));
            }
        };
        out.push(Active { path, value });
    }
    Ok(out)
}

fn not_table(file: &ResolvedFile, path: &[String], at: String, found: &str) -> Diagnostic {
    let full = path.join(".");
    let dst = file.dst.display().to_string();
    Diagnostic::from_code(
        "set-path-not-table",
        &[
            ("path", &full),
            ("at", &at),
            ("found", found),
            ("file", &dst),
        ],
    )
    .with_provenance(Provenance {
        source: "files.set".into(),
        file: Some(dst),
        line: None,
        col: None,
    })
}

/// Apply patches to a value tree.
pub(crate) fn apply_node(
    root: &mut Node,
    patches: &[Active],
    file: &ResolvedFile,
) -> Result<(), Diagnostic> {
    for p in patches {
        set_in(root, &p.path, 0, p, file)?;
    }
    Ok(())
}

fn set_in(
    node: &mut Node,
    segs: &[String],
    walked: usize,
    p: &Active,
    file: &ResolvedFile,
) -> Result<(), Diagnostic> {
    let Some((seg, rest)) = segs.split_first() else {
        return Ok(());
    };
    let found = node.type_name();
    let Node::Table(t) = node else {
        let at = if walked == 0 {
            "(root)".to_string()
        } else {
            p.path[..walked].join(".")
        };
        return Err(not_table(file, &p.path, at, found));
    };
    if rest.is_empty() {
        match &p.value {
            Some(v) => {
                t.insert(seg.clone(), v.clone());
            }
            None => {
                t.shift_remove(seg);
            }
        }
        return Ok(());
    }
    if !t.contains_key(seg) {
        if p.value.is_none() {
            return Ok(());
        }
        t.insert(seg.clone(), Node::Table(Default::default()));
    }
    let child = t.get_mut(seg).expect("present");
    set_in(child, rest, walked + 1, p, file)
}

/// Apply patches to a TOML document in place, keeping its comments.
pub(crate) fn apply_toml(
    doc: &mut DocumentMut,
    patches: &[Active],
    file: &ResolvedFile,
) -> Result<(), Diagnostic> {
    for p in patches {
        patch_toml(doc, &p.path, p.value.as_ref())
            .map_err(|(at, found)| not_table(file, &p.path, at, found))?;
    }
    Ok(())
}
