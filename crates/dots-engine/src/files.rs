//! The seam to the files phase. The engine calls it after the package phase; the real
//! implementation (links, renders, merges, restore) lives in `dots-files` and is wired in by
//! the CLI. [`NoFiles`] is the default and plans nothing.

use std::any::Any;
use std::path::Path;
use std::sync::Arc;

/// An opaque value a [`FilesPhase`] attaches to a step at plan time and gets back at apply
/// time (the real phase keeps its full, non-serializable step here). The engine never looks
/// inside, and two payloads always compare equal so plans stay comparable by their summary.
#[derive(Clone, Default)]
pub struct Payload(pub Option<Arc<dyn Any + Send + Sync>>);

impl Payload {
    pub fn new<T: Any + Send + Sync>(v: T) -> Self {
        Payload(Some(Arc::new(v)))
    }

    pub fn get<T: Any + Send + Sync>(&self) -> Option<&T> {
        self.0.as_deref().and_then(|a| a.downcast_ref::<T>())
    }
}

impl std::fmt::Debug for Payload {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(if self.0.is_some() {
            "Payload(..)"
        } else {
            "Payload(none)"
        })
    }
}

impl PartialEq for Payload {
    fn eq(&self, _: &Payload) -> bool {
        true
    }
}
impl Eq for Payload {}

/// One planned file action: summary fields the engine, journal and views use, plus the
/// phase's own [`Payload`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FileStep {
    pub package: String,
    /// Destination as the user sees it (`~/.config/x/y.toml`).
    pub dst: String,
    /// `link`, `unlink`, `render`, `inject`, `backup`, ... (the journal `op`).
    pub kind: String,
    /// One line for the plan (`-> packages/x/y.toml`, `3 keys merged`).
    pub detail: String,
    /// dots already owns `dst` and only refreshes it. A plan made only of such steps needs no
    /// confirmation.
    pub owned: bool,
    /// `create`, `update`, `replace`, `remove` or `failed` (empty when the phase has no such
    /// notion). A step that changes nothing is never planned.
    pub action: String,
    /// The step repairs a broken destination (a link to a missing target) that package steps
    /// would trip over, so `apply` runs it before the package phase instead of after it.
    pub early: bool,
    pub payload: Payload,
}

/// What the files phase is asked to plan or apply.
#[derive(Debug, Clone, Copy)]
pub struct FilesRequest<'a> {
    pub run_id: &'a str,
    /// The packages whose files are in scope. For `apply` this is already reduced to the
    /// packages whose install did not fail, so config never lands before (or without) its
    /// software.
    pub packages: &'a [String],
    pub home: &'a Path,
    pub state_dir: &'a Path,
    pub dry_run: bool,
}

/// The outcome of one applied step, reported as soon as it is done so state and journal stay
/// accurate if the run dies half way.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileStepResult {
    pub step: FileStep,
    pub ok: bool,
    /// The failure, or a note.
    pub detail: Option<String>,
    pub dur_ms: u64,
}

pub trait FilesPhase: Send + Sync {
    /// The file actions needed for `req.packages`, in apply order. Pure: no side effects.
    fn plan(&self, req: &FilesRequest<'_>) -> Result<Vec<FileStep>, String>;

    /// Apply `steps` in order, calling `done` after each. Must not run anything when
    /// `req.dry_run`. An `Err` aborts the phase; steps already reported stay reported.
    fn apply(
        &self,
        req: &FilesRequest<'_>,
        steps: &[FileStep],
        done: &mut dyn FnMut(FileStepResult),
    ) -> Result<(), String>;
}

/// No file actions at all.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoFiles;

impl FilesPhase for NoFiles {
    fn plan(&self, _req: &FilesRequest<'_>) -> Result<Vec<FileStep>, String> {
        Ok(Vec::new())
    }

    fn apply(
        &self,
        _req: &FilesRequest<'_>,
        _steps: &[FileStep],
        _done: &mut dyn FnMut(FileStepResult),
    ) -> Result<(), String> {
        Ok(())
    }
}
