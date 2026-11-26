//! Task 5.12: the new provider keys load from `package.toml` and reach `Spec::opts`.

use std::fs;
use std::path::Path;

use dots_model::Repo;
use dots_resolve::spec_for_in;

fn load(pkg: &str) -> (tempfile::TempDir, Repo) {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("dots.toml"), "schema = 1\n").unwrap();
    let p = dir.path().join("packages/x/package.toml");
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(&p, pkg).unwrap();
    let repo = Repo::load(dir.path())
        .unwrap_or_else(|e| panic!("{:#?}", e.errors))
        .value;
    (dir, repo)
}

fn spec(repo: &Repo, provider: &str, dir: Option<&Path>) -> dots_providers::Spec {
    let p = &repo.packages["x"];
    spec_for_in("x", p.providers.get(provider), dir)
}

#[test]
fn apt_repo_and_pin() {
    let (_d, repo) = load(
        r#"schema = 1
name = "x"
[providers.apt]
repo = { url = "https://pkgs.example.com/deb", suite = "stable", components = ["main", "contrib"], key = "https://pkgs.example.com/key.asc", fingerprint = "ABCDEF0123456789ABCDEF0123456789ABCDEF01" }
pin = { origin = "pkgs.example.com", priority = 900 }
"#,
    );
    let s = spec(&repo, "apt", None);
    assert_eq!(s.get("repo.url"), Some("https://pkgs.example.com/deb"));
    assert_eq!(s.get("repo.suite"), Some("stable"));
    assert_eq!(s.get("repo.components"), Some("main,contrib"));
    assert_eq!(s.get("repo.key"), Some("https://pkgs.example.com/key.asc"));
    assert_eq!(s.get("pin.origin"), Some("pkgs.example.com"));
    assert_eq!(s.get("pin.priority"), Some("900"));
    assert!(
        dots_providers::apt_repo::AptRepo::from_spec(&s)
            .unwrap()
            .is_some()
    );

    let (_d, repo) =
        load("schema = 1\nname = \"x\"\n[providers.apt]\nrepo = { ppa = \"git-core/ppa\" }\n");
    assert_eq!(
        spec(&repo, "apt", None).get("repo.ppa"),
        Some("git-core/ppa")
    );
}

/// Flat apt repositories (`deb <url> apt/stable/`): `components = []` and `flat = true` both
/// reach the provider as `repo.flat`, and a flat repository renders without `Components`.
#[test]
fn flat_apt_repository_by_empty_components_or_flat_key() {
    for decl in [
        r#"components = [],"#,
        r#"flat = true,"#,
        // Implied by a suite path with no components at all.
        r#""#,
    ] {
        let (_d, repo) = load(&format!(
            r#"schema = 1
name = "x"
[providers.apt]
repo = {{ url = "https://download.sublimetext.com/", suite = "apt/stable/", {decl} key = "https://download.sublimetext.com/sublimehq-pub.gpg", fingerprint = "1EDDE2CDFC025D17F6DA9EC0ADAE6AD28A8F901A" }}
"#
        ));
        let s = spec(&repo, "apt", None);
        let r = dots_providers::apt_repo::AptRepo::from_spec(&s)
            .unwrap_or_else(|e| panic!("{decl}: {e}"))
            .unwrap();
        let text = r.sources_text(None);
        assert!(text.contains("Suites: apt/stable/\n"), "{decl}: {text}");
        assert!(!text.contains("Components"), "{decl}: {text}");
    }
}

#[test]
fn cargo_and_uv_sources() {
    let (_d, repo) = load(
        "schema = 1\nname = \"x\"\n[providers.cargo]\npath = \"herdr-last-tab\"\n[providers.uv]\ngit = \"git+ssh://git@github.com/x/y\"\nrev = \"abc\"\ngit_fetch_with_cli = true\n",
    );
    let s = spec(&repo, "cargo", Some(Path::new("/repo/packages/x")));
    assert_eq!(s.get("path"), Some("herdr-last-tab"));
    assert_eq!(s.get("pkg_dir"), Some("/repo/packages/x"));
    let u = spec(&repo, "uv", Some(Path::new("/repo/packages/x")));
    assert_eq!(u.get("git"), Some("git+ssh://git@github.com/x/y"));
    assert_eq!(u.get("rev"), Some("abc"));
    assert!(u.flag("git_fetch_with_cli", false));
    assert_eq!(u.get("pkg_dir"), None, "no path, no package dir");

    let (_d, repo) = load(
        "schema = 1\nname = \"x\"\n[providers.cargo]\ngit = \"ssh://git@github.com/x/y\"\ngit_fetch_with_cli = true\n",
    );
    assert!(spec(&repo, "cargo", None).flag("git_fetch_with_cli", false));
}

#[test]
fn cargo_locked_false_and_uv_break_system_packages_reach_the_spec() {
    let (_d, repo) = load(
        "schema = 1\nname = \"x\"\n[providers.cargo]\nlocked = false\n[providers.uv]\nkind = \"lib\"\npython = \"system\"\nbreak_system_packages = true\n",
    );
    assert_eq!(spec(&repo, "cargo", None).get("locked"), Some("false"));
    let u = spec(&repo, "uv", None);
    assert_eq!(u.get("kind"), Some("lib"));
    assert!(u.flag("break_system_packages", false));
    let (_d, repo) = load("schema = 1\nname = \"x\"\n[providers.cargo]\nfeatures = [\"a\"]\n");
    assert_eq!(
        spec(&repo, "cargo", None).get("locked"),
        None,
        "default stays locked"
    );
}

#[test]
fn script_fetch_loads() {
    let (_d, repo) = load(
        "schema = 1\nname = \"x\"\n[providers.script]\ncheck = \"true\"\nversionless = true\ninstall = \"sh {fetched}\"\nfetch = { url = \"https://sh.rustup.rs\", sha256 = \"abc\" }\n",
    );
    let Some(dots_model::ProviderConfig::Script(s)) = repo.packages["x"].providers.get("script")
    else {
        panic!("script config");
    };
    let f = s.fetch.as_ref().unwrap();
    assert_eq!(f.url.as_deref(), Some("https://sh.rustup.rs"));
    assert_eq!(f.sha256.as_deref(), Some("abc"));
}
