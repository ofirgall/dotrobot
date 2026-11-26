//! The resolver itself. One [`Pass`] computes everything for a given set of "dropped"
//! (replaced) packages and injected bootstrap packages; [`resolve`] repeats passes until
//! neither set grows. Both only ever grow, so it terminates.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use dots_core::{Diagnostic, Provenance};
use dots_facts::{Chain, CondError, Condition, EvalCtx, Facts, NoSettings, SettingLookup, suggest};
use dots_model::{MachineConfig, Package, Repo, Requirement};
use dots_providers::{Installed, Registry};

use crate::constraints::{
    Constraint, ConstraintSet, ConstraintSource, NoOffers, OfferSource, can_satisfy,
};
use crate::diag::{At, cond_diag, cycle_at, entry_at, prov_at};
use crate::graph;
use crate::model::*;
use dots_model::vspec::Spec;

/// Installed packages per provider: provider id -> provider-native name -> install.
/// Built from each provider's one batched `installed()` call.
pub type InstalledMap = BTreeMap<String, BTreeMap<String, Installed>>;

/// Everything resolution reads.
pub struct Inputs<'a> {
    pub repo: &'a Repo,
    pub machine: &'a MachineConfig,
    pub facts: &'a Facts,
    pub chain: &'a Chain,
    pub providers: &'a Registry,
    pub installed: &'a InstalledMap,
}

/// Resolve the machine's package set. Never panics on bad input; problems are returned as
/// `errors` (resolution is then partial) and `warnings`.
pub fn resolve(inp: &Inputs) -> Resolution {
    resolve_with(inp, &NoOffers)
}

/// Like [`resolve`], with what the providers offer, so provider selection can skip a candidate
/// that cannot deliver the intersected version constraint.
pub fn resolve_with(inp: &Inputs, offers: &dyn OfferSource) -> Resolution {
    resolve_full(inp, offers, &NoSettingsSource)
}

/// Where `when = "setting.x == ..."` on packages, requirements, provider sections and bundle
/// members reads package settings from. `setting.x` means the setting of the package the
/// condition belongs to (for a bundle member, the member's package).
///
/// Settings resolution never depends on the resolver: a setting's own `when` is evaluated
/// without settings (facts, tags and platforms only), and a package's applicability is not
/// an input of settings resolution. So settings can be resolved first, from the repo and the
/// machine config, and then handed to the resolver, which breaks the cycle between
/// "setting applies" and "package `when`".
pub trait SettingsSource {
    fn lookup<'a>(&'a self, package: &'a str) -> Box<dyn SettingLookup + 'a>;
}

/// No settings: every `setting.*` reference is an unresolved-reference error.
pub struct NoSettingsSource;

impl SettingsSource for NoSettingsSource {
    fn lookup<'a>(&'a self, _: &'a str) -> Box<dyn SettingLookup + 'a> {
        Box::new(NoSettings)
    }
}

impl SettingsSource for dots_config::Settings {
    fn lookup<'a>(&'a self, package: &'a str) -> Box<dyn SettingLookup + 'a> {
        Box::new(dots_config::Settings::lookup(self, package))
    }
}

/// [`resolve_with`], with package settings readable from conditions.
pub fn resolve_full(
    inp: &Inputs,
    offers: &dyn OfferSource,
    settings: &dyn SettingsSource,
) -> Resolution {
    let mut dropped: BTreeMap<String, String> = BTreeMap::new();
    let mut injected: BTreeSet<(String, String, String)> = BTreeSet::new();
    for _ in 0..256 {
        let mut pass = Pass::new(inp, offers, settings);
        let out = pass.run(&dropped, &injected);
        let mut changed = false;
        for (victim, by) in out.new_replacements {
            changed |= dropped.insert(victim, by).is_none();
        }
        for i in out.new_injections {
            changed |= injected.insert(i);
        }
        if !changed {
            return out.resolution;
        }
    }
    // Unreachable in practice (both sets are bounded by the package count squared).
    Pass::new(inp, offers, settings)
        .run(&dropped, &injected)
        .resolution
}

struct PassOut {
    resolution: Resolution,
    new_replacements: Vec<(String, String)>,
    new_injections: Vec<(String, String, String)>,
}

struct Pass<'a> {
    inp: &'a Inputs<'a>,
    offers: &'a dyn OfferSource,
    settings: &'a dyn SettingsSource,
    tags: BTreeSet<String>,
    declared_tags: BTreeSet<String>,
    errors: Vec<Diagnostic>,
    warnings: Vec<Diagnostic>,
    own_when: BTreeMap<String, bool>,
}

fn pkg_file(name: &str) -> String {
    format!("packages/{name}/package.toml")
}

fn prov(source: &str, file: &str) -> Provenance {
    Provenance {
        source: source.to_string(),
        file: Some(file.to_string()),
        line: None,
        col: None,
    }
}

fn path_label(steps: &[Step]) -> String {
    steps
        .iter()
        .map(Step::label)
        .collect::<Vec<_>>()
        .join(" → ")
}

fn arrow_join(names: &[String]) -> String {
    names.join(" → ")
}

/// A queue entry: a package reference found in `file`, arriving by `inc`.
struct Reach {
    name: String,
    inc: Inclusion,
    file: String,
    at: At,
}

impl<'a> Pass<'a> {
    fn new(
        inp: &'a Inputs<'a>,
        offers: &'a dyn OfferSource,
        settings: &'a dyn SettingsSource,
    ) -> Self {
        Pass {
            inp,
            offers,
            settings,
            tags: inp.machine.tags.iter().cloned().collect(),
            declared_tags: inp.repo.manifest.tags.keys().cloned().collect(),
            errors: Vec::new(),
            warnings: Vec::new(),
            own_when: BTreeMap::new(),
        }
    }

    fn error(&mut self, d: Diagnostic) {
        if !self.errors.contains(&d) {
            self.errors.push(d);
        }
    }

    fn warn(&mut self, d: Diagnostic) {
        if !self.warnings.contains(&d) {
            self.warnings.push(d);
        }
    }

    // ------------------------------------------------------------ conditions

    /// The package's own `version` as a constraint, attributed to the layer that set it
    /// (an override replaces the package's own spec, never its dependents').
    fn own_constraint(&self, name: &str, spec: &str, path: &[Step]) -> Constraint {
        let repo = self.inp.repo;
        let located = repo
            .effective
            .as_ref()
            .and_then(|info| info.objects.get(&format!("pkg:{name}")))
            .and_then(|obj| obj.prov.get("version"));
        if let Some(loc) = located {
            let file = loc.source.file.clone();
            return if loc.source.scope == dots_model::Scope::Definition {
                Constraint {
                    spec: spec.to_string(),
                    source: ConstraintSource::Own { file },
                    line: loc.line,
                    via: path_label(path),
                }
            } else {
                Constraint {
                    spec: spec.to_string(),
                    source: ConstraintSource::Override { file },
                    line: loc.line,
                    via: loc.source.scope.label().to_string(),
                }
            };
        }
        Constraint {
            spec: spec.to_string(),
            source: ConstraintSource::Own {
                file: pkg_file(name),
            },
            line: repo.packages[name].spans.get("version").map(|s| s.0),
            via: path_label(path),
        }
    }

    /// `owner` is the package whose settings `setting.x` reads.
    fn eval_when(&mut self, owner: &str, when: &str, file: &str, at: At) -> bool {
        let lookup = self.settings.lookup(owner);
        let result = Condition::parse(when).and_then(|c| {
            c.eval(&EvalCtx {
                facts: self.inp.facts,
                chain: self.inp.chain,
                tags: &self.tags,
                declared_tags: &self.declared_tags,
                settings: &*lookup,
            })
        });
        match result {
            Ok(b) => b,
            Err(e) => {
                self.cond_error(&e, file, at);
                false
            }
        }
    }

    fn cond_error(&mut self, e: &CondError, file: &str, at: At) {
        self.error(cond_diag(e, file, at));
    }

    /// Evaluate a `when` that belongs to a path step, recording it.
    fn path_cond(
        &mut self,
        inc_conds: &mut Vec<PathCondition>,
        active: &mut bool,
        (owner, on): (&str, &str),
        when: Option<&str>,
        file: &str,
        at: At,
    ) {
        if let Some(w) = when {
            let result = self.eval_when(owner, w, file, at);
            inc_conds.push(PathCondition {
                on: on.to_string(),
                when: w.to_string(),
                result,
            });
            *active &= result;
        }
    }

    fn package_when(&mut self, name: &str) -> bool {
        if let Some(b) = self.own_when.get(name) {
            return *b;
        }
        let pkg = self.inp.repo.packages.get(name);
        let when = pkg.and_then(|p| p.when.clone());
        let at = pkg.and_then(|p| p.spans.get("when").copied());
        let b = match when {
            Some(w) => self.eval_when(name, &w, &pkg_file(name), at),
            None => true,
        };
        self.own_when.insert(name.to_string(), b);
        b
    }

    // ------------------------------------------------------------ unknown names

    fn unknown(
        &mut self,
        code: &str,
        kind: &str,
        name: &str,
        (file, at): (&str, At),
        known: Vec<&str>,
    ) {
        let sug = suggest(name, known)
            .map(|s| format!(" Did you mean `{s}`?"))
            .unwrap_or_default();
        let d = Diagnostic::from_code(code, &[("name", name), ("suggestion", &sug)])
            .with_provenance(prov_at(kind, file, at));
        self.error(d);
    }

    // ------------------------------------------------------------ expansion

    #[allow(clippy::too_many_arguments)]
    fn walk_bundle(
        &mut self,
        name: &str,
        prefix: &[Step],
        conds: &[PathCondition],
        active: bool,
        from: (&str, At),
        stack: &mut Vec<String>,
        queue: &mut VecDeque<Reach>,
    ) {
        let repo = self.inp.repo;
        let Some(bundle) = repo.bundles.get(name) else {
            let known: Vec<&str> = repo.bundles.keys().map(String::as_str).collect();
            self.unknown("unknown-bundle", "bundle", name, from, known);
            return;
        };
        let file = format!("bundles/{name}.toml");
        if let Some(pos) = stack.iter().position(|b| b == name) {
            let mut cycle: Vec<String> = stack[pos..].to_vec();
            cycle.push(name.to_string());
            let d = Diagnostic::from_code("dependency-cycle", &[("cycle", &arrow_join(&cycle))])
                .with_provenance(prov_at(&format!("bundle:{name}"), from.0, from.1));
            self.error(d);
            return;
        }
        stack.push(name.to_string());
        let mut steps = prefix.to_vec();
        steps.push(Step::Bundle(name.to_string()));
        for (i, inc) in bundle.include.iter().enumerate() {
            let at = bundle.spans.get(&format!("include.{i}")).copied();
            self.walk_bundle(inc, &steps, conds, active, (&file, at), stack, queue);
        }
        for (i, member) in bundle.packages.iter().enumerate() {
            let mut conds = conds.to_vec();
            let mut active = active;
            let when_at = bundle.spans.get(&format!("packages.{i}.when")).copied();
            self.path_cond(
                &mut conds,
                &mut active,
                (member.name(), name),
                member.when(),
                &file,
                when_at,
            );
            let mut steps = steps.clone();
            steps.push(Step::Package(member.name().to_string()));
            queue.push_back(Reach {
                name: member.name().to_string(),
                inc: Inclusion {
                    steps,
                    active,
                    conditions: conds,
                },
                file: file.clone(),
                at: entry_at(&bundle.spans, "packages", i),
            });
        }
        stack.pop();
    }

    /// The set of packages and every way each was reached.
    fn expand(
        &mut self,
        dropped: &BTreeMap<String, String>,
        injected: &BTreeSet<(String, String, String)>,
    ) -> BTreeMap<String, Vec<Inclusion>> {
        let repo = self.inp.repo;
        let machine = self.inp.machine;
        let mut queue: VecDeque<Reach> = VecDeque::new();
        let mut stack = Vec::new();
        for b in &machine.bundles {
            self.walk_bundle(
                b,
                &[Step::Machine],
                &[],
                true,
                ("machine.toml", None),
                &mut stack,
                &mut queue,
            );
        }
        for (bp, provider, for_package) in injected {
            queue.push_back(Reach {
                name: bp.clone(),
                inc: Inclusion {
                    steps: vec![
                        Step::Machine,
                        Step::Bootstrap {
                            provider: provider.clone(),
                            for_package: for_package.clone(),
                        },
                    ],
                    active: true,
                    conditions: vec![],
                },
                file: pkg_file(for_package),
                at: None,
            });
        }

        let mut nodes: BTreeMap<String, Vec<Inclusion>> = BTreeMap::new();
        while let Some(Reach {
            name,
            inc,
            file,
            at,
        }) = queue.pop_front()
        {
            let Some(pkg) = repo.packages.get(&name) else {
                let known: Vec<&str> = repo.packages.keys().map(String::as_str).collect();
                self.unknown("unknown-package", "package", &name, (&file, at), known);
                continue;
            };
            let list = nodes.entry(name.clone()).or_default();
            if list.contains(&inc) {
                continue;
            }
            list.push(inc.clone());
            let expandable = inc.active
                && !machine.exclude.contains(&name)
                && !dropped.contains_key(&name)
                && self.package_when(&name);
            if !expandable {
                continue;
            }
            let pfile = pkg_file(&name);
            for (ri, req) in pkg.requires.iter().enumerate() {
                let (rname, rver, rwhen) = requirement_parts(req);
                // A package already on the path is a cycle; the graph stage reports it.
                let on_path = inc.steps.iter().any(|s| match s {
                    Step::Package(n) | Step::Requires { package: n, .. } => n == rname,
                    _ => false,
                });
                if on_path {
                    continue;
                }
                let mut conds = inc.conditions.clone();
                let mut active = true;
                let when_at = pkg.spans.get(&format!("requires.{ri}.when")).copied();
                self.path_cond(
                    &mut conds,
                    &mut active,
                    (&name, &name),
                    rwhen,
                    &pfile,
                    when_at,
                );
                let mut steps = inc.steps.clone();
                steps.push(Step::Requires {
                    package: rname.to_string(),
                    version: rver.map(str::to_string),
                });
                queue.push_back(Reach {
                    name: rname.to_string(),
                    inc: Inclusion {
                        steps,
                        active,
                        conditions: conds,
                    },
                    file: pfile.clone(),
                    at: entry_at(&pkg.spans, "requires", ri),
                });
            }
        }
        nodes
    }

    fn state_of(
        &mut self,
        name: &str,
        incs: &[Inclusion],
        dropped: &BTreeMap<String, String>,
    ) -> PackageState {
        if self.inp.machine.exclude.iter().any(|e| e == name) {
            return PackageState::Excluded;
        }
        if let Some(by) = dropped.get(name) {
            return PackageState::Replaced { by: by.clone() };
        }
        if !self.package_when(name) {
            let w = self
                .inp
                .repo
                .packages
                .get(name)
                .and_then(|p| p.when.clone())
                .unwrap_or_default();
            return PackageState::Inert { condition: w };
        }
        if incs.iter().any(|i| i.active) {
            return PackageState::Active;
        }
        let w = incs
            .iter()
            .find_map(|i| i.blocked_by().map(|c| c.when.clone()))
            .unwrap_or_default();
        PackageState::Inert { condition: w }
    }

    // ------------------------------------------------------------ provider selection

    fn priority_list(&self) -> Option<(String, Vec<String>)> {
        self.inp.chain.ordered().into_iter().rev().find_map(|k| {
            self.inp
                .repo
                .manifest
                .priority
                .get(&k)
                .map(|l| (k, l.clone()))
        })
    }

    fn availability(&self, id: &str, pkg: &str) -> Result<dots_providers::Availability, String> {
        match self.inp.providers.get_for(id, pkg) {
            None => Err("not implemented in this build".into()),
            Some(p) => match p.availability(self.inp.facts) {
                dots_providers::Availability::Unsupported => {
                    Err("not available on this machine".into())
                }
                a => Ok(a),
            },
        }
    }

    fn needs_root(&self, id: &str, pkg: &str, cfg: Option<&dots_model::ProviderConfig>) -> bool {
        let caps = self
            .inp
            .providers
            .get_for(id, pkg)
            .is_some_and(|p| p.caps().needs_root);
        let script = matches!(cfg, Some(dots_model::ProviderConfig::Script(s)) if s.needs_root);
        caps || script
    }

    fn check_bootstrap(
        &mut self,
        bp: &str,
        dependent: &str,
        dropped: &BTreeMap<String, String>,
    ) -> Result<(), String> {
        if bp == dependent {
            return Err("it would have to bootstrap itself".into());
        }
        if !self.inp.repo.packages.contains_key(bp) {
            return Err("no such package is defined in this repo".into());
        }
        if self.inp.machine.exclude.iter().any(|e| e == bp) {
            return Err("it is excluded on this machine".into());
        }
        if dropped.contains_key(bp) {
            return Err("it is replaced by another package".into());
        }
        if !self.package_when(bp) {
            return Err("it is not available on this platform".into());
        }
        Ok(())
    }

    /// Can provider `id` deliver `wanted` for `native`? Asks the provider what it offers only
    /// when the answer could matter (a numeric constraint on an exact-version-limited provider).
    fn constraint_check(
        &self,
        id: &str,
        pkg: &str,
        native: &str,
        cfg: Option<&dots_model::ProviderConfig>,
        wanted: &Spec,
    ) -> Result<(), String> {
        let exact = self
            .inp
            .providers
            .get_for(id, pkg)
            .map(|p| p.caps().exact_version);
        let installed = self
            .inp
            .installed
            .get(id)
            .and_then(|m| m.get(native))
            .map(|i| i.version.as_str());
        let needs_offer = (wanted.is_range() || matches!(wanted, Spec::Exact(_)))
            && exact != Some(dots_providers::Support::Yes)
            && crate::constraints::offers_only_current(id);
        let offered = if needs_offer {
            let dir = self.inp.repo.packages.get(pkg).map(|p| p.dir.as_path());
            let spec = crate::spec::spec_for_in(native, cfg, dir);
            self.offers.offered(id, pkg, native, &spec)
        } else {
            None
        };
        can_satisfy(id, exact, wanted, installed, offered.as_deref())
    }

    fn select(
        &mut self,
        name: &str,
        wanted: &Spec,
        dropped: &BTreeMap<String, String>,
    ) -> ProviderSelection {
        let repo = self.inp.repo;
        let pkg = &repo.packages[name];
        let file = pkg_file(name);
        let priority = self.priority_list();
        let priority_ids: Vec<String> = priority.as_ref().map(|p| p.1.clone()).unwrap_or_default();

        // A meta package carries other content (files, hooks, env, settings, system settings,
        // `requires`, a `manual/` dir) and names nothing to install: no providers, no
        // `bin`/`bins`, no `version`/`prefer`, no `[service]` that needs the software.
        // repo-schema says "no `[providers]` table = every provider", but applied to such a
        // package it ran `apt install <name>` for something that is not software (found
        // migrating a real repo: a manual-only `serial-access`). It is inert for providers.
        // A package that is only `schema` + `name` still gets the default chain.
        let has_other_content = !pkg.files.is_empty()
            || !pkg.requires.is_empty()
            || pkg.hooks != dots_model::Hooks::default()
            || pkg.env != dots_model::EnvSpec::default()
            || !pkg.settings.is_empty()
            || !pkg.defaults.is_empty()
            || !pkg.registry.is_empty()
            || !pkg.xdg.is_empty()
            || pkg.dir.join("manual").is_dir();
        // (A package with `[[files]]` and nothing to install was always inert, whatever else it
        // says.)
        let files_only = pkg.providers.is_empty()
            && pkg.bin.is_none()
            && pkg.bins.is_empty()
            && (!pkg.files.is_empty()
                || (pkg.version.is_none()
                    && pkg.prefer.is_empty()
                    && pkg.service.is_none()
                    && has_other_content));

        // Candidate ids, before `when`.
        let mut ids: Vec<String> = if !pkg.providers.is_empty() {
            pkg.providers.keys().cloned().collect()
        } else if !pkg.providers_only && !files_only {
            let mut seen = Vec::new();
            for p in &priority_ids {
                if !seen.contains(p) {
                    seen.push(p.clone());
                }
            }
            seen
        } else {
            // `providers_only` with no providers, or a meta package (see above): nothing to install.
            return ProviderSelection {
                chosen: None,
                candidates: vec![],
                ranking: Ranking::Unordered,
                hint: None,
            };
        };
        // Rank: prefer, then priority order, then the rest alphabetically.
        let mut ordered: Vec<String> = Vec::new();
        for list in [&pkg.prefer, &priority_ids] {
            for p in list {
                if ids.contains(p) && !ordered.contains(p) {
                    ordered.push(p.clone());
                }
            }
        }
        ids.sort();
        for p in ids {
            if !ordered.contains(&p) {
                ordered.push(p);
            }
        }
        let ranking = if pkg.prefer.iter().any(|p| ordered.contains(p)) {
            Ranking::Prefer
        } else if let Some((k, _)) = &priority {
            Ranking::Priority(k.clone())
        } else {
            Ranking::Unordered
        };

        struct Eval {
            id: String,
            skip: Option<SkipReason>,
            avail: Option<dots_providers::Availability>,
            installed: Option<String>,
        }
        let mut evals: Vec<Eval> = Vec::new();
        for id in &ordered {
            let cfg = pkg.providers.get(id);
            let common = cfg.map(|c| c.common()).unwrap_or_default();
            let native = common.name.clone().unwrap_or_else(|| name.to_string());
            let installed = self
                .inp
                .installed
                .get(id)
                .and_then(|m| m.get(&native))
                .map(|i| i.version.clone());
            let mut ev = Eval {
                id: id.clone(),
                skip: None,
                avail: None,
                installed,
            };
            let when_at = pkg.spans.get(&format!("providers.{id}.when")).copied();
            if let Some(w) = &common.when
                && !self.eval_when(name, w, &file, when_at)
            {
                ev.skip = Some(SkipReason::WhenFalse(w.clone()));
            } else {
                match self.availability(id, name) {
                    Err(reason) => ev.skip = Some(SkipReason::Unavailable(reason)),
                    Ok(a) => {
                        if self.needs_root(id, name, cfg) && !self.inp.facts.has_sudo {
                            ev.skip = Some(SkipReason::NeedsRoot);
                        } else if let Err(why) =
                            self.constraint_check(id, name, &native, cfg, wanted)
                        {
                            ev.skip = Some(SkipReason::CannotSatisfy(why));
                        } else if let dots_providers::Availability::Bootstrap { pkg: bp } = &a
                            && let Err(why) = self.check_bootstrap(bp, name, dropped)
                        {
                            ev.skip = Some(SkipReason::Bootstrap {
                                package: bp.clone(),
                                why,
                            });
                        }
                        ev.avail = Some(a);
                    }
                }
            }
            evals.push(ev);
        }

        let usable = |e: &Eval| e.skip.is_none();
        let preferred = evals.iter().position(usable);
        // Sticky: the best-ranked candidate that already has the package installed. A root
        // requirement does not unstick it (staying put needs no root); other skips do.
        let sticky = evals.iter().position(|e| {
            e.installed.is_some() && matches!(e.skip, None | Some(SkipReason::NeedsRoot))
        });
        let chosen_idx = sticky.or(preferred);
        let is_sticky = sticky.is_some() && sticky != preferred;
        let hint = match (is_sticky, preferred) {
            (true, Some(p)) => Some(format!("dots migrate {name} --to {}", evals[p].id)),
            _ => None,
        };

        let chosen = chosen_idx.map(|i| {
            let e = &evals[i];
            let cfg = pkg.providers.get(&e.id);
            ProviderChoice {
                id: e.id.clone(),
                native_name: cfg
                    .and_then(|c| c.common().name)
                    .unwrap_or_else(|| name.to_string()),
                bootstrap: match &e.avail {
                    Some(dots_providers::Availability::Bootstrap { pkg }) => Some(pkg.clone()),
                    _ => None,
                },
                sticky: is_sticky,
            }
        });
        let candidates: Vec<CandidateReport> = evals
            .iter()
            .enumerate()
            .map(|(i, e)| CandidateReport {
                id: e.id.clone(),
                installed: e.installed.clone(),
                outcome: if Some(i) == chosen_idx {
                    CandidateOutcome::Chosen
                } else if let Some(r) = &e.skip {
                    CandidateOutcome::Skipped(r.clone())
                } else {
                    CandidateOutcome::Standby
                },
            })
            .collect();

        if chosen.is_none() {
            let detail = if candidates.is_empty() {
                format!(
                    "`{name}` has no candidate providers: declare `[providers.<id>]` in {file}, or add a `[priority]` list to dots.toml."
                )
            } else {
                format!(
                    "Candidates: {}.",
                    candidates
                        .iter()
                        .map(CandidateReport::line)
                        .collect::<Vec<_>>()
                        .join("; ")
                )
            };
            let mut d = Diagnostic::from_code("no-provider-satisfies", &[("package", name)])
                .with_provenance(prov_at(
                    &format!("package:{name}"),
                    &file,
                    pkg.spans.get("providers").copied(),
                ));
            d.hint = detail;
            self.error(d);
        }
        ProviderSelection {
            chosen,
            candidates,
            ranking,
            hint,
        }
    }

    // ------------------------------------------------------------ the pass

    fn run(
        &mut self,
        dropped: &BTreeMap<String, String>,
        injected: &BTreeSet<(String, String, String)>,
    ) -> PassOut {
        let repo = self.inp.repo;
        let machine = self.inp.machine;

        let nodes = self.expand(dropped, injected);
        let mut packages: BTreeMap<String, PackageResolution> = BTreeMap::new();
        for (name, incs) in nodes {
            let state = self.state_of(&name, &incs, dropped);
            packages.insert(
                name.clone(),
                PackageResolution {
                    name,
                    state,
                    inclusions: incs,
                    provider: None,
                    constraints: ConstraintSet::default(),
                    wanted: Spec::Latest,
                },
            );
        }

        for ex in &machine.exclude {
            if !repo.packages.contains_key(ex) {
                let known: Vec<&str> = repo.packages.keys().map(String::as_str).collect();
                let sug = suggest(ex, known)
                    .map(|s| format!(" Did you mean `{s}`?"))
                    .unwrap_or_default();
                let d =
                    Diagnostic::from_code("unknown-package", &[("name", ex), ("suggestion", &sug)])
                        .with_provenance(prov("machine", "machine.toml"));
                self.warn(d);
            }
        }

        // An excluded package that an active package requires cannot be honoured.
        for (name, pr) in &packages {
            if pr.state != PackageState::Excluded {
                continue;
            }
            for inc in pr.inclusions.iter().filter(|i| i.active) {
                if let [
                    ..,
                    Step::Package(by)
                    | Step::Requires { package: by, .. }
                    | Step::Bootstrap {
                        for_package: by, ..
                    },
                    Step::Requires { .. },
                ] = inc.steps.as_slice()
                {
                    let d = Diagnostic::from_code(
                        "excluded-dependency",
                        &[("package", name), ("required_by", by)],
                    )
                    .with_provenance(prov("machine", "machine.toml"));
                    self.error(d);
                }
            }
        }

        let active: Vec<String> = packages
            .values()
            .filter(|p| p.is_active())
            .map(|p| p.name.clone())
            .collect();

        // Constraints with provenance, intersected per package.
        for name in &active {
            let pkg = &repo.packages[name];
            let file = pkg_file(name);
            let path = packages[name]
                .inclusions
                .iter()
                .find(|i| i.active)
                .map(|i| i.steps.clone())
                .unwrap_or_default();
            if let Some(v) = &pkg.version {
                let c = self.own_constraint(name, v, &path);
                packages
                    .get_mut(name)
                    .unwrap()
                    .constraints
                    .constraints
                    .push(c);
            }
            for (ri, req) in pkg.requires.iter().enumerate() {
                let (rname, rver, rwhen) = requirement_parts(req);
                let Some(ver) = rver else { continue };
                let when_at = pkg.spans.get(&format!("requires.{ri}.when")).copied();
                if let Some(w) = rwhen
                    && !self.eval_when(name, w, &file, when_at)
                {
                    continue;
                }
                if let Some(t) = packages.get_mut(rname).filter(|t| t.is_active()) {
                    let mut p = path.clone();
                    p.push(Step::Requires {
                        package: rname.to_string(),
                        version: Some(ver.to_string()),
                    });
                    t.constraints.constraints.push(Constraint {
                        spec: ver.to_string(),
                        line: pkg
                            .spans
                            .get(&format!("requires.{ri}.version"))
                            .map(|s| s.0),
                        via: path_label(&p),
                        source: ConstraintSource::Requires {
                            from: name.clone(),
                            file: file.clone(),
                            path: p,
                        },
                    });
                }
            }
        }
        for name in &active {
            let t = packages.get_mut(name).unwrap();
            t.constraints.sort();
            match t.constraints.effective(name) {
                Ok(spec) => t.wanted = spec,
                Err(d) => {
                    t.wanted = Spec::Latest;
                    self.error(d);
                }
            }
        }

        // Providers.
        let mut new_injections = Vec::new();
        for name in &active {
            let wanted = packages[name].wanted.clone();
            let sel = self.select(name, &wanted, dropped);
            if let Some(bp) = sel.chosen.as_ref().and_then(|c| c.bootstrap.clone()) {
                let id = sel.chosen.as_ref().unwrap().id.clone();
                new_injections.push((bp, id, name.clone()));
            }
            packages.get_mut(name).unwrap().provider = Some(sel);
        }
        new_injections.retain(|i| !injected.contains(i));

        // `replaces`: drop the other package; the next pass recomputes without it.
        let mut new_replacements: Vec<(String, String)> = Vec::new();
        let mut gone: BTreeSet<&str> = BTreeSet::new();
        for a in &active {
            if gone.contains(a.as_str()) {
                continue;
            }
            for b in &repo.packages[a].replaces {
                if b != a && active.contains(b) && !gone.contains(b.as_str()) {
                    gone.insert(b.as_str());
                    new_replacements.push((b.clone(), a.clone()));
                }
            }
        }
        if !new_replacements.is_empty() || !new_injections.is_empty() {
            return PassOut {
                resolution: Resolution::default(),
                new_replacements,
                new_injections,
            };
        }

        // Final: graph, order, conflicts, collisions.
        let mut res = Resolution {
            replaced: dropped
                .iter()
                .filter(|(v, _)| packages.contains_key(*v))
                .map(|(v, by)| Replacement {
                    package: v.clone(),
                    by: by.clone(),
                })
                .collect(),
            ..Default::default()
        };
        self.finish_graph(&packages, &active, &mut res);
        res.packages = packages;
        res.errors = std::mem::take(&mut self.errors);
        res.warnings = std::mem::take(&mut self.warnings);
        PassOut {
            resolution: res,
            new_replacements: vec![],
            new_injections: vec![],
        }
    }

    fn finish_graph(
        &mut self,
        packages: &BTreeMap<String, PackageResolution>,
        active: &[String],
        res: &mut Resolution,
    ) {
        let repo = self.inp.repo;
        // The package that stands in for `name`: itself, or its replacer.
        let target = |name: &str| -> Option<String> {
            match &packages.get(name)?.state {
                PackageState::Active => Some(name.to_string()),
                PackageState::Replaced { by }
                    if packages.get(by).is_some_and(|p| p.is_active()) =>
                {
                    Some(by.clone())
                }
                _ => None,
            }
        };

        let mut edges: Vec<Edge> = Vec::new();
        for name in active {
            let pkg: &Package = &repo.packages[name];
            let file = pkg_file(name);
            for (ri, req) in pkg.requires.iter().enumerate() {
                let (rname, _, rwhen) = requirement_parts(req);
                let when_at = pkg.spans.get(&format!("requires.{ri}.when")).copied();
                if let Some(w) = rwhen
                    && !self.eval_when(name, w, &file, when_at)
                {
                    continue;
                }
                if let Some(t) = target(rname)
                    && (t != *name || rname == name)
                {
                    edges.push(Edge {
                        package: name.clone(),
                        dependency: t,
                        kind: EdgeKind::Requires,
                    });
                }
            }
            for a in &pkg.after {
                if let Some(t) = target(a)
                    && t != *name
                {
                    edges.push(Edge {
                        package: name.clone(),
                        dependency: t,
                        kind: EdgeKind::After,
                    });
                }
            }
            if let Some(bp) = packages[name]
                .chosen_provider()
                .and_then(|c| c.bootstrap.clone())
                && let Some(t) = target(&bp)
            {
                edges.push(Edge {
                    package: name.clone(),
                    dependency: t,
                    kind: EdgeKind::Bootstrap,
                });
            }
        }

        for cycle in graph::find_cycles(active, &edges) {
            let d = Diagnostic::from_code("dependency-cycle", &[("cycle", &arrow_join(&cycle))])
                .with_provenance(prov_at(
                    &format!("package:{}", cycle[0]),
                    &pkg_file(&cycle[0]),
                    cycle_at(repo, &cycle),
                ));
            self.error(d);
        }
        res.order = graph::topo_order(active, &edges);
        res.edges = edges;

        // Conflicts.
        for (i, a) in active.iter().enumerate() {
            for b in &active[i + 1..] {
                let (pa, pb) = (&repo.packages[a], &repo.packages[b]);
                if pa.conflicts.contains(b) || pb.conflicts.contains(a) {
                    let d = Diagnostic::from_code("package-conflict", &[("left", a), ("right", b)])
                        .with_provenance(prov(&format!("package:{a}"), &pkg_file(a)))
                        .with_provenance(prov(&format!("package:{b}"), &pkg_file(b)));
                    self.error(d);
                }
            }
        }

        // Binary collisions.
        let mut claims: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for name in active {
            for bin in claimed_bins(&repo.packages[name], packages[name].chosen_provider()) {
                claims.entry(bin).or_default().push(name.clone());
            }
        }
        for (bin, owners) in &claims {
            for other in owners.iter().skip(1) {
                let left = &owners[0];
                let d = Diagnostic::from_code(
                    "binary-collision",
                    &[("bin", bin), ("left", left), ("right", other)],
                )
                .with_provenance(prov(&format!("package:{left}"), &pkg_file(left)))
                .with_provenance(prov(&format!("package:{other}"), &pkg_file(other)));
                self.error(d);
            }
        }
    }
}

fn requirement_parts(r: &Requirement) -> (&str, Option<&str>, Option<&str>) {
    match r {
        Requirement::Name(n) => (n, None, None),
        Requirement::Detailed {
            name,
            version,
            when,
        } => (name, version.as_deref(), when.as_deref()),
    }
}

/// The executable names a package puts on PATH with its chosen provider: the provider's own
/// `bin`/`bins` if it sets any (else the package's), plus the provider's `shim`.
pub(crate) fn claimed_bins(pkg: &Package, choice: Option<&ProviderChoice>) -> BTreeSet<String> {
    let common = choice
        .and_then(|c| pkg.providers.get(&c.id))
        .map(|c| c.common())
        .unwrap_or_default();
    let mut out = BTreeSet::new();
    if common.bin.is_some() || !common.bins.is_empty() {
        out.extend(common.bin);
        out.extend(common.bins);
    } else {
        out.extend(pkg.bin.clone());
        out.extend(pkg.bins.iter().cloned());
    }
    out.extend(common.shim);
    out
}
