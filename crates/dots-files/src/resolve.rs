//! Turn a package's `[[files]]` entry into a [`ResolvedFile`].

use std::path::{Path, PathBuf};

use dots_core::report::{Diagnostic, Provenance};
use dots_model::{FileEntry, Layer, Layers, PlatformString};

use crate::merge::{Strategy, parse_strategy};
use crate::model::{Extras, FilesCtx, LayerSrc, Mode, ResolvedFile, SetPatch};
use crate::pathvars::{expand_path, pick_platform};

fn src_path(pkg_dir: &Path, s: &str, ctx: &FilesCtx) -> PathBuf {
    match s.strip_prefix("//") {
        Some(rest) => ctx.root.join(rest),
        None => pkg_dir.join(s),
    }
}

fn label(p: &Path, ctx: &FilesCtx) -> String {
    // A repo-relative label is `/`-separated on every OS: it lands in generated file headers
    // and goldens, which must not differ between machines.
    match p.strip_prefix(&ctx.root) {
        Ok(rel) => rel
            .components()
            .map(|c| c.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join("/"),
        Err(_) => p.display().to_string(),
    }
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

/// The line-comment leader for a destination, from its format, then its name and extension
/// (ARCHITECTURE section 17.3). `comment = "..."` on the entry overrides this.
pub fn infer_comment(format: &str, dst: &Path) -> &'static str {
    if format == "jsonc" {
        return "//";
    }
    let name = dst
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let ext = dst
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match ext.as_str() {
        "lua" | "sql" | "hs" | "elm" | "ada" => return "--",
        "vim" => return "\"",
        "kbd" => return ";;",
        "el" | "lisp" | "scm" | "clj" | "asm" | "ini" => return ";",
        "json5" | "js" | "mjs" | "cjs" | "ts" | "jsx" | "tsx" | "c" | "cc" | "cpp" | "h"
        | "hpp" | "go" | "rs" | "java" | "kt" | "swift" | "scala" | "glsl" => return "//",
        "tex" | "erl" => return "%",
        _ => {}
    }
    match name.as_str() {
        "vimrc" | ".vimrc" | "gvimrc" | ".gvimrc" | "_vimrc" => "\"",
        ".xresources" | ".xdefaults" => "!",
        _ => "#",
    }
}

/// The destination of an entry for this machine (platform table applied, variables expanded),
/// without resolving its layers. `None` when it has none or it cannot be expanded.
pub fn entry_dst(entry: &FileEntry, ctx: &FilesCtx) -> Option<PathBuf> {
    let raw = match entry.dst.as_ref()? {
        PlatformString::Plain(s) => s.clone(),
        PlatformString::PerPlatform(m) => pick_platform(m, &ctx.chain)?.clone(),
    };
    expand_path(&raw, ctx).ok()
}

/// Where the override layer of a file lives under an `overrides/` scope: keyed by the whole
/// destination, so two files of one package with the same name cannot collide, and never
/// next to `override.toml`. `files/home/<path below home>` or `files/root/<absolute path>`.
pub fn override_layer_rel(package: &str, dst: &Path, home: &Path) -> PathBuf {
    use std::path::Component;
    let mut p = PathBuf::from("packages").join(package).join("files");
    let rest = match dst.strip_prefix(home) {
        Ok(r) => {
            p.push("home");
            r
        }
        Err(_) => {
            p.push("root");
            dst
        }
    };
    for c in rest.components() {
        if let Component::Normal(n) = c {
            p.push(n);
        }
    }
    p
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
    // A fragment renders like a `render` entry; the engine composes the fragments of one `dst`.
    let fragment = (entry.mode.as_deref() == Some("fragment")).then(|| entry.priority.unwrap_or(0));
    let mode = match entry.mode.as_deref().unwrap_or("link") {
        "link" => Mode::Link,
        "copy" => Mode::Copy,
        "render" | "fragment" => Mode::Render,
        "inject" => Mode::Inject,
        "dir" => Mode::Dir,
        "terminfo" => Mode::Terminfo,
        other => {
            return Err(Diagnostic::from_code(
                "not-implemented",
                &[("command", &format!("files mode `{other}`"))],
            ));
        }
    };
    let dst_raw = match &entry.dst {
        None if mode == Mode::Terminfo => "~/.terminfo".to_string(),
        None => return Err(bad("dst", "missing".into())),
        Some(PlatformString::Plain(s)) => s.clone(),
        Some(PlatformString::PerPlatform(m)) => pick_platform(m, &ctx.chain)
            .cloned()
            .ok_or_else(|| bad("dst", "no entry for this platform and no `default`".into()))?,
    };
    let mut dst = expand_path(&dst_raw, ctx).map_err(|name| {
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

    // A terminfo entry's `dst` is the terminfo directory; the entry itself is `<dir>/<name>`.
    if mode == Mode::Terminfo {
        let from_source = src
            .as_deref()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|t| crate::terminfo::source_name(&t));
        let name = from_source.unwrap_or_else(|| {
            src.as_deref()
                .and_then(|p| p.file_stem())
                .map_or_else(|| "terminfo".into(), |n| n.to_string_lossy().into_owned())
        });
        dst = dst.join(name);
    }

    let mut merge = std::collections::BTreeMap::<String, Strategy>::new();
    for (k, v) in &entry.merge {
        merge.insert(
            k.clone(),
            parse_strategy(v).map_err(|e| bad(&format!("files.merge.{k}"), e))?,
        );
    }
    let format = detect_format(entry.format.as_deref(), &dst);
    let comment = entry
        .comment
        .clone()
        .unwrap_or_else(|| infer_comment(&format, &dst).to_string());
    let set = entry
        .set
        .iter()
        .map(|s| SetPatch {
            path: s.path.clone(),
            value: s.value.clone(),
            delete: s.delete,
            when: s.when.clone(),
        })
        .collect();
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
        extras: Extras {
            set,
            fragment,
            parts: Vec::new(),
        },
    }))
}
