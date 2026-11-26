//! `dots questions`: what onboarding still has to ask, each with its recommended answer.
//!
//! The list covers recommended bundles and tags that are not selected, and the `ask` and
//! `required` settings nobody has answered. Settings are listed for the packages of the
//! bundles that *would* be selected if every recommendation were accepted, so answering all
//! questions with their recommended values leaves only the `required` settings without a
//! default (the `accept defaults` scenario). The JSON is stable: agents read it to onboard.

use std::collections::BTreeSet;

use dots_config::{Inputs, Setting, Why, packages_of};
use dots_core::{Block, ExitCode, Report, Tone};
use dots_facts::Chain;
use serde_json::{Value, json};

use crate::app::Context;
use crate::config_cmd::plain;
use crate::context::{load_repo, platform_parents};

pub fn run(ctx: &Context) -> Report {
    let rc = match load_repo(ctx, None) {
        Ok(rc) => rc,
        Err((errors, warnings)) => {
            let mut r = Report::failure("questions", ExitCode::Error, errors);
            r.warnings = warnings;
            return r;
        }
    };
    let machine = &rc.m.machine;
    let chain = Chain::build(
        &rc.m.facts,
        &platform_parents(&rc.repo),
        &machine.platform_extra,
    );
    let rec = crate::init::recommendations(&rc.repo, &chain);

    let mut questions: Vec<Value> = Vec::new();
    let mut rows: Vec<(bool, String, String, String)> = Vec::new();

    for b in rec.bundles.iter().filter(|b| !machine.bundles.contains(b)) {
        let description = rc.repo.bundles.get(b).and_then(|x| x.description.clone());
        let q = format!("Install the `{b}` bundle?");
        questions.push(json!({
            "id": format!("bundle:{b}"),
            "kind": "bundle",
            "name": b,
            "question": q,
            "description": description,
            "required": false,
            "recommended": true,
            "answer": format!("add \"{b}\" to `bundles` in machine.toml"),
        }));
        rows.push((false, format!("bundle:{b}"), q, "yes".into()));
    }
    for t in rec.tags.iter().filter(|t| !machine.tags.contains(t)) {
        let q = format!("Use the `{t}` tag on this machine?");
        questions.push(json!({
            "id": format!("tag:{t}"),
            "kind": "tag",
            "name": t,
            "question": q,
            "description": null,
            "required": false,
            "recommended": true,
            "answer": format!("add \"{t}\" to `tags` in machine.toml"),
        }));
        rows.push((false, format!("tag:{t}"), q, "yes".into()));
    }

    // Settings of the machine's bundles plus the recommended ones.
    let mut bundles = machine.bundles.clone();
    for b in &rec.bundles {
        if !bundles.contains(b) {
            bundles.push(b.clone());
        }
    }
    let packages = packages_of(&rc.repo, &bundles);
    let tags: BTreeSet<String> = machine.tags.iter().cloned().collect();
    let declared: BTreeSet<String> = rc.repo.manifest.tags.keys().cloned().collect();
    let when = |src: &str| -> Result<bool, String> {
        dots_facts::Condition::parse(src)
            .and_then(|c| {
                c.eval(&dots_facts::EvalCtx {
                    facts: &rc.m.facts,
                    chain: &rc.chain,
                    tags: &tags,
                    declared_tags: &declared,
                    settings: &dots_facts::NoSettings,
                })
            })
            .map_err(|e| e.message)
    };
    let mut inputs = Inputs::new(&rc.repo, machine, &when);
    inputs.bundles = &bundles;
    let settings = dots_config::resolve(&inputs);
    let mut pending = settings.pending(&packages);
    pending.sort_by_key(|(s, why)| (*why != Why::Required, s.id()));
    for (s, why) in pending {
        let (json, row) = setting_question(s, why);
        questions.push(json);
        rows.push(row);
    }

    let required = questions
        .iter()
        .filter(|q| q["required"] == json!(true))
        .count();
    let data = json!({
        "questions": questions,
        "unanswered": questions.len(),
        "required_unanswered": required,
    });
    let mut r = Report::ok("questions", data);
    r.warnings = rc.m.warnings.clone();
    r.warnings.extend(settings.errors.iter().cloned());
    if questions.is_empty() {
        return r.with_headline(Tone::Ok, "nothing to answer");
    }
    let tone = if required > 0 { Tone::Warn } else { Tone::Info };
    r = r.with_headline(
        tone,
        format!(
            "{}{}",
            dots_core::blocks::plural(rows.len(), "question", "questions"),
            if required > 0 {
                format!(", {required} required")
            } else {
                String::new()
            }
        ),
    );
    // One line per question: the id and the question, never dropped on a narrow terminal.
    let lines: Vec<String> = rows
        .into_iter()
        .map(|(req, id, q, _)| format!("{} {id}  {q}", if req { "*" } else { " " }))
        .collect();
    r.push(Block::Code {
        title: Some("* = required".into()),
        lines,
    });
    r.push(Block::hint("dots config set <pkg>.<key> <value>"));
    r
}

fn setting_question(s: &Setting, why: Why) -> (Value, (bool, String, String, String)) {
    let id = s.id();
    let required = why == Why::Required;
    let question = s
        .decl
        .question
        .clone()
        .or_else(|| s.decl.description.clone())
        .unwrap_or_else(|| format!("What should `{id}` be?"));
    // A secret's recommendation is never shown; a required setting has none.
    let recommended = if s.secret() {
        Value::Null
    } else {
        s.value().cloned().unwrap_or(Value::Null)
    };
    let shown = match &recommended {
        Value::Null if required => "(required)".to_string(),
        Value::Null => String::new(),
        v => plain(v),
    };
    let json = json!({
        "id": format!("setting:{id}"),
        "kind": "setting",
        "name": id,
        "package": s.package,
        "key": s.key,
        "question": question,
        "description": s.decl.description,
        "type": s.decl.type_name(),
        "values": s.decl.values,
        "required": required,
        "secret": s.secret(),
        "reason": if required { "required" } else { "ask" },
        "recommended": recommended,
        "answer": format!("dots config set {id} <value>"),
    });
    let text = if shown.is_empty() {
        question
    } else {
        format!("{question} [{shown}]")
    };
    (json, (required, format!("setting:{id}"), text, shown))
}
