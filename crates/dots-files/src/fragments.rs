//! Fragments: several packages contribute to one `dst` (ARCHITECTURE 6.2).
//!
//! Each package's `mode = "fragment"` entry resolves like a `render` entry that carries a
//! priority. [`compose_fragments`] folds all entries for one `dst` into a single owned file
//! (package [`FRAGMENT_OWNER`]), which renders every contribution and merges them from the
//! lowest priority to the highest. A contributor that leaves the machine leaves the next
//! render, so only its part disappears.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use dots_core::report::{Diagnostic, Provenance};

use crate::merge::{Node, Strategies, merge_value, strip_deletes};
use crate::model::{FilesCtx, Mode, ResolvedFile, SettingUse};
use crate::render::{Loaded, header, mismatch_diag, node_fmt, parse_diag, render_body, with_head};
use crate::scope::Scope;
use crate::structured::{
    finish_toml, merge_toml, parse_toml, toml_to_node, write_ini, write_json, write_yaml,
};

/// The owner recorded in the ledger for a composed file. It is not a valid package name, so it
/// cannot collide with one, and it stays the same while contributors come and go.
pub const FRAGMENT_OWNER: &str = "@fragments";

/// Group the fragment entries of `files` by destination. Everything else passes through, and a
/// composed file takes the place of the first fragment that fed it.
pub fn compose_fragments(files: Vec<ResolvedFile>) -> Vec<ResolvedFile> {
    enum Slot {
        One(Box<ResolvedFile>),
        Group(PathBuf),
    }
    let mut slots: Vec<Slot> = Vec::new();
    let mut groups: BTreeMap<PathBuf, Vec<ResolvedFile>> = BTreeMap::new();
    for f in files {
        if f.extras.fragment.is_some() {
            let g = groups.entry(f.dst.clone()).or_default();
            if g.is_empty() {
                slots.push(Slot::Group(f.dst.clone()));
            }
            g.push(f);
        } else {
            slots.push(Slot::One(Box::new(f)));
        }
    }
    slots
        .into_iter()
        .map(|s| match s {
            Slot::One(f) => *f,
            Slot::Group(dst) => compose(dst.clone(), groups.remove(&dst).unwrap_or_default()),
        })
        .collect()
}

fn compose(dst: PathBuf, mut parts: Vec<ResolvedFile>) -> ResolvedFile {
    parts.sort_by(|a, b| (a.extras.fragment, &a.package).cmp(&(b.extras.fragment, &b.package)));
    let first = &parts[0];
    let mut merge = BTreeMap::new();
    for p in &parts {
        merge.extend(p.merge.clone());
    }
    ResolvedFile {
        package: FRAGMENT_OWNER.into(),
        mode: Mode::Render,
        dst,
        src: None,
        layers: parts.iter().flat_map(|p| p.layers.clone()).collect(),
        format: first.format.clone(),
        kind: None,
        template: None,
        merge,
        comment: first.comment.clone(),
        extras: crate::model::Extras {
            set: vec![],
            fragment: None,
            parts,
        },
    }
}

/// One fragment's text, for the lint that runs without a render.
#[derive(Debug, Clone)]
pub struct FragmentSource {
    pub package: String,
    pub priority: i64,
    pub format: String,
    /// Shown in diagnostics.
    pub path: String,
    pub text: String,
}

fn prov(source: &str, file: &str) -> Provenance {
    Provenance {
        source: source.to_string(),
        file: Some(file.to_string()),
        line: None,
        col: None,
    }
}

fn leaves(n: &Node, prefix: &str, out: &mut BTreeSet<String>) {
    match n {
        Node::Table(t) => {
            for (k, v) in t {
                let p = if prefix.is_empty() {
                    k.clone()
                } else {
                    format!("{prefix}.{k}")
                };
                leaves(v, &p, out);
            }
        }
        _ if prefix.is_empty() => {}
        _ => {
            out.insert(prefix.to_string());
        }
    }
}

struct Part<'a> {
    package: &'a str,
    priority: i64,
    path: &'a str,
    node: Node,
}

/// Every key two fragments of the same priority both set.
fn collisions(parts: &[Part], dst: &str) -> Vec<Diagnostic> {
    let sets: Vec<BTreeSet<String>> = parts
        .iter()
        .map(|p| {
            let mut s = BTreeSet::new();
            leaves(&p.node, "", &mut s);
            s
        })
        .collect();
    let mut out = Vec::new();
    for i in 0..parts.len() {
        for j in i + 1..parts.len() {
            if parts[i].priority != parts[j].priority {
                continue;
            }
            for key in sets[i].intersection(&sets[j]) {
                out.push(
                    Diagnostic::from_code(
                        "fragment-collision",
                        &[
                            ("left", parts[i].package),
                            ("right", parts[j].package),
                            ("key", key),
                            ("priority", &parts[i].priority.to_string()),
                            ("dst", dst),
                        ],
                    )
                    .with_provenance(prov(parts[i].package, parts[i].path))
                    .with_provenance(prov(parts[j].package, parts[j].path)),
                );
            }
        }
    }
    out
}

fn parse_node(format: &str, text: &str) -> Option<Node> {
    match format {
        "toml" => parse_toml(text).ok().map(|d| toml_to_node(&d)),
        "json" | "jsonc" | "yaml" | "ini" | "gitconfig" => node_fmt(format, text).ok(),
        _ => None,
    }
}

/// The collisions among fragments for one `dst`, by parsing each source as it is on disk.
/// A source that does not parse (it may hold `@dots()@` values) is left to the render to report.
pub fn lint_collisions(dst: &str, srcs: &[FragmentSource]) -> Vec<Diagnostic> {
    let parts: Vec<Part> = srcs
        .iter()
        .filter_map(|s| {
            Some(Part {
                package: &s.package,
                priority: s.priority,
                path: &s.path,
                node: parse_node(&s.format, &s.text)?,
            })
        })
        .collect();
    collisions(&parts, dst)
}

fn invalid(dst: &str, detail: String) -> Diagnostic {
    Diagnostic::from_code(
        "schema-invalid-value",
        &[
            ("key", "format"),
            ("file", dst),
            ("line", "1"),
            ("detail", &detail),
        ],
    )
}

fn ensure_nl(mut s: String) -> String {
    if !s.is_empty() && !s.ends_with('\n') {
        s.push('\n');
    }
    s
}

/// Render a composed file: every contribution, then the merge, under one header.
pub(crate) fn render_composed(file: &ResolvedFile, ctx: &FilesCtx) -> Result<String, Diagnostic> {
    let dst = file.dst.display().to_string();
    let parts = &file.extras.parts;
    if let Some(p) = parts.iter().find(|p| p.format != file.format) {
        return Err(invalid(
            &dst,
            format!(
                "fragment of `{}` is `{}` but `{}` is `{}`",
                p.package, p.format, parts[0].package, file.format
            ),
        ));
    }
    let mut used: BTreeMap<String, SettingUse> = BTreeMap::new();
    let mut bodies = Vec::new();
    for p in parts {
        let scope = Scope::new(ctx, &p.package);
        let text = render_body(p, ctx, &scope)?;
        for u in scope.used() {
            let name = if u.name.contains('.') {
                u.name.clone()
            } else {
                format!("{}.{}", p.package, u.name)
            };
            used.insert(name.clone(), SettingUse { name, ..u });
        }
        let label = p
            .layers
            .iter()
            .map(|l| l.label.as_str())
            .collect::<Vec<_>>()
            .join(" + ");
        bodies.push((p, label, text));
    }
    let used: Vec<SettingUse> = used.into_values().collect();
    let head = header(file, &used);
    let strat: Strategies = file.merge.clone();
    let body = match file.format.as_str() {
        "toml" | "json" | "jsonc" | "yaml" | "ini" | "gitconfig" => {
            merge_bodies(file, &bodies, &strat, &dst)?
        }
        "markdown" => {
            return Err(invalid(
                &dst,
                "markdown files cannot be assembled from fragments".into(),
            ));
        }
        _ => {
            let mut out = String::new();
            for (p, label, text) in &bodies {
                out.push_str(&format!(
                    "{} --- dots: fragment from package `{}` (priority {}, {label}) ---\n",
                    file.comment,
                    p.package,
                    p.extras.fragment.unwrap_or(0)
                ));
                out.push_str(&ensure_nl(text.clone()));
            }
            out
        }
    };
    Ok(with_head(&body, &head))
}

fn merge_bodies(
    file: &ResolvedFile,
    bodies: &[(&ResolvedFile, String, String)],
    strat: &Strategies,
    dst: &str,
) -> Result<String, Diagnostic> {
    let fmt = file.format.as_str();
    if let [(_, _, text)] = bodies {
        return Ok(ensure_nl(text.clone()));
    }
    let loaded: Vec<Loaded> = bodies
        .iter()
        .map(|(p, label, text)| Loaded {
            label: p.package.clone(),
            path: label.clone(),
            text: text.clone(),
        })
        .collect();
    let mut docs = Vec::new();
    let mut nodes = Vec::new();
    for l in &loaded {
        if fmt == "toml" {
            let d = parse_toml(&l.text).map_err(|e| parse_diag(l, "TOML", e))?;
            nodes.push(toml_to_node(&d));
            docs.push(d);
        } else {
            nodes.push(node_fmt(fmt, &l.text).map_err(|e| parse_diag(l, fmt, e))?);
        }
    }
    let parts: Vec<Part> = bodies
        .iter()
        .zip(nodes.iter())
        .map(|((p, label, _), n)| Part {
            package: &p.package,
            priority: p.extras.fragment.unwrap_or(0),
            path: label,
            node: n.clone(),
        })
        .collect();
    if let Some(d) = collisions(&parts, dst).into_iter().next() {
        return Err(d);
    }
    if fmt == "toml" {
        let mut it = docs.into_iter().enumerate();
        let (_, mut base) = it.next().expect("non-empty");
        for (i, d) in it {
            merge_toml(base.as_table_mut(), d.as_table(), "", strat)
                .map_err(|m| mismatch_diag(m, &loaded[0], &loaded[i]))?;
        }
        finish_toml(&mut base);
        return Ok(base.to_string());
    }
    let mut it = nodes.into_iter().enumerate();
    let (_, mut base) = it.next().expect("non-empty");
    for (i, n) in it {
        merge_value(&mut base, n, "", strat)
            .map_err(|m| mismatch_diag(m, &loaded[0], &loaded[i]))?;
    }
    let base = strip_deletes(base);
    Ok(match fmt {
        "json" | "jsonc" => write_json(&base),
        "yaml" => write_yaml(&base),
        "gitconfig" => write_ini(&base, "\t"),
        _ => write_ini(&base, ""),
    })
}
