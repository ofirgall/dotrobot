//! Generated files must be up to date. Regenerate with `DOTS_REGEN=1 cargo test -p dots-core`
//! or `cargo run -p dots-core --example gen-errors-doc`.

use std::collections::HashSet;
use std::path::PathBuf;

use dots_core::{commands, errors};

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn check(path: PathBuf, expected: String) {
    if std::env::var(dots_core::REGEN_ENV).is_ok_and(|v| v == "1") {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, &expected).unwrap();
        return;
    }
    let actual = std::fs::read_to_string(&path).unwrap_or_default();
    assert_eq!(
        actual,
        expected,
        "{} is stale; run `cargo run -p dots-core --example gen-errors-doc`",
        path.display()
    );
}

#[test]
fn errors_doc_is_up_to_date() {
    check(root().join("docs/errors.md"), errors::render_doc());
}

#[test]
fn schemas_are_up_to_date() {
    for c in commands::schema_commands() {
        check(
            root().join("schemas").join(commands::schema_file_name(c)),
            commands::schema_text(c),
        );
    }
}

#[test]
fn every_code_has_message_and_hint() {
    let mut seen = HashSet::new();
    for d in errors::REGISTRY {
        assert!(seen.insert(d.code), "duplicate code {}", d.code);
        assert!(
            !d.message.is_empty() && !d.hint.is_empty(),
            "{} lacks message or hint",
            d.code
        );
        assert!(
            d.code.split('-').all(|w| !w.is_empty()
                && w.chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())),
            "{} is not kebab-case",
            d.code
        );
    }
}

#[test]
fn registry_covers_every_spec_code() {
    // Codes named in openspec/changes/implement-dots-v1/specs.
    let named = [
        "schema-unknown-key",
        "version-conflict",
        "binary-collision",
        "dependency-cycle",
        "stale-override",
        "integrity-mismatch",
        "provider-unavailable",
        "check-failed-after-install",
        "lock-busy",
        "patch-does-not-apply",
        "name-collision",
        "version-unsupported",
        "version-spec-invalid",
        "unknown-tag",
        "unknown-platform",
        "unknown-name",
        "skipped-due-to-dep",
        "setting-ref-unresolved",
        "setting-no-default",
        "setting-conflict",
        "schema-unsupported",
        "schema-name-mismatch",
        "path-escapes-package",
        "override-identity-field",
        "origin-modified",
        "no-provider-satisfies",
        "method-unsupported",
        "lock-conflicted",
        "dots-too-old",
        "timeout",
        "not-implemented",
    ];
    for c in named {
        assert!(errors::lookup(c).is_some(), "{c} missing from the registry");
    }
}

#[test]
fn diagnostic_fills_templates() {
    let d = dots_core::Diagnostic::from_code("not-implemented", &[("command", "apply")]);
    assert!(d.message.contains("`apply`"));
}
