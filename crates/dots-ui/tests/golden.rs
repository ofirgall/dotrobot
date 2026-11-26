//! Renderer-level goldens for every terminal-ui scenario: fixture reports rendered in the
//! plain, basic and rich tiers (and at narrow width). Set `DOTS_UPDATE_GOLDEN=1` to rewrite.
//! Escape characters are stored as `<ESC>` so the files are reviewable.

use std::path::PathBuf;

use dots_core::blocks::{Block, Cell, Marker, Row, Section, State, Table, Tone, TreeNode};
use dots_core::{GlobalOptions, Report};
use dots_ui::{IconSet, Mode, Tier, Ui, UiConfig, render, render_with, width};
use serde_json::json;

fn golden_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden")
        .join(format!("{name}.txt"))
}

fn check(name: &str, actual: &str) {
    let shown = actual.replace('\x1b', "<ESC>");
    if name.ends_with(".plain") {
        assert!(
            actual.is_ascii(),
            "plain tier of `{name}` has non-ASCII text: {:?}",
            actual.chars().filter(|c| !c.is_ascii()).collect::<String>()
        );
    }
    let path = golden_path(name);
    if std::env::var("DOTS_UPDATE_GOLDEN").is_ok_and(|v| v == "1") {
        std::fs::write(&path, &shown).unwrap();
        return;
    }
    let expected = std::fs::read_to_string(&path)
        .unwrap_or_else(|_| panic!("missing golden {name}; run with DOTS_UPDATE_GOLDEN=1"));
    assert_eq!(
        expected, shown,
        "golden `{name}` differs (DOTS_UPDATE_GOLDEN=1 to accept)"
    );
}

fn human(report: &Report, ui: &Ui) -> String {
    let out = render_with(report, ui, &GlobalOptions::default());
    format!("{}{}", out.stdout, out.stderr)
}

/// One golden per tier at the given width.
fn all_tiers(name: &str, report: &Report, w: usize) {
    for (tier, label) in [
        (Tier::Plain, "plain"),
        (Tier::Basic, "basic"),
        (Tier::Rich, "rich"),
    ] {
        check(
            &format!("{name}.{label}"),
            &human(report, &Ui::fixed(tier, w)),
        );
    }
}

// ------------------------------------------------------------------------ fixtures

fn status() -> Report {
    let packages = Table::new()
        .styled_column("provider", 2, dots_core::Style::Muted)
        .column("version", 3)
        .column("detail", 1)
        .row(
            Row::new(State::Outdated, "bottom")
                .cell("cargo")
                .cell(Cell::version("0.9.6", "0.10.2"))
                .cell("released 4 days ago")
                .hint("dots upgrade bottom"),
        )
        .row(
            Row::new(State::Outdated, "delta")
                .cell("cargo")
                .cell(Cell::version("0.17.0", "0.18.2")),
        )
        .row(
            Row::new(State::Drift, "neovim")
                .cell("github")
                .cell("0.11.4")
                .cell("lock wants 0.11.5")
                .hint("dots apply neovim"),
        )
        .row(
            Row::new(State::Missing, "herdr")
                .cell("script")
                .cell("")
                .cell("not installed")
                .hint("dots apply herdr"),
        )
        .row(
            Row::new(State::Orphan, "zellij")
                .cell("brew")
                .cell("0.41.1")
                .cell("installed in worktree try-zellij, Nov 20")
                .hint("dots remove zellij")
                .hint("dots adopt-package zellij"),
        );
    let files = Table::new()
        .column("kind", 2)
        .column("detail", 1)
        .row(
            Row::named(State::Modified, Cell::path("~/.claude/settings.json"))
                .cell("render")
                .cell("app changed 2 keys")
                .hint("dots adopt ~/.claude/settings.json"),
        )
        .row(
            Row::named(State::Conflict, Cell::path("~/.config/herdr/"))
                .cell("link")
                .cell("herdr.sock, herdr-server.log, sessions/ live in your repo")
                .hint("dots explain foreign-entries"),
        );
    let mut r = Report::ok("status", json!({"ok": 62}))
        .with_headline(Tone::Warn, "7 things need attention");
    r.push(Block::Context {
        title: "dots status".into(),
        parts: vec![
            "~/dotfiles".into(),
            "main @ f452241 (clean)".into(),
            "work-mbp".into(),
            "macOS arm64".into(),
        ],
    });
    r.push(
        Section::new("PACKAGES")
            .pill(State::Ok, 62)
            .pill(State::Outdated, 2)
            .pill(State::Drift, 1)
            .pill(State::Missing, 1)
            .pill(State::Orphan, 0)
            .block(packages),
    );
    r.push(
        Section::new("FILES")
            .pill(State::Ok, 84)
            .pill(State::Modified, 1)
            .block(files),
    );
    r.push(Block::Collapsed {
        state: State::Ok,
        text: "62 packages · 84 files ok".into(),
        aside: Some("checked in 0.9s".into()),
    });
    r
}

fn healthy() -> Report {
    let mut r = Report::ok("status", json!({})).with_headline(Tone::Ok, "All good");
    r.push(Block::Collapsed {
        state: State::Ok,
        text: "64 packages · 86 files · 3 git sources".into(),
        aside: Some("checked in 0.8s".into()),
    });
    r
}

fn plan() -> Report {
    let installs = Table::new()
        .styled_column("provider", 2, dots_core::Style::Muted)
        .column("version", 3)
        .column("note", 1)
        .row(Row::new(Marker::Add, "ripgrep").cell("brew").cell("14.1.1"))
        .row(
            Row::new(Marker::Add, "kv")
                .cell("cargo")
                .cell("git:9e1c0b7")
                .cell("builds from source · usually ~2m"),
        )
        .row(
            Row::new(Marker::Upgrade, "bottom")
                .cell("cargo")
                .cell(Cell::version("0.9.6", "0.10.2")),
        )
        .row(
            Row::new(Marker::Remove, "zellij")
                .cell("brew")
                .cell("0.41.1")
                .cell("not declared anymore"),
        )
        .row(
            Row::new(Marker::Replace, "~/.config/yazi")
                .cell("link")
                .cell("")
                .cell("existing folder moved to backup first"),
        );
    let mut r = Report::ok("plan", json!({}))
        .with_headline(Tone::Info, "Plan: 2 installs · 1 upgrade · 1 removal");
    r.push(Section::new("Packages").block(installs));
    r.push(
        Section::new("Heads up")
            .block(Block::callout("Needs your password once (apt)"))
            .block(Block::callout(
                "1 existing folder will be backed up to ~/.local/state/dots/backup/r-7f3c/",
            )),
    );
    r
}

fn why() -> Report {
    let tree = TreeNode::new(Cell::muted("work-mbp (machine)")).child(
        TreeNode::new("cli (bundle)").child(
            TreeNode::new("koala (bundle)").child(
                TreeNode::new(Cell::accent("kv"))
                    .note("cargo · git:9e1c0b7")
                    .child(TreeNode::new("needs rust ≥1.80").note("provided by rustup 1.83.0")),
            ),
        ),
    );
    let mut r = Report::ok("why", json!({})).with_headline(Tone::Info, "kv is installed because");
    r.push(Block::Tree(vec![tree]));
    r
}

fn deep() -> Report {
    let mut r = Report::ok("status", json!({})).with_headline(Tone::Ok, "ghostty is ok");
    r.push(Block::facts([
        ("provider", Cell::from("brew (cask)")),
        ("version", Cell::from("1.2.3")),
        ("bundle", Cell::accent("mac-desktop")),
        (
            "config",
            Cell::path("~/.config/ghostty/some/deeply/nested/folder/for/testing/config"),
        ),
    ]));
    r
}

fn nerd() -> Report {
    let mut r = Report::ok("status", json!({}));
    r.push(Block::Table(
        Table::new().column("kind", 1).row(
            Row::named(State::Modified, Cell::path("~/.claude/settings.json"))
                .cell("render")
                .hint("dots adopt ~/.claude/settings.json"),
        ),
    ));
    r
}

// -------------------------------------------------------------------------- tests

#[test]
fn status_in_every_tier() {
    all_tiers("status", &status(), 100);
}

#[test]
fn status_at_80_columns_drops_low_priority_columns() {
    all_tiers("status-80", &status(), 80);
}

#[test]
fn narrow_terminal_uses_the_compact_layout_without_wrapping() {
    let r = status();
    all_tiers("status-narrow", &r, 50);
    let plain = human(&r, &Ui::fixed(Tier::Plain, 50));
    for line in plain.lines() {
        assert!(width(line) <= 50, "line wider than the terminal: {line:?}");
    }
    // Compact rows are stacked: the name line carries the symbol, the next the state word.
    assert!(plain.contains(" [up] bottom\n   outdated"), "{plain}");
}

#[test]
fn healthy_machine_is_one_headline_and_one_count_line() {
    let r = healthy();
    all_tiers("healthy", &r, 100);
    let plain = human(&r, &Ui::fixed(Tier::Plain, 100));
    assert_eq!(plain.lines().count(), 2, "{plain}");
    assert!(plain.starts_with(" [ok] All good"), "{plain}");
}

#[test]
fn plan_with_markers_and_callouts() {
    all_tiers("plan", &plan(), 100);
}

#[test]
fn why_tree() {
    all_tiers("why", &why(), 100);
}

#[test]
fn deep_view_truncates_paths_in_the_middle() {
    let r = deep();
    all_tiers("deep-narrow", &r, 50);
    let plain = human(&r, &Ui::fixed(Tier::Plain, 50));
    assert!(plain.contains("…") || plain.contains("..."), "{plain}");
    assert!(plain.contains("/config"), "the file name survives: {plain}");
}

#[test]
fn piped_output_has_no_escapes_and_ascii_markers() {
    let out = human(&status(), &Ui::fixed(Tier::Plain, 100));
    assert!(!out.contains('\x1b'));
    assert!(out.is_ascii() || !out.contains('✓'), "{out}");
    assert!(
        out.contains("[up]") && out.contains("[--]") && out.contains("[!!]"),
        "{out}"
    );
    // The same through the public entry point used by the binary.
    let via = render(
        &status(),
        Mode::Human(Tier::Plain),
        &GlobalOptions::default(),
    );
    assert!(!via.stdout.contains('\x1b'));
    assert!(via.stdout.contains("[mod]"));
}

#[test]
fn forced_color_emits_ansi() {
    let out = human(&status(), &Ui::fixed(Tier::Basic, 100));
    assert!(out.contains("\x1b[33m"), "warn is yellow in basic: {out:?}");
    let rich = human(&status(), &Ui::fixed(Tier::Rich, 100));
    assert!(rich.contains("\x1b[38;2;"), "truecolor in rich");
}

#[test]
fn monochrome_rows_carry_symbol_and_state_word() {
    let out = human(&status(), &Ui::fixed(Tier::Plain, 100));
    for (sym, word) in [
        ("[up]", "outdated"),
        ("[!=]", "drift"),
        ("[--]", "missing"),
        ("[orph]", "orphan"),
        ("[mod]", "modified"),
        ("[!!]", "conflict"),
    ] {
        let line = out
            .lines()
            .find(|l| l.contains(sym))
            .unwrap_or_else(|| panic!("no row with {sym}"));
        assert!(line.contains(word), "{line}");
    }
}

#[test]
fn modified_config_row_ends_with_its_fix() {
    let out = human(&status(), &Ui::fixed(Tier::Rich, 100)).replace('\x1b', "<ESC>");
    assert!(out.contains("dots adopt ~/.claude/settings.json"), "{out}");
    let plain = human(&status(), &Ui::fixed(Tier::Plain, 100));
    let lines: Vec<&str> = plain.lines().collect();
    let i = lines
        .iter()
        .position(|l| l.contains("[mod] modified"))
        .unwrap();
    assert!(
        lines[i + 1]
            .trim_start()
            .starts_with("-> dots adopt ~/.claude/settings.json")
    );
}

#[test]
fn a_row_gets_at_most_two_hints() {
    let t = Table::new().row(Row::new(State::Missing, "x").hint("a").hint("b").hint("c"));
    let mut r = Report::ok("status", json!({}));
    r.push(t);
    let out = human(&r, &Ui::fixed(Tier::Plain, 80));
    assert_eq!(out.matches("->").count(), 2);
}

#[test]
fn nerd_icons_change_symbols_but_not_state_words() {
    let mut ui = Ui::fixed(Tier::Basic, 100);
    ui.icons = IconSet::Nerd;
    let out = human(&nerd(), &ui);
    check("nerd.basic", &out);
    assert!(out.contains('\u{f040}'), "nerd modified glyph");
    assert!(out.contains("modified"));
    assert!(!out.contains('✎'));
}

#[test]
fn light_theme_uses_light_palette() {
    let mut ui = Ui::fixed(Tier::Rich, 100);
    ui.theme = dots_ui::Theme::Light;
    let out = human(&status(), &ui);
    assert!(out.contains("38;2;154;103;0"), "light warn #9a6700");
    assert!(!out.contains("38;2;227;179;65"), "dark warn absent");
}

#[test]
fn hyperlinks_only_when_enabled() {
    let mut r = Report::ok("status", json!({}));
    r.push(Table::new().row(Row::named(
        State::Ok,
        Cell::path("~/a.toml").link("file:///home/u/a.toml"),
    )));
    let mut ui = Ui::fixed(Tier::Rich, 80);
    assert!(!human(&r, &ui).contains("\x1b]8;;"));
    ui.hyperlinks = true;
    assert!(human(&r, &ui).contains("\x1b]8;;file:///home/u/a.toml\x1b\\"));
}

#[test]
fn json_is_unaffected_by_blocks() {
    let with_blocks = status();
    let mut bare = Report::ok("status", json!({"ok": 62}));
    bare.headline = with_blocks.headline.clone();
    let json = |r: &Report| {
        render(
            r,
            Mode::Json,
            &GlobalOptions {
                json: true,
                ..Default::default()
            },
        )
        .stdout
    };
    assert_eq!(json(&with_blocks), json(&bare));
    assert!(!json(&with_blocks).contains("PACKAGES"));
}

#[test]
fn ui_toml_unknown_key_is_a_schema_error() {
    let err = UiConfig::parse("colour = \"always\"\n", "~/.config/dots/ui.toml").unwrap_err();
    assert_eq!(err.code, "schema-unknown-key");
    let mut r = Report::failure("status", dots_core::ExitCode::Error, vec![err]);
    r.headline = None;
    check(
        "ui-toml-error.plain",
        &human(&r, &Ui::fixed(Tier::Plain, 80)),
    );
}

#[test]
fn quiet_prints_only_errors() {
    let out = render_with(
        &status(),
        &Ui::fixed(Tier::Plain, 80),
        &GlobalOptions {
            quiet: true,
            ..Default::default()
        },
    );
    assert!(out.stdout.is_empty());
}

#[test]
fn every_plain_golden_is_ascii() {
    let dir = golden_path("x").parent().unwrap().to_path_buf();
    let mut n = 0;
    for e in std::fs::read_dir(dir).unwrap() {
        let p = e.unwrap().path();
        if p.to_string_lossy().ends_with(".plain.txt") {
            let t = std::fs::read_to_string(&p).unwrap();
            assert!(t.is_ascii(), "{} has non-ASCII text", p.display());
            n += 1;
        }
    }
    assert!(n > 0);
}

#[test]
fn plain_tier_folds_the_platform_chain_separator() {
    let mut report = Report::ok("facts", json!({}));
    report.push(Block::Facts(vec![(
        "platform".to_string(),
        Cell::from("any › unix › linux (x86_64)"),
    )]));
    let plain = human(&report, &Ui::fixed(Tier::Plain, 100));
    assert!(plain.is_ascii(), "{plain}");
    assert!(plain.contains("any > unix > linux"), "{plain}");
    let mut winner = Report::ok("which", json!({}));
    winner.push(Block::Facts(vec![(
        "path".to_string(),
        Cell::from("\u{25b6} ~/bin/tool"),
    )]));
    let plain = human(&winner, &Ui::fixed(Tier::Plain, 100));
    assert!(
        plain.is_ascii() && plain.contains("> ~/bin/tool"),
        "{plain}"
    );
    let rich = human(&report, &Ui::fixed(Tier::Rich, 100));
    assert!(rich.contains('›'), "{rich}");
}
