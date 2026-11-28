//! `dots upgrade [<pkg>...|--all] [--provider <id>] [--commit]`: move packages to the newest
//! version the intersected constraints allow, rewrite this platform's lock rows, journal every
//! change (all through the engine), and commit only when asked.
//!
//! Leaving a range is done by editing the spec, never by `upgrade`: a prefix `0.11` stays in
//! `0.11.x`, and a version a dependent's constraint rejects is refused, naming the dependent.

use std::cell::RefCell;
use std::collections::BTreeMap;

use dots_core::{Block, Diagnostic, ExitCode, Report, Tone};
use dots_engine::{UpgradeTarget, compare};
use dots_lock::rules::Exact;
use dots_model::vspec::{self, Spec};
use dots_resolve::{ConstraintSource, spec_for_in};
use serde_json::{Value, json};

use crate::app::Context;
use crate::lock_cmd::Written;
use crate::providers::{LiveOffers, OfferCandidate};
use crate::reconcile::{ApplyArgs, Args, Prepared, UpgradeSet, apply_inner};
use dots_resolve::OfferSource;

pub struct UpgradeArgs<'a> {
    pub pkgs: &'a [String],
    pub all: bool,
    pub provider: Option<&'a str>,
    pub commit: bool,
    pub sudo: dots_engine::SudoPolicy,
}

/// One package that moves.
#[derive(Debug, Clone)]
struct Move {
    package: String,
    provider: String,
    from: String,
    target: UpgradeTarget,
}

#[derive(Default)]
struct Outcome {
    moves: Vec<Move>,
    skipped: Vec<(String, String)>,
    errors: Vec<Diagnostic>,
    /// This machine's platform key, for the commit message.
    key: String,
}

enum Verdict {
    Move(Move),
    Skip(String),
    Blocked(Diagnostic),
}

fn exact_of(p: &Prepared, provider: &str, pkg: &str) -> Exact {
    match p
        .registry
        .get_for(provider, pkg)
        .map(|x| x.caps().exact_version)
    {
        Some(dots_providers::Support::Yes) => Exact::Yes,
        Some(dots_providers::Support::No) => Exact::No,
        _ => Exact::Limited,
    }
}

/// What `name` would move to. `into_spec`: the installed version violates the spec (a
/// dependency whose floor is tighter), so moving is required rather than optional.
fn verdict(p: &Prepared, offers: &LiveOffers<'_>, name: &str, into_spec: bool) -> Verdict {
    let Some(pr) = p.resolution.get(name).filter(|r| r.is_active()) else {
        return Verdict::Skip("not part of this machine's package set".into());
    };
    let Some(choice) = pr.chosen_provider() else {
        return Verdict::Skip("no provider".into());
    };
    let Some(inst) = p
        .installed
        .get(&choice.id)
        .and_then(|m| m.get(&choice.native_name))
    else {
        return Verdict::Skip("not installed (dots apply installs it)".into());
    };
    if inst.pinned {
        return Verdict::Skip("pinned by its provider (dots unpin to release it)".into());
    }
    let wanted = &pr.wanted;
    let installed_ok = wanted.admits(&inst.version);
    if !installed_ok && !into_spec {
        return Verdict::Skip(format!(
            "installed {} is outside `{wanted}`; dots apply moves it into the spec",
            inst.version
        ));
    }
    if matches!(wanted, Spec::Exact(_)) {
        return Verdict::Skip(format!(
            "pinned to `{wanted}`; edit the spec to move it (or dots unpin)"
        ));
    }
    let pkg = p.rc.repo.packages.get(name);
    let cfg = pkg.and_then(|k| k.providers.get(&choice.id));
    let spec = spec_for_in(&choice.native_name, cfg, pkg.map(|k| k.dir.as_path()));
    let offered = offers.offered(&choice.id, name, &choice.native_name, &spec);
    let exact = exact_of(p, &choice.id, name);
    let mv = |ask: Option<String>, to: Option<String>| {
        Verdict::Move(Move {
            package: name.to_string(),
            provider: choice.id.clone(),
            from: inst.version.clone(),
            target: UpgradeTarget { version: ask, to },
        })
    };

    if let Spec::Git(r) = wanted {
        // A branch moves; a tag or a sha is a fixed point.
        let fixed = r
            .trim_start_matches('v')
            .starts_with(|c: char| c.is_ascii_digit())
            || (r.len() >= 7 && r.chars().all(|c| c.is_ascii_hexdigit()));
        if fixed {
            return Verdict::Skip(format!(
                "`{wanted}` names a fixed ref; edit the spec to move"
            ));
        }
        return match (&offered, &inst.source) {
            (Some(o), Some(s)) if o.starts_with(s.as_str()) || s.starts_with(o.as_str()) => {
                Verdict::Skip("already at the newest commit".into())
            }
            (Some(o), _) => mv(None, Some(o.clone())),
            (None, _) => Verdict::Skip("the newest commit could not be determined".into()),
        };
    }
    let Some(off) = offered else {
        return Verdict::Skip("the newest version could not be determined (offline?)".into());
    };
    if compare(&off, &inst.version) != std::cmp::Ordering::Greater && installed_ok {
        return Verdict::Skip("already the newest".into());
    }
    if !wanted.admits(&off) || vspec::is_prerelease(&off) {
        // Someone's constraint stops it: name the dependent, or say the spec does.
        let rejecting = pr.constraints.rejecting(&off);
        if let Some(c) = rejecting
            .iter()
            .find(|c| matches!(c.source, ConstraintSource::Requires { .. }))
            && let ConstraintSource::Requires { from, file, .. } = &c.source
        {
            let mut d = Diagnostic::from_code(
                "upgrade-blocked",
                &[
                    ("package", name),
                    ("version", &off),
                    ("dependent", from),
                    ("spec", &c.spec),
                    ("file", file),
                ],
            );
            d.provenance.push(dots_core::Provenance {
                source: format!("package:{from}"),
                file: Some(file.clone()),
                line: c.line,
                col: None,
            });
            return Verdict::Blocked(d);
        }
        return match beyond_range(wanted, &off, exact, &choice.id) {
            Some(Ok(ask)) => mv(Some(ask), None),
            Some(Err(why)) => Verdict::Skip(why),
            None => Verdict::Skip(format!(
                "newest is {off}, outside `{wanted}`; edit the spec to move"
            )),
        };
    }
    let ask = (exact == Exact::Yes).then(|| off.clone());
    mv(ask, Some(off))
}

/// The provider's single "latest" lies beyond a prefix spec: the newest version inside the prefix
/// may still be newer than what is installed. An exact-capable provider is asked for the prefix
/// (it resolves the newest match, as `rules::target` does); any other can only say so.
fn beyond_range(
    wanted: &Spec,
    off: &str,
    exact: Exact,
    provider: &str,
) -> Option<Result<String, String>> {
    let prefix = match wanted {
        Spec::Prefix(p) => p,
        Spec::Bounded { prefix, .. } => prefix,
        _ => return None,
    };
    if vspec::is_prerelease(off) || wanted.admits(off) {
        return None;
    }
    let dotted = prefix
        .iter()
        .map(u64::to_string)
        .collect::<Vec<_>>()
        .join(".");
    Some(if exact == Exact::Yes {
        Ok(dotted)
    } else {
        Err(format!(
            "can't find newest {dotted}.x via {provider} (it only reports the single latest, {off})"
        ))
    })
}

/// The (provider, spec) pairs `verdict` will look up for `names`: installed, unpinned
/// packages with a provider.
fn candidates(p: &Prepared, names: &[String]) -> Vec<OfferCandidate> {
    let mut out = Vec::new();
    for name in names {
        let Some(pr) = p.resolution.get(name).filter(|r| r.is_active()) else {
            continue;
        };
        let Some(choice) = pr.chosen_provider() else {
            continue;
        };
        let installed = p
            .installed
            .get(&choice.id)
            .and_then(|m| m.get(&choice.native_name));
        if installed.is_none_or(|i| i.pinned) {
            continue;
        }
        let pkg = p.rc.repo.packages.get(name);
        let cfg = pkg.and_then(|k| k.providers.get(&choice.id));
        out.push(OfferCandidate {
            provider: choice.id.clone(),
            pkg: name.clone(),
            spec: spec_for_in(&choice.native_name, cfg, pkg.map(|k| k.dir.as_path())),
        });
    }
    out
}

fn compute(p: &Prepared, args: &UpgradeArgs<'_>) -> Outcome {
    let mut out = Outcome {
        key: p.machine_key.clone(),
        ..Outcome::default()
    };
    let explicit = !args.all;
    let names: Vec<String> = if args.all {
        p.resolution
            .active()
            .filter(|r| r.chosen_provider().is_some())
            .map(|r| r.name.clone())
            .collect()
    } else {
        args.pkgs.to_vec()
    };
    // One `latest()` call per shared provider for everything that may be asked.
    let offers = LiveOffers::with_candidates(&p.registry, &p.pctx, candidates(p, &names));
    let mut done: BTreeMap<String, ()> = BTreeMap::new();
    let mut queue: Vec<(String, bool)> = names.into_iter().map(|n| (n, false)).collect();
    while let Some((name, into_spec)) = queue.pop() {
        if done.insert(name.clone(), ()).is_some() {
            continue;
        }
        if !p.rc.repo.packages.contains_key(&name) {
            out.errors.push(Diagnostic::from_code(
                "unknown-package",
                &[("name", &name), ("suggestion", ""), ("file", "")],
            ));
            continue;
        }
        if let Some(only) = args.provider
            && p.resolution
                .get(&name)
                .and_then(|r| r.chosen_provider())
                .is_some_and(|c| c.id != only)
        {
            if explicit {
                out.skipped
                    .push((name.clone(), format!("not installed through {only}")));
            }
            continue;
        }
        match verdict(p, &offers, &name, into_spec) {
            Verdict::Move(m) => {
                // Dependencies whose floors the installed version no longer meets move first.
                for e in p
                    .resolution
                    .edges
                    .iter()
                    .filter(|e| e.package == name && e.kind == dots_resolve::EdgeKind::Requires)
                {
                    let dep = &e.dependency;
                    let unmet = p.resolution.get(dep).is_some_and(|r| {
                        r.chosen_provider()
                            .and_then(|c| p.installed.get(&c.id)?.get(&c.native_name))
                            .is_some_and(|i| !r.wanted.admits(&i.version))
                    });
                    if unmet {
                        queue.push((dep.clone(), true));
                    }
                }
                out.moves.push(m);
            }
            Verdict::Skip(why) => {
                // Skipping a dependency that must move is an error of its own.
                if into_spec {
                    out.errors.push(Diagnostic::from_code(
                        "plan-failed",
                        &[
                            ("package", &name),
                            ("detail", &format!("it needs a newer version, but {why}")),
                        ],
                    ));
                } else {
                    out.skipped.push((name.clone(), why));
                }
            }
            Verdict::Blocked(d) => {
                if explicit || into_spec {
                    out.errors.push(d);
                } else {
                    out.skipped.push((name.clone(), d.message.clone()));
                }
            }
        }
    }
    out
}

fn moves_json(moves: &[Move]) -> Vec<Value> {
    moves
        .iter()
        .map(|m| {
            json!({
                "package": m.package,
                "provider": m.provider,
                "from": m.from,
                "to": m.target.to,
            })
        })
        .collect()
}

fn skipped_json(s: &[(String, String)]) -> Vec<Value> {
    s.iter()
        .map(|(p, why)| json!({ "package": p, "reason": why }))
        .collect()
}

/// `dots: upgrade bottom 0.9.6→0.10.2, delta 0.17→0.18 (macos-15/aarch64)`.
fn commit_message(written: &[Written], key: &str) -> String {
    let rows: Vec<String> = written
        .iter()
        .map(|w| match &w.from {
            Some(f) => format!("{} {f}\u{2192}{}", w.package, w.to),
            None => format!("{} {}", w.package, w.to),
        })
        .collect();
    let mut m = format!(
        "dots: upgrade {} ({key})\n\nDots-Op: upgrade\n",
        rows.join(", ")
    );
    for w in written {
        m.push_str(&format!("Dots-Package: {}\n", w.package));
    }
    m
}

pub fn run(ctx: &Context, a: &UpgradeArgs<'_>) -> Report {
    if !a.all && a.pkgs.is_empty() {
        let d = Diagnostic::from_code(
            "usage-error",
            &[
                ("detail", "name the packages to upgrade, or pass --all"),
                ("help", "dots upgrade --help"),
            ],
        );
        return Report::failure("upgrade", ExitCode::Usage, vec![d]);
    }
    let info: RefCell<Option<Outcome>> = RefCell::new(None);
    let planner = |p: &Prepared| -> Result<UpgradeSet, Box<Report>> {
        let out = compute(p, a);
        if !out.errors.is_empty() {
            let mut r = Report::failure("upgrade", ExitCode::Error, out.errors.clone());
            r.data = json!({ "upgrade": { "targets": [], "skipped": skipped_json(&out.skipped) } });
            return Err(Box::new(r));
        }
        if out.moves.is_empty() {
            let mut r = Report::ok(
                "upgrade",
                json!({ "upgrade": { "targets": [], "skipped": skipped_json(&out.skipped) } }),
            )
            .with_headline(Tone::Ok, "Everything is up to date within its constraints.");
            for (pkg, why) in &out.skipped {
                r.push(Block::text(format!("{pkg}: {why}")));
            }
            return Err(Box::new(r));
        }
        let set: UpgradeSet = out
            .moves
            .iter()
            .map(|m| (m.package.clone(), m.target.clone()))
            .collect();
        *info.borrow_mut() = Some(out);
        Ok(set)
    };
    let args = ApplyArgs {
        common: Args {
            targets: Vec::new(),
            files_only: false,
            packages_only: true,
        },
        // The user asked for exactly this; `upgrade` never downgrades, so nothing else needs
        // a question.
        yes: true,
        dry_run: false,
        allow_downgrade: false,
        root: None,
        // The same sudo modes as `apply`: plain skips the sudo steps and warns.
        sudo: a.sudo,
    };
    let (mut report, written) = apply_inner(ctx, &args, "upgrade", Some(&planner));
    if let Some(out) = info.into_inner() {
        if let Some(obj) = report.data.as_object_mut() {
            obj.insert(
                "upgrade".into(),
                json!({
                    "targets": moves_json(&out.moves),
                    "skipped": skipped_json(&out.skipped),
                    "commit": Value::Null,
                }),
            );
        }
        if !out.skipped.is_empty() && report.errors.is_empty() {
            for (pkg, why) in &out.skipped {
                report.push(Block::text(format!("{pkg}: {why}")));
            }
        }
        if a.commit && report.is_ok() && !written.is_empty() {
            let root = crate::context::load_machine(ctx, None).map(|m| m.root);
            match root {
                Ok(root) => match crate::lock_cmd::commit(
                    &root,
                    &[dots_lock::FILE],
                    &commit_message(&written, &out.key),
                ) {
                    Ok(sha) => {
                        if let Some(o) = report
                            .data
                            .get_mut("upgrade")
                            .and_then(Value::as_object_mut)
                        {
                            o.insert("commit".into(), json!(sha));
                        }
                    }
                    Err(d) => report.warnings.push(d),
                },
                Err(e) => report.warnings.extend(e),
            }
        }
    }
    report
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_latest_beyond_the_prefix_still_moves_within_it_on_exact_providers() {
        let w = Spec::parse("0.11").unwrap();
        assert_eq!(
            beyond_range(&w, "0.12.0", Exact::Yes, "cargo"),
            Some(Ok("0.11".to_string()))
        );
        let skip = beyond_range(&w, "0.12.0", Exact::Limited, "brew")
            .unwrap()
            .unwrap_err();
        assert!(skip.contains("can't find newest 0.11.x via brew"), "{skip}");
        // A floor or an exact spec is not a prefix.
        assert_eq!(
            beyond_range(&Spec::parse(">=1.0").unwrap(), "2.0.0-rc1", Exact::Yes, "x"),
            None
        );
    }
}
