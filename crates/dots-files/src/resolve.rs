//! Turn a package's `[[files]]` entry into a [`ResolvedFile`].

use std::path::{Path, PathBuf};

use dots_core::report::{Diagnostic, Provenance};
use dots_model::{FileEntry, Layer, Layers, PlatformString};

use crate::merge::{Strategy, parse_strategy};
use crate::model::{FilesCtx, LayerSrc, Mode, ResolvedFile};
use crate::pathvars::{expand_path, pick_platform};

fn src_path(pkg_dir: &Path, s: &str, ctx: &FilesCtx) -> PathBuf {
    match s.strip_prefix("//") {
        Some(rest) => ctx.root.join(rest),
        None => pkg_dir.join(s),
    }
}

fn label(p: &Path, ctx: &FilesCtx) -> String {
    p.strip_prefix(&ctx.root).unwrap_or(p).display().to_string()
}

fn layer(p: PathBuf, ctx: &FilesCtx) -> LayerSrc {
    LayerSrc {
        label: label(&p, ctx),
        path: p,
    }
}

/// File name matches `key` exactly or as `key.<ext>`.
fn matches_key(name: &str, key: &str) -> bool {
    name == key || name.strip_prefix(key).is_some_and(|r| r.starts_with('.'))
}

fn auto_layers(dir: &Path, ctx: &FilesCtx) -> Vec<LayerSrc> {
    let names: Vec<(String, PathBuf)> = std::fs::read_dir(dir)
        .map(|rd| {
            let mut v: Vec<_> = rd
                .filter_map(|e| e.ok())
                .filter(|e| e.path().is_file())
                .map(|e| (e.file_name().to_string_lossy().into_owned(), e.path()))
                .collect();
            v.sort();
            v
        })
        .unwrap_or_default();
    let mut keys = vec!["common".to_string()];
    keys.extend(ctx.chain.iter().filter(|k| *k != "common").cloned());
    let mut out = Vec::new();
    for k in keys {
        // An exact name beats `<key>.<ext>`.
        let hit = names
            .iter()
            .find(|(n, _)| *n == k)
            .or_else(|| names.iter().find(|(n, _)| matches_key(n, &k)));
        if let Some((_, p)) = hit {
            out.push(layer(p.clone(), ctx));
        }
    }
    out
}

pub fn detect_format(explicit: Option<&str>, dst: &Path) -> String {
    if let Some(f) = explicit {
        return f.to_string();
    }
    let name = dst.file_name().and_then(|n| n.to_str()).unwrap_or("");
    let ext = dst.extension().and_then(|e| e.to_str()).unwrap_or("");
    match ext {
        "toml" => "toml",
        "json" => "json",
        "jsonc" => "jsonc",
        "yaml" | "yml" => "yaml",
        "ini" => "ini",
        "md" | "markdown" => "markdown",
        _ if name.ends_with("gitconfig") => "gitconfig",
        _ => "text",
    }
    .to_string()
}

/// Resolve one entry. `Ok(None)` when its `when` is false. `when` evaluates condition strings;
/// `extra_layers` are the repo-override, machine-profile and machine-override layers, appended
/// after the package layers.
pub fn resolve_entry(
    package: &str,
    pkg_dir: &Path,
    entry: &FileEntry,
    ctx: &FilesCtx,
    when: &dyn Fn(&str) -> Result<bool, String>,
    extra_layers: &[LayerSrc],
) -> Result<Option<ResolvedFile>, Diagnostic> {
    let pkg_file = pkg_dir.join("package.toml").display().to_string();
    let bad = |key: &str, detail: String| {
        Diagnostic::from_code(
            "schema-invalid-value",
            &[
                ("key", key),
                ("file", &pkg_file),
                ("line", "1"),
                ("detail", &detail),
            ],
        )
        .with_provenance(Provenance {
            source: package.to_string(),
            file: Some(pkg_file.clone()),
            line: None,
            col: None,
        })
    };
    if let Some(c) = &entry.when
        && !when(c).map_err(|e| bad("when", e))?
    {
        return Ok(None);
    }
    let mode = match entry.mode.as_deref().unwrap_or("link") {
        "link" => Mode::Link,
        "copy" => Mode::Copy,
        "render" => Mode::Render,
        "inject" => Mode::Inject,
        "dir" => Mode::Dir,
        other => {
            return Err(Diagnostic::from_code(
                "not-implemented",
                &[("command", &format!("files mode `{other}`"))],
            ));
        }
    };
    let dst_raw = match &entry.dst {
        None => return Err(bad("dst", "missing".into())),
        Some(PlatformString::Plain(s)) => s.clone(),
        Some(PlatformString::PerPlatform(m)) => pick_platform(m, &ctx.chain)
            .cloned()
            .ok_or_else(|| bad("dst", "no entry for this platform and no `default`".into()))?,
    };
    let dst = expand_path(&dst_raw, ctx).map_err(|name| {
        Diagnostic::from_code(
            "unknown-name",
            &[("name", &name), ("file", &pkg_file), ("line", "1")],
        )
    })?;

    let mut layers = Vec::new();
    let mut src = None;
    if mode == Mode::Render {
        match &entry.layers {
            Some(Layers::Auto { dir, auto }) => {
                let d = src_path(pkg_dir, dir, ctx);
                if *auto {
                    layers = auto_layers(&d, ctx);
                } else {
                    return Err(bad("layers", "`auto = true` is required with `dir`".into()));
                }
            }
            Some(Layers::List(l)) => {
                for item in l {
                    match item {
                        Layer::Path(p) => layers.push(layer(src_path(pkg_dir, p, ctx), ctx)),
                        Layer::Conditional { src, when: w } => {
                            let keep = match w {
                                Some(c) => when(c).map_err(|e| bad("layers", e))?,
                                None => true,
                            };
                            if keep {
                                layers.push(layer(src_path(pkg_dir, src, ctx), ctx));
                            }
                        }
                    }
                }
            }
            None => {
                let s = entry
                    .src
                    .as_deref()
                    .ok_or_else(|| bad("src", "render needs `src` or `layers`".into()))?;
                let base = src_path(pkg_dir, s, ctx);
                // `<src>.<platform>` siblings, general to specific.
                let fname = base
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                layers.push(layer(base.clone(), ctx));
                for k in &ctx.chain {
                    let sib = base.with_file_name(format!("{fname}.{k}"));
                    if sib.is_file() {
                        layers.push(layer(sib, ctx));
                    }
                }
            }
        }
        layers.extend(extra_layers.iter().cloned());
    } else if mode != Mode::Dir {
        let s = entry.src.as_deref().ok_or_else(|| {
            bad(
                "src",
                format!(
                    "mode `{}` needs `src`",
                    entry.mode.as_deref().unwrap_or("link")
                ),
            )
        })?;
        src = Some(src_path(pkg_dir, s, ctx));
    }

    let mut merge = std::collections::BTreeMap::<String, Strategy>::new();
    for (k, v) in &entry.merge {
        merge.insert(
            k.clone(),
            parse_strategy(v).map_err(|e| bad(&format!("files.merge.{k}"), e))?,
        );
    }
    let format = detect_format(entry.format.as_deref(), &dst);
    let comment = if format == "jsonc" { "//" } else { "#" }.to_string();
    Ok(Some(ResolvedFile {
        package: package.to_string(),
        mode,
        dst,
        src,
        layers,
        format,
        kind: entry.kind.clone(),
        template: entry.template.clone(),
        merge,
        comment,
    }))
}
