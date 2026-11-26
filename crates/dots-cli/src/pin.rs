//! `dots pin <pkg> [<version>] [--commit]` and `dots unpin <pkg> [--commit]`.
//!
//! `pin` writes `version` into the package's `package.toml` (through `toml_edit`, so the file's
//! formatting survives) and applies the provider's own pin where it has one (`brew pin`,
//! `apt-mark hold`), so a package manager run behind dots' back does not move it. `unpin`
//! reverses both: the `version` key goes (the package is `latest` again) and the native pin is
//! released.

use std::path::{Path, PathBuf};

use dots_core::{Diagnostic, ExitCode, Provenance, Report, Tone};
use dots_engine::{Journal, JournalEntry, RunLock, new_run_id, normalize};
use dots_model::vspec::Spec;
use dots_providers::{Op, OpKind};
use dots_resolve::spec_for_in;
use serde_json::{Value, json};
use toml_edit::{DocumentMut, value};

use crate::app::Context;
use crate::reconcile::{Prepared, prepare};

pub struct PinArgs<'a> {
    pub pkg: &'a str,
    /// `None` for `unpin`; for `pin`, the version to hold (default: what is installed).
    pub version: Option<&'a str>,
    pub unpin: bool,
    pub commit: bool,
}

fn command(a: &PinArgs<'_>) -> &'static str {
    if a.unpin { "unpin" } else { "pin" }
}

fn fail(a: &PinArgs<'_>, d: Diagnostic) -> Report {
    Report::failure(command(a), ExitCode::Error, vec![d])
}

/// Marks the line `pin` replaced a range on: `version = "0.10.2"  # dots:pinned-from = ">=0.10"`.
const MARKER: &str = "dots:pinned-from = \"";

/// What an edit did.
#[derive(Debug, PartialEq, Eq)]
enum Edited {
    /// The file was written.
    Changed,
    /// The file already said so.
    Unchanged,
    /// `unpin` found a `version` that is a range the user wrote, not a pin: left alone.
    NotAPin(String),
    /// `unpin` put this earlier spec back (the file was written).
    Restored(String),
}

/// The spec recorded in a `dots:pinned-from` marker, and the comment text after it.
fn split_marker(suffix: &str) -> Option<(String, String)> {
    let at = suffix.find(MARKER)? + MARKER.len();
    let len = suffix[at..].find('"')?;
    Some((
        suffix[at..at + len].to_string(),
        suffix[at + len + 1..].to_string(),
    ))
}

fn is_exact(spec: &str) -> bool {
    matches!(Spec::parse(spec), Ok(Spec::Exact(_)))
}

/// The key edit: set or restore/remove top-level `version`, leaving the rest of the file
/// untouched. `pin` replacing a range records it in a trailing comment so `unpin` can put it back.
fn edit(path: &Path, version: Option<&str>) -> Result<Edited, String> {
    let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    let mut doc: DocumentMut = text
        .parse()
        .map_err(|e: toml_edit::TomlError| e.to_string())?;
    let before = doc.to_string();
    let mut outcome = Edited::Changed;
    match version {
        Some(v) => match doc.get_mut("version").and_then(|i| i.as_value_mut()) {
            Some(existing) => {
                let old = existing.as_str().unwrap_or_default().to_string();
                let mut decor = existing.decor().clone();
                let suffix = decor
                    .suffix()
                    .and_then(|s| s.as_str())
                    .unwrap_or_default()
                    .to_string();
                if !old.is_empty() && !is_exact(&old) && !suffix.contains(MARKER) {
                    let rest = suffix.trim();
                    let note = if rest.is_empty() {
                        String::new()
                    } else {
                        format!("  {rest}")
                    };
                    decor.set_suffix(format!(" # {MARKER}{old}\"{note}"));
                }
                let mut new = toml_edit::Value::from(v);
                *new.decor_mut() = decor;
                *existing = new;
            }
            None => {
                // Root keys print before any sub-table, so this lands with the others.
                doc.as_table_mut().insert("version", value(v));
            }
        },
        None => {
            let Some(existing) = doc.get_mut("version").and_then(|i| i.as_value_mut()) else {
                return Ok(Edited::Unchanged);
            };
            let old = existing.as_str().unwrap_or_default().to_string();
            let suffix = existing
                .decor()
                .suffix()
                .and_then(|s| s.as_str())
                .unwrap_or_default()
                .to_string();
            if let Some((from, rest)) = split_marker(&suffix) {
                let mut decor = existing.decor().clone();
                decor.set_suffix(if rest.trim().is_empty() {
                    String::new()
                } else {
                    rest
                });
                let mut new = toml_edit::Value::from(from.as_str());
                *new.decor_mut() = decor;
                *existing = new;
                outcome = Edited::Restored(from);
            } else if is_exact(&old) {
                doc.remove("version");
            } else {
                return Ok(Edited::NotAPin(old));
            }
        }
    }
    let after = doc.to_string();
    if after == before {
        return Ok(Edited::Unchanged);
    }
    std::fs::write(path, after).map_err(|e| e.to_string())?;
    Ok(outcome)
}

/// Run the provider's own pin or unpin, when it has one and the package is installed with it.
/// `Ok(None)`: nothing to do (no native pin, or not installed).
fn native(
    p: &Prepared,
    ctx: &Context,
    name: &str,
    unpin: bool,
) -> Result<Option<Value>, Diagnostic> {
    let Some(choice) = p.resolution.get(name).and_then(|r| r.chosen_provider()) else {
        return Ok(None);
    };
    let Some(provider) = p.registry.get_for(&choice.id, name) else {
        return Ok(None);
    };
    if !provider.caps().pin {
        return Ok(None);
    }
    let installed = p
        .installed
        .get(&choice.id)
        .is_some_and(|m| m.contains_key(&choice.native_name));
    if !installed {
        return Ok(None);
    }
    let pkg = p.rc.repo.packages.get(name);
    let cfg = pkg.and_then(|k| k.providers.get(&choice.id));
    let spec = spec_for_in(&choice.native_name, cfg, pkg.map(|k| k.dir.as_path()));
    // Real runs only: the sandboxed `--home` still talks to the real package manager's pin.
    let ectx = crate::providers::exec_ctx(&ctx.dirs, ctx.opts.offline, false, "pin");
    native_pin(
        provider,
        &ectx,
        (&choice.id, name),
        spec,
        unpin,
        p.rc.m.facts.user != "root",
    )
    .map(Some)
}

/// Run `provider`'s pin (or unpin) of `spec`. Root actions get a plain `sudo` in front, which
/// prompts on the terminal.
fn native_pin(
    provider: &dyn dots_providers::Provider,
    ctx: &dots_providers::Ctx,
    (id, name): (&str, &str),
    spec: dots_providers::Spec,
    unpin: bool,
    use_sudo: bool,
) -> Result<Value, Diagnostic> {
    let kind = if unpin { OpKind::Unpin } else { OpKind::Pin };
    let word = if unpin { "unpin" } else { "pin" };
    let failed = |detail: String| {
        Diagnostic::from_code(
            "native-pin-failed",
            &[
                ("provider", id),
                ("action", word),
                ("package", name),
                ("detail", &detail),
                ("file", ""),
            ],
        )
    };
    let actions = provider
        .plan(&[Op {
            kind,
            spec,
            version: None,
        }])
        .map_err(|e| failed(e.to_string()))?;
    let mut ran: Vec<String> = Vec::new();
    for mut action in actions {
        if action.needs_root && use_sudo {
            action.argv.insert(0, "sudo".into());
        }
        ran.push(action.argv.join(" "));
        match provider.exec(ctx, &action) {
            Ok(o) if o.success => {}
            Ok(_) => return Err(failed(format!("`{}` failed", action.argv.join(" ")))),
            Err(e) => return Err(failed(e.to_string())),
        }
    }
    Ok(json!({ "provider": id, "action": word, "commands": ran }))
}

/// Refuse a pin that would not take effect, or that a dependent forbids. Nothing is written.
fn pin_blocked(p: &Prepared, name: &str, version: &str, shown: &str) -> Option<Diagnostic> {
    use dots_resolve::{Constraint, ConstraintSet, ConstraintSource};
    let set = &p.resolution.get(name)?.constraints;
    // An override that sets `version` wins over the package file: editing the file would report
    // "Pinned" while nothing changes.
    if let Some(c) = set
        .constraints
        .iter()
        .find(|c| matches!(c.source, ConstraintSource::Override { .. }))
    {
        let file = c.source.file();
        let mut d = Diagnostic::from_code(
            "pin-unsupported",
            &[
                ("package", name),
                (
                    "detail",
                    &format!(
                        "an override sets its `version` (\"{}\"), so pinning {shown} would change nothing; edit `version` in {file}",
                        c.spec
                    ),
                ),
                ("file", file),
            ],
        );
        d.provenance.push(Provenance {
            source: "override".into(),
            file: Some(file.to_string()),
            line: c.line,
            col: None,
        });
        return Some(d);
    }
    // Every dependent's `requires` must still hold with the new spec.
    let mut check = ConstraintSet::default();
    check.constraints.push(Constraint {
        spec: version.to_string(),
        source: ConstraintSource::Own {
            file: shown.to_string(),
        },
        line: None,
        via: String::new(),
    });
    check.constraints.extend(
        set.constraints
            .iter()
            .filter(|c| matches!(c.source, ConstraintSource::Requires { .. }))
            .cloned(),
    );
    check.effective(name).err()
}

pub fn run(ctx: &Context, a: &PinArgs<'_>) -> Report {
    let cmd = command(a);
    let _ = ctx.dirs.ensure_all();
    let lock = match RunLock::acquire(&ctx.dirs.run_lock()) {
        Ok(l) => l,
        Err(d) => {
            let exit = if d.code == "lock-busy" {
                ExitCode::LockBusy
            } else {
                ExitCode::Error
            };
            return Report::failure(cmd, exit, vec![d]);
        }
    };
    let run_id = new_run_id();
    let p = match prepare(ctx, None, &run_id) {
        Ok(p) => p,
        Err((e, w)) => {
            let mut r = Report::failure(cmd, ExitCode::Error, e);
            r.warnings = w;
            return r;
        }
    };
    let Some(pkg) = p.rc.repo.packages.get(a.pkg) else {
        let known: Vec<&str> = p.rc.repo.packages.keys().map(String::as_str).collect();
        let sug = dots_facts::suggest(a.pkg, known)
            .map(|s| format!(" Did you mean `{s}`?"))
            .unwrap_or_default();
        return fail(
            a,
            Diagnostic::from_code(
                "unknown-package",
                &[("name", a.pkg), ("suggestion", &sug), ("file", "")],
            ),
        );
    };
    let file: PathBuf = pkg.dir.join("package.toml");
    let shown = format!("packages/{}/package.toml", a.pkg);
    let choice = p.resolution.get(a.pkg).and_then(|r| r.chosen_provider());
    let inst = choice.and_then(|c| p.installed.get(&c.id)?.get(&c.native_name));

    // The version to write.
    let new_version: Option<String> = if a.unpin {
        None
    } else if let Some(v) = a.version {
        if Spec::parse(v).is_err() {
            let mut d = Diagnostic::from_code("version-spec-invalid", &[("spec", v)]);
            d.provenance.push(Provenance {
                source: "command line".into(),
                file: None,
                line: None,
                col: None,
            });
            return fail(a, d);
        }
        Some(v.trim().to_string())
    } else {
        let have = inst.map(|i| normalize(&i.version)).or_else(|| {
            let row = p
                .lock
                .pkgs
                .iter()
                .find(|r| r.name == a.pkg && r.version.is_some());
            row.and_then(|r| r.version.clone())
        });
        match have.filter(|v| Spec::parse(v).is_ok()) {
            Some(v) => Some(v),
            None => {
                return fail(
                    a,
                    Diagnostic::from_code(
                        "pin-unsupported",
                        &[
                            ("package", a.pkg),
                            (
                                "detail",
                                "it is not installed here, so there is no version to hold; pass one: `dots pin <pkg> <version>`",
                            ),
                            ("file", &shown),
                        ],
                    ),
                );
            }
        }
    };

    if let Some(v) = &new_version
        && let Some(d) = pin_blocked(&p, a.pkg, v, &shown)
    {
        return fail(a, d);
    }

    let edited = match edit(&file, new_version.as_deref()) {
        Ok(c) => c,
        Err(e) => {
            return fail(
                a,
                Diagnostic::from_code("write-failed", &[("path", &shown), ("detail", &e)]),
            );
        }
    };

    let changed = matches!(edited, Edited::Changed | Edited::Restored(_));
    let mut warnings: Vec<Diagnostic> = Vec::new();
    let native_result = match native(&p, ctx, a.pkg, a.unpin) {
        Ok(v) => v,
        Err(d) => {
            warnings.push(d);
            None
        }
    };

    // Journal: what happened on this machine.
    let journal = Journal::new(
        &ctx.dirs.journal(),
        &run_id,
        crate::reconcile::journal_meta(&p.rc),
    );
    let _ = journal.append(&JournalEntry {
        op: cmd.into(),
        pkg: a.pkg.to_string(),
        provider: choice.map(|c| c.id.clone()),
        from: inst.map(|i| i.version.clone()),
        to: new_version.clone(),
        exit: 0,
        dur_ms: 0,
        log: None,
    });

    let mut commit: Option<String> = None;
    if a.commit {
        let message = if a.unpin {
            format!(
                "dots: unpin {}\n\nDots-Op: unpin\nDots-Package: {}\n",
                a.pkg, a.pkg
            )
        } else {
            format!(
                "dots: pin {} {}\n\nDots-Op: pin\nDots-Package: {}\n",
                a.pkg,
                new_version.as_deref().unwrap_or(""),
                a.pkg
            )
        };
        let rel = format!("packages/{}/package.toml", a.pkg);
        match crate::lock_cmd::commit(&p.rc.m.root, &[&rel], &message) {
            Ok(sha) => commit = Some(sha),
            Err(d) => warnings.push(d),
        }
    }
    drop(lock);

    let headline = match (&new_version, &edited) {
        (Some(v), _) => format!("Pinned {} to {v}", a.pkg),
        (None, Edited::Restored(from)) => format!("Unpinned {} (restored `{from}`)", a.pkg),
        (None, Edited::NotAPin(v)) => {
            format!("{} is not pinned: `{v}` is a range, left as written", a.pkg)
        }
        (None, _) => format!("Unpinned {}", a.pkg),
    };
    let mut report = Report::ok(
        cmd,
        json!({
            "package": a.pkg,
            "version": new_version,
            "file": shown,
            "changed": changed,
            "native": native_result,
            "commit": commit,
        }),
    )
    .with_headline(Tone::Ok, headline);
    if let Some(n) = &native_result {
        let cmds = n["commands"]
            .as_array()
            .map(|c| {
                c.iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join("; ")
            })
            .unwrap_or_default();
        report.push(dots_core::Block::text(format!(
            "{} {}: {cmds}",
            n["provider"].as_str().unwrap_or(""),
            n["action"].as_str().unwrap_or("")
        )));
    }
    if !changed && !matches!(edited, Edited::NotAPin(_)) {
        report.push(dots_core::Block::text(format!(
            "{shown} already said so; nothing to write"
        )));
    }
    report.warnings = warnings;
    report
}

#[cfg(test)]
mod tests {
    use super::*;
    use dots_testkit::FakeProvider;

    #[test]
    fn a_native_pin_runs_the_providers_own_pin() {
        let brew = FakeProvider::new("brew").with_installed("bottom", "0.10.2");
        let ctx = dots_providers::Ctx::default();
        let r = native_pin(
            &brew,
            &ctx,
            ("brew", "bottom"),
            dots_providers::Spec::new("bottom"),
            false,
            false,
        )
        .unwrap();
        assert_eq!(r["commands"][0], "brew pin bottom");
        let ran = brew.executed();
        assert_eq!(ran.len(), 1);
        assert_eq!(ran[0].kind, OpKind::Pin);
        // And unpin reverses it.
        let r = native_pin(
            &brew,
            &ctx,
            ("brew", "bottom"),
            dots_providers::Spec::new("bottom"),
            true,
            false,
        )
        .unwrap();
        assert_eq!(r["commands"][0], "brew unpin bottom");
    }

    #[test]
    fn edits_keep_the_file_formatting() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("package.toml");
        let original = "schema = 1
name = \"bottom\"  # the tool

[providers.brew]
tap = \"x/y\"
";
        std::fs::write(&f, original).unwrap();
        assert_eq!(edit(&f, Some("0.10.2")).unwrap(), Edited::Changed);
        let text = std::fs::read_to_string(&f).unwrap();
        assert!(text.contains("version = \"0.10.2\""), "{text}");
        assert!(text.contains("name = \"bottom\"  # the tool"), "{text}");
        assert!(
            text.contains("\n[providers.brew]\ntap = \"x/y\"\n"),
            "{text}"
        );
        // Re-pinning rewrites the value in place.
        assert_eq!(edit(&f, Some("0.11.0")).unwrap(), Edited::Changed);
        assert_eq!(
            std::fs::read_to_string(&f)
                .unwrap()
                .matches("version")
                .count(),
            1
        );
        // Unpin removes it again and restores the original bytes.
        assert_eq!(edit(&f, None).unwrap(), Edited::Changed);
        assert_eq!(std::fs::read_to_string(&f).unwrap(), original);
        assert_eq!(edit(&f, None).unwrap(), Edited::Unchanged);
    }

    #[test]
    fn pin_remembers_the_range_it_replaced_and_unpin_restores_it() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("package.toml");
        let original = "schema = 1\nname = \"bottom\"\nversion = \">=0.10\"  # keep me\n";
        std::fs::write(&f, original).unwrap();
        assert_eq!(edit(&f, Some("0.10.2")).unwrap(), Edited::Changed);
        let pinned = std::fs::read_to_string(&f).unwrap();
        assert!(pinned.contains("version = \"0.10.2\""), "{pinned}");
        assert!(pinned.contains("dots:pinned-from = \">=0.10\""), "{pinned}");
        // Re-pinning keeps the memory of the original range.
        assert_eq!(edit(&f, Some("0.10.3")).unwrap(), Edited::Changed);
        assert_eq!(
            edit(&f, None).unwrap(),
            Edited::Restored(">=0.10".to_string())
        );
        assert_eq!(std::fs::read_to_string(&f).unwrap(), original);
    }

    #[test]
    fn unpin_leaves_a_user_range_alone() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("package.toml");
        let original = "schema = 1\nname = \"bottom\"\nversion = \"0.11\"\n";
        std::fs::write(&f, original).unwrap();
        assert_eq!(edit(&f, None).unwrap(), Edited::NotAPin("0.11".into()));
        assert_eq!(std::fs::read_to_string(&f).unwrap(), original);
    }
}
