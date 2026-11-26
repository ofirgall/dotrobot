//! Package-resolution scenarios (tasks 2.6-2.8), driven by fixture repos and FakeProvider.

use std::collections::BTreeMap;
use std::path::PathBuf;

use dots_facts::{Chain, Facts};
use dots_model::{MachineConfig, Repo};
use dots_providers::{Availability, Caps, Installed, Provider, Registry};
use dots_resolve::{
    CandidateOutcome, EdgeKind, Inputs, InstalledMap, PackageState, Ranking, Resolution, Step,
    resolve,
};
use dots_testkit::FakeProvider;

fn fixture(name: &str) -> Repo {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name);
    match Repo::load(&root) {
        Ok(l) => l.value,
        Err(e) => panic!("fixture {name} does not load: {:#?}", e.errors),
    }
}

fn debian(sudo: bool) -> Facts {
    Facts {
        os: "linux".into(),
        arch: "x86_64".into(),
        distro: Some("debian".into()),
        has_sudo: sudo,
        ..Default::default()
    }
}

fn machine(bundles: &[&str]) -> MachineConfig {
    MachineConfig {
        bundles: bundles.iter().map(|s| s.to_string()).collect(),
        ..Default::default()
    }
}

fn needs_root() -> Caps {
    Caps {
        needs_root: true,
        ..FakeProvider::new("x").caps()
    }
}

/// apt (needs root) + cargo + brew + script, all ready.
fn registry() -> Registry {
    Registry::new()
        .with(Box::new(FakeProvider::new("apt").with_caps(needs_root())))
        .with(Box::new(FakeProvider::new("cargo")))
        .with(Box::new(FakeProvider::new("brew")))
        .with(Box::new(FakeProvider::new("script")))
}

struct Case {
    repo: Repo,
    machine: MachineConfig,
    facts: Facts,
    registry: Registry,
    installed: InstalledMap,
}

impl Case {
    fn new(fixture_name: &str, bundles: &[&str]) -> Case {
        Case {
            repo: fixture(fixture_name),
            machine: machine(bundles),
            facts: debian(true),
            registry: registry(),
            installed: BTreeMap::new(),
        }
    }

    fn run(&self) -> Resolution {
        let chain = Chain::build(&self.facts, &BTreeMap::new(), &self.machine.platform_extra);
        resolve(&Inputs {
            repo: &self.repo,
            machine: &self.machine,
            facts: &self.facts,
            chain: &chain,
            providers: &self.registry,
            installed: &self.installed,
        })
    }
}

fn installed(provider: &str, name: &str) -> InstalledMap {
    let mut m = BTreeMap::new();
    m.insert(
        name.to_string(),
        Installed {
            version: "1.0.0".into(),
            pinned: false,
            source: None,
        },
    );
    BTreeMap::from([(provider.to_string(), m)])
}

// ------------------------------------------------------------------ 2.6

#[test]
fn transitive_requirement_is_in_the_set_with_its_path() {
    let r = Case::new("main", &["kv"]).run();
    assert!(r.is_ok(), "{:#?}", r.errors);
    let rust = r.get("rust").expect("rust is in the set");
    assert!(rust.is_active());
    let paths: Vec<String> = rust.inclusions.iter().map(|i| i.display()).collect();
    assert_eq!(paths, ["machine → kv → kv → requires rust >=1.80"]);

    let r = Case::new("main", &["cli"]).run();
    let paths: Vec<String> = r
        .get("rust")
        .unwrap()
        .inclusions
        .iter()
        .map(|i| i.display())
        .collect();
    assert_eq!(paths, ["machine → cli → jless → requires rust >=1.80"]);
    // The requirement's version spec is kept as a constraint with its provenance.
    let c = &r.get("rust").unwrap().constraints.constraints;
    assert_eq!(c.len(), 1);
    assert_eq!(c[0].spec, ">=1.80");
    // rust installs before jless.
    let pos = |n: &str| r.order.iter().position(|o| o == n).unwrap();
    assert!(pos("rust") < pos("jless"));
}

#[test]
fn duplicate_inclusion_is_one_package_with_every_path() {
    // cli lists ripgrep and includes base, which lists it too; the machine also lists base.
    let r = Case::new("main", &["cli", "base"]).run();
    assert!(r.is_ok(), "{:#?}", r.errors);
    assert_eq!(r.order.iter().filter(|n| *n == "ripgrep").count(), 1);
    let mut paths: Vec<String> = r
        .get("ripgrep")
        .unwrap()
        .inclusions
        .iter()
        .map(|i| i.display())
        .collect();
    paths.sort();
    assert_eq!(
        paths,
        [
            "machine → base → ripgrep",
            "machine → cli → base → ripgrep",
            "machine → cli → ripgrep",
        ]
    );
}

#[test]
fn excluded_package_is_listed_but_not_in_the_order() {
    let mut c = Case::new("main", &["cli"]);
    c.machine.exclude = vec!["spotify".into()];
    let r = c.run();
    assert!(r.is_ok(), "{:#?}", r.errors);
    assert_eq!(r.get("spotify").unwrap().state, PackageState::Excluded);
    assert!(!r.order.iter().any(|n| n == "spotify"));
    assert!(r.get("spotify").unwrap().provider.is_none());
}

#[test]
fn excluding_something_an_active_package_requires_is_an_error() {
    let mut c = Case::new("main", &["cli"]);
    c.machine.exclude = vec!["rust".into()];
    let r = c.run();
    assert!(
        r.error_codes().contains(&"excluded-dependency"),
        "{:#?}",
        r.errors
    );
}

#[test]
fn false_when_makes_a_package_inert() {
    let r = Case::new("main", &["cli"]).run();
    match &r.get("macapp").unwrap().state {
        PackageState::Inert { condition } => assert_eq!(condition, "os = \"macos\""),
        s => panic!("{s:?}"),
    }
    assert!(!r.order.iter().any(|n| n == "macapp"));
    // A member `when` that is false makes it inert too, and names the condition.
    match &r.get("remote-only").unwrap().state {
        PackageState::Inert { condition } => assert_eq!(condition, "tag = \"remote\""),
        s => panic!("{s:?}"),
    }
}

#[test]
fn a_package_is_active_if_any_path_condition_is_true() {
    let mut c = Case::new("main", &["gated", "gated-alt"]);
    c.machine.tags = vec![];
    let r = c.run();
    let g = r.get("gated").unwrap();
    assert!(g.is_active(), "{:?}", g.state);
    assert_eq!(g.inclusions.len(), 2);
    assert_eq!(g.inclusions.iter().filter(|i| i.active).count(), 1);

    // Only the conditional path: inert.
    let r = Case::new("main", &["gated"]).run();
    assert!(matches!(
        r.get("gated").unwrap().state,
        PackageState::Inert { .. }
    ));
}

#[test]
fn unknown_package_suggests_the_closest_name() {
    let r = Case::new("main", &["typo"]).run();
    let d = r
        .errors
        .iter()
        .find(|d| d.code == "unknown-package")
        .expect("error");
    assert!(d.message.contains("ripgerp"), "{}", d.message);
    assert!(d.hint.contains("`ripgrep`"), "{}", d.hint);
    assert_eq!(d.provenance[0].file.as_deref(), Some("bundles/typo.toml"));
    // `packages = ["ripgerp"]`: the string starts at line 2, column 13.
    assert_eq!(
        (d.provenance[0].line, d.provenance[0].col),
        (Some(2), Some(13))
    );
}

#[test]
fn unknown_bundle_and_bundle_cycle() {
    let r = Case::new("main", &["clii"]).run();
    let d = r
        .errors
        .iter()
        .find(|d| d.code == "unknown-bundle")
        .expect("error");
    assert!(d.hint.contains("`cli`"), "{}", d.hint);

    let r = Case::new("main", &["loop-a"]).run();
    let d = r
        .errors
        .iter()
        .find(|d| d.code == "dependency-cycle")
        .expect("cycle");
    assert!(
        d.message.contains("loop-a → loop-b → loop-a"),
        "{}",
        d.message
    );
}

// ------------------------------------------------------------------ 2.7

#[test]
fn priority_of_the_most_specific_platform_key_applies() {
    // debian: apt first.
    let r = Case::new("main", &["base"]).run();
    let bat = r.get("ripgrep").unwrap().provider.as_ref().unwrap();
    assert_eq!(bat.chosen.as_ref().unwrap().id, "apt");
    assert_eq!(bat.ranking, Ranking::Priority("debian".into()));

    // ubuntu 22.04 has its own list (cargo first), more specific than debian's.
    let mut c = Case::new("main", &["base"]);
    c.facts = Facts {
        os: "linux".into(),
        arch: "x86_64".into(),
        distro: Some("ubuntu".into()),
        distro_version: Some("22.04".into()),
        id_like: vec!["debian".into()],
        has_sudo: true,
        ..Default::default()
    };
    let r = c.run();
    let sel = r.get("ripgrep").unwrap().provider.as_ref().unwrap();
    assert_eq!(sel.chosen.as_ref().unwrap().id, "cargo");
    assert_eq!(sel.ranking, Ranking::Priority("ubuntu-22.04".into()));
}

#[test]
fn prefer_overrides_the_priority_list() {
    let r = Case::new("main", &["base"]).run();
    let sel = r.get("bat").unwrap().provider.as_ref().unwrap();
    assert_eq!(sel.chosen.as_ref().unwrap().id, "cargo");
    assert_eq!(sel.ranking, Ranking::Prefer);
}

#[test]
fn no_sudo_falls_back_and_says_why() {
    let mut c = Case::new("main", &["cli"]);
    c.facts = debian(false);
    let r = c.run();
    let sel = r.get("fd").unwrap().provider.as_ref().unwrap();
    assert_eq!(sel.chosen.as_ref().unwrap().id, "cargo");
    assert!(
        sel.reasons().iter().any(|l| l == "apt skipped: needs root"),
        "{:?}",
        sel.reasons()
    );
    assert_eq!(sel.chosen.as_ref().unwrap().native_name, "fd-find");
}

#[test]
fn unusable_providers_are_skipped_with_a_reason() {
    let mut c = Case::new("main", &["script"]);
    // brew/cargo not registered: the fixture's providers are apt (when false) and cargo.
    c.registry = Registry::new()
        .with(Box::new(FakeProvider::new("apt")))
        .with(Box::new(
            FakeProvider::new("cargo").with_availability(Availability::Unsupported),
        ))
        .with(Box::new(FakeProvider::new("script")));
    let r = c.run();
    let sel = r.get("whenprov").unwrap().provider.as_ref().unwrap();
    assert!(sel.chosen.is_none());
    assert!(
        r.error_codes().contains(&"no-provider-satisfies"),
        "{:#?}",
        r.errors
    );
    let lines: Vec<String> = sel.candidates.iter().map(|c| c.line()).collect();
    assert!(
        lines
            .iter()
            .any(|l| l.starts_with("apt skipped: its `when` is false")),
        "{lines:?}"
    );
    assert!(
        lines
            .iter()
            .any(|l| l == "cargo skipped: not available on this machine"),
        "{lines:?}"
    );
    // The error's hint lists what each candidate said.
    let d = r
        .errors
        .iter()
        .find(|d| d.code == "no-provider-satisfies")
        .unwrap();
    assert!(d.hint.contains("cargo skipped"), "{}", d.hint);
}

#[test]
fn unregistered_providers_report_not_implemented() {
    let mut c = Case::new("main", &["base"]);
    c.registry = Registry::new();
    let r = c.run();
    let sel = r.get("ripgrep").unwrap().provider.as_ref().unwrap();
    assert!(sel.candidates.iter().all(|c| matches!(
        &c.outcome,
        CandidateOutcome::Skipped(s) if s.to_string() == "not implemented in this build"
    )));
}

#[test]
fn cargo_without_rustup_injects_the_bootstrap_package() {
    let mut c = Case::new("main", &["cargo"]);
    c.registry = Registry::new()
        .with(Box::new(FakeProvider::new("cargo").with_availability(
            Availability::Bootstrap {
                pkg: "rustup".into(),
            },
        )))
        .with(Box::new(FakeProvider::new("script")));
    let r = c.run();
    assert!(r.is_ok(), "{:#?}", r.errors);
    let choice = r.get("cargotool").unwrap().chosen_provider().unwrap();
    assert_eq!(choice.id, "cargo");
    assert_eq!(choice.bootstrap.as_deref(), Some("rustup"));
    let rustup = r.get("rustup").expect("rustup injected");
    assert!(rustup.is_active());
    assert!(matches!(
        rustup.inclusions[0].steps.last(),
        Some(Step::Bootstrap { .. })
    ));
    assert_eq!(rustup.chosen_provider().unwrap().id, "script");
    let pos = |n: &str| r.order.iter().position(|o| o == n).unwrap();
    assert!(pos("rustup") < pos("cargotool"), "{:?}", r.order);
    assert!(r.edges.iter().any(|e| e.package == "cargotool"
        && e.dependency == "rustup"
        && e.kind == EdgeKind::Bootstrap));
}

#[test]
fn bootstrap_package_that_cannot_be_added_skips_the_provider() {
    let mut c = Case::new("main", &["cargo"]);
    c.machine.exclude = vec!["rustup".into()];
    c.registry = Registry::new().with(Box::new(FakeProvider::new("cargo").with_availability(
        Availability::Bootstrap {
            pkg: "rustup".into(),
        },
    )));
    let r = c.run();
    let sel = r.get("cargotool").unwrap().provider.as_ref().unwrap();
    assert!(sel.chosen.is_none());
    assert!(
        sel.reasons()[0].contains("needs bootstrap package `rustup`"),
        "{:?}",
        sel.reasons()
    );
    assert!(r.error_codes().contains(&"no-provider-satisfies"));
}

#[test]
fn existing_apt_install_stays_selected_with_a_migrate_hint() {
    let mut c = Case::new("main", &["base"]);
    c.installed = installed("apt", "bat");
    let r = c.run();
    let sel = r.get("bat").unwrap().provider.as_ref().unwrap();
    let chosen = sel.chosen.as_ref().unwrap();
    assert_eq!((chosen.id.as_str(), chosen.sticky), ("apt", true));
    assert_eq!(sel.hint.as_deref(), Some("dots migrate bat --to cargo"));
    // cargo (preferred) stays a usable standby.
    assert!(
        sel.candidates
            .iter()
            .any(|c| c.id == "cargo" && c.outcome == CandidateOutcome::Standby)
    );

    // Installed by the provider that is also preferred: no hint, not marked sticky.
    let mut c = Case::new("main", &["base"]);
    c.installed = installed("cargo", "bat");
    let r = c.run();
    let sel = r.get("bat").unwrap().provider.as_ref().unwrap();
    assert!(!sel.chosen.as_ref().unwrap().sticky);
    assert!(sel.hint.is_none());
}

#[test]
fn sticky_uses_the_provider_native_name() {
    // fd is `fd-find` in apt; an install under that name is found.
    let mut c = Case::new("main", &["cli"]);
    c.facts = Facts {
        os: "linux".into(),
        arch: "x86_64".into(),
        distro: Some("ubuntu".into()),
        distro_version: Some("22.04".into()),
        id_like: vec!["debian".into()],
        has_sudo: true,
        ..Default::default()
    };
    c.installed = installed("apt", "fd-find");
    let r = c.run();
    let sel = r.get("fd").unwrap().provider.as_ref().unwrap();
    assert_eq!(sel.chosen.as_ref().unwrap().id, "apt");
    assert_eq!(sel.hint.as_deref(), Some("dots migrate fd --to cargo"));
}

#[test]
fn files_only_package_needs_no_provider() {
    let r = Case::new("main", &["files"]).run();
    assert!(r.is_ok(), "{:#?}", r.errors);
    let p = r.get("dotfiles-only").unwrap();
    assert!(p.is_active() && p.chosen_provider().is_none());
}

#[test]
fn package_with_only_files_and_no_providers_or_bin_raises_no_error() {
    let r = Case::new("main", &["bare-files"]).run();
    assert!(r.is_ok(), "{:#?}", r.errors);
    assert!(r.errors.is_empty() && r.warnings.is_empty(), "{r:#?}");
    let p = r.get("bare-files").unwrap();
    assert!(p.is_active() && p.chosen_provider().is_none());
}

/// A package with no providers/bin but other content (a `manual/` dir, hooks, ...) is a meta
/// package: no provider installs it (it used to become `apt install <name>`). A bare
/// `schema` + `name` package still gets the default provider chain.
#[test]
fn meta_package_without_install_target_is_not_installed_by_every_provider() {
    let dir = tempfile::tempdir().unwrap();
    let w = |rel: &str, text: &str| {
        let p = dir.path().join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    };
    w("dots.toml", "schema = 1\n[priority]\nany = [\"apt\"]\n");
    w(
        "bundles/b.toml",
        "schema = 1\npackages = [\"serial-access\", \"hooked\", \"ripgrep\"]\n",
    );
    w(
        "packages/serial-access/package.toml",
        "schema = 1\nname = \"serial-access\"\n",
    );
    w(
        "packages/serial-access/manual/dialout.md",
        "# add yourself to dialout\n",
    );
    w(
        "packages/hooked/package.toml",
        "schema = 1\nname = \"hooked\"\n[hooks]\npost_files = \"true\"\n",
    );
    w(
        "packages/ripgrep/package.toml",
        "schema = 1\nname = \"ripgrep\"\n",
    );
    let c = Case {
        repo: Repo::load(dir.path()).unwrap().value,
        machine: machine(&["b"]),
        facts: debian(true),
        registry: registry(),
        installed: BTreeMap::new(),
    };
    let r = c.run();
    assert!(r.is_ok(), "{:#?}", r.errors);
    for meta in ["serial-access", "hooked"] {
        let p = r.get(meta).unwrap();
        assert!(p.is_active() && p.chosen_provider().is_none(), "{meta}");
    }
    assert!(r.get("ripgrep").unwrap().chosen_provider().is_some());
}

// ------------------------------------------------------------------ 2.8

#[test]
fn replaces_drops_the_other_package_and_its_dependencies() {
    let r = Case::new("collide", &["replaces"]).run();
    assert!(r.is_ok(), "{:#?}", r.errors);
    assert_eq!(
        r.get("vim").unwrap().state,
        PackageState::Replaced {
            by: "neovim".into()
        }
    );
    assert_eq!(r.replaced.len(), 1);
    assert_eq!(
        (r.replaced[0].package.as_str(), r.replaced[0].by.as_str()),
        ("vim", "neovim")
    );
    assert!(!r.order.iter().any(|n| n == "vim"));
    // vim's own requirement is no longer pulled in...
    assert!(r.get("vim-runtime").is_none_or(|p| !p.is_active()));
    // ...and a dependent of vim is now ordered after neovim.
    let pos = |n: &str| r.order.iter().position(|o| o == n).unwrap();
    assert!(pos("neovim") < pos("plugin"));
}

#[test]
fn binary_collision_is_an_error_naming_both() {
    let r = Case::new("collide", &["bins"]).run();
    let d = r
        .errors
        .iter()
        .find(|d| d.code == "binary-collision")
        .expect("error");
    assert!(
        d.message.contains("`x`") && d.message.contains("tool-a") && d.message.contains("tool-b"),
        "{}",
        d.message
    );
    assert_eq!(d.provenance.len(), 2);
}

#[test]
fn provider_shim_counts_as_a_claimed_binary() {
    // fd (apt) shims `fd`; a second package claiming `fd` collides.
    let mut c = Case::new("main", &["cli"]);
    // Make `zed` claim fd as well.
    let mut zed = c.repo.packages["zed"].clone();
    zed.bin = Some("fd".into());
    c.repo.packages.insert("zed".into(), zed);
    let r = c.run();
    assert!(
        r.error_codes().contains(&"binary-collision"),
        "{:#?}",
        r.errors
    );
}

#[test]
fn conflicts_make_selecting_both_an_error() {
    let r = Case::new("collide", &["conflicts"]).run();
    let d = r
        .errors
        .iter()
        .find(|d| d.code == "package-conflict")
        .expect("error");
    assert!(
        d.message.contains("`left`") && d.message.contains("`right`"),
        "{}",
        d.message
    );
    // Selecting only one is fine.
    let mut c = Case::new("collide", &["conflicts"]);
    c.machine.exclude = vec!["right".into()];
    assert!(c.run().is_ok());
}

#[test]
fn two_node_cycle_prints_the_cycle() {
    let r = Case::new("cycle", &["loop"]).run();
    let d = r
        .errors
        .iter()
        .find(|d| d.code == "dependency-cycle")
        .expect("cycle");
    assert!(d.message.ends_with("a → b → a"), "{}", d.message);
    // Points at `requires = ["b"]` in a's package.toml.
    assert_eq!(
        d.provenance[0].file.as_deref(),
        Some("packages/a/package.toml")
    );
    assert_eq!(
        (d.provenance[0].line, d.provenance[0].col),
        (Some(3), Some(13))
    );
    assert_eq!(
        r.errors
            .iter()
            .filter(|d| d.code == "dependency-cycle")
            .count(),
        1
    );
}

#[test]
fn after_edges_take_part_in_cycle_detection_and_ordering() {
    let r = Case::new("cycle", &["afterloop"]).run();
    let d = r
        .errors
        .iter()
        .find(|d| d.code == "dependency-cycle")
        .expect("cycle");
    assert!(d.message.ends_with("c → d → c"), "{}", d.message);

    // `zed` is ordered after `bat` (after = ["bat"]) even though bat is only in base.
    let r = Case::new("main", &["cli"]).run();
    let pos = |n: &str| r.order.iter().position(|o| o == n).unwrap();
    assert!(pos("bat") < pos("zed"));
    // `after` does not include: a package that is not in the set is ignored.
    assert!(
        r.edges
            .iter()
            .any(|e| e.kind == EdgeKind::After && e.package == "zed")
    );
}

#[test]
fn order_is_deterministic() {
    let a = Case::new("main", &["cli", "kv"]).run().order;
    let b = Case::new("main", &["kv", "cli"]).run().order;
    let mut sa = a.clone();
    let mut sb = b.clone();
    sa.sort();
    sb.sort();
    assert_eq!(sa, sb);
    assert_eq!(a, Case::new("main", &["cli", "kv"]).run().order);
}
