//! Task 1.4: every stub's `--json` output validates against its checked-in schema.

mod common;

use std::path::PathBuf;

use common::{golden, stub_invocations};
use dots_core::commands::schema_file_name;
use serde_json::Value;

fn schema(command: &str) -> Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../schemas")
        .join(schema_file_name(command));
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    serde_json::from_str(&text).unwrap()
}

#[test]
fn every_stub_output_validates_against_its_schema() {
    let g = golden();
    for inv in stub_invocations() {
        let mut args: Vec<&str> = inv.to_vec();
        args.push("--json");
        let out = g.run(&args, &[]);
        assert_eq!(out.code, 1, "{inv:?}");
        assert!(
            out.stderr.is_empty(),
            "{inv:?}: stderr must be quiet under --json: {}",
            out.stderr
        );
        // stdout is exactly one JSON document.
        let doc: Value =
            serde_json::from_str(&out.stdout).unwrap_or_else(|e| panic!("{inv:?}: {e}"));
        let command = inv[0];
        let validator = jsonschema::validator_for(&schema(command)).unwrap();
        if let Err(e) = validator.validate(&doc) {
            panic!("{inv:?} violates its schema: {e}");
        }
        assert_eq!(doc["schema"], format!("dots/{command}@1"));
        assert_eq!(doc["ok"], false);
        assert_eq!(doc["errors"][0]["code"], "not-implemented");
    }
}

#[test]
fn bare_dots_json_is_status() {
    let out = golden().run(&["--json"], &[]);
    let doc: Value = serde_json::from_str(&out.stdout).unwrap();
    assert_eq!(doc["schema"], "dots/status@1");
}

#[test]
fn unrecognised_command_uses_usage_schema_with_clean_message() {
    let out = golden().run(&["stauts", "--json"], &[]);
    assert_eq!(out.code, 64);
    assert!(out.stderr.is_empty());
    let doc: Value = serde_json::from_str(&out.stdout).unwrap();
    assert_eq!(doc["schema"], "dots/usage@1");
    assert_eq!(doc["ok"], false);
    let e = &doc["errors"][0];
    assert_eq!(e["code"], "usage-error");
    assert_eq!(e["message"], "unrecognized subcommand 'stauts'");
    assert_eq!(e["hint"], "Did you mean `status`?");
    jsonschema::validator_for(&schema("usage"))
        .unwrap()
        .validate(&doc)
        .unwrap();
}

#[test]
fn recognised_command_keeps_its_schema() {
    let out = golden().run(&["migrate", "fd", "--json"], &[]);
    assert_eq!(out.code, 64);
    let doc: Value = serde_json::from_str(&out.stdout).unwrap();
    assert_eq!(doc["schema"], "dots/migrate@1");
    let e = &doc["errors"][0];
    let msg = e["message"].as_str().unwrap();
    assert!(
        msg.starts_with("the following required arguments were not provided:"),
        "{msg}"
    );
    assert!(msg.contains("--to <provider>"));
    for bad in ["error:", "Usage:", "For more information", "\n"] {
        assert!(!msg.contains(bad), "{msg}");
    }
    assert_eq!(
        e["hint"],
        "Run `dots migrate --help` to see the accepted arguments."
    );
    jsonschema::validator_for(&schema("migrate"))
        .unwrap()
        .validate(&doc)
        .unwrap();
}

#[test]
fn schemas_exist_for_every_command_and_reject_bad_envelopes() {
    for c in dots_core::commands::schema_commands() {
        let v = jsonschema::validator_for(&schema(c)).unwrap();
        assert!(!v.is_valid(&serde_json::json!({"ok": true})), "{c}");
        assert!(!v.is_valid(&serde_json::json!({
            "schema": "dots/other@1", "ok": true, "data": {}, "warnings": [], "errors": []
        })));
    }
}
