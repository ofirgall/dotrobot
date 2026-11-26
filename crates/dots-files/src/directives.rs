//! The directive preprocessor (ARCHITECTURE 17.3): `dots:if`/`elif`/`else`/`end`, `dots:if!`,
//! `dots:include` and `@dots(expr | filter)@`, line-based and run per layer before parsing.
//!
//! Directives sit in the file's own comment syntax. Escaping: `@@dots(` writes a literal
//! `@dots(` (the rest of the text is left as it is).

use std::path::Path;

use dots_core::report::{Diagnostic, Provenance};
use serde_json::Value as Json;

use crate::model::ResolvedFile;
use crate::scope::{Scope, UnknownName};

const MAX_DEPTH: usize = 8;

/// Does `text` hold anything the preprocessor would act on?
pub(crate) fn has_directives(text: &str) -> bool {
    text.contains("dots:") || text.contains("@dots(") || text.contains("@@dots(")
}

/// Accepted comment leaders and the closing token (markdown's `-->`), or `None` where the
/// format has no comments (strict JSON).
fn leaders(file: &ResolvedFile) -> Option<(Vec<String>, String)> {
    leaders_for(&file.format, &file.comment)
}

fn leaders_for(format: &str, comment: &str) -> Option<(Vec<String>, String)> {
    let own = comment.to_string();
    Some(match format {
        "json" => return None,
        "jsonc" => (vec!["//".into()], String::new()),
        "markdown" => (vec!["<!--".into()], "-->".into()),
        "toml" | "yaml" => (vec!["#".into()], String::new()),
        "ini" | "gitconfig" => (vec!["#".into(), ";".into()], String::new()),
        _ => (vec![own], String::new()),
    })
}

/// A layer that has `dots:` directives, none of them behind the effective comment leader.
/// Such a file renders silently wrong, so say how to fix it.
fn leader_problem(
    text: &str,
    label: &str,
    leaders: &[String],
    close: &str,
    comment: &str,
) -> Option<Diagnostic> {
    let directive_like = |rest: &str| {
        let rest = rest.trim_start();
        rest.strip_prefix("dots:").is_some_and(|b| {
            ["if", "if!", "elif", "else", "end", "include"]
                .iter()
                .any(|k| {
                    b.strip_prefix(k).is_some_and(|after| {
                        !after.starts_with(|c: char| c.is_ascii_alphanumeric() || c == '!')
                    })
                })
        })
    };
    let mut first = None;
    for (i, line) in text.lines().enumerate() {
        if let Some(rest) = strip_leader(line, leaders)
            && directive_like(rest)
        {
            return None;
        }
        if first.is_none() {
            let t = line.trim_start();
            // Up to three punctuation characters, then the directive: a foreign comment leader.
            let lead = t.find("dots:").filter(|&p| p > 0 && p <= 4);
            if let Some(p) = lead
                && t[..p]
                    .chars()
                    .all(|c| !c.is_ascii_alphanumeric() && c != '@')
                && directive_like(&t[p..])
            {
                first = Some(i + 1);
            }
        }
    }
    let _ = close;
    first.map(|n| {
        let mut d = err(
            label,
            n,
            &format!(
                "this file has `dots:` directives, but none starts with its comment leader `{comment}`"
            ),
        );
        d.hint = "Set `comment = \"...\"` on the file entry to the comment leader of this format."
            .to_string();
        d
    })
}

/// Lint one `[[files]]` entry's layer sources, without a machine: directives must use the
/// file's comment leader. Only plain `src` and `layers` paths are read.
pub fn lint_layers(pkg_dir: &Path, entry: &dots_model::FileEntry) -> Vec<Diagnostic> {
    use dots_model::{Layer, Layers, PlatformString};
    if !matches!(entry.mode.as_deref(), Some("render" | "fragment")) {
        return Vec::new();
    }
    let dst = match &entry.dst {
        Some(PlatformString::Plain(s)) => s.clone(),
        Some(PlatformString::PerPlatform(m)) => m.values().next().cloned().unwrap_or_default(),
        None => return Vec::new(),
    };
    let format = crate::resolve::detect_format(entry.format.as_deref(), Path::new(&dst));
    let comment = entry
        .comment
        .clone()
        .unwrap_or_else(|| crate::resolve::infer_comment(&format, Path::new(&dst)).to_string());
    let Some((leaders, close)) = leaders_for(&format, &comment) else {
        return Vec::new();
    };
    let mut paths = Vec::new();
    match &entry.layers {
        Some(Layers::List(l)) => {
            for item in l {
                match item {
                    Layer::Path(p) => paths.push(p.clone()),
                    Layer::Conditional { src, .. } => paths.push(src.clone()),
                }
            }
        }
        Some(Layers::Auto { .. }) => {}
        None => paths.extend(entry.src.clone()),
    }
    paths
        .iter()
        .filter_map(|p| {
            let full = pkg_dir.join(p);
            let text = std::fs::read_to_string(&full).ok()?;
            leader_problem(
                &text,
                &full.display().to_string(),
                &leaders,
                &close,
                &comment,
            )
        })
        .collect()
}

fn err(file: &str, line: usize, detail: &str) -> Diagnostic {
    Diagnostic::from_code(
        "directive-error",
        &[
            ("file", file),
            ("line", &line.to_string()),
            ("detail", detail),
        ],
    )
    .with_provenance(Provenance {
        source: "layer".into(),
        file: Some(file.to_string()),
        line: Some(line as u32),
        col: None,
    })
}

fn unknown(file: &str, line: usize, u: UnknownName) -> Diagnostic {
    let mut d = Diagnostic::from_code(
        "unknown-name",
        &[
            ("name", &u.name),
            ("file", file),
            ("line", &line.to_string()),
        ],
    )
    .with_provenance(Provenance {
        source: "layer".into(),
        file: Some(file.to_string()),
        line: Some(line as u32),
        col: None,
    });
    if let Some(s) = u.suggestion {
        d.hint = format!("Did you mean `{s}`?");
    }
    d
}

enum Dir<'a> {
    If { bang: bool, cond: &'a str },
    Elif(&'a str),
    Else,
    End,
    Include(&'a str),
}

/// The comment leader a line starts with (after indentation), and the rest after it.
fn strip_leader<'l>(line: &'l str, leaders: &[String]) -> Option<&'l str> {
    let t = line.trim_start();
    leaders
        .iter()
        .filter(|l| !l.is_empty())
        .find_map(|l| t.strip_prefix(l.as_str()))
}

fn parse_directive<'l>(
    line: &'l str,
    leaders: &[String],
    close: &str,
    file: &str,
    n: usize,
) -> Result<Option<Dir<'l>>, Diagnostic> {
    let Some(rest) = strip_leader(line, leaders) else {
        return Ok(None);
    };
    let rest = rest.trim_start();
    let Some(body) = rest.strip_prefix("dots:") else {
        return Ok(None);
    };
    let name_len = body
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '!'))
        .unwrap_or(body.len());
    // `dots: from ...` and `dots:/path` are ordinary comments, not directives.
    if name_len == 0 || !body[..name_len].starts_with(|c: char| c.is_ascii_alphabetic()) {
        return Ok(None);
    }
    let (name, args) = body.split_at(name_len);
    let mut args = args.trim();
    if !close.is_empty() {
        args = args.strip_suffix(close).unwrap_or(args).trim_end();
    }
    let need = |what: &str| -> Result<&str, Diagnostic> {
        if args.is_empty() {
            Err(err(file, n, &format!("`dots:{name}` needs {what}")))
        } else {
            Ok(args)
        }
    };
    let none = |d: Dir<'l>| -> Result<Option<Dir<'l>>, Diagnostic> {
        if args.is_empty() {
            Ok(Some(d))
        } else {
            Err(err(file, n, &format!("`dots:{name}` takes no arguments")))
        }
    };
    match name {
        "if" => Ok(Some(Dir::If {
            bang: false,
            cond: need("a condition")?,
        })),
        "if!" => Ok(Some(Dir::If {
            bang: true,
            cond: need("a condition")?,
        })),
        "elif" => Ok(Some(Dir::Elif(need("a condition")?))),
        "else" => none(Dir::Else),
        "end" => none(Dir::End),
        "include" => Ok(Some(Dir::Include(need("a relative path")?))),
        other => Err(err(file, n, &format!("unknown directive `dots:{other}`"))),
    }
}

struct Frame {
    line: usize,
    /// Is the enclosing region emitting?
    parent: bool,
    /// Has a branch of this chain been taken?
    taken: bool,
    /// Is the current branch emitting?
    active: bool,
    seen_else: bool,
    /// Is the current branch a `dots:if!` one (its body is commented in the source)?
    bang: bool,
    /// Is the enclosing body commented in the source (a `dots:if!` further out)? Everything
    /// nested in it is commented too, whatever branch it is in.
    inherited: bool,
}

/// Preprocess one layer. `label` is the path used in errors, `dir` resolves `dots:include`.
pub(crate) fn process(
    text: &str,
    label: &str,
    dir: &Path,
    file: &ResolvedFile,
    scope: &Scope,
) -> Result<String, Diagnostic> {
    run(text, label, dir, file, scope, 0)
}

fn run(
    text: &str,
    label: &str,
    dir: &Path,
    file: &ResolvedFile,
    scope: &Scope,
    depth: usize,
) -> Result<String, Diagnostic> {
    let Some((leaders, close)) = leaders(file) else {
        // Strict JSON has no comments, so nothing here can be a directive.
        for (i, l) in text.lines().enumerate() {
            if l.contains("@dots(") || l.trim_start().starts_with("//") && l.contains("dots:") {
                return Err(err(
                    label,
                    i + 1,
                    "directives are unavailable in strict JSON; use format `jsonc` or `[[files.set]]`",
                ));
            }
        }
        return Ok(text.to_string());
    };
    if let Some(d) = leader_problem(text, label, &leaders, &close, &file.comment) {
        return Err(d);
    }
    let mut out = String::new();
    let eol = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let mut stack: Vec<Frame> = Vec::new();
    for (i, line) in text.lines().enumerate() {
        let n = i + 1;
        let active = stack.last().is_none_or(|f| f.active);
        match parse_directive(line, &leaders, &close, label, n)? {
            Some(Dir::If { bang, cond }) => {
                if stack.len() >= MAX_DEPTH {
                    return Err(err(label, n, "directives nest more than 8 levels deep"));
                }
                let on = active && cond_true(scope, cond, label, n)?;
                let inherited = stack.last().is_some_and(|f| f.inherited || f.bang);
                stack.push(Frame {
                    line: n,
                    parent: active,
                    taken: on,
                    active: on,
                    seen_else: false,
                    bang,
                    inherited,
                });
            }
            Some(Dir::Elif(cond)) => {
                let Some(f) = stack.last_mut() else {
                    return Err(err(label, n, "`dots:elif` without `dots:if`"));
                };
                if f.seen_else {
                    return Err(err(label, n, "`dots:elif` after `dots:else`"));
                }
                f.bang = false;
                f.active = false;
                if f.parent && !f.taken {
                    let on = cond_true(scope, cond, label, n)?;
                    f.active = on;
                    f.taken = on;
                }
            }
            Some(Dir::Else) => {
                let Some(f) = stack.last_mut() else {
                    return Err(err(label, n, "`dots:else` without `dots:if`"));
                };
                if f.seen_else {
                    return Err(err(label, n, "second `dots:else` in one `dots:if`"));
                }
                f.seen_else = true;
                f.bang = false;
                f.active = f.parent && !f.taken;
                f.taken = true;
            }
            Some(Dir::End) => {
                if stack.pop().is_none() {
                    return Err(err(label, n, "`dots:end` without `dots:if`"));
                }
            }
            Some(Dir::Include(rel)) => {
                if active {
                    out.push_str(&include(rel, label, dir, file, scope, depth, n)?);
                }
            }
            None => {
                let bang = stack.last().is_some_and(|f| f.inherited || f.bang);
                let body = if bang {
                    uncomment(line, &leaders, &close, label, n)?
                } else {
                    line.to_string()
                };
                if active {
                    out.push_str(&inline(&body, label, n, scope)?);
                    out.push_str(eol);
                }
            }
        }
    }
    if let Some(f) = stack.last() {
        return Err(err(
            label,
            f.line,
            "`dots:if` is never closed by `dots:end`",
        ));
    }
    if !text.ends_with('\n') && out.ends_with(eol) {
        out.truncate(out.len() - eol.len());
    }
    Ok(out)
}

fn cond_true(scope: &Scope, cond: &str, file: &str, n: usize) -> Result<bool, Diagnostic> {
    scope
        .eval_cond(cond)
        .map_err(|e| e.to_diagnostic(Some(file), Some(n as u32)))
}

/// A `dots:if!` body line is commented in the source: leader, then one space.
fn uncomment(
    line: &str,
    leaders: &[String],
    close: &str,
    file: &str,
    n: usize,
) -> Result<String, Diagnostic> {
    if line.trim().is_empty() {
        return Ok(line.to_string());
    }
    let Some(rest) = strip_leader(line, leaders) else {
        return Err(err(
            file,
            n,
            &format!(
                "this line is in a `dots:if!` body, so it must start with `{}`",
                leaders[0]
            ),
        ));
    };
    let indent = &line[..line.len() - line.trim_start().len()];
    let mut body = rest.strip_prefix(' ').unwrap_or(rest);
    if !close.is_empty() {
        body = body.trim_end();
        body = body.strip_suffix(close).unwrap_or(body);
        body = body.strip_suffix(' ').unwrap_or(body);
    }
    Ok(format!("{indent}{body}"))
}

fn include(
    rel: &str,
    label: &str,
    dir: &Path,
    file: &ResolvedFile,
    scope: &Scope,
    depth: usize,
    n: usize,
) -> Result<String, Diagnostic> {
    let p = Path::new(rel);
    if p.is_absolute()
        || p.components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return Err(err(
            label,
            n,
            &format!("`dots:include {rel}` must be a relative path below the layer's directory"),
        ));
    }
    if depth + 1 > MAX_DEPTH {
        return Err(err(
            label,
            n,
            "`dots:include` nests more than 8 levels deep",
        ));
    }
    let path = dir.join(p);
    let shown = path.display().to_string();
    let text = std::fs::read_to_string(&path)
        .map_err(|e| err(label, n, &format!("cannot include {shown}: {e}")))?;
    let sub = path.parent().unwrap_or(dir);
    let mut s = run(&text, &shown, sub, file, scope, depth + 1)?;
    if !s.is_empty() && !s.ends_with('\n') {
        s.push('\n');
    }
    Ok(s)
}

/// Replace every `@dots(expr | filter ...)@` in some text (used by `[[files.set]]` values).
pub(crate) fn inline_text(
    text: &str,
    file: &str,
    n: usize,
    scope: &Scope,
) -> Result<String, Diagnostic> {
    inline(text, file, n, scope)
}

/// Replace every `@dots(expr | filter ...)@` in one line.
fn inline(line: &str, file: &str, n: usize, scope: &Scope) -> Result<String, Diagnostic> {
    const OPEN: &str = "@dots(";
    if !line.contains(OPEN) {
        return Ok(line.to_string());
    }
    let mut out = String::new();
    let mut rest = line;
    while let Some(at) = rest.find(OPEN) {
        if at > 0 && rest[..at].ends_with('@') {
            // `@@dots(` is an escaped literal.
            out.push_str(&rest[..at - 1]);
            out.push_str(OPEN);
            rest = &rest[at + OPEN.len()..];
            continue;
        }
        out.push_str(&rest[..at]);
        let after = &rest[at + OPEN.len()..];
        let Some(end) = after.find(")@") else {
            return Err(err(file, n, "`@dots(` is never closed by `)@`"));
        };
        out.push_str(&substitute(&after[..end], file, n, scope)?);
        rest = &after[end + 2..];
    }
    out.push_str(rest);
    Ok(out)
}

/// The value of one `expr | filter | ...`, as text.
fn substitute(expr: &str, file: &str, n: usize, scope: &Scope) -> Result<String, Diagnostic> {
    let (v, filters) = evaluate(expr, file, n, scope)?;
    render_with(&v, &filters, file, n)
}

/// Split `expr | filter | ...` and resolve the expression.
pub(crate) fn evaluate<'e>(
    expr: &'e str,
    file: &str,
    n: usize,
    scope: &Scope,
) -> Result<(Json, Vec<&'e str>), Diagnostic> {
    let mut parts = expr.split('|').map(str::trim);
    let name = parts.next().unwrap_or("");
    if name.is_empty() {
        return Err(err(file, n, "empty `@dots()@` expression"));
    }
    let v = scope.value(name).map_err(|u| unknown(file, n, u))?;
    Ok((v, parts.collect()))
}

pub(crate) fn plain(v: &Json) -> String {
    match v {
        Json::String(s) => s.clone(),
        Json::Null => String::new(),
        other => other.to_string(),
    }
}

pub(crate) fn render_with(
    v: &Json,
    filters: &[&str],
    file: &str,
    n: usize,
) -> Result<String, Diagnostic> {
    // The first formatting filter sees the typed value; later ones see the text before them.
    let mut val = v.clone();
    for f in filters {
        let text = match *f {
            "json" => serde_json::to_string(&val).unwrap_or_default(),
            "toml" => toml_value(&val),
            "yaml" => yaml_value(&val),
            "shell" => shell_value(&val),
            "upper" => plain(&val).to_uppercase(),
            "lower" => plain(&val).to_lowercase(),
            other => {
                return Err(err(
                    file,
                    n,
                    &format!(
                        "unknown filter `{other}`; use json, toml, yaml, shell, upper or lower"
                    ),
                ));
            }
        };
        val = Json::String(text);
    }
    Ok(plain(&val))
}

fn toml_value(v: &Json) -> String {
    match v {
        Json::String(s) => toml_edit::Value::from(s.as_str())
            .to_string()
            .trim()
            .to_string(),
        Json::Null => "\"\"".into(),
        Json::Array(a) => format!(
            "[{}]",
            a.iter().map(toml_value).collect::<Vec<_>>().join(", ")
        ),
        Json::Object(o) => format!(
            "{{ {} }}",
            o.iter()
                .map(|(k, v)| format!(
                    "{} = {}",
                    toml_value(&Json::String(k.clone())),
                    toml_value(v)
                ))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        other => other.to_string(),
    }
}

fn yaml_value(v: &Json) -> String {
    match v {
        Json::String(_) | Json::Null => serde_yaml_ng::to_string(v)
            .map(|s| s.trim_end().to_string())
            .unwrap_or_default(),
        // Compact JSON is valid flow-style YAML.
        other => other.to_string(),
    }
}

fn shell_quote(s: &str) -> String {
    let safe = !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || "_@%+=:,./-".contains(c));
    if safe {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
}

fn shell_value(v: &Json) -> String {
    match v {
        Json::Array(a) => a
            .iter()
            .map(|x| shell_quote(&plain(x)))
            .collect::<Vec<_>>()
            .join(" "),
        Json::Object(_) => shell_quote(&v.to_string()),
        other => shell_quote(&plain(other)),
    }
}
