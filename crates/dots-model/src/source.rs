//! Source files, positions and the diagnostic collector shared by every loader.

use std::ops::Range;

use dots_core::{Diagnostic, Provenance};

/// A file read from disk: its display name (repo-relative where possible) and text.
#[derive(Debug, Clone)]
pub struct SourceFile {
    pub display: String,
    pub text: String,
    /// What kind of file this is (`dots.toml`, `bundle:cli`, `package:fd`, `machine`); used as
    /// the provenance `source`.
    pub kind: String,
}

impl SourceFile {
    pub fn new(display: impl Into<String>, kind: impl Into<String>, text: String) -> Self {
        SourceFile {
            display: display.into(),
            text,
            kind: kind.into(),
        }
    }

    /// 1-based line and column (in characters) of a byte offset.
    pub fn pos(&self, offset: usize) -> (u32, u32) {
        let offset = offset.min(self.text.len());
        let before = &self.text[..self.text.floor_char_boundary(offset)];
        let line = before.matches('\n').count() as u32 + 1;
        let col = match before.rfind('\n') {
            Some(i) => before[i + 1..].chars().count(),
            None => before.chars().count(),
        } as u32
            + 1;
        (line, col)
    }
}

/// Errors and warnings collected while loading. Loading never stops at the first error.
#[derive(Debug, Clone, Default)]
pub struct Diags {
    pub errors: Vec<Diagnostic>,
    pub warnings: Vec<Diagnostic>,
}

impl Diags {
    pub fn extend(&mut self, other: Diags) {
        self.errors.extend(other.errors);
        self.warnings.extend(other.warnings);
    }

    /// Build a diagnostic for `code` located at `span` of `src` and record it as an error
    /// or (for `warning`) a warning. `{file}` and `{line}` are always available as arguments.
    pub fn report(
        &mut self,
        warning: bool,
        code: &str,
        args: &[(&str, &str)],
        src: &SourceFile,
        span: Option<Range<usize>>,
        hint: Option<String>,
    ) {
        let pos = span.map(|r| src.pos(r.start));
        let line = pos.map(|p| p.0.to_string()).unwrap_or_else(|| "1".into());
        let mut all: Vec<(&str, &str)> = vec![("file", &src.display), ("line", &line)];
        all.extend_from_slice(args);
        let mut d = Diagnostic::from_code(code, &all);
        if let Some(h) = hint {
            d.hint = h;
        }
        d = d.with_provenance(Provenance {
            source: src.kind.clone(),
            file: Some(src.display.clone()),
            line: pos.map(|p| p.0),
            col: pos.map(|p| p.1),
        });
        if warning {
            self.warnings.push(d);
        } else {
            self.errors.push(d);
        }
    }

    pub fn error(
        &mut self,
        code: &str,
        args: &[(&str, &str)],
        src: &SourceFile,
        span: Option<Range<usize>>,
    ) {
        self.report(false, code, args, src, span, None);
    }

    /// A file-level problem with no useful position (unreadable file, missing key).
    pub fn file_error(&mut self, code: &str, args: &[(&str, &str)], src: &SourceFile) {
        self.report(false, code, args, src, None, None);
    }
}

/// The closest candidate within a small edit distance, if any.
pub fn suggest<'a>(word: &str, candidates: impl IntoIterator<Item = &'a str>) -> Option<String> {
    let max = (word.chars().count() / 3).max(2);
    candidates
        .into_iter()
        .map(|c| (strsim::levenshtein(word, c), c))
        .filter(|(d, _)| *d > 0 && *d <= max)
        .min_by_key(|(d, _)| *d)
        .map(|(_, c)| c.to_string())
}
