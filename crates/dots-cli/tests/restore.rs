use std::fs;
use std::process::Command;

use dots_files::{FilesCtx, LayerSrc, Mode, ResolvedFile, apply, plan};

fn dots(home: &std::path::Path, args: &[&str]) -> (i32, serde_json::Value) {
    let out = Command::new(env!("CARGO_BIN_EXE_dots"))
        .arg("--home")
        .arg(home)
        .args(args)
        .arg("--json")
        .output()
        .unwrap();
    let v = serde_json::from_slice(&out.stdout).unwrap_or(serde_json::Value::Null);
    (out.status.code().unwrap(), v)
}

#[test]
fn restore_puts_back_a_backed_up_file() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("h");
    let state = home.join(".local/state/dots");
    let root = tmp.path().join("repo");
    fs::create_dir_all(&root).unwrap();
    fs::create_dir_all(&home).unwrap();
    fs::write(home.join(".gitconfig"), "mine").unwrap();
    fs::write(root.join("g"), "[user]\nname = x\n").unwrap();
    let ctx = FilesCtx::new(&home, &state, &root, "r", "run-9");
    let file = ResolvedFile {
        package: "git".into(),
        mode: Mode::Copy,
        dst: home.join(".gitconfig"),
        src: Some(root.join("g")),
        layers: Vec::<LayerSrc>::new(),
        format: "gitconfig".into(),
        kind: None,
        template: None,
        merge: Default::default(),
        comment: "#".into(),
        extras: Default::default(),
    };
    for s in plan(&[file], &ctx) {
        assert!(apply(&s, &ctx).error.is_none());
    }
    assert_ne!(fs::read_to_string(home.join(".gitconfig")).unwrap(), "mine");

    let (code, v) = dots(&home, &["restore", "run-9"]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["schema"], "dots/restore@1");
    assert_eq!(v["data"]["restored"].as_array().unwrap().len(), 1);
    assert_eq!(fs::read_to_string(home.join(".gitconfig")).unwrap(), "mine");

    let (code, v) = dots(&home, &["restore", "nope"]);
    assert_eq!(code, 1);
    assert_eq!(v["errors"][0]["code"], "backup-not-found");
}
