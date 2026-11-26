//! `dots override list | edit <address> [--machine] | diff <address>`.

use std::path::{Path, PathBuf};
use std::process::Command;

use dots_core::report::Diagnostic;
use dots_core::{Block, Cell, ExitCode, Report, Row, Section, Table, Tone};
use dots_model::{Address, Scope, Sub};
use serde_json::{Value, json};

use crate::app::Context;
use crate::cli::OverrideAction;
use crate::context::{RepoCtx, load_repo, override_locations};
use crate::show::{not_found, parse, scope_name};

fn failure(errors: Vec<Diagnostic>, warnings: Vec<Diagnostic>) -> Report {
    let mut r = Report::failure("override", ExitCode::Error, errors);
    r.warnings = warnings;
    r
}

fn load(ctx: &Context) -> Result<RepoCtx, Box<Report>> {
    load_repo(ctx, None).map_err(|(e, w)| Box::new(failure(e, w)))
}

pub fn run(ctx: &Context, action: &OverrideAction) -> Report {
    let rc = match load(ctx) {
        Ok(rc) => rc,
        Err(r) => return *r,
    };
    let mut report = match action {
        OverrideAction::List => list(ctx, &rc),
        OverrideAction::Diff { address } => diff(&rc, address),
        OverrideAction::Edit { address, machine } => edit(ctx, &rc, address, *machine),
    };
    let mut w = rc.m.warnings.clone();
    w.append(&mut report.warnings);
    report.warnings = w;
    report
}

// ---------------------------------------------------------------------- list

fn list(ctx: &Context, rc: &RepoCtx) -> Report {
    let Some(info) = rc.repo.effective.clone() else {
        return Report::ok("override", json!({ "action": "list", "scopes": [] }));
    };
    let locations = override_locations(&rc.m, ctx);
    let mut scopes = Vec::new();
    let mut total = 0;
    let mut report = Report::ok("override", Value::Null);
    let mut sections = Vec::new();
    for loc in &locations {
        let mine: Vec<_> = info
            .overrides
            .iter()
            .filter(|o| o.scope == loc.scope)
            .collect();
        if mine.is_empty() {
            continue;
        }
        total += mine.len();
        let mut items = Vec::new();
        let mut table = Table::new()
            .styled_column("keys", 4, dots_core::Style::Muted)
            .styled_column("file", 3, dots_core::Style::Muted);
        for o in mine {
            let keys: Vec<String> = o
                .value
                .as_object()
                .map(|m| m.keys().filter(|k| *k != "schema").cloned().collect())
                .unwrap_or_default();
            table.push(
                Row::new(dots_core::Lead::None, o.target.clone())
                    .cell(Cell::muted(keys.join(", ")))
                    .cell(Cell::muted(o.file.clone())),
            );
            items.push(json!({ "target": o.target, "file": o.file, "keys": keys }));
        }
        scopes.push(json!({
            "scope": scope_name(loc.scope),
            "location": loc.display,
            "overrides": items,
        }));
        sections.push(
            Section::new(&format!("{} ({})", scope_title(loc.scope), loc.display))
                .block(Block::Table(table)),
        );
    }
    report.data = json!({ "action": "list", "scopes": scopes });
    if total == 0 {
        return report.with_headline(Tone::Muted, "no overrides");
    }
    report = report.with_headline(
        Tone::Info,
        dots_core::blocks::plural(total, "override", "overrides"),
    );
    for s in sections {
        report.push(Block::Section(s));
    }
    report
}

fn scope_title(s: Scope) -> &'static str {
    match s {
        Scope::Repo => "repo",
        Scope::Profile => "machine profile",
        Scope::Machine => "machine",
        Scope::Definition => "definition",
    }
}

// ---------------------------------------------------------------------- diff

fn diff(rc: &RepoCtx, address: &str) -> Report {
    let addr = match parse("override", address) {
        Ok(a) => a,
        Err(r) => return *r,
    };
    let Some(info) = rc.repo.effective.clone() else {
        return Report::ok(
            "override",
            json!({ "action": "diff", "address": address, "changes": [] }),
        );
    };
    let Some(changes) = info.diff(&addr) else {
        return not_found("override", &addr, &info);
    };
    let show = |v: &Option<Value>| v.as_ref().map(|v| v.to_string()).unwrap_or_default();
    let mut lines = Vec::new();
    for c in &changes {
        let note = c
            .source
            .as_ref()
            .map(|s| format!("   # {s}"))
            .unwrap_or_default();
        match c.kind {
            "added" => lines.push(format!("+ {} = {}{note}", c.path, show(&c.after))),
            "removed" => lines.push(format!("- {} = {}{note}", c.path, show(&c.before))),
            _ => {
                lines.push(format!("- {} = {}", c.path, show(&c.before)));
                lines.push(format!("+ {} = {}{note}", c.path, show(&c.after)));
            }
        }
    }
    let data = json!({
        "action": "diff",
        "address": addr.to_string(),
        "changes": changes.iter().map(|c| json!({
            "path": c.path, "change": c.kind, "before": c.before, "after": c.after, "source": c.source,
        })).collect::<Vec<_>>(),
    });
    let mut report = Report::ok("override", data);
    if changes.is_empty() {
        return report.with_headline(Tone::Muted, format!("{addr}: no overrides change it"));
    }
    report = report.with_headline(
        Tone::Info,
        format!(
            "{addr}: {}",
            dots_core::blocks::plural(changes.len(), "change", "changes")
        ),
    );
    report.push(Block::Code { title: None, lines });
    report
}

// ---------------------------------------------------------------------- edit

/// The override file for an address's object, relative to an override directory.
fn file_for(addr: &Address) -> Option<PathBuf> {
    match addr {
        Address::Pkg { name, .. } => Some(Path::new("packages").join(name).join("override.toml")),
        Address::Bundle { name, .. } => Some(Path::new("bundles").join(format!("{name}.toml"))),
        Address::Item { .. } => None,
        _ => Some(PathBuf::from("dots.toml")),
    }
}

/// What a new override file starts with: `schema = 1` and, for a part of an object, the table
/// that targets it.
fn scaffold(addr: &Address) -> String {
    let sub = match addr {
        Address::Pkg { sub, .. } | Address::Item { sub, .. } | Address::Bundle { sub, .. } => {
            sub.as_ref()
        }
        _ => None,
    };
    let q = |s: &str| toml_edit::Key::new(s).to_string();
    let table = match sub {
        Some(Sub::File(v)) => Some(format!("[files.{}]", q(v))),
        Some(Sub::Setting(v)) => Some(format!("[settings.{}]", q(v))),
        Some(Sub::Provider(v)) => Some(format!("[providers.{}]", q(v))),
        Some(Sub::Manual(v)) => Some(format!("[manual.{}]", q(v))),
        Some(Sub::Hook(v)) => Some(format!("[hooks.{}]", q(v))),
        Some(Sub::Member(v)) => Some(format!("[members.{}]", q(v))),
        None => None,
    };
    match table {
        Some(t) => format!("schema = 1\n\n{t}\n"),
        None => "schema = 1\n".to_string(),
    }
}

fn edit(ctx: &Context, rc: &RepoCtx, address: &str, machine: bool) -> Report {
    let addr = match parse("override", address) {
        Ok(a) => a,
        Err(r) => return *r,
    };
    let Some(info) = rc.repo.effective.clone() else {
        return Report::failure("override", ExitCode::Error, vec![]);
    };
    // The object must exist; a part of it need not (an override can add one).
    if !info.objects.contains_key(&addr.object()) {
        return not_found("override", &addr, &info);
    }
    let Some(rel) = file_for(&addr) else {
        return not_found("override", &addr, &info);
    };
    let (scope, dir) = if machine {
        (Scope::Machine, ctx.dirs.config.join("overrides"))
    } else {
        (Scope::Repo, rc.m.root.join("overrides"))
    };
    let path = dir.join(&rel);
    let created = !path.exists();
    if created {
        let made = path
            .parent()
            .map_or(Ok(()), std::fs::create_dir_all)
            .and_then(|()| std::fs::write(&path, scaffold(&addr)));
        if let Err(e) = made {
            let msg = format!("cannot create {}: {e}", path.display());
            return failure(
                vec![Diagnostic::from_code(
                    "usage-error",
                    &[("detail", &msg), ("help", "dots override --help")],
                )],
                vec![],
            );
        }
    }
    let editor = std::env::var("EDITOR")
        .ok()
        .filter(|e| !e.trim().is_empty())
        .unwrap_or_else(|| "vi".into());
    let status = Command::new("sh")
        .arg("-c")
        .arg(format!("{editor} \"$1\""))
        .arg("dots-editor")
        .arg(&path)
        .status();
    // A scaffold this call made and nothing was added to is not worth keeping.
    let untouched = created && std::fs::read_to_string(&path).is_ok_and(|t| t == scaffold(&addr));
    let ok = status.is_ok_and(|s| s.success());
    if created && (!ok || untouched) {
        let _ = std::fs::remove_file(&path);
    }
    if !ok {
        return failure(
            vec![Diagnostic::from_code(
                "editor-failed",
                &[("editor", &editor)],
            )],
            vec![],
        );
    }
    if untouched {
        let report = Report::ok(
            "override",
            json!({
                "action": "edit",
                "address": addr.to_string(),
                "scope": scope_name(scope),
                "path": path.display().to_string(),
                "created": false,
            }),
        );
        return report.with_headline(Tone::Muted, "no changes; no override file was created");
    }
    let mut data = json!({
        "action": "edit",
        "address": addr.to_string(),
        "scope": scope_name(scope),
        "path": path.display().to_string(),
        "created": created,
    });
    // The file is saved either way; say so when it no longer loads.
    let mut report = Report::ok("override", Value::Null);
    if let Err((errors, _)) = load_repo(ctx, None) {
        data["valid"] = json!(false);
        report.warnings = errors;
    }
    report.data = data;
    let verb = if created {
        "created and edited"
    } else {
        "edited"
    };
    report.with_headline(Tone::Ok, format!("{verb} {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scaffold_quotes_keys_as_valid_toml() {
        for key in ["~/.zshrc", "a\"b", "back\\slash", "plain", "del\u{7f}"] {
            let addr = Address::Pkg {
                name: "p".into(),
                sub: Some(Sub::File(key.into())),
            };
            let doc: toml_edit::DocumentMut = scaffold(&addr).parse().unwrap();
            let files = doc["files"].as_table().unwrap();
            assert!(files.contains_key(key), "{key}: {doc}");
        }
    }
}
