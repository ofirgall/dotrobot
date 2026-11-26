//! `dots adopt` end to end (task 5.10): an edited managed file goes back into the repo, and
//! status reads `ok` again. Runs the real binary in a `--home` sandbox.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::Value;

struct Sb {
    tmp: tempfile::TempDir,
    home: PathBuf,
    root: PathBuf,
    machine: PathBuf,
}

fn write(p: &Path, text: &str) {
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, text).unwrap();
}

/// `pkgs`: (name, package.toml body after the header). Every package is in bundle `all`.
fn sandbox(pkgs: &[(&str, &str)]) -> Sb {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("repo");
    write(&root.join("dots.toml"), "schema = 1\nname = \"t\"\n");
    let mut names = Vec::new();
    for (name, body) in pkgs {
        names.push(format!("\"{name}\""));
        write(
            &root.join(format!("packages/{name}/package.toml")),
            &format!("schema = 1\nname = \"{name}\"\n{body}"),
        );
    }
    write(
        &root.join("bundles/all.toml"),
        &format!("schema = 1\npackages = [{}]\n", names.join(", ")),
    );
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let machine = tmp.path().join("machine.toml");
    write(
        &machine,
        &format!(
            "schema = 1\nroot = {:?}\nbundles = [\"all\"]\n",
            root.display().to_string()
        ),
    );
    Sb {
        tmp,
        home,
        root,
        machine,
    }
}

struct Out {
    stdout: String,
    stderr: String,
    code: i32,
}

fn run(b: &Sb, args: &[&str]) -> Out {
    run_env(b, args, &[])
}

fn run_env(b: &Sb, args: &[&str], envs: &[(&str, &Path)]) -> Out {
    let o = Command::new(env!("CARGO_BIN_EXE_dots"))
        .env_clear()
        .envs(envs.iter().copied())
        .env("PATH", "/usr/bin:/bin")
        .env("SHELL", "/bin/zsh")
        .env("DOTS_PROVIDERS", "script")
        .env("HOME", b.tmp.path().join("sentinel"))
        .current_dir(b.tmp.path())
        .arg("--home")
        .arg(&b.home)
        .arg("--machine-config")
        .arg(&b.machine)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    Out {
        stdout: String::from_utf8_lossy(&o.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&o.stderr).into_owned(),
        code: o.status.code().unwrap_or(-1),
    }
}

fn json(o: &Out) -> Value {
    serde_json::from_str(&o.stdout).unwrap_or_else(|e| panic!("{e}: {} {}", o.stdout, o.stderr))
}

fn validate(doc: &Value) {
    let schema: Value = serde_json::from_str(
        &std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../schemas/dots-adopt.schema.json"),
        )
        .unwrap(),
    )
    .unwrap();
    jsonschema::validator_for(&schema)
        .unwrap()
        .validate(doc)
        .unwrap_or_else(|e| panic!("{e}\n{doc}"));
}

fn read(p: &Path) -> String {
    std::fs::read_to_string(p).unwrap()
}

/// States of the file rows in `status --all`.
fn file_states(b: &Sb) -> Vec<String> {
    let d = json(&run(b, &["status", "--all", "--json"]));
    d["data"]["items"]
        .as_array()
        .unwrap_or_else(|| panic!("{d}"))
        .iter()
        .filter(|r| r["area"] == "files")
        .map(|r| r["state"].as_str().unwrap().to_string())
        .collect()
}

/// An app saving by rename: the link is replaced by a regular file.
fn app_writes(dst: &Path, text: &str) {
    let _ = std::fs::remove_file(dst);
    std::fs::write(dst, text).unwrap();
}

fn applied(pkgs: &[(&str, &str)]) -> Sb {
    sandbox(pkgs)
}

fn apply(b: &Sb) {
    let o = run(b, &["apply", "--yes", "--json"]);
    assert_eq!(o.code, 0, "{} {}", o.stdout, o.stderr);
}

const CLAUDE: &str = r#"
[[files]]
mode = "render"
format = "json"
src = "settings.json"
dst = "{home}/.claude/settings.json"
"#;

fn claude() -> (Sb, PathBuf) {
    let b = applied(&[("claude", CLAUDE)]);
    write(
        &b.root.join("packages/claude/settings.json"),
        "{\n  \"model\": \"opus\",\n  \"env\": {\"A\": \"1\"}\n}\n",
    );
    apply(&b);
    let dst = b.home.join(".claude/settings.json");
    (b, dst)
}

#[test]
fn adopt_into_machine_scope_writes_changed_keys_and_returns_to_ok() {
    let (b, dst) = claude();
    assert_eq!(file_states(&b), ["ok"]);
    // The app adds a key, changes one and drops none.
    app_writes(
        &dst,
        "{\"model\": \"sonnet\", \"env\": {\"A\": \"1\"}, \"theme\": \"dark\"}\n",
    );
    assert_eq!(file_states(&b), ["modified"]);

    let o = run(
        &b,
        &["adopt", &dst.display().to_string(), "--machine", "--json"],
    );
    assert_eq!(o.code, 0, "{} {}", o.stdout, o.stderr);
    let d = json(&o);
    validate(&d);
    assert_eq!(d["schema"], "dots/adopt@1");
    assert_eq!(d["data"]["method"], "structural");
    assert_eq!(d["data"]["created"], true);
    let mut keys: Vec<_> = d["data"]["keys"]
        .as_array()
        .unwrap()
        .iter()
        .map(|k| k.as_str().unwrap())
        .collect();
    keys.sort();
    assert_eq!(keys, ["model", "theme"]);

    let over = b
        .home
        .join(".config/dots/overrides/packages/claude/files/home/.claude/settings.json");
    let v: Value = serde_json::from_str(&read(&over)).unwrap();
    assert_eq!(v, serde_json::json!({"model": "sonnet", "theme": "dark"}));
    // The repo is untouched; the machine is converged.
    assert!(read(&b.root.join("packages/claude/settings.json")).contains("opus"));
    assert_eq!(file_states(&b), ["ok"]);
    let now: Value = serde_json::from_str(&read(&dst)).unwrap();
    assert_eq!(now["model"], "sonnet");
    assert_eq!(now["env"]["A"], "1");
    assert_eq!(run(&b, &["plan", "--check"]).code, 0);
    // Adopting again has nothing to do.
    let o = run(&b, &["adopt", &dst.display().to_string(), "--json"]);
    assert_eq!(o.code, 0, "{}", o.stdout);
    assert_eq!(json(&o)["data"]["changed"], false);
}

#[test]
fn machine_adopt_of_a_removed_key_writes_a_delete_marker() {
    let (b, dst) = claude();
    app_writes(&dst, "{\"model\": \"opus\"}\n");
    let o = run(&b, &["adopt", "~/.claude/settings.json", "--machine"]);
    assert_eq!(o.code, 0, "{} {}", o.stdout, o.stderr);
    let over = b
        .home
        .join(".config/dots/overrides/packages/claude/files/home/.claude/settings.json");
    let v: Value = serde_json::from_str(&read(&over)).unwrap();
    assert_eq!(v, serde_json::json!({"env": "__dots_delete__"}));
    assert_eq!(file_states(&b), ["ok"]);
    let now: Value = serde_json::from_str(&read(&dst)).unwrap();
    assert_eq!(now, serde_json::json!({"model": "opus"}));
}

#[test]
fn single_layer_adopt_is_a_3way_merge_that_keeps_comments() {
    let b = applied(&[(
        "claude",
        "\n[[files]]\nmode = \"render\"\nformat = \"jsonc\"\nsrc = \"settings.jsonc\"\ndst = \"{home}/s.jsonc\"\n",
    )]);
    let src = b.root.join("packages/claude/settings.jsonc");
    write(&src, "{\n  // keep me\n  \"a\": 1,\n  \"b\": 2\n}\n");
    apply(&b);
    let dst = b.home.join("s.jsonc");
    // The app writes through the link, changing `b`.
    std::fs::write(
        &dst,
        std::fs::read_to_string(&dst)
            .unwrap()
            .replace("\"b\": 2", "\"b\": 3"),
    )
    .unwrap();
    assert_eq!(file_states(&b), ["modified"]);
    let o = run(&b, &["adopt", &dst.display().to_string(), "--json"]);
    assert_eq!(o.code, 0, "{} {}", o.stdout, o.stderr);
    assert_eq!(json(&o)["data"]["method"], "text-merge");
    assert_eq!(read(&src), "{\n  // keep me\n  \"a\": 1,\n  \"b\": 3\n}\n");
    assert_eq!(file_states(&b), ["ok"]);
}

const TWO_LAYERS: &str = r#"
[[files]]
mode = "render"
dst = "{config}/app/conf.toml"
layers = ["layers/common.toml", "layers/extra.toml"]
"#;

fn two_layers() -> (Sb, PathBuf) {
    let b = applied(&[("app", TWO_LAYERS)]);
    let p = b.root.join("packages/app");
    write(
        &p.join("layers/common.toml"),
        "# base\na = 1\n[s]\nx = \"common\"\n",
    );
    write(&p.join("layers/extra.toml"), "b = 2\n[s]\ny = \"extra\"\n");
    apply(&b);
    let dst = b.home.join(".config/app/conf.toml");
    (b, dst)
}

#[test]
fn default_layer_is_the_most_specific_and_only_changed_keys_go_in() {
    let (b, dst) = two_layers();
    app_writes(
        &dst,
        "a = 1\nb = 20\nc = 3\n[s]\nx = \"common\"\ny = \"extra\"\n",
    );
    let o = run(&b, &["adopt", &dst.display().to_string(), "--json"]);
    assert_eq!(o.code, 0, "{} {}", o.stdout, o.stderr);
    let d = json(&o);
    validate(&d);
    assert!(
        d["data"]["layer"]
            .as_str()
            .unwrap()
            .ends_with("layers/extra.toml"),
        "{d}"
    );
    let p = b.root.join("packages/app/layers");
    assert_eq!(
        read(&p.join("common.toml")),
        "# base\na = 1\n[s]\nx = \"common\"\n"
    );
    let extra = read(&p.join("extra.toml"));
    assert!(
        extra.contains("b = 20") && extra.contains("c = 3"),
        "{extra}"
    );
    assert_eq!(file_states(&b), ["ok"]);
}

#[test]
fn layer_flag_picks_the_layer_and_keeps_its_comments() {
    let (b, dst) = two_layers();
    app_writes(&dst, "a = 5\nb = 2\n[s]\nx = \"common\"\ny = \"extra\"\n");
    let o = run(
        &b,
        &["adopt", &dst.display().to_string(), "--layer", "common"],
    );
    assert_eq!(o.code, 0, "{} {}", o.stdout, o.stderr);
    let p = b.root.join("packages/app/layers");
    assert_eq!(
        read(&p.join("common.toml")),
        "# base\na = 5\n[s]\nx = \"common\"\n"
    );
    assert_eq!(read(&p.join("extra.toml")), "b = 2\n[s]\ny = \"extra\"\n");
    assert_eq!(file_states(&b), ["ok"]);
}

#[test]
fn removing_a_key_a_lower_layer_defines_writes_a_delete_marker() {
    let (b, dst) = two_layers();
    app_writes(&dst, "b = 2\n[s]\nx = \"common\"\ny = \"extra\"\n");
    let o = run(&b, &["adopt", &dst.display().to_string()]);
    assert_eq!(o.code, 0, "{} {}", o.stdout, o.stderr);
    let extra = read(&b.root.join("packages/app/layers/extra.toml"));
    assert!(extra.contains("a = \"__dots_delete__\""), "{extra}");
    assert_eq!(file_states(&b), ["ok"]);
    assert!(!read(&dst).contains("a = 1"));
}

#[test]
fn an_edit_a_later_layer_overrides_is_refused_and_nothing_is_written() {
    let (b, dst) = two_layers();
    // `s.y` is set by extra; adopting a change to it into common cannot take effect.
    app_writes(&dst, "a = 1\nb = 2\n[s]\nx = \"common\"\ny = \"new\"\n");
    let before = read(&b.root.join("packages/app/layers/common.toml"));
    let o = run(
        &b,
        &[
            "adopt",
            &dst.display().to_string(),
            "--layer",
            "common",
            "--json",
        ],
    );
    assert_eq!(o.code, 1, "{}", o.stdout);
    assert_eq!(json(&o)["errors"][0]["code"], "adopt-ineffective");
    assert_eq!(
        read(&b.root.join("packages/app/layers/common.toml")),
        before
    );
    assert_eq!(file_states(&b), ["modified"]);
}

#[test]
fn unknown_layer_lists_the_real_ones() {
    let (b, dst) = two_layers();
    app_writes(&dst, "a = 2\n");
    let o = run(
        &b,
        &[
            "adopt",
            &dst.display().to_string(),
            "--layer",
            "nope",
            "--json",
        ],
    );
    assert_eq!(o.code, 1);
    let d = json(&o);
    assert_eq!(d["errors"][0]["code"], "adopt-unknown-layer");
    assert!(
        d["errors"][0]["message"]
            .as_str()
            .unwrap()
            .contains("layers/common.toml")
    );
}

const TEMPLATED: &str = r#"
[[files]]
mode = "render"
format = "text"
template = "jinja"
src = "conf.tmpl"
dst = "{home}/app.conf"
"#;

fn templated() -> (Sb, PathBuf) {
    let b = applied(&[("app", TEMPLATED)]);
    write(
        &b.root.join("packages/app/conf.tmpl"),
        "os = {{ os }}\nkeep = 1\nmid = a\ngap = 0\nend = z\n",
    );
    apply(&b);
    let dst = b.home.join("app.conf");
    (b, dst)
}

#[test]
fn text_adopt_keeps_template_expressions_the_edit_did_not_touch() {
    let (b, dst) = templated();
    let os = if cfg!(target_os = "macos") {
        "macos"
    } else {
        "linux"
    };
    assert!(read(&dst).contains(&format!("os = {os}")), "{}", read(&dst));
    app_writes(&dst, &read(&dst).replace("mid = a", "mid = b"));
    let o = run(&b, &["adopt", &dst.display().to_string(), "--json"]);
    assert_eq!(o.code, 0, "{} {}", o.stdout, o.stderr);
    assert_eq!(json(&o)["data"]["method"], "text-merge");
    assert_eq!(
        read(&b.root.join("packages/app/conf.tmpl")),
        "os = {{ os }}\nkeep = 1\nmid = b\ngap = 0\nend = z\n"
    );
    assert_eq!(file_states(&b), ["ok"]);
}

#[test]
fn text_adopt_merges_a_source_change_made_elsewhere() {
    let (b, dst) = templated();
    let src = b.root.join("packages/app/conf.tmpl");
    // The repo moved on (line 4) while the app edited line 3.
    write(&src, "os = {{ os }}\nkeep = 1\nmid = a\ngap = 0\nend = Z\n");
    app_writes(&dst, &read(&dst).replace("mid = a", "mid = b"));
    assert_eq!(file_states(&b), ["modified"]);
    let o = run(&b, &["adopt", &dst.display().to_string()]);
    assert_eq!(o.code, 0, "{} {}", o.stdout, o.stderr);
    assert_eq!(
        read(&src),
        "os = {{ os }}\nkeep = 1\nmid = b\ngap = 0\nend = Z\n"
    );
    assert_eq!(file_states(&b), ["ok"]);
}

#[test]
fn text_adopt_conflict_fails_cleanly_and_writes_nothing() {
    let (b, dst) = templated();
    let src = b.root.join("packages/app/conf.tmpl");
    write(
        &src,
        "os = {{ os }}\nkeep = 1\nmid = SRC\ngap = 0\nend = z\n",
    );
    app_writes(&dst, &read(&dst).replace("mid = a", "mid = APP"));
    let o = run(&b, &["adopt", &dst.display().to_string(), "--json"]);
    assert_eq!(o.code, 1, "{}", o.stdout);
    let d = json(&o);
    assert_eq!(d["errors"][0]["code"], "adopt-conflict");
    let m = d["errors"][0]["message"].as_str().unwrap();
    assert!(m.contains("mid = SRC") && m.contains("mid = APP"), "{m}");
    assert_eq!(
        read(&src),
        "os = {{ os }}\nkeep = 1\nmid = SRC\ngap = 0\nend = z\n"
    );
    assert_eq!(file_states(&b), ["modified"]);
}

#[test]
fn text_adopt_cannot_go_to_the_machine_scope() {
    let (b, dst) = templated();
    app_writes(&dst, &read(&dst).replace("mid = a", "mid = b"));
    let o = run(
        &b,
        &["adopt", &dst.display().to_string(), "--machine", "--json"],
    );
    assert_eq!(o.code, 1);
    assert_eq!(json(&o)["errors"][0]["code"], "adopt-unsupported");
}

#[test]
fn copy_and_inject_files_adopt_into_their_source() {
    let b = applied(&[(
        "app",
        r#"
[[files]]
mode = "copy"
src = "plain.txt"
dst = "{home}/plain.txt"

[[files]]
mode = "inject"
src = "block.sh"
dst = "{home}/.profile"
"#,
    )]);
    let p = b.root.join("packages/app");
    write(&p.join("plain.txt"), "one\ntwo\n");
    write(&p.join("block.sh"), "export A=1\n");
    write(&b.home.join(".profile"), "# mine\n");
    apply(&b);
    assert_eq!(file_states(&b), ["ok", "ok"]);

    std::fs::write(b.home.join("plain.txt"), "one\nTWO\n").unwrap();
    let prof = read(&b.home.join(".profile")).replace("export A=1", "export A=2");
    std::fs::write(b.home.join(".profile"), prof).unwrap();
    assert!(
        file_states(&b).iter().all(|s| s == "modified"),
        "{:?}",
        file_states(&b)
    );

    let o = run(&b, &["adopt", "~/plain.txt", "--json"]);
    assert_eq!(o.code, 0, "{} {}", o.stdout, o.stderr);
    assert_eq!(json(&o)["data"]["method"], "copy");
    assert_eq!(read(&p.join("plain.txt")), "one\nTWO\n");
    let o = run(&b, &["adopt", "~/.profile", "--json"]);
    assert_eq!(o.code, 0, "{} {}", o.stdout, o.stderr);
    assert_eq!(json(&o)["data"]["method"], "inject");
    assert_eq!(read(&p.join("block.sh")), "export A=2\n");
    assert_eq!(file_states(&b), ["ok", "ok"]);
    assert!(read(&b.home.join(".profile")).starts_with("# mine\n"));
}

#[test]
fn as_patch_is_not_implemented_yet() {
    let (b, dst) = claude();
    app_writes(&dst, "{}\n");
    let o = run(
        &b,
        &["adopt", &dst.display().to_string(), "--as-patch", "--json"],
    );
    assert_eq!(o.code, 1);
    assert_eq!(json(&o)["errors"][0]["code"], "not-implemented");
    assert_eq!(file_states(&b), ["modified"]);
}

#[test]
fn refusals_name_the_reason() {
    let (b, dst) = claude();
    let o = run(&b, &["adopt", "~/nowhere", "--json"]);
    assert_eq!(json(&o)["errors"][0]["code"], "adopt-not-managed");
    // A file that was deleted is `missing`, not adoptable.
    std::fs::remove_file(&dst).unwrap();
    let o = run(&b, &["adopt", &dst.display().to_string(), "--json"]);
    assert_eq!(o.code, 1);
    assert_eq!(json(&o)["errors"][0]["code"], "adopt-not-modified");
    let o = run(
        &b,
        &[
            "adopt",
            &dst.display().to_string(),
            "--machine",
            "--layer",
            "x",
        ],
    );
    assert_eq!(o.code, 64, "{}", o.stderr);
}

#[test]
fn a_linked_file_has_nothing_to_adopt() {
    let b = applied(&[("app", "\n[[files]]\nsrc = \"rc\"\ndst = \"{home}/.rc\"\n")]);
    write(&b.root.join("packages/app/rc"), "x\n");
    apply(&b);
    let o = run(&b, &["adopt", "~/.rc", "--json"]);
    assert_eq!(o.code, 1);
    assert_eq!(json(&o)["errors"][0]["code"], "adopt-unsupported");
}

const PER_PLATFORM: &str = "\n[[files]]\nmode = \"render\"\nformat = \"json\"\nsrc = \"settings.json\"\ndst = { macos = \"{home}/.claude/settings.json\", linux = \"{home}/.claude/settings.json\" }\n";

#[test]
fn machine_adopt_with_a_per_platform_dst_survives_the_next_apply() {
    let b = applied(&[("claude", PER_PLATFORM)]);
    write(
        &b.root.join("packages/claude/settings.json"),
        "{\n  \"model\": \"opus\"\n}\n",
    );
    apply(&b);
    let dst = b.home.join(".claude/settings.json");
    app_writes(&dst, "{\"model\": \"sonnet\"}\n");
    let o = run(
        &b,
        &["adopt", &dst.display().to_string(), "--machine", "--json"],
    );
    assert_eq!(o.code, 0, "{} {}", o.stdout, o.stderr);
    assert_eq!(file_states(&b), ["ok"]);
    apply(&b);
    assert!(read(&dst).contains("sonnet"), "{}", read(&dst));
    assert_eq!(file_states(&b), ["ok"]);
}

#[test]
fn adopt_into_a_symlinked_source_keeps_the_link() {
    let b = applied(&[(
        "app",
        "\n[[files]]\nmode = \"render\"\nsrc = \"conf\"\ndst = \"{home}/app.conf\"\n",
    )]);
    let shared = b.root.join("shared/conf");
    write(&shared, "a = 1\nb = 2\n");
    let link = b.root.join("packages/app/conf");
    std::os::unix::fs::symlink("../../shared/conf", &link).unwrap();
    apply(&b);
    let dst = b.home.join("app.conf");
    app_writes(&dst, "a = 1\nb = 3\n");
    let o = run(&b, &["adopt", &dst.display().to_string(), "--json"]);
    assert_eq!(o.code, 0, "{} {}", o.stdout, o.stderr);
    assert!(
        std::fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink(),
        "the layer symlink was replaced by a regular file"
    );
    assert!(read(&shared).contains("b = 3"), "{}", read(&shared));
}

#[test]
fn machine_layers_of_files_with_the_same_name_do_not_collide() {
    let body = "\n[[files]]\nmode = \"render\"\nformat = \"json\"\nsrc = \"a.json\"\ndst = \"{home}/.a/settings.json\"\n\n[[files]]\nmode = \"render\"\nformat = \"json\"\nsrc = \"b.json\"\ndst = \"{home}/.b/settings.json\"\n";
    let b = applied(&[("app", body)]);
    write(&b.root.join("packages/app/a.json"), "{\"k\": 1}\n");
    write(&b.root.join("packages/app/b.json"), "{\"k\": 2}\n");
    apply(&b);
    for (dir, v) in [(".a", 10), (".b", 20)] {
        let dst = b.home.join(dir).join("settings.json");
        app_writes(&dst, &format!("{{\"k\": {v}}}\n"));
        let o = run(&b, &["adopt", &dst.display().to_string(), "--machine"]);
        assert_eq!(o.code, 0, "{} {}", o.stdout, o.stderr);
    }
    assert_eq!(file_states(&b), ["ok", "ok"]);
    let base = b
        .home
        .join(".config/dots/overrides/packages/app/files/home");
    assert!(read(&base.join(".a/settings.json")).contains("10"));
    assert!(read(&base.join(".b/settings.json")).contains("20"));
    assert!(
        !b.home
            .join(".config/dots/overrides/packages/app/settings.json")
            .exists()
    );
}

#[test]
fn structural_adopt_refuses_a_layer_whose_comments_it_would_lose() {
    let body = "\n[[files]]\nmode = \"render\"\nformat = \"jsonc\"\nsrc = \"s.jsonc\"\ndst = \"{home}/.c/s.jsonc\"\n";
    let b = applied(&[("app", body)]);
    write(&b.root.join("packages/app/s.jsonc"), "{\"k\": 1}\n");
    let layer = b
        .home
        .join(".config/dots/overrides/packages/app/files/home/.c/s.jsonc");
    let text = "{\n  // keep me\n  \"x\": 1\n}\n";
    write(&layer, text);
    apply(&b);
    let dst = b.home.join(".c/s.jsonc");
    app_writes(&dst, "{\"k\": 1, \"x\": 2}\n");
    let o = run(
        &b,
        &["adopt", &dst.display().to_string(), "--machine", "--json"],
    );
    assert_ne!(o.code, 0, "{}", o.stdout);
    assert!(o.stdout.contains("adopt-unsupported"), "{}", o.stdout);
    assert_eq!(read(&layer), text);
}

#[test]
fn an_unreadable_machine_layer_is_not_overwritten_or_deleted() {
    let (b, dst) = claude();
    // A directory where the layer file would be: reading it fails, but not with NotFound.
    let layer = b
        .home
        .join(".config/dots/overrides/packages/claude/files/home/.claude/settings.json");
    std::fs::create_dir_all(&layer).unwrap();
    app_writes(&dst, "{\"model\": \"sonnet\", \"env\": {\"A\": \"1\"}}\n");
    let o = run(
        &b,
        &["adopt", &dst.display().to_string(), "--machine", "--json"],
    );
    assert_ne!(o.code, 0, "{}", o.stdout);
    assert!(o.stdout.contains("file-unreadable"), "{}", o.stdout);
    assert!(layer.is_dir());
}

#[test]
fn adopt_takes_the_run_lock() {
    let (b, dst) = claude();
    app_writes(&dst, "{\"model\": \"sonnet\", \"env\": {\"A\": \"1\"}}\n");
    let _held = dots_engine::RunLock::acquire(&b.home.join(".local/state/dots/run.lock")).unwrap();
    let o = run(&b, &["adopt", &dst.display().to_string(), "--json"]);
    assert_eq!(o.code, 75, "{} {}", o.stdout, o.stderr);
    assert!(o.stdout.contains("lock-busy"), "{}", o.stdout);
}
