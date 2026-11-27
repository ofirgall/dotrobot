//! `requires_sudo = true` on a package or a provider block: it loads, is read back through the
//! model, is type-checked by the schema, and `dots lint` flags it where it is redundant.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use dots_facts::{Chain, Facts};
use dots_model::{ProviderConfig, Repo};
use dots_resolve::{LintInputs, lint};

fn write(root: &Path, rel: &str, text: &str) {
    let p = root.join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, text).unwrap();
}

fn repo_with(body: &str) -> (tempfile::TempDir, Result<Repo, Vec<String>>) {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "dots.toml", "schema = 1\n");
    write(
        dir.path(),
        "packages/orbstack/package.toml",
        &format!("schema = 1\nname = \"orbstack\"\n{body}"),
    );
    let r = Repo::load(dir.path())
        .map(|l| l.value)
        .map_err(|e| e.codes().iter().map(|c| c.to_string()).collect());
    (dir, r)
}

fn lint_codes(repo: &Repo) -> Vec<String> {
    let chain = Chain::build(&Facts::default(), &BTreeMap::new(), &[]);
    lint(&LintInputs {
        repo,
        machine: None,
        chain: &chain,
    })
    .warnings
    .into_iter()
    .map(|d| d.code.to_string())
    .collect()
}

#[test]
fn package_level_requires_sudo_loads() {
    let (_d, r) = repo_with("requires_sudo = true\n[providers.brew]\ncask = true\n");
    let repo = r.unwrap();
    let p = &repo.packages["orbstack"];
    assert!(p.requires_sudo);
    assert!(!p.providers["brew"].common().requires_sudo);
    assert!(lint_codes(&repo).is_empty());
}

#[test]
fn provider_level_requires_sudo_loads_for_typed_and_custom_providers() {
    let (_d, r) = repo_with("[providers.brew]\ncask = true\nrequires_sudo = true\n");
    let repo = r.unwrap();
    let p = &repo.packages["orbstack"];
    assert!(!p.requires_sudo);
    assert!(p.providers["brew"].common().requires_sudo);
    assert!(matches!(p.providers["brew"], ProviderConfig::Brew(ref b) if b.requires_sudo));

    let (_d, r) = repo_with("[providers.mytool]\nrequires_sudo = true\n");
    let repo = r.unwrap();
    assert!(
        repo.packages["orbstack"].providers["mytool"]
            .common()
            .requires_sudo
    );
}

#[test]
fn requires_sudo_must_be_a_boolean() {
    let (_d, r) = repo_with("requires_sudo = \"yes\"\n[providers.brew]\n");
    assert_eq!(r.err().unwrap(), ["schema-wrong-type"]);
    let (_d, r) = repo_with("[providers.brew]\nrequires_sudo = 1\n");
    assert_eq!(r.err().unwrap(), ["schema-wrong-type"]);
}

#[test]
fn lint_flags_requires_sudo_on_a_provider_that_is_root_anyway() {
    let (_d, r) = repo_with("[providers.apt]\nrequires_sudo = true\n");
    assert_eq!(lint_codes(&r.unwrap()), ["requires-sudo-redundant"]);
    let (_d, r) = repo_with(
        "[providers.script]\ncheck = \"true\"\nneeds_root = true\nrequires_sudo = true\n",
    );
    assert_eq!(lint_codes(&r.unwrap()), ["requires-sudo-redundant"]);
}
