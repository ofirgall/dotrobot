//! platform-conditions scenarios (tasks 2.3 - 2.5).

use std::collections::{BTreeMap, BTreeSet};

use dots_facts::*;

const UBUNTU: &str = "PRETTY_NAME=\"Ubuntu 24.04 LTS\"\nNAME=\"Ubuntu\"\nVERSION_ID=\"24.04\"\nID=ubuntu\nID_LIKE=debian\n";
const MINT: &str =
    "NAME=\"Linux Mint\"\nVERSION_ID=\"22.1\"\nID=linuxmint\nID_LIKE=\"ubuntu debian\"\n";
const POP: &str = "NAME=\"Pop!_OS\"\nVERSION_ID=\"22.04\"\nID=pop\nID_LIKE=\"ubuntu debian\"\n";
const POP_NO_LIKE: &str = "NAME=\"Pop!_OS\"\nVERSION_ID=\"22.04\"\nID=pop\n";

fn linux(os_release: &str) -> FakeSources {
    FakeSources::new("linux")
        .file("/etc/os-release", os_release)
        .command("uname -m", 0, "x86_64\n")
        .command("id -u", 0, "1000\n")
        .command("sudo -n true", 0, "")
        .command("id -Gn", 0, "me users\n")
        .command("hostname", 0, "box\n")
        .env("USER", "me")
        .env("HOME", "/home/me")
        .env("SHELL", "/usr/bin/zsh")
        .env("DISPLAY", ":0")
}

fn facts(s: &FakeSources) -> Facts {
    detect(s, &FactOverrides::default())
}

fn chain_of(f: &Facts) -> Chain {
    Chain::build(f, &BTreeMap::new(), &[])
}

#[test]
fn detects_linux_facts_with_sources() {
    let f = facts(&linux(UBUNTU));
    assert_eq!(f.os, "linux");
    assert_eq!(f.arch, "x86_64");
    assert_eq!(f.distro.as_deref(), Some("ubuntu"));
    assert_eq!(f.distro_family.as_deref(), Some("debian"));
    assert_eq!(f.distro_version.as_deref(), Some("24.04"));
    assert!(f.has_sudo && f.gui && !f.is_wsl);
    assert_eq!((f.user.as_str(), f.shell.as_str()), ("me", "zsh"));
    assert_eq!(f.hostname, "box");
    assert!(f.entries().iter().all(|(_, _, s)| *s == Source::Detected));
}

#[test]
fn wsl2_detected_in_any_case() {
    for text in ["5.15.90.1-microsoft-standard-WSL2\n", "4.4.0-Microsoft\n"] {
        let f = facts(&linux(UBUNTU).file("/proc/sys/kernel/osrelease", text));
        assert!(f.is_wsl, "{text}");
        assert!(chain_of(&f).levels().ends_with(&["wsl".to_string()]));
    }
    assert!(!facts(&linux(UBUNTU).file("/proc/sys/kernel/osrelease", "6.8.0-generic")).is_wsl);
}

#[test]
fn forced_no_sudo_beats_working_sudo() {
    let o = FactOverrides {
        sudo: Some(false),
        ..Default::default()
    };
    let f = detect(&linux(UBUNTU), &o);
    assert!(!f.has_sudo);
    assert_eq!(f.source_of("sudo"), Source::Overridden);
    assert_eq!(f.source_of("gui"), Source::Detected);
    let sudo = f.entries().into_iter().find(|e| e.0 == "sudo").unwrap();
    assert_eq!(sudo.2.label(), "machine.toml");
}

#[test]
fn sudo_false_without_sudo_and_true_for_root() {
    let s = FakeSources::new("linux").command("uname -m", 0, "x86_64");
    assert!(!facts(&s).has_sudo);
    let s = s.command("id -u", 0, "0\n");
    assert!(facts(&s).has_sudo);
}

#[test]
fn sudo_via_admin_group_without_cached_credentials() {
    for (groups, want, note) in [
        ("me adm sudo", true, Some("group sudo")),
        ("me staff admin", true, Some("group admin")),
        ("me wheel", true, Some("group wheel")),
        ("me users", false, None),
    ] {
        let s = FakeSources::new("linux")
            .command("uname -m", 0, "x86_64")
            .command("id -u", 0, "1000")
            .command("sudo -n true", 1, "")
            .command("id -Gn", 0, groups);
        let f = facts(&s);
        assert_eq!(f.has_sudo, want, "{groups}");
        assert_eq!(f.notes.get("sudo").map(String::as_str), note, "{groups}");
    }
    let s = FakeSources::new("linux")
        .command("id -u", 0, "1000")
        .command("sudo -n true", 1, "")
        .command("id -Gn", 0, "me admin");
    assert_eq!(facts(&s).source_label("sudo"), "detected (group admin)");
    // The override still wins over group membership.
    let o = FactOverrides {
        sudo: Some(false),
        ..Default::default()
    };
    let f = detect(&s, &o);
    assert!(!f.has_sudo);
    assert_eq!(f.source_label("sudo"), "machine.toml");
}

#[test]
fn rosetta_arch_is_runtime() {
    let s = FakeSources::new("macos")
        .command("uname -m", 0, "x86_64\n")
        .command("sw_vers -productVersion", 0, "15.2\n");
    assert_eq!(facts(&s).arch, "x86_64");
    let s = FakeSources::new("macos").command("uname -m", 0, "arm64\n");
    assert_eq!(facts(&s).arch, "aarch64");
}

#[test]
fn ubuntu_chain() {
    let f = facts(&linux(UBUNTU));
    let c = chain_of(&f);
    assert_eq!(
        c.pretty(),
        "any › unix › linux › debian › ubuntu › ubuntu-24 › ubuntu-24.04 (x86_64)"
    );
    assert_eq!(
        &c.ordered()[..6],
        [
            "any",
            "any/x86_64",
            "unix",
            "unix/x86_64",
            "linux",
            "linux/x86_64"
        ]
    );
}

#[test]
fn mint_chain_has_ubuntu_before_linuxmint() {
    let c = chain_of(&facts(&linux(MINT)));
    assert_eq!(
        c.levels(),
        [
            "any",
            "unix",
            "linux",
            "debian",
            "ubuntu",
            "linuxmint",
            "linuxmint-22",
            "linuxmint-22.1"
        ]
    );
}

#[test]
fn custom_platform_parent_inherits_full_ancestry() {
    // Pop with its ID_LIKE missing, and a custom parent declared.
    let plat = BTreeMap::from([("pop".to_string(), "ubuntu".to_string())]);
    let c = Chain::build(&facts(&linux(POP_NO_LIKE)), &plat, &[]);
    assert_eq!(
        c.levels()[..6],
        ["any", "unix", "linux", "debian", "ubuntu", "pop"]
    );
    // An unknown distro with a custom parent also gets the parent's lineage (from the table).
    let f = facts(&linux("ID=mydistro\nVERSION_ID=1\n"));
    let plat = BTreeMap::from([("mydistro".to_string(), "ubuntu".to_string())]);
    let c = Chain::build(&f, &plat, &[]);
    assert_eq!(
        c.levels()[..6],
        ["any", "unix", "linux", "debian", "ubuntu", "mydistro"]
    );
    // Custom parents chain through custom platforms too.
    let plat = BTreeMap::from([
        ("mydistro".to_string(), "spin".to_string()),
        ("spin".to_string(), "linuxmint".to_string()),
    ]);
    let c = Chain::build(&f, &plat, &[]);
    assert_eq!(
        c.levels()[2..8],
        ["linux", "debian", "ubuntu", "linuxmint", "spin", "mydistro"]
    );
    // Natural ID_LIKE already has it: no duplicates.
    let plat = BTreeMap::from([("pop".to_string(), "ubuntu".to_string())]);
    let c = Chain::build(&facts(&linux(POP)), &plat, &[]);
    assert_eq!(c.levels().iter().filter(|k| *k == "ubuntu").count(), 1);
}

#[test]
fn missing_id_like_is_completed_from_known_lineage() {
    let c = chain_of(&facts(&linux(POP_NO_LIKE)));
    assert_eq!(
        c.levels()[..6],
        ["any", "unix", "linux", "debian", "ubuntu", "pop"]
    );
    let f = facts(&linux("ID=manjaro\nVERSION_ID=24\n"));
    assert_eq!(f.distro_family.as_deref(), Some("arch"));
}

#[test]
fn platform_extra_goes_last_with_parent() {
    let f = facts(&linux(UBUNTU));
    let plat = BTreeMap::from([("steamdeck".to_string(), "arch".to_string())]);
    let c = Chain::build(&f, &plat, &["steamdeck".into()]);
    let l = c.levels();
    assert_eq!(l[l.len() - 2..], ["arch", "steamdeck"]);
}

#[test]
fn macos_chain_via_sw_vers() {
    let s = FakeSources::new("macos")
        .command("uname -m", 0, "arm64\n")
        .command("sw_vers -productVersion", 0, "15.2\n");
    let f = facts(&s);
    assert!(f.gui);
    assert_eq!(
        chain_of(&f).pretty(),
        "any › unix › macos › macos-15 › macos-15.2 (aarch64)"
    );
}

#[test]
fn windows_chain() {
    let s = FakeSources::new("windows")
        .command("uname -m", 0, "")
        .env("PROCESSOR_ARCHITECTURE", "AMD64")
        .command(
            "cmd /c ver",
            0,
            "\nMicrosoft Windows [Version 10.0.22631.3007]\n",
        );
    // `ver` prints a leading blank line on real systems; detection takes the first non-empty line.
    let f = facts(&s);
    assert_eq!(f.distro_version.as_deref(), Some("11"));
    assert_eq!(chain_of(&f).pretty(), "any › windows › windows-11 (x86_64)");
    let s = FakeSources::new("windows").command(
        "cmd /c ver",
        0,
        "Microsoft Windows [Version 10.0.19045.1]\n",
    );
    assert_eq!(facts(&s).distro_version.as_deref(), Some("10"));
}

// ---- conditions ----

struct World {
    facts: Facts,
    chain: Chain,
    tags: BTreeSet<String>,
    declared: BTreeSet<String>,
    settings: MapSettings,
}

impl World {
    fn new(src: &FakeSources) -> World {
        let facts = facts(src);
        let chain = chain_of(&facts);
        World {
            facts,
            chain,
            tags: ["work".to_string()].into(),
            declared: ["work", "remote", "personal"].map(String::from).into(),
            settings: MapSettings::default()
                .with(None, "font_size", Value::Num(14.0))
                .with(None, "cursor_shader", Value::Bool(true))
                .with(Some("ghostty"), "theme", Value::Str("light".into())),
        }
    }
    fn eval(&self, src: &str) -> Result<bool, CondError> {
        let c = Condition::parse(src)?;
        c.eval(&EvalCtx {
            facts: &self.facts,
            chain: &self.chain,
            tags: &self.tags,
            declared_tags: &self.declared,
            settings: &self.settings,
        })
    }
}

#[test]
fn is_a_platform_on_mint() {
    let w = World::new(&linux(MINT));
    assert!(w.eval(r#"platform = "ubuntu""#).unwrap());
    assert!(w.eval(r#"platform = "linux/x86_64""#).unwrap());
    assert!(!w.eval(r#"platform != "ubuntu""#).unwrap());
    assert!(!w.eval(r#"platform = "fedora""#).unwrap());
}

#[test]
fn version_comparison_on_2204() {
    let ubuntu2204 = UBUNTU.replace("24.04", "22.04");
    let w = World::new(&linux(&ubuntu2204));
    assert!(
        !w.eval(r#"all(platform = "ubuntu", distro_version >= "24.04")"#)
            .unwrap()
    );
    assert!(
        w.eval(r#"all(platform = "ubuntu", distro_version < "24.04")"#)
            .unwrap()
    );
    assert!(
        w.eval(r#"distro_version >= "9.1""#).unwrap(),
        "numeric segments, not text"
    );
}

#[test]
fn facts_bool_and_combinators() {
    let w = World::new(&linux(UBUNTU));
    assert!(
        w.eval(r#"all(os = "linux", not(tag = "remote"), sudo)"#)
            .unwrap()
    );
    assert!(
        w.eval(r#"any(distro = "ubuntu", distro = "debian")"#)
            .unwrap()
    );
    assert!(w.eval("all()").unwrap());
    assert!(!w.eval("any()").unwrap());
    assert!(w.eval("true").unwrap());
    assert!(!w.eval("not(true)").unwrap());
    assert!(w.eval(r#"shell != "bash""#).unwrap());
    assert!(w.eval(r#"tag = "work""#).unwrap());
    assert!(w.eval(r#"any(wsl, gui,)"#).unwrap(), "trailing comma");
}

#[test]
fn unknown_tag_suggests() {
    let w = World::new(&linux(UBUNTU));
    let e = w.eval(r#"tag = "remtoe""#).unwrap_err();
    assert_eq!(e.code, "unknown-tag");
    assert_eq!(e.suggestion.as_deref(), Some("remote"));
    let d = e.to_diagnostic(Some("dots.toml"), Some(3));
    assert!(d.hint.contains("`remote`"));
    assert_eq!(d.provenance[0].col, Some(7));
    // Even inside a short-circuiting branch it is an error, never false.
    assert_eq!(
        w.eval(r#"any(true, tag = "nope")"#).unwrap_err().code,
        "unknown-tag"
    );
}

#[test]
fn unknown_fact_is_an_error_with_suggestion() {
    let e = Condition::parse(r#"all(os = "linux", distro_famly = "x")"#).unwrap_err();
    assert_eq!(e.code, "unknown-fact");
    assert_eq!(e.col, Some(19));
    assert_eq!(e.suggestion.as_deref(), Some("distro_family"));
    let d = e.to_diagnostic(None, None);
    assert!(d.hint.contains("Did you mean `distro_family`"));
    assert_eq!(
        Condition::parse("bogus")
            .unwrap_err()
            .to_diagnostic(None, None)
            .hint,
        "Run `dots facts` to see the available facts."
    );
}

#[test]
fn provider_facts_use_the_hook() {
    let mut w = World::new(&linux(UBUNTU));
    w.facts.set_provider("apt", true, Source::Detected);
    w.facts.set_provider("brew", false, Source::Detected);
    assert!(w.eval("provider.apt").unwrap());
    assert!(!w.eval("provider.brew").unwrap());
    assert_eq!(w.eval("provider.pacman").unwrap_err().code, "unknown-fact");
}

#[test]
fn settings_and_setting_ref_check() {
    let w = World::new(&linux(UBUNTU));
    assert!(w.eval("setting.font_size >= 14").unwrap());
    assert!(!w.eval("setting.font_size > 14").unwrap());
    assert!(w.eval("setting.cursor_shader").unwrap());
    assert!(w.eval(r#"settings.ghostty.theme = "light""#).unwrap());
    let e = w.eval(r#"settings.kitty.theme = "light""#).unwrap_err();
    assert_eq!(e.code, "setting-ref-unresolved");
    assert!(e.message.contains("settings.kitty.theme"));

    // The lint-time resolve check, using the caller's fake resolved set.
    let c = Condition::parse(r#"all(settings.ghostty.theme = "light", settings.kitty.font = "x", setting.nope, tag = "wrok")"#).unwrap();
    let errs = c.check(&w.declared, &w.settings);
    let codes: Vec<_> = errs.iter().map(|e| e.code).collect();
    assert_eq!(
        codes,
        [
            "setting-ref-unresolved",
            "setting-ref-unresolved",
            "unknown-tag"
        ]
    );
    assert_eq!(errs[2].suggestion.as_deref(), Some("work"));

    let r = c.references();
    assert!(r.tags.contains("wrok"));
    assert!(r.settings.contains(&SettingRef {
        package: Some("ghostty".into()),
        key: "theme".into()
    }));
    assert!(r.settings.contains(&SettingRef {
        package: None,
        key: "nope".into()
    }));
    let none = Condition::parse("sudo")
        .unwrap()
        .check(&w.declared, &NoSettings);
    assert!(none.is_empty());
}

#[test]
fn parse_errors_carry_columns() {
    let cases = [
        (r#"os = "linux" extra"#, 14),
        (r#"all(os = "linux""#, 17),
        (r#"os = linux"#, 6),
        (r#"os = "lin"#, 6),
        (r#"not(sudo, gui)"#, 1),
        ("", 1),
        ("  sudo )", 8),
        (r#"os ! "x""#, 4),
    ];
    for (src, col) in cases {
        let e = Condition::parse(src).unwrap_err();
        assert_eq!(e.code, "condition-syntax", "{src}");
        assert_eq!(e.col, Some(col), "{src}: {e}");
    }
}

#[test]
fn type_errors() {
    let w = World::new(&linux(UBUNTU));
    assert_eq!(w.eval("os").unwrap_err().code, "condition-type");
    assert_eq!(
        w.eval(r#"platform >= "x""#).unwrap_err().code,
        "condition-type"
    );
    assert_eq!(w.eval(r#"os > "a""#).unwrap_err().code, "condition-type");
}

#[test]
fn version_compare_rules() {
    use std::cmp::Ordering::*;
    assert_eq!(compare_versions("3.10", "3.9"), Some(Greater));
    assert_eq!(compare_versions("15", "15.0.0"), Some(Equal));
    assert_eq!(compare_versions("22.04", "24.04"), Some(Less));
    assert_eq!(compare_versions("1.x", "1"), None);
}
