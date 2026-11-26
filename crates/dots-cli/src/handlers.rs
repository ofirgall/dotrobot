//! Command handlers. Unimplemented commands return a "not-implemented" report; later waves
//! replace the stub for their command. None of them prints directly.

use dots_core::Report;

use crate::app::Context;
use crate::cli::Cmd;

pub fn dispatch(cmd: Option<&Cmd>, ctx: &Context) -> Report {
    // `dots` with no arguments behaves like `dots status`.
    let name = cmd.map_or("status", Cmd::name);
    match cmd {
        Some(Cmd::Facts) => facts(ctx),
        Some(Cmd::Why { pkg }) => crate::why::run(ctx, pkg),
        None => crate::status::dashboard_cmd(ctx),
        Some(Cmd::Status { pkg, all, strict }) => {
            crate::status::run(ctx, pkg.as_deref(), *all, *strict)
        }
        Some(Cmd::Outdated { pkgs, refresh }) => crate::outdated::run(ctx, pkgs, *refresh),
        Some(Cmd::Which { bin }) => crate::bins::which_cmd(ctx, bin),
        Some(Cmd::Bins {
            target,
            managed,
            unmanaged,
            unknown,
            shadowed,
        }) => crate::bins::bins_cmd(
            ctx,
            &crate::bins::BinsArgs {
                target: target.as_deref(),
                managed: *managed,
                unmanaged: *unmanaged,
                unknown: *unknown,
                shadowed: *shadowed,
            },
        ),
        Some(Cmd::Doctor) => crate::doctor::run(ctx),
        Some(Cmd::Restore { run_id }) => crate::restore::run(ctx, run_id),
        Some(Cmd::Lint) => crate::lint_cmd::run(ctx),
        Some(Cmd::Init {
            from,
            root,
            defaults,
        }) => crate::init::run(
            ctx,
            &crate::init::Args {
                from: from.clone(),
                root: root.clone(),
                defaults: *defaults,
            },
        ),
        Some(Cmd::Plan {
            targets,
            files_only,
            packages_only,
            check,
        }) => crate::reconcile::plan_cmd(
            ctx,
            &crate::reconcile::Args {
                targets: targets.clone(),
                files_only: *files_only,
                packages_only: *packages_only,
            },
            *check,
        ),
        Some(Cmd::Apply {
            targets,
            files_only,
            packages_only,
            yes,
            dry_run,
            allow_downgrade,
            root,
        }) => crate::reconcile::apply_cmd(
            ctx,
            &crate::reconcile::ApplyArgs {
                common: crate::reconcile::Args {
                    targets: targets.clone(),
                    files_only: *files_only,
                    packages_only: *packages_only,
                },
                yes: *yes,
                dry_run: *dry_run,
                allow_downgrade: *allow_downgrade,
                root: root.clone(),
            },
        ),
        _ => not_implemented(name, ctx),
    }
}

fn not_implemented(name: &str, _ctx: &Context) -> Report {
    Report::not_implemented(name)
}

/// `dots facts`: every fact with its source, the platform chain and the tags, from the real
/// machine config (`[facts]` overrides, tags, `platform_extra`) and the repo's `[platforms]`.
fn facts(ctx: &Context) -> Report {
    use dots_core::ExitCode;
    use serde_json::{Map, json};

    let m = match crate::context::load_machine(ctx, None) {
        Ok(m) => m,
        Err(errors) => return Report::failure("facts", ExitCode::Error, errors),
    };
    let mut warnings = m.warnings.clone();
    // Platform parents live in the repo; a missing or broken repo only costs those.
    let repo = if m.root.join("dots.toml").is_file() {
        match dots_model::Repo::load(&m.root) {
            Ok(l) => Some(l.value),
            Err(e) => {
                warnings.extend(e.errors.into_iter().take(1));
                None
            }
        }
    } else {
        None
    };
    let f = &m.facts;
    let chain = crate::context::build_chain(&m, repo.as_ref());
    let tags: Vec<String> = m.machine.tags.clone();

    let entries = f.entries();
    let mut map = Map::new();
    for (name, value, source) in &entries {
        let mut entry = json!({ "value": value.to_json(), "source": source.as_str() });
        if let Some(n) = f.notes.get(name) {
            entry["detail"] = json!(n);
        }
        map.insert(name.clone(), entry);
    }
    let mut report = Report::ok(
        "facts",
        json!({
            "facts": map,
            "platform_chain": chain.levels(),
            "arch": chain.arch(),
            "tags": tags,
        }),
    );
    report.warnings = warnings;

    use dots_core::{Block, Cell, Section, Tone};
    // One key/value block: stays whole on a narrow terminal, where table columns would drop.
    let detected: Vec<(String, Cell)> = entries
        .iter()
        .map(|(n, v, _)| {
            let shown = v.render();
            let shown = if shown.is_empty() { "-".into() } else { shown };
            (
                n.clone(),
                Cell::from(format!("{shown}  ({})", f.source_label(n))),
            )
        })
        .collect();
    report = report.with_headline(Tone::Info, "Facts about this machine");
    report.push(Block::Section(
        Section::new("Detected").block(Block::Facts(detected)),
    ));
    report.push(Block::facts([
        ("platform", Cell::from(chain.pretty())),
        (
            "tags",
            Cell::from(if tags.is_empty() {
                "(none)".to_string()
            } else {
                tags.join(", ")
            }),
        ),
    ]));
    report
}
