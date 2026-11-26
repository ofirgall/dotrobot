//! `dots why <pkg>`: every inclusion path as a tree, the chosen provider and why.

use dots_core::Lead;
use dots_core::report::Diagnostic;
use dots_core::{Block, Cell, ExitCode, Report, Row, Section, Table, Tone, TreeNode};
use dots_facts::suggest;
use dots_providers::Registry;
use dots_resolve::{
    ConstraintSource, Inclusion, Inputs, InstalledMap, PackageResolution, PackageState, Step,
    resolve,
};
use serde_json::{Value, json};

use crate::app::Context;
use crate::context::{RepoCtx, load_repo};
use crate::providers;

pub fn run(ctx: &Context, pkg: &str) -> Report {
    match load_repo(ctx, None) {
        Err((errors, warnings)) => failure(errors, warnings),
        Ok(rc) => {
            let pctx = providers::exec_ctx(&ctx.dirs, ctx.opts.offline, false, "why");
            let registry =
                providers::registry(&ctx.dirs, &pctx, &rc.m.facts, Some(&rc.repo), &rc.m.root);
            let installed = providers::installed(&registry, &pctx);
            explain(&rc, &registry, &installed, pkg)
        }
    }
}

fn failure(errors: Vec<Diagnostic>, warnings: Vec<Diagnostic>) -> Report {
    let mut r = Report::failure("why", ExitCode::Error, errors);
    r.warnings = warnings;
    r
}

/// Explain `pkg` against an already loaded context and provider registry.
pub fn explain(rc: &RepoCtx, registry: &Registry, installed: &InstalledMap, pkg: &str) -> Report {
    let resolution = resolve(&Inputs {
        repo: &rc.repo,
        machine: &rc.m.machine,
        facts: &rc.m.facts,
        chain: &rc.chain,
        providers: registry,
        installed,
    });

    // Provider problems explain themselves in the output; everything else is an error.
    let mut errors: Vec<Diagnostic> = Vec::new();
    let mut warnings: Vec<Diagnostic> = rc.m.warnings.clone();
    for d in &resolution.errors {
        if d.code == "no-provider-satisfies" {
            warnings.push(d.clone());
        } else {
            errors.push(d.clone());
        }
    }
    warnings.extend(resolution.warnings.iter().cloned());

    if !rc.repo.packages.contains_key(pkg) {
        let sug = suggest(pkg, rc.repo.packages.keys().map(String::as_str))
            .map(|s| format!(" Did you mean `{s}`?"))
            .unwrap_or_default();
        let d = Diagnostic::from_code("unknown-package", &[("name", pkg), ("suggestion", &sug)]);
        return failure(vec![d], warnings);
    }

    let mut report = match resolution.get(pkg) {
        Some(p) => human(Report::ok("why", describe(p)), p),
        None => Report::ok(
            "why",
            json!({ "package": pkg, "status": "not-included", "paths": [], "provider": null, "constraints": [] }),
        )
        .with_headline(Tone::Warn, format!("{pkg}: not included"))
        .with_block(Block::text(
            "no bundle selected on this machine lists it, and no included package requires it",
        )),
    };
    report.warnings = warnings;
    if !errors.is_empty() {
        report.errors = errors;
        report.exit = ExitCode::Error;
    }
    report
}

fn describe(p: &PackageResolution) -> Value {
    let mut data = json!({
        "package": p.name,
        "status": p.state.as_str(),
        "paths": p.inclusions.iter().map(Inclusion::to_json).collect::<Vec<_>>(),
        "provider": p.provider.as_ref().map(|s| s.to_json()),
        "constraints": p.constraints.constraints.iter().map(|c| {
            let (kind, from, file) = match &c.source {
                ConstraintSource::Own { file } => ("own", None, file.clone()),
                ConstraintSource::Requires { from, file, .. } => ("requires", Some(from.clone()), file.clone()),
                ConstraintSource::Override { file } => ("override", None, file.clone()),
            };
            json!({ "spec": c.spec, "source": kind, "from": from, "file": file })
        }).collect::<Vec<_>>(),
    });
    match &p.state {
        PackageState::Inert { condition } => data["condition"] = json!(condition),
        PackageState::Replaced { by } => data["replaced_by"] = json!(by),
        _ => {}
    }
    data
}

// ---------------------------------------------------------------- human output

fn human(report: Report, p: &PackageResolution) -> Report {
    let (tone, state) = match &p.state {
        PackageState::Active => (Tone::Ok, "active".to_string()),
        PackageState::Inert { condition } if condition.is_empty() => {
            (Tone::Muted, "inert (n/a here)".into())
        }
        PackageState::Inert { condition } => (
            Tone::Muted,
            format!("inert (n/a here: `{condition}` is false)"),
        ),
        PackageState::Excluded => (Tone::Muted, "excluded (machine.toml `exclude`)".into()),
        PackageState::Replaced { by } => (Tone::Muted, format!("replaced by `{by}`")),
    };
    let mut report = report.with_headline(tone, format!("{}: {state}", p.name));
    report.push(Block::Tree(vec![tree(&p.inclusions)]));
    if let Some(sel) = &p.provider {
        let mut sec = Section::new("Provider");
        let line = match &sel.chosen {
            Some(c) => format!("provider: {}", c.id),
            None if sel.candidates.is_empty()
                && sel.ranking == dots_resolve::Ranking::Unordered =>
            {
                "provider: none needed (files only)".to_string()
            }
            None => "provider: none usable".to_string(),
        };
        sec = sec.block(Block::text(line));
        let mut reasons: Vec<String> = sel.reasons().into_iter().collect();
        // Skipped candidates are in `reasons`; list the standby ones too for the full picture.
        for c in &sel.candidates {
            if matches!(c.outcome, dots_resolve::CandidateOutcome::Standby) {
                reasons.push(c.line());
            }
        }
        if !reasons.is_empty() {
            sec = sec.block(Block::Code {
                title: None,
                lines: reasons,
            });
        }
        report.push(Block::Section(sec));
    }
    if !p.constraints.is_empty() {
        let mut t = Table::new().column("from", 1);
        for c in &p.constraints.constraints {
            let src = match &c.source {
                ConstraintSource::Own { file } => format!("its own spec ({file})"),
                ConstraintSource::Requires { from, file, .. } => {
                    format!("required by {from} ({file})")
                }
                ConstraintSource::Override { file } => format!("override ({file})"),
            };
            t.push(Row::new(Lead::None, c.spec.clone()).cell(Cell::muted(format!("from {src}"))));
        }
        report.push(Block::Section(Section::new("Version constraints").block(t)));
    }
    report
}

/// The inclusion paths merged on their shared prefixes.
fn tree(paths: &[Inclusion]) -> TreeNode {
    let mut root = TreeNode::new(Step::Machine.label());
    for inc in paths {
        let mut at = &mut root;
        let n = inc.steps.len();
        for (i, step) in inc.steps.iter().enumerate().skip(1) {
            let mut label = step.label();
            if i + 1 == n && !inc.active {
                match inc.blocked_by() {
                    Some(c) => label.push_str(&format!("  (inactive: `{}` is false)", c.when)),
                    None => label.push_str("  (inactive)"),
                }
            }
            let pos = match at.children.iter().position(|c| c.label.text == label) {
                Some(p) => p,
                None => {
                    at.children.push(TreeNode::new(label));
                    at.children.len() - 1
                }
            };
            at = &mut at.children[pos];
        }
    }
    root
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::Path;

    use dots_facts::{Chain, Facts};
    use dots_model::{MachineConfig, Repo};
    use dots_providers::{Caps, Provider, Registry};
    use dots_testkit::FakeProvider;

    use super::*;
    use crate::context::MachineCtx;

    fn rc(sudo: bool) -> RepoCtx {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/good");
        let repo = Repo::load(&root).unwrap().value;
        let facts = Facts {
            os: "linux".into(),
            arch: "x86_64".into(),
            distro: Some("debian".into()),
            has_sudo: sudo,
            ..Default::default()
        };
        let chain = Chain::build(&facts, &BTreeMap::new(), &[]);
        let machine = MachineConfig {
            bundles: vec!["cli".into()],
            ..Default::default()
        };
        RepoCtx {
            m: MachineCtx {
                root,
                machine,
                facts,
                warnings: vec![],
            },
            repo,
            chain,
        }
    }

    #[test]
    fn shows_chosen_provider_and_skip_reasons() {
        let root_only = Caps {
            needs_root: true,
            ..FakeProvider::new("x").caps()
        };
        let reg = Registry::new()
            .with(Box::new(FakeProvider::new("brew").with_caps(root_only)))
            .with(Box::new(FakeProvider::new("cargo")));
        let r = explain(&rc(false), &reg, &BTreeMap::new(), "ripgrep");
        assert!(r.is_ok(), "{:?}", r.errors);
        let text = dots_ui::Blocks::new(&dots_ui::Ui::fixed(dots_ui::Tier::Plain, 100)).report(&r);
        assert!(text.contains("provider: cargo"), "{text}");
        assert!(
            text.contains("cargo chosen: first usable in the priority list for `any`"),
            "{text}"
        );
        // Cargo outranks brew in `any`, so brew is just a standby here.
        assert!(
            text.contains("brew available (not needed)")
                || text.contains("brew skipped: needs root"),
            "{text}"
        );
        assert_eq!(r.data["provider"]["chosen"]["id"], "cargo");
    }
}
