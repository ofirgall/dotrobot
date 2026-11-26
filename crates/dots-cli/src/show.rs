//! `dots show <address> [--effective]`: print an object, with the layer that set each key.

use dots_core::report::Diagnostic;
use dots_core::{Block, ExitCode, Report, Tone};
use dots_model::{Address, EffectiveInfo, Scope};
use serde_json::{Value, json};

use crate::app::Context;
use crate::context::load_repo;

fn fail(d: Diagnostic) -> Report {
    Report::failure("show", ExitCode::Error, vec![d])
}

/// Parse an address or build the `address-invalid` error.
pub fn parse(command: &str, text: &str) -> Result<Address, Box<Report>> {
    Address::parse(text).map_err(|detail| {
        let d = Diagnostic::from_code("address-invalid", &[("address", text), ("detail", &detail)]);
        Box::new(Report::failure(command, ExitCode::Error, vec![d]))
    })
}

/// The `address-not-found` error, with the closest object as a suggestion.
pub fn not_found(command: &str, addr: &Address, info: &EffectiveInfo) -> Report {
    let text = addr.to_string();
    let sug = info
        .suggest_object(&addr.object())
        .map(|s| format!(" Did you mean `{s}`?"))
        .unwrap_or_default();
    Report::failure(
        command,
        ExitCode::Error,
        vec![Diagnostic::from_code(
            "address-not-found",
            &[("address", &text), ("suggestion", &sug)],
        )],
    )
}

pub fn scope_name(s: Scope) -> &'static str {
    s.short()
}

pub fn run(ctx: &Context, address: &str, effective: bool) -> Report {
    let addr = match parse("show", address) {
        Ok(a) => a,
        Err(r) => return *r,
    };
    let rc = match load_repo(ctx, None) {
        Ok(rc) => rc,
        Err((errors, warnings)) => {
            let mut r = Report::failure("show", ExitCode::Error, errors);
            r.warnings = warnings;
            return r;
        }
    };
    let Some(info) = rc.repo.effective.clone() else {
        return fail(Diagnostic::from_code(
            "address-not-found",
            &[("address", address), ("suggestion", "")],
        ));
    };
    let Some(rendered) = info.render(&addr, effective) else {
        return not_found("show", &addr, &info);
    };
    let object = addr.object();
    let provenance: Vec<Value> = rendered
        .lines
        .iter()
        .filter_map(|l| l.origin.as_ref())
        .map(|o| {
            json!({
                "path": o.path,
                "scope": scope_name(o.scope),
                "source": o.source,
                "file": o.file,
                "line": o.line,
            })
        })
        .collect();
    let overrides: Vec<String> = info.overrides_of(&object).map(|o| o.file.clone()).collect();
    let text: Vec<String> = rendered
        .lines
        .iter()
        .map(|l| match &l.origin {
            Some(o) => format!("{:<28} # {}", l.text, o.note()),
            None => l.text.clone(),
        })
        .collect();
    let mut report = Report::ok(
        "show",
        json!({
            "address": addr.to_string(),
            "effective": effective,
            "value": rendered.value,
            "text": text,
            "provenance": provenance,
            "overrides": overrides,
        }),
    );
    report.warnings = rc.m.warnings;
    let word = if effective { "effective" } else { "definition" };
    report = report.with_headline(Tone::Info, format!("{addr} ({word})"));
    report.push(Block::Code {
        title: None,
        lines: text,
    });
    report
}
