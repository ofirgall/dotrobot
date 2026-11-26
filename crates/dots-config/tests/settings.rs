//! Task 5.2: every `package-settings` scenario except `questions`, against real repos on disk.

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use dots_config::{Inputs, SettingScope, Why, resolve};
use dots_facts::{Condition, EvalCtx, SettingLookup};
use dots_model::{MachineConfig, OverrideLocation, Repo};
use serde_json::{Value, json};

fn write(root: &Path, rel: &str, text: &str) {
    let p = root.join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, text).unwrap();
}

const GHOSTTY: &str = r#"schema = 1
name = "ghostty"
[settings.font_size]
type = "int"
default = 13
range = [8, 32]
[settings.theme]
type = "enum"
values = ["dark", "light"]
default = "dark"
ask = true
question = "Dark or light?"
[settings.token]
type = "string"
required = true
secret = true
[settings.cursor_shader]
type = "bool"
default = false
when = 'os = "never"'
"#;

fn base(extra: &[(&str, &str)]) -> tempfile::TempDir {
    let d = tempfile::tempdir().unwrap();
    write(d.path(), "dots.toml", "schema = 1\n");
    write(d.path(), "packages/ghostty/package.toml", GHOSTTY);
    for (p, t) in extra {
        write(d.path(), p, t);
    }
    d
}

fn machine(local: &str, profile: Option<&str>, root: &Path, bundles: &[&str]) -> MachineConfig {
    let local_path = root.join("machine-local.toml");
    let mut text = format!("schema = 1\n{local}");
    if !bundles.is_empty() {
        let b: Vec<String> = bundles.iter().map(|b| format!("{b:?}")).collect();
        text = format!("schema = 1\nbundles = [{}]\n{local}", b.join(", "));
    }
    if let Some(p) = profile {
        text = text.replacen("schema = 1\n", &format!("schema = 1\nprofile = {p:?}\n"), 1);
    }
    fs::write(&local_path, text).unwrap();
    MachineConfig::load(root, &local_path, "box").unwrap().value
}

fn never(_: &str) -> Result<bool, String> {
    Ok(false)
}

fn run(root: &Path, m: &MachineConfig) -> dots_config::Settings {
    let repo = Repo::load(root).unwrap().value;
    resolve(&Inputs::new(&repo, m, &never))
}

#[test]
fn neither_default_nor_required_fails_loading() {
    let d = base(&[(
        "packages/bad/package.toml",
        "schema = 1\nname = \"bad\"\n[settings.x]\ntype = \"int\"\n",
    )]);
    let e = Repo::load(d.path()).unwrap_err();
    assert_eq!(e.codes(), vec!["setting-no-default"]);
    assert!(e.errors[0].message.contains("bad.x"));
}

#[test]
fn declarations_are_checked() {
    for (decl, what) in [
        ("type = \"int\"\ndefault = 1\nrequired = true", "both"),
        ("type = \"enum\"\ndefault = \"a\"", "enum without values"),
        (
            "type = \"int\"\ndefault = \"x\"",
            "default of the wrong type",
        ),
        (
            "type = \"string\"\ndefault = \"a\"\nvalidate = \"(\"",
            "bad regex",
        ),
    ] {
        let d = base(&[(
            "packages/bad/package.toml",
            &format!("schema = 1\nname = \"bad\"\n[settings.x]\n{decl}\n"),
        )]);
        let e = Repo::load(d.path()).unwrap_err();
        assert_eq!(e.codes(), vec!["schema-invalid-value"], "{what}");
    }
}

#[test]
fn defaults_are_the_lowest_scope() {
    let d = base(&[]);
    let s = run(d.path(), &machine("", None, d.path(), &[]));
    let fs_ = s.get("ghostty", "font_size").unwrap();
    assert_eq!(fs_.value(), Some(&json!(13)));
    assert_eq!(fs_.scope(), Some(SettingScope::Default));
    assert!(!fs_.answered());
}

#[test]
fn machine_wins_over_repo() {
    let d = base(&[(
        "dots.toml",
        "schema = 1\n[settings.ghostty]\nfont_size = 14\n",
    )]);
    let m = machine("[settings.ghostty]\nfont_size = 15\n", None, d.path(), &[]);
    let s = run(d.path(), &m);
    let st = s.get("ghostty", "font_size").unwrap();
    assert_eq!(st.value(), Some(&json!(15)));
    assert_eq!(st.scope(), Some(SettingScope::Machine));
    assert!(s.errors.is_empty());
}

#[test]
fn all_six_scopes_in_order() {
    let d = base(&[
        (
            "dots.toml",
            "schema = 1\n[settings.ghostty]\nfont_size = 14\n",
        ),
        (
            "bundles/desk.toml",
            "schema = 1\npackages = [\"ghostty\"]\n[settings.ghostty]\nfont_size = 12\n",
        ),
        (
            "machines/box.toml",
            "schema = 1\n[settings.ghostty]\nfont_size = 16\n",
        ),
    ]);
    let repo = Repo::load(d.path()).unwrap().value;
    let upstream = serde_json::from_value(json!({"ghostty": {"font_size": 13}})).unwrap();
    let step = |local: &str| {
        let m = machine(local, None, d.path(), &["desk"]);
        let mut i = Inputs::new(&repo, &m, &never);
        i.upstream = &upstream;
        let s = resolve(&i);
        let st = s.get("ghostty", "font_size").unwrap().clone();
        (
            st.layers.iter().map(|l| l.scope).collect::<Vec<_>>(),
            st.value().cloned(),
            st.scope(),
        )
    };
    let (scopes, v, w) = step("");
    use SettingScope as S;
    assert_eq!(
        scopes,
        vec![S::Default, S::Bundle, S::Upstream, S::Repo, S::Profile]
    );
    assert_eq!((v, w), (Some(json!(16)), Some(S::Profile)));
    let (scopes, v, w) = step("[settings.ghostty]\nfont_size = 20\n");
    assert_eq!(scopes.last(), Some(&S::Machine));
    assert_eq!((v, w), (Some(json!(20)), Some(S::Machine)));
}

#[test]
fn conflicting_bundles_name_both() {
    let d = base(&[
        (
            "bundles/a.toml",
            "schema = 1\n[settings.ghostty]\nfont_size = 13\n",
        ),
        (
            "bundles/b.toml",
            "schema = 1\n[settings.ghostty]\nfont_size = 14\n",
        ),
    ]);
    let s = run(d.path(), &machine("", None, d.path(), &["a", "b"]));
    let e = s
        .errors
        .iter()
        .find(|e| e.code == "setting-conflict")
        .unwrap();
    assert!(
        e.message.contains("bundle `a`") && e.message.contains("bundle `b`"),
        "{}",
        e.message
    );
    // The same value in both is fine, and a higher scope settles a disagreement.
    let same = base(&[
        (
            "bundles/a.toml",
            "schema = 1\n[settings.ghostty]\nfont_size = 13\n",
        ),
        (
            "bundles/b.toml",
            "schema = 1\n[settings.ghostty]\nfont_size = 13\n",
        ),
    ]);
    assert!(
        run(same.path(), &machine("", None, same.path(), &["a", "b"]))
            .errors
            .is_empty()
    );
    let m = machine(
        "[settings.ghostty]\nfont_size = 9\n",
        None,
        d.path(),
        &["a", "b"],
    );
    assert!(
        run(d.path(), &m)
            .errors
            .iter()
            .all(|e| e.code != "setting-conflict"),
        "a machine value settles it"
    );
}

#[test]
fn wrong_type_names_the_file_and_the_type() {
    let d = base(&[(
        "dots.toml",
        "schema = 1\n[settings.ghostty]\nfont_size = \"big\"\n",
    )]);
    let s = run(d.path(), &machine("", None, d.path(), &[]));
    let e = s
        .errors
        .iter()
        .find(|e| e.code == "setting-invalid")
        .unwrap();
    assert!(
        e.message.contains("dots.toml") && e.message.contains("expected int"),
        "{}",
        e.message
    );
    // The bad value is not used.
    assert_eq!(
        s.get("ghostty", "font_size").unwrap().value(),
        Some(&json!(13))
    );
}

#[test]
fn enum_range_and_pattern_violations() {
    let d = base(&[(
        "dots.toml",
        "schema = 1\n[settings.ghostty]\ntheme = \"neon\"\nfont_size = 99\n",
    )]);
    let s = run(d.path(), &machine("", None, d.path(), &[]));
    let msgs: Vec<&str> = s.errors.iter().map(|e| e.message.as_str()).collect();
    assert!(
        msgs.iter()
            .any(|m| m.contains("theme") && m.contains("not one of")),
        "{msgs:?}"
    );
    assert!(
        msgs.iter()
            .any(|m| m.contains("font_size") && m.contains("range")),
        "{msgs:?}"
    );

    let p = "schema = 1\nname = \"git\"\n[settings.email]\ntype = \"string\"\nrequired = true\nvalidate = '^[^@]+@[^@]+$'\n";
    let d = base(&[("packages/git/package.toml", p)]);
    let m = machine("[settings.git]\nemail = \"nope\"\n", None, d.path(), &[]);
    let s = run(d.path(), &m);
    let e = s
        .errors
        .iter()
        .find(|e| e.message.contains("git.email"))
        .unwrap();
    assert!(e.message.contains("machine.toml") && e.message.contains("does not match"));
}

#[test]
fn required_without_value_is_missing_and_a_value_fixes_it() {
    let d = base(&[]);
    let s = run(d.path(), &machine("", None, d.path(), &[]));
    let miss = s.missing("ghostty");
    assert_eq!(miss.len(), 1);
    assert_eq!(miss[0].id(), "ghostty.token");
    let m = machine("[settings.ghostty]\ntoken = \"abc\"\n", None, d.path(), &[]);
    assert!(run(d.path(), &m).missing("ghostty").is_empty());
}

#[test]
fn secrets_only_in_machine_scope_and_redacted() {
    let d = base(&[(
        "dots.toml",
        "schema = 1\n[settings.ghostty]\ntoken = \"leak\"\n",
    )]);
    let s = run(d.path(), &machine("", None, d.path(), &[]));
    let e = s
        .errors
        .iter()
        .find(|e| e.code == "setting-secret-scope")
        .unwrap();
    assert!(e.message.contains("dots.toml"));
    assert!(
        s.get("ghostty", "token").unwrap().missing(),
        "a committed secret is not used"
    );

    let m = machine("[settings.ghostty]\ntoken = \"abc\"\n", None, d.path(), &[]);
    let d2 = base(&[]);
    let s = run(d2.path(), &m);
    let t = s.get("ghostty", "token").unwrap();
    assert_eq!(t.value(), Some(&json!("abc")));
    assert_eq!(t.shown(), Some(json!("•••")));
    let uses = s.uses("ghostty");
    assert!(
        uses.iter()
            .any(|u| u.name == "token" && u.value == json!("•••") && u.secret)
    );
    assert!(!format!("{uses:?}").contains("abc"));
    // Real value is only in the render variables.
    assert_eq!(s.vars("ghostty")["setting"]["token"], json!("abc"));
    assert_eq!(
        s.vars("other")["settings"]["ghostty"]["token"],
        json!("abc")
    );
}

#[test]
fn when_false_means_the_setting_does_not_apply() {
    let d = base(&[]);
    let s = run(d.path(), &machine("", None, d.path(), &[]));
    let c = s.get("ghostty", "cursor_shader").unwrap();
    assert!(!c.applies);
    assert!(s.uses("ghostty").iter().all(|u| u.name != "cursor_shader"));
    assert!(!s.lookup("ghostty").resolves(None, "cursor_shader"));
    assert!(s.lookup("ghostty").resolves(None, "font_size"));
}

#[test]
fn stale_values_warn() {
    let d = base(&[(
        "dots.toml",
        "schema = 1\n[settings.gone]\nx = 1\n[settings.ghostty]\nnope = 1\n",
    )]);
    let s = run(d.path(), &machine("", None, d.path(), &[]));
    let w: Vec<&str> = s.warnings.iter().map(|w| w.message.as_str()).collect();
    assert_eq!(w.len(), 2, "{w:?}");
    assert!(s.errors.is_empty());
}

#[test]
fn conditions_see_package_settings() {
    let d = base(&[(
        "dots.toml",
        "schema = 1\n[settings.ghostty]\ntheme = \"light\"\n",
    )]);
    let s = run(d.path(), &machine("", None, d.path(), &[]));
    let lookup = s.lookup("ghostty");
    let facts = dots_facts::detect(&dots_facts::RealSources, &Default::default());
    let chain = dots_facts::Chain::build(&facts, &Default::default(), &[]);
    let (tags, declared) = (BTreeSet::new(), BTreeSet::new());
    let ctx = EvalCtx {
        facts: &facts,
        chain: &chain,
        tags: &tags,
        declared_tags: &declared,
        settings: &lookup,
    };
    assert!(
        Condition::parse("setting.theme = \"light\"")
            .unwrap()
            .eval(&ctx)
            .unwrap()
    );
    assert!(
        Condition::parse("settings.ghostty.font_size = 13")
            .unwrap()
            .eval(&ctx)
            .unwrap()
    );
}

#[test]
fn override_changes_the_default_and_the_chosen_value_still_wins() {
    let d = base(&[(
        "overrides/packages/ghostty/override.toml",
        "schema = 1\n[settings.font_size]\ndefault = 18\n",
    )]);
    let repo = Repo::load(d.path()).unwrap().value;
    let eff = repo
        .effective(
            &|_| vec![],
            &OverrideLocation::standard(d.path(), None, d.path().join("none"), "m".into()),
        )
        .unwrap()
        .value;
    let m = machine("", None, d.path(), &[]);
    let s = resolve(&Inputs::new(&eff, &m, &never));
    assert_eq!(
        s.get("ghostty", "font_size").unwrap().value(),
        Some(&json!(18))
    );
    let m = machine("[settings.ghostty]\nfont_size = 10\n", None, d.path(), &[]);
    let s = resolve(&Inputs::new(&eff, &m, &never));
    assert_eq!(
        s.get("ghostty", "font_size").unwrap().value(),
        Some(&json!(10))
    );
}

#[test]
fn override_of_a_repo_value_comes_through_the_effective_model() {
    // A repo-level override of dots.toml replaces the value at the repo scope; a machine-level
    // one ranks as machine and keeps dots.toml's own value as a lower layer.
    let d = base(&[
        (
            "dots.toml",
            "schema = 1\n[settings.ghostty]\nfont_size = 14\n",
        ),
        (
            "overrides/dots.toml",
            "schema = 1\n[settings.ghostty]\nfont_size = 20\n",
        ),
    ]);
    let mdir = tempfile::tempdir().unwrap();
    write(
        mdir.path(),
        "overrides/dots.toml",
        "schema = 1\n[settings.ghostty]\nfont_size = 22\n",
    );
    let repo = Repo::load(d.path()).unwrap().value;
    let machine_cfg = machine("", None, d.path(), &[]);
    let eff = |with_machine: bool| {
        let dir = if with_machine {
            mdir.path().join("overrides")
        } else {
            d.path().join("none")
        };
        repo.effective(
            &|_| vec![],
            &OverrideLocation::standard(d.path(), None, dir, "~/o".into()),
        )
        .unwrap()
        .value
    };
    let e = eff(false);
    let s = resolve(&Inputs::new(&e, &machine_cfg, &never));
    let st = s.get("ghostty", "font_size").unwrap();
    assert_eq!(st.value(), Some(&json!(20)), "{:?}", st.layers);
    assert_eq!(st.winning_layer().unwrap().file, "overrides/dots.toml");

    let e = eff(true);
    let s = resolve(&Inputs::new(&e, &machine_cfg, &never));
    let st = s.get("ghostty", "font_size").unwrap();
    assert_eq!(st.value(), Some(&json!(22)));
    assert_eq!(st.scope(), Some(SettingScope::Machine));
    assert!(
        st.layers
            .iter()
            .any(|l| l.value == json!(14) && l.file == "dots.toml")
    );
}

#[test]
fn pending_lists_required_then_ask_and_accepting_defaults_leaves_only_required() {
    let d = base(&[("bundles/b.toml", "schema = 1\npackages = [\"ghostty\"]\n")]);
    let repo = Repo::load(d.path()).unwrap().value;
    let pkgs = dots_config::packages_of(&repo, &["b".to_string()]);
    let m = machine("", None, d.path(), &["b"]);
    let s = resolve(&Inputs::new(&repo, &m, &never));
    let pending: Vec<(String, Why)> = s.pending(&pkgs).iter().map(|(s, w)| (s.id(), *w)).collect();
    assert_eq!(
        pending,
        vec![
            ("ghostty.theme".to_string(), Why::Ask),
            ("ghostty.token".to_string(), Why::Required)
        ]
    );
    // Answer every question that has a recommended value with it.
    let m = machine(
        "[settings.ghostty]\ntheme = \"dark\"\n",
        None,
        d.path(),
        &["b"],
    );
    let s = resolve(&Inputs::new(&repo, &m, &never));
    let pending: Vec<_> = s.pending(&pkgs).iter().map(|(s, w)| (s.id(), *w)).collect();
    assert_eq!(pending, vec![("ghostty.token".to_string(), Why::Required)]);
}

#[test]
fn parse_text_follows_the_declared_type() {
    let d = base(&[]);
    let repo = Repo::load(d.path()).unwrap().value;
    let decl = &repo.packages["ghostty"].settings;
    assert_eq!(decl["font_size"].parse_text("15").unwrap(), json!(15));
    assert!(
        decl["font_size"]
            .parse_text("big")
            .unwrap_err()
            .contains("expected int")
    );
    assert!(decl["font_size"].parse_text("99").is_err());
    assert_eq!(
        decl["cursor_shader"].parse_text("yes").unwrap(),
        json!(true)
    );
    assert_eq!(
        decl["theme"].parse_text("light").unwrap(),
        Value::from("light")
    );
    assert!(decl["theme"].parse_text("x").is_err());
}

#[test]
fn set_and_unset_preserve_formatting() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("dots.toml");
    let original = "# my dots\nschema = 1\nname = \"x\"   # keep\n\n[settings.ghostty]\n# size\nfont_size = 14 # why\n";
    fs::write(&p, original).unwrap();
    dots_config::set_in_file(&p, "ghostty", "font_size", &json!(15), false).unwrap();
    assert_eq!(
        fs::read_to_string(&p).unwrap(),
        original.replace("14 # why", "15 # why")
    );
    dots_config::set_in_file(&p, "git", "email", &json!("a@b.c"), false).unwrap();
    let t = fs::read_to_string(&p).unwrap();
    assert!(
        t.starts_with("# my dots\nschema = 1\nname = \"x\"   # keep\n"),
        "{t}"
    );
    assert!(t.contains("[settings.git]\nemail = \"a@b.c\""), "{t}");
    assert!(dots_config::unset_in_file(&p, "git", "email").unwrap());
    assert!(!dots_config::unset_in_file(&p, "git", "email").unwrap());
    assert_eq!(
        fs::read_to_string(&p).unwrap(),
        original.replace("14 # why", "15 # why")
    );
    // A new machine.toml gets a schema line.
    let m = d.path().join("sub/machine.toml");
    dots_config::set_in_file(&m, "a", "b", &json!(["x", "y"]), false).unwrap();
    assert_eq!(
        fs::read_to_string(&m).unwrap(),
        "schema = 1\n\n[settings.a]\nb = [\"x\", \"y\"]\n"
    );
}
