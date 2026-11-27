//! `Repo::load`: dots.toml, every bundle and every package, collecting all errors.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use dots_core::Diagnostic;

use crate::defs;
use crate::effective::{EffectiveInfo, PlatformKeyRef};
use crate::load::{load_typed, read_source, top_key_span};
use crate::setting::DeclProblem;
use crate::source::{Diags, SourceFile};
use crate::types::{Bundle, Package, RepoManifest};

/// A loaded repository.
#[derive(Debug, Clone)]
pub struct Repo {
    pub root: PathBuf,
    pub manifest: RepoManifest,
    pub bundles: BTreeMap<String, Bundle>,
    pub packages: BTreeMap<String, Package>,
    /// Every `[platform."<key>"]` key in the definition files (and, after
    /// [`Repo::effective`], in the override files), for the `unknown-platform` lint.
    pub platform_keys: Vec<PlatformKeyRef>,
    /// Set by [`Repo::effective`]: provenance and the raw objects. `None` on a plain load.
    pub effective: Option<Arc<EffectiveInfo>>,
}

/// Result of a successful load: the value plus lint warnings (such as `escape-hatch-path`).
#[derive(Debug, Clone)]
pub struct Loaded<T> {
    pub value: T,
    pub warnings: Vec<Diagnostic>,
}

/// Every problem found while loading. Never empty `errors`.
#[derive(Debug, Clone)]
pub struct LoadErrors {
    pub errors: Vec<Diagnostic>,
    pub warnings: Vec<Diagnostic>,
}

impl LoadErrors {
    pub fn codes(&self) -> Vec<&str> {
        self.errors.iter().map(|d| d.code.as_str()).collect()
    }
}

impl Repo {
    /// Load `dots.toml`, `bundles/*.toml` and `packages/*/package.toml` under `root`.
    ///
    /// All files are parsed even when one fails; the error list holds every problem found.
    pub fn load(root: &Path) -> Result<Loaded<Repo>, LoadErrors> {
        let mut diags = Diags::default();

        let mut keys = Vec::new();
        let manifest = load_manifest(root, &mut diags, &mut keys);
        let bundles = load_bundles(root, &mut diags, &mut keys);
        let packages = load_packages(root, &mut diags, &mut keys);

        match (manifest, diags.errors.is_empty()) {
            (Some(manifest), true) => Ok(Loaded {
                value: Repo {
                    root: root.to_path_buf(),
                    manifest,
                    bundles,
                    packages,
                    platform_keys: keys,
                    effective: None,
                },
                warnings: diags.warnings,
            }),
            _ => Err(LoadErrors {
                errors: diags.errors,
                warnings: diags.warnings,
            }),
        }
    }

    /// `dots-too-old` when `requires_dots` is not met by the running dots version.
    pub fn check_dots_version(&self, current: &str) -> Option<Diagnostic> {
        self.manifest.check_dots_version(current)
    }
}

impl RepoManifest {
    /// `dots-too-old` when `requires_dots` is not met by the running dots version.
    pub fn check_dots_version(&self, current: &str) -> Option<Diagnostic> {
        crate::version::check_requires_dots(self.requires_dots.as_deref()?, current, "dots.toml")
    }
}

fn load_manifest(
    root: &Path,
    diags: &mut Diags,
    keys: &mut Vec<PlatformKeyRef>,
) -> Option<RepoManifest> {
    let path = root.join("dots.toml");
    let src = read_source(&path, "dots.toml".into(), "dots.toml".into(), diags)?;
    load_typed::<RepoManifest>(&src, &defs::REPO, false, diags).map(|l| {
        let mut m = l.value;
        m.spans = l.spans;
        keys.extend(l.platform_keys);
        m
    })
}

fn sorted_entries(dir: &Path) -> Vec<PathBuf> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut v: Vec<PathBuf> = rd.filter_map(|e| e.ok().map(|e| e.path())).collect();
    v.sort();
    v
}

fn load_bundles(
    root: &Path,
    diags: &mut Diags,
    keys: &mut Vec<PlatformKeyRef>,
) -> BTreeMap<String, Bundle> {
    let mut out = BTreeMap::new();
    for path in sorted_entries(&root.join("bundles")) {
        if path.extension().and_then(|e| e.to_str()) != Some("toml") {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        let display = format!("bundles/{stem}.toml");
        let Some(src) = read_source(&path, display, format!("bundle:{stem}"), diags) else {
            continue;
        };
        if let Some(l) = load_typed::<Bundle>(&src, &defs::BUNDLE, false, diags) {
            let mut b = l.value;
            b.name = stem.to_string();
            b.spans = l.spans;
            keys.extend(l.platform_keys);
            out.insert(stem.to_string(), b);
        }
    }
    out
}

fn load_packages(
    root: &Path,
    diags: &mut Diags,
    keys: &mut Vec<PlatformKeyRef>,
) -> BTreeMap<String, Package> {
    let mut out = BTreeMap::new();
    for dir in sorted_entries(&root.join("packages")) {
        if !dir.is_dir() {
            continue;
        }
        let Some(dirname) = dir.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        let display = format!("packages/{dirname}/package.toml");
        // A directory without a manifest (a placeholder, a leftover) is not a package: warn
        // and skip it, it must not break every command.
        if !dir.join("package.toml").exists() {
            let stub = SourceFile::new(display, format!("package:{dirname}"), String::new());
            diags.report(true, "package-missing-manifest", &[], &stub, None, None);
            continue;
        }
        let Some(src) = read_source(
            &dir.join("package.toml"),
            display,
            format!("package:{dirname}"),
            diags,
        ) else {
            continue;
        };
        let Some(l) = load_typed::<Package>(&src, &defs::PACKAGE, true, diags) else {
            continue;
        };
        let mut pkg = l.value;
        if pkg.name.is_empty() && top_key_span(&src, "name").is_none() {
            diags.file_error("schema-missing-key", &[("key", "name")], &src);
            continue;
        }
        if pkg.name != dirname {
            let (declared, expected) = (pkg.name.clone(), dirname.to_string());
            diags.error(
                "schema-name-mismatch",
                &[("declared", &declared), ("expected", &expected)],
                &src,
                top_key_span(&src, "name"),
            );
            continue;
        }
        if let Err(e) = pkg.finish() {
            diags.error(
                "schema-invalid-value",
                &[("key", "providers"), ("detail", &e)],
                &src,
                None,
            );
            continue;
        }
        if !check_setting_decls(&pkg, &src, diags) {
            continue;
        }
        pkg.manual = load_manual(&pkg.name, &dir, diags);
        pkg.dir = dir.clone();
        pkg.escape_hatch_paths = l.escape_hatch;
        pkg.spans = l.spans;
        keys.extend(l.platform_keys);
        out.insert(dirname.to_string(), pkg);
    }
    out
}

/// The package's `manual/*.md` files. A bad file is an error (`manual-invalid`) that does not
/// hide the package's other problems; two files with the same id are `manual-duplicate-id`.
fn load_manual(name: &str, dir: &Path, diags: &mut Diags) -> Vec<crate::manual::ManualAction> {
    let mut out: Vec<crate::manual::ManualAction> = Vec::new();
    for path in sorted_entries(&dir.join("manual")) {
        if path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        let display = format!("packages/{name}/manual/{stem}.md");
        let Some(src) = read_source(&path, display, format!("package:{name}"), diags) else {
            continue;
        };
        match crate::manual::parse(name, stem, &path, &src.text) {
            Err(p) => {
                let at = p.line.and_then(|l| line_span(&src, l));
                diags.error("manual-invalid", &[("detail", &p.detail)], &src, at);
            }
            Ok(a) => {
                if let Some(first) = out.iter().find(|o| o.id == a.id) {
                    let (id, other) = (a.full_id(), first.path.display().to_string());
                    diags.error(
                        "manual-duplicate-id",
                        &[("id", &id), ("other", &other)],
                        &src,
                        None,
                    );
                } else {
                    out.push(a);
                }
            }
        }
    }
    out
}

/// Byte range of 1-based `line`.
fn line_span(src: &SourceFile, line: u32) -> Option<std::ops::Range<usize>> {
    let mut start = 0usize;
    for (i, l) in src.text.split_inclusive('\n').enumerate() {
        if i as u32 + 1 == line {
            return Some(start..start + l.len());
        }
        start += l.len();
    }
    None
}

/// `[settings.<key>]`: exactly one of `default` / `required`, and a consistent declaration
/// (ARCHITECTURE §17.1). Returns false when the package must be dropped.
fn check_setting_decls(pkg: &Package, src: &SourceFile, diags: &mut Diags) -> bool {
    let mut ok = true;
    for (key, decl) in &pkg.settings {
        let Err(problem) = decl.check_decl() else {
            continue;
        };
        ok = false;
        let span = pkg
            .spans
            .get(&format!("settings.{key}"))
            .and_then(|(line, _)| line_span(src, *line));
        let setting = format!("{}.{key}", pkg.name);
        match problem {
            DeclProblem::NoDefault => {
                diags.error("setting-no-default", &[("setting", &setting)], src, span)
            }
            DeclProblem::Invalid(detail) => diags.report(
                false,
                "schema-invalid-value",
                &[("key", &format!("settings.{key}")), ("detail", &detail)],
                src,
                span,
                Some(format!("Fix the declaration of `{setting}`.")),
            ),
        }
    }
    ok
}
