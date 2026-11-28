#![allow(clippy::cloned_ref_to_slice_refs)]
use std::fs;
use std::path::{Path, PathBuf};

use dots_files::*;
use dots_model::FileEntry;
use serde_json::json;

struct Env {
    _tmp: tempfile::TempDir,
    home: PathBuf,
    state: PathBuf,
    root: PathBuf,
}

fn env() -> Env {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let state = home.join(".local/state/dots");
    let root = tmp.path().join("dotfiles");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&root).unwrap();
    Env {
        _tmp: tmp,
        home,
        state,
        root,
    }
}

impl Env {
    fn ctx(&self, run: &str) -> FilesCtx {
        let mut c = FilesCtx::new(&self.home, &self.state, &self.root, "root1", run);
        c.os = "macos".into();
        c.chain = vec!["any".into(), "unix".into(), "macos".into()];
        c
    }
    fn write(&self, rel: &str, text: &str) -> PathBuf {
        let p = self.root.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(&p, text).unwrap();
        p
    }
    fn file(
        &self,
        pkg: &str,
        mode: Mode,
        dst: &str,
        src: Option<PathBuf>,
        layers: &[&str],
        format: &str,
    ) -> ResolvedFile {
        ResolvedFile {
            package: pkg.into(),
            mode,
            dst: self.home.join(dst),
            src,
            layers: layers
                .iter()
                .map(|l| LayerSrc {
                    path: self.root.join(l),
                    label: l.to_string(),
                })
                .collect(),
            format: format.into(),
            kind: None,
            template: None,
            merge: Default::default(),
            comment: "#".into(),
            extras: Default::default(),
        }
    }
}

fn apply_all(files: &[ResolvedFile], ctx: &FilesCtx) -> Vec<FileOutcome> {
    plan(files, ctx).iter().map(|s| apply(s, ctx)).collect()
}

fn render_text(e: &Env, f: &ResolvedFile) -> String {
    render_layers(f, &e.ctx("r")).unwrap()
}

#[test]
fn per_platform_destination() {
    let e = env();
    let src = e.write("pkg/c.toml", "a = 1\n");
    let entry: FileEntry = serde_json::from_value(json!({
        "dst": { "default": "~/.config/ez/c.toml", "macos": "{app_support}/ez/c.toml" },
        "src": "c.toml"
    }))
    .unwrap();
    let ctx = e.ctx("r1");
    let f = resolve_entry("pkg", &e.root.join("pkg"), &entry, &ctx, &|_| Ok(true), &[])
        .unwrap()
        .unwrap();
    assert_eq!(f.dst, e.home.join("Library/Application Support/ez/c.toml"));
    assert_eq!(f.src.as_deref(), Some(src.as_path()));
    let mut linux = e.ctx("r2");
    linux.chain = vec!["any".into(), "unix".into(), "linux".into()];
    linux.os = "linux".into();
    let f = resolve_entry(
        "pkg",
        &e.root.join("pkg"),
        &entry,
        &linux,
        &|_| Ok(true),
        &[],
    )
    .unwrap()
    .unwrap();
    assert_eq!(f.dst, e.home.join(".config/ez/c.toml"));
}

#[test]
fn most_specific_platform_wins_and_user_vars_override() {
    let e = env();
    let mut ctx = e.ctx("r");
    ctx.vars.insert("app_support".into(), "~/AS".into());
    ctx.chain = vec![
        "any".into(),
        "unix".into(),
        "macos".into(),
        "macos-15".into(),
    ];
    let entry: FileEntry = serde_json::from_value(json!({
        "dst": { "default": "~/d", "macos": "~/m", "macos-15": "{app_support}/x" }, "mode": "dir"
    }))
    .unwrap();
    let f = resolve_entry("p", &e.root, &entry, &ctx, &|_| Ok(true), &[])
        .unwrap()
        .unwrap();
    assert_eq!(f.dst, e.home.join("AS/x"));
    let bad: FileEntry =
        serde_json::from_value(json!({ "dst": "{nope}/x", "mode": "dir" })).unwrap();
    let err = resolve_entry("p", &e.root, &bad, &ctx, &|_| Ok(true), &[]).unwrap_err();
    assert_eq!(err.code, "unknown-name");
}

#[test]
fn link_goes_through_active() {
    let e = env();
    let src = e.write("packages/ghostty/config", "font-size = 14\n");
    let f = e.file(
        "ghostty",
        Mode::Link,
        ".config/ghostty/config",
        Some(src),
        &[],
        "text",
    );
    let ctx = e.ctx("r1");
    let out = apply_all(&[f.clone()], &ctx);
    assert_eq!(out[0].status, OutcomeStatus::Applied, "{out:?}");
    let target = fs::read_link(&f.dst).unwrap();
    assert_eq!(target, e.state.join("active/packages/ghostty/config"));
    assert_eq!(fs::read_to_string(&f.dst).unwrap(), "font-size = 14\n");
    // Switching roots flips one link.
    assert_eq!(fs::read_link(e.state.join("active")).unwrap(), e.root);
    let st = status(&[f.clone()], &ctx);
    assert_eq!(st[0].state, FileState::Ok);
    // second apply is a no-op
    assert_eq!(plan(&[f.clone()], &ctx)[0].action, Action::Unchanged);
    // worktree vanishes: dangling
    // (a directory symlink is removed with `remove_dir` on Windows)
    let active = e.state.join("active");
    fs::remove_file(&active)
        .or_else(|_| fs::remove_dir(&active))
        .unwrap();
    assert_eq!(status(&[f], &ctx)[0].state, FileState::Dangling);
}

#[test]
fn array_replace_and_delete_marker_and_toml_comments() {
    let e = env();
    e.write(
        "l/common",
        "# base comment\nname = \"x\"\npaths = [\"a\"]\nold = 1\n[t]\nk = 1\n",
    );
    e.write(
        "l/macos",
        "paths = [\"b\"]\nold = \"__dots_delete__\"\n[t]\nj = 2\n",
    );
    let f = e.file(
        "p",
        Mode::Render,
        "c.toml",
        None,
        &["l/common", "l/macos"],
        "toml",
    );
    let out = render_text(&e, &f);
    assert!(out.contains("paths = [\"b\"]"), "{out}");
    assert!(!out.contains("\"a\""));
    assert!(!out.contains("old ="));
    assert!(out.contains("# base comment"));
    assert!(out.contains("k = 1") && out.contains("j = 2"));
    assert!(out.starts_with("# Generated by dots"));
}

#[test]
fn merge_strategies_toml_and_json() {
    let e = env();
    e.write("l/a.toml", "[[servers]]\nname = \"x\"\nport = 1\n[[servers]]\nname = \"y\"\nport = 2\nkeys = [\"a\"]\n");
    e.write(
        "l/b.toml",
        "[[servers]]\nname = \"x\"\nhost = \"h\"\n[[servers]]\nname = \"z\"\nport = 3\n",
    );
    let mut f = e.file(
        "p",
        Mode::Render,
        "c.toml",
        None,
        &["l/a.toml", "l/b.toml"],
        "toml",
    );
    f.merge
        .insert("servers".into(), Strategy::By("name".into()));
    let out = render_text(&e, &f);
    assert_eq!(out.matches("name = \"x\"").count(), 1, "{out}");
    assert!(out.contains("host = \"h\"") && out.contains("port = 1"));
    assert!(out.contains("name = \"z\"") && out.contains("name = \"y\""));

    e.write(
        "l/a.json",
        "{\"servers\":[{\"name\":\"x\",\"port\":1}],\"list\":[1],\"u\":[1,2]}",
    );
    e.write(
        "l/b.json",
        "{\"servers\":[{\"name\":\"x\",\"host\":\"h\"}],\"list\":[2],\"u\":[2,3]}",
    );
    let mut f = e.file(
        "p",
        Mode::Render,
        "c.json",
        None,
        &["l/a.json", "l/b.json"],
        "json",
    );
    f.merge
        .insert("servers".into(), Strategy::By("name".into()));
    f.merge.insert("list".into(), Strategy::Append);
    f.merge.insert("u".into(), Strategy::Union);
    let v: serde_json::Value = serde_json::from_str(&render_text(&e, &f)).unwrap();
    assert_eq!(v["servers"], json!([{"name":"x","port":1,"host":"h"}]));
    assert_eq!(v["list"], json!([1, 2]));
    assert_eq!(v["u"], json!([1, 2, 3]));
    assert!(!render_text(&e, &f).starts_with('#'), "json has no header");
}

#[test]
fn type_mismatch_shows_both_layers() {
    let e = env();
    e.write("l/a.yaml", "x: \"s\"\n");
    e.write("l/b.yaml", "x: [1]\n");
    let f = e.file(
        "p",
        Mode::Render,
        "c.yaml",
        None,
        &["l/a.yaml", "l/b.yaml"],
        "yaml",
    );
    let err = render_layers(&f, &e.ctx("r")).unwrap_err();
    assert_eq!(err.code, "merge-type-mismatch");
    assert!(
        err.message.contains("l/a.yaml") && err.message.contains("l/b.yaml"),
        "{}",
        err.message
    );
    assert_eq!(err.provenance.len(), 2);
    // toml too
    e.write("l/a.toml", "x = \"s\"\n");
    e.write("l/b.toml", "x = 1\n");
    let f = e.file(
        "p",
        Mode::Render,
        "c.toml",
        None,
        &["l/a.toml", "l/b.toml"],
        "toml",
    );
    assert_eq!(
        render_layers(&f, &e.ctx("r")).unwrap_err().code,
        "merge-type-mismatch"
    );
}

#[test]
fn parse_errors_report_file_and_line() {
    let e = env();
    e.write("l/bad.json", "{\n \"a\": 1,\n \"b\": }\n");
    let f = e.file("p", Mode::Render, "c.json", None, &["l/bad.json"], "json");
    let err = render_layers(&f, &e.ctx("r")).unwrap_err();
    assert_eq!(err.code, "layer-parse-error");
    assert_eq!(err.provenance[0].line, Some(3));
}

#[test]
fn jsonc_comments_and_trailing_commas_parse() {
    let e = env();
    e.write(
        "l/a.jsonc",
        "{\n // c\n \"a\": [1,], /* x */ \"b\": {\"c\": 1,},\n}\n",
    );
    e.write("l/b.jsonc", "{\"b\": {\"d\": 2}}");
    let f = e.file(
        "p",
        Mode::Render,
        "c.jsonc",
        None,
        &["l/a.jsonc", "l/b.jsonc"],
        "jsonc",
    );
    let out = render_text(&e, &f);
    assert!(out.starts_with("// Generated by dots"));
    let body: String = out
        .lines()
        .filter(|l| !l.starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n");
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["b"], json!({"c": 1, "d": 2}));
}

#[test]
fn yaml_ini_markdown_text() {
    let e = env();
    e.write("l/a.yaml", "# keep me\na:\n  b: 1\nl: [1]\n");
    e.write("l/b.yaml", "a:\n  c: 2\nl: [2]\n");
    let f = e.file(
        "p",
        Mode::Render,
        "c.yaml",
        None,
        &["l/a.yaml", "l/b.yaml"],
        "yaml",
    );
    let v: serde_yaml_ng::Value = serde_yaml_ng::from_str(&render_text(&e, &f)).unwrap();
    assert_eq!(v["a"]["b"], serde_yaml_ng::Value::from(1));
    assert_eq!(v["a"]["c"], serde_yaml_ng::Value::from(2));
    assert_eq!(v["l"][0], serde_yaml_ng::Value::from(2));
    // single yaml layer keeps its comments
    let f1 = e.file("p", Mode::Render, "c.yaml", None, &["l/a.yaml"], "yaml");
    assert!(render_text(&e, &f1).contains("# keep me"));

    e.write("l/a.ini", "[user]\nname = A\n[core]\nx = 1\n");
    e.write(
        "l/b.ini",
        "[user]\nemail = e\n[remote \"origin\"]\nurl = u\n",
    );
    let f = e.file(
        "p",
        Mode::Render,
        ".gitconfig",
        None,
        &["l/a.ini", "l/b.ini"],
        "gitconfig",
    );
    let out = render_text(&e, &f);
    assert!(out.contains("[user]\n\tname = A\n\temail = e\n"), "{out}");
    assert!(out.contains("[remote \"origin\"]\n\turl = u"));

    e.write("l/a.md", "---\nname: s\ntags: [a]\n---\nbody one\n");
    e.write("l/b.md", "---\ntags: [b]\ndisable: true\n---\nbody two\n");
    let f = e.file(
        "p",
        Mode::Render,
        "SKILL.md",
        None,
        &["l/a.md", "l/b.md"],
        "markdown",
    );
    let out = render_text(&e, &f);
    assert!(out.starts_with("---\n"), "frontmatter stays first: {out}");
    assert!(
        out.contains("tags:\n- b") && out.contains("disable: true") && out.contains("name: s"),
        "{out}"
    );
    assert!(
        out.contains("body one")
            && out.contains("body two")
            && out.contains("<!-- dots: from l/b.md -->")
    );

    e.write("l/t1", "one\n");
    e.write("l/t2", "two\n");
    let f = e.file("p", Mode::Render, "t", None, &["l/t1", "l/t2"], "text");
    let out = render_text(&e, &f);
    assert!(
        out.contains("# --- dots: from l/t1 ---\none\n# --- dots: from l/t2 ---\ntwo\n"),
        "{out}"
    );
}

#[test]
fn auto_layers_pick_common_then_chain() {
    let e = env();
    e.write("pkg/config/common", "a = 1\n");
    e.write("pkg/config/ubuntu-22.04", "b = 2\n");
    e.write("pkg/config/arch", "c = 3\n");
    e.write("pkg/config/ubuntu", "u = 1\n");
    let mut ctx = e.ctx("r");
    ctx.chain = vec![
        "any".into(),
        "unix".into(),
        "linux".into(),
        "debian".into(),
        "ubuntu".into(),
        "ubuntu-22.04".into(),
    ];
    let entry: FileEntry = serde_json::from_value(json!({
        "dst": "~/.config/x.toml", "mode": "render", "format": "toml",
        "layers": { "dir": "config", "auto": true }
    }))
    .unwrap();
    let f = resolve_entry("pkg", &e.root.join("pkg"), &entry, &ctx, &|_| Ok(true), &[])
        .unwrap()
        .unwrap();
    let labels: Vec<_> = f.layers.iter().map(|l| l.label.as_str()).collect();
    assert_eq!(
        labels,
        [
            "pkg/config/common",
            "pkg/config/ubuntu",
            "pkg/config/ubuntu-22.04"
        ]
    );
    let out = render_layers(&f, &ctx).unwrap();
    assert!(out.contains("a = 1") && out.contains("b = 2") && !out.contains("c = 3"));
}

#[test]
fn ghostty_native_include() {
    let e = env();
    e.write("pkg/config/common", "font-size = 14\n");
    e.write("pkg/config/macos", "x = 1\n");
    let ctx = e.ctx("r");
    let entry: FileEntry = serde_json::from_value(json!({
        "dst": "~/.config/ghostty/config", "mode": "render", "kind": "include",
        "layers": { "dir": "config", "auto": true }
    }))
    .unwrap();
    let f = resolve_entry(
        "ghostty",
        &e.root.join("pkg"),
        &entry,
        &ctx,
        &|_| Ok(true),
        &[],
    )
    .unwrap()
    .unwrap();
    let out = render_layers(&f, &ctx).unwrap();
    let a = e.state.join("active").join("pkg").join("config");
    assert!(
        out.contains(&format!("config-file = {}\n", a.join("common").display())),
        "{out}"
    );
    assert!(out.contains(&format!("config-file = {}\n", a.join("macos").display())));
    assert!(
        !out.contains("font-size"),
        "layers are referenced, not merged"
    );
    // an unknown app has no include
    let mut other = f.clone();
    other.dst = e.home.join("random.conf");
    assert_eq!(
        render_layers(&other, &ctx).unwrap_err().code,
        "include-unsupported"
    );
}

#[test]
fn header_names_layers_and_settings() {
    let e = env();
    e.write(
        "l/g",
        "font-size = @dots(setting.font_size)@\ntheme = @dots(setting.theme)@\ntoken = @dots(setting.token)@\n",
    );
    let f = e.file(
        "ghostty",
        Mode::Render,
        ".config/ghostty/config",
        None,
        &["l/g"],
        "text",
    );
    let mut ctx = e.ctx("r");
    ctx.settings = vec![
        SettingUse {
            name: "font_size".into(),
            value: json!(15),
            scope: "machine".into(),
            secret: false,
        },
        SettingUse {
            name: "theme".into(),
            value: json!("dark"),
            scope: "default".into(),
            secret: false,
        },
        SettingUse {
            name: "token".into(),
            value: json!("tok-9"),
            scope: "machine".into(),
            secret: true,
        },
    ];
    let out = render_layers(&f, &ctx).unwrap();
    assert!(out.contains("font_size=15(machine)"), "{out}");
    assert!(out.contains("theme=dark(default)") && out.contains("layers: l/g"));
    assert!(out.contains("token=•••(machine)"), "{out}");
    let header: String = out.lines().filter(|l| l.starts_with('#')).collect();
    assert!(!header.contains("tok-9"), "{header}");
}

#[test]
fn jinja_templates_are_opt_in() {
    let e = env();
    e.write("l/t", "size = {{ setting.size }}\n");
    let mut f = e.file("p", Mode::Render, "t", None, &["l/t"], "text");
    let mut ctx = e.ctx("r");
    ctx.template_vars = json!({"setting": {"size": 15}});
    assert!(
        render_layers(&f, &ctx)
            .unwrap()
            .contains("size = {{ setting.size }}")
    );
    f.template = Some("jinja".into());
    assert!(render_layers(&f, &ctx).unwrap().contains("size = 15"));
    e.write("l/u", "{{ setting.nope }}\n");
    let mut g = e.file("p", Mode::Render, "t", None, &["l/u"], "text");
    g.template = Some("jinja".into());
    let err = render_layers(&g, &ctx).unwrap_err();
    assert_eq!(err.code, "unknown-name");
}

#[test]
fn render_writes_gen_and_links_there() {
    let e = env();
    e.write("l/a.toml", "a = 1\n");
    let f = e.file(
        "p",
        Mode::Render,
        ".config/x/c.toml",
        None,
        &["l/a.toml"],
        "toml",
    );
    let ctx = e.ctx("r1");
    assert_eq!(
        apply_all(&[f.clone()], &ctx)[0].status,
        OutcomeStatus::Applied
    );
    let t = fs::read_link(&f.dst).unwrap();
    assert!(t.starts_with(e.state.join("gen/root1")), "{t:?}");
    assert!(fs::read_to_string(&f.dst).unwrap().contains("a = 1"));
    assert_eq!(status(&[f.clone()], &ctx)[0].state, FileState::Ok);
    // source layer changes: stale
    e.write("l/a.toml", "a = 2\n");
    assert_eq!(status(&[f.clone()], &ctx)[0].state, FileState::Stale);
    apply_all(&[f.clone()], &ctx);
    assert_eq!(status(&[f], &ctx)[0].state, FileState::Ok);
}

#[test]
fn first_apply_over_existing_backs_up_and_restore_brings_it_back() {
    let e = env();
    let dst = e.home.join(".config/yazi");
    fs::create_dir_all(&dst).unwrap();
    fs::write(dst.join("yazi.toml"), "mine").unwrap();
    let src = e.write("pkg/yazi/y", "x");
    let f = e.file("yazi", Mode::Link, ".config/yazi", Some(src), &[], "text");
    let ctx = e.ctx("run-1");
    assert_eq!(status(&[f.clone()], &ctx)[0].state, FileState::Conflict);
    let steps = plan(&[f.clone()], &ctx);
    assert_eq!(steps[0].action, Action::Replace);
    assert_eq!(steps[0].backup, BackupKind::Move);
    let out = apply(&steps[0], &ctx);
    assert_eq!(out.status, OutcomeStatus::Applied);
    let b = out.backup.unwrap();
    assert!(b.starts_with(e.state.join("backup/run-1")));
    assert_eq!(fs::read_to_string(b.join("yazi.toml")).unwrap(), "mine");
    assert!(
        fs::symlink_metadata(&f.dst)
            .unwrap()
            .file_type()
            .is_symlink()
    );

    let rep = restore(&e.state, "run-1", false).unwrap();
    assert_eq!(rep.restored, vec![f.dst.clone()]);
    assert!(rep.skipped.is_empty());
    assert_eq!(fs::read_to_string(dst.join("yazi.toml")).unwrap(), "mine");
    assert!(fs::symlink_metadata(&dst).unwrap().is_dir());
    assert_eq!(
        status(&[f], &ctx)[0].state,
        FileState::Conflict,
        "no longer owned"
    );
    assert_eq!(
        restore(&e.state, "nope", false).unwrap_err().code,
        "backup-not-found"
    );
}

#[test]
fn restore_skips_paths_changed_since() {
    let e = env();
    let p = e.home.join(".zshenv");
    fs::write(&p, "orig").unwrap();
    let src = e.write("pkg/zshenv", "new\n");
    let f = e.file("z", Mode::Copy, ".zshenv", Some(src), &[], "text");
    let ctx = e.ctx("run-2");
    apply_all(&[f], &ctx);
    assert_eq!(fs::read_to_string(&p).unwrap(), "new\n");
    fs::write(&p, "user edit").unwrap();
    let rep = restore(&e.state, "run-2", false).unwrap();
    assert!(rep.restored.is_empty());
    assert_eq!(rep.skipped[0].code, "restore-skipped");
    assert_eq!(fs::read_to_string(&p).unwrap(), "user edit");
    // after the user moves it, restore works
    fs::remove_file(&p).unwrap();
    assert_eq!(restore(&e.state, "run-2", false).unwrap().restored.len(), 1);
    assert_eq!(fs::read_to_string(&p).unwrap(), "orig");
}

#[test]
fn crash_during_write_leaves_original_intact() {
    let e = env();
    let p = e.home.join("f.txt");
    fs::write(&p, "old").unwrap();
    let err = atomic_write_hooked(&p, b"new content", &|s| {
        assert_eq!(s, Stage::AfterTempWrite);
        Err(std::io::Error::other("killed"))
    });
    assert!(err.is_err());
    assert_eq!(fs::read_to_string(&p).unwrap(), "old");
    let leftovers: Vec<_> = fs::read_dir(&e.home)
        .unwrap()
        .map(|x| x.unwrap().file_name())
        .collect();
    assert_eq!(leftovers.len(), 1, "temp file cleaned: {leftovers:?}");
    atomic_write(&p, b"new").unwrap();
    assert_eq!(fs::read_to_string(&p).unwrap(), "new");
    // symlink swap never exposes a missing link
    let t1 = e.home.join("t1");
    let t2 = e.home.join("t2");
    fs::write(&t1, "1").unwrap();
    fs::write(&t2, "2").unwrap();
    let l = e.home.join("l");
    atomic_symlink(&t1, &l).unwrap();
    atomic_symlink(&t2, &l).unwrap();
    assert_eq!(fs::read_to_string(&l).unwrap(), "2");
}

#[test]
fn app_rewrote_config_is_modified_with_resolutions() {
    let e = env();
    e.write("l/s.json", "{\"a\":1}");
    e.write("l/s2.json", "{\"b\":2}");
    let f = e.file(
        "claude",
        Mode::Render,
        ".claude/settings.json",
        None,
        &["l/s.json", "l/s2.json"],
        "json",
    );
    let ctx = e.ctx("r1");
    apply_all(&[f.clone()], &ctx);
    assert_eq!(status(&[f.clone()], &ctx)[0].state, FileState::Ok);

    // 1. the app writes through the link
    fs::write(&f.dst, "{\"a\":1,\"b\":2,\"added\":true}").unwrap();
    let st = &status(&[f.clone()], &ctx)[0];
    assert_eq!(st.state, FileState::Modified);
    assert!(st.hint.as_ref().unwrap().contains("dots adopt"));
    let ids: Vec<_> = st.resolutions.iter().map(|r| r.id.as_str()).collect();
    assert_eq!(ids, ["adopt", "adopt-machine", "overwrite"]);
    assert_eq!(
        st.resolutions[0].command,
        format!("dots adopt {}", f.dst.display())
    );
    assert!(serde_json::to_string(st).unwrap().contains("\"modified\""));

    // 2. the app replaced the symlink with a regular file (atomic rename)
    fs::remove_file(&f.dst).unwrap();
    fs::write(&f.dst, "{\"a\":9}").unwrap();
    assert_eq!(status(&[f.clone()], &ctx)[0].state, FileState::Modified);

    // applying again backs up the user's version before overwriting
    let steps = plan(&[f.clone()], &ctx);
    assert_eq!(steps[0].action, Action::Update);
    let out = apply(&steps[0], &ctx);
    assert_eq!(
        fs::read_to_string(out.backup.unwrap()).unwrap(),
        "{\"a\":9}"
    );
    assert_eq!(status(&[f], &ctx)[0].state, FileState::Ok);
}

#[test]
fn copy_modified_and_stale() {
    let e = env();
    let src = e.write("pkg/c", "v1\n");
    let f = e.file("c", Mode::Copy, "c.txt", Some(src), &[], "text");
    let ctx = e.ctx("r1");
    assert_eq!(status(&[f.clone()], &ctx)[0].state, FileState::Missing);
    apply_all(&[f.clone()], &ctx);
    assert_eq!(status(&[f.clone()], &ctx)[0].state, FileState::Ok);
    e.write("pkg/c", "v2\n");
    assert_eq!(status(&[f.clone()], &ctx)[0].state, FileState::Stale);
    fs::write(&f.dst, "edited").unwrap();
    assert_eq!(status(&[f], &ctx)[0].state, FileState::Modified);
}

/// A package's inject block and dots' own env block (`# >>> dots env >>>`) share one profile
/// without touching each other, and the profile's directory is created when it is missing.
#[test]
fn an_inject_block_shares_a_profile_with_the_env_block() {
    let e = env();
    let src = e.write("pkg/profile.inject", "Set-Alias ll ls\n");
    let rc = ".config/powershell/Microsoft.PowerShell_profile.ps1";
    let f = e.file("pwsh-conf", Mode::Inject, rc, Some(src), &[], "text");
    let ctx = e.ctx("r1");
    apply_all(&[f.clone()], &ctx);
    let path = e.home.join(rc);
    // The env step adds its block (what `dots_env::inject_rc` writes) after the package's.
    let env_block = "# >>> dots env >>>\nif (Test-Path x) { . x }\n# <<< dots env <<<\n";
    let mut text = fs::read_to_string(&path).unwrap();
    text.push_str(env_block);
    fs::write(&path, &text).unwrap();
    // The package's block is still ok and untouched by a re-apply; the env block survives.
    assert_eq!(status(&[f.clone()], &ctx)[0].state, FileState::Ok);
    assert_eq!(plan(&[f.clone()], &ctx)[0].action, Action::Unchanged);
    apply_all(&[f.clone()], &ctx);
    assert_eq!(fs::read_to_string(&path).unwrap(), text);
    // Removing the package's block leaves the env block.
    let removals = plan_removals(&[], &ctx, None);
    assert_eq!(apply(&removals[0], &ctx).status, OutcomeStatus::Applied);
    let left = fs::read_to_string(&path).unwrap();
    assert!(
        left.contains(env_block) && !left.contains("dots:pwsh-conf"),
        "{left}"
    );
}

#[test]
fn inject_is_idempotent_and_removable() {
    let e = env();
    let src = e.write("pkg/rc", "source ~/.dots/env.sh\n");
    let zshrc = e.home.join(".zshrc");
    fs::write(&zshrc, "export A=1").unwrap();
    let f = e.file(
        "shell",
        Mode::Inject,
        ".zshrc",
        Some(src.clone()),
        &[],
        "text",
    );
    let ctx = e.ctx("r1");
    assert_eq!(status(&[f.clone()], &ctx)[0].state, FileState::Missing);
    apply_all(&[f.clone()], &ctx);
    let once = fs::read_to_string(&zshrc).unwrap();
    assert!(once.starts_with("export A=1\n") && once.contains("source ~/.dots/env.sh"));
    assert_eq!(status(&[f.clone()], &ctx)[0].state, FileState::Ok);
    let steps = plan(&[f.clone()], &ctx);
    assert_eq!(steps[0].action, Action::Unchanged);
    apply_all(&[f.clone()], &ctx);
    assert_eq!(fs::read_to_string(&zshrc).unwrap(), once, "idempotent");
    // block changed in source: replaced in place
    e.write("pkg/rc", "source ~/.dots/new.sh\n");
    assert_eq!(status(&[f.clone()], &ctx)[0].state, FileState::Stale);
    apply_all(&[f.clone()], &ctx);
    let now = fs::read_to_string(&zshrc).unwrap();
    assert_eq!(now.matches(">>> dots:shell").count(), 1);
    assert!(now.contains("new.sh") && !now.contains("env.sh"));
    // user edits inside the block
    fs::write(&zshrc, now.replace("new.sh", "hand.sh")).unwrap();
    assert_eq!(status(&[f.clone()], &ctx)[0].state, FileState::Modified);
    apply_all(&[f.clone()], &ctx);
    // removal strips only our block
    let removals = plan_removals(&[], &ctx, None);
    assert_eq!(removals.len(), 1);
    assert_eq!(apply(&removals[0], &ctx).status, OutcomeStatus::Applied);
    assert_eq!(fs::read_to_string(&zshrc).unwrap(), "export A=1\n");
}

#[test]
fn dir_mode_and_parents_cleanup() {
    let e = env();
    let f = e.file("d", Mode::Dir, "workspace/personal", None, &[], "text");
    let ctx = e.ctx("r1");
    apply_all(&[f.clone()], &ctx);
    assert!(f.dst.is_dir());
    assert_eq!(status(&[f.clone()], &ctx)[0].state, FileState::Ok);
    let rm = plan_removals(&[], &ctx, Some(&["d".to_string()]));
    assert_eq!(apply(&rm[0], &ctx).status, OutcomeStatus::Applied);
    assert!(
        !e.home.join("workspace").exists(),
        "created parents removed when empty"
    );
}

#[test]
fn removal_only_touches_owned_paths() {
    let e = env();
    let src = e.write("pkg/f", "x");
    let f = e.file("p", Mode::Link, "l", Some(src), &[], "text");
    let ctx = e.ctx("r1");
    apply_all(&[f.clone()], &ctx);
    // someone replaced our link with a real file: removal leaves it
    fs::remove_file(&f.dst).unwrap();
    fs::write(&f.dst, "theirs").unwrap();
    let rm = plan_removals(&[], &ctx, None);
    assert_eq!(apply(&rm[0], &ctx).status, OutcomeStatus::Skipped);
    assert_eq!(fs::read_to_string(&f.dst).unwrap(), "theirs");
    // an unowned path has no removal at all
    fs::write(e.home.join("other"), "z").unwrap();
    assert_eq!(plan_removals(&[], &ctx, None).len(), 1);
}

#[test]
fn dry_run_writes_nothing_and_plan_serializes() {
    let e = env();
    let src = e.write("pkg/f", "x");
    let f = e.file("p", Mode::Link, "l", Some(src), &[], "text");
    let mut ctx = e.ctx("r1");
    ctx.dry_run = true;
    let steps = plan(&[f], &ctx);
    let j = serde_json::to_value(&steps).unwrap();
    assert_eq!(j[0]["action"], "create");
    assert_eq!(j[0]["mode"], "link");
    assert_eq!(apply(&steps[0], &ctx).status, OutcomeStatus::DryRun);
    assert!(!e.home.join("l").exists());
    assert!(!e.state.exists());
}

#[test]
fn unreadable_layer_fails_the_step_not_the_run() {
    let e = env();
    let f = e.file(
        "p",
        Mode::Render,
        "c.toml",
        None,
        &["l/missing.toml"],
        "toml",
    );
    let ctx = e.ctx("r1");
    let steps = plan(&[f], &ctx);
    assert_eq!(steps[0].action, Action::Failed);
    assert_eq!(apply(&steps[0], &ctx).status, OutcomeStatus::Failed);
    let _ = Path::new("");
}
