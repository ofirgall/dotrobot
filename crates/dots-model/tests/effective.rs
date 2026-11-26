//! Tasks 5.1 and 5.6: platform tables and overrides, merged into the effective model.

use std::fs;
use std::path::Path;

use dots_model::{Address, LoadErrors, OverrideLocation, Repo, Scope};
use tempfile::TempDir;

fn write(root: &Path, rel: &str, text: &str) {
    let p = root.join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, text).unwrap();
}

/// A repo with `dots.toml` and the given `(path, text)` files.
fn repo(files: &[(&str, &str)]) -> TempDir {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "dots.toml", "schema = 1\n");
    for (p, t) in files {
        write(dir.path(), p, t);
    }
    dir
}

/// The ordered keys for a chain of `levels` on `arch`: each level then its arch qualifier.
fn chain(levels: &[&str], arch: &str) -> Vec<String> {
    levels
        .iter()
        .flat_map(|l| [l.to_string(), format!("{l}/{arch}")])
        .collect()
}

const UBUNTU_24: &[&str] = &[
    "any",
    "unix",
    "linux",
    "debian",
    "ubuntu",
    "ubuntu-24",
    "ubuntu-24.04",
];
const UBUNTU_22: &[&str] = &[
    "any",
    "unix",
    "linux",
    "debian",
    "ubuntu",
    "ubuntu-22",
    "ubuntu-22.04",
];

fn locations(
    dir: &TempDir,
    machine: Option<&TempDir>,
    profile: Option<&str>,
) -> Vec<OverrideLocation> {
    let (mdir, mdisp) = match machine {
        Some(m) => (
            m.path().join("overrides"),
            "~/.config/dots/overrides".to_string(),
        ),
        None => (
            dir.path().join("nonexistent"),
            "~/.config/dots/overrides".to_string(),
        ),
    };
    OverrideLocation::standard(dir.path(), profile, mdir, mdisp)
}

fn effective(dir: &TempDir, ch: &[String], locs: &[OverrideLocation]) -> Result<Repo, LoadErrors> {
    let base = Repo::load(dir.path())
        .unwrap_or_else(|e| panic!("{:#?}", e.errors))
        .value;
    let ch = ch.to_vec();
    base.effective(&move |_| ch.clone(), locs).map(|l| l.value)
}

fn eff_ok(dir: &TempDir, ch: &[String]) -> Repo {
    let locs = locations(dir, None, None);
    effective(dir, ch, &locs).unwrap_or_else(|e| panic!("{:#?}", e.errors))
}

const NEOVIM: &str = r#"
schema = 1
name = "neovim"
bin = "nvim"

[providers.apt]
name = "neovim"

[platform.ubuntu.providers.apt]
ppa = "ppa:neovim-ppa/unstable"

[platform."ubuntu-22.04"]
prefer = ["github-release"]

[platform."linux/aarch64".providers.apt]
name = "neovim-arm"

[platform.wsl]
requires = { add = ["win32yank"] }
"#;

// ------------------------------------------------------------------ 5.1: platform tables

#[test]
fn release_specific_override() {
    let dir = repo(&[("packages/neovim/package.toml", NEOVIM)]);
    let r24 = eff_ok(&dir, &chain(UBUNTU_24, "x86_64"));
    let p = &r24.packages["neovim"];
    assert_eq!(p.providers["apt"].common().name.as_deref(), Some("neovim"));
    assert!(p.prefer.is_empty(), "24.04 keeps the default preference");
    match &p.providers["apt"] {
        dots_model::ProviderConfig::Apt(a) => {
            assert_eq!(a.ppa.as_deref(), Some("ppa:neovim-ppa/unstable"))
        }
        _ => panic!(),
    }

    let r22 = eff_ok(&dir, &chain(UBUNTU_22, "x86_64"));
    let p = &r22.packages["neovim"];
    assert_eq!(p.prefer, ["github-release"], "22.04 prefers the release");
    match &p.providers["apt"] {
        dots_model::ProviderConfig::Apt(a) => {
            assert_eq!(a.ppa.as_deref(), Some("ppa:neovim-ppa/unstable"))
        }
        _ => panic!(),
    }
}

#[test]
fn arch_qualifier_and_list_edits() {
    let dir = repo(&[("packages/neovim/package.toml", NEOVIM)]);
    // aarch64 applies the `linux/aarch64` table; wsl adds a requirement.
    let mut levels = UBUNTU_24.to_vec();
    levels.push("wsl");
    let r = eff_ok(&dir, &chain(&levels, "aarch64"));
    let p = &r.packages["neovim"];
    assert_eq!(
        p.providers["apt"].common().name.as_deref(),
        Some("neovim-arm")
    );
    assert_eq!(
        p.requires.iter().map(|x| x.name()).collect::<Vec<_>>(),
        ["win32yank"]
    );
    // x86_64 without wsl: neither.
    let r = eff_ok(&dir, &chain(UBUNTU_24, "x86_64"));
    let p = &r.packages["neovim"];
    assert_eq!(p.providers["apt"].common().name.as_deref(), Some("neovim"));
    assert!(p.requires.is_empty());
}

#[test]
fn most_specific_wins_and_scalars_replace() {
    let dir = repo(&[(
        "packages/x/package.toml",
        r#"
schema = 1
name = "x"
bin = "base"
conflicts = ["a", "b"]
[platform.linux]
bin = "linux"
conflicts = ["c"]
[platform.debian]
bin = "debian"
[platform.ubuntu]
bin = "ubuntu"
[platform."ubuntu-24.04"]
bin = "u2404"
"#,
    )]);
    let p24 = eff_ok(&dir, &chain(UBUNTU_24, "x86_64")).packages["x"].clone();
    assert_eq!(p24.bin.as_deref(), Some("u2404"));
    assert_eq!(p24.conflicts, ["c"], "arrays replace");
    let p22 = eff_ok(&dir, &chain(UBUNTU_22, "x86_64")).packages["x"].clone();
    assert_eq!(p22.bin.as_deref(), Some("ubuntu"));
    let mac = eff_ok(&dir, &chain(&["any", "unix", "macos"], "aarch64")).packages["x"].clone();
    assert_eq!(mac.bin.as_deref(), Some("base"));
    assert_eq!(mac.conflicts, ["a", "b"]);
}

#[test]
fn nested_platform_tables() {
    let dir = repo(&[(
        "packages/ghostty/package.toml",
        r#"
schema = 1
name = "ghostty"

[providers.brew]
name = "ghostty"
[providers.brew.platform.macos]
cask = true

[[files]]
dst = "~/.config/ghostty/config"
src = "config"
[files.platform.macos]
src = "config.mac"

[settings.font_size]
type = "int"
default = 12
[settings.font_size.platform.macos]
default = 14
"#,
    )]);
    let mac = eff_ok(&dir, &chain(&["any", "unix", "macos"], "aarch64"));
    let p = &mac.packages["ghostty"];
    match &p.providers["brew"] {
        dots_model::ProviderConfig::Brew(b) => assert!(b.cask),
        _ => panic!(),
    }
    assert_eq!(p.files[0].src.as_deref(), Some("config.mac"));
    assert_eq!(p.settings["font_size"].default, Some(14.into()));
    let linux = eff_ok(&dir, &chain(UBUNTU_24, "x86_64"));
    let p = &linux.packages["ghostty"];
    assert_eq!(p.files[0].src.as_deref(), Some("config"));
    assert_eq!(p.settings["font_size"].default, Some(12.into()));
}

#[test]
fn platform_table_can_change_when() {
    let dir = repo(&[(
        "packages/mac/package.toml",
        "schema = 1\nname = \"mac\"\nwhen = 'os = \"macos\"'\n[platform.wsl]\nwhen = \"true\"\n",
    )]);
    let mut levels = UBUNTU_24.to_vec();
    assert_eq!(
        eff_ok(&dir, &chain(&levels, "x86_64")).packages["mac"]
            .when
            .as_deref(),
        Some("os = \"macos\"")
    );
    levels.push("wsl");
    assert_eq!(
        eff_ok(&dir, &chain(&levels, "x86_64")).packages["mac"]
            .when
            .as_deref(),
        Some("true")
    );
}

#[test]
fn platform_tables_on_bundles_and_manifest() {
    let dir = repo(&[
        (
            "bundles/cli.toml",
            "schema = 1\npackages = [\"a\", \"b\"]\n[platform.macos]\npackages = { add = [\"c\"], remove = [\"b\"] }\n",
        ),
        ("packages/a/package.toml", "schema = 1\nname = \"a\"\n"),
    ]);
    let r = eff_ok(&dir, &chain(&["any", "unix", "macos"], "aarch64"));
    let names: Vec<_> = r.bundles["cli"].packages.iter().map(|m| m.name()).collect();
    assert_eq!(names, ["a", "c"]);
    let r = eff_ok(&dir, &chain(UBUNTU_24, "x86_64"));
    let names: Vec<_> = r.bundles["cli"].packages.iter().map(|m| m.name()).collect();
    assert_eq!(names, ["a", "b"]);
}

#[test]
fn platform_keys_are_collected_with_lines() {
    let dir = repo(&[("packages/neovim/package.toml", NEOVIM)]);
    let base = Repo::load(dir.path()).unwrap().value;
    let keys: Vec<&str> = base.platform_keys.iter().map(|k| k.key.as_str()).collect();
    assert!(keys.contains(&"ubuntu") && keys.contains(&"ubuntu-22.04") && keys.contains(&"wsl"));
    let k = base
        .platform_keys
        .iter()
        .find(|k| k.key == "ubuntu-22.04")
        .unwrap();
    assert_eq!(k.file, "packages/neovim/package.toml");
    assert_eq!(k.line, Some(12));
}

#[test]
fn provenance_names_the_platform_table() {
    let dir = repo(&[("packages/neovim/package.toml", NEOVIM)]);
    let r = eff_ok(&dir, &chain(UBUNTU_22, "x86_64"));
    let info = r.effective.as_ref().unwrap();
    let obj = &info.objects["pkg:neovim"];
    let at = obj.prov.get("prefer").expect("set by a platform table");
    assert_eq!(at.source.scope, Scope::Definition);
    assert_eq!(at.source.platform.as_deref(), Some("ubuntu-22.04"));
    assert_eq!(at.line, Some(13));
    assert_eq!(
        at.source.describe(),
        "platform table [ubuntu-22.04] (packages/neovim/package.toml)"
    );
    // Keys the definition set are not stored.
    assert!(obj.prov.get("providers.apt.name").is_none());
    let ppa = obj.prov.get("providers.apt.ppa").unwrap();
    assert_eq!(ppa.source.platform.as_deref(), Some("ubuntu"));
}

#[test]
fn plain_load_has_no_effective_info() {
    let dir = repo(&[("packages/neovim/package.toml", NEOVIM)]);
    assert!(Repo::load(dir.path()).unwrap().value.effective.is_none());
}

// ----------------------------------------------------------------- 5.6: overrides

const GHOSTTY: &str = r#"
schema = 1
name = "ghostty"
version = "1.1"
prefer = ["brew"]

[providers.brew]
name = "ghostty"
[providers.script]
check = "true"
install = "true"

[[files]]
dst = "~/.config/ghostty/config"
src = "config"
[[files]]
dst = "~/.config/ghostty/themes/old"
src = "themes/old"

[settings.font_size]
type = "int"
default = 13
[settings.theme]
type = "enum"
values = ["light", "dark"]
default = "dark"
"#;

fn ghostty_repo() -> TempDir {
    repo(&[
        ("packages/ghostty/package.toml", GHOSTTY),
        (
            "packages/zoxide/package.toml",
            "schema = 1\nname = \"zoxide\"\n",
        ),
        (
            "bundles/cli.toml",
            "schema = 1\npackages = [\"ghostty\", \"jless\", { name = \"bottom\" }]\ninclude = [\"koala\"]\n",
        ),
        ("bundles/koala.toml", "schema = 1\npackages = []\n"),
    ])
}

#[test]
fn edit_a_bundles_list() {
    let dir = ghostty_repo();
    write(
        dir.path(),
        "overrides/bundles/cli.toml",
        "schema = 1\npackages = { add = [\"zoxide\"], remove = [\"jless\"] }\ninclude = { remove = [\"koala\"] }\n",
    );
    let r = eff_ok(&dir, &chain(UBUNTU_24, "x86_64"));
    let names: Vec<_> = r.bundles["cli"].packages.iter().map(|m| m.name()).collect();
    assert_eq!(names, ["ghostty", "bottom", "zoxide"]);
    assert!(r.bundles["cli"].include.is_empty());
    // The repo's own file is unchanged.
    let on_disk = fs::read_to_string(dir.path().join("bundles/cli.toml")).unwrap();
    assert!(on_disk.contains("jless"));
    // Provenance and the override list.
    let info = r.effective.as_ref().unwrap();
    assert_eq!(info.scopes_of("bundle:cli"), [Scope::Repo]);
    let at = info.objects["bundle:cli"].prov.get("packages").unwrap();
    assert_eq!(
        at.source.describe(),
        "repo override (overrides/bundles/cli.toml)"
    );
    assert_eq!(at.line, Some(2));
}

#[test]
fn bundle_members_and_settings() {
    let dir = ghostty_repo();
    write(
        dir.path(),
        "overrides/bundles/cli.toml",
        r#"
schema = 1
[members.bottom]
when = 'platform = "linux"'
[members.jless]
remove = true
[members.fresh]
when = "true"
[settings.ghostty]
font_size = 14
"#,
    );
    let r = eff_ok(&dir, &chain(UBUNTU_24, "x86_64"));
    let b = &r.bundles["cli"];
    let members: Vec<(&str, Option<&str>)> =
        b.packages.iter().map(|m| (m.name(), m.when())).collect();
    assert_eq!(
        members,
        [
            ("ghostty", None),
            ("bottom", Some("platform = \"linux\"")),
            ("fresh", Some("true"))
        ]
    );
    assert_eq!(b.settings["ghostty"]["font_size"], 14);
    let at = r.effective.as_ref().unwrap().objects["bundle:cli"]
        .prov
        .get("packages.bottom.when")
        .unwrap();
    assert_eq!(at.line, Some(4));
}

#[test]
fn identity_field() {
    for (body, field) in [
        ("name = \"x\"\n", "name"),
        ("[origin]\nrepo = \"x\"\n", "origin"),
        ("[platform.linux]\nname = \"x\"\n", "name"),
    ] {
        let dir = ghostty_repo();
        write(
            dir.path(),
            "overrides/packages/ghostty/override.toml",
            &format!("schema = 1\n{body}"),
        );
        let e = effective(
            &dir,
            &chain(UBUNTU_24, "x86_64"),
            &locations(&dir, None, None),
        )
        .expect_err("identity override must fail");
        assert_eq!(e.codes(), ["override-identity-field"], "{body}");
        let d = &e.errors[0];
        assert!(d.message.contains(&format!("`{field}`")), "{}", d.message);
        assert!(d.hint.contains("pkg:ghostty"), "{}", d.hint);
        assert_eq!(
            d.provenance[0].file.as_deref(),
            Some("overrides/packages/ghostty/override.toml")
        );
        assert!(d.provenance[0].line.is_some());
    }
}

#[test]
fn machine_only_tweak() {
    let dir = ghostty_repo();
    let machine = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "overrides/packages/ghostty/override.toml",
        "schema = 1\nversion = \"1.2\"\nprefer = [\"archive\"]\n",
    );
    write(
        machine.path(),
        "overrides/packages/ghostty/override.toml",
        "schema = 1\nversion = \"1.3\"\n",
    );
    let before =
        fs::read_to_string(dir.path().join("overrides/packages/ghostty/override.toml")).unwrap();
    let locs = locations(&dir, Some(&machine), None);
    let r = effective(&dir, &chain(UBUNTU_24, "x86_64"), &locs).unwrap();
    let p = &r.packages["ghostty"];
    assert_eq!(p.version.as_deref(), Some("1.3"), "the machine value wins");
    assert_eq!(
        p.prefer,
        ["archive"],
        "untouched repo override keys survive"
    );
    assert_eq!(
        fs::read_to_string(dir.path().join("overrides/packages/ghostty/override.toml")).unwrap(),
        before,
        "the repo file stays unchanged"
    );
    let info = r.effective.as_ref().unwrap();
    let obj = &info.objects["pkg:ghostty"];
    assert_eq!(
        obj.prov.get("version").unwrap().source.describe(),
        "machine override (~/.config/dots/overrides/packages/ghostty/override.toml)"
    );
    assert_eq!(
        obj.prov.get("prefer").unwrap().source.describe(),
        "repo override (overrides/packages/ghostty/override.toml)"
    );
    assert_eq!(info.scopes_of("pkg:ghostty"), [Scope::Repo, Scope::Machine]);
    // The definition is still the definition.
    assert_eq!(obj.definition["version"], "1.1");
    assert_eq!(obj.effective["version"], "1.3");
}

#[test]
fn precedence_repo_profile_machine() {
    let dir = ghostty_repo();
    let machine = tempfile::tempdir().unwrap();
    let ov = |v: &str| {
        let ver = match v {
            "repo" => "1.2",
            _ => "1.3",
        };
        format!("schema = 1\nversion = \"{ver}\"\ndescription = \"{v}\"\n")
    };
    write(
        dir.path(),
        "overrides/packages/ghostty/override.toml",
        &ov("repo"),
    );
    write(
        dir.path(),
        "machines/laptop/overrides/packages/ghostty/override.toml",
        &ov("profile"),
    );
    // Without a machine override the profile beats the repo.
    let r = effective(
        &dir,
        &chain(UBUNTU_24, "x86_64"),
        &locations(&dir, None, Some("laptop")),
    )
    .unwrap();
    assert_eq!(r.packages["ghostty"].version.as_deref(), Some("1.3"));
    // A different profile's overrides are not read.
    let r = effective(
        &dir,
        &chain(UBUNTU_24, "x86_64"),
        &locations(&dir, None, Some("desktop")),
    )
    .unwrap();
    assert_eq!(r.packages["ghostty"].version.as_deref(), Some("1.2"));
    // The machine beats both.
    write(
        machine.path(),
        "overrides/packages/ghostty/override.toml",
        "schema = 1\nversion = \"1.4\"\n",
    );
    let r = effective(
        &dir,
        &chain(UBUNTU_24, "x86_64"),
        &locations(&dir, Some(&machine), Some("laptop")),
    )
    .unwrap();
    let p = &r.packages["ghostty"];
    assert_eq!(p.version.as_deref(), Some("1.4"));
    assert_eq!(
        p.description.as_deref(),
        Some("profile"),
        "per key, not per file"
    );
    let at = r.effective.as_ref().unwrap().objects["pkg:ghostty"]
        .prov
        .get("description")
        .unwrap();
    assert_eq!(at.source.scope, Scope::Profile);
    assert_eq!(
        at.source.describe(),
        "machine-profile override (machines/laptop/overrides/packages/ghostty/override.toml)"
    );
}

#[test]
fn platform_tables_apply_between_definition_and_overrides() {
    // definition < definition platform table < override < override platform table
    let dir = repo(&[(
        "packages/x/package.toml",
        "schema = 1\nname = \"x\"\nversion = \"1\"\n[platform.linux]\nversion = \"2\"\n",
    )]);
    write(
        dir.path(),
        "overrides/packages/x/override.toml",
        "schema = 1\nversion = \"3\"\n[platform.macos]\nversion = \"4\"\n[platform.\"ubuntu-24.04\"]\ndescription = \"u\"\n",
    );
    let r = eff_ok(&dir, &chain(UBUNTU_24, "x86_64"));
    let p = &r.packages["x"];
    assert_eq!(
        p.version.as_deref(),
        Some("3"),
        "override beats the definition's platform table"
    );
    assert_eq!(p.description.as_deref(), Some("u"));
    let at = r.effective.as_ref().unwrap().objects["pkg:x"]
        .prov
        .get("description")
        .unwrap();
    assert_eq!(
        at.source.describe(),
        "repo override, platform table [ubuntu-24.04] (overrides/packages/x/override.toml)"
    );
    let mac = eff_ok(&dir, &chain(&["any", "unix", "macos"], "aarch64"));
    assert_eq!(mac.packages["x"].version.as_deref(), Some("4"));
}

#[test]
fn package_override_grammar() {
    let dir = ghostty_repo();
    write(
        dir.path(),
        "overrides/packages/ghostty/override.toml",
        r#"
schema = 1
prefer = ["archive", "brew"]
requires = { add = ["zoxide"] }
when = 'os = "linux"'

[settings.font_size]
default = 15
[settings.theme]
values = { add = ["solarized"], remove = ["light"] }
[settings.extra]
type = "bool"
default = true

[providers.script]
remove = true
[providers.brew]
cask = true

[files."~/.config/ghostty/config"]
mode = "copy"
[files."~/.config/ghostty/themes/old"]
remove = true
[[files.add]]
dst = "~/.config/ghostty/themes/mine"
src = "themes/mine"

[platform."ubuntu-22.04"]
prefer = ["script"]
"#,
    );
    let r = eff_ok(&dir, &chain(UBUNTU_24, "x86_64"));
    let p = &r.packages["ghostty"];
    assert_eq!(p.prefer, ["archive", "brew"]);
    assert_eq!(
        p.requires.iter().map(|x| x.name()).collect::<Vec<_>>(),
        ["zoxide"]
    );
    assert_eq!(p.when.as_deref(), Some("os = \"linux\""));
    assert_eq!(p.version.as_deref(), Some("1.1"), "untouched keys remain");
    assert_eq!(p.settings["font_size"].default, Some(15.into()));
    assert_eq!(
        p.settings["font_size"].ty.as_deref(),
        Some("int"),
        "tables deep-merge"
    );
    assert_eq!(p.settings["theme"].values, ["dark", "solarized"]);
    assert!(p.settings.contains_key("extra"), "a new key adds an entry");
    assert!(!p.providers.contains_key("script"), "remove = true deletes");
    match &p.providers["brew"] {
        dots_model::ProviderConfig::Brew(b) => assert!(b.cask),
        _ => panic!(),
    }
    let dsts: Vec<String> = p
        .files
        .iter()
        .map(|f| match f.dst.as_ref().unwrap() {
            dots_model::PlatformString::Plain(s) => s.clone(),
            _ => panic!(),
        })
        .collect();
    assert_eq!(
        dsts,
        ["~/.config/ghostty/config", "~/.config/ghostty/themes/mine"]
    );
    assert_eq!(p.files[0].mode.as_deref(), Some("copy"));
    assert_eq!(p.files[0].src.as_deref(), Some("config"));

    // The 22.04-only table does not apply on 24.04, and does on 22.04.
    let r22 = eff_ok(&dir, &chain(UBUNTU_22, "x86_64"));
    assert_eq!(r22.packages["ghostty"].prefer, ["script"]);
}

#[test]
fn manifest_override() {
    let dir = repo(&[(
        "dots.toml",
        "schema = 1\n[priority]\nmacos = [\"brew\"]\n[tags]\nwork = \"work machines\"\n",
    )]);
    write(
        dir.path(),
        "overrides/dots.toml",
        "schema = 1\n[priority]\nmacos = { prepend = [\"archive\"] }\n[tags]\ngaming = \"Steam\"\n[provider.brew]\ncask_args = [\"--no-quarantine\"]\n",
    );
    let r = eff_ok(&dir, &chain(&["any", "unix", "macos"], "aarch64"));
    assert_eq!(r.manifest.priority["macos"], ["archive", "brew"]);
    assert!(r.manifest.tags.contains_key("gaming") && r.manifest.tags.contains_key("work"));
    assert!(r.manifest.provider.contains_key("brew"));
    assert_eq!(
        r.effective.as_ref().unwrap().scopes_of("repo:manifest"),
        [Scope::Repo]
    );
}

#[test]
fn manifest_override_can_declare_platform_parents() {
    // The chain function sees the effective manifest, so an override's `[platforms]` count.
    let dir = repo(&[(
        "packages/x/package.toml",
        "schema = 1\nname = \"x\"\n[platform.ubuntu]\nbin = \"u\"\n",
    )]);
    write(
        dir.path(),
        "overrides/dots.toml",
        "schema = 1\n[platforms.pop]\nparent = \"ubuntu\"\n",
    );
    let base = Repo::load(dir.path()).unwrap().value;
    let r = base
        .effective(
            &|m| {
                if m.platforms.contains_key("pop") {
                    vec!["any".into(), "ubuntu".into(), "pop".into()]
                } else {
                    vec!["any".into(), "pop".into()]
                }
            },
            &locations(&dir, None, None),
        )
        .unwrap()
        .value;
    assert_eq!(r.packages["x"].bin.as_deref(), Some("u"));
}

#[test]
fn stale_override_targets() {
    let dir = ghostty_repo();
    write(
        dir.path(),
        "overrides/packages/ghosty/override.toml",
        "schema = 1\nversion = \"1\"\n",
    );
    write(dir.path(), "overrides/bundles/clii.toml", "schema = 1\n");
    let e = effective(
        &dir,
        &chain(UBUNTU_24, "x86_64"),
        &locations(&dir, None, None),
    )
    .unwrap_err();
    assert_eq!(e.codes(), ["stale-override", "stale-override"]);
    let pkg = &e.errors[0];
    assert!(pkg.message.contains("pkg:ghosty"));
    assert!(pkg.hint.contains("pkg:ghostty"), "{}", pkg.hint);
    assert!(e.errors[1].hint.contains("bundle:cli"));
}

#[test]
fn override_files_are_validated_strictly() {
    let dir = ghostty_repo();
    write(
        dir.path(),
        "overrides/packages/ghostty/override.toml",
        "schema = 1\nversoin = \"1\"\nprefer = 3\n",
    );
    let e = effective(
        &dir,
        &chain(UBUNTU_24, "x86_64"),
        &locations(&dir, None, None),
    )
    .unwrap_err();
    let codes = e.codes();
    assert!(
        codes.contains(&"schema-unknown-key") && codes.contains(&"schema-wrong-type"),
        "{codes:?}"
    );
    let unk = e
        .errors
        .iter()
        .find(|d| d.code == "schema-unknown-key")
        .unwrap();
    assert!(unk.hint.contains("version"));
    assert_eq!(unk.provenance[0].line, Some(2));
    assert_eq!(
        unk.provenance[0].file.as_deref(),
        Some("overrides/packages/ghostty/override.toml")
    );

    write(
        dir.path(),
        "overrides/packages/ghostty/override.toml",
        "version = \"1\"\n",
    );
    let e = effective(
        &dir,
        &chain(UBUNTU_24, "x86_64"),
        &locations(&dir, None, None),
    )
    .unwrap_err();
    assert_eq!(e.codes(), ["schema-missing-key"]);
}

#[test]
fn unknown_platform_keys_in_overrides_are_collected() {
    let dir = ghostty_repo();
    write(
        dir.path(),
        "overrides/packages/ghostty/override.toml",
        "schema = 1\n[platform.\"ubunut-22.04\"]\nprefer = [\"x\"]\n",
    );
    let r = eff_ok(&dir, &chain(UBUNTU_24, "x86_64"));
    let k = r
        .platform_keys
        .iter()
        .find(|k| k.key == "ubunut-22.04")
        .unwrap();
    assert_eq!(k.file, "overrides/packages/ghostty/override.toml");
    assert_eq!(k.line, Some(2));
}

#[test]
fn redundant_override_changes_nothing_and_remove_of_missing_is_recorded() {
    let dir = ghostty_repo();
    write(
        dir.path(),
        "overrides/packages/ghostty/override.toml",
        "schema = 1\nversion = \"1.1\"\n[providers.nope]\nremove = true\n[files.\"~/gone\"]\nremove = true\n[files.\"~/renamed\"]\nmode = \"copy\"\n",
    );
    let r = eff_ok(&dir, &chain(UBUNTU_24, "x86_64"));
    assert_eq!(r.packages["ghostty"].version.as_deref(), Some("1.1"));
    assert_eq!(
        r.packages["ghostty"].files.len(),
        2,
        "a partial copy of a missing file adds nothing"
    );
    let mut paths: Vec<_> = r
        .effective
        .as_ref()
        .unwrap()
        .misses
        .iter()
        .map(|m| m.path.as_str())
        .collect();
    paths.sort_unstable();
    assert_eq!(paths, ["files.~/gone", "files.~/renamed", "providers.nope"]);
}

#[test]
fn addresses() {
    let dir = ghostty_repo();
    write(
        dir.path(),
        "overrides/packages/ghostty/override.toml",
        "schema = 1\n[settings.font_size]\ndefault = 15\n",
    );
    let r = eff_ok(&dir, &chain(UBUNTU_24, "x86_64"));
    let info = r.effective.as_ref().unwrap();

    let a = Address::parse("pkg:ghostty/setting:font_size").unwrap();
    assert_eq!(a.to_string(), "pkg:ghostty/setting:font_size");
    let f = info.lookup(&a).unwrap();
    assert_eq!(f.path, "settings.font_size");
    assert_eq!(f.effective["default"], 15);
    assert_eq!(f.definition.unwrap()["default"], 13);
    assert_eq!(
        info.objects["pkg:ghostty"]
            .prov
            .get("settings.font_size.default")
            .unwrap()
            .source
            .scope,
        Scope::Repo
    );

    let f = info
        .lookup(&Address::parse("pkg:ghostty/file:~/.config/ghostty/config").unwrap())
        .unwrap();
    assert_eq!(f.effective["src"], "config");
    assert!(
        info.lookup(&Address::parse("pkg:ghostty/provider:brew").unwrap())
            .is_some()
    );
    assert!(
        info.lookup(&Address::parse("pkg:ghostty/setting:nope").unwrap())
            .is_none()
    );
    assert!(info.lookup(&Address::parse("pkg:nope").unwrap()).is_none());
    assert!(
        info.lookup(&Address::parse("bundle:cli").unwrap())
            .is_some()
    );
    assert!(
        info.lookup(&Address::parse("repo:tags").unwrap()).is_none(),
        "no [tags] in this repo"
    );

    for ok in [
        "pkg:x",
        "pkg:x/file:~/.zshrc",
        "pkg:x/setting:a",
        "pkg:x/provider:apt",
        "pkg:x/manual:m",
        "pkg:x/hook:post_files",
        "bundle:cli",
        "bundle:cli/member:fd",
        "provider:brew",
        "priority:macos",
        "repo:tags",
        "repo:paths",
        "repo:platforms",
        "repo:env",
        "upstream:ofirgall",
        "ofirgall:ghostty",
        "ofirgall:ghostty/file:~/x",
    ] {
        let a = Address::parse(ok).unwrap_or_else(|e| panic!("{ok}: {e}"));
        assert_eq!(a.to_string(), ok);
    }
    for bad in [
        "ghostty",
        "pkg:",
        "pkg:x/nope:y",
        "bundle:x/file:y",
        "repo:other",
        "priority:macos/file:x",
        "pkg:x/member:y",
    ] {
        assert!(Address::parse(bad).is_err(), "{bad}");
    }
}

#[test]
fn a_plain_repo_is_unchanged() {
    let dir = ghostty_repo();
    let base = Repo::load(dir.path()).unwrap().value;
    let r = eff_ok(&dir, &chain(UBUNTU_24, "x86_64"));
    assert_eq!(r.packages["ghostty"], base.packages["ghostty"]);
    assert_eq!(r.bundles["cli"], base.bundles["cli"]);
    let info = r.effective.as_ref().unwrap();
    assert!(info.overrides.is_empty() && info.objects["pkg:ghostty"].prov.is_empty());
}

#[test]
fn nested_specific_platform_beats_general_top_level_table() {
    let dir = repo(&[(
        "packages/ghostty/package.toml",
        r#"
schema = 1
name = "ghostty"

[providers.brew]
name = "top"

[platform.unix.providers.brew]
name = "unix-top"

[providers.brew.platform.macos]
name = "macos-nested"

[settings.size]
type = "int"
default = 1
[platform.unix.settings.size]
default = 2
[settings.size.platform.macos]
default = 3
"#,
    )]);
    let mac = eff_ok(&dir, &chain(&["any", "unix", "macos"], "aarch64"));
    let p = &mac.packages["ghostty"];
    match &p.providers["brew"] {
        dots_model::ProviderConfig::Brew(b) => assert_eq!(b.name.as_deref(), Some("macos-nested")),
        _ => panic!(),
    }
    assert_eq!(p.settings["size"].default, Some(3.into()));
}

#[test]
fn bundle_member_name_override_is_rejected() {
    let dir = ghostty_repo();
    write(
        dir.path(),
        "overrides/bundles/cli.toml",
        "schema = 1\n[members.bottom]\nname = \"btm\"\n",
    );
    let e = effective(
        &dir,
        &chain(UBUNTU_24, "x86_64"),
        &locations(&dir, None, None),
    )
    .expect_err("renaming a member must fail");
    assert_eq!(e.codes(), ["override-identity-field"]);
    assert!(e.errors[0].message.contains("`name`"));
    assert_eq!(e.errors[0].provenance[0].line, Some(3));
}
