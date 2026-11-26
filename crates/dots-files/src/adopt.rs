//! `dots adopt`: bring an edit made to a managed file back into the repo source.
//!
//! * `copy` and `inject`: the edited content becomes the source.
//! * `render`, text path (text and markdown, templated or directive layers, single-layer
//!   files): a 3-way merge. `base` is what dots last rendered, `ours` the layer's source and
//!   `theirs` the edited destination, so template expressions survive wherever the edit did
//!   not touch them.
//! * `render`, structural path (several layers, or `--machine`): the structural difference
//!   between the last render and the edited file is written into one layer, as the changed
//!   keys only (a removed key is `__dots_delete__` when a lower layer defines it).
//!
//! Nothing is written when the merge conflicts, and a write that would not reproduce the
//! edited file once re-rendered is rolled back.

use std::fs;
use std::path::{Path, PathBuf};

use dots_core::report::Diagnostic;
use serde::Serialize;

use crate::diff3::merge3;
use crate::directives::has_directives;
use crate::fsops::{atomic_write_follow, ensure_parents, resolve_link, sha256_hex, sha256_path};
use crate::ledger::Ledger;
use crate::merge::{DELETE_MARKER, Node};
use crate::model::{BackupKind, FileState, FilesCtx, LayerSrc, Mode, ResolvedFile};
use crate::ops::{apply, plan};
use crate::render::{comment_prefix, node_fmt};
use crate::status::{build_step, extract_block, observe};
use crate::structured::{
    node_to_item, parse_toml, patch_toml, toml_to_node, write_ini, write_json, write_yaml,
};

/// Where the edit goes and what to do about it.
#[derive(Debug, Clone, Default)]
pub struct AdoptOpts<'a> {
    /// `--layer`: a layer of the file, by label, file name or platform name.
    pub layer: Option<&'a str>,
    /// `--machine`: the machine override layer file for this destination (it may not exist).
    pub machine_layer: Option<&'a Path>,
}

/// What `adopt` did.
#[derive(Debug, Clone, Serialize)]
pub struct Adopted {
    pub changed: bool,
    /// `none`, `copy`, `inject`, `text-merge` or `structural`.
    pub method: &'static str,
    /// The source file that was written (the first, when an edit spans layers).
    pub target: Option<PathBuf>,
    pub layer: Option<String>,
    /// The target file did not exist before.
    pub created: bool,
    /// Changed key paths (structural only).
    pub keys: Vec<String>,
    pub notes: Vec<String>,
}

impl Adopted {
    fn none() -> Self {
        Adopted {
            changed: false,
            method: "none",
            target: None,
            layer: None,
            created: false,
            keys: vec![],
            notes: vec![],
        }
    }
}

type Res<T> = Result<T, Diagnostic>;

fn err(code: &str, args: &[(&str, &str)]) -> Diagnostic {
    Diagnostic::from_code(code, args)
}

fn ensure_nl(mut s: String) -> String {
    if !s.ends_with('\n') {
        s.push('\n');
    }
    s
}

struct Ctx<'a> {
    file: &'a ResolvedFile,
    ctx: &'a FilesCtx,
    dst: String,
}

impl Ctx<'_> {
    fn unsupported(&self, detail: &str) -> Diagnostic {
        err(
            "adopt-unsupported",
            &[("dst", &self.dst), ("detail", detail)],
        )
    }
    fn unknown_base(&self, detail: &str) -> Diagnostic {
        err(
            "adopt-base-unknown",
            &[("dst", &self.dst), ("detail", detail)],
        )
    }
    fn conflict(&self, file: &Path, detail: &str) -> Diagnostic {
        let f = file.display().to_string();
        err(
            "adopt-conflict",
            &[("dst", &self.dst), ("file", &f), ("detail", detail)],
        )
        .with_provenance(dots_core::report::Provenance {
            source: "layer".into(),
            file: Some(f),
            line: None,
            col: None,
        })
    }
}

/// Adopt the edits made to `file`'s destination.
pub fn adopt(file: &ResolvedFile, ctx: &FilesCtx, opts: &AdoptOpts) -> Res<Adopted> {
    let c = Ctx {
        file,
        ctx,
        dst: file.dst.display().to_string(),
    };
    match file.mode {
        Mode::Render | Mode::Copy | Mode::Inject => {}
        Mode::Link => {
            return Err(c.unsupported("it is a link, so edits already land in the repo source"));
        }
        _ => return Err(c.unsupported("this kind of entry has no editable content")),
    }
    if file.mode == Mode::Render {
        if !file.extras.parts.is_empty() {
            return Err(c.unsupported(
                "it is composed from the fragments of several packages; edit the fragments",
            ));
        }
        if file.kind.as_deref() == Some("include") {
            return Err(c.unsupported("it is a generated include stub; edit the included layers"));
        }
    }
    let step = build_step(file, ctx);
    if let Some(e) = step.error.clone() {
        return Err(e);
    }
    let ledger = Ledger::load(&ctx.state);
    let rec = ledger.get(&file.dst, file.mode, &file.package);
    let state = observe(&step, rec, ctx);
    match state {
        FileState::Modified => {}
        FileState::Ok => return Ok(Adopted::none()),
        other => {
            let s = serde_json::to_value(other)
                .ok()
                .and_then(|v| v.as_str().map(str::to_string))
                .unwrap_or_default();
            return Err(err("adopt-not-modified", &[("dst", &c.dst), ("state", &s)]));
        }
    }
    let recorded = rec.and_then(|r| r.sha256.clone());
    if file.mode != Mode::Render && (opts.layer.is_some() || opts.machine_layer.is_some()) {
        return Err(c.unsupported("it has a single source, so there are no layers to choose from"));
    }
    match file.mode {
        Mode::Copy => adopt_copy(&c, recorded.as_deref()),
        Mode::Inject => adopt_inject(&c, recorded.as_deref()),
        _ => adopt_render(&c, recorded.as_deref(), opts),
    }
}

/// Re-plan the file against the new source and apply it, so the ledger and the generated copy
/// match and status reads `ok`. The edited content is already in the source, so no backup.
fn finalize(file: &ResolvedFile, ctx: &FilesCtx) -> Res<()> {
    if ctx.dry_run {
        return Ok(());
    }
    let mut steps = plan(std::slice::from_ref(file), ctx);
    let mut s = steps.remove(0);
    if let Some(e) = s.error.clone() {
        return Err(e);
    }
    s.backup = BackupKind::None;
    let out = apply(&s, ctx);
    match out.error {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

fn write_failed(path: &Path, e: &std::io::Error) -> Diagnostic {
    err(
        "write-failed",
        &[
            ("path", &path.display().to_string()),
            ("detail", &e.to_string()),
        ],
    )
}

fn adopt_copy(c: &Ctx, recorded: Option<&str>) -> Res<Adopted> {
    let src = c.file.src.clone().unwrap_or_default();
    let ours = fs::read(&src).map_err(|e| c.unknown_base(&e.to_string()))?;
    if Some(sha256_hex(&ours).as_str()) != recorded {
        return Err(c.unknown_base("the source changed since the last apply"));
    }
    let theirs = fs::read(&c.file.dst).map_err(|e| c.unsupported(&e.to_string()))?;
    if !c.ctx.dry_run {
        atomic_write_follow(&src, &theirs).map_err(|e| write_failed(&src, &e))?;
        finalize(c.file, c.ctx)?;
    }
    Ok(Adopted {
        changed: true,
        method: "copy",
        target: Some(src),
        ..Adopted::none()
    })
}

fn adopt_inject(c: &Ctx, recorded: Option<&str>) -> Res<Adopted> {
    let src = c.file.src.clone().unwrap_or_default();
    let ours = fs::read_to_string(&src).map_err(|e| c.unknown_base(&e.to_string()))?;
    if Some(sha256_hex(ours.trim_end_matches('\n').as_bytes()).as_str()) != recorded {
        return Err(c.unknown_base("the source changed since the last apply"));
    }
    let text = fs::read_to_string(&c.file.dst).map_err(|e| c.unsupported(&e.to_string()))?;
    let block = extract_block(&text, &c.file.package, &c.file.comment)
        .ok_or_else(|| c.unknown_base("the managed block is gone from the file"))?;
    if !c.ctx.dry_run {
        let new = if block.is_empty() {
            String::new()
        } else {
            format!("{block}\n")
        };
        atomic_write_follow(&src, new.as_bytes()).map_err(|e| write_failed(&src, &e))?;
        finalize(c.file, c.ctx)?;
    }
    Ok(Adopted {
        changed: true,
        method: "inject",
        target: Some(src),
        ..Adopted::none()
    })
}

/// The file with its generated header taken out (a file can lose or keep it).
pub(crate) fn strip_header(file: &ResolvedFile, text: &str) -> String {
    let Some((open, _)) = comment_prefix(file) else {
        return text.to_string();
    };
    let first = format!("{open} Generated by dots");
    let follow = [" layers: ", " fragments: ", " settings: "].map(|k| format!("{open}{k}"));
    let ls: Vec<&str> = text.split_inclusive('\n').collect();
    let Some(i) = ls.iter().position(|l| l.starts_with(&first)) else {
        return text.to_string();
    };
    let mut end = i + 1;
    while end < ls.len() && end <= i + 3 && follow.iter().any(|f| ls[end].starts_with(f.as_str())) {
        end += 1;
    }
    ls[..i].concat() + &ls[end..].concat()
}

fn is_override(l: &LayerSrc) -> bool {
    let comps: Vec<_> = l.path.components().collect();
    comps
        .windows(2)
        .any(|w| w[0].as_os_str() == "overrides" && w[1].as_os_str() == "packages")
}

fn find_layer(file: &ResolvedFile, name: &str) -> Option<usize> {
    let name_of = |l: &LayerSrc| {
        l.path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    };
    let dotted = format!("{name}.");
    let suffix = format!(".{name}");
    let by = |pred: &dyn Fn(&LayerSrc) -> bool| file.layers.iter().rposition(pred);
    by(&|l| l.label == name || l.path.to_string_lossy() == name || name_of(l) == name)
        .or_else(|| by(&|l| name_of(l).starts_with(&dotted) || name_of(l).ends_with(&suffix)))
        .or_else(|| {
            (name == "base")
                .then(|| file.layers.iter().position(|l| !is_override(l)))
                .flatten()
        })
}

/// Where an edit written for a render file goes.
enum Target {
    Layer(usize),
    Machine(PathBuf),
}

const STRUCTURED: [&str; 6] = ["toml", "json", "jsonc", "yaml", "ini", "gitconfig"];

fn adopt_render(c: &Ctx, recorded: Option<&str>, opts: &AdoptOpts) -> Res<Adopted> {
    let file = c.file;
    let theirs_full =
        fs::read_to_string(&file.dst).map_err(|_| c.unsupported("it is not valid UTF-8 text"))?;
    // The last render: the generated copy when it is still pristine (the app replaced the
    // link with its own file), else a fresh render when the source has not moved since.
    let step = build_step(file, c.ctx);
    let pristine = step
        .gen_path
        .as_deref()
        .filter(|g| sha256_path(g).as_deref() == recorded)
        .and_then(|g| fs::read_to_string(g).ok());
    let fresh = step
        .content
        .as_deref()
        .filter(|b| Some(sha256_hex(b).as_str()) == recorded)
        .map(|b| String::from_utf8_lossy(b).into_owned());
    let base_full = pristine.or(fresh).ok_or_else(|| {
        c.unknown_base("it changed since the last apply, and the generated copy was overwritten")
    })?;
    let base_body = ensure_nl(strip_header(file, &base_full));
    let theirs_body = ensure_nl(strip_header(file, &theirs_full));
    if base_body == theirs_body {
        finalize(file, c.ctx)?;
        return Ok(Adopted::none());
    }

    let structured = STRUCTURED.contains(&file.format.as_str());
    let target = match (opts.machine_layer, opts.layer) {
        (Some(p), _) => Target::Machine(p.to_path_buf()),
        (None, Some(name)) => Target::Layer(find_layer(file, name).ok_or_else(|| {
            let layers: Vec<&str> = file.layers.iter().map(|l| l.label.as_str()).collect();
            err(
                "adopt-unknown-layer",
                &[
                    ("layer", name),
                    ("dst", &c.dst),
                    ("layers", &layers.join(", ")),
                ],
            )
        })?),
        (None, None) => {
            let i = file
                .layers
                .iter()
                .rposition(|l| !is_override(l))
                .or_else(|| file.layers.len().checked_sub(1))
                .ok_or_else(|| c.unsupported("it has no source layers"))?;
            Target::Layer(i)
        }
    };

    // Which files get which new text.
    let mut writes: Vec<Write> = Vec::new();
    let mut out = Adopted::none();
    let mut extra_layer: Option<LayerSrc> = None;
    match &target {
        Target::Machine(p) => {
            if !structured || file.template.is_some() {
                return Err(c.unsupported(
                    "a text file cannot take a partial machine override; use `--layer`",
                ));
            }
            let idx = file.layers.iter().position(|l| &l.path == p);
            if idx.is_none() {
                extra_layer = Some(LayerSrc {
                    path: p.clone(),
                    label: p.display().to_string(),
                });
            }
            let upto = idx.unwrap_or(file.layers.len());
            let w = structural(c, &base_body, &theirs_body, p, upto, &mut out)?;
            writes.push(w);
        }
        Target::Layer(i) => {
            let l = &file.layers[*i];
            let raw = fs::read_to_string(&l.path).map_err(|e| {
                err(
                    "file-unreadable",
                    &[
                        ("file", &l.path.display().to_string()),
                        ("detail", &e.to_string()),
                    ],
                )
            })?;
            let single = file.layers.len() == 1;
            let templated = file.template.is_some() || has_directives(&raw);
            if single {
                writes.push(text_merge(c, l, &raw, &base_body, &theirs_body)?);
                out.method = "text-merge";
            } else if !structured && file.format != "markdown" {
                // Concatenated text: each section belongs to the layer it came from.
                let only = opts.layer.is_some().then_some(*i);
                writes.extend(sections(c, only, &base_body, &theirs_body)?);
                out.method = "text-merge";
            } else if templated || file.format == "markdown" {
                return Err(c.unsupported(
                    "this layer is merged with others and has directives, a template or a \
                     markdown body; use `--machine`, or adopt into a single-layer file",
                ));
            } else {
                writes.push(structural(
                    c,
                    &base_body,
                    &theirs_body,
                    &l.path,
                    *i,
                    &mut out,
                )?);
            }
        }
    }
    if writes.is_empty() {
        finalize(file, c.ctx)?;
        return Ok(Adopted::none());
    }
    out.changed = true;
    if out.method == "none" {
        out.method = "structural";
    }
    out.target = writes.first().map(|w| w.path.clone());
    out.created = writes.first().is_some_and(|w| w.old.is_none());
    out.layer = out.target.as_ref().map(|t| {
        file.layers
            .iter()
            .find(|l| &l.path == t)
            .map_or_else(|| t.display().to_string(), |l| l.label.clone())
    });
    if c.ctx.dry_run {
        return Ok(out);
    }

    for (n, w) in writes.iter().enumerate() {
        let r =
            ensure_parents(&w.path).and_then(|_| atomic_write_follow(&w.path, w.new.as_bytes()));
        if let Err(e) = r {
            rollback(&writes[..=n]);
            return Err(write_failed(&w.path, &e));
        }
    }
    let mut updated = file.clone();
    updated.layers.extend(extra_layer);
    // The new sources must render back to the edited file.
    let check = build_step(&updated, c.ctx);
    let reason = match (&check.error, &check.content) {
        (Some(e), _) => Some(e.message.clone()),
        (None, Some(b)) => {
            let got = ensure_nl(strip_header(&updated, &String::from_utf8_lossy(b)));
            same_render(&updated, &got, &base_body, &theirs_body)
        }
        _ => Some("the file no longer renders".into()),
    };
    if let Some(detail) = reason {
        rollback(&writes);
        let f = writes[0].path.display().to_string();
        return Err(err(
            "adopt-ineffective",
            &[("file", &f), ("dst", &c.dst), ("detail", &detail)],
        ));
    }
    finalize(&updated, c.ctx)?;
    Ok(out)
}

/// `None` when `got` (the re-render of the new sources) carries every edit between `base` and
/// `want` (the edited file). It may differ from `want` where the sources changed on their own.
fn same_render(file: &ResolvedFile, got: &str, base: &str, want: &str) -> Option<String> {
    let fmt = file.format.as_str();
    if STRUCTURED.contains(&fmt)
        && let (Ok(b), Ok(w), Ok(g)) = (
            parse_node(fmt, base),
            parse_node(fmt, want),
            parse_node(fmt, got),
        )
    {
        let mut ops = Vec::new();
        diff(&b, &w, &mut Vec::new(), &mut ops);
        return ops.iter().find_map(|op| {
            let have = get_path(&g, &op.path);
            (have != op.value.as_ref()).then(|| {
                format!(
                    "`{}` would not take the value you gave it",
                    op.path.join(".")
                )
            })
        });
    }
    let m = merge3(base, got, want);
    if m.is_clean() && m.text == got {
        None
    } else {
        first_difference(got, want)
    }
}

fn first_difference(got: &str, want: &str) -> Option<String> {
    if got == want {
        return None;
    }
    let (g, w): (Vec<_>, Vec<_>) = (got.lines().collect(), want.lines().collect());
    let n = (0..g.len().max(w.len()))
        .find(|&i| g.get(i) != w.get(i))
        .unwrap_or(0);
    Some(format!(
        "line {} would render as `{}` instead of `{}`",
        n + 1,
        g.get(n).unwrap_or(&"<end>"),
        w.get(n).unwrap_or(&"<end>")
    ))
}

struct Write {
    path: PathBuf,
    old: Option<String>,
    new: String,
}

fn rollback(writes: &[Write]) {
    for w in writes.iter().rev() {
        match &w.old {
            Some(o) => {
                let _ = atomic_write_follow(&w.path, o.as_bytes());
            }
            None => {
                let _ = fs::remove_file(resolve_link(&w.path));
            }
        }
    }
}

/// 3-way merge of one layer's source text.
fn text_merge(c: &Ctx, l: &LayerSrc, raw: &str, base: &str, theirs: &str) -> Res<Write> {
    let ours = ensure_nl(raw.to_string());
    let m = merge3(base, &ours, theirs);
    if !m.is_clean() {
        return Err(conflict_error(c, &l.path, &m.conflicts));
    }
    Ok(Write {
        path: l.path.clone(),
        old: Some(raw.to_string()),
        new: m.text,
    })
}

fn conflict_error(c: &Ctx, path: &Path, conflicts: &[crate::diff3::Conflict]) -> Diagnostic {
    let first = &conflicts[0];
    let show = |s: &str| s.lines().next().unwrap_or("").trim().to_string();
    let detail = format!(
        "{} region(s) changed on both sides, first at line {} (source `{}`, your edit `{}`)",
        conflicts.len(),
        first.ours_line,
        show(&first.ours),
        show(&first.theirs)
    );
    c.conflict(path, &detail)
}

fn marker_line(c: &Ctx, label: &str) -> String {
    format!("{} --- dots: from {label} ---\n", c.file.comment)
}

/// Split concatenated text layers at their `from` markers.
fn split_sections(c: &Ctx, body: &str) -> Option<Vec<String>> {
    let markers: Vec<String> = c
        .file
        .layers
        .iter()
        .map(|l| marker_line(c, &l.label))
        .collect();
    let ls: Vec<&str> = body.split_inclusive('\n').collect();
    let mut at = Vec::new();
    let mut from = 0;
    for m in &markers {
        let i = from + ls[from..].iter().position(|l| l == m)?;
        at.push(i);
        from = i + 1;
    }
    if at[0] != 0 {
        return None;
    }
    let mut out = Vec::new();
    for (n, &i) in at.iter().enumerate() {
        let end = at.get(n + 1).copied().unwrap_or(ls.len());
        out.push(ls[i + 1..end].concat());
    }
    Some(out)
}

fn sections(c: &Ctx, only: Option<usize>, base: &str, theirs: &str) -> Res<Vec<Write>> {
    let (Some(bs), Some(ts)) = (split_sections(c, base), split_sections(c, theirs)) else {
        return Err(c.unsupported(
            "the `dots: from` marker lines that separate its layers were changed or removed",
        ));
    };
    let mut out = Vec::new();
    for (i, l) in c.file.layers.iter().enumerate() {
        if bs[i] == ts[i] {
            continue;
        }
        if only.is_some_and(|o| o != i) {
            return Err(c.unsupported(&format!(
                "your edit touches the part that comes from `{}`; drop `--layer` to adopt each part into its own layer",
                l.label
            )));
        }
        let raw = fs::read_to_string(&l.path).map_err(|e| {
            err(
                "file-unreadable",
                &[
                    ("file", &l.path.display().to_string()),
                    ("detail", &e.to_string()),
                ],
            )
        })?;
        out.push(text_merge(c, l, &raw, &bs[i], &ts[i])?);
    }
    Ok(out)
}

// ---------------------------------------------------------------- structural

struct Op {
    path: Vec<String>,
    value: Option<Node>,
}

fn diff(base: &Node, theirs: &Node, path: &mut Vec<String>, out: &mut Vec<Op>) {
    match (base, theirs) {
        (Node::Table(b), Node::Table(t)) => {
            for (k, bv) in b {
                path.push(k.clone());
                match t.get(k) {
                    None => out.push(Op {
                        path: path.clone(),
                        value: None,
                    }),
                    Some(tv) => diff(bv, tv, path, out),
                }
                path.pop();
            }
            for (k, tv) in t {
                if !b.contains_key(k) {
                    path.push(k.clone());
                    out.push(Op {
                        path: path.clone(),
                        value: Some(tv.clone()),
                    });
                    path.pop();
                }
            }
        }
        (b, t) if b == t => {}
        (_, t) => out.push(Op {
            path: path.clone(),
            value: Some(t.clone()),
        }),
    }
}

fn get_path<'a>(n: &'a Node, path: &[String]) -> Option<&'a Node> {
    match path.split_first() {
        None => Some(n),
        Some((k, rest)) => match n {
            Node::Table(t) => get_path(t.get(k)?, rest),
            _ => None,
        },
    }
}

fn set_path(n: &mut Node, path: &[String], v: Option<Node>) {
    let Some((k, rest)) = path.split_first() else {
        return;
    };
    if !matches!(n, Node::Table(_)) {
        *n = Node::Table(Default::default());
    }
    let Node::Table(t) = n else { return };
    if rest.is_empty() {
        match v {
            Some(v) => {
                t.insert(k.clone(), v);
            }
            None => {
                t.shift_remove(k);
            }
        }
    } else if let Some(child) = t.get_mut(k) {
        set_path(child, rest, v);
    } else if v.is_some() {
        let mut child = Node::Table(Default::default());
        set_path(&mut child, rest, v);
        t.insert(k.clone(), child);
    }
}

/// Replace the value of an existing key without touching the comments around it.
fn set_in_place(doc: &mut toml_edit::DocumentMut, path: &[String], v: Option<&Node>) -> bool {
    use toml_edit::TableLike;
    let (Some(v), Some((last, parents))) = (v, path.split_last()) else {
        return false;
    };
    let mut cur: &mut dyn TableLike = doc.as_table_mut();
    for seg in parents {
        match cur.get_mut(seg).and_then(|i| i.as_table_like_mut()) {
            Some(t) => cur = t,
            None => return false,
        }
    }
    match cur.get_mut(last) {
        Some(item) if item.is_value() && !matches!(v, Node::Table(_)) => {
            *item = node_to_item(v);
            true
        }
        _ => false,
    }
}

fn parse_node(format: &str, text: &str) -> Result<Node, (u32, String)> {
    if format == "toml" {
        parse_toml(text).map(|d| toml_to_node(&d))
    } else {
        node_fmt(format, text)
    }
}

/// Write the structural difference `base -> theirs` into the layer at `target`. `below` is
/// how many of the file's layers come before it.
fn structural(
    c: &Ctx,
    base: &str,
    theirs: &str,
    target: &Path,
    below: usize,
    out: &mut Adopted,
) -> Res<Write> {
    let fmt = c.file.format.as_str();
    let bn = parse_node(fmt, base).map_err(|(l, d)| {
        c.unknown_base(&format!(
            "the last render does not parse as {fmt} (line {l}: {d})"
        ))
    })?;
    let tn = parse_node(fmt, theirs).map_err(|(l, d)| {
        c.unsupported(&format!("your version is not valid {fmt} (line {l}: {d})"))
    })?;
    let mut ops = Vec::new();
    diff(&bn, &tn, &mut Vec::new(), &mut ops);
    if ops.is_empty() {
        return Err(c.unsupported("the edit changes formatting only, no values"));
    }
    let below_nodes: Vec<Node> = c.file.layers[..below]
        .iter()
        .filter_map(|l| fs::read_to_string(&l.path).ok())
        .filter_map(|t| parse_node(fmt, &t).ok())
        .collect();
    let defined_below = |p: &[String]| below_nodes.iter().any(|n| get_path(n, p).is_some());
    // Only a missing file is a new file; a layer that cannot be read must not be overwritten
    // (and then deleted again by a rollback).
    let old = match fs::read_to_string(target) {
        Ok(t) => Some(t),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => {
            return Err(err(
                "file-unreadable",
                &[
                    ("file", &target.display().to_string()),
                    ("detail", &e.to_string()),
                ],
            ));
        }
    };
    let existing = old.clone().unwrap_or_default();
    out.method = "structural";
    out.keys = ops.iter().map(|o| o.path.join(".")).collect();
    let marker = || Some(Node::Str(DELETE_MARKER.to_string()));
    let new = if fmt == "toml" {
        let mut doc = parse_toml(&existing).map_err(|(l, d)| {
            err(
                "layer-parse-error",
                &[
                    ("file", &target.display().to_string()),
                    ("line", &l.to_string()),
                    ("format", "TOML"),
                    ("detail", &d),
                ],
            )
        })?;
        for op in &ops {
            let bad = |(at, found): (String, &str)| {
                c.unsupported(&format!(
                    "`{at}` in {} is a {found}, not a table",
                    target.display()
                ))
            };
            if !set_in_place(&mut doc, &op.path, op.value.as_ref()) {
                patch_toml(&mut doc, &op.path, op.value.as_ref()).map_err(bad)?;
            }
            if op.value.is_none() && defined_below(&op.path) {
                patch_toml(&mut doc, &op.path, marker().as_ref()).map_err(bad)?;
            }
        }
        doc.to_string()
    } else {
        let mut n = if existing.trim().is_empty() {
            Node::Table(Default::default())
        } else {
            parse_node(fmt, &existing).map_err(|(l, d)| {
                err(
                    "layer-parse-error",
                    &[
                        ("file", &target.display().to_string()),
                        ("line", &l.to_string()),
                        ("format", fmt),
                        ("detail", &d),
                    ],
                )
            })?
        };
        let ser = |n: &Node| match fmt {
            "json" | "jsonc" => write_json(n),
            "yaml" => write_yaml(n),
            "gitconfig" => write_ini(n, "\t"),
            _ => write_ini(n, ""),
        };
        // Only these formats are written whole, so the layer must survive that unchanged
        // apart from the keys being adopted: no comments, canonical formatting.
        if !existing.trim().is_empty() && ensure_nl(ser(&n)) != ensure_nl(existing.clone()) {
            return Err(c.unsupported(&format!(
                "{} has comments or formatting that rewriting the {fmt} would lose; adopt into a different layer (`--layer`, \
                 `--machine`), or edit it by hand",
                target.display()
            )));
        }
        for op in &ops {
            set_path(&mut n, &op.path, op.value.clone());
            if op.value.is_none() && defined_below(&op.path) {
                set_path(&mut n, &op.path, marker());
            }
        }
        ser(&n)
    };
    Ok(Write {
        path: target.to_path_buf(),
        old,
        new: ensure_nl(new),
    })
}
