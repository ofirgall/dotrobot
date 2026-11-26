//! Task 5.9: the `lockfile` format scenarios (stable output, platform keying, repair).

use dots_lock::{FILE, Lock, PkgRow, Upsert, fix_text, platform_for};

fn row(name: &str, platform: &str, provider: &str, version: &str) -> PkgRow {
    let mut r = PkgRow::new(name, platform, provider);
    r.version = Some(version.into());
    r.locked_at = "2026-11-25T20:11:03Z".into();
    r.by_rev = Some("f4522416".into());
    r
}

#[test]
fn locking_the_same_resolution_twice_is_byte_identical() {
    let resolution = [
        row("bottom", "macos-15/aarch64", "brew", "0.10.2"),
        row("kv", "*", "cargo", "0.4.1"),
        row("ripgrep", "ubuntu-24.04/x86_64", "apt", "14.1.0"),
    ];
    let lock_once = |at: &str| {
        let mut l = Lock::default();
        for r in &resolution {
            let mut r = r.clone();
            r.locked_at = at.into();
            l.upsert(r);
        }
        l
    };
    let mut first = lock_once("2026-11-25T20:11:03Z");
    let before = first.render();
    // A later run locks the same versions with a later timestamp: nothing changes.
    for r in &resolution {
        let mut r = r.clone();
        r.locked_at = "2027-01-01T00:00:00Z".into();
        assert_eq!(first.upsert(r), Upsert::Unchanged);
    }
    assert_eq!(first.render(), before);
    // And the order rows were inserted in does not matter.
    let mut reversed = Lock::default();
    for r in resolution.iter().rev() {
        reversed.upsert(r.clone());
    }
    assert_eq!(reversed.render(), before);
}

#[test]
fn two_machines_share_a_lock_and_read_only_their_own_rows() {
    let mac = "macos-15/aarch64";
    let ubuntu = "ubuntu-24.04/x86_64";
    let mut l = Lock::default();
    l.upsert(row("ripgrep", &platform_for("brew", mac), "brew", "14.1.1"));
    l.upsert(row(
        "ripgrep",
        &platform_for("apt", ubuntu),
        "apt",
        "14.1.0",
    ));
    l.upsert(row("kv", &platform_for("cargo", mac), "cargo", "0.4.1"));
    assert_eq!(l.pkgs.len(), 3);
    assert_eq!(l.pkg("ripgrep", mac).unwrap().provider, "brew");
    assert_eq!(
        l.pkg("ripgrep", ubuntu).unwrap().version.as_deref(),
        Some("14.1.0")
    );
    // Platform-independent providers use `*` on every machine.
    assert_eq!(platform_for("cargo", ubuntu), "*");
    assert!(l.pkg("kv", "*").is_some());
    assert!(l.pkg("ripgrep", "*").is_none());
    let text = l.render();
    assert!(text.contains("platform = \"macos-15/aarch64\""));
    assert!(text.contains("platform = \"*\""));
}

#[test]
fn rev_rows_keep_their_source_and_ref() {
    let mut r = PkgRow::new("kv", "*", "cargo");
    r.rev = Some("9e1c0b7d2f".into());
    r.git_ref = Some("main".into());
    r.source = Some("git+https://github.com/KoalaVim/kv".into());
    r.locked_at = "2026-11-25T20:11:03Z".into();
    let mut l = Lock::default();
    l.upsert(r);
    let back = Lock::parse(&l.render()).unwrap();
    assert_eq!(back, l);
    assert!(l.render().contains("ref = \"main\""));
}

#[test]
fn load_reports_conflict_markers_and_garbage_differently() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(FILE);
    assert_eq!(
        Lock::load(&path).unwrap(),
        Lock::default(),
        "a missing lock is empty"
    );

    std::fs::write(
        &path,
        "schema = 1\n<<<<<<< HEAD\nx\n=======\ny\n>>>>>>> b\n",
    )
    .unwrap();
    let d = Lock::load(&path).unwrap_err();
    assert_eq!(d.code, "lock-conflicted");
    assert_eq!(d.provenance[0].file.as_deref(), Some("dots.lock"));

    std::fs::write(&path, "schema = 1\n[[pkg]\n").unwrap();
    assert_eq!(Lock::load(&path).unwrap_err().code, "lock-invalid");
}

#[test]
fn a_conflicted_lock_is_repaired_keeping_every_valid_row() {
    let mine = row("bat", "*", "cargo", "0.24.0");
    let theirs = row("fd", "*", "cargo", "10.2.0");
    let mut base = Lock::default();
    base.upsert(row("zoxide", "*", "cargo", "0.9.4"));
    let text = format!(
        "{}\n<<<<<<< HEAD\n{}=======\n{}>>>>>>> feature\n",
        base.render(),
        block(&mine),
        block(&theirs)
    );
    assert!(Lock::parse(&text).is_err());
    let (fixed, report) = fix_text(&text);
    assert!(report.had_markers);
    let names: Vec<&str> = fixed.pkgs.iter().map(|r| r.name.as_str()).collect();
    assert_eq!(names, ["bat", "fd", "zoxide"]);
    // The result is a valid lock that round-trips.
    let again = Lock::parse(&fixed.render()).unwrap();
    assert_eq!(again, fixed);
}

fn block(r: &PkgRow) -> String {
    let mut l = Lock::default();
    l.upsert(r.clone());
    let text = l.render();
    text[text.find("[[pkg]]").unwrap()..].to_string() + "\n"
}
