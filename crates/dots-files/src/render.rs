//! The render pipeline: load layers, template, parse, merge, serialize, header.

use dots_core::report::{Diagnostic, Provenance};

use crate::directives;
use crate::merge::{DELETE_MARKER, Mismatch, Node, Strategies, merge_value, strip_deletes};
use crate::model::{FilesCtx, ResolvedFile, SettingUse};
use crate::scope::Scope;
use crate::set;
use crate::structured::{
    ParseErr, finish_toml, merge_toml, parse_ini, parse_json, parse_toml, parse_yaml, write_ini,
    write_json, write_yaml,
};

pub(crate) struct Loaded {
    pub label: String,
    pub path: String,
    pub text: String,
}

fn prov(file: &str, line: Option<u32>) -> Provenance {
    Provenance {
        source: "layer".into(),
        file: Some(file.to_string()),
        line,
        col: None,
    }
}

pub(crate) fn parse_diag(l: &Loaded, format: &str, (line, detail): ParseErr) -> Diagnostic {
    Diagnostic::from_code(
        "layer-parse-error",
        &[
            ("file", &l.path),
            ("line", &line.to_string()),
            ("format", format),
            ("detail", &detail),
        ],
    )
    .with_provenance(prov(&l.path, Some(line)))
}

pub(crate) fn mismatch_diag(m: Mismatch, left: &Loaded, right: &Loaded) -> Diagnostic {
    let (l, r) = (
        format!("{} ({})", left.path, left.label),
        format!("{} ({})", right.path, right.label),
    );
    Diagnostic::from_code(
        "merge-type-mismatch",
        &[
            ("path", &m.path),
            ("left_type", m.left),
            ("right_type", m.right),
            ("left", &l),
            ("right", &r),
        ],
    )
    .with_provenance(prov(&left.path, None))
    .with_provenance(prov(&right.path, None))
}

fn load(file: &ResolvedFile, ctx: &FilesCtx, scope: &Scope) -> Result<Vec<Loaded>, Diagnostic> {
    let mut out = Vec::new();
    for l in &file.layers {
        let path = l.path.display().to_string();
        if l.path.extension().is_some_and(|e| e == "patch") {
            return Err(Diagnostic::from_code(
                "layer-parse-error",
                &[
                    ("file", &path),
                    ("line", "1"),
                    ("format", "layer"),
                    ("detail", "patch layers are not supported yet"),
                ],
            ));
        }
        let mut text = std::fs::read_to_string(&l.path).map_err(|e| {
            Diagnostic::from_code(
                "file-unreadable",
                &[("file", &path), ("detail", &e.to_string())],
            )
            .with_provenance(prov(&path, None))
        })?;
        if directives::has_directives(&text) {
            let dir = l.path.parent().unwrap_or(std::path::Path::new("."));
            text = directives::process(&text, &path, dir, file, scope)?;
        }
        if file.template.as_deref() == Some("jinja") {
            text = jinja(&text, &path, ctx)?;
        }
        out.push(Loaded {
            label: l.label.clone(),
            path,
            text,
        });
    }
    Ok(out)
}

fn jinja(text: &str, path: &str, ctx: &FilesCtx) -> Result<String, Diagnostic> {
    let mut env = minijinja::Environment::new();
    env.set_undefined_behavior(minijinja::UndefinedBehavior::Strict);
    env.render_str(
        text,
        minijinja::Value::from(minijinja::value::Serde(&ctx.template_vars)),
    )
    .map_err(|e| {
        let line = e.line().unwrap_or(1).to_string();
        let name = e
            .detail()
            .map(str::to_string)
            .unwrap_or_else(|| e.to_string());
        if matches!(e.kind(), minijinja::ErrorKind::UndefinedError) {
            Diagnostic::from_code(
                "unknown-name",
                &[("name", &name), ("file", path), ("line", &line)],
            )
        } else {
            Diagnostic::from_code(
                "layer-parse-error",
                &[
                    ("file", path),
                    ("line", &line),
                    ("format", "jinja"),
                    ("detail", &name),
                ],
            )
        }
        .with_provenance(prov(path, line.parse().ok()))
    })
}

pub(crate) fn node_fmt(format: &str, t: &str) -> Result<Node, ParseErr> {
    match format {
        "json" => parse_json(t, false),
        "jsonc" => parse_json(t, true),
        "yaml" => parse_yaml(t),
        _ => parse_ini(t),
    }
}

pub(crate) fn comment_prefix(file: &ResolvedFile) -> Option<(String, String)> {
    match file.format.as_str() {
        "json" => None,
        "jsonc" => Some(("//".into(), String::new())),
        "markdown" => Some(("<!--".into(), " -->".into())),
        "toml" | "yaml" | "ini" | "gitconfig" => Some(("#".into(), String::new())),
        _ => Some((file.comment.clone(), String::new())),
    }
}

fn fmt_setting(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// The generated header, empty for formats without comments (JSON).
pub fn header(file: &ResolvedFile, used: &[SettingUse]) -> String {
    let Some((open, close)) = comment_prefix(file) else {
        return String::new();
    };
    let mut lines = if file.extras.parts.is_empty() {
        vec![
            format!(
                "Generated by dots from package `{}`. Do not edit; run `dots adopt {}` to keep changes.",
                file.package,
                file.dst.display()
            ),
            format!(
                "layers: {}",
                file.layers
                    .iter()
                    .map(|l| l.label.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        ]
    } else {
        vec![
            format!(
                "Generated by dots from fragments. Do not edit; run `dots adopt {}` to keep changes.",
                file.dst.display()
            ),
            format!(
                "fragments: {}",
                file.extras
                    .parts
                    .iter()
                    .map(|p| format!(
                        "{} (priority {}, {})",
                        p.package,
                        p.extras.fragment.unwrap_or(0),
                        p.layers
                            .iter()
                            .map(|l| l.label.as_str())
                            .collect::<Vec<_>>()
                            .join(" + ")
                    ))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        ]
    };
    if !used.is_empty() {
        let s: Vec<String> = used
            .iter()
            .map(|s| {
                let shown = if s.secret {
                    "•••".to_string()
                } else {
                    fmt_setting(&s.value)
                };
                format!("{}={}({})", s.name, shown, s.scope)
            })
            .collect();
        lines.push(format!("settings: {}", s.join(", ")));
    }
    lines
        .iter()
        .map(|l| format!("{open} {l}{close}\n"))
        .collect()
}

/// Native include stub for an application, one line per layer.
fn include_stub(file: &ResolvedFile, ctx: &FilesCtx) -> Result<String, Diagnostic> {
    let dst = file.dst.display().to_string();
    let name = file
        .dst
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_string();
    let line: fn(&str) -> String = if dst.contains("ghostty") {
        |p| format!("config-file = {p}\n")
    } else if file.format == "gitconfig" || name.ends_with("gitconfig") {
        |p| format!("[include]\n\tpath = {p}\n")
    } else if name.ends_with("tmux.conf") {
        |p| format!("source-file {p}\n")
    } else if name.starts_with(".zsh")
        || name.starts_with(".bash")
        || name.ends_with(".zsh")
        || name.ends_with(".sh")
    {
        |p| format!("source {p}\n")
    } else {
        return Err(Diagnostic::from_code(
            "include-unsupported",
            &[("dst", &dst)],
        ));
    };
    let mut out = String::new();
    for l in &file.layers {
        let p = match l.path.strip_prefix(&ctx.root) {
            Ok(rel) => ctx.active_dir().join(rel),
            Err(_) => l.path.clone(),
        };
        out.push_str(&line(&p.display().to_string()));
    }
    Ok(out)
}

fn split_frontmatter(t: &str) -> (Option<&str>, &str) {
    let Some(rest) = t.strip_prefix("---\n") else {
        return (None, t);
    };
    let mut pos = 0;
    for line in rest.split_inclusive('\n') {
        if line.trim_end() == "---" {
            return (Some(&rest[..pos]), &rest[pos + line.len()..]);
        }
        pos += line.len();
    }
    (None, t)
}

/// Combine a file's layers into its final text, header included.
pub fn render_layers(file: &ResolvedFile, ctx: &FilesCtx) -> Result<String, Diagnostic> {
    if !file.extras.parts.is_empty() {
        return crate::fragments::render_composed(file, ctx);
    }
    if file.kind.as_deref() == Some("include") {
        let stub = include_stub(file, ctx)?;
        return Ok(format!(
            "{}{stub}",
            comment_prefix(file)
                .map(|_| header(file, &[]))
                .unwrap_or_default()
        ));
    }
    let scope = Scope::new(ctx, &file.package);
    let body = render_body(file, ctx, &scope)?;
    let head = header(file, &scope.used());
    if file.format == "markdown" && !body.is_empty() {
        return Ok(with_md_header(&body, &head));
    }
    Ok(with_head(&body, &head))
}

/// Put the generated header on top of `body`, but after a `#!` line: the kernel only honours
/// a shebang on the very first line.
pub(crate) fn with_head(body: &str, head: &str) -> String {
    // A CRLF file gets a CRLF header.
    let crlf;
    let head = if body.contains("\r\n") && !head.contains("\r\n") {
        crlf = head.replace('\n', "\r\n");
        crlf.as_str()
    } else {
        head
    };
    if head.is_empty() || !body.starts_with("#!") {
        return format!("{head}{body}");
    }
    let end = body.find('\n').map_or(body.len(), |i| i + 1);
    let (first, rest) = body.split_at(end);
    let nl = if first.is_empty() || first.ends_with('\n') {
        ""
    } else {
        "\n"
    };
    format!("{first}{nl}{head}{rest}")
}

/// The merged, patched and serialized layers of one file, without the generated header.
pub(crate) fn render_body(
    file: &ResolvedFile,
    ctx: &FilesCtx,
    scope: &Scope,
) -> Result<String, Diagnostic> {
    let layers = load(file, ctx, scope)?;
    if layers.is_empty() {
        return Ok(String::new());
    }
    let strat: Strategies = file.merge.clone();
    let patches = if matches!(file.format.as_str(), "text") {
        let p = set::active(file, scope)?;
        if !p.is_empty() {
            return Err(Diagnostic::from_code(
                "schema-invalid-value",
                &[
                    ("key", "files.set"),
                    ("file", &file.dst.display().to_string()),
                    ("line", "1"),
                    (
                        "detail",
                        "`[[files.set]]` needs a structured format (toml, json, jsonc, yaml, ini, gitconfig or markdown)",
                    ),
                ],
            ));
        }
        p
    } else {
        set::active(file, scope)?
    };
    let body = match file.format.as_str() {
        "toml" => {
            let mut docs = Vec::new();
            for l in &layers {
                docs.push(parse_toml(&l.text).map_err(|e| parse_diag(l, "TOML", e))?);
            }
            let mut it = docs.into_iter().enumerate();
            let (_, mut base) = it.next().expect("non-empty");
            for (i, d) in it {
                merge_toml(base.as_table_mut(), d.as_table(), "", &strat)
                    .map_err(|m| mismatch_diag(m, &layers[0], &layers[i]))?;
            }
            finish_toml(&mut base);
            set::apply_toml(&mut base, &patches, file)?;
            base.to_string()
        }
        "json" | "jsonc" | "yaml" | "ini" | "gitconfig" => {
            let fmt = file.format.as_str();
            let mut nodes = Vec::new();
            for l in &layers {
                nodes.push(node_fmt(fmt, &l.text).map_err(|e| parse_diag(l, fmt, e))?);
            }
            let single_verbatim = layers.len() == 1
                && patches.is_empty()
                && !layers[0].text.contains(DELETE_MARKER)
                && matches!(fmt, "jsonc" | "yaml" | "json" | "ini" | "gitconfig");
            if single_verbatim {
                ensure_nl(layers[0].text.clone())
            } else {
                let mut it = nodes.into_iter().enumerate();
                let (_, mut base) = it.next().expect("non-empty");
                for (i, n) in it {
                    merge_value(&mut base, n, "", &strat)
                        .map_err(|m| mismatch_diag(m, &layers[0], &layers[i]))?;
                }
                let mut base = strip_deletes(base);
                set::apply_node(&mut base, &patches, file)?;
                match fmt {
                    "json" | "jsonc" => write_json(&base),
                    "yaml" => write_yaml(&base),
                    "gitconfig" => write_ini(&base, "\t"),
                    _ => write_ini(&base, ""),
                }
            }
        }
        "markdown" => markdown(&layers, &strat, file, &patches)?,
        _ => text_concat(&layers, file),
    };
    Ok(body)
}

fn ensure_nl(mut s: String) -> String {
    if !s.ends_with('\n') {
        s.push('\n');
    }
    s
}

fn text_concat(layers: &[Loaded], file: &ResolvedFile) -> String {
    let mut out = String::new();
    for l in layers {
        if layers.len() > 1 {
            out.push_str(&format!(
                "{} --- dots: from {} ---\n",
                file.comment, l.label
            ));
        }
        out.push_str(&ensure_nl(l.text.clone()));
    }
    out
}

fn markdown(
    layers: &[Loaded],
    strat: &Strategies,
    file: &ResolvedFile,
    patches: &[set::Active],
) -> Result<String, Diagnostic> {
    let parts: Vec<(Option<&str>, &str)> =
        layers.iter().map(|l| split_frontmatter(&l.text)).collect();
    let mut fm: Option<Node> = None;
    let mut fm_idx = 0;
    for (i, (f, _)) in parts.iter().enumerate() {
        if let Some(f) = f {
            let n = parse_yaml(f).map_err(|e| parse_diag(&layers[i], "YAML frontmatter", e))?;
            match fm.as_mut() {
                None => {
                    fm = Some(n);
                    fm_idx = i;
                }
                Some(b) => merge_value(b, n, "", strat)
                    .map_err(|m| mismatch_diag(m, &layers[fm_idx], &layers[i]))?,
            }
        }
    }
    let mut out = String::new();
    let fm_count = parts.iter().filter(|(f, _)| f.is_some()).count();
    if !patches.is_empty() {
        // Patches address the frontmatter as `frontmatter.<key>`.
        let mut root = Node::Table(indexmap::IndexMap::from([(
            "frontmatter".to_string(),
            strip_deletes(fm.take().unwrap_or(Node::Table(Default::default()))),
        )]));
        set::apply_node(&mut root, patches, file)?;
        if let Node::Table(mut t) = root {
            fm = t.shift_remove("frontmatter");
        }
        if matches!(&fm, Some(Node::Table(t)) if t.is_empty()) {
            fm = None;
        }
    }
    if let Some(n) = fm {
        let verbatim = patches.is_empty()
            && fm_count == 1
            && parts
                .iter()
                .find_map(|(f, _)| *f)
                .is_some_and(|f| !f.contains(DELETE_MARKER));
        let y = if verbatim {
            ensure_nl(parts.iter().find_map(|(f, _)| *f).unwrap_or("").to_string())
        } else {
            write_yaml(&strip_deletes(n))
        };
        out.push_str(&format!("---\n{y}---\n"));
    }
    let bodies: Vec<(&Loaded, &str)> = layers.iter().zip(parts.iter().map(|p| p.1)).collect();
    let multi = bodies.iter().filter(|(_, b)| !b.trim().is_empty()).count() > 1;
    for (l, b) in bodies {
        if b.trim().is_empty() {
            continue;
        }
        if multi {
            out.push_str(&format!("<!-- dots: from {} -->\n", l.label));
        }
        out.push_str(&ensure_nl(b.to_string()));
    }
    Ok(out)
}

/// The header goes after the frontmatter, which must stay first in the file.
fn with_md_header(body: &str, head: &str) -> String {
    match split_frontmatter(body) {
        (Some(f), rest) => format!("---\n{f}---\n{head}{rest}"),
        _ => with_head(body, head),
    }
}
