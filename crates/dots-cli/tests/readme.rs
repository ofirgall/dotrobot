//! Tasks 4.12 and 5.11: the README's `console` blocks as golden tests (v0.1 and v0.2).
//!
//! Every ```console block in README.md must be announced by an HTML comment right above it:
//!
//! - `<!-- golden: <name> -->`: a v0.1 block. The harness builds the fixture for `<name>` (see
//!   `fixture`), runs the block's `$ dots ...` command through the real binary in the plain
//!   tier, and compares the output to the block.
//! - `<!-- golden-skip: <name> | v0.x | <reason> -->`: a block of a later milestone. It is
//!   listed (and the test prints why), never silently ignored.
//!
//! A block with neither comment fails the test. A block that shows only `$` commands and no
//! output (`config-set`, v0.2) runs every command in order and requires exit 0.
//!
//! Normalizations (applied to the README text and to the output alike, so each is symmetric):
//!
//! 1. The README shows the unicode tier; the plain tier is ASCII. The README side is folded with
//!    the plain tier's own substitutions: `▲`->`[!]`, `✓`->`[ok]`, `↑`->`[up]`, `✎`->`[mod]`,
//!    `○`->`[ ]`, `→`->`->`, `·`->`-`, `…`->`...`.
//! 2. Runs of spaces collapse to one and line ends and starts are trimmed: ASCII state symbols
//!    are wider than the unicode ones, so column alignment differs between tiers. Blank lines
//!    and line order are compared exactly.
//! 3. The git commit (`@ <7-40 hex>`) becomes `@ <sha>`: the fixture commit is created at test
//!    time and its hash cannot be fixed.
//! 4. On the context line only, the machine name and the OS/arch (the last two ` - ` parts) are
//!    replaced by `work-mbp` and `macOS arm64`: they come from the machine running the test.
//!
//! 5. The terminal is 120 columns wide (`COLUMNS`), so no line of a block wraps.
//!
//! Durations, run ids and home paths do not appear in the v0.1 blocks, so nothing else is
//! normalized: the fixture lives under `--home`, so `~` paths are exact.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

struct Block {
    marker: String,
    lines: Vec<String>,
}

fn readme_blocks() -> Vec<Block> {
    let text =
        std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../README.md"))
            .unwrap();
    let lines: Vec<&str> = text.lines().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        if lines[i].trim_end() == "```console" {
            let marker = if i > 0 {
                lines[i - 1].trim().to_string()
            } else {
                String::new()
            };
            let mut body = Vec::new();
            i += 1;
            while lines[i].trim_end() != "```" {
                body.push(lines[i].to_string());
                i += 1;
            }
            out.push(Block {
                marker,
                lines: body,
            });
        }
        i += 1;
    }
    out
}

fn write(p: &Path, text: &str) {
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, dots_testkit::bash_on_windows(text)).unwrap();
}

fn git(dir: &Path, args: &[&str]) {
    let o = Command::new("git")
        .args([
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(
        o.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&o.stderr)
    );
}

struct Fx {
    _tmp: tempfile::TempDir,
    home: PathBuf,
}

impl Fx {
    fn run(&self, args: &[&str]) -> (i32, String, String) {
        let o = Command::new(env!("CARGO_BIN_EXE_dots"))
            .env_clear()
            .envs(dots_testkit::system_env())
            .env("PATH", dots_testkit::system_path())
            .env("SHELL", "/bin/zsh")
            .env("COLUMNS", "120")
            .env("DOTS_PROVIDERS", "script")
            .env("HOME", self._tmp.path().join("sentinel"))
            .current_dir(self._tmp.path())
            .arg("--home")
            .arg(&self.home)
            .args(args)
            .stdin(Stdio::null())
            .output()
            .unwrap();
        (
            o.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&o.stdout).into_owned(),
            String::from_utf8_lossy(&o.stderr).into_owned(),
        )
    }
}

/// A script package: installed when `$HOME/<name>.installed` exists (version 1.0.0).
fn script_pkg(root: &Path, home: &Path, name: &str, latest: &str, files: &[String]) {
    let mut body = format!(
        "schema = 1\nname = \"{name}\"\n[providers.script]\ncheck = \"test -f $HOME/{name}.installed\"\nversion = \"echo 1.0.0\"\nlatest = \"echo {latest}\"\ninstall = \"touch $HOME/{name}.installed\"\n"
    );
    for f in files {
        let src = format!("files/{}", f.replace('/', "_"));
        body.push_str(&format!(
            "[[files]]\nmode = \"copy\"\nsrc = \"{src}\"\ndst = \"{{home}}/{f}\"\n"
        ));
        write(
            &root.join(format!("packages/{name}/{src}")),
            &format!("# {f}\n"),
        );
    }
    write(&root.join(format!("packages/{name}/package.toml")), &body);
    write(&home.join(format!("{name}.installed")), "");
}

/// `dashboard`: the README's `dots` screen. 67 packages (3 outdated: bottom, delta, yazi), 87
/// copied files (the app changed `~/.claude/settings.json`), two pending manual steps, a git
/// repo at `~/dotfiles` on branch `main`.
fn dashboard() -> Fx {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let root = home.join("dotfiles");
    write(&root.join("dots.toml"), "schema = 1\nname = \"dotfiles\"\n");
    let mut names: Vec<String> = Vec::new();
    for n in ["bottom", "delta", "yazi"] {
        script_pkg(&root, &home, n, "2.0.0", &[]);
        names.push(n.to_string());
    }
    // 63 healthy packages with 86 files between them, plus `claude` with the settings file.
    script_pkg(
        &root,
        &home,
        "claude",
        "1.0.0",
        &[".claude/settings.json".to_string()],
    );
    names.push("claude".into());
    for i in 1..=63 {
        let name = format!("pkg{i:02}");
        let mut files = vec![format!(".config/{name}/a")];
        if i <= 23 {
            files.push(format!(".config/{name}/b"));
        }
        script_pkg(&root, &home, &name, "1.0.0", &files);
        names.push(name);
    }
    let list: Vec<String> = names.iter().map(|n| format!("\"{n}\"")).collect();
    write(
        &root.join("bundles/all.toml"),
        &format!("schema = 1\npackages = [{}]\n", list.join(", ")),
    );
    write(
        &home.join(".config/dots/machine.toml"),
        "schema = 1\nbundles = [\"all\"]\n",
    );
    git(&root, &["init", "-q", "-b", "main"]);
    git(&root, &["add", "-A"]);
    git(&root, &["commit", "-q", "-m", "init"]);

    let fx = Fx { _tmp: tmp, home };
    let (code, out, err) = fx.run(&["apply", "--yes", "--files-only"]);
    assert_eq!(code, 0, "{out}{err}");
    // The app rewrites its config.
    write(
        &fx.home.join(".claude/settings.json"),
        "{\"changed\": true}\n",
    );
    // Latest versions, as if `dots outdated` had just run.
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let versions: serde_json::Map<String, serde_json::Value> = names
        .iter()
        .map(|n| {
            let v = if ["bottom", "delta", "yazi"].contains(&n.as_str()) {
                "2.0.0"
            } else {
                "1.0.0"
            };
            (n.clone(), v.into())
        })
        .collect();
    write(
        &fx.home.join(".cache/dots/latest/script.json"),
        &serde_json::json!({ "schema": 1, "fetched": now, "versions": versions }).to_string(),
    );
    // Two manual steps are pending.
    let state = fx.home.join(".local/state/dots/state.json");
    let mut doc: serde_json::Value = std::fs::read_to_string(&state)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_else(|| serde_json::json!({ "schema": 1 }));
    doc["todos"] = serde_json::json!([
        { "package": "karabiner", "reason": "karabiner permissions", "run": "r-1" },
        { "package": "raycast", "reason": "raycast login", "run": "r-1" },
    ]);
    write(&state, &doc.to_string());
    fx
}

/// `config-set`: `delta` declares the `side_by_side` setting; everything is installed, so the
/// block's `dots apply` has nothing to do.
fn config_set() -> Fx {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let root = home.join("dotfiles");
    write(&root.join("dots.toml"), "schema = 1\nname = \"dotfiles\"\n");
    script_pkg(&root, &home, "delta", "1.0.0", &[]);
    let pkg = root.join("packages/delta/package.toml");
    let mut body = std::fs::read_to_string(&pkg).unwrap();
    body.push_str("[settings.side_by_side]\ntype = \"bool\"\ndefault = false\n");
    write(&pkg, &body);
    write(
        &root.join("bundles/all.toml"),
        "schema = 1\npackages = [\"delta\"]\n",
    );
    write(
        &home.join(".config/dots/machine.toml"),
        "schema = 1\nbundles = [\"all\"]\n",
    );
    git(&root, &["init", "-q", "-b", "main"]);
    git(&root, &["add", "-A"]);
    git(&root, &["commit", "-q", "-m", "init"]);
    let fx = Fx { _tmp: tmp, home };
    let (code, out, err) = fx.run(&["apply", "--yes"]);
    assert_eq!(code, 0, "{out}{err}");
    fx
}

fn fixture(name: &str) -> Fx {
    match name {
        "dashboard" => dashboard(),
        "config-set" => config_set(),
        other => panic!("README block `{other}` has no fixture in tests/readme.rs"),
    }
}

/// Normalization 1 and 2 on README text; 2 on output.
fn fold_readme(s: &str) -> String {
    s.replace('▲', "[!]")
        .replace('✓', "[ok]")
        .replace('↑', "[up]")
        .replace('✎', "[mod]")
        .replace('○', "[ ]")
        .replace('→', "->")
        .replace('·', "-")
        .replace('…', "...")
}

fn squash(s: &str) -> Vec<String> {
    s.lines()
        .map(|l| l.split_whitespace().collect::<Vec<_>>().join(" "))
        .collect()
}

/// Normalizations 3 and 4.
fn mask_variable(lines: &mut [String]) {
    for l in lines.iter_mut() {
        if let Some(at) = l.find("@ ") {
            let rest = &l[at + 2..];
            let n = rest.chars().take_while(char::is_ascii_hexdigit).count();
            if (7..=40).contains(&n) {
                *l = format!("{}@ <sha>{}", &l[..at], &rest[n..]);
            }
        }
    }
}

fn mask_context(lines: &mut [String]) {
    let Some(first) = lines.iter_mut().find(|l| l.starts_with("dots ")) else {
        return;
    };
    let mut parts: Vec<String> = first.split(" - ").map(str::to_string).collect();
    let n = parts.len();
    if n >= 3 {
        parts[n - 2] = "work-mbp".into();
        parts[n - 1] = "macOS arm64".into();
    }
    *first = parts.join(" - ");
}

fn command_args(line: &str) -> Vec<String> {
    let cmd = line.strip_prefix("$ ").unwrap();
    let cmd = cmd.split("  #").next().unwrap().trim();
    let mut words = cmd.split_whitespace();
    assert_eq!(words.next(), Some("dots"), "{line}");
    words.map(str::to_string).collect()
}

#[test]
fn readme_console_blocks_are_goldens_or_explicitly_skipped() {
    let blocks = readme_blocks();
    assert!(!blocks.is_empty());
    let (mut passed, mut skipped) = (Vec::new(), Vec::new());
    for b in &blocks {
        if let Some(rest) = b.marker.strip_prefix("<!-- golden-skip:") {
            let rest = rest.trim_end_matches("-->").trim();
            let mut f = rest.split('|').map(str::trim);
            let name = f.next().unwrap();
            let milestone = f.next().unwrap_or("");
            assert!(
                milestone.starts_with("v0.") && !milestone.starts_with("v0.1"),
                "{rest}"
            );
            skipped.push(format!("{name} ({milestone})"));
            continue;
        }
        let name = b
            .marker
            .strip_prefix("<!-- golden:")
            .and_then(|r| r.strip_suffix("-->"))
            .map(str::trim)
            .unwrap_or_else(|| {
                panic!(
                    "console block without a golden marker:\n{}",
                    b.lines.join("\n")
                )
            });
        let fx = fixture(name);
        assert!(b.lines[0].starts_with("$ "), "{name}");
        // A block that shows only commands (no output) must run clean, one after the other.
        if b.lines
            .iter()
            .filter(|l| !l.trim().is_empty())
            .all(|l| l.starts_with("$ "))
        {
            for line in b.lines.iter().filter(|l| l.starts_with("$ ")) {
                let args = command_args(line);
                let args: Vec<&str> = args.iter().map(String::as_str).collect();
                let (code, stdout, stderr) = fx.run(&args);
                assert_eq!(code, 0, "{name}: `{line}` exited {code}:\n{stdout}{stderr}");
            }
            passed.push(name.to_string());
            continue;
        }
        let args = command_args(&b.lines[0]);
        let args: Vec<&str> = args
            .iter()
            .map(String::as_str)
            .chain(["--color", "never"])
            .collect();
        let (_code, stdout, stderr) = fx.run(&args);
        assert!(stderr.is_empty(), "{name}: stderr: {stderr}");

        let mut want = squash(&fold_readme(&b.lines[1..].join("\n")));
        let mut got = squash(&stdout);
        assert!(
            stdout.is_ascii(),
            "{name}: plain output must be ASCII:\n{stdout}"
        );
        for v in [&mut want, &mut got] {
            mask_variable(v);
            mask_context(v);
        }
        assert_eq!(
            got, want,
            "README block `{name}` and `{}` disagree:\n{stdout}",
            b.lines[0]
        );
        passed.push(name.to_string());
    }
    println!("README goldens passed: {passed:?}; skipped: {skipped:?}");
    assert_eq!(
        passed,
        ["config-set", "dashboard"],
        "the golden blocks changed: update this test"
    );
    assert_eq!(
        skipped,
        ["add-bundle (v0.3)", "pull (v0.3)"],
        "the skipped blocks changed: update this test"
    );
}
