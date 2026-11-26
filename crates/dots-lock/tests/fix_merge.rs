//! `lock --fix` on conflicts produced by real `git merge-file`.

use std::process::Command;

use dots_lock::fix_text;

fn pkg(name: &str, version: &str, at: &str) -> String {
    format!(
        "[[pkg]]\nname = \"{name}\"\nplatform = \"*\"\nprovider = \"cargo\"\nversion = \"{version}\"\nlocked_at = \"{at}\"\n"
    )
}

/// Three-way merge three lock texts the way git does.
fn merge(base: &str, ours: &str, theirs: &str) -> String {
    let dir = tempfile::tempdir().unwrap();
    for (n, t) in [("base", base), ("ours", ours), ("theirs", theirs)] {
        std::fs::write(dir.path().join(n), t).unwrap();
    }
    let out = Command::new("git")
        .current_dir(dir.path())
        .args(["merge-file", "-p", "ours", "base", "theirs"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1), "expected a conflict");
    String::from_utf8(out.stdout).unwrap()
}

#[test]
fn git_conflict_inside_a_block_between_two_added_packages_keeps_both() {
    let base = format!(
        "schema = 1\n\n{}",
        pkg("bat", "1.0.0", "2026-01-01T00:00:00Z")
    );
    let ours = format!("{base}\n{}", pkg("fd", "9.0.0", "2026-01-02T00:00:00Z"));
    let theirs = format!("{base}\n{}", pkg("rg", "14.0.0", "2026-01-03T00:00:00Z"));
    let text = merge(&base, &ours, &theirs);
    assert!(text.contains("<<<<<<<"));
    let (lock, rep) = fix_text(&text);
    let names: Vec<_> = lock.pkgs.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(names, ["bat", "fd", "rg"]);
    assert_eq!(rep.kept, 3);
    assert!(rep.dropped.is_empty(), "{:?}", rep.dropped);
}

#[test]
fn both_sides_upgrading_the_same_package_keeps_the_newer_locked_at_either_way() {
    let base = format!(
        "schema = 1\n\n{}",
        pkg("bat", "1.0.0", "2026-01-01T00:00:00Z")
    );
    let newer = format!(
        "schema = 1\n\n{}",
        pkg("bat", "1.2.0", "2026-02-01T00:00:00Z")
    );
    let older = format!(
        "schema = 1\n\n{}",
        pkg("bat", "1.1.0", "2026-01-05T00:00:00Z")
    );
    for (ours, theirs) in [(&newer, &older), (&older, &newer)] {
        let text = merge(&base, ours, theirs);
        let (lock, rep) = fix_text(&text);
        assert_eq!(lock.pkgs.len(), 1);
        assert_eq!(lock.pkgs[0].version.as_deref(), Some("1.2.0"));
        assert_eq!(rep.duplicates, 1);
        assert_eq!(rep.dropped.len(), 1, "{:?}", rep.dropped);
        assert!(rep.dropped[0].contains("bat") && rep.dropped[0].contains("1.1.0"));
    }
}

#[test]
fn equal_locked_at_prefers_the_higher_version() {
    let base = format!(
        "schema = 1\n\n{}",
        pkg("bat", "1.0.0", "2026-01-01T00:00:00Z")
    );
    let a = format!(
        "schema = 1\n\n{}",
        pkg("bat", "1.3.0", "2026-02-01T00:00:00Z")
    );
    let b = format!(
        "schema = 1\n\n{}",
        pkg("bat", "1.10.0", "2026-02-01T00:00:00Z")
    );
    let (lock, _) = fix_text(&merge(&base, &b, &a));
    assert_eq!(lock.pkgs[0].version.as_deref(), Some("1.10.0"));
}
