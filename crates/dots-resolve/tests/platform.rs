//! Task 5.1: platform tables, merged before `when` is evaluated, and the `unknown-platform`
//! lint on `[platform."<key>"]` keys.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use dots_facts::{Chain, Facts};
use dots_model::{MachineConfig, OverrideLocation, Repo};
use dots_providers::Registry;
use dots_resolve::{Inputs, LintInputs, PackageState, lint, resolve};
use dots_testkit::FakeProvider;

fn write(root: &Path, rel: &str, text: &str) {
    let p = root.join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, text).unwrap();
}

fn ubuntu(wsl: bool) -> Facts {
    Facts {
        os: "linux".into(),
        arch: "x86_64".into(),
        distro: Some("ubuntu".into()),
        distro_version: Some("24.04".into()),
        id_like: vec!["debian".into()],
        is_wsl: wsl,
        has_sudo: true,
        ..Default::default()
    }
}

fn load(dir: &Path, facts: &Facts) -> (Repo, Chain) {
    let base = Repo::load(dir)
        .unwrap_or_else(|e| panic!("{:#?}", e.errors))
        .value;
    let f = facts.clone();
    let chain_of = move |m: &dots_model::RepoManifest| {
        let parents: BTreeMap<String, String> = m
            .platforms
            .iter()
            .filter_map(|(k, d)| Some((k.clone(), d.parent.clone()?)))
            .collect();
        Chain::build(&f, &parents, &[]).ordered()
    };
    let locs = OverrideLocation::standard(
        dir,
        None,
        dir.join("none"),
        "~/.config/dots/overrides".into(),
    );
    let repo = base
        .effective(&chain_of, &locs)
        .unwrap_or_else(|e| panic!("{:#?}", e.errors))
        .value;
    let chain = Chain::build(facts, &BTreeMap::new(), &[]);
    (repo, chain)
}

fn repo_with_when_package() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "dots.toml", "schema = 1\n");
    write(
        dir.path(),
        "bundles/all.toml",
        "schema = 1\npackages = [\"winclip\"]\n",
    );
    write(
        dir.path(),
        "packages/winclip/package.toml",
        "schema = 1\nname = \"winclip\"\nwhen = 'os = \"macos\"'\n[platform.wsl]\nwhen = \"true\"\n",
    );
    dir
}

fn state_of(repo: &Repo, chain: &Chain, facts: &Facts) -> PackageState {
    let machine = MachineConfig {
        bundles: vec!["all".into()],
        ..Default::default()
    };
    let registry = Registry::new().with(Box::new(FakeProvider::new("apt")));
    let installed = BTreeMap::new();
    resolve(&Inputs {
        repo,
        machine: &machine,
        facts,
        chain,
        providers: &registry,
        installed: &installed,
    })
    .get("winclip")
    .unwrap()
    .state
    .clone()
}

#[test]
fn platform_changes_existence() {
    let dir = repo_with_when_package();
    // Plain Ubuntu: `os = "macos"` is false, so the package is inert.
    let facts = ubuntu(false);
    let (repo, chain) = load(dir.path(), &facts);
    match state_of(&repo, &chain, &facts) {
        PackageState::Inert { condition } => assert_eq!(condition, "os = \"macos\""),
        s => panic!("{s:?}"),
    }
    // Under WSL the `[platform.wsl]` table replaces `when` before it is evaluated.
    let facts = ubuntu(true);
    let (repo, chain) = load(dir.path(), &facts);
    assert_eq!(state_of(&repo, &chain, &facts), PackageState::Active);
}

#[test]
fn unknown_platform_key_in_a_table_is_a_lint_error_with_a_suggestion() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "dots.toml", "schema = 1\n");
    write(
        dir.path(),
        "packages/neovim/package.toml",
        "schema = 1\nname = \"neovim\"\n[platform.\"ubunut-22.04\"]\nprefer = [\"archive\"]\n[platform.\"ubuntu-22.04\"]\nprefer = [\"archive\"]\n[platform.macos]\nbin = \"x\"\n",
    );
    // An override file's tables are checked too.
    write(
        dir.path(),
        "overrides/packages/neovim/override.toml",
        "schema = 1\n[platform.\"debain\"]\nbin = \"y\"\n",
    );
    let facts = ubuntu(false);
    let (repo, chain) = load(dir.path(), &facts);
    let report = lint(&LintInputs {
        repo: &repo,
        machine: None,
        chain: &chain,
    });
    let unknown: Vec<_> = report
        .errors
        .iter()
        .filter(|d| d.code == "unknown-platform")
        .collect();
    assert_eq!(unknown.len(), 2, "{:#?}", report.errors);
    let first = unknown
        .iter()
        .find(|d| d.message.contains("ubunut"))
        .unwrap();
    assert!(first.hint.contains("ubuntu-22.04"), "{}", first.hint);
    assert_eq!(
        first.provenance[0].file.as_deref(),
        Some("packages/neovim/package.toml")
    );
    assert_eq!(first.provenance[0].line, Some(3));
    let second = unknown
        .iter()
        .find(|d| d.message.contains("debain"))
        .unwrap();
    assert!(second.hint.contains("debian"), "{}", second.hint);
    assert_eq!(
        second.provenance[0].file.as_deref(),
        Some("overrides/packages/neovim/override.toml")
    );
}
