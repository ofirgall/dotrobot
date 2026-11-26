#![allow(dead_code)]
use std::fs;
use std::path::PathBuf;

use dots_files::*;
use serde_json::json;

pub struct Env {
    pub _tmp: tempfile::TempDir,
    pub home: PathBuf,
    pub state: PathBuf,
    pub root: PathBuf,
}

pub fn env() -> Env {
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

pub fn setting(name: &str, value: serde_json::Value, scope: &str) -> SettingUse {
    SettingUse {
        name: name.into(),
        value,
        scope: scope.into(),
        secret: false,
    }
}

impl Env {
    pub fn ctx(&self, run: &str) -> FilesCtx {
        let mut c = FilesCtx::new(&self.home, &self.state, &self.root, "root1", run);
        c.os = "macos".into();
        c.chain = vec!["any".into(), "unix".into(), "macos".into()];
        c.settings = vec![
            setting("font_size", json!(15), "machine"),
            setting("theme", json!("auto"), "default"),
            setting("telemetry", json!(false), "default"),
            setting("cursor_shader", json!(true), "repo"),
            setting("name", json!("it's me"), "default"),
        ];
        c.template_vars = json!({ "tags": ["work"], "facts": { "hostname": "box" } });
        c
    }
    pub fn write(&self, rel: &str, text: &str) -> PathBuf {
        let p = self.root.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(&p, text).unwrap();
        p
    }
    /// A render of `layers` (relative to the root) to `dst` (relative to home).
    pub fn file(&self, pkg: &str, dst: &str, layers: &[&str], format: &str) -> ResolvedFile {
        ResolvedFile {
            package: pkg.into(),
            mode: Mode::Render,
            dst: self.home.join(dst),
            src: None,
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
            comment: if format == "jsonc" { "//" } else { "#" }.into(),
            extras: Default::default(),
        }
    }
    /// Render one file with a single layer holding `text`.
    pub fn one(&self, format: &str, text: &str) -> Result<String, dots_core::Diagnostic> {
        self.write("l/one", text);
        let f = self.file("pkg", "out", &["l/one"], format);
        render_layers(&f, &self.ctx("r"))
    }
}

/// The text after the generated header (the leading comment block).
pub fn body(out: &str) -> String {
    out.lines()
        .skip_while(|l| l.starts_with('#') || l.starts_with("//") || l.starts_with("<!--"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Compare with `tests/golden/<name>`; `DOTS_REGEN=1` rewrites it.
pub fn golden(name: &str, actual: &str) {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden")
        .join(name);
    if std::env::var_os("DOTS_REGEN").is_some() {
        fs::write(&p, actual).unwrap();
        return;
    }
    let want = fs::read_to_string(&p).unwrap_or_default();
    assert_eq!(
        actual, want,
        "golden {name} differs; run with DOTS_REGEN=1 to update"
    );
}
