//! `dots config list | get | set | unset | explain`: package settings (ARCHITECTURE §17).
//!
//! Values come from `dots-config`; this file is the command surface and the glue other
//! phases share ([`settings_for`]). Secrets are redacted everywhere here: JSON carries
//! `value: null` and `redacted: true`, human output prints `•••`.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;

use dots_config::{Inputs, REDACTED, Setting, SettingScope, Settings, packages_of, split_id};
use dots_core::report::Diagnostic;
use dots_core::{Block, Cell, ExitCode, Lead, Report, Row, State, Table, Tone};
use dots_facts::{Condition, EvalCtx, NoSettings, suggest};
use serde_json::{Value, json};

use crate::app::Context;
use crate::cli::ConfigAction;
use crate::context::{RepoCtx, load_repo};

/// Resolve every package setting for this machine. Cheap; callers may repeat it.
pub fn settings_for(rc: &RepoCtx) -> Settings {
    let tags: BTreeSet<String> = rc.m.machine.tags.iter().cloned().collect();
    let declared: BTreeSet<String> = rc.repo.manifest.tags.keys().cloned().collect();
    let when = |src: &str| -> Result<bool, String> {
        Condition::parse(src)
            .and_then(|c| {
                c.eval(&EvalCtx {
                    facts: &rc.m.facts,
                    chain: &rc.chain,
                    tags: &tags,
                    declared_tags: &declared,
                    settings: &NoSettings,
                })
            })
            .map_err(|e| e.message)
    };
    dots_config::resolve(&Inputs::new(&rc.repo, &rc.m.machine, &when))
}

/// How a value reads on a terminal: strings bare, lists comma-joined.
pub fn plain(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Array(a) => a.iter().map(plain).collect::<Vec<_>>().join(", "),
        other => other.to_string(),
    }
}

fn failure(errors: Vec<Diagnostic>, warnings: Vec<Diagnostic>) -> Report {
    let mut r = Report::failure("config", ExitCode::Error, errors);
    r.warnings = warnings;
    r
}

fn fail1(code: &str, args: &[(&str, &str)]) -> Report {
    failure(vec![Diagnostic::from_code(code, args)], vec![])
}

/// The JSON of one setting. A secret has `value: null` and `redacted: true`.
pub fn setting_json(s: &Setting, active: Option<bool>) -> Value {
    let hide = s.secret();
    let shown = |v: Option<&Value>| match v {
        Some(_) if hide => Value::Null,
        Some(v) => v.clone(),
        None => Value::Null,
    };
    let mut o = json!({
        "id": s.id(),
        "package": s.package,
        "key": s.key,
        "type": s.decl.type_name(),
        "description": s.decl.description,
        "value": shown(s.value()),
        "scope": s.scope().map(SettingScope::as_str),
        "default": shown(s.default()),
        "required": s.decl.required,
        "ask": s.decl.ask,
        "secret": hide,
        "redacted": hide && s.value().is_some(),
        "values": s.decl.values,
        "applies": s.applies,
        "missing": s.missing(),
    });
    if let Some(a) = active {
        o["active"] = json!(a);
    }
    o
}

/// The repo with its settings, or the failure to report.
struct Loaded {
    rc: RepoCtx,
    settings: Settings,
}

fn load(ctx: &Context) -> Result<Loaded, Box<Report>> {
    let rc = load_repo(ctx, None).map_err(|(e, w)| Box::new(failure(e, w)))?;
    let settings = settings_for(&rc);
    Ok(Loaded { rc, settings })
}

pub fn run(ctx: &Context, action: &ConfigAction) -> Report {
    let l = match load(ctx) {
        Ok(l) => l,
        Err(r) => return *r,
    };
    let mut report = match action {
        ConfigAction::List => list(&l),
        ConfigAction::Get { key } => get(&l, key),
        ConfigAction::Explain { key } => explain(&l, key),
        ConfigAction::Set {
            key,
            value,
            repo,
            commit,
            ..
        } => set(ctx, &l, key, value, *repo, *commit),
        ConfigAction::Unset { key } => unset(ctx, &l, key),
    };
    // Invalid values elsewhere do not stop a read; they ride along as warnings.
    let mut extra: Vec<Diagnostic> = l.rc.m.warnings.clone();
    extra.extend(l.settings.errors.iter().cloned());
    extra.extend(l.settings.warnings.iter().cloned());
    extra.extend(std::mem::take(&mut report.warnings));
    report.warnings = extra;
    report
}

/// Find `<pkg>.<key>` or explain why not.
fn find<'a>(l: &'a Loaded, id: &str) -> Result<&'a Setting, Box<Report>> {
    let Some((p, k)) = split_id(id) else {
        return Err(Box::new(fail1("setting-key-invalid", &[("key", id)])));
    };
    l.settings.get(p, k).ok_or_else(|| {
        let ids: Vec<String> = l.settings.iter().map(Setting::id).collect();
        let sug = suggest(id, ids.iter().map(String::as_str))
            .map(|s| format!(" Did you mean `{s}`?"))
            .unwrap_or_default();
        Box::new(fail1(
            "setting-unknown",
            &[("setting", id), ("suggestion", &sug)],
        ))
    })
}

// ---------------------------------------------------------------------- list

fn list(l: &Loaded) -> Report {
    let active = packages_of(&l.rc.repo, &l.rc.m.machine.bundles);
    let shown: Vec<&Setting> = l.settings.iter().filter(|s| s.applies).collect();
    let items: Vec<Value> = shown
        .iter()
        .map(|s| setting_json(s, Some(active.contains(&s.package))))
        .collect();
    let mut report = Report::ok("config", json!({ "action": "list", "settings": items }));
    if shown.is_empty() {
        return report.with_headline(Tone::Muted, "no package declares settings");
    }
    let missing = shown.iter().filter(|s| s.missing()).count();
    let tone = if missing > 0 { Tone::Warn } else { Tone::Ok };
    let mut text = dots_core::blocks::plural(shown.len(), "setting", "settings");
    if missing > 0 {
        text.push_str(&format!(", {missing} required and unset"));
    }
    report = report.with_headline(tone, text);
    let mut t = Table::new()
        .styled_column("value", 5, dots_core::Style::Plain)
        .styled_column("scope", 4, dots_core::Style::Muted)
        .styled_column("default", 3, dots_core::Style::Muted);
    for s in shown {
        let value = match s.shown() {
            Some(v) => Cell::new(plain(&v), dots_core::Style::Plain),
            None => Cell::tone("(unset)", Tone::Warn),
        };
        let default = match s.default() {
            Some(_) if s.value() == s.default() => String::new(),
            Some(d) => format!("(default {})", plain(&s.redact(d))),
            None => "(required)".into(),
        };
        let lead: Lead = if s.missing() {
            State::Missing.into()
        } else {
            Lead::None
        };
        let mut row = Row::new(lead, s.id())
            .cell(value)
            .cell(Cell::muted(s.scope().map_or("-", SettingScope::as_str)))
            .cell(Cell::muted(default));
        if s.missing() {
            row = row.hint(format!("dots config set {} <value>", s.id()));
        }
        t.push(row);
    }
    report.push(Block::Table(t));
    report
}

// ----------------------------------------------------------------------- get

fn get(l: &Loaded, id: &str) -> Report {
    let s = match find(l, id) {
        Ok(s) => s,
        Err(r) => return *r,
    };
    if !s.applies {
        return fail1("setting-inapplicable", &[("setting", id)]);
    }
    if s.missing() {
        return fail1("setting-required", &[("setting", id)]);
    }
    let mut data = setting_json(s, None);
    data["action"] = json!("get");
    let text = s.shown().map(|v| plain(&v)).unwrap_or_default();
    let mut r = Report::ok("config", data);
    r.headline = Some(text);
    r
}

// ------------------------------------------------------------------- explain

fn explain(l: &Loaded, id: &str) -> Report {
    let s = match find(l, id) {
        Ok(s) => s,
        Err(r) => return *r,
    };
    // One entry per scope; a scope with no value (or several bundles) lists what it has.
    let mut scopes: Vec<Value> = Vec::new();
    let mut lines: Vec<String> = Vec::new();
    let winner = s.winning_layer();
    for scope in SettingScope::ALL {
        let layers: Vec<&dots_config::Layer> =
            s.layers.iter().filter(|l| l.scope == scope).collect();
        if layers.is_empty() {
            scopes.push(json!({
                "scope": scope.as_str(), "set": false, "value": null, "source": null,
                "line": null, "winner": false, "valid": null, "error": null,
            }));
            lines.push(format!("  {:<9} -", scope.as_str()));
            continue;
        }
        for layer in layers {
            let wins = winner.is_some_and(|w| std::ptr::eq(w, layer));
            let hidden = s.secret();
            scopes.push(json!({
                "scope": scope.as_str(),
                "set": true,
                "value": if hidden { Value::Null } else { layer.value.clone() },
                "source": layer.file,
                "line": layer.line,
                "winner": wins,
                "valid": layer.valid(),
                "error": layer.error,
            }));
            let shown = plain(&s.redact(&layer.value));
            let at = match layer.line {
                Some(n) => format!("{}:{n}", layer.file),
                None => layer.file.clone(),
            };
            let mut line = format!("  {:<9} {shown:<14} {at}", scope.as_str());
            if wins {
                line.push_str("   (wins)");
            }
            if let Some(e) = &layer.error {
                line.push_str(&format!("   (ignored: {e})"));
            }
            lines.push(line);
        }
    }
    let hide = s.secret();
    let data = json!({
        "action": "explain",
        "id": s.id(),
        "type": s.decl.type_name(),
        "description": s.decl.description,
        "required": s.decl.required,
        "secret": hide,
        "redacted": hide && s.value().is_some(),
        "applies": s.applies,
        "value": if hide { Value::Null } else { s.value().cloned().unwrap_or(Value::Null) },
        "winner": s.scope().map(SettingScope::as_str),
        "scopes": scopes,
    });
    let head = match (s.shown(), s.scope()) {
        (Some(v), Some(sc)) => format!("{} = {} ({})", s.id(), plain(&v), sc.as_str()),
        _ => format!("{} has no value", s.id()),
    };
    let tone = if s.missing() { Tone::Warn } else { Tone::Ok };
    let mut r = Report::ok("config", data).with_headline(tone, head);
    if !s.applies {
        r.push(Block::text(
            "does not exist on this machine: its `when` condition is false",
        ));
    }
    r.push(Block::Code {
        title: Some(format!("{}, {}", s.id(), s.decl.expected())),
        lines,
    });
    if s.missing() {
        r.push(Block::hint(format!("dots config set {} <value>", s.id())));
    }
    r
}

// ----------------------------------------------------------------- set / unset

fn machine_path(ctx: &Context) -> PathBuf {
    ctx.dirs.machine_toml(ctx.opts.machine_config.as_deref())
}

#[cfg(unix)]
pub fn private(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
}
#[cfg(not(unix))]
pub fn private(_: &Path) {}

fn write_failed(path: &Path, detail: &str) -> Report {
    fail1(
        "write-failed",
        &[("path", &path.display().to_string()), ("detail", detail)],
    )
}

fn git_commit(root: &Path, id: &str, what: &str) -> Result<(), String> {
    let run = |args: &[&str]| {
        let out = Command::new("git")
            .arg("-C")
            .arg(root)
            .args(args)
            .output()
            .map_err(|e| format!("cannot run git: {e}"))?;
        if out.status.success() {
            Ok(())
        } else {
            let t = String::from_utf8_lossy(&out.stderr);
            let t2 = String::from_utf8_lossy(&out.stdout);
            Err(t
                .lines()
                .chain(t2.lines())
                .find(|l| !l.trim().is_empty())
                .unwrap_or("git failed")
                .trim()
                .to_string())
        }
    };
    run(&["add", "--", "dots.toml"])?;
    run(&[
        "commit",
        "--quiet",
        "-m",
        &format!("config: set {id} = {what}"),
        "--",
        "dots.toml",
    ])
}

fn set(ctx: &Context, l: &Loaded, id: &str, text: &str, repo: bool, commit: bool) -> Report {
    let s = match find(l, id) {
        Ok(s) => s,
        Err(r) => return *r,
    };
    if !s.applies {
        return fail1("setting-inapplicable", &[("setting", id)]);
    }
    let (scope, path, file) = if repo {
        (
            SettingScope::Repo,
            l.rc.m.root.join("dots.toml"),
            "dots.toml".to_string(),
        )
    } else {
        let p = machine_path(ctx);
        (SettingScope::Machine, p, "machine.toml".to_string())
    };
    if s.secret() && scope != SettingScope::Machine {
        return fail1(
            "setting-secret-scope",
            &[("setting", id), ("origin", &file)],
        );
    }
    let value = match s.decl.parse_text(text) {
        Ok(v) => v,
        Err(detail) => {
            // `parse_text` never echoes a secret's text, so `detail` is safe as it is.
            return fail1(
                "setting-invalid",
                &[
                    ("setting", id),
                    ("origin", "the command line"),
                    ("detail", &detail),
                ],
            );
        }
    };
    if let Err(e) = dots_config::set_in_file(&path, &s.package, &s.key, &value, s.secret()) {
        return write_failed(&path, &e);
    }
    if s.secret() {
        private(&path);
    }
    let mut warnings = Vec::new();
    let mut committed = false;
    if commit {
        match git_commit(&l.rc.m.root, id, &plain(&value)) {
            Ok(()) => committed = true,
            Err(e) => return write_failed(&l.rc.m.root.join("dots.toml"), &format!("git: {e}")),
        }
    }
    // What now wins: a higher scope may still shadow what was just written.
    let after = load_repo(ctx, None)
        .ok()
        .map(|rc| {
            let st = settings_for(&rc);
            st.get(&s.package, &s.key)
                .map(|n| (n.value().cloned(), n.scope()))
        })
        .unwrap_or_default()
        .unwrap_or((None, None));
    let hide = s.secret();
    let shown = if hide { json!(REDACTED) } else { value.clone() };
    let data = json!({
        "action": "set",
        "id": id,
        "scope": scope.as_str(),
        "file": path.display().to_string(),
        "value": if hide { Value::Null } else { value.clone() },
        "redacted": hide,
        "committed": committed,
        "effective_value": if hide { Value::Null } else { after.0.clone().unwrap_or(Value::Null) },
        "effective_scope": after.1.map(SettingScope::as_str),
    });
    let mut r = Report::ok("config", data).with_headline(
        Tone::Ok,
        format!("{id} = {} ({})", plain(&shown), scope.as_str()),
    );
    r.push(Block::facts([("file", path.display().to_string())]));
    if let Some(sc) = after.1
        && sc > scope
    {
        warnings.push(Diagnostic::from_code(
            "setting-invalid",
            &[
                ("setting", id),
                ("origin", &format!("{} scope", sc.as_str())),
                (
                    "detail",
                    &format!(
                        "it still wins over the {} value you just set",
                        scope.as_str()
                    ),
                ),
            ],
        ));
        r.push(Block::hint(format!("dots config explain {id}")));
    } else {
        r.push(Block::hint("dots plan"));
    }
    r.warnings = warnings;
    r
}

fn unset(ctx: &Context, l: &Loaded, id: &str) -> Report {
    let Some((pkg, key)) = split_id(id) else {
        return fail1("setting-key-invalid", &[("key", id)]);
    };
    let path = machine_path(ctx);
    let removed = match dots_config::unset_in_file(&path, pkg, key) {
        Ok(r) => r,
        Err(e) => return write_failed(&path, &e),
    };
    // A value that was never declared is cleared without complaint; one that does not
    // exist anywhere is probably a typo.
    let declared = l.settings.get(pkg, key);
    if declared.is_none()
        && !removed
        && let Err(r) = find(l, id)
    {
        return *r;
    }
    let after = load_repo(ctx, None).ok().and_then(|rc| {
        let st = settings_for(&rc);
        st.get(pkg, key).map(|n| (n.shown(), n.scope()))
    });
    let (now, from) = after.unwrap_or((None, None));
    let data = json!({
        "action": "unset",
        "id": id,
        "scope": "machine",
        "removed": removed,
        "value": declared.filter(|s| !s.secret()).and(now.clone()).unwrap_or(Value::Null),
        "redacted": declared.is_some_and(Setting::secret) && now.is_some(),
        "effective_scope": from.map(SettingScope::as_str),
    });
    let headline = match (removed, &now, from) {
        (true, Some(v), Some(sc)) => {
            format!(
                "{id} unset in machine.toml; now {} ({})",
                plain(v),
                sc.as_str()
            )
        }
        (true, _, _) => format!("{id} unset in machine.toml"),
        (false, ..) => format!("{id} is not set in machine.toml"),
    };
    let mut r = Report::ok("config", data)
        .with_headline(if removed { Tone::Ok } else { Tone::Muted }, headline);
    if !removed && from.is_some_and(|s| s != SettingScope::Default) {
        r.push(Block::text(
            "its value comes from a committed scope; edit dots.toml or run `dots config set --repo`",
        ));
        r.push(Block::hint(format!("dots config explain {id}")));
    }
    r
}
