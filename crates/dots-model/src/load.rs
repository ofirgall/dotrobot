//! Parse, strictly validate and deserialize one schema file.

use std::ops::Range;
use std::path::Path;

use serde::de::DeserializeOwned;
use toml_edit::{Document, Item};

use crate::defs;
use crate::schema::{Env, State, Ty, validate};
use crate::source::{Diags, SourceFile};

/// The only schema number this dots understands.
pub const SUPPORTED_SCHEMA: i64 = 1;

/// Read a file into a [`SourceFile`], recording `file-unreadable` on failure.
pub(crate) fn read_source(
    path: &Path,
    display: String,
    kind: String,
    diags: &mut Diags,
) -> Option<SourceFile> {
    match std::fs::read_to_string(path) {
        Ok(text) => Some(SourceFile::new(display, kind, text)),
        Err(e) => {
            let stub = SourceFile::new(display, kind, String::new());
            diags.file_error("file-unreadable", &[("detail", &e.to_string())], &stub);
            None
        }
    }
}

/// Byte span of a top-level key (or its value when keyless), for later checks.
pub(crate) fn top_key_span(src: &SourceFile, key: &str) -> Option<Range<usize>> {
    let doc = Document::parse(src.text.as_str()).ok()?;
    let (k, v) = doc.as_table().get_key_value(key)?;
    k.span().or_else(|| v.span())
}

pub(crate) struct Loaded<T> {
    pub value: T,
    /// `//`-prefixed paths seen while validating.
    pub escape_hatch: Vec<String>,
    /// Key path to `(line, col)`.
    pub spans: crate::spans::Spans,
}

/// Parse `src`, require a supported `schema = 1`, validate against `ty`, then deserialize.
/// Every problem is appended to `diags`; `None` means the file is unusable.
pub(crate) fn load_typed<T: DeserializeOwned>(
    src: &SourceFile,
    ty: &Ty,
    check_paths: bool,
    diags: &mut Diags,
) -> Option<Loaded<T>> {
    let doc = match Document::parse(src.text.as_str()) {
        Ok(d) => d,
        Err(e) => {
            diags.report(
                false,
                "schema-syntax",
                &[("detail", e.message())],
                src,
                e.span(),
                None,
            );
            return None;
        }
    };
    let root: &Item = doc.as_item();

    match root.as_table().and_then(|t| t.get("schema")) {
        None => {
            diags.file_error("schema-missing-key", &[("key", "schema")], src);
            return None;
        }
        Some(item) => match item.as_integer() {
            None => {
                diags.error(
                    "schema-wrong-type",
                    &[
                        ("key", "schema"),
                        ("expected", "an integer"),
                        ("found", "something else"),
                    ],
                    src,
                    item.span(),
                );
                return None;
            }
            Some(n) if n > SUPPORTED_SCHEMA => {
                diags.report(
                    false,
                    "schema-unsupported",
                    &[("found", &n.to_string())],
                    src,
                    item.span(),
                    Some(format!(
                        "This file needs a newer dots that supports schema {n}; this dots supports \
                         schema {SUPPORTED_SCHEMA}. Upgrade dots."
                    )),
                );
                return None;
            }
            Some(n) if n < SUPPORTED_SCHEMA => {
                diags.report(
                    false,
                    "schema-invalid-value",
                    &[
                        ("key", "schema"),
                        ("detail", &format!("`{n}` is not a schema number")),
                    ],
                    src,
                    item.span(),
                    Some(format!("Use `schema = {SUPPORTED_SCHEMA}`.")),
                );
                return None;
            }
            Some(_) => {}
        },
    }

    let env = Env {
        src,
        check_paths,
        provider_ty: defs::provider_ty,
    };
    let mut state = State::default();
    validate(&env, &mut state, ty, root);
    let failed = !state.diags.errors.is_empty();
    diags.extend(state.diags);
    if failed {
        return None;
    }

    match toml_edit::de::from_str::<T>(&src.text) {
        Ok(value) => Some(Loaded {
            value,
            escape_hatch: state.escape_hatch,
            spans: crate::spans::collect(src, root),
        }),
        Err(e) => {
            diags.report(
                false,
                "schema-invalid-value",
                &[("key", ""), ("detail", e.message())],
                src,
                e.span(),
                Some("Fix the value so it matches the schema.".into()),
            );
            None
        }
    }
}
