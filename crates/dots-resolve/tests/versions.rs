//! Task 5.8: the version grammar, constraint intersection with provenance, and
//! constraint-aware provider selection (`package-resolution`), through the real loader.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use dots_facts::{Chain, Facts};
use dots_model::vspec::Spec;
use dots_model::{MachineConfig, OverrideLocation, Repo};
use dots_providers::{Caps, Installed, Provider, Registry, Support};
use dots_resolve::{
    CandidateOutcome, Inputs, InstalledMap, OfferSource, Resolution, SkipReason, resolve_with,
};
use dots_testkit::FakeProvider;

fn write(root: &Path, rel: &str, text: &str) {
    let p = root.join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, text).unwrap();
}

/// A repo with bundle `all` holding every package in `pkgs` (`name`, body after the header).
fn repo(pkgs: &[(&str, &str)], extra: &[(&str, &str)]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "dots.toml",
        "schema = 1\n[priority]\nany = [\"brew\", \"cargo\"]\n",
    );
    let names: Vec<String> = pkgs.iter().map(|(n, _)| format!("\"{n}\"")).collect();
    write(
        dir.path(),
        "bundles/all.toml",
        &format!("schema = 1\npackages = [{}]\n", names.join(", ")),
    );
    for (n, body) in pkgs {
        write(
            dir.path(),
            &format!("packages/{n}/package.toml"),
            &format!("schema = 1\nname = \"{n}\"\n{body}"),
        );
    }
    for (p, t) in extra {
        write(dir.path(), p, t);
    }
    dir
}

struct Offers(BTreeMap<(String, String), String>);

impl OfferSource for Offers {
    fn offered(
        &self,
        provider: &str,
        _: &str,
        native: &str,
        _: &dots_providers::Spec,
    ) -> Option<String> {
        self.0.get(&(provider.into(), native.into())).cloned()
    }
}

fn offers(rows: &[(&str, &str, &str)]) -> Offers {
    Offers(
        rows.iter()
            .map(|(p, n, v)| ((p.to_string(), n.to_string()), v.to_string()))
            .collect(),
    )
}

fn limited() -> Caps {
    Caps {
        exact_version: Support::Limited,
        ..FakeProvider::new("x").caps()
    }
}

fn run(
    dir: &tempfile::TempDir,
    registry: &Registry,
    offers: &Offers,
    installed: &InstalledMap,
) -> Resolution {
    let facts = Facts {
        os: "linux".into(),
        arch: "x86_64".into(),
        distro: Some("debian".into()),
        has_sudo: true,
        ..Default::default()
    };
    let machine = MachineConfig {
        bundles: vec!["all".into()],
        ..Default::default()
    };
    let chain = Chain::build(&facts, &BTreeMap::new(), &machine.platform_extra);
    let loaded = Repo::load(dir.path()).unwrap_or_else(|e| panic!("{:#?}", e.errors));
    let locations = OverrideLocation::standard(
        dir.path(),
        None,
        dir.path().join("machine-overrides"),
        "~/.config/dots/overrides".into(),
    );
    let repo = loaded
        .value
        .effective(&|_| chain.ordered(), &locations)
        .unwrap_or_else(|e| panic!("{:#?}", e.errors))
        .value;
    resolve_with(
        &Inputs {
            repo: &repo,
            machine: &machine,
            facts: &facts,
            chain: &chain,
            providers: registry,
            installed,
        },
        offers,
    )
}

fn plain_registry() -> Registry {
    Registry::new()
        .with(Box::new(FakeProvider::new("brew").with_caps(limited())))
        .with(Box::new(FakeProvider::new("cargo")))
}

// ---------------------------------------------------------------- the grammar

#[test]
fn invalid_spec_fails_loading_with_the_five_forms() {
    let dir = repo(&[("fd", "version = \"~1.2\"\n[providers.cargo]\n")], &[]);
    let e = Repo::load(dir.path()).unwrap_err();
    let d = e
        .errors
        .iter()
        .find(|d| d.code == "version-spec-invalid")
        .unwrap_or_else(|| panic!("{:#?}", e.errors));
    assert!(d.message.contains("~1.2"));
    for form in ["latest", "1.2.3", "1.2", ">=1.2", "git:<ref>"] {
        assert!(d.hint.contains(form), "{form} missing from {}", d.hint);
    }
    let p = &d.provenance[0];
    assert!(
        p.file
            .as_deref()
            .unwrap()
            .ends_with("packages/fd/package.toml")
    );
    assert_eq!(p.line, Some(3));
}

#[test]
fn an_invalid_requires_version_is_rejected_too() {
    let dir = repo(
        &[
            (
                "kv",
                "requires = [{ name = \"rust\", version = \"^1\" }]\n[providers.cargo]\n",
            ),
            ("rust", "[providers.cargo]\n"),
        ],
        &[],
    );
    let e = Repo::load(dir.path()).unwrap_err();
    assert!(e.errors.iter().any(|d| d.code == "version-spec-invalid"));
}

// ---------------------------------------------------------------- intersection

#[test]
fn compatible_prefix_and_floor_give_the_prefix() {
    let dir = repo(
        &[
            ("tool", "version = \"1.4\"\n[providers.cargo]\n"),
            (
                "user",
                "requires = [{ name = \"tool\", version = \">=1.2\" }]\n[providers.cargo]\n",
            ),
        ],
        &[],
    );
    let r = run(&dir, &plain_registry(), &offers(&[]), &InstalledMap::new());
    assert!(r.is_ok(), "{:#?}", r.errors);
    assert_eq!(r.get("tool").unwrap().wanted, Spec::Prefix(vec![1, 4]));
    assert_eq!(r.get("tool").unwrap().wanted.to_string(), "1.4");
}

#[test]
fn conflict_names_both_sides_with_file_line_and_inclusion_path() {
    // kv requires rust >=1.80; a repo override pins rust to 1.75.
    let dir = repo(
        &[
            (
                "kv",
                "requires = [\n  { name = \"rust\", version = \">=1.80\" },\n]\n[providers.cargo]\n",
            ),
            ("rust", "[providers.cargo]\n"),
        ],
        &[(
            "overrides/packages/rust/override.toml",
            "schema = 1\nversion = \"1.75\"\n",
        )],
    );
    let r = run(&dir, &plain_registry(), &offers(&[]), &InstalledMap::new());
    let d = r
        .errors
        .iter()
        .find(|d| d.code == "version-conflict")
        .unwrap_or_else(|| panic!("{:#?}", r.errors));
    assert!(d.message.contains("`rust`"), "{}", d.message);
    // Each side with its file and line.
    assert!(
        d.message.contains("packages/kv/package.toml:4"),
        "{}",
        d.message
    );
    assert!(
        d.message
            .contains("overrides/packages/rust/override.toml:2"),
        "{}",
        d.message
    );
    // The inclusion path and the override layer.
    assert!(
        d.message
            .contains("machine → all → kv → requires rust >=1.80"),
        "{}",
        d.message
    );
    assert!(d.message.contains("repo override"), "{}", d.message);
    assert!(d.hint.contains("override the dependent"), "{}", d.hint);
    assert_eq!(d.provenance.len(), 2);
    let files: Vec<&str> = d
        .provenance
        .iter()
        .filter_map(|p| p.file.as_deref())
        .collect();
    assert!(files.iter().any(|f| f.ends_with("kv/package.toml")));
    assert!(files.iter().any(|f| f.ends_with("rust/override.toml")));
}

#[test]
fn an_override_cannot_loosen_what_a_dependent_requires() {
    let dir = repo(
        &[
            (
                "kv",
                "requires = [{ name = \"rust\", version = \">=1.80\" }]\n[providers.cargo]\n",
            ),
            ("rust", "version = \"1.70\"\n[providers.cargo]\n"),
        ],
        &[(
            "overrides/packages/rust/override.toml",
            "schema = 1\nversion = \"latest\"\n",
        )],
    );
    let r = run(&dir, &plain_registry(), &offers(&[]), &InstalledMap::new());
    assert!(r.is_ok(), "{:#?}", r.errors);
    let rust = r.get("rust").unwrap();
    assert_eq!(rust.wanted.to_string(), ">=1.80");
    // The constraint list shows the override as the own spec, and kv's requirement beside it.
    let specs: Vec<(&str, &str)> = rust
        .constraints
        .constraints
        .iter()
        .map(|c| (c.spec.as_str(), c.source.file()))
        .collect();
    assert_eq!(
        specs,
        [
            ("latest", "overrides/packages/rust/override.toml"),
            (">=1.80", "packages/kv/package.toml")
        ]
    );
}

// ---------------------------------------------------------------- provider selection

#[test]
fn a_provider_that_cannot_satisfy_the_constraint_is_skipped_with_the_reason() {
    let dir = repo(
        &[(
            "fd",
            "version = \"8.7\"\n[providers.brew]\n[providers.cargo]\n",
        )],
        &[],
    );
    let r = run(
        &dir,
        &plain_registry(),
        &offers(&[("brew", "fd", "10.2.0")]),
        &InstalledMap::new(),
    );
    assert!(r.is_ok(), "{:#?}", r.errors);
    let sel = r.get("fd").unwrap().provider.as_ref().unwrap();
    assert_eq!(sel.chosen.as_ref().unwrap().id, "cargo");
    let brew = sel.candidates.iter().find(|c| c.id == "brew").unwrap();
    match &brew.outcome {
        CandidateOutcome::Skipped(SkipReason::CannotSatisfy(why)) => {
            assert_eq!(why, "offers 10.2.0, need 8.7");
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn brew_is_used_when_its_current_version_satisfies() {
    let dir = repo(
        &[(
            "fd",
            "version = \"10\"\n[providers.brew]\n[providers.cargo]\n",
        )],
        &[],
    );
    let r = run(
        &dir,
        &plain_registry(),
        &offers(&[("brew", "fd", "10.2.0")]),
        &InstalledMap::new(),
    );
    assert_eq!(r.get("fd").unwrap().chosen_provider().unwrap().id, "brew");
}

#[test]
fn an_installed_version_that_satisfies_counts_as_an_offer() {
    let dir = repo(
        &[(
            "fd",
            "version = \"8.7\"\n[providers.brew]\n[providers.cargo]\n",
        )],
        &[],
    );
    let mut installed = InstalledMap::new();
    installed.insert(
        "brew".into(),
        BTreeMap::from([(
            "fd".to_string(),
            Installed {
                version: "8.7.1".into(),
                pinned: false,
                source: None,
            },
        )]),
    );
    let r = run(
        &dir,
        &plain_registry(),
        &offers(&[("brew", "fd", "10.2.0")]),
        &installed,
    );
    assert_eq!(r.get("fd").unwrap().chosen_provider().unwrap().id, "brew");
}

#[test]
fn nothing_satisfying_the_constraint_lists_what_each_candidate_offers() {
    let dir = repo(&[("fd", "version = \"8.7\"\n[providers.brew]\n")], &[]);
    let r = run(
        &dir,
        &plain_registry(),
        &offers(&[("brew", "fd", "10.2.0")]),
        &InstalledMap::new(),
    );
    let d = r
        .errors
        .iter()
        .find(|d| d.code == "no-provider-satisfies")
        .unwrap_or_else(|| panic!("{:#?}", r.errors));
    assert!(d.hint.contains("brew"), "{}", d.hint);
    assert!(d.hint.contains("offers 10.2.0, need 8.7"), "{}", d.hint);
}

#[test]
fn a_git_ref_needs_a_git_capable_provider() {
    let dir = repo(
        &[(
            "kv",
            "version = \"git:main\"\n[providers.brew]\n[providers.cargo]\n",
        )],
        &[],
    );
    let r = run(&dir, &plain_registry(), &offers(&[]), &InstalledMap::new());
    let sel = r.get("kv").unwrap().provider.as_ref().unwrap();
    assert_eq!(sel.chosen.as_ref().unwrap().id, "cargo");
    let brew = sel.candidates.iter().find(|c| c.id == "brew").unwrap();
    assert!(matches!(
        &brew.outcome,
        CandidateOutcome::Skipped(SkipReason::CannotSatisfy(w)) if w.contains("git ref")
    ));
}

#[test]
fn a_git_ref_and_a_numeric_requirement_conflict() {
    let dir = repo(
        &[
            ("kv", "version = \"git:main\"\n[providers.cargo]\n"),
            (
                "user",
                "requires = [{ name = \"kv\", version = \"1.2\" }]\n[providers.cargo]\n",
            ),
        ],
        &[],
    );
    let r = run(&dir, &plain_registry(), &offers(&[]), &InstalledMap::new());
    let d = r
        .errors
        .iter()
        .find(|d| d.code == "version-conflict")
        .unwrap();
    assert!(d.hint.contains("git:main"), "{}", d.hint);
}
