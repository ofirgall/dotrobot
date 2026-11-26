//! The report model and JSON envelope (design D1, D2). Commands return a `Report`;
//! renderers turn it into rich, plain or JSON output.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::blocks::{Block, Tone};
use crate::errors;
use crate::exit::ExitCode;

/// Current version of every command's output schema. Bumped per command on breaking changes.
pub const SCHEMA_VERSION: u32 = 1;

/// Where a problem comes from (a file position, a package, a source, ...).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Provenance {
    pub source: String,
    pub file: Option<String>,
    pub line: Option<u32>,
    pub col: Option<u32>,
}

/// An error or warning. `errors[]` entries in the envelope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Diagnostic {
    pub code: String,
    pub message: String,
    pub hint: String,
    pub provenance: Vec<Provenance>,
}

impl Diagnostic {
    /// Build from the registry, filling `{placeholders}` in the templates.
    pub fn from_code(code: &str, args: &[(&str, &str)]) -> Self {
        let (message, hint) = match errors::lookup(code) {
            Some(def) => (
                errors::fill(def.message, args),
                errors::fill(def.hint, args),
            ),
            None => (format!("unregistered error code `{code}`"), String::new()),
        };
        Diagnostic {
            code: code.to_string(),
            message,
            hint,
            provenance: Vec::new(),
        }
    }

    pub fn with_provenance(mut self, p: Provenance) -> Self {
        self.provenance.push(p);
        self
    }
}

/// The typed result of one command.
#[derive(Debug, Clone)]
pub struct Report {
    /// Top-level command name, e.g. `status`. Forms the `schema` field.
    pub command: String,
    pub data: Value,
    /// First line of human output: the answer to the command's question.
    pub headline: Option<String>,
    /// Presentation blocks for the human renderers. Never part of the JSON envelope.
    pub blocks: Vec<Block>,
    pub warnings: Vec<Diagnostic>,
    pub errors: Vec<Diagnostic>,
    pub exit: ExitCode,
}

impl Report {
    pub fn ok(command: &str, data: Value) -> Self {
        Report {
            command: command.to_string(),
            data,
            headline: None,
            blocks: Vec::new(),
            warnings: Vec::new(),
            errors: Vec::new(),
            exit: ExitCode::Success,
        }
    }

    pub fn failure(command: &str, exit: ExitCode, errors: Vec<Diagnostic>) -> Self {
        Report {
            command: command.to_string(),
            data: json!({}),
            headline: None,
            blocks: Vec::new(),
            warnings: Vec::new(),
            errors,
            exit,
        }
    }

    /// The stub every unimplemented command returns.
    pub fn not_implemented(command: &str) -> Self {
        Self::failure(
            command,
            ExitCode::Error,
            vec![Diagnostic::from_code(
                "not-implemented",
                &[("command", command)],
            )],
        )
    }

    /// Set the headline with a tone: it renders as `symbol + sentence` in the tone's color.
    /// (`headline` keeps the plain text, which is what a renderer shows when there are no blocks.)
    pub fn with_headline(mut self, tone: Tone, text: impl Into<String>) -> Self {
        let text = text.into();
        self.headline = Some(text.clone());
        self.blocks.insert(0, Block::Headline { tone, text });
        self
    }

    /// Append a presentation block.
    pub fn push(&mut self, block: impl Into<Block>) -> &mut Self {
        self.blocks.push(block.into());
        self
    }

    pub fn with_block(mut self, block: impl Into<Block>) -> Self {
        self.blocks.push(block.into());
        self
    }

    pub fn is_ok(&self) -> bool {
        self.errors.is_empty() && matches!(self.exit, ExitCode::Success)
    }

    pub fn schema_id(&self) -> String {
        schema_id(&self.command)
    }

    /// The JSON envelope. Field order is part of the contract.
    pub fn envelope(&self) -> Value {
        let mut m = Map::new();
        m.insert("schema".into(), Value::String(self.schema_id()));
        m.insert("ok".into(), Value::Bool(self.is_ok()));
        m.insert("data".into(), self.data.clone());
        m.insert(
            "warnings".into(),
            serde_json::to_value(&self.warnings).unwrap_or_default(),
        );
        m.insert(
            "errors".into(),
            serde_json::to_value(&self.errors).unwrap_or_default(),
        );
        Value::Object(m)
    }
}

pub fn schema_id(command: &str) -> String {
    format!("dots/{command}@{SCHEMA_VERSION}")
}
