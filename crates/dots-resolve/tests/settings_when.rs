//! `when` on packages and bundle members can read package settings (the resolver is handed
//! the already-resolved settings).

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use dots_facts::{Chain, Condition, EvalCtx, Facts};
use dots_model::{MachineConfig, Repo};
use dots_resolve::{Inputs, InstalledMap, resolve, resolve_full};
use dots_testkit::FakeProvider;

fn write(root: &Path, rel: &str, text: &str) {
    let p = root.join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, text).unwrap();
}

fn setup() -> (tempfile::TempDir, Repo) {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "dots.toml", "schema = 1\n");
    write(
        dir.path(),
        "packages/tool/package.toml",
        "schema = 1\nname = \"tool\"\n[providers.brew]\nname = \"tool\"\n[settings.flavor]\ntype = \"string\"\ndefault = \"a\"\n",
    );
    write(
        dir.path(),
        "packages/ext/package.toml",
        "schema = 1\nname = \"ext\"\nwhen = 'settings.tool.flavor = \"b\"'\n[providers.brew]\nname = \"ext\"\n",
    );
    write(
        dir.path(),
        "packages/mem/package.toml",
        "schema = 1\nname = \"mem\"\n[providers.brew]\nname = \"mem\"\n[settings.on]\ntype = \"bool\"\ndefault = false\n",
    );
    write(
        dir.path(),
        "bundles/all.toml",
        "schema = 1\npackages = [\"tool\", \"ext\", { name = \"mem\", when = \"setting.on\" }]\n",
    );
    let repo = Repo::load(dir.path()).unwrap().value;
    (dir, repo)
}

fn run(repo: &Repo, machine: &MachineConfig, with_settings: bool) -> dots_resolve::Resolution {
    let facts = Facts {
        os: "macos".into(),
        arch: "aarch64".into(),
        ..Default::default()
    };
    let chain = Chain::build(&facts, &BTreeMap::new(), &[]);
    let tags = Default::default();
    let declared = Default::default();
    let when = |src: &str| {
        Condition::parse(src)
            .and_then(|c| {
                c.eval(&EvalCtx {
                    facts: &facts,
                    chain: &chain,
                    tags: &tags,
                    declared_tags: &declared,
                    settings: &dots_facts::NoSettings,
                })
            })
            .map_err(|e| e.message)
    };
    let settings = dots_config::resolve(&dots_config::Inputs::new(repo, machine, &when));
    let registry = dots_providers::Registry::new().with(Box::new(FakeProvider::new("brew")));
    let installed: InstalledMap = BTreeMap::new();
    let inp = Inputs {
        repo,
        machine,
        facts: &facts,
        chain: &chain,
        providers: &registry,
        installed: &installed,
    };
    if with_settings {
        resolve_full(&inp, &dots_resolve::NoOffers, &settings)
    } else {
        resolve(&inp)
    }
}

fn machine(local: &[(&str, &str, serde_json::Value)]) -> MachineConfig {
    let mut m = MachineConfig {
        bundles: vec!["all".into()],
        ..Default::default()
    };
    for (p, k, v) in local {
        m.local_settings
            .entry(p.to_string())
            .or_default()
            .insert(k.to_string(), v.clone());
    }
    m
}

#[test]
fn package_and_member_when_read_settings() {
    let (_d, repo) = setup();
    // Defaults: tool.flavor = a, mem.on = false.
    let r = run(&repo, &machine(&[]), true);
    assert!(r.errors.is_empty(), "{:#?}", r.errors);
    assert!(!r.get("ext").unwrap().is_active());
    assert!(!r.get("mem").unwrap().is_active());

    let m = machine(&[
        ("tool", "flavor", serde_json::json!("b")),
        ("mem", "on", serde_json::json!(true)),
    ]);
    let r = run(&repo, &m, true);
    assert!(r.errors.is_empty(), "{:#?}", r.errors);
    assert!(r.get("ext").unwrap().is_active());
    assert!(r.get("mem").unwrap().is_active());
}

#[test]
fn without_a_settings_source_a_setting_reference_is_an_error() {
    let (_d, repo) = setup();
    let r = run(&repo, &machine(&[]), false);
    assert!(!r.errors.is_empty());
}
