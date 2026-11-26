//! `set_in_file` / `write_atomic`: file modes and symlinks.
#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;

use serde_json::json;

fn mode(p: &std::path::Path) -> u32 {
    std::fs::metadata(p).unwrap().permissions().mode() & 0o777
}

#[test]
fn a_new_file_with_a_secret_is_born_private() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("machine.toml");
    dots_config::set_in_file(&p, "gh", "token", &json!("x"), true).unwrap();
    assert_eq!(mode(&p), 0o600);
    assert!(
        std::fs::read_dir(d.path()).unwrap().count() == 1,
        "no temp file left"
    );
}

#[test]
fn an_existing_mode_is_kept_and_a_secret_tightens_it() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("machine.toml");
    std::fs::write(&p, "schema = 1\n").unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o640)).unwrap();
    dots_config::set_in_file(&p, "gh", "size", &json!(1), false).unwrap();
    assert_eq!(mode(&p), 0o640);
    dots_config::set_in_file(&p, "gh", "token", &json!("x"), true).unwrap();
    assert_eq!(mode(&p), 0o600);
}

#[test]
fn a_symlink_is_written_through() {
    let d = tempfile::tempdir().unwrap();
    let real = d.path().join("real.toml");
    let link = d.path().join("machine.toml");
    std::fs::write(&real, "schema = 1\n").unwrap();
    std::os::unix::fs::symlink(&real, &link).unwrap();
    dots_config::set_in_file(&link, "gh", "size", &json!(2), false).unwrap();
    assert!(
        std::fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert!(std::fs::read_to_string(&real).unwrap().contains("size = 2"));
}
