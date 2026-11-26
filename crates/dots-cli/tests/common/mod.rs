#![allow(dead_code)]

use std::path::PathBuf;

use dots_testkit::Golden;

pub fn golden() -> Golden {
    Golden::new(
        env!("CARGO_BIN_EXE_dots"),
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/golden"),
    )
}

/// A minimal valid invocation of each top-level command.
pub const VALID_INVOCATIONS: &[&[&str]] = &[
    &["init", "--defaults"],
    &["apply", "--dry-run"],
    &["plan", "--check"],
    &["status", "--all"],
    &["outdated"],
    &["why", "fd"],
    &["which", "fd"],
    &["bins", "--managed"],
    &["doctor"],
    &["facts"],
    &["upgrade", "--all"],
    &["pin", "fd", "1.0"],
    &["unpin", "fd"],
    &["remove", "pkg", "fd"],
    &["migrate", "fd", "--to", "cargo"],
    &["add", "pkg", "src:fd", "--copy"],
    &["search", "src", "fd"],
    &["sources"],
    &["pull", "--dry-run"],
    &["eject", "pkg:fd"],
    &["uneject", "pkg:fd"],
    &["todo", "done", "1", "--note", "x"],
    &["adopt", "~/.zshrc", "--machine"],
    &["try", "fd", "--from", "main"],
    &["use", "-"],
    &["roots"],
    &["diff", "a..b"],
    &["log", "fd", "--since", "30d"],
    &["verify"],
    &["restore", "r-1"],
    &["config", "set", "fd.x", "1", "--repo", "--commit"],
    &["questions"],
    &["show", "pkg:fd", "--effective"],
    &["override", "edit", "pkg:fd", "--machine"],
    &["skill", "install", "--agent", "claude"],
    &["lint"],
    &["lock", "--fix"],
    &["import", "dotbot", "a.yaml", "b.yaml"],
    &["completions", "zsh"],
];

/// Commands that are implemented; the stub-contract tests skip them.
pub const IMPLEMENTED: &[&str] = &[
    "facts",
    "why",
    "lint",
    "restore",
    "plan",
    "apply",
    "init",
    "status",
    "outdated",
    "bins",
    "which",
    "doctor",
    "config",
    "questions",
    "upgrade",
    "pin",
    "unpin",
    "lock",
    "show",
    "override",
    "migrate",
    "adopt",
];

/// Valid invocations of the commands that are still stubs.
pub fn stub_invocations() -> impl Iterator<Item = &'static &'static [&'static str]> {
    VALID_INVOCATIONS
        .iter()
        .filter(|i| !IMPLEMENTED.contains(&i[0]))
}
