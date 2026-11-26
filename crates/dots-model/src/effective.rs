//! The effective model: `[platform."<key>"]` tables and overrides applied to every object,
//! with provenance.
//!
//! [`Repo::effective`] turns a loaded [`Repo`] into the repo every other crate consumes.
//! Per object the layers apply lowest first (ARCHITECTURE §18.3):
//!
//! 1. the definition, then its platform tables, general to specific;
//! 2. `overrides/` (repo), then `machines/<profile>/overrides/`, then
//!    `~/.config/dots/overrides/` (machine). Each layer's own platform tables apply after
//!    its base content.
//!
//! The merge happens on raw values before the typed structs are rebuilt, so a platform table
//! can change `when` and the resolver only ever sees the merged result.
//!
//! Provenance is kept cheaply: only keys set by something other than the definition are
//! stored, as `(source id, line)`. A key with no entry comes from the definition, and its
//! position is in the typed object's `spans`.

use std::collections::BTreeMap;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use dots_core::{Diagnostic, Provenance};
use serde_json::{Map, Value};

use crate::defs;
use crate::load::{parse_schema1, read_source};
use crate::merge::{Merger, descend, entry_key, item_key, join};
use crate::repo::{LoadErrors, Loaded, Repo};
use crate::schema::{Env, State, Ty, validate};
use crate::source::{Diags, SourceFile, suggest};
use crate::spans::Spans;
use crate::types::{Bundle, Package, RepoManifest};

// ------------------------------------------------------------------ public types

/// Which layer a value came from, lowest first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Scope {
    /// The object's own file.
    Definition,
    /// `overrides/`, committed.
    Repo,
    /// `machines/<name>/overrides/`, committed, one machine profile only.
    Profile,
    /// `~/.config/dots/overrides/`, never committed.
    Machine,
}

impl Scope {
    /// `repo`, `profile` or `machine`: how `status` lists the scopes of a customization.
    pub fn short(self) -> &'static str {
        match self {
            Scope::Definition => "definition",
            Scope::Repo => "repo",
            Scope::Profile => "profile",
            Scope::Machine => "machine",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Scope::Definition => "definition",
            Scope::Repo => "repo override",
            Scope::Profile => "machine-profile override",
            Scope::Machine => "machine override",
        }
    }
}

/// Where a value was set: a layer's file, optionally one of its platform tables.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Source {
    pub scope: Scope,
    /// The `[platform."<key>"]` table inside that file, when the value came from one.
    pub platform: Option<String>,
    /// Display path: `packages/x/package.toml`, `overrides/packages/x/override.toml`, ...
    pub file: String,
}

impl Source {
    /// The annotation `show --effective` prints, e.g. `repo override (overrides/packages/x/override.toml)`.
    pub fn describe(&self) -> String {
        let table = |k: &str| format!("platform table [{k}]");
        match (&self.scope, &self.platform) {
            (Scope::Definition, None) => format!("definition ({})", self.file),
            (Scope::Definition, Some(k)) => format!("{} ({})", table(k), self.file),
            (s, None) => format!("{} ({})", s.label(), self.file),
            (s, Some(k)) => format!("{}, {} ({})", s.label(), table(k), self.file),
        }
    }
}

/// Where one effective key came from.
#[derive(Debug, Clone, Copy)]
pub struct Located<'a> {
    pub source: &'a Source,
    /// 1-based line in `source.file`, when known.
    pub line: Option<u32>,
}

/// Provenance of one object: the keys that something other than the definition set.
///
/// Key paths join keys with `.`; entries of keyed lists (`files`, `defaults`, `registry`,
/// `xdg`, bundle `packages` members) use their key: `files.~/.zshrc.src`,
/// `providers.apt.ppa`, `settings.font_size.default`. Plain lists are one key.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Prov {
    sources: Vec<Source>,
    keys: BTreeMap<String, (u16, u32)>,
}

impl Prov {
    /// Who set `path`: the exact key, else the nearest enclosing key that was set as a whole.
    /// `None` means the definition.
    pub fn get(&self, path: &str) -> Option<Located<'_>> {
        let mut p = path;
        loop {
            if let Some(&(s, line)) = self.keys.get(p) {
                return Some(Located {
                    source: &self.sources[s as usize],
                    line: (line > 0).then_some(line),
                });
            }
            p = p.rsplit_once('.')?.0;
        }
    }

    /// Every stored key with its source, in key order.
    pub fn entries(&self) -> impl Iterator<Item = (&str, Located<'_>)> {
        self.keys.iter().map(|(k, &(s, line))| {
            (
                k.as_str(),
                Located {
                    source: &self.sources[s as usize],
                    line: (line > 0).then_some(line),
                },
            )
        })
    }

    /// Entries at or under `prefix`.
    pub fn under<'a>(&'a self, prefix: &'a str) -> impl Iterator<Item = (&'a str, Located<'a>)> {
        self.entries().filter(move |(k, _)| {
            *k == prefix
                || k.strip_prefix(prefix)
                    .is_some_and(|rest| rest.starts_with('.'))
        })
    }

    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }

    /// The scopes that set anything, lowest first (the definition is never listed).
    pub fn scopes(&self) -> Vec<Scope> {
        let mut v: Vec<Scope> = self
            .keys
            .values()
            .map(|&(s, _)| self.sources[s as usize].scope)
            .filter(|s| *s != Scope::Definition)
            .collect();
        v.sort();
        v.dedup();
        v
    }

    pub(crate) fn source(&mut self, s: Source) -> u16 {
        if let Some(i) = self.sources.iter().position(|x| *x == s) {
            return i as u16;
        }
        self.sources.push(s);
        (self.sources.len() - 1) as u16
    }

    pub(crate) fn set(&mut self, path: String, src: u16, line: u32) {
        self.keys.insert(path, (src, line));
    }

    /// Forget everything under `path` (but not `path` itself).
    pub(crate) fn prune(&mut self, path: &str) {
        let start = format!("{path}.");
        let doomed: Vec<String> = self
            .keys
            .range(start.clone()..)
            .take_while(|(k, _)| k.starts_with(&start))
            .map(|(k, _)| k.clone())
            .collect();
        for k in doomed {
            self.keys.remove(&k);
        }
    }
}

/// One object of the effective model, as raw values plus provenance.
#[derive(Debug, Clone, PartialEq)]
pub struct EffObject {
    /// The file as written (platform tables included).
    pub definition: Value,
    /// After platform tables and overrides. `[platform.*]` tables stay in place, merged.
    pub effective: Value,
    pub prov: Prov,
}

/// An override file that was found and applied.
#[derive(Debug, Clone, PartialEq)]
pub struct OverrideFile {
    pub scope: Scope,
    /// Display path.
    pub file: String,
    pub path: PathBuf,
    /// The object it targets: `pkg:<name>`, `bundle:<name>` or `repo:manifest`.
    pub target: String,
    pub value: Value,
    /// Key path to `(line, col)` in the override file.
    pub spans: Spans,
}

/// A keyed override entry whose target does not exist: `remove = true` on nothing, or a
/// partial copy of a file entry that is not there. Task 5.7's stale-override lint reports
/// these; the merge ignores them.
#[derive(Debug, Clone, PartialEq)]
pub struct Miss {
    /// The object: `pkg:<name>`, ...
    pub address: String,
    /// Key path of the missing entry, e.g. `files.~/.zshrc`.
    pub path: String,
    pub scope: Scope,
    pub file: String,
    pub line: Option<u32>,
}

/// Everything the effective pass knows beyond the typed objects.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct EffectiveInfo {
    /// The ordered platform keys that were merged (with `<key>/<arch>` qualifiers).
    pub chain: Vec<String>,
    /// By address: `pkg:<name>`, `bundle:<name>`, `repo:manifest`.
    pub objects: BTreeMap<String, EffObject>,
    /// Every override file applied, in precedence order.
    pub overrides: Vec<OverrideFile>,
    pub misses: Vec<Miss>,
}

impl EffectiveInfo {
    /// The overrides that target `address`, lowest precedence first.
    pub fn overrides_of<'a>(&'a self, address: &'a str) -> impl Iterator<Item = &'a OverrideFile> {
        self.overrides.iter().filter(move |o| o.target == address)
    }

    /// The scopes that customize `address` (what `status` shows as `✎ overridden (repo, machine)`).
    pub fn scopes_of(&self, address: &str) -> Vec<Scope> {
        let mut v: Vec<Scope> = self.overrides_of(address).map(|o| o.scope).collect();
        v.sort();
        v.dedup();
        v
    }
}

/// A `[platform."<key>"]` key somewhere in a file, for the `unknown-platform` lint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlatformKeyRef {
    pub key: String,
    /// Display path of the file.
    pub file: String,
    /// Provenance source kind (`package:x`, `bundle:x`, `dots.toml`, `override:repo`, ...).
    pub source: String,
    pub line: Option<u32>,
    pub col: Option<u32>,
}

pub(crate) fn platform_refs(
    src: &SourceFile,
    keys: &[(String, Option<Range<usize>>)],
) -> Vec<PlatformKeyRef> {
    keys.iter()
        .map(|(k, span)| {
            let pos = span.as_ref().map(|s| src.pos(s.start));
            PlatformKeyRef {
                key: k.clone(),
                file: src.display.clone(),
                source: src.kind.clone(),
                line: pos.map(|p| p.0),
                col: pos.map(|p| p.1),
            }
        })
        .collect()
}

/// One place overrides are read from.
#[derive(Debug, Clone)]
pub struct OverrideLocation {
    pub scope: Scope,
    /// The directory holding `dots.toml`, `packages/` and `bundles/`.
    pub dir: PathBuf,
    /// How that directory is shown: `overrides`, `machines/<name>/overrides`,
    /// `~/.config/dots/overrides`.
    pub display: String,
}

impl OverrideLocation {
    /// The three locations of a repo at `root`, lowest precedence first. `profile` is the
    /// applied machine profile; `machine_dir` is `~/.config/dots/overrides`.
    pub fn standard(
        root: &Path,
        profile: Option<&str>,
        machine_dir: PathBuf,
        machine_display: String,
    ) -> Vec<OverrideLocation> {
        let mut v = vec![OverrideLocation {
            scope: Scope::Repo,
            dir: root.join("overrides"),
            display: "overrides".into(),
        }];
        if let Some(p) = profile {
            v.push(OverrideLocation {
                scope: Scope::Profile,
                dir: root.join("machines").join(p).join("overrides"),
                display: format!("machines/{p}/overrides"),
            });
        }
        v.push(OverrideLocation {
            scope: Scope::Machine,
            dir: machine_dir,
            display: machine_display,
        });
        v
    }
}

// --------------------------------------------------------------------- internals

#[derive(Debug, Clone)]
pub(crate) struct RawErr {
    pub path: String,
    pub src: u16,
    pub line: u32,
    pub expected: String,
}

#[derive(Debug, Clone)]
pub(crate) struct RawMiss {
    pub path: String,
    pub src: u16,
    pub line: u32,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Manifest,
    Bundle,
    Package,
}

impl Kind {
    fn ty(self) -> &'static Ty {
        match self {
            Kind::Manifest => &defs::REPO,
            Kind::Bundle => &BUNDLE_OVERRIDE,
            Kind::Package => &defs::PACKAGE,
        }
    }
}

/// A bundle override is a partial bundle plus `[members.<name>]` entries.
const BUNDLE_OVERRIDE: Ty = Ty::Obj(&[
    ("schema", Ty::Int),
    ("description", Ty::Str),
    ("include", Ty::List(&Ty::Str)),
    (
        "packages",
        Ty::List(&Ty::Alt(&[
            Ty::Str,
            Ty::Obj(&[("name", Ty::Str), ("when", Ty::Str)]),
        ])),
    ),
    ("settings", Ty::Map(&Ty::Map(&Ty::Any))),
    ("recommended", Ty::Bool),
    ("integrity", Ty::Enum(&["warn", "enforce"])),
    (
        "members",
        Ty::Map(&Ty::Table(&[("when", Ty::Str), ("name", Ty::Str)])),
    ),
]);

fn sorted_entries(dir: &Path) -> Vec<PathBuf> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut v: Vec<PathBuf> = rd.filter_map(|e| e.ok().map(|e| e.path())).collect();
    v.sort();
    v
}

fn diag_at(code: &str, args: &[(&str, &str)], source: &str, file: &str, line: u32) -> Diagnostic {
    let line_s = line.max(1).to_string();
    let mut all: Vec<(&str, &str)> = vec![("file", file), ("line", &line_s)];
    all.extend_from_slice(args);
    Diagnostic::from_code(code, &all).with_provenance(Provenance {
        source: source.to_string(),
        file: Some(file.to_string()),
        line: (line > 0).then_some(line),
        col: None,
    })
}

/// `name` and `origin` keys at the top of an override or of one of its platform tables.
fn identity_fields(doc: &toml_edit::Document<&str>) -> Vec<(String, Option<Range<usize>>)> {
    let mut out = Vec::new();
    let mut scan = |t: &dyn toml_edit::TableLike| {
        for k in ["name", "origin"] {
            if let Some((key, v)) = t.get_key_value(k) {
                out.push((k.to_string(), key.span().or_else(|| v.span())));
            }
        }
    };
    let root = doc.as_table();
    scan(root);
    if let Some(plat) = root.get("platform").and_then(|p| p.as_table_like()) {
        for (_, t) in plat.iter() {
            if let Some(t) = t.as_table_like() {
                scan(t);
            }
        }
    }
    // A bundle member is keyed by its package name: `[members.x] name = ...` would rename it.
    if let Some(ms) = root.get("members").and_then(|m| m.as_table_like()) {
        for (_, t) in ms.iter() {
            if let Some(t) = t.as_table_like() {
                scan(t);
            }
        }
    }
    out
}

struct Discovery<'a> {
    repo: &'a Repo,
    diags: &'a mut Diags,
    files: Vec<OverrideFile>,
    platform_keys: Vec<PlatformKeyRef>,
}

impl Discovery<'_> {
    fn stale(&mut self, address: &str, display: &str, scope: Scope, closest: Option<String>) {
        let src = SourceFile::new(
            display.to_string(),
            format!("override:{}", scope.short()),
            String::new(),
        );
        let hint = match closest {
            Some(c) => format!("Did you mean `{c}`? Move the override there, or remove it."),
            None => {
                "Remove the override, or run `dots override edit` on an address that exists.".into()
            }
        };
        self.diags.report(
            false,
            "stale-override",
            &[("address", address)],
            &src,
            None,
            Some(hint),
        );
    }

    fn read(&mut self, path: &Path, display: String, scope: Scope, kind: Kind, address: &str) {
        let diags = &mut *self.diags;
        let Some(src) = read_source(
            path,
            display.clone(),
            format!("override:{}", scope.short()),
            diags,
        ) else {
            return;
        };
        let Some(doc) = parse_schema1(&src, diags) else {
            return;
        };
        let ids = identity_fields(&doc);
        for (field, span) in &ids {
            diags.error(
                "override-identity-field",
                &[("field", field), ("address", address)],
                &src,
                span.clone(),
            );
        }
        if !ids.is_empty() {
            return;
        }
        let env = Env {
            src: &src,
            check_paths: kind == Kind::Package,
            partial: true,
            provider_ty: defs::provider_ty,
        };
        let mut st = State::default();
        validate(&env, &mut st, kind.ty(), doc.as_item());
        let failed = !st.diags.errors.is_empty();
        diags.extend(st.diags);
        if failed {
            return;
        }
        let value: Value = match toml_edit::de::from_str(&src.text) {
            Ok(v) => v,
            Err(e) => {
                diags.report(
                    false,
                    "schema-invalid-value",
                    &[("key", ""), ("detail", e.message())],
                    &src,
                    e.span(),
                    None,
                );
                return;
            }
        };
        self.platform_keys
            .extend(platform_refs(&src, &st.platform_keys));
        self.files.push(OverrideFile {
            scope,
            file: display,
            path: path.to_path_buf(),
            target: address.to_string(),
            value,
            spans: crate::spans::collect(&src, doc.as_item()),
        });
    }

    fn location(&mut self, loc: &OverrideLocation) {
        let manifest = loc.dir.join("dots.toml");
        if manifest.is_file() {
            self.read(
                &manifest,
                format!("{}/dots.toml", loc.display),
                loc.scope,
                Kind::Manifest,
                "repo:manifest",
            );
        }
        for dir in sorted_entries(&loc.dir.join("packages")) {
            let file = dir.join("override.toml");
            let Some(name) = dir.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            if !dir.is_dir() || !file.is_file() {
                continue;
            }
            let display = format!("{}/packages/{name}/override.toml", loc.display);
            if self.repo.packages.contains_key(name) {
                self.read(
                    &file,
                    display,
                    loc.scope,
                    Kind::Package,
                    &format!("pkg:{name}"),
                );
            } else {
                let closest = suggest(name, self.repo.packages.keys().map(String::as_str))
                    .map(|s| format!("pkg:{s}"));
                self.stale(&format!("pkg:{name}"), &display, loc.scope, closest);
            }
        }
        for path in sorted_entries(&loc.dir.join("bundles")) {
            if path.extension().and_then(|e| e.to_str()) != Some("toml") {
                continue;
            }
            let Some(name) = path.file_stem().and_then(|n| n.to_str()) else {
                continue;
            };
            let display = format!("{}/bundles/{name}.toml", loc.display);
            if self.repo.bundles.contains_key(name) {
                self.read(
                    &path,
                    display,
                    loc.scope,
                    Kind::Bundle,
                    &format!("bundle:{name}"),
                );
            } else {
                let closest = suggest(name, self.repo.bundles.keys().map(String::as_str))
                    .map(|s| format!("bundle:{s}"));
                self.stale(&format!("bundle:{name}"), &display, loc.scope, closest);
            }
        }
    }
}

/// One layer to merge: a definition or an override file.
struct Layer<'a> {
    scope: Scope,
    file: &'a str,
    value: &'a Value,
    spans: &'a Spans,
}

#[derive(Default)]
struct Run {
    target: Value,
    prov: Prov,
    misses: Vec<RawMiss>,
    errors: Vec<RawErr>,
    /// A platform table matched or an override applied: the typed object must be rebuilt.
    applied: bool,
}

impl Run {
    fn merge_step(
        &mut self,
        layer: &Layer,
        platform: Option<&str>,
        at: &[&str],
        src: &Value,
        (path, spath): (&str, &str),
    ) {
        // The definition's own content is the default and is not stored.
        let record = platform.is_some() || layer.scope != Scope::Definition;
        let id = self.prov.source(Source {
            scope: layer.scope,
            platform: platform.map(str::to_string),
            file: layer.file.to_string(),
        });
        let Some(target) = descend(&mut self.target, at) else {
            return;
        };
        Merger {
            prov: &mut self.prov,
            misses: &mut self.misses,
            errors: &mut self.errors,
            spans: layer.spans,
            src: id,
            record,
        }
        .merge(target, src, path, spath);
    }

    fn apply(&mut self, layer: &Layer, kind: Kind, chain: &[String]) {
        let is_def = layer.scope == Scope::Definition;
        // 1. The layer's own content. A bundle override's `[members.<n>]` is applied
        // separately because it addresses entries of `packages`.
        let mut base = layer.value.clone();
        let members = match (&mut base, kind) {
            (Value::Object(m), Kind::Bundle) if !is_def => m.remove("members"),
            _ => None,
        };
        self.merge_step(layer, None, &[], &base, ("", ""));
        if let Some(Value::Object(ms)) = members {
            self.members(layer, &ms);
        }

        // 2. Platform tables, general to specific. For each chain key the top-level
        // `[platform.<key>]` table applies first, then the tables nested in providers,
        // settings and file entries, so a more specific key always beats a more general
        // one regardless of where it is written.
        for key in chain {
            if let Some(Value::Object(plat)) = layer.value.get("platform")
                && let Some(tbl) = plat.get(key)
            {
                self.applied = true;
                let spath = join("platform", key);
                self.merge_step(layer, Some(key), &[], tbl, ("", &spath));
            }
            if kind == Kind::Package {
                for coll in ["providers", "settings", "files"] {
                    self.nested_platform(layer, coll, key);
                }
            }
        }
    }

    fn nested_platform(&mut self, layer: &Layer, coll: &str, ck: &str) {
        let Some(cv) = layer.value.get(coll) else {
            return;
        };
        // (key, entry, its position path in the layer's file)
        let mut entries: Vec<(String, &Value, String)> = Vec::new();
        fn list<'v>(
            entries: &mut Vec<(String, &'v Value, String)>,
            coll: &str,
            a: &'v [Value],
            sp: &str,
        ) {
            for (i, e) in a.iter().enumerate() {
                entries.push((entry_key(coll, e), e, join(sp, &i.to_string())));
            }
        }
        match cv {
            Value::Array(a) if coll == "files" => list(&mut entries, coll, a, coll),
            Value::Object(m) if coll == "files" => {
                for (k, v) in m {
                    match (k.as_str(), v) {
                        ("add" | "prepend", Value::Array(a)) => {
                            list(&mut entries, coll, a, &join(coll, k))
                        }
                        ("remove", _) => {}
                        _ => entries.push((k.clone(), v, join(coll, k))),
                    }
                }
            }
            Value::Object(m) => {
                for (k, v) in m {
                    entries.push((k.clone(), v, join(coll, k)));
                }
            }
            _ => {}
        }
        for (key, entry, espath) in entries {
            let Some(Value::Object(plat)) = entry.get("platform") else {
                continue;
            };
            if let Some(tbl) = plat.get(ck) {
                self.applied = true;
                let spath = format!("{espath}.platform.{ck}");
                self.merge_step(
                    layer,
                    Some(ck),
                    &[coll, &key],
                    tbl,
                    (&join(coll, &key), &spath),
                );
            }
        }
    }

    /// Bundle `[members.<name>]`: patch, add or (with `remove = true`) delete one entry of
    /// `packages`.
    fn members(&mut self, layer: &Layer, ms: &Map<String, Value>) {
        let id = self.prov.source(Source {
            scope: layer.scope,
            platform: None,
            file: layer.file.to_string(),
        });
        let Some(root) = self.target.as_object_mut() else {
            return;
        };
        let packages = root
            .entry("packages")
            .or_insert_with(|| Value::Array(Vec::new()));
        let Some(list) = packages.as_array_mut() else {
            return;
        };
        for (name, partial) in ms {
            let spath = join("members", name);
            let line = layer.spans.get(&spath).map_or(0, |p| p.0);
            let idx = list.iter().position(|e| item_key(e) == *name);
            let epath = join("packages", name);
            if partial.get("remove") == Some(&Value::Bool(true)) {
                match idx {
                    Some(i) => {
                        list.remove(i);
                        self.prov.prune(&epath);
                    }
                    None => self.misses.push(RawMiss {
                        path: epath,
                        src: id,
                        line,
                    }),
                }
                continue;
            }
            let i = idx.unwrap_or_else(|| {
                list.push(Value::String(name.clone()));
                list.len() - 1
            });
            if !list[i].is_object() {
                list[i] = serde_json::json!({ "name": name });
            }
            let mut patch = partial.clone();
            if let Value::Object(m) = &mut patch {
                m.remove("remove");
            }
            Merger {
                prov: &mut self.prov,
                misses: &mut self.misses,
                errors: &mut self.errors,
                spans: layer.spans,
                src: id,
                record: true,
            }
            .merge(&mut list[i], &patch, &epath, &spath);
        }
    }
}

impl EffectiveInfo {
    /// The merged value of `address` with only the `below` overrides applied (the definition
    /// and each layer's platform tables for this machine's chain included), i.e. what the
    /// value would be just under the next override. `None` for an unknown object.
    pub(crate) fn merged_below(&self, address: &str, below: &[&OverrideFile]) -> Option<Value> {
        let obj = self.objects.get(address)?;
        let kind = if address.starts_with("pkg:") {
            Kind::Package
        } else if address.starts_with("bundle:") {
            Kind::Bundle
        } else {
            Kind::Manifest
        };
        let run = run_object(kind, &obj.definition, "", &Spans::new(), below, &self.chain);
        Some(run.target)
    }
}

/// Merge one object's layers.
fn run_object(
    kind: Kind,
    definition: &Value,
    def_file: &str,
    def_spans: &Spans,
    overrides: &[&OverrideFile],
    chain: &[String],
) -> Run {
    let mut run = Run {
        target: Value::Object(Map::new()),
        ..Default::default()
    };
    run.apply(
        &Layer {
            scope: Scope::Definition,
            file: def_file,
            value: definition,
            spans: def_spans,
        },
        kind,
        chain,
    );
    for o in overrides {
        run.applied = true;
        run.apply(
            &Layer {
                scope: o.scope,
                file: &o.file,
                value: &o.value,
                spans: &o.spans,
            },
            kind,
            chain,
        );
    }
    run
}

fn read_value(path: &Path) -> Option<Value> {
    toml_edit::de::from_str(&std::fs::read_to_string(path).ok()?).ok()
}

struct Pass<'a> {
    overrides: &'a [OverrideFile],
    diags: Diags,
    info: EffectiveInfo,
}

impl Pass<'_> {
    fn overrides_of(&self, address: &str) -> Vec<&OverrideFile> {
        self.overrides
            .iter()
            .filter(|o| o.target == address)
            .collect()
    }

    /// Run one object. `None` when its file cannot be read again (keep the typed object).
    fn object(
        &mut self,
        kind: Kind,
        address: &str,
        path: &Path,
        display: &str,
        spans: &Spans,
        chain: &[String],
    ) -> Option<(Value, bool)> {
        let definition = read_value(path)?;
        let ovs = self.overrides_of(address);
        let run = run_object(kind, &definition, display, spans, &ovs, chain);
        let source = format!("override:{address}");
        for e in &run.errors {
            let file = &run.prov.sources[e.src as usize].file;
            self.diags.errors.push(diag_at(
                "schema-wrong-type",
                &[
                    ("key", &e.path),
                    ("expected", &e.expected),
                    ("found", "something else"),
                ],
                &source,
                file,
                e.line,
            ));
        }
        for m in &run.misses {
            let s = &run.prov.sources[m.src as usize];
            self.info.misses.push(Miss {
                address: address.to_string(),
                path: m.path.clone(),
                scope: s.scope,
                file: s.file.clone(),
                line: (m.line > 0).then_some(m.line),
            });
        }
        let applied = run.applied;
        self.info.objects.insert(
            address.to_string(),
            EffObject {
                definition,
                effective: run.target.clone(),
                prov: run.prov,
            },
        );
        Some((run.target, applied))
    }

    fn rebuild_error(&mut self, address: &str, display: &str, e: &str) {
        let last = self
            .overrides_of(address)
            .last()
            .map_or(display.to_string(), |o| o.file.clone());
        self.diags.errors.push(diag_at(
            "schema-invalid-value",
            &[("key", ""), ("detail", e)],
            &format!("override:{address}"),
            &last,
            0,
        ));
    }
}

impl Repo {
    /// Apply platform tables and overrides: the effective repo every command works on.
    ///
    /// `chain_of` gives the ordered platform keys (`Chain::ordered`) for a manifest; it runs
    /// on the definition first and again on the effective manifest, because overrides can
    /// declare `[platforms]` parents. `locations` lists the override directories, lowest
    /// precedence first (see [`OverrideLocation::standard`]).
    ///
    /// Problems with override files (syntax, schema, identity fields, targets that no longer
    /// exist) come back together as [`LoadErrors`]. The result's `effective` field holds the
    /// provenance.
    pub fn effective(
        &self,
        chain_of: &dyn Fn(&RepoManifest) -> Vec<String>,
        locations: &[OverrideLocation],
    ) -> Result<Loaded<Repo>, LoadErrors> {
        let mut diags = Diags::default();
        let mut disc = Discovery {
            repo: self,
            diags: &mut diags,
            files: Vec::new(),
            platform_keys: Vec::new(),
        };
        for loc in locations {
            disc.location(loc);
        }
        let files = disc.files;
        let override_keys = disc.platform_keys;

        let mut pass = Pass {
            overrides: &files,
            diags,
            info: EffectiveInfo::default(),
        };
        let mut out = self.clone();

        // The manifest first: its `[platforms]` shape the chain everything else uses.
        let manifest_path = self.root.join("dots.toml");
        let mut chain = chain_of(&self.manifest);
        let mut manifest = None;
        for _ in 0..2 {
            pass.info.misses.clear();
            let Some((value, applied)) = pass.object(
                Kind::Manifest,
                "repo:manifest",
                &manifest_path,
                "dots.toml",
                &self.manifest.spans,
                &chain,
            ) else {
                break;
            };
            if !applied {
                break;
            }
            match serde_json::from_value::<RepoManifest>(value) {
                Ok(mut m) => {
                    m.spans = self.manifest.spans.clone();
                    let next = chain_of(&m);
                    manifest = Some(m);
                    if next == chain {
                        break;
                    }
                    chain = next;
                }
                Err(e) => {
                    pass.rebuild_error("repo:manifest", "dots.toml", &e.to_string());
                    break;
                }
            }
        }
        if let Some(m) = manifest {
            out.manifest = m;
        }

        for (name, b) in &self.bundles {
            let display = format!("bundles/{name}.toml");
            let path = self.root.join(&display);
            let address = format!("bundle:{name}");
            if let Some((v, true)) =
                pass.object(Kind::Bundle, &address, &path, &display, &b.spans, &chain)
            {
                match serde_json::from_value::<Bundle>(v) {
                    Ok(mut nb) => {
                        nb.name = name.clone();
                        nb.spans = b.spans.clone();
                        out.bundles.insert(name.clone(), nb);
                    }
                    Err(e) => pass.rebuild_error(&address, &display, &e.to_string()),
                }
            }
        }
        for (name, p) in &self.packages {
            let display = format!("packages/{name}/package.toml");
            let path = p.dir.join("package.toml");
            let address = format!("pkg:{name}");
            if let Some((v, true)) =
                pass.object(Kind::Package, &address, &path, &display, &p.spans, &chain)
            {
                let built = serde_json::from_value::<Package>(v)
                    .map_err(|e| e.to_string())
                    .and_then(|mut np| {
                        np.finish()?;
                        np.dir = p.dir.clone();
                        np.escape_hatch_paths = p.escape_hatch_paths.clone();
                        np.spans = p.spans.clone();
                        Ok(np)
                    });
                match built {
                    Ok(np) => {
                        out.packages.insert(name.clone(), np);
                    }
                    Err(e) => pass.rebuild_error(&address, &display, &e),
                }
            }
        }

        let Pass {
            diags, mut info, ..
        } = pass;
        if !diags.errors.is_empty() {
            return Err(LoadErrors {
                errors: diags.errors,
                warnings: diags.warnings,
            });
        }
        info.chain = chain;
        info.overrides = files;
        out.platform_keys.extend(override_keys);
        out.effective = Some(Arc::new(info));
        Ok(Loaded {
            value: out,
            warnings: diags.warnings,
        })
    }
}
