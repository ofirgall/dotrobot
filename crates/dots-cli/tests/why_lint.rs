//! Tasks 2.6 and 2.9: `dots why` and `dots lint` end to end.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

struct Out {
    stdout: String,
    stderr: String,
    code: i32,
}

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

/// Run `dots <args>` against fixture repo `repo`, with a machine.toml in a sandbox home.
fn run(repo: &str, machine_extra: &str, args: &[&str]) -> Out {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let machine = tmp.path().join("machine.toml");
    std::fs::write(
        &machine,
        format!(
            "schema = 1\nroot = {:?}\n{machine_extra}",
            fixture(repo).display().to_string()
        ),
    )
    .unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_dots"))
        .env_clear()
        .envs(dots_testkit::system_env())
        .env("PATH", dots_testkit::system_path())
        .env("DOTS_PROVIDERS", "none")
        .env("HOME", tmp.path().join("sentinel"))
        .current_dir(tmp.path())
        .arg("--home")
        .arg(&home)
        .arg("--machine-config")
        .arg(&machine)
        .args(args)
        .output()
        .unwrap();
    Out {
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        code: out.status.code().unwrap(),
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
    jsonschema::validator_for(&schema)
        .unwrap()
        .validate(doc)
        .unwrap();
}

const CLI: &str = "bundles = [\"cli\"]\ntags = [\"work\"]\n";

// ---------------------------------------------------------------- why

#[test]
fn why_prints_every_path_as_a_tree() {
    let o = run("good", CLI, &["why", "rust", "--color", "never"]);
    assert_eq!(o.code, 0, "{}", o.stderr);
    // Both dependents are shown, sharing the machine → cli prefix.
    assert!(
        o.stdout
            .contains("[ok] rust: active\n\n   machine\n   \\-- cli\n"),
        "{}",
        o.stdout
    );
    assert!(
        o.stdout
            .contains("|-- jless\n       |   \\-- requires rust >=1.80"),
        "{}",
        o.stdout
    );
    assert!(
        o.stdout
            .contains("\\-- kv\n           \\-- requires rust >=1.80"),
        "{}",
        o.stdout
    );
    // Constraints with their sources.
    assert!(
        o.stdout
            .contains(">=1.80  from required by jless (packages/jless/package.toml)"),
        "{}",
        o.stdout
    );
    assert!(
        o.stdout.contains(">=1.80  from required by kv"),
        "{}",
        o.stdout
    );
}

#[test]
fn why_shows_duplicate_inclusion_and_provider_reasons() {
    let o = run("good", CLI, &["why", "ripgrep", "--color", "never"]);
    assert_eq!(o.code, 0, "{}", o.stderr);
    assert!(o.stdout.contains("|-- base\n"), "{}", o.stdout);
    assert!(o.stdout.contains("ripgrep"), "{}", o.stdout);
    // No real providers yet: every candidate reports why it is unusable, and the missing
    // provider is a warning, not a failure.
    assert!(o.stdout.contains("provider: none usable"), "{}", o.stdout);
    assert!(
        o.stdout
            .contains("cargo skipped: not implemented in this build"),
        "{}",
        o.stdout
    );
    assert!(o.stderr.contains("no-provider-satisfies"), "{}", o.stderr);
}

#[test]
fn why_marks_inactive_paths_and_inert_packages() {
    let o = run("good", CLI, &["why", "wiremix", "--color", "never"]);
    assert_eq!(o.code, 0, "{}", o.stderr);
    assert!(o.stdout.contains("wiremix: inert"), "{}", o.stdout);
    assert!(
        o.stdout.contains("(inactive: `tag = \"remote\"` is false)"),
        "{}",
        o.stdout
    );
}

#[test]
fn why_unknown_package_suggests_a_name() {
    let o = run("good", CLI, &["why", "jles", "--color", "never"]);
    assert_eq!(o.code, 1);
    assert!(o.stderr.contains("unknown-package"), "{}", o.stderr);
    assert!(o.stderr.contains("Did you mean `jless`?"), "{}", o.stderr);
}

#[test]
fn why_not_included_is_not_an_error() {
    let o = run(
        "good",
        "bundles = [\"base\"]\n",
        &["why", "jless", "--color", "never"],
    );
    assert_eq!(o.code, 0, "{}", o.stderr);
    assert!(o.stdout.contains("jless: not included"), "{}", o.stdout);
}

#[test]
fn why_json_follows_its_schema() {
    let o = run("good", CLI, &["why", "rust", "--json"]);
    assert_eq!(o.code, 0, "{}", o.stderr);
    let doc: Value = serde_json::from_str(&o.stdout).unwrap();
    validate("why", &doc);
    assert_eq!(doc["schema"], "dots/why@1");
    let d = &doc["data"];
    assert_eq!(d["package"], "rust");
    assert_eq!(d["status"], "active");
    let paths = d["paths"].as_array().unwrap();
    let texts: Vec<&str> = paths.iter().map(|p| p["text"].as_str().unwrap()).collect();
    assert!(
        texts.contains(&"machine → cli → jless → requires rust >=1.80"),
        "{texts:?}"
    );
    assert!(
        texts.contains(&"machine → cli → kv → requires rust >=1.80"),
        "{texts:?}"
    );
    assert_eq!(paths[0]["steps"][0]["kind"], "machine");
    assert_eq!(d["constraints"][0]["spec"], ">=1.80");
    assert_eq!(d["constraints"][0]["source"], "requires");
    assert!(d["provider"]["candidates"].as_array().unwrap().len() >= 2);
    // The machine's bundle is the only thing in `warnings` besides the provider problem.
    assert_eq!(doc["warnings"][0]["code"], "no-provider-satisfies");
}

// ---------------------------------------------------------------- lint

#[test]
fn lint_unknown_tag_fails_with_a_suggestion() {
    let o = run("badtag", "", &["lint", "--color", "never"]);
    assert_eq!(o.code, 1, "{}", o.stderr);
    assert!(o.stderr.contains("error[unknown-tag]"), "{}", o.stderr);
    assert!(
        o.stderr.contains("`wrok`") && o.stderr.contains("did you mean `work`?"),
        "{}",
        o.stderr
    );
    // Unknown platform keys, in a condition and as a priority key, get suggestions too.
    assert!(o.stderr.contains("error[unknown-platform]"), "{}", o.stderr);
    assert!(
        o.stderr.contains("`linx`") || o.stderr.contains("linx"),
        "{}",
        o.stderr
    );
    assert!(o.stderr.contains("Did you mean `linux`?"), "{}", o.stderr);
    assert!(
        o.stderr.contains("Did you mean `ubuntu-22.04`?") || o.stderr.contains("ubunut-22.04"),
        "{}",
        o.stderr
    );
    assert!(
        o.stdout.contains("lint: 3 errors, 0 warnings"),
        "{}",
        o.stdout
    );
}

#[test]
fn lint_warnings_only_exits_zero() {
    let o = run("warny", "", &["lint", "--color", "never"]);
    assert_eq!(o.code, 0, "{}", o.stderr);
    assert!(
        o.stderr.contains("warning[escape-hatch-path]"),
        "{}",
        o.stderr
    );
    assert!(
        o.stderr.contains("warning[script-no-check]"),
        "{}",
        o.stderr
    );
    assert!(
        o.stdout.contains("lint: 0 errors, 2 warnings"),
        "{}",
        o.stdout
    );
}

#[test]
fn lint_clean_repo_and_machine_tag_check() {
    let o = run("good", CLI, &["lint", "--color", "never"]);
    assert_eq!(o.code, 0, "{}{}", o.stdout, o.stderr);
    assert!(
        o.stdout
            .contains("lint: ok (5 packages, 2 bundles checked)"),
        "{}",
        o.stdout
    );
    // A machine tag that dots.toml does not declare is an error.
    let o = run("good", "tags = [\"wrk\"]\n", &["lint", "--color", "never"]);
    assert_eq!(o.code, 1);
    assert!(
        o.stderr.contains("error[unknown-tag]") && o.stderr.contains("`work`"),
        "{}",
        o.stderr
    );
}

#[test]
fn lint_json_follows_its_schema() {
    for (repo, code) in [("badtag", 1), ("warny", 0), ("good", 0)] {
        let o = run(repo, "", &["lint", "--json"]);
        assert_eq!(o.code, code, "{repo}: {}", o.stderr);
        let doc: Value = serde_json::from_str(&o.stdout).unwrap();
        validate("lint", &doc);
        assert_eq!(doc["ok"], code == 0);
        assert_eq!(
            doc["data"]["errors"].as_u64().unwrap() as usize,
            doc["errors"].as_array().unwrap().len()
        );
    }
}

#[test]
fn lint_reports_a_missing_repo_as_an_error() {
    let o = run("does-not-exist", "", &["lint", "--color", "never"]);
    assert_eq!(o.code, 1);
    assert!(o.stderr.contains("error["), "{}", o.stderr);
}

// ---------------------------------------------------------------- facts

#[test]
fn facts_use_machine_overrides_tags_and_platform_extra() {
    let o = run(
        "good",
        "tags = [\"work\", \"gui\"]\nplatform_extra = [\"steamdeck\"]\n[facts]\nsudo = false\n",
        &["facts", "--json"],
    );
    assert_eq!(o.code, 0, "{}", o.stderr);
    let doc: Value = serde_json::from_str(&o.stdout).unwrap();
    validate("facts", &doc);
    let d = &doc["data"];
    assert_eq!(d["tags"], serde_json::json!(["work", "gui"]));
    assert_eq!(d["facts"]["sudo"]["value"], false);
    assert_eq!(d["facts"]["sudo"]["source"], "overridden");
    let chain: Vec<&str> = d["platform_chain"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert_eq!(chain.last(), Some(&"steamdeck"));
}

#[test]
fn resolution_errors_point_at_file_line_and_column() {
    // `packages = ["ripgrep", "ripgerp"]`: the second string starts at column 24.
    for cmd in [&["why", "ripgrep"][..], &["lint"][..]] {
        let args: Vec<&str> = cmd.iter().copied().chain(["--color", "never"]).collect();
        let o = run("badref", CLI, &args);
        assert_eq!(o.code, 1, "{}", o.stderr);
        assert!(
            o.stderr.contains("at bundles/cli.toml:2:24"),
            "{cmd:?}: {}",
            o.stderr
        );
    }
    let o = run("badref", CLI, &["why", "ripgrep", "--json"]);
    let doc: Value = serde_json::from_str(&o.stdout).unwrap();
    let p = &doc["errors"][0]["provenance"][0];
    assert_eq!(
        (p["file"].as_str(), p["line"].as_u64(), p["col"].as_u64()),
        (Some("bundles/cli.toml"), Some(2), Some(24))
    );
}
