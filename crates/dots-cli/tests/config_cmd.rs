//! Tasks 5.2 and 5.3: package settings through the real binary, in a `--home` sandbox.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::{Value, json};

struct Sb {
    tmp: tempfile::TempDir,
    home: PathBuf,
    root: PathBuf,
    machine: PathBuf,
}

struct Out {
    stdout: String,
    stderr: String,
    code: i32,
}

impl Out {
    fn json(&self) -> Value {
        serde_json::from_str(&self.stdout)
            .unwrap_or_else(|e| panic!("{e}: {} {}", self.stdout, self.stderr))
    }
    fn all(&self) -> String {
        format!("{}{}", self.stdout, self.stderr)
    }
}

fn write(p: &Path, text: &str) {
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, text).unwrap();
}

const GHOSTTY: &str = r#"schema = 1
name = "ghostty"
[settings.font_size]
type = "int"
default = 13
range = [8, 32]
description = "Terminal font size"
[settings.theme]
type = "enum"
values = ["dark", "light"]
default = "dark"
ask = true
question = "Dark or light terminal theme?"
[settings.api_token]
type = "string"
required = true
secret = true
"#;

const GIT: &str = r#"schema = 1
name = "git"
[settings.git_email]
type = "string"
required = true
validate = '^[^@]+@[^@]+$'

[[files]]
mode = "render"
format = "toml"
dst = "{config}/git/identity.toml"
layers = ["layers/identity.toml"]
template = "jinja"
"#;

/// Repo `dots.toml` text, `machine.toml` extra text.
fn sandbox(dots_toml: &str, machine_extra: &str) -> Sb {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("repo");
    write(&root.join("dots.toml"), dots_toml);
    write(&root.join("packages/ghostty/package.toml"), GHOSTTY);
    write(&root.join("packages/git/package.toml"), GIT);
    write(
        &root.join("packages/git/layers/identity.toml"),
        "email = \"{{ settings.git.git_email }}\"\n",
    );
    write(
        &root.join("packages/tools/package.toml"),
        "schema = 1\nname = \"tools\"\n[[files]]\nsrc = \"files/rc\"\ndst = \"{home}/.toolsrc\"\n",
    );
    write(&root.join("packages/tools/files/rc"), "x\n");
    write(
        &root.join("bundles/base.toml"),
        "schema = 1\ndescription = \"the basics\"\npackages = [\"ghostty\", \"git\", \"tools\"]\n",
    );
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let machine = tmp.path().join("machine.toml");
    write(
        &machine,
        &format!(
            "schema = 1\nroot = {:?}\n{machine_extra}",
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

fn plain_sb() -> Sb {
    sandbox("schema = 1\nname = \"t\"\n", "bundles = [\"base\"]\n")
}

fn run(b: &Sb, args: &[&str]) -> Out {
    let o = Command::new(env!("CARGO_BIN_EXE_dots"))
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("SHELL", "/bin/zsh")
        .env("DOTS_PROVIDERS", "none")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
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

fn validate(command: &str, doc: &Value) {
    let schema: Value = serde_json::from_str(
        &std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join(format!("../../schemas/dots-{command}.schema.json")),
        )
        .unwrap(),
    )
    .unwrap();
    let v = jsonschema::validator_for(&schema).unwrap();
    if let Err(e) = v.validate(doc) {
        panic!("{command} output violates its schema: {e}\n{doc:#}");
    }
}

/// `--json` run that must validate against the command's schema.
fn jrun(b: &Sb, command: &str, args: &[&str]) -> (Out, Value) {
    let mut a = vec!["--json"];
    a.extend_from_slice(args);
    let o = run(b, &a);
    let doc = o.json();
    validate(command, &doc);
    (o, doc)
}

fn data(doc: &Value) -> &Value {
    &doc["data"]
}

// ------------------------------------------------------------------ explain / get

#[test]
fn explain_lists_every_scope_and_marks_the_winner() {
    let b = sandbox(
        "schema = 1\nname = \"t\"\n[settings.ghostty]\nfont_size = 14\n",
        "bundles = [\"base\"]\n[settings.ghostty]\nfont_size = 15\n",
    );
    let o = run(
        &b,
        &["config", "explain", "ghostty.font_size", "--color", "never"],
    );
    assert_eq!(o.code, 0, "{}", o.all());
    let line = |scope: &str| {
        o.stdout
            .lines()
            .find(|l| l.trim_start().starts_with(scope))
            .unwrap_or_else(|| panic!("no {scope} line in {}", o.stdout))
            .to_string()
    };
    assert!(line("default").contains("13"));
    assert!(line("bundle").trim_end().ends_with('-'));
    assert!(line("upstream").trim_end().ends_with('-'));
    assert!(line("repo").contains("14") && line("repo").contains("dots.toml"));
    assert!(line("profile").trim_end().ends_with('-'));
    assert!(line("machine").contains("15") && line("machine").contains("(wins)"));
    assert!(!line("repo").contains("(wins)"));

    let (_, doc) = jrun(&b, "config", &["config", "explain", "ghostty.font_size"]);
    let d = data(&doc);
    assert_eq!(d["winner"], "machine");
    assert_eq!(d["value"], 15);
    let scopes = d["scopes"].as_array().unwrap();
    assert_eq!(scopes.len(), 6);
    assert_eq!(scopes[3]["value"], 14);
    assert_eq!(scopes[1]["set"], false);
    assert_eq!(scopes[5]["winner"], true);
}

#[test]
fn machine_wins_over_repo_in_get() {
    let b = sandbox(
        "schema = 1\nname = \"t\"\n[settings.ghostty]\nfont_size = 14\n",
        "bundles = [\"base\"]\n[settings.ghostty]\nfont_size = 15\n",
    );
    let o = run(
        &b,
        &["config", "get", "ghostty.font_size", "--color", "never"],
    );
    assert_eq!(o.stdout.trim(), "15", "{}", o.all());
}

#[test]
fn get_of_a_required_setting_without_a_value_points_at_set() {
    let b = plain_sb();
    let (o, doc) = jrun(&b, "config", &["config", "get", "git.git_email"]);
    assert_ne!(o.code, 0);
    assert_eq!(doc["errors"][0]["code"], "setting-required");
    assert_eq!(
        doc["errors"][0]["hint"],
        "dots config set git.git_email <value>"
    );
}

#[test]
fn unknown_setting_suggests_the_closest() {
    let b = plain_sb();
    let (o, doc) = jrun(&b, "config", &["config", "get", "ghostty.font_sze"]);
    assert_ne!(o.code, 0);
    assert_eq!(doc["errors"][0]["code"], "setting-unknown");
    assert!(
        doc["errors"][0]["message"]
            .as_str()
            .unwrap()
            .contains("ghostty.font_size")
    );
    let (_, doc) = jrun(&b, "config", &["config", "get", "nodot"]);
    assert_eq!(doc["errors"][0]["code"], "setting-key-invalid");
}

// ------------------------------------------------------------------------- list

#[test]
fn list_json_redacts_secrets() {
    let secret = "s3cr3t-token-value";
    let b = sandbox(
        "schema = 1\nname = \"t\"\n",
        &format!("bundles = [\"base\"]\n[settings.ghostty]\napi_token = \"{secret}\"\n"),
    );
    let (o, doc) = jrun(&b, "config", &["config", "list"]);
    assert_eq!(o.code, 0, "{}", o.all());
    let t = data(&doc)["settings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["id"] == "ghostty.api_token")
        .unwrap()
        .clone();
    assert_eq!(t["value"], Value::Null);
    assert_eq!(t["redacted"], true);
    assert_eq!(t["secret"], true);
    assert_eq!(t["scope"], "machine");
    // The secret value is nowhere in any output of any config command.
    for args in [
        vec!["config", "list"],
        vec!["config", "get", "ghostty.api_token"],
        vec!["config", "explain", "ghostty.api_token"],
    ] {
        for json in [true, false] {
            let mut a: Vec<&str> = if json { vec!["--json"] } else { vec![] };
            a.extend(args.iter().copied());
            let o = run(&b, &a);
            assert!(
                !o.all().contains(secret),
                "{args:?} json={json}: {}",
                o.all()
            );
        }
    }
    let o = run(
        &b,
        &["config", "get", "ghostty.api_token", "--color", "never"],
    );
    assert_eq!(o.stdout.trim(), "•••");
    // An unset secret is reported as missing, not redacted.
    let b2 = plain_sb();
    let (_, doc) = jrun(&b2, "config", &["config", "list"]);
    let t = data(&doc)["settings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["id"] == "ghostty.api_token")
        .unwrap()
        .clone();
    assert_eq!(
        (t["missing"].clone(), t["redacted"].clone()),
        (json!(true), json!(false))
    );
}

#[test]
fn list_human_shows_scope_and_a_fix_for_missing() {
    let b = plain_sb();
    let o = run(&b, &["config", "list", "--color", "never"]);
    assert_eq!(o.code, 0, "{}", o.all());
    assert!(
        o.stdout.contains("ghostty.font_size") && o.stdout.contains("13"),
        "{}",
        o.stdout
    );
    assert!(
        o.stdout.contains("dots config set git.git_email <value>"),
        "{}",
        o.stdout
    );
}

// ------------------------------------------------------------------- set / unset

#[test]
fn set_defaults_to_machine_scope_and_unset_falls_back() {
    let b = plain_sb();
    let (o, doc) = jrun(&b, "config", &["config", "set", "ghostty.font_size", "15"]);
    assert_eq!(o.code, 0, "{}", o.all());
    assert_eq!(data(&doc)["scope"], "machine");
    assert_eq!(data(&doc)["effective_value"], 15);
    let m = std::fs::read_to_string(&b.machine).unwrap();
    assert!(m.contains("[settings.ghostty]\nfont_size = 15"), "{m}");
    assert!(m.contains("bundles = [\"base\"]"), "{m}");
    let o = run(
        &b,
        &["config", "get", "ghostty.font_size", "--color", "never"],
    );
    assert_eq!(o.stdout.trim(), "15");

    let (o, doc) = jrun(&b, "config", &["config", "unset", "ghostty.font_size"]);
    assert_eq!(o.code, 0, "{}", o.all());
    assert_eq!(data(&doc)["removed"], true);
    assert_eq!(data(&doc)["effective_scope"], "default");
    let o = run(
        &b,
        &["config", "get", "ghostty.font_size", "--color", "never"],
    );
    assert_eq!(o.stdout.trim(), "13");
    // Idempotent.
    let (_, doc) = jrun(&b, "config", &["config", "unset", "ghostty.font_size"]);
    assert_eq!(data(&doc)["removed"], false);
}

#[test]
fn set_repo_edits_dots_toml_and_keeps_its_formatting() {
    let original = "# my repo\nschema = 1\nname = \"t\"   # keep me\n\n[settings.ghostty]\n# size\nfont_size = 14 # old\n";
    let b = sandbox(original, "bundles = [\"base\"]\n");
    let (o, doc) = jrun(
        &b,
        "config",
        &["config", "set", "ghostty.font_size", "16", "--repo"],
    );
    assert_eq!(o.code, 0, "{}", o.all());
    assert_eq!(data(&doc)["scope"], "repo");
    let t = std::fs::read_to_string(b.root.join("dots.toml")).unwrap();
    assert_eq!(t, original.replace("14 # old", "16 # old"));
    // The machine file was not touched.
    assert!(
        !std::fs::read_to_string(&b.machine)
            .unwrap()
            .contains("settings")
    );

    // A machine value still wins; the command says so.
    run(&b, &["config", "set", "ghostty.font_size", "20"]);
    let (_, doc) = jrun(
        &b,
        "config",
        &["config", "set", "ghostty.font_size", "17", "--repo"],
    );
    assert_eq!(data(&doc)["effective_value"], 20);
    assert_eq!(data(&doc)["effective_scope"], "machine");
    assert!(!doc["warnings"].as_array().unwrap().is_empty());
}

#[test]
fn set_repo_commit_makes_a_git_commit() {
    let b = plain_sb();
    let git = |args: &[&str]| {
        let o = Command::new("git")
            .arg("-C")
            .arg(&b.root)
            .args(["-c", "user.name=t", "-c", "user.email=t@t"])
            .args(args)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .output()
            .unwrap();
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
        String::from_utf8_lossy(&o.stdout).into_owned()
    };
    git(&["init", "-q"]);
    git(&["add", "."]);
    git(&["commit", "-q", "-m", "init"]);
    // `git commit` inside dots needs an identity; the sandbox has none, so give the repo one.
    git(&["config", "user.name", "t"]);
    git(&["config", "user.email", "t@t"]);
    let o = run(
        &b,
        &[
            "config",
            "set",
            "ghostty.font_size",
            "18",
            "--repo",
            "--commit",
        ],
    );
    assert_eq!(o.code, 0, "{}", o.all());
    assert!(git(&["log", "--oneline"]).contains("config: set ghostty.font_size = 18"));
    assert!(git(&["status", "--porcelain"]).trim().is_empty());
}

#[test]
fn set_validates_type_enum_and_pattern() {
    let b = plain_sb();
    let (o, doc) = jrun(&b, "config", &["config", "set", "ghostty.font_size", "big"]);
    assert_ne!(o.code, 0);
    assert_eq!(doc["errors"][0]["code"], "setting-invalid");
    assert!(
        doc["errors"][0]["message"]
            .as_str()
            .unwrap()
            .contains("expected int")
    );
    let (o, _) = jrun(&b, "config", &["config", "set", "ghostty.theme", "neon"]);
    assert_ne!(o.code, 0);
    let (o, doc) = jrun(&b, "config", &["config", "set", "git.git_email", "nope"]);
    assert_ne!(o.code, 0);
    assert!(
        doc["errors"][0]["message"]
            .as_str()
            .unwrap()
            .contains("does not match")
    );
    // Nothing was written.
    assert!(
        !std::fs::read_to_string(&b.machine)
            .unwrap()
            .contains("settings")
    );
}

#[test]
fn secrets_are_machine_only_and_the_file_is_private() {
    let secret = "tok-123456";
    let b = plain_sb();
    let (o, doc) = jrun(
        &b,
        "config",
        &["config", "set", "ghostty.api_token", secret, "--repo"],
    );
    assert_ne!(o.code, 0);
    assert_eq!(doc["errors"][0]["code"], "setting-secret-scope");
    assert!(
        !std::fs::read_to_string(b.root.join("dots.toml"))
            .unwrap()
            .contains(secret)
    );

    let (o, doc) = jrun(
        &b,
        "config",
        &["config", "set", "ghostty.api_token", secret],
    );
    assert_eq!(o.code, 0, "{}", o.all());
    assert!(!o.all().contains(secret), "{}", o.all());
    assert_eq!(data(&doc)["redacted"], true);
    assert_eq!(data(&doc)["value"], Value::Null);
    assert!(
        std::fs::read_to_string(&b.machine)
            .unwrap()
            .contains(secret)
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&b.machine).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }
    // A committed secret is an error everywhere.
    let b = sandbox(
        "schema = 1\nname = \"t\"\n[settings.ghostty]\napi_token = \"leak\"\n",
        "bundles = [\"base\"]\n",
    );
    let o = run(&b, &["--json", "plan"]);
    assert!(
        o.json()["errors"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["code"] == "setting-secret-scope")
    );
}

#[test]
fn wrong_type_in_dots_toml_names_the_file_and_type() {
    let b = sandbox(
        "schema = 1\nname = \"t\"\n[settings.ghostty]\nfont_size = \"big\"\n",
        "bundles = [\"base\"]\n",
    );
    let o = run(&b, &["--json", "plan"]);
    assert_ne!(o.code, 0);
    let e = &o.json()["errors"][0];
    assert_eq!(e["code"], "setting-invalid");
    let m = e["message"].as_str().unwrap();
    assert!(m.contains("dots.toml") && m.contains("expected int"), "{m}");
    // `lint` reports it too, and `config list` still works so it can be fixed.
    let o = run(&b, &["--json", "lint"]);
    assert!(
        o.json()["errors"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["code"] == "setting-invalid")
    );
    assert_eq!(run(&b, &["config", "list"]).code, 0);
}

#[test]
fn conflicting_bundles_fail_resolution_naming_both() {
    let b = plain_sb();
    write(
        &b.root.join("bundles/a.toml"),
        "schema = 1\n[settings.ghostty]\nfont_size = 13\n",
    );
    write(
        &b.root.join("bundles/b.toml"),
        "schema = 1\n[settings.ghostty]\nfont_size = 14\n",
    );
    write(
        &b.machine,
        &format!(
            "schema = 1\nroot = {:?}\nbundles = [\"base\", \"a\", \"b\"]\n",
            b.root.display().to_string()
        ),
    );
    let o = run(&b, &["--json", "plan"]);
    assert_ne!(o.code, 0);
    let e = o.json()["errors"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["code"] == "setting-conflict")
        .cloned()
        .unwrap();
    let m = e["message"].as_str().unwrap();
    assert!(m.contains("bundle `a`") && m.contains("bundle `b`"), "{m}");
    // Setting it at the machine scope fixes it.
    assert_eq!(
        run(&b, &["config", "set", "ghostty.font_size", "13"]).code,
        0
    );
    let o = run(&b, &["--json", "plan"]);
    assert!(!o.stdout.contains("setting-conflict"), "{}", o.stdout);
}

#[test]
fn a_setting_whose_when_is_false_cannot_be_set() {
    let b = plain_sb();
    let p = b.root.join("packages/ghostty/package.toml");
    let mut t = std::fs::read_to_string(&p).unwrap();
    t.push_str("[settings.shader]\ntype = \"bool\"\ndefault = false\nwhen = 'os = \"plan9\"'\n");
    std::fs::write(&p, t).unwrap();
    let (o, doc) = jrun(&b, "config", &["config", "set", "ghostty.shader", "true"]);
    assert_ne!(o.code, 0);
    assert_eq!(doc["errors"][0]["code"], "setting-inapplicable");
    let (_, doc) = jrun(&b, "config", &["config", "list"]);
    assert!(
        data(&doc)["settings"]
            .as_array()
            .unwrap()
            .iter()
            .all(|s| s["id"] != "ghostty.shader")
    );
}

#[test]
fn a_setting_without_default_or_required_fails_loading() {
    let b = plain_sb();
    let p = b.root.join("packages/ghostty/package.toml");
    let mut t = std::fs::read_to_string(&p).unwrap();
    t.push_str("[settings.oops]\ntype = \"int\"\n");
    std::fs::write(&p, t).unwrap();
    let o = run(&b, &["--json", "config", "list"]);
    assert_ne!(o.code, 0);
    assert_eq!(o.json()["errors"][0]["code"], "setting-no-default");
}

// ------------------------------------------------------- required blocks rendering

#[test]
fn required_setting_blocks_only_its_packages_files_until_set() {
    let b = plain_sb();
    // Only the package that needs it is blocked: `tools` still plans its link.
    let o = run(&b, &["plan", "--color", "never", "--files-only"]);
    assert!(o.stdout.contains("identity.toml"), "{}", o.stdout);
    assert!(
        o.stdout.contains("dots config set git.git_email <value>"),
        "{}",
        o.stdout
    );
    assert!(o.stdout.contains(".toolsrc"), "{}", o.stdout);
    let j = run(&b, &["--json", "plan", "--files-only"]).json();
    let files = j["data"]["files"].as_array().unwrap();
    let blocked = files.iter().find(|f| f["package"] == "git").unwrap();
    assert_eq!(blocked["action"], "failed");
    let tools = files.iter().find(|f| f["package"] == "tools").unwrap();
    assert_eq!(tools["action"], "create");

    // Apply writes the healthy package and leaves the blocked file out.
    let o = run(&b, &["apply", "--yes", "--files-only", "--color", "never"]);
    assert!(b.home.join(".toolsrc").exists(), "{}", o.all());
    assert!(!b.home.join(".config/git/identity.toml").exists());

    // Setting it unblocks the render, with the value substituted.
    assert_eq!(
        run(&b, &["config", "set", "git.git_email", "me@example.com"]).code,
        0
    );
    let o = run(&b, &["apply", "--yes", "--files-only", "--color", "never"]);
    assert_eq!(o.code, 0, "{}", o.all());
    let text = std::fs::read_to_string(b.home.join(".config/git/identity.toml")).unwrap();
    assert!(text.contains("email = \"me@example.com\""), "{text}");
}

#[test]
fn secret_never_reaches_plan_status_or_the_header() {
    let secret = "tok-987654";
    let b = plain_sb();
    // A package that renders the secret into a file.
    write(
        &b.root.join("packages/tools/package.toml"),
        "schema = 1\nname = \"tools\"\n[[files]]\nmode = \"render\"\nformat = \"toml\"\ndst = \"{config}/tools/c.toml\"\nlayers = [\"l.toml\"]\ntemplate = \"jinja\"\n",
    );
    write(
        &b.root.join("packages/tools/l.toml"),
        "token = \"{{ settings.ghostty.api_token }}\"\n",
    );
    run(&b, &["config", "set", "ghostty.api_token", secret]);
    run(&b, &["config", "set", "git.git_email", "me@example.com"]);
    for args in [
        vec!["--json", "plan"],
        vec!["plan"],
        vec!["--json", "status"],
        vec!["status"],
    ] {
        let o = run(&b, &args);
        assert!(!o.all().contains(secret), "{args:?}: {}", o.all());
    }
    let o = run(&b, &["apply", "--yes", "--files-only"]);
    assert!(!o.all().contains(secret), "{}", o.all());
    let text = std::fs::read_to_string(b.home.join(".config/tools/c.toml")).unwrap();
    assert!(text.contains(&format!("token = \"{secret}\"")), "{text}");
    let header: String = text
        .lines()
        .take_while(|l| l.starts_with('#'))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(!header.contains(secret), "secret in the header: {header}");
}

// --------------------------------------------------------------------- questions

const REC: &str = "schema = 1\nname = \"t\"\n[tags]\nwork = { recommended = true }\n";

fn rec_sb() -> Sb {
    let b = sandbox(REC, "");
    write(
        &b.root.join("bundles/base.toml"),
        "schema = 1\nrecommended = true\ndescription = \"the basics\"\npackages = [\"ghostty\", \"git\"]\n",
    );
    b
}

#[test]
fn questions_lists_bundles_tags_and_settings_with_recommended_answers() {
    let b = rec_sb();
    let (o, doc) = jrun(&b, "questions", &["questions"]);
    assert_eq!(o.code, 0, "{}", o.all());
    let d = data(&doc);
    let qs = d["questions"].as_array().unwrap();
    let get = |id: &str| {
        qs.iter()
            .find(|q| q["id"] == id)
            .unwrap_or_else(|| panic!("{id} in {qs:?}"))
    };
    assert_eq!(get("bundle:base")["recommended"], true);
    assert_eq!(get("bundle:base")["description"], "the basics");
    assert_eq!(get("tag:work")["kind"], "tag");
    let theme = get("setting:ghostty.theme");
    assert_eq!(theme["recommended"], "dark");
    assert_eq!(theme["reason"], "ask");
    assert_eq!(theme["question"], "Dark or light terminal theme?");
    assert_eq!(theme["values"], json!(["dark", "light"]));
    let email = get("setting:git.git_email");
    assert_eq!(
        (email["required"].clone(), email["recommended"].clone()),
        (json!(true), Value::Null)
    );
    let tok = get("setting:ghostty.api_token");
    assert_eq!(tok["secret"], true);
    assert_eq!(tok["recommended"], Value::Null);
    // Not asked: defaults without `ask`.
    assert!(qs.iter().all(|q| q["id"] != "setting:ghostty.font_size"));
    assert_eq!(d["unanswered"].as_u64().unwrap() as usize, qs.len());
    assert_eq!(d["required_unanswered"], 2);

    let o = run(&b, &["questions", "--color", "never"]);
    assert!(
        o.stdout.contains("setting:ghostty.theme") && o.stdout.contains("dark"),
        "{}",
        o.stdout
    );
}

#[test]
fn accepting_every_recommendation_leaves_only_required_settings() {
    let b = rec_sb();
    let (_, doc) = jrun(&b, "questions", &["questions"]);
    let qs = data(&doc)["questions"].as_array().unwrap().clone();
    for q in &qs {
        match q["kind"].as_str().unwrap() {
            "bundle" => {
                let t = std::fs::read_to_string(&b.machine).unwrap();
                std::fs::write(&b.machine, format!("{t}bundles = [{}]\n", q["name"])).unwrap();
            }
            "tag" => {
                let t = std::fs::read_to_string(&b.machine).unwrap();
                std::fs::write(&b.machine, format!("{t}tags = [{}]\n", q["name"])).unwrap();
            }
            _ => {
                if let Some(v) = q["recommended"].as_str() {
                    let o = run(&b, &["config", "set", q["name"].as_str().unwrap(), v]);
                    assert_eq!(o.code, 0, "{}", o.all());
                }
            }
        }
    }
    let (_, doc) = jrun(&b, "questions", &["questions"]);
    let left: Vec<(String, bool, Value)> = data(&doc)["questions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|q| {
            (
                q["id"].as_str().unwrap().to_string(),
                q["required"].as_bool().unwrap(),
                q["recommended"].clone(),
            )
        })
        .collect();
    assert_eq!(
        left,
        vec![
            ("setting:ghostty.api_token".to_string(), true, Value::Null),
            ("setting:git.git_email".to_string(), true, Value::Null),
        ]
    );
    // Answering those too empties the list.
    run(&b, &["config", "set", "ghostty.api_token", "x"]);
    run(&b, &["config", "set", "git.git_email", "a@b.c"]);
    let (_, doc) = jrun(&b, "questions", &["questions"]);
    assert_eq!(data(&doc)["unanswered"], 0);
    let o = run(&b, &["questions", "--color", "never"]);
    assert!(o.stdout.contains("nothing to answer"), "{}", o.stdout);
}

#[test]
fn directive_use_of_a_secret_is_redacted_in_the_header() {
    let secret = "tok-555111";
    let b = plain_sb();
    write(
        &b.root.join("packages/tools/package.toml"),
        "schema = 1\nname = \"tools\"\n[[files]]\nmode = \"render\"\nformat = \"toml\"\ndst = \"{config}/tools/d.toml\"\nlayers = [\"l.toml\"]\n",
    );
    write(
        &b.root.join("packages/tools/l.toml"),
        "token = \"@dots(settings.ghostty.api_token)@\"\nsize = @dots(settings.ghostty.font_size)@\n",
    );
    run(&b, &["config", "set", "ghostty.api_token", secret]);
    run(&b, &["config", "set", "git.git_email", "me@example.com"]);
    let o = run(&b, &["apply", "--yes", "--files-only"]);
    assert_eq!(o.code, 0, "{}", o.all());
    let text = std::fs::read_to_string(b.home.join(".config/tools/d.toml")).unwrap();
    assert!(text.contains(&format!("token = \"{secret}\"")), "{text}");
    assert!(text.contains("size = 13"), "{text}");
    let header: String = text
        .lines()
        .take_while(|l| l.starts_with('#'))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(header.contains("ghostty.font_size=13(default)"), "{header}");
    assert!(!header.contains(secret), "secret in the header: {header}");
}
