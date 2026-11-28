//! `dots migrate <pkg> --to <provider>`: switch a package to another of its providers.
//!
//! The order keeps the old install working until the new one is proven:
//!
//! 1. install with the new provider (skipped when it is already installed there);
//! 2. verify: the new provider lists the package (its check passes) and every executable it
//!    reports exists. A failure removes what step 1 installed again and changes nothing else;
//! 3. remove it from the old provider(s);
//! 4. only then persist: `prefer` in `package.toml` (through `toml_edit`, only when the ranking
//!    would not already pick the new provider), the ownership record, the journal and
//!    `dots.lock` (the old provider's row goes, the new one's is written).
//!
//! If step 3 fails, both installs stay (the new one works) and nothing is persisted.
//!
//! One deviation from "the bin resolves to the new one": while the old copy is still installed
//! it may legitimately come first on `PATH`, so the check before removal is "the new bin is
//! on disk and executable". After the removal the `PATH` resolution is checked again and a
//! mismatch is a warning.

use std::path::{Path, PathBuf};

use dots_core::{Block, Diagnostic, ExitCode, Report, Tone};
use dots_engine::{Journal, JournalEntry, OwnedPackage, RunLock, State, new_run_id, rfc3339};
use dots_lock::{Lock, PkgRow};
use dots_model::vspec::Spec as VSpec;
use dots_providers::{Availability, Installed, Op, OpKind, Provider};
use dots_resolve::{CandidateOutcome, SkipReason, spec_for_in};
use serde_json::{Value, json};
use toml_edit::{Array, DocumentMut, Item, Value as TomlValue};

use crate::app::Context;
use crate::reconcile::{Prepared, prepare};

pub struct MigrateArgs<'a> {
    pub pkg: &'a str,
    pub to: &'a str,
    /// Remove this provider's copy (the package may no longer list it as a provider).
    pub from: Option<&'a str>,
    /// `--yes`: do not ask before removing the `--from` copy.
    pub yes: bool,
}

fn fail(d: Diagnostic) -> Report {
    Report::failure("migrate", ExitCode::Error, vec![d])
}

fn plan_failed(pkg: &str, detail: impl AsRef<str>) -> Diagnostic {
    Diagnostic::from_code(
        "plan-failed",
        &[("package", pkg), ("detail", detail.as_ref())],
    )
}

/// What an edit of `prefer` did.
#[derive(Debug, PartialEq, Eq)]
enum Edited {
    Changed,
    Unchanged,
}

/// Put `to` first in `prefer` of `table` (the top level, or `[platform.<key>]` when `platform`
/// is set), creating the key, keeping the rest of the file and the other entries' order.
fn edit_prefer(path: &Path, platform: Option<&str>, to: &str) -> Result<Edited, String> {
    use toml_edit::TableLike;
    let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    let mut doc: DocumentMut = text
        .parse()
        .map_err(|e: toml_edit::TomlError| e.to_string())?;
    let before = doc.to_string();
    let table: &mut dyn TableLike = match platform {
        None => doc.as_table_mut(),
        Some(key) => doc
            .get_mut("platform")
            .and_then(Item::as_table_like_mut)
            .and_then(|t| t.get_mut(key))
            .and_then(Item::as_table_like_mut)
            .ok_or_else(|| format!("no [platform.{key}] table in {}", path.display()))?,
    };
    let mut list: Vec<String> = vec![to.to_string()];
    if let Some(existing) = table.get("prefer").and_then(Item::as_array) {
        list.extend(
            existing
                .iter()
                .filter_map(TomlValue::as_str)
                .filter(|p| *p != to)
                .map(str::to_string),
        );
    }
    let mut arr = Array::new();
    for p in &list {
        arr.push(p.as_str());
    }
    match table.get_mut("prefer").and_then(Item::as_value_mut) {
        Some(v) => {
            // Keep the comment after the old value.
            let decor = v.decor().clone();
            *v = TomlValue::Array(arr);
            *v.decor_mut() = decor;
        }
        None => {
            // Root keys print before any sub-table, so this lands with the others.
            table.insert("prefer", Item::Value(TomlValue::Array(arr)));
        }
    }
    if doc.to_string() == before {
        return Ok(Edited::Unchanged);
    }
    dots_files::atomic_write_follow(path, doc.to_string().as_bytes()).map_err(|e| e.to_string())?;
    Ok(Edited::Changed)
}

/// The file, `[platform.<key>]` table and display name of the layer whose `prefer` wins for
/// `pkg`: the highest override or platform table that set it, else the package file.
fn locate_prefer(p: &Prepared, pkg: &str, dir: &Path) -> (PathBuf, Option<String>, String) {
    let own = (
        dir.join("package.toml"),
        None,
        format!("packages/{pkg}/package.toml"),
    );
    let Some(info) = p.rc.repo.effective.as_ref() else {
        return own;
    };
    let address = format!("pkg:{pkg}");
    let Some(obj) = info.objects.get(&address) else {
        return own;
    };
    let Some(src) = obj
        .prov
        .under("prefer")
        .map(|(_, l)| l.source)
        .max_by_key(|s| (s.scope, s.platform.is_some()))
    else {
        return own;
    };
    if src.scope == dots_model::Scope::Definition {
        return (own.0, src.platform.clone(), own.2);
    }
    match info
        .overrides_of(&address)
        .find(|o| o.scope == src.scope && o.file == src.file)
    {
        Some(o) => (o.path.clone(), src.platform.clone(), o.file.clone()),
        None => own,
    }
}

/// The commands of one provider step, for the report and the journal.
#[derive(Default)]
struct Ran {
    steps: Vec<Value>,
}

/// Plan and run `kind` for `spec` through `provider`; root actions get a plain `sudo`.
#[allow(clippy::too_many_arguments)]
fn run_op(
    provider: &dyn Provider,
    ctx: &dots_providers::Ctx,
    id: &str,
    kind: OpKind,
    spec: dots_providers::Spec,
    version: Option<String>,
    wanted: &[dots_providers::Spec],
    use_sudo: bool,
    ran: &mut Ran,
) -> Result<(), String> {
    run_op_checked(
        provider, ctx, id, kind, spec, version, wanted, use_sudo, ran, false,
    )
}

/// As [`run_op`]; with `check_dependents`, a removal first simulates and refuses when it would
/// take other packages along.
#[allow(clippy::too_many_arguments)]
fn run_op_checked(
    provider: &dyn Provider,
    ctx: &dots_providers::Ctx,
    id: &str,
    kind: OpKind,
    spec: dots_providers::Spec,
    version: Option<String>,
    wanted: &[dots_providers::Spec],
    use_sudo: bool,
    ran: &mut Ran,
    check_dependents: bool,
) -> Result<(), String> {
    let word = match kind {
        OpKind::Install => "install",
        OpKind::Remove => "remove",
        _ => "other",
    };
    let actions = provider
        .plan_with_wanted(
            &[Op {
                kind,
                spec,
                version,
            }],
            wanted,
        )
        .map_err(|e| e.to_string())?;
    let mut commands = Vec::new();
    for mut action in actions {
        if check_dependents && action.kind == OpKind::Remove {
            let others = provider
                .removal_collateral(ctx, &action)
                .map_err(|e| format!("could not check what removing it would also remove: {e}"))?;
            if !others.is_empty() {
                return Err(format!(
                    "removing it with `{id}` would also remove other packages ({}); nothing was removed",
                    others.join(", ")
                ));
            }
        }
        if action.needs_root && use_sudo {
            action.argv.insert(0, "sudo".into());
        }
        commands.push(action.argv.join(" "));
        match provider.exec(ctx, &action) {
            Ok(o) if o.success => {}
            Ok(o) => {
                ran.steps
                    .push(json!({ "provider": id, "action": word, "commands": commands }));
                let log = o
                    .log
                    .map(|l| format!(" (log: {})", l.display()))
                    .unwrap_or_default();
                return Err(format!("`{}` failed{log}", action.argv.join(" ")));
            }
            Err(e) => {
                ran.steps
                    .push(json!({ "provider": id, "action": word, "commands": commands }));
                return Err(e.to_string());
            }
        }
    }
    ran.steps
        .push(json!({ "provider": id, "action": word, "commands": commands }));
    Ok(())
}

/// The specs of `id` that stay wanted by every other active package (apt repositories are
/// reference counted through this), plus `extra`.
fn wanted_for(
    p: &Prepared,
    id: &str,
    skip: &str,
    extra: Option<&dots_providers::Spec>,
) -> Vec<dots_providers::Spec> {
    let mut out: Vec<dots_providers::Spec> = p
        .resolution
        .active()
        .filter(|r| r.name != skip)
        .filter_map(|r| {
            let c = r.chosen_provider().filter(|c| c.id == id)?;
            let pkg = p.rc.repo.packages.get(&r.name)?;
            Some(spec_for_in(
                &c.native_name,
                pkg.providers.get(id),
                Some(pkg.dir.as_path()),
            ))
        })
        .collect();
    out.extend(extra.cloned());
    out
}

/// The new provider's view after the install: is it listed, and is every executable it reports
/// on disk?
fn verify(
    provider: &dyn Provider,
    ctx: &dots_providers::Ctx,
    native: &str,
) -> Result<(Installed, Vec<PathBuf>), String> {
    let installed = provider.installed(ctx).map_err(|e| e.to_string())?;
    let Some(inst) = installed.get(native).cloned() else {
        return Err(format!(
            "the provider does not list `{native}` after installing it"
        ));
    };
    let bins: Vec<PathBuf> = provider
        .bins(ctx, &installed)
        .map_err(|e| format!("could not list the executables of `{native}`: {e}"))?
        .into_iter()
        .filter(|e| e.package == native)
        .map(|e| e.path)
        .collect();
    for b in &bins {
        if !crate::bins::is_executable(b) {
            return Err(format!(
                "it reported `{}` but that is not an executable file",
                b.display()
            ));
        }
    }
    Ok((inst, bins))
}

/// Does the first copy of each new bin on PATH belong to the new install? Names the ones that
/// do not.
fn shadowed_by_others(ctx: &dots_providers::Ctx, bins: &[PathBuf]) -> Vec<String> {
    let path: Vec<PathBuf> = ctx
        .env
        .get("PATH")
        .map(|p| {
            std::env::split_paths(p)
                .filter(|d| !d.as_os_str().is_empty())
                .collect()
        })
        .unwrap_or_default();
    let canon = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    let mut out = Vec::new();
    for b in bins {
        let Some(name) = b.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        let copies = crate::bins::find_copies(&path, name);
        // Not on PATH at all is a PATH problem for `dots doctor`, not a shadowing one.
        if let Some(first) = copies.first()
            && canon(first) != canon(b)
        {
            out.push(format!("{name} resolves to {}", first.display()));
        }
    }
    out
}

/// Move this machine's lock row from the old provider(s) to the new one.
fn update_lock(
    p: &Prepared,
    pkg: &str,
    olds: &[String],
    new: (&str, &Installed, &dots_providers::Spec),
) -> Result<bool, Diagnostic> {
    let file = crate::lock_cmd::path(&p.rc.m.root);
    let mut lock = Lock::load(&file)?;
    let before = lock.render();
    for old in olds {
        let platform = dots_lock::platform_for(old, &p.machine_key);
        if lock.pkg(pkg, &platform).is_some_and(|r| r.provider == *old) {
            lock.remove_pkg(pkg, &platform);
        }
    }
    let (id, inst, spec) = new;
    let platform = dots_lock::platform_for(id, &p.machine_key);
    let mut row = PkgRow::new(pkg, &platform, id);
    let (version, rev) = dots_engine::version_and_rev(id, spec, inst);
    row.version = version;
    row.rev = rev;
    row.source = dots_engine::source_of(spec);
    row.locked_at = rfc3339(std::time::SystemTime::now());
    row.by_rev = crate::lock_cmd::repo_rev(&p.rc.m.root);
    if row.version.is_some() || row.rev.is_some() {
        lock.upsert(row);
    }
    if lock.render() == before {
        return Ok(false);
    }
    lock.save(&file)?;
    Ok(true)
}

pub fn run(ctx: &Context, a: &MigrateArgs<'_>) -> Report {
    let _ = ctx.dirs.ensure_all();
    let lock = match RunLock::acquire(&ctx.dirs.run_lock()) {
        Ok(l) => l,
        Err(d) => {
            let exit = if d.code == "lock-busy" {
                ExitCode::LockBusy
            } else {
                ExitCode::Error
            };
            return Report::failure("migrate", exit, vec![d]);
        }
    };
    let run_id = new_run_id();
    let p = match prepare(ctx, None, &run_id) {
        Ok(p) => p,
        Err((e, w)) => {
            let mut r = Report::failure("migrate", ExitCode::Error, e);
            r.warnings = w;
            return r;
        }
    };
    let report = migrate(ctx, &p, a, &run_id);
    drop(lock);
    report
}

fn migrate(ctx: &Context, p: &Prepared, a: &MigrateArgs<'_>, run_id: &str) -> Report {
    let (pkg_name, to) = (a.pkg, a.to);
    let Some(pkg) = p.rc.repo.packages.get(pkg_name) else {
        let known: Vec<&str> = p.rc.repo.packages.keys().map(String::as_str).collect();
        let sug = dots_facts::suggest(pkg_name, known)
            .map(|s| format!(" Did you mean `{s}`?"))
            .unwrap_or_default();
        return fail(Diagnostic::from_code(
            "unknown-package",
            &[("name", pkg_name), ("suggestion", &sug), ("file", "")],
        ));
    };
    let Some(pr) = p.resolution.get(pkg_name).filter(|r| r.is_active()) else {
        return fail(plan_failed(
            pkg_name,
            "it is not part of this machine's package set",
        ));
    };
    let Some(selection) = pr.provider.as_ref() else {
        return fail(plan_failed(pkg_name, "it has no providers to move between"));
    };
    let Some(cand) = selection.candidates.iter().find(|c| c.id == to) else {
        let ids: Vec<&str> = selection.candidates.iter().map(|c| c.id.as_str()).collect();
        return fail(plan_failed(
            pkg_name,
            format!(
                "`{to}` is not one of its providers (candidates: {})",
                ids.join(", ")
            ),
        ));
    };
    if let CandidateOutcome::Skipped(reason) = &cand.outcome {
        return fail(match reason {
            SkipReason::Unavailable(_) | SkipReason::Bootstrap { .. } => Diagnostic::from_code(
                "provider-unavailable",
                &[("provider", to), ("package", pkg_name)],
            ),
            other => plan_failed(pkg_name, format!("`{to}` cannot be used: {other}")),
        });
    }
    let Some(provider) = p.registry.get_for(to, pkg_name) else {
        return fail(Diagnostic::from_code(
            "provider-unavailable",
            &[("provider", to), ("package", pkg_name)],
        ));
    };
    match provider.availability_in(&p.pctx, &p.rc.m.facts) {
        Availability::Ready { .. } => {}
        Availability::Bootstrap { pkg: needs } => {
            return fail(plan_failed(
                pkg_name,
                format!("`{to}` needs `{needs}` installed first; run `dots apply {needs}`"),
            ));
        }
        Availability::Unsupported => {
            return fail(Diagnostic::from_code(
                "provider-unavailable",
                &[("provider", to), ("package", pkg_name)],
            ));
        }
    }
    let use_sudo = p.rc.m.facts.user != "root";
    if provider.caps().needs_root && use_sudo && !p.rc.m.facts.has_sudo {
        return fail(plan_failed(
            pkg_name,
            format!("`{to}` needs root and this machine has no sudo"),
        ));
    }

    let native_of = |id: &str| {
        pkg.providers
            .get(id)
            .and_then(|c| c.common().name)
            .unwrap_or_else(|| pkg_name.to_string())
    };
    let new_native = native_of(to);
    let new_spec = spec_for_in(&new_native, pkg.providers.get(to), Some(pkg.dir.as_path()));
    let have_new = p
        .installed
        .get(to)
        .is_some_and(|m| m.contains_key(&new_native));
    // Every other candidate that has it installed, or the one `--from` names.
    let olds: Vec<(String, String)> = match a.from {
        Some(from) => {
            if from == to {
                return fail(plan_failed(
                    pkg_name,
                    "`--from` and `--to` are the same provider",
                ));
            }
            let native = native_of(from);
            if !p
                .installed
                .get(from)
                .is_some_and(|m| m.contains_key(&native))
            {
                return fail(plan_failed(
                    pkg_name,
                    format!("`{from}` has no `{native}` installed, so there is nothing to remove"),
                ));
            }
            vec![(from.to_string(), native)]
        }
        None => selection
            .candidates
            .iter()
            .filter(|c| c.id != to && c.installed.is_some())
            .map(|c| (c.id.clone(), native_of(&c.id)))
            .collect(),
    };
    if a.from.is_some() && !a.yes {
        let interactive = !ctx.opts.no_input
            && !ctx.opts.json
            && std::io::IsTerminal::is_terminal(&std::io::stdin())
            && std::io::IsTerminal::is_terminal(&std::io::stderr());
        if interactive {
            let (id, native) = &olds[0];
            eprint!(
                "Remove `{native}` from `{id}` (keeping the `{to}` copy of {pkg_name})? [y/N] "
            );
            let mut line = String::new();
            let yes = std::io::stdin().read_line(&mut line).is_ok()
                && matches!(line.trim().to_ascii_lowercase().as_str(), "y" | "yes");
            if !yes {
                return Report::failure(
                    "migrate",
                    ExitCode::Error,
                    vec![Diagnostic::from_code("declined", &[])],
                );
            }
        }
    }
    if olds.is_empty() && !have_new {
        return fail(plan_failed(
            pkg_name,
            format!(
                "it is not installed by any provider, so there is nothing to migrate; `dots apply {pkg_name}` installs it"
            ),
        ));
    }

    let cmd_ctx = crate::providers::exec_ctx(&ctx.dirs, ctx.opts.offline, false, run_id);
    let journal = Journal::new(
        &ctx.dirs.journal(),
        run_id,
        crate::reconcile::journal_meta(&p.rc),
    );
    let jot = |op: &str, provider: &str, from: Option<String>, to: Option<String>, exit: i32| {
        let _ = journal.append(&JournalEntry {
            op: op.into(),
            pkg: pkg_name.to_string(),
            provider: Some(provider.to_string()),
            from,
            to,
            exit,
            dur_ms: 0,
            log: None,
        });
    };
    let mut ran = Ran::default();
    let mut warnings: Vec<Diagnostic> = Vec::new();

    // 1. install with the new provider.
    let wanted_version = match &pr.wanted {
        VSpec::Exact(v) => Some(v.clone()),
        _ => None,
    };
    if !have_new {
        let wanted = wanted_for(p, to, pkg_name, Some(&new_spec));
        if let Err(e) = run_op(
            provider,
            &cmd_ctx,
            to,
            OpKind::Install,
            new_spec.clone(),
            wanted_version,
            &wanted,
            use_sudo,
            &mut ran,
        ) {
            jot("install", to, None, None, 1);
            return fail(Diagnostic::from_code(
                "action-failed",
                &[
                    ("package", pkg_name),
                    ("action", "install"),
                    ("detail", &format!("{e}; the old install is untouched")),
                ],
            ));
        }
    }

    // 2. verify before touching the old install.
    let (inst, bins) = match verify(provider, &cmd_ctx, &new_native) {
        Ok(v) => v,
        Err(why) => {
            if !have_new {
                // Take back what step 1 added, so no duplicate is left behind.
                let wanted = wanted_for(p, to, pkg_name, None);
                if let Err(e) = run_op(
                    provider,
                    &cmd_ctx,
                    to,
                    OpKind::Remove,
                    new_spec.clone(),
                    None,
                    &wanted,
                    use_sudo,
                    &mut ran,
                ) {
                    warnings.push(plan_failed(
                        pkg_name,
                        format!("could not remove the unverified `{to}` install again: {e}"),
                    ));
                }
            }
            jot("install", to, None, None, 1);
            let mut r = fail(Diagnostic::from_code(
                "check-failed-after-install",
                &[("package", pkg_name), ("log", &why), ("file", "")],
            ));
            r.warnings = warnings;
            return r;
        }
    };
    if !have_new {
        jot("install", to, None, Some(inst.version.clone()), 0);
    }

    // 3. remove from the old provider(s).
    let mut removed: Vec<String> = Vec::new();
    for (id, native) in &olds {
        let Some(old) = p.registry.get_for(id, pkg_name) else {
            continue;
        };
        let old_spec = spec_for_in(native, pkg.providers.get(id), Some(pkg.dir.as_path()));
        let old_version = p
            .installed
            .get(id)
            .and_then(|m| m.get(native))
            .map(|i| i.version.clone());
        let wanted = wanted_for(p, id, pkg_name, None);
        match run_op_checked(
            old,
            &cmd_ctx,
            id,
            OpKind::Remove,
            old_spec,
            None,
            &wanted,
            use_sudo,
            &mut ran,
            true,
        ) {
            Ok(()) => {
                jot("remove", id, old_version, None, 0);
                removed.push(id.clone());
            }
            Err(e) => {
                jot("remove", id, old_version, None, 1);
                let mut r = fail(Diagnostic::from_code(
                    "action-failed",
                    &[
                        ("package", pkg_name),
                        ("action", "remove"),
                        (
                            "detail",
                            &format!(
                                "{e}; `{to}` is installed and works, so `{id}` and `{to}` now both have it. Nothing was recorded; fix the removal and run `dots migrate {pkg_name} --to {to}` again"
                            ),
                        ),
                    ],
                ));
                r.warnings = warnings;
                return r;
            }
        }
    }

    // 4. persist.
    let shadow = shadowed_by_others(&cmd_ctx, &bins);
    if !shadow.is_empty() {
        warnings.push(plan_failed(
            pkg_name,
            format!(
                "after the migration {}: check your PATH order (`dots doctor`)",
                shadow.join(", ")
            ),
        ));
    }
    let first_usable = selection
        .candidates
        .iter()
        .find(|c| !matches!(c.outcome, CandidateOutcome::Skipped(_)))
        .map(|c| c.id.as_str());
    let shown = format!("packages/{pkg_name}/package.toml");
    let mut file_changed = false;
    let mut shown = shown;
    if first_usable != Some(to) {
        // The layer that sets `prefer` is the one to edit; another one would not change what wins.
        let (file, platform, layer) = locate_prefer(p, pkg_name, &pkg.dir);
        if layer != "package.toml" {
            shown = layer.clone();
        }
        match edit_prefer(&file, platform.as_deref(), to) {
            Ok(e) => file_changed = e == Edited::Changed,
            Err(e) => warnings.push(Diagnostic::from_code(
                "write-failed",
                &[("path", &layer), ("detail", &e)],
            )),
        }
        // Re-read the model: did `to` really become the first preference?
        let wins = match crate::context::load_repo(ctx, None) {
            Ok(rc) => rc
                .repo
                .packages
                .get(pkg_name)
                .is_some_and(|k| k.prefer.first().map(String::as_str) == Some(to)),
            Err(_) => false,
        };
        if !wins {
            warnings.push(plan_failed(
                pkg_name,
                format!(
                    "`prefer` is set in {layer}, and after the edit `{to}` is still not first; \
                     edit it there so the next `dots apply` keeps `{to}`"
                ),
            ));
        }
    }
    let old_ids: Vec<String> = olds.iter().map(|(id, _)| id.clone()).collect();
    let lock_changed = match update_lock(p, pkg_name, &old_ids, (to, &inst, &new_spec)) {
        Ok(c) => c,
        Err(d) => {
            warnings.push(d);
            false
        }
    };
    let (state_path, mut state) = {
        let path = ctx.dirs.state_json();
        let (s, _) = State::load(&path);
        (path, s)
    };
    state.packages.insert(
        pkg_name.to_string(),
        OwnedPackage {
            provider: to.to_string(),
            name: new_native.clone(),
            version: Some(inst.version.clone()),
            run: run_id.to_string(),
            ts: rfc3339(std::time::SystemTime::now()),
        },
    );
    if let Err(e) = state.save(&state_path) {
        warnings.push(Diagnostic::from_code(
            "write-failed",
            &[
                ("path", &state_path.display().to_string()),
                ("detail", &e.to_string()),
            ],
        ));
    }
    jot(
        "migrate_provider",
        to,
        Some(old_ids.join(",")),
        Some(to.to_string()),
        0,
    );

    let nothing = old_ids.is_empty() && have_new && !file_changed && !lock_changed;
    let from_shown = old_ids.join(", ");
    let mut report = Report::ok(
        "migrate",
        json!({
            "package": pkg_name,
            "from": old_ids,
            "to": to,
            "version": inst.version,
            "installed": !have_new,
            "removed": removed,
            "steps": ran.steps,
            "file": shown,
            "changed": file_changed,
            "lock": lock_changed,
        }),
    )
    .with_headline(
        if nothing { Tone::Info } else { Tone::Ok },
        if nothing {
            format!(
                "Nothing to migrate: {pkg_name} is already installed with {to} and no other provider's copy is known; to remove one, run `dots migrate {pkg_name} --from <provider> --to {to}`"
            )
        } else if old_ids.is_empty() {
            format!("Recorded {pkg_name} on {to} ({})", inst.version)
        } else {
            format!(
                "Migrated {pkg_name} from {from_shown} to {to} ({})",
                inst.version
            )
        },
    );
    for s in &ran.steps {
        let cmds: Vec<&str> = s["commands"]
            .as_array()
            .map(|c| c.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        report.push(Block::text(format!(
            "{} {}: {}",
            s["provider"].as_str().unwrap_or(""),
            s["action"].as_str().unwrap_or(""),
            cmds.join("; ")
        )));
    }
    if file_changed {
        report.push(Block::text(format!("{shown}: prefer starts with {to}")));
    }
    if lock_changed {
        report.push(Block::text("dots.lock updated".to_string()));
    }
    report.warnings = warnings;
    report
}

#[cfg(test)]
mod tests {
    use super::*;

    fn edited(original: &str, to: &str) -> (Edited, String) {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("package.toml");
        std::fs::write(&f, original).unwrap();
        let e = edit_prefer(&f, None, to).unwrap();
        (e, std::fs::read_to_string(&f).unwrap())
    }

    #[test]
    fn prefer_is_created_before_the_sub_tables() {
        let (e, text) = edited(
            "schema = 1\nname = \"bat\"\n\n[providers.apt]\n\n[providers.cargo]\n",
            "cargo",
        );
        assert_eq!(e, Edited::Changed);
        assert!(
            text.contains("prefer = [\"cargo\"]\n")
                && text.find("prefer").unwrap() < text.find("[providers.apt]").unwrap(),
            "{text}"
        );
    }

    #[test]
    fn prefer_moves_the_target_first_and_keeps_the_rest_and_comments() {
        let (e, text) = edited(
            "schema = 1\nname = \"bat\"\nprefer = [\"apt\", \"cargo\", \"brew\"]  # why\n",
            "cargo",
        );
        assert_eq!(e, Edited::Changed);
        assert!(
            text.contains("prefer = [\"cargo\", \"apt\", \"brew\"]  # why"),
            "{text}"
        );
        let (e, _) = edited("schema = 1\nprefer = [\"cargo\", \"apt\"]\n", "cargo");
        assert_eq!(e, Edited::Unchanged);
    }

    use std::collections::BTreeMap;
    use std::sync::Mutex;

    use dots_providers::{Action, BinEntry, Caps, Ctx, Latest, Outcome, ProviderError, Spec};

    /// Lists one package, runs removals (recording them), and can fail `bins` or report
    /// reverse dependencies.
    #[derive(Default)]
    struct Stub {
        collateral: Vec<String>,
        bins_fail: bool,
        executed: Mutex<Vec<String>>,
    }

    impl Provider for Stub {
        fn id(&self) -> &'static str {
            "stub"
        }
        fn caps(&self) -> Caps {
            Caps {
                batch_install: true,
                exact_version: dots_providers::Support::No,
                pin: false,
                needs_root: false,
                uninstall: true,
                exclusive_lock: false,
            }
        }
        fn availability(&self, _: &dots_facts::Facts) -> Availability {
            Availability::Unsupported
        }
        fn installed(&self, _: &Ctx) -> dots_providers::Result<BTreeMap<String, Installed>> {
            Ok(BTreeMap::from([(
                "tool".to_string(),
                Installed {
                    version: "1".into(),
                    pinned: false,
                    source: None,
                },
            )]))
        }
        fn latest(&self, _: &Ctx, _: &[Spec]) -> dots_providers::Result<BTreeMap<String, Latest>> {
            Ok(BTreeMap::new())
        }
        fn plan(&self, ops: &[Op]) -> dots_providers::Result<Vec<Action>> {
            Ok(vec![Action {
                kind: ops[0].kind,
                ops: ops.to_vec(),
                argv: vec!["stub".into(), "remove".into()],
                needs_root: false,
            }])
        }
        fn exec(&self, _: &Ctx, a: &Action) -> dots_providers::Result<Outcome> {
            self.executed.lock().unwrap().push(a.argv.join(" "));
            Ok(Outcome {
                success: true,
                log: None,
            })
        }
        fn removal_collateral(&self, _: &Ctx, _: &Action) -> dots_providers::Result<Vec<String>> {
            Ok(self.collateral.clone())
        }
        fn bins(
            &self,
            _: &Ctx,
            _: &BTreeMap<String, Installed>,
        ) -> dots_providers::Result<Vec<BinEntry>> {
            if self.bins_fail {
                Err(ProviderError::Other("bins exploded".into()))
            } else {
                Ok(vec![])
            }
        }
    }

    #[test]
    fn removal_that_would_take_other_packages_along_is_refused_and_not_run() {
        let p = Stub {
            collateral: vec!["libdep".into(), "other".into()],
            ..Stub::default()
        };
        let mut ran = Ran::default();
        let err = run_op_checked(
            &p,
            &Ctx::default(),
            "stub",
            OpKind::Remove,
            Spec::new("tool"),
            None,
            &[],
            false,
            &mut ran,
            true,
        )
        .unwrap_err();
        assert!(err.contains("libdep") && err.contains("other"), "{err}");
        assert!(p.executed.lock().unwrap().is_empty());
        // Without collateral it runs.
        let p = Stub::default();
        run_op_checked(
            &p,
            &Ctx::default(),
            "stub",
            OpKind::Remove,
            Spec::new("tool"),
            None,
            &[],
            false,
            &mut ran,
            true,
        )
        .unwrap();
        assert_eq!(p.executed.lock().unwrap().len(), 1);
    }

    #[test]
    fn verification_fails_when_the_provider_cannot_list_bins() {
        let p = Stub {
            bins_fail: true,
            ..Stub::default()
        };
        let e = verify(&p, &Ctx::default(), "tool").unwrap_err();
        assert!(e.contains("bins exploded"), "{e}");
    }

    #[cfg(unix)]
    #[test]
    fn prefer_edit_replaces_the_file_atomically_and_keeps_a_symlink() {
        use std::os::unix::fs::MetadataExt;
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real.toml");
        let link = dir.path().join("package.toml");
        std::fs::write(&real, "schema = 1\nprefer = [\"apt\"]\n").unwrap();
        std::os::unix::fs::symlink("real.toml", &link).unwrap();
        let ino = std::fs::metadata(&real).unwrap().ino();
        assert_eq!(edit_prefer(&link, None, "cargo").unwrap(), Edited::Changed);
        assert!(
            std::fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_ne!(std::fs::metadata(&real).unwrap().ino(), ino, "not atomic");
        assert!(
            std::fs::read_to_string(&real)
                .unwrap()
                .contains("[\"cargo\", \"apt\"]")
        );
    }

    #[test]
    fn prefer_edit_targets_a_platform_table() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("package.toml");
        std::fs::write(
            &f,
            "schema = 1\nprefer = [\"apt\"]\n[platform.macos]\nprefer = [\"brew\"]\n",
        )
        .unwrap();
        edit_prefer(&f, Some("macos"), "cargo").unwrap();
        let t = std::fs::read_to_string(&f).unwrap();
        assert!(
            t.contains("prefer = [\"apt\"]") && t.contains("[\"cargo\", \"brew\"]"),
            "{t}"
        );
    }
}
