//! Task 2.3: `dots facts` human and JSON output.

mod common;

use common::golden;
use serde_json::Value;

#[test]
fn facts_json_validates_and_has_the_documented_shape() {
    let out = golden().run(&["facts", "--json"], &[]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    let doc: Value = serde_json::from_str(&out.stdout).unwrap();
    let schema: Value = serde_json::from_str(
        &std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../schemas/dots-facts.schema.json"),
        )
        .unwrap(),
    )
    .unwrap();
    jsonschema::validator_for(&schema)
        .unwrap()
        .validate(&doc)
        .unwrap();
    assert_eq!(doc["schema"], "dots/facts@1");
    assert_eq!(doc["ok"], true);
    let d = &doc["data"];
    for k in ["os", "arch", "sudo", "gui", "wsl", "shell", "home"] {
        assert!(d["facts"][k]["source"] == "detected", "{k}");
    }
    assert_eq!(d["platform_chain"][0], "any");
    assert_eq!(d["tags"], serde_json::json!([]));
    assert_eq!(d["facts"]["arch"]["value"], d["arch"]);
}

#[test]
fn facts_human_output_lists_sources_chain_and_tags() {
    let out = golden().run(&["facts", "--color", "never"], &[]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert!(out.stdout.contains("(detected)"), "{}", out.stdout);
    assert!(out.stdout.contains("platform  "), "{}", out.stdout);
    assert!(out.stdout.contains("any > "), "{}", out.stdout);
    assert!(out.stdout.is_ascii(), "{}", out.stdout);
    assert!(out.stdout.contains("tags"), "{}", out.stdout);
}
