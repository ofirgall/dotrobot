//! Task 1.2 / 1.3: the command tree, signatures, flags and exit codes.

mod common;

use common::{VALID_INVOCATIONS, golden, stub_invocations};
use dots_core::commands::COMMANDS;

/// The `Usage:` block of `--help`, up to the first blank line.
fn usage_block(help: &str) -> String {
    help.lines()
        .skip_while(|l| !l.starts_with("Usage:"))
        .take_while(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn every_command_usage_matches_the_signature_table() {
    let g = golden();
    let mut all = String::new();
    for c in COMMANDS {
        let out = g.run(&[c, "--help"], &[]);
        assert_eq!(out.code, 0, "{c} --help");
        all.push_str(&format!("{}\n\n", usage_block(&out.stdout)));
    }
    g.compare(
        &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden/usage.txt"),
        &all,
    );
}

#[test]
fn valid_invocations_cover_every_command() {
    let covered: Vec<&str> = VALID_INVOCATIONS.iter().map(|a| a[0]).collect();
    assert_eq!(covered, COMMANDS);
}

#[test]
fn help_lists_every_command_with_a_description() {
    let out = golden().run(&["--help"], &[]);
    for c in COMMANDS {
        let listed = out.stdout.lines().any(|l| {
            l.trim_start()
                .strip_prefix(c)
                .is_some_and(|rest| rest.starts_with(' ') && rest.trim().len() > 3)
        });
        assert!(listed, "`{c}` missing from --help");
    }
}

#[test]
fn version_works() {
    let out = golden().run(&["--version"], &[]);
    assert_eq!(out.code, 0);
    assert!(out.stdout.starts_with("dots "), "{}", out.stdout);
}

#[test]
fn typo_exits_64_with_suggestion() {
    let out = golden().run(&["stauts"], &[]);
    assert_eq!(out.code, 64);
    assert!(out.stderr.contains("status"), "{}", out.stderr);
}

#[test]
fn mutually_exclusive_flags_exit_64_naming_both() {
    let out = golden().run(&["apply", "--files-only", "--packages-only"], &[]);
    assert_eq!(out.code, 64);
    assert!(out.stderr.contains("--files-only") && out.stderr.contains("--packages-only"));
}

#[test]
fn missing_required_argument_shows_signature() {
    let out = golden().run(&["migrate", "fd"], &[]);
    assert_eq!(out.code, 64);
    assert!(
        out.stderr
            .contains("Usage: dots migrate <pkg> --to <provider>"),
        "{}",
        out.stderr
    );
}

#[test]
fn unknown_flag_exits_64() {
    assert_eq!(golden().run(&["status", "--bogus"], &[]).code, 64);
}

#[test]
fn global_flags_are_accepted_by_every_command() {
    let g = golden();
    for inv in stub_invocations() {
        let mut args: Vec<&str> = inv.to_vec();
        args.extend([
            "-vv",
            "-q",
            "--no-input",
            "--offline",
            "--color",
            "never",
            "--machine-config",
            "/x/m.toml",
        ]);
        let out = g.run(&args, &[]);
        assert_eq!(out.code, 1, "{args:?}: {}", out.stderr);
    }
}

#[test]
fn stubs_exit_1_with_not_implemented() {
    let g = golden();
    for inv in stub_invocations() {
        let out = g.run(inv, &[]);
        assert_eq!(out.code, 1, "{inv:?}");
        assert!(
            out.stderr.contains("not-implemented"),
            "{inv:?}: {}",
            out.stderr
        );
    }
}

#[test]
fn bare_dots_behaves_like_status() {
    // With no repo both fail the same way (and neither is a stub).
    let g = golden();
    let (bare, status) = (g.run(&[], &[]), g.run(&["status"], &[]));
    assert_eq!(bare.code, status.code);
    assert_eq!(bare.stderr, status.stderr);
    assert!(!bare.stderr.contains("not-implemented"), "{}", bare.stderr);
}

#[test]
fn quiet_still_shows_errors_and_color_never_has_no_escapes() {
    let out = golden().run(&["roots", "-q", "--color", "never"], &[]);
    assert!(out.stderr.contains("error[not-implemented]"));
    assert!(!out.stderr.contains('\x1b'));
}

#[test]
fn color_always_emits_ansi_and_env_is_honored() {
    let g = golden();
    assert!(
        g.run(&["roots", "--color", "always"], &[])
            .stderr
            .contains('\x1b')
    );
    // NO_COLOR / DOTS_UI force plain unless a flag says otherwise.
    assert!(
        !g.run(&["roots"], &[("NO_COLOR", "1"), ("FORCE_COLOR", "1")])
            .stderr
            .contains('\x1b')
    );
    assert!(
        g.run(&["roots"], &[("DOTS_UI", "rich")])
            .stderr
            .contains('\x1b')
    );
    assert!(
        !g.run(&["roots", "--color", "never"], &[("DOTS_UI", "rich")])
            .stderr
            .contains('\x1b')
    );
    assert!(
        g.run(&["roots"], &[("CLICOLOR_FORCE", "1")])
            .stderr
            .contains('\x1b')
    );
}
