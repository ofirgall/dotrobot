//! `Repo::load`: dots.toml, every bundle and every package, collecting all errors.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use dots_core::Diagnostic;

use crate::defs;
use crate::load::{load_typed, read_source, top_key_span};
use crate::source::Diags;
use crate::types::{Bundle, Package, RepoManifest};

/// A loaded repository.
#[derive(Debug, Clone)]
pub struct Repo {
    pub root: PathBuf,
    pub manifest: RepoManifest,
    pub bundles: BTreeMap<String, Bundle>,
    pub packages: BTreeMap<String, Package>,
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

        let manifest = load_manifest(root, &mut diags);
        let bundles = load_bundles(root, &mut diags);
        let packages = load_packages(root, &mut diags);

        match (manifest, diags.errors.is_empty()) {
            (Some(manifest), true) => Ok(Loaded {
                value: Repo {
                    root: root.to_path_buf(),
                    manifest,
                    bundles,
                    packages,
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

fn load_manifest(root: &Path, diags: &mut Diags) -> Option<RepoManifest> {
    let path = root.join("dots.toml");
    let src = read_source(&path, "dots.toml".into(), "dots.toml".into(), diags)?;
    load_typed::<RepoManifest>(&src, &defs::REPO, false, diags).map(|l| {
        let mut m = l.value;
        m.spans = l.spans;
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

fn load_bundles(root: &Path, diags: &mut Diags) -> BTreeMap<String, Bundle> {
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
            out.insert(stem.to_string(), b);
        }
    }
    out
}

fn load_packages(root: &Path, diags: &mut Diags) -> BTreeMap<String, Package> {
    let mut out = BTreeMap::new();
    for dir in sorted_entries(&root.join("packages")) {
        if !dir.is_dir() {
            continue;
        }
        let Some(dirname) = dir.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        let display = format!("packages/{dirname}/package.toml");
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
        pkg.dir = dir.clone();
        pkg.escape_hatch_paths = l.escape_hatch;
        pkg.spans = l.spans;
        out.insert(dirname.to_string(), pkg);
    }
    out
}
