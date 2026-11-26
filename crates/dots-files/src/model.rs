//! Public data types.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use dots_core::report::Diagnostic;
use serde::{Deserialize, Serialize};

use crate::merge::Strategy;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Mode {
    Link,
    Copy,
    Render,
    Inject,
    Dir,
    /// Compile a terminfo source with `tic -x -o <dir>`.
    Terminfo,
}

/// One source layer of a render.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LayerSrc {
    /// Absolute path of the layer file.
    pub path: PathBuf,
    /// Name shown in headers and errors, e.g. `packages/ghostty/config/macos`.
    pub label: String,
}

/// A `[[files]]` entry after platform, path-variable and layer resolution.
#[derive(Debug, Clone)]
pub struct ResolvedFile {
    pub package: String,
    pub mode: Mode,
    /// Absolute destination.
    pub dst: PathBuf,
    /// Source for `link`, `copy` and `inject` (absolute, inside the root).
    pub src: Option<PathBuf>,
    /// Layers for `render`, in merge order.
    pub layers: Vec<LayerSrc>,
    /// `toml`, `json`, `jsonc`, `yaml`, `ini`, `gitconfig`, `markdown` or `text`.
    pub format: String,
    /// `include` renders a stub using the application's native include.
    pub kind: Option<String>,
    /// `jinja` renders each layer through minijinja before parsing.
    pub template: Option<String>,
    pub merge: BTreeMap<String, Strategy>,
    /// Line-comment prefix for text/inject (default `#`).
    pub comment: String,
    pub extras: Extras,
}

/// What a `render` file carries beyond layers: structured patches and fragment composition.
#[derive(Debug, Clone, Default)]
pub struct Extras {
    /// `[[files.set]]`, applied after the merge in declaration order.
    pub set: Vec<SetPatch>,
    /// `Some(priority)` for one package's `mode = "fragment"` entry (it renders like a `render`
    /// entry but is meant to be composed with the others for the same `dst`).
    pub fragment: Option<i64>,
    /// For a composed file: its contributions. Empty otherwise.
    pub parts: Vec<ResolvedFile>,
}

/// One `[[files.set]]` entry.
#[derive(Debug, Clone, PartialEq)]
pub struct SetPatch {
    /// Dotted key path; a segment containing `.` is written `"a.b"`.
    pub path: String,
    pub value: Option<serde_json::Value>,
    pub delete: bool,
    pub when: Option<String>,
}

/// Facts, platform chain and tags for evaluating conditions in directives and patches.
#[derive(Debug, Clone)]
pub struct CondEnv {
    pub facts: dots_facts::Facts,
    pub chain: dots_facts::Chain,
    pub tags: std::collections::BTreeSet<String>,
    pub declared_tags: std::collections::BTreeSet<String>,
}

/// A setting value that went into a render, shown in the generated header.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SettingUse {
    pub name: String,
    pub value: serde_json::Value,
    /// Scope the value came from: `default`, `repo`, `machine`, ...
    pub scope: String,
    /// A secret setting: its value is never printed (the header shows `•••`).
    #[serde(default)]
    pub secret: bool,
}

/// Everything the file engine needs besides the files themselves.
#[derive(Debug, Clone)]
pub struct FilesCtx {
    pub home: PathBuf,
    /// The dots state directory (`<state>/active`, `gen/`, `backup/`, ownership ledger).
    pub state: PathBuf,
    /// The checkout the layers are read from (the target of `<state>/active`).
    pub root: PathBuf,
    pub root_id: String,
    pub run_id: String,
    /// `macos`, `linux` or `windows`.
    pub os: String,
    /// Platform chain, general to specific (as `dots_facts::Chain::levels`).
    pub chain: Vec<String>,
    /// Resolved path variables (`[paths]`), overriding the built-ins.
    pub vars: BTreeMap<String, String>,
    pub settings: Vec<SettingUse>,
    /// Context for `template = "jinja"` layers.
    pub template_vars: serde_json::Value,
    /// Conditions in `dots:if` and `[[files.set]] when`. Without it a minimal environment is
    /// derived from `os`, `chain` and `template_vars.tags`.
    pub cond: Option<std::sync::Arc<CondEnv>>,
    pub dry_run: bool,
    /// `tic` and `infocmp` for the `terminfo` mode (real processes by default).
    pub tools: std::sync::Arc<dyn crate::terminfo::Tools>,
}

impl FilesCtx {
    pub fn new(home: &Path, state: &Path, root: &Path, root_id: &str, run_id: &str) -> Self {
        let os = match std::env::consts::OS {
            "macos" => "macos",
            "windows" => "windows",
            _ => "linux",
        };
        FilesCtx {
            home: home.to_path_buf(),
            state: state.to_path_buf(),
            root: root.to_path_buf(),
            root_id: root_id.to_string(),
            run_id: run_id.to_string(),
            os: os.to_string(),
            chain: Vec::new(),
            vars: BTreeMap::new(),
            settings: Vec::new(),
            template_vars: serde_json::json!({}),
            cond: None,
            dry_run: false,
            tools: crate::terminfo::system_tools(),
        }
    }
    pub fn active_dir(&self) -> PathBuf {
        self.state.join("active")
    }
    pub fn gen_dir(&self) -> PathBuf {
        self.state.join("gen").join(&self.root_id)
    }
}

/// The drift states (the `tampered` state belongs to the integrity wave).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FileState {
    Ok,
    Missing,
    Conflict,
    Modified,
    Stale,
    Dangling,
    ForeignEntries,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Action {
    Unchanged,
    Create,
    Update,
    /// An existing path that dots does not own is backed up first, then replaced.
    Replace,
    Remove,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum BackupKind {
    None,
    /// Move the existing path to `backup/<run-id>/`.
    Move,
    /// Copy the content visible at `dst` (it is about to be overwritten through a link).
    Copy,
}

/// One planned file operation. Serializable for the plan JSON.
#[derive(Debug, Clone, Serialize)]
pub struct FileStep {
    pub package: String,
    pub dst: PathBuf,
    pub mode: Mode,
    pub action: Action,
    /// The observed state this step starts from.
    pub from: Option<FileState>,
    pub backup: BackupKind,
    pub link_target: Option<PathBuf>,
    pub gen_path: Option<PathBuf>,
    pub src: Option<PathBuf>,
    pub sha256: Option<String>,
    pub layers: Vec<String>,
    /// Inject block id.
    pub marker: Option<String>,
    pub comment: Option<String>,
    pub error: Option<Diagnostic>,
    /// Rendered bytes (render), copied bytes (copy) or the block body (inject). Not serialized.
    #[serde(skip)]
    pub content: Option<Vec<u8>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum OutcomeStatus {
    Applied,
    Unchanged,
    DryRun,
    /// Left alone on purpose (e.g. removal of a modified file).
    Skipped,
    Failed,
}

#[derive(Debug, Clone, Serialize)]
pub struct FileOutcome {
    pub dst: PathBuf,
    pub status: OutcomeStatus,
    /// Where the pre-existing path was moved or copied.
    pub backup: Option<PathBuf>,
    pub error: Option<Diagnostic>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Resolution {
    pub id: String,
    pub command: String,
    pub description: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct FileStatus {
    pub package: String,
    pub dst: PathBuf,
    pub mode: Mode,
    pub state: FileState,
    pub hint: Option<String>,
    /// For `modified` and `conflict`: the ways out.
    pub resolutions: Vec<Resolution>,
    pub error: Option<Diagnostic>,
}
