//! One test per repo-schema scenario, plus the strict-parsing edge cases.

use std::path::{Path, PathBuf};

use dots_core::Diagnostic;
use dots_model::{
    BundleMember, ListSpec, LoadErrors, MachineConfig, ProviderConfig, Repo, check_requires_dots,
};

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

fn load_err(name: &str) -> LoadErrors {
    match Repo::load(&fixture(name)) {
        Ok(_) => panic!("{name} should fail to load"),
        Err(e) => e,
    }
}

fn only(e: &LoadErrors, code: &str) -> Diagnostic {
    let hits: Vec<_> = e.errors.iter().filter(|d| d.code == code).collect();
    assert_eq!(hits.len(), 1, "expected one {code} in {:?}", e.codes());
    hits[0].clone()
}

#[test]
fn package_name_mismatch() {
    let e = load_err("name-mismatch");
    let d = only(&e, "schema-name-mismatch");
    assert_eq!(
        d.provenance[0].file.as_deref(),
        Some("packages/fd/package.toml")
    );
    assert!(d.message.contains("fdx") && d.message.contains("fd"));
    assert_eq!(d.provenance[0].line, Some(2));
}

#[test]
fn escaping_path_is_rejected() {
    let e = load_err("escaping-path");
    let d = only(&e, "path-escapes-package");
    assert!(d.message.contains("../shared/x"));
    assert_eq!(d.provenance[0].line, Some(6));
    assert_eq!(
        d.provenance[0].file.as_deref(),
        Some("packages/ghostty/package.toml")
    );
}

#[test]
fn double_slash_path_is_accepted_with_lint_warning() {
    let ok = Repo::load(&fixture("escape-hatch")).expect("loads");
    assert_eq!(ok.warnings.len(), 1);
    assert_eq!(ok.warnings[0].code, "escape-hatch-path");
    assert_eq!(
        ok.value.packages["legacy"].escape_hatch_paths,
        ["//shared/legacy"]
    );
}

#[test]
fn future_schema_is_unsupported() {
    let e = load_err("future-schema");
    let d = only(&e, "schema-unsupported");
    assert!(d.hint.contains("newer dots"), "{}", d.hint);
    assert!(d.message.contains("schema 2"));
    assert_eq!(e.errors.len(), 1);
}

#[test]
fn dots_too_old() {
    let repo = Repo::load(&fixture("dots-too-old")).expect("loads").value;
    let d = repo.check_dots_version("0.2.1").expect("too old");
    assert_eq!(d.code, "dots-too-old");
    assert!(d.message.contains(">=0.3") && d.message.contains("0.2.1"));
    assert!(repo.check_dots_version("0.3.0").is_none());
    assert!(repo.check_dots_version("1.0.0-rc1").is_none());
    assert!(check_requires_dots(">=0.3", "0.2.9", "dots.toml").is_some());
    assert!(check_requires_dots(">=0.3.1", "0.3", "dots.toml").is_some());
    assert!(check_requires_dots(">= 0.1", "0.1.0", "dots.toml").is_none());
}

#[test]
fn bad_requires_dots_is_an_error() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("dots.toml"),
        "schema = 1\nrequires_dots = \"^0.1\"\n",
    )
    .unwrap();
    let e = Repo::load(dir.path()).unwrap_err();
    assert_eq!(e.codes(), ["schema-invalid-value"]);
}

#[test]
fn minimal_package() {
    let repo = Repo::load(&fixture("minimal")).expect("loads").value;
    let p = &repo.packages["ripgrep"];
    assert_eq!(p.name, "ripgrep");
    assert_eq!(p.bin.as_deref(), Some("rg"));
    assert!(p.providers.is_empty() && !p.providers_only);
    assert!(p.origin.is_none());
}

#[test]
fn unknown_key_suggests_and_locates() {
    let e = load_err("unknown-key");
    let d = only(&e, "schema-unknown-key");
    assert!(d.message.contains("descripton"));
    assert!(d.hint.contains("`description`"), "{}", d.hint);
    let p = &d.provenance[0];
    assert_eq!(p.file.as_deref(), Some("packages/fd/package.toml"));
    assert_eq!((p.line, p.col), (Some(3), Some(1)));
}

#[test]
fn wrong_type_has_position() {
    let e = load_err("wrong-type");
    let d = only(&e, "schema-wrong-type");
    let p = &d.provenance[0];
    assert_eq!((p.line, p.col), (Some(3), Some(7)));
    assert!(d.message.contains("`bin`") && d.message.contains("a string"));
}

#[test]
fn all_errors_are_collected() {
    let e = load_err("multi-error");
    let mut codes = e.codes();
    codes.sort_unstable();
    assert_eq!(
        codes,
        [
            "schema-name-mismatch",
            "schema-unknown-key",
            "schema-wrong-type"
        ]
    );
}

#[test]
fn syntax_error_is_reported_with_position() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("dots.toml"), "schema = 1\nname = \n").unwrap();
    let e = Repo::load(dir.path()).unwrap_err();
    assert_eq!(e.codes(), ["schema-syntax"]);
    assert!(e.errors[0].provenance[0].line.is_some());
}

#[test]
fn missing_schema_is_an_error() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("dots.toml"), "name = \"x\"\n").unwrap();
    let e = Repo::load(dir.path()).unwrap_err();
    assert_eq!(e.codes(), ["schema-missing-key"]);
}

#[test]
fn full_surface_parses() {
    let loaded = Repo::load(&fixture("full")).unwrap_or_else(|e| panic!("{:#?}", e.errors));
    assert!(loaded.warnings.is_empty());
    let repo = loaded.value;
    assert_eq!(repo.manifest.priority["macos"], ["brew", "cargo"]);
    assert!(repo.manifest.providers.contains_key("go"));
    assert!(repo.manifest.platform.contains_key("macos"));
    assert_eq!(repo.manifest.upstream[0].alias.as_deref(), Some("ofirgall"));
    assert_eq!(repo.bundles["cli"].packages[1].name(), "wiremix");
    assert!(matches!(
        repo.bundles["cli"].packages[0],
        BundleMember::Name(_)
    ));
    assert!(repo.bundles["cli"].platform.contains_key("macos"));

    let g = &repo.packages["ghostty"];
    assert!(matches!(g.providers["brew"], ProviderConfig::Brew(ref b) if b.cask));
    assert!(matches!(g.providers["go"], ProviderConfig::Custom { .. }));
    assert!(
        matches!(g.providers["script"], ProviderConfig::Script(ref s) if s.timeout.as_deref() == Some("20m"))
    );
    assert_eq!(g.files.len(), 3);
    assert_eq!(g.files[1].set.len(), 1);
    assert_eq!(g.settings["font_size"].range, [8.0, 32.0]);
    assert!(g.settings["git_email"].required);
    assert!(g.platform["wsl"]["requires"]["add"][0] == "win32yank");
    assert_eq!(g.origin.as_ref().unwrap().rev.as_deref(), Some("f452"));
    assert_eq!(g.requires.len(), 2);

    let v = &repo.packages["vimium"];
    assert!(
        matches!(v.providers["browser-ext"], ProviderConfig::BrowserExt(ref b) if b.method.as_deref() == Some("policy"))
    );
    assert!(matches!(
        repo.packages["pinned"].integrity,
        Some(dots_model::Integrity::Pins(_))
    ));
}

#[test]
fn enum_values_are_checked() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("dots.toml"), "schema = 1\n").unwrap();
    let pkg = dir.path().join("packages/x");
    std::fs::create_dir_all(&pkg).unwrap();
    std::fs::write(
        pkg.join("package.toml"),
        "schema = 1\nname = \"x\"\n[[files]]\ndst = \"~/x\"\nmode = \"rendr\"\n",
    )
    .unwrap();
    let e = Repo::load(dir.path()).unwrap_err();
    assert_eq!(e.codes(), ["schema-invalid-value"]);
    assert!(e.errors[0].hint.contains("`render`"));
}

/// A directory under `packages/` without `package.toml` is a lint warning and is skipped; it
/// must not fail the load (spec repo-schema; it broke every command).
#[test]
fn directory_without_manifest_is_a_warning_not_an_error() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("dots.toml"), "schema = 1\n").unwrap();
    std::fs::create_dir_all(dir.path().join("packages/placeholder")).unwrap();
    let ok = dir.path().join("packages/ok");
    std::fs::create_dir_all(&ok).unwrap();
    std::fs::write(ok.join("package.toml"), "schema = 1\nname = \"ok\"\n").unwrap();
    let loaded = Repo::load(dir.path()).expect("loads despite the bare directory");
    assert!(loaded.value.packages.contains_key("ok"));
    assert!(!loaded.value.packages.contains_key("placeholder"));
    let w: Vec<_> = loaded
        .warnings
        .iter()
        .filter(|d| d.code == "package-missing-manifest")
        .collect();
    assert_eq!(w.len(), 1, "{:?}", loaded.warnings);
    assert!(w[0].message.contains("packages/placeholder/package.toml"));
}

// ---------------------------------------------------------------- machines

#[test]
fn excluded_package_parses() {
    let root = fixture("excluded");
    let repo = Repo::load(&root).expect("loads").value;
    assert!(repo.packages.contains_key("spotify"));
    assert_eq!(repo.bundles["desktop"].packages[0].name(), "spotify");

    let m = MachineConfig::load(&root, &root.join("machine.toml"), "anyhost")
        .expect("loads")
        .value;
    assert_eq!(m.exclude, ["spotify"]);
    assert_eq!(m.bundles, ["desktop"]);
    assert_eq!(m.profile, None);
}

#[test]
fn profile_plus_local_tweak() {
    let root = fixture("profile");
    let m = MachineConfig::load(&root, &root.join("local-add.toml"), "elsewhere")
        .expect("loads")
        .value;
    assert_eq!(m.profile.as_deref(), Some("work-mbp"));
    assert_eq!(m.bundles, ["base", "cli", "gaming"]);
    assert_eq!(m.tags, ["work"]);
    // settings merge key by key: local `name` wins, profile `email` stays.
    assert_eq!(m.settings["git"]["name"], "Local");
    assert_eq!(m.settings["git"]["email"], "me@work.example");
}

#[test]
fn profile_selected_by_hostname() {
    let root = fixture("profile");
    let m = MachineConfig::load(&root, &root.join("absent.toml"), "work-mbp")
        .expect("loads")
        .value;
    assert_eq!(m.profile.as_deref(), Some("work-mbp"));
    assert_eq!(m.bundles, ["base", "cli"]);
    let none = MachineConfig::load(&root, &root.join("absent.toml"), "other")
        .expect("loads")
        .value;
    assert_eq!(none, MachineConfig::default());
}

#[test]
fn local_remove_and_plain_replace() {
    let root = fixture("profile");
    let m = MachineConfig::load(&root, &root.join("local-remove-replace.toml"), "h")
        .expect("loads")
        .value;
    assert_eq!(m.bundles, ["base", "mac"]);
    assert_eq!(m.tags, ["home"]);
}

#[test]
fn missing_profile_and_machine_typos_are_errors() {
    let root = fixture("profile");
    let e = MachineConfig::load(&root, &root.join("local-missing-profile.toml"), "h").unwrap_err();
    assert_eq!(e.codes(), ["machine-profile-missing"]);
    let e = MachineConfig::load(&root, &root.join("local-typo.toml"), "h").unwrap_err();
    assert_eq!(e.codes(), ["schema-unknown-key"]);
    assert!(e.errors[0].hint.contains("`bundles`"));
}

#[test]
fn list_edit_semantics() {
    let base = vec!["a".to_string(), "b".to_string()];
    let edit = ListSpec::Edit {
        add: vec!["b".into(), "c".into()],
        remove: vec!["a".into()],
    };
    assert_eq!(edit.apply(&base), ["b", "c"]);
}
