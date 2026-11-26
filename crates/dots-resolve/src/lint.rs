//! `dots lint` for the v0.1 checks (task 2.9): conditions (syntax, unknown facts, tags and
//! platform keys), unresolved references, dependency cycles and script packages without
//! `check`. Schema errors and `//` escape warnings come from `Repo::load`; directive,
//! fragment and override lint is a later task.

use std::collections::BTreeSet;

use dots_core::{Diagnostic, Provenance};

use crate::diag::{At, cond_diag, cycle_at, entry_at, prov_at};
use dots_facts::{Chain, Condition, SettingLookup, suggest};
use dots_model::{Layer, Layers, MachineConfig, ProviderConfig, Repo, Requirement};

pub struct LintInputs<'a> {
    pub repo: &'a Repo,
    pub machine: Option<&'a MachineConfig>,
    /// The platform chain of this machine (its keys are always known).
    pub chain: &'a Chain,
}

#[derive(Debug, Clone, Default)]
pub struct LintReport {
    pub errors: Vec<Diagnostic>,
    pub warnings: Vec<Diagnostic>,
}

const BUILTIN_PLATFORMS: &[&str] = &[
    "any",
    "unix",
    "linux",
    "macos",
    "windows",
    "wsl",
    "debian",
    "ubuntu",
    "linuxmint",
    "pop",
    "raspbian",
    "elementary",
    "arch",
    "manjaro",
    "endeavouros",
    "fedora",
    "rhel",
    "centos",
    "rocky",
    "almalinux",
    "alpine",
    "opensuse",
    "suse",
    "nixos",
    "gentoo",
    "void",
    "kali",
];
const ARCHES: &[&str] = &["aarch64", "x86_64", "x86", "arm", "armv7", "riscv64"];

/// Settings declared by the repo's packages, as `setting.<k>` / `settings.<pkg>.<k>`.
struct RepoSettings<'a> {
    repo: &'a Repo,
    current: Option<&'a str>,
}

impl SettingLookup for RepoSettings<'_> {
    fn resolves(&self, package: Option<&str>, key: &str) -> bool {
        package
            .or(self.current)
            .and_then(|p| self.repo.packages.get(p))
            .is_some_and(|p| p.settings.contains_key(key))
    }
    fn value(&self, _: Option<&str>, _: &str) -> Option<dots_facts::Value> {
        None
    }
}

struct Linter<'a> {
    inp: &'a LintInputs<'a>,
    declared_tags: BTreeSet<String>,
    known_platforms: BTreeSet<String>,
    out: LintReport,
}

pub fn lint(inp: &LintInputs) -> LintReport {
    let repo = inp.repo;
    let mut known: BTreeSet<String> = BUILTIN_PLATFORMS.iter().map(|s| s.to_string()).collect();
    known.extend(inp.chain.levels().iter().cloned());
    for (k, d) in &repo.manifest.platforms {
        known.insert(k.clone());
        known.extend(d.parent.clone());
    }
    if let Some(m) = inp.machine {
        known.extend(m.platform_extra.iter().cloned());
    }
    let mut l = Linter {
        inp,
        declared_tags: repo.manifest.tags.keys().cloned().collect(),
        known_platforms: known,
        out: LintReport::default(),
    };
    l.structure();
    l.conditions();
    l.platforms();
    l.packages();
    l.machine();
    l.out
}

fn prov(source: &str, file: &str, at: At) -> Provenance {
    prov_at(source, file, at)
}

impl Linter<'_> {
    fn error(&mut self, d: Diagnostic) {
        if !self.out.errors.contains(&d) {
            self.out.errors.push(d);
        }
    }
    fn warn(&mut self, d: Diagnostic) {
        if !self.out.warnings.contains(&d) {
            self.out.warnings.push(d);
        }
    }

    fn platform_known(&self, key: &str) -> bool {
        let (base, arch) = match key.split_once('/') {
            Some((b, a)) => (b, Some(a)),
            None => (key, None),
        };
        if arch.is_some_and(|a| !ARCHES.contains(&a)) {
            return false;
        }
        if self.known_platforms.contains(base) {
            return true;
        }
        // `<known>-<version>`, e.g. `ubuntu-22.04` or `macos-15`.
        base.rsplit_once('-').is_some_and(|(id, ver)| {
            self.known_platforms.contains(id)
                && !ver.is_empty()
                && ver.chars().all(|c| c.is_ascii_digit() || c == '.')
        })
    }

    fn unknown_platform(&mut self, key: &str, file: &str, source: &str, at: At) {
        let pool: Vec<&str> = self.known_platforms.iter().map(String::as_str).collect();
        // Suggest on the id and keep a trailing version (`ubunut-22.04` -> `ubuntu-22.04`).
        let (id, ver) = match key.rsplit_once('-') {
            Some((id, v)) if v.chars().all(|c| c.is_ascii_digit() || c == '.') => (id, Some(v)),
            _ => (key, None),
        };
        let sug = suggest(id, pool).map(|s| match ver {
            Some(v) => format!("{s}-{v}"),
            None => s,
        });
        let mut d = Diagnostic::from_code(
            "unknown-platform",
            &[
                ("platform", key),
                ("suggestion", sug.as_deref().unwrap_or("")),
            ],
        )
        .with_provenance(prov(source, file, at));
        if sug.is_none() {
            d.hint = "Run `dots facts` to see this machine's platform keys; custom platforms are declared under `[platforms]` in dots.toml.".into();
        }
        self.error(d);
    }

    /// Bundles and requirements must name things that exist; cycles are errors.
    fn structure(&mut self) {
        let repo = self.inp.repo;
        for (bn, b) in &repo.bundles {
            let file = format!("bundles/{bn}.toml");
            for (i, inc) in b.include.iter().enumerate() {
                if !repo.bundles.contains_key(inc) {
                    let at = b.spans.get(&format!("include.{i}")).copied();
                    self.unknown(
                        "unknown-bundle",
                        "bundle",
                        inc,
                        (&file, at),
                        repo.bundles.keys(),
                    );
                }
            }
            for (i, m) in b.packages.iter().enumerate() {
                if !repo.packages.contains_key(m.name()) {
                    let at = entry_at(&b.spans, "packages", i);
                    self.unknown(
                        "unknown-package",
                        "bundle",
                        m.name(),
                        (&file, at),
                        repo.packages.keys(),
                    );
                }
            }
        }
        for (pn, p) in &repo.packages {
            let file = format!("packages/{pn}/package.toml");
            for (i, r) in p.requires.iter().enumerate() {
                if !repo.packages.contains_key(r.name()) {
                    let at = entry_at(&p.spans, "requires", i);
                    self.unknown(
                        "unknown-package",
                        "package",
                        r.name(),
                        (&file, at),
                        repo.packages.keys(),
                    );
                }
            }
            for (list, names) in [
                ("after", &p.after),
                ("conflicts", &p.conflicts),
                ("replaces", &p.replaces),
            ] {
                for (i, n) in names.iter().enumerate() {
                    if repo.packages.contains_key(n) {
                        continue;
                    }
                    let sug = suggest(n, repo.packages.keys().map(String::as_str))
                        .map(|s| format!(" Did you mean `{s}`?"))
                        .unwrap_or_default();
                    let at = p.spans.get(&format!("{list}.{i}")).copied();
                    let d = Diagnostic::from_code(
                        "unknown-package",
                        &[("name", n), ("suggestion", &sug)],
                    )
                    .with_provenance(prov("package", &file, at));
                    self.warn(d);
                }
            }
        }
        // Bundle include cycles and requires cycles.
        let bundle_edges: Vec<crate::model::Edge> = repo
            .bundles
            .iter()
            .flat_map(|(n, b)| {
                b.include.iter().map(|i| crate::model::Edge {
                    package: n.clone(),
                    dependency: i.clone(),
                    kind: crate::model::EdgeKind::Requires,
                })
            })
            .filter(|e| repo.bundles.contains_key(&e.dependency))
            .collect();
        let bundles: Vec<String> = repo.bundles.keys().cloned().collect();
        for c in crate::graph::find_cycles(&bundles, &bundle_edges) {
            let at = repo.bundles.get(&c[0]).and_then(|b| {
                let i = b.include.iter().position(|x| Some(x) == c.get(1))?;
                b.spans.get(&format!("include.{i}")).copied()
            });
            let d = Diagnostic::from_code("dependency-cycle", &[("cycle", &c.join(" → "))])
                .with_provenance(prov("bundle", &format!("bundles/{}.toml", c[0]), at));
            self.error(d);
        }
        let req_edges: Vec<crate::model::Edge> = repo
            .packages
            .iter()
            .flat_map(|(n, p)| {
                p.requires.iter().map(|r| crate::model::Edge {
                    package: n.clone(),
                    dependency: r.name().to_string(),
                    kind: crate::model::EdgeKind::Requires,
                })
            })
            .filter(|e| repo.packages.contains_key(&e.dependency))
            .collect();
        let pkgs: Vec<String> = repo.packages.keys().cloned().collect();
        for c in crate::graph::find_cycles(&pkgs, &req_edges) {
            let d = Diagnostic::from_code("dependency-cycle", &[("cycle", &c.join(" → "))])
                .with_provenance(prov(
                    "package",
                    &format!("packages/{}/package.toml", c[0]),
                    cycle_at(repo, &c),
                ));
            self.error(d);
        }
    }

    fn unknown<'k>(
        &mut self,
        code: &str,
        source: &str,
        name: &str,
        (file, at): (&str, At),
        known: impl Iterator<Item = &'k String>,
    ) {
        let sug = suggest(name, known.map(String::as_str))
            .map(|s| format!(" Did you mean `{s}`?"))
            .unwrap_or_default();
        let d = Diagnostic::from_code(code, &[("name", name), ("suggestion", &sug)])
            .with_provenance(prov(source, file, at));
        self.error(d);
    }

    /// Check one condition string: syntax and facts (parse), tags and settings (`check`),
    /// platform keys (references). `at` is where the string value starts.
    fn condition(&mut self, when: &str, (file, at): (&str, At), source: &str, pkg: Option<&str>) {
        let cond = match Condition::parse(when) {
            Ok(c) => c,
            Err(e) => {
                self.error(cond_diag(&e, file, at));
                return;
            }
        };
        let settings = RepoSettings {
            repo: self.inp.repo,
            current: pkg,
        };
        for e in cond.check(&self.declared_tags, &settings) {
            self.error(cond_diag(&e, file, at));
        }
        for key in cond.references().platforms {
            if !self.platform_known(&key) {
                self.unknown_platform(&key, file, source, at);
            }
        }
    }

    fn conditions(&mut self) {
        let repo = self.inp.repo;
        for (bn, b) in &repo.bundles {
            let file = format!("bundles/{bn}.toml");
            for (i, m) in b.packages.iter().enumerate() {
                if let Some(w) = m.when() {
                    let at = b.spans.get(&format!("packages.{i}.when")).copied();
                    self.condition(w, (&file, at), &format!("bundle:{bn}"), None);
                }
            }
        }
        for (pn, p) in &repo.packages {
            let file = format!("packages/{pn}/package.toml");
            let src = format!("package:{pn}");
            let cur = Some(pn.as_str());
            let at = |path: String| p.spans.get(&path).copied();
            if let Some(w) = &p.when {
                self.condition(w, (&file, at("when".into())), &src, cur);
            }
            for (i, r) in p.requires.iter().enumerate() {
                if let Requirement::Detailed { when: Some(w), .. } = r {
                    self.condition(w, (&file, at(format!("requires.{i}.when"))), &src, cur);
                }
            }
            for (id, cfg) in &p.providers {
                if let Some(w) = cfg.common().when {
                    self.condition(&w, (&file, at(format!("providers.{id}.when"))), &src, cur);
                }
            }
            for (fi, f) in p.files.iter().enumerate() {
                if let Some(w) = &f.when {
                    self.condition(w, (&file, at(format!("files.{fi}.when"))), &src, cur);
                }
                if let Some(Layers::List(ls)) = &f.layers {
                    for (li, l) in ls.iter().enumerate() {
                        if let Layer::Conditional { when: Some(w), .. } = l {
                            let path = format!("files.{fi}.layers.{li}.when");
                            self.condition(w, (&file, at(path)), &src, cur);
                        }
                    }
                }
                for (si, s) in f.set.iter().enumerate() {
                    if let Some(w) = &s.when {
                        let path = format!("files.{fi}.set.{si}.when");
                        self.condition(w, (&file, at(path)), &src, cur);
                    }
                }
            }
            for (k, s) in &p.settings {
                if let Some(w) = &s.when {
                    self.condition(w, (&file, at(format!("settings.{k}.when"))), &src, cur);
                }
            }
        }
    }

    /// Platform keys used as `[priority]` keys and `[platforms]` parents.
    fn platforms(&mut self) {
        let repo = self.inp.repo;
        let spans = &repo.manifest.spans;
        for k in repo.manifest.priority.keys() {
            if !self.platform_known(k) {
                let at = spans.get(&format!("priority.{k}")).copied();
                self.unknown_platform(k, "dots.toml", "dots.toml", at);
            }
        }
        for (k, d) in &repo.manifest.platforms {
            if let Some(p) = &d.parent
                && !self.platform_known(p)
            {
                let at = spans.get(&format!("platforms.{k}.parent")).copied();
                self.unknown_platform(p, "dots.toml", "dots.toml", at);
            }
        }
    }

    fn packages(&mut self) {
        let repo = self.inp.repo;
        for (pn, p) in &repo.packages {
            if let Some(ProviderConfig::Script(s)) = p.providers.get("script")
                && s.check.is_none()
            {
                let file = format!("packages/{pn}/package.toml");
                let at = p.spans.get("providers.script").copied();
                let d =
                    Diagnostic::from_code("script-no-check", &[("package", pn), ("file", &file)])
                        .with_provenance(prov(&format!("package:{pn}"), &file, at));
                self.warn(d);
            }
        }
    }

    fn machine(&mut self) {
        let Some(m) = self.inp.machine else { return };
        for t in &m.tags {
            if !self.declared_tags.contains(t) {
                let sug =
                    suggest(t, self.declared_tags.iter().map(String::as_str)).unwrap_or_default();
                let d = Diagnostic::from_code("unknown-tag", &[("tag", t), ("suggestion", &sug)])
                    .with_provenance(prov("machine", "machine.toml", None));
                self.error(d);
            }
        }
    }
}
