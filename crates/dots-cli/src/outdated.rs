//! `dots outdated`: packages with a newer version available. Refreshes the latest-version
//! cache (`<cache>/latest/<provider>.json`, 6 hour TTL) unless `--offline`; `--refresh` forces
//! a fetch. `status` reads the same cache and never fetches.

use std::collections::BTreeMap;

use dots_core::blocks::plural;
use dots_core::{
    Block, Cell, Diagnostic, ExitCode, Report, Row, Section, State, Style, Table, Tone,
};
use dots_facts::suggest;
use dots_providers::Spec;
use serde_json::json;

use crate::app::Context;
use crate::inspect::{
    CacheFile, LatestCache, LatestRequest, Loaded, cached_latest, is_outdated, load, now_secs,
    par_map,
};

/// One provider call: the shared instance with all its specs, or a package's own instance.
struct Job<'a> {
    provider: &'a str,
    package: Option<&'a str>,
    specs: Vec<Spec>,
}

/// Ask the providers whose cache is missing, stale or forced. Failures keep the old cache.
fn refresh(
    l: &Loaded,
    cache: &LatestCache,
    reqs: &[LatestRequest],
    force: bool,
    warnings: &mut Vec<Diagnostic>,
) {
    let now = now_secs();
    let mut by_provider: BTreeMap<&str, Vec<&LatestRequest>> = BTreeMap::new();
    for r in reqs {
        by_provider.entry(&r.provider).or_default().push(r);
    }
    let mut jobs: Vec<Job> = Vec::new();
    for (id, rs) in &by_provider {
        let covered = cache.load(id).is_some_and(|f| {
            f.fresh(now) && rs.iter().all(|r| f.versions.contains_key(&r.spec.name))
        });
        if covered && !force {
            continue;
        }
        let mut shared = Vec::new();
        for r in rs {
            if l.registry.has_scoped(id, &r.package) {
                jobs.push(Job {
                    provider: id,
                    package: Some(&r.package),
                    specs: vec![r.spec.clone()],
                });
            } else {
                shared.push(r.spec.clone());
            }
        }
        if !shared.is_empty() {
            jobs.push(Job {
                provider: id,
                package: None,
                specs: shared,
            });
        }
    }
    let results = par_map(&jobs, |j| {
        match l.registry.get_for(j.provider, j.package.unwrap_or("")) {
            Some(p) => p.latest(&l.pctx, &j.specs).map_err(|e| e.to_string()),
            None => Err("provider is not available".to_string()),
        }
    });
    // Merge per provider; a provider with any failed job keeps its previous file.
    let mut fresh: BTreeMap<&str, CacheFile> = BTreeMap::new();
    let mut failed: BTreeMap<&str, String> = BTreeMap::new();
    for (j, r) in jobs.iter().zip(results) {
        let f = fresh.entry(j.provider).or_insert_with(|| CacheFile {
            fetched: now,
            versions: BTreeMap::new(),
        });
        for s in &j.specs {
            f.versions.entry(s.name.clone()).or_insert(None);
        }
        match r {
            Ok(m) => {
                for (name, latest) in m {
                    f.versions.insert(name, Some(latest.version));
                }
            }
            Err(e) => {
                failed.entry(j.provider).or_insert(e);
            }
        }
    }
    for (id, mut f) in fresh {
        if let Some(why) = failed.get(id) {
            warnings.push(Diagnostic::from_code(
                "latest-lookup-failed",
                &[("provider", id), ("detail", why)],
            ));
            continue;
        }
        // Keep answers for packages this call did not ask about.
        if let Some(old) = cache.load(id) {
            for (k, v) in old.versions {
                f.versions.entry(k).or_insert(v);
            }
        }
        let _ = cache.store(id, &f);
    }
}

pub fn run(ctx: &Context, pkgs: &[String], force: bool) -> Report {
    let l = match load(ctx, "outdated") {
        Ok(l) => l,
        Err(r) => return *r,
    };
    for p in pkgs {
        if !l.rc.repo.packages.contains_key(p) {
            let sug = suggest(p, l.rc.repo.packages.keys().map(String::as_str))
                .map(|s| format!(" Did you mean `{s}`?"))
                .unwrap_or_default();
            let d = Diagnostic::from_code("unknown-package", &[("name", p), ("suggestion", &sug)]);
            return crate::inspect::failure(
                "outdated",
                vec![d],
                l.warnings.clone(),
                ExitCode::Error,
            );
        }
    }
    let offline = ctx.opts.offline;
    let cache = LatestCache::new(&ctx.dirs);
    let reqs: Vec<LatestRequest> = l
        .latest_requests()
        .into_iter()
        .filter(|r| pkgs.is_empty() || pkgs.contains(&r.package))
        .collect();
    let mut warnings = l.warnings.clone();
    if !offline {
        refresh(&l, &cache, &reqs, force, &mut warnings);
    }
    let cached = cache.load_all();

    let mut rows: Vec<serde_json::Value> = Vec::new();
    let mut outdated: Vec<(String, String, String, String)> = Vec::new();
    let mut unknown: Vec<String> = Vec::new();
    for r in &reqs {
        let Some(inst) = l
            .active()
            .find(|a| a.name == r.package)
            .and_then(|a| a.installed)
        else {
            continue;
        };
        let wanted =
            l.rc.repo
                .packages
                .get(&r.package)
                .and_then(|p| p.version.clone());
        let latest = cached_latest(&cached, &r.provider, &r.spec.name);
        let state = match latest {
            None => {
                unknown.push(r.package.clone());
                None
            }
            Some(n) if is_outdated(wanted.as_deref(), &inst.version, n, inst.pinned) => {
                outdated.push((
                    r.package.clone(),
                    r.provider.clone(),
                    inst.version.clone(),
                    n.to_string(),
                ));
                Some(State::Outdated)
            }
            Some(_) => Some(State::Ok),
        };
        rows.push(json!({
            "package": r.package,
            "provider": r.provider,
            "wanted": wanted.unwrap_or_else(|| "latest".into()),
            "installed": inst.version,
            "latest": latest,
            "state": state.map(State::word),
        }));
    }
    let data = json!({
        "offline": offline,
        "refreshed": !offline,
        "packages": rows,
        "outdated": outdated.len(),
        "unknown": unknown,
    });
    let mut report = Report::ok("outdated", data);
    report.warnings = warnings;
    if outdated.is_empty() {
        report = report.with_headline(Tone::Ok, "Everything is up to date.");
    } else {
        report = report.with_headline(
            Tone::Warn,
            format!(
                "{} outdated",
                plural(outdated.len(), "package is", "packages are")
            ),
        );
        let mut t = Table::new()
            .styled_column("provider", 3, Style::Muted)
            .column("version", 4);
        for (p, prov, have, latest) in &outdated {
            t.push(
                Row::new(State::Outdated, p.clone())
                    .cell(Cell::muted(prov.clone()))
                    .cell(Cell::version(have, latest)),
            );
        }
        report.push(Block::Section(
            Section::new("OUTDATED")
                .pill(State::Outdated, outdated.len())
                .block(t),
        ));
        report.push(Block::hint("dots upgrade --all"));
    }
    if !unknown.is_empty() {
        let why = if offline {
            "no cached latest version (run `dots outdated` without --offline)"
        } else {
            "its provider reports no latest version"
        };
        report.push(Block::text(format!(
            "{} unknown, {why}: {}",
            plural(unknown.len(), "package", "packages"),
            unknown.join(", ")
        )));
    }
    report
}
