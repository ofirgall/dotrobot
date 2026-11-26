//! Package settings (ARCHITECTURE §17): typed knobs a package author anticipates, resolved
//! across six scopes, lowest to highest:
//!
//! 1. package default (`packages/<p>/package.toml`)
//! 2. bundle (`[settings.<pkg>]` in a selected bundle)
//! 3. upstream (a followed upstream's `dots.toml`)
//! 4. repo (`dots.toml`, overrides included)
//! 5. committed machine profile (`machines/<name>.toml`)
//! 6. machine (`machine.toml`)
//!
//! Not to be confused with `dots-settings`, which manages *system* settings (`[[defaults]]`).
//!
//! [`resolve`] reads the effective repo and the machine config and returns every declared
//! setting with the full chain of layers, so `config explain` can show each scope and the
//! winner. Values are validated against their declaration; a violation names the scope and
//! file it came from. Secrets live only in the machine scope and are redacted by the
//! `shown_*` accessors; the real value is only for rendering ([`Settings::vars`]).

mod edit;

use std::collections::{BTreeMap, BTreeSet};

use dots_core::Diagnostic;
use dots_facts::SettingLookup;
use dots_model::{MachineConfig, Repo, Scope, SettingDecl, SettingValues};
use serde_json::{Map, Value};

pub use edit::{set_in_file, unset_in_file, write_atomic};

/// What a secret's value is replaced with in human output, JSON and logs.
pub const REDACTED: &str = "•••";

/// Where a value came from, lowest precedence first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SettingScope {
    Default,
    Bundle,
    Upstream,
    Repo,
    Profile,
    Machine,
}

impl SettingScope {
    pub const ALL: [SettingScope; 6] = [
        SettingScope::Default,
        SettingScope::Bundle,
        SettingScope::Upstream,
        SettingScope::Repo,
        SettingScope::Profile,
        SettingScope::Machine,
    ];

    /// Stable name used in JSON and the file header: `default`, `bundle`, `upstream`,
    /// `repo`, `profile`, `machine`.
    pub fn as_str(self) -> &'static str {
        match self {
            SettingScope::Default => "default",
            SettingScope::Bundle => "bundle",
            SettingScope::Upstream => "upstream",
            SettingScope::Repo => "repo",
            SettingScope::Profile => "profile",
            SettingScope::Machine => "machine",
        }
    }
}

/// One scope's value for one setting.
#[derive(Debug, Clone, PartialEq)]
pub struct Layer {
    pub scope: SettingScope,
    /// The bundle name, `dots.toml`, `machines/<name>.toml`, `machine.toml`, a package.toml
    /// path, or an override file.
    pub file: String,
    pub line: Option<u32>,
    pub value: Value,
    /// Why this value is not used (type, enum or pattern violation, secret outside machine).
    pub error: Option<String>,
}

impl Layer {
    /// `dots.toml (repo)`: what an error names.
    pub fn origin(&self) -> String {
        format!("{} ({})", self.file, self.scope.as_str())
    }

    pub fn valid(&self) -> bool {
        self.error.is_none()
    }
}

/// A declared setting with every layer that sets it.
#[derive(Debug, Clone)]
pub struct Setting {
    pub package: String,
    pub key: String,
    pub decl: SettingDecl,
    /// False when its `when` is false here: it cannot be set and is not shown.
    pub applies: bool,
    /// Lowest precedence first; the default (when declared) is the first layer.
    pub layers: Vec<Layer>,
    winner: Option<usize>,
}

impl Setting {
    /// `<pkg>.<key>`.
    pub fn id(&self) -> String {
        format!("{}.{}", self.package, self.key)
    }

    pub fn secret(&self) -> bool {
        self.decl.secret
    }

    pub fn winning_layer(&self) -> Option<&Layer> {
        self.winner.map(|i| &self.layers[i])
    }

    /// The effective value (real, even for secrets: use [`Setting::shown`] for output).
    pub fn value(&self) -> Option<&Value> {
        self.winning_layer().map(|l| &l.value)
    }

    pub fn scope(&self) -> Option<SettingScope> {
        self.winning_layer().map(|l| l.scope)
    }

    /// The default value, if declared.
    pub fn default(&self) -> Option<&Value> {
        self.decl.default.as_ref()
    }

    /// Required, applicable and still without a value.
    pub fn missing(&self) -> bool {
        self.applies && self.decl.required && self.winner.is_none()
    }

    /// Some scope above the default sets a usable value.
    pub fn answered(&self) -> bool {
        self.layers
            .iter()
            .any(|l| l.scope != SettingScope::Default && l.valid())
    }

    /// The value for output: `•••` for a secret that has one.
    pub fn shown(&self) -> Option<Value> {
        self.value().map(|v| self.redact(v))
    }

    /// `v` as it may be printed.
    pub fn redact(&self, v: &Value) -> Value {
        if self.secret() {
            Value::String(REDACTED.into())
        } else {
            v.clone()
        }
    }
}

/// Everything [`resolve`] reads.
pub struct Inputs<'a> {
    pub repo: &'a Repo,
    /// Selected bundle names; their `include`s are followed.
    pub bundles: &'a [String],
    /// `[settings]` of followed upstreams' `dots.toml` (scope 3). Empty until upstreams land.
    pub upstream: &'a SettingValues,
    pub profile: &'a SettingValues,
    /// Display name of the profile file, `machines/<name>.toml`.
    pub profile_file: String,
    pub machine: &'a SettingValues,
    /// Display name of the local file, `machine.toml`.
    pub machine_file: String,
    /// Evaluates the `when` condition of a setting (without settings, which would be circular).
    pub when: &'a dyn Fn(&str) -> Result<bool, String>,
}

static NO_VALUES: SettingValues = BTreeMap::new();

impl<'a> Inputs<'a> {
    /// The usual wiring: the machine's selected bundles, profile and local settings.
    pub fn new(
        repo: &'a Repo,
        machine: &'a MachineConfig,
        when: &'a dyn Fn(&str) -> Result<bool, String>,
    ) -> Self {
        Inputs {
            repo,
            bundles: &machine.bundles,
            upstream: &NO_VALUES,
            profile: &machine.profile_settings,
            profile_file: machine.profile.as_ref().map_or_else(
                || "machines/<profile>.toml".to_string(),
                |p| format!("machines/{p}.toml"),
            ),
            machine: &machine.local_settings,
            machine_file: "machine.toml".to_string(),
            when,
        }
    }
}

/// All settings of a repo for one machine.
#[derive(Debug, Clone, Default)]
pub struct Settings {
    settings: BTreeMap<(String, String), Setting>,
    /// Invalid values, bundle conflicts, secrets outside the machine scope.
    pub errors: Vec<Diagnostic>,
    /// Values for settings nobody declares.
    pub warnings: Vec<Diagnostic>,
}

/// A value to show someone: for a setting, the answer to ask for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Why {
    Required,
    Ask,
}

impl Settings {
    pub fn get(&self, package: &str, key: &str) -> Option<&Setting> {
        self.settings.get(&(package.to_string(), key.to_string()))
    }

    /// Look up `<pkg>.<key>`.
    pub fn find(&self, id: &str) -> Option<&Setting> {
        let (p, k) = split_id(id)?;
        self.get(p, k)
    }

    pub fn iter(&self) -> impl Iterator<Item = &Setting> {
        self.settings.values()
    }

    pub fn of_package<'a>(&'a self, package: &'a str) -> impl Iterator<Item = &'a Setting> {
        self.settings.values().filter(move |s| s.package == package)
    }

    /// Required settings of `package` that have no value.
    pub fn missing<'a>(&'a self, package: &'a str) -> Vec<&'a Setting> {
        self.of_package(package).filter(|s| s.missing()).collect()
    }

    /// The settings a package's files used, with the value as it may be printed (secrets
    /// redacted): what the generated-file header names.
    pub fn uses(&self, package: &str) -> Vec<Use> {
        self.of_package(package)
            .filter(|s| s.applies)
            .filter_map(|s| {
                Some(Use {
                    name: s.key.clone(),
                    value: s.shown()?,
                    scope: s.scope()?,
                    secret: s.secret(),
                })
            })
            .collect()
    }

    /// Template variables of one package: `setting` (its own, by key) and `settings`
    /// (every package's, `settings.<pkg>.<key>`). Real values, secrets included: this is
    /// what gets substituted into files.
    pub fn vars(&self, package: &str) -> Value {
        let mut own = Map::new();
        let mut all: Map<String, Value> = Map::new();
        for s in self.settings.values().filter(|s| s.applies) {
            let Some(v) = s.value() else { continue };
            if s.package == package {
                own.insert(s.key.clone(), v.clone());
            }
            all.entry(s.package.clone())
                .or_insert_with(|| Value::Object(Map::new()))
                .as_object_mut()
                .expect("object")
                .insert(s.key.clone(), v.clone());
        }
        let mut m = Map::new();
        m.insert("setting".into(), Value::Object(own));
        m.insert("settings".into(), Value::Object(all));
        Value::Object(m)
    }

    /// Settings the user still has to answer, for `packages`: required ones without a value,
    /// then `ask` ones that only have their default. Sorted by id.
    pub fn pending(&self, packages: &BTreeSet<String>) -> Vec<(&Setting, Why)> {
        self.settings
            .values()
            .filter(|s| s.applies && packages.contains(&s.package))
            .filter_map(|s| {
                if s.missing() {
                    Some((s, Why::Required))
                } else if s.decl.ask && !s.answered() {
                    Some((s, Why::Ask))
                } else {
                    None
                }
            })
            .collect()
    }

    /// A [`SettingLookup`] for conditions inside `package` (`setting.x` is its own).
    pub fn lookup<'a>(&'a self, package: &'a str) -> PackageLookup<'a> {
        PackageLookup {
            settings: self,
            own: package,
        }
    }
}

/// One setting used by a package's files.
#[derive(Debug, Clone, PartialEq)]
pub struct Use {
    pub name: String,
    /// Redacted for secrets.
    pub value: Value,
    pub scope: SettingScope,
    pub secret: bool,
}

/// Conditions (`when = 'setting.theme = "light"'`) evaluate against this.
pub struct PackageLookup<'a> {
    settings: &'a Settings,
    own: &'a str,
}

impl SettingLookup for PackageLookup<'_> {
    fn resolves(&self, package: Option<&str>, key: &str) -> bool {
        self.settings
            .get(package.unwrap_or(self.own), key)
            .is_some_and(|s| s.applies)
    }

    fn value(&self, package: Option<&str>, key: &str) -> Option<dots_facts::Value> {
        use dots_facts::Value as C;
        let s = self.settings.get(package.unwrap_or(self.own), key)?;
        Some(match s.applies.then(|| s.value()).flatten()? {
            Value::Bool(b) => C::Bool(*b),
            Value::Number(n) => C::Num(n.as_f64()?),
            Value::String(t) => C::Str(t.clone()),
            // Conditions compare scalars; a list compares as its comma-joined items.
            Value::Array(items) => C::Str(
                items
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(","),
            ),
            _ => return None,
        })
    }
}

/// `ghostty.font_size` into `("ghostty", "font_size")`. The key may not contain a dot.
pub fn split_id(id: &str) -> Option<(&str, &str)> {
    let (p, k) = id.split_once('.')?;
    (!p.is_empty() && !k.is_empty() && !k.contains('.')).then_some((p, k))
}

/// The bundles reachable from `names` through `include`, each once, in visit order.
pub fn bundle_closure(repo: &Repo, names: &[String]) -> Vec<String> {
    fn walk(repo: &Repo, name: &str, out: &mut Vec<String>) {
        let Some(b) = repo.bundles.get(name) else {
            return;
        };
        if out.iter().any(|o| o == name) {
            return;
        }
        out.push(name.to_string());
        for i in &b.include {
            walk(repo, i, out);
        }
    }
    let mut out = Vec::new();
    for n in names {
        walk(repo, n, &mut out);
    }
    out
}

/// Packages the bundles list, through `include`, closed over `requires` (apply installs those
/// too, so their settings matter). Conditions are not evaluated: a conditional package may
/// still need its answers.
pub fn packages_of(repo: &Repo, bundles: &[String]) -> BTreeSet<String> {
    use dots_model::BundleMember;
    let mut pkgs = BTreeSet::new();
    for b in bundle_closure(repo, bundles) {
        for m in &repo.bundles[&b].packages {
            match m {
                BundleMember::Name(n) | BundleMember::Conditional { name: n, .. } => {
                    pkgs.insert(n.clone());
                }
            }
        }
    }
    let mut queue: Vec<String> = pkgs.iter().cloned().collect();
    while let Some(n) = queue.pop() {
        let Some(pkg) = repo.packages.get(&n) else {
            continue;
        };
        for r in &pkg.requires {
            if pkgs.insert(r.name().to_string()) {
                queue.push(r.name().to_string());
            }
        }
    }
    pkgs
}

fn scope_of(scope: Scope) -> SettingScope {
    match scope {
        Scope::Definition | Scope::Repo => SettingScope::Repo,
        Scope::Profile => SettingScope::Profile,
        Scope::Machine => SettingScope::Machine,
    }
}

fn lookup_value<'a>(values: &'a SettingValues, pkg: &str, key: &str) -> Option<&'a Value> {
    values.get(pkg)?.get(key)
}

/// The repo-scope layers of one setting: `dots.toml`, plus anything an override changed.
fn repo_layers(repo: &Repo, pkg: &str, key: &str) -> Vec<Layer> {
    let Some(effective) = lookup_value(&repo.manifest.settings, pkg, key) else {
        return Vec::new();
    };
    let spans = &repo.manifest.spans;
    let line = spans.get(&format!("settings.{pkg}.{key}")).map(|(l, _)| *l);
    let dots_toml = |value: Value, line| Layer {
        scope: SettingScope::Repo,
        file: "dots.toml".into(),
        line,
        value,
        error: None,
    };
    let path = format!("settings.{pkg}.{key}");
    let info = repo
        .effective
        .as_ref()
        .and_then(|e| e.objects.get("repo:manifest"));
    let Some(obj) = info else {
        return vec![dots_toml(effective.clone(), line)];
    };
    let Some(loc) = obj.prov.get(&path) else {
        return vec![dots_toml(effective.clone(), line)];
    };
    let scope = scope_of(loc.source.scope);
    let mut out = Vec::new();
    // What dots.toml itself said is still a layer of the repo scope, unless the override
    // replaced it at that very scope.
    if let Some(orig) = obj
        .definition
        .get("settings")
        .and_then(|s| s.get(pkg))
        .and_then(|p| p.get(key))
        && scope != SettingScope::Repo
    {
        out.push(dots_toml(orig.clone(), line));
    }
    out.push(Layer {
        scope,
        file: loc.source.file.clone(),
        line: loc.line,
        value: effective.clone(),
        error: None,
    });
    out
}

/// Resolve every declared setting of `inputs.repo`.
pub fn resolve(inputs: &Inputs<'_>) -> Settings {
    let repo = inputs.repo;
    let mut out = Settings::default();
    let bundles = bundle_closure(repo, inputs.bundles);

    // Stale values: set somewhere, declared nowhere. Only worth a warning.
    let declared = |p: &str, k: &str| {
        repo.packages
            .get(p)
            .is_some_and(|pk| pk.settings.contains_key(k))
    };
    let mut warn_stale = |values: &SettingValues, file: &str| {
        for (p, kv) in values {
            for k in kv.keys() {
                if !declared(p, k) {
                    out.warnings.push(Diagnostic::from_code(
                        "setting-stale",
                        &[("setting", &format!("{p}.{k}")), ("origin", file)],
                    ));
                }
            }
        }
    };
    warn_stale(&repo.manifest.settings, "dots.toml");
    for b in &bundles {
        warn_stale(&repo.bundles[b].settings, &format!("bundles/{b}.toml"));
    }
    warn_stale(inputs.upstream, "upstream");
    warn_stale(inputs.profile, &inputs.profile_file);
    warn_stale(inputs.machine, &inputs.machine_file);

    for (pname, pkg) in &repo.packages {
        for (key, decl) in &pkg.settings {
            let id = format!("{pname}.{key}");
            let applies = match &decl.when {
                None => true,
                Some(w) => match (inputs.when)(w) {
                    Ok(b) => b,
                    Err(e) => {
                        out.errors.push(Diagnostic::from_code(
                            "setting-invalid",
                            &[
                                ("setting", &id),
                                ("origin", &format!("packages/{pname}/package.toml")),
                                ("detail", &format!("`when` cannot be evaluated: {e}")),
                            ],
                        ));
                        true
                    }
                },
            };
            let mut layers: Vec<Layer> = Vec::new();
            if let Some(d) = &decl.default {
                layers.push(Layer {
                    scope: SettingScope::Default,
                    file: format!("packages/{pname}/package.toml"),
                    line: pkg
                        .spans
                        .get(&format!("settings.{key}.default"))
                        .map(|l| l.0),
                    value: d.clone(),
                    error: None,
                });
            }
            for b in &bundles {
                let bundle = &repo.bundles[b];
                if let Some(v) = lookup_value(&bundle.settings, pname, key) {
                    layers.push(Layer {
                        scope: SettingScope::Bundle,
                        file: b.clone(),
                        line: bundle
                            .spans
                            .get(&format!("settings.{pname}.{key}"))
                            .map(|l| l.0),
                        value: v.clone(),
                        error: None,
                    });
                }
            }
            if let Some(v) = lookup_value(inputs.upstream, pname, key) {
                layers.push(Layer {
                    scope: SettingScope::Upstream,
                    file: "upstream".into(),
                    line: None,
                    value: v.clone(),
                    error: None,
                });
            }
            layers.extend(repo_layers(repo, pname, key));
            if let Some(v) = lookup_value(inputs.profile, pname, key) {
                layers.push(Layer {
                    scope: SettingScope::Profile,
                    file: inputs.profile_file.clone(),
                    line: None,
                    value: v.clone(),
                    error: None,
                });
            }
            if let Some(v) = lookup_value(inputs.machine, pname, key) {
                layers.push(Layer {
                    scope: SettingScope::Machine,
                    file: inputs.machine_file.clone(),
                    line: None,
                    value: v.clone(),
                    error: None,
                });
            }
            layers.sort_by_key(|l| l.scope); // stable: bundles keep their visit order

            // Validate every value; a secret is only allowed at the machine scope.
            for l in layers
                .iter_mut()
                .filter(|l| l.scope != SettingScope::Default)
            {
                if decl.secret && l.scope != SettingScope::Machine {
                    out.errors.push(Diagnostic::from_code(
                        "setting-secret-scope",
                        &[("setting", &id), ("origin", &l.file)],
                    ));
                    l.error = Some("secret outside the machine scope".into());
                    continue;
                }
                if let Err(detail) = decl.check_value(&l.value) {
                    let detail = match l.line {
                        Some(n) => format!("{detail} (line {n})"),
                        None => detail,
                    };
                    out.errors.push(Diagnostic::from_code(
                        "setting-invalid",
                        &[
                            ("setting", &id),
                            ("origin", &l.origin()),
                            ("detail", &detail),
                        ],
                    ));
                    l.error = Some(detail);
                }
            }

            // Two bundles that disagree are an error, unless a higher scope settles it.
            let higher = layers
                .iter()
                .any(|l| l.valid() && l.scope > SettingScope::Bundle);
            let bundle_layers: Vec<usize> = layers
                .iter()
                .enumerate()
                .filter(|(_, l)| l.scope == SettingScope::Bundle && l.valid())
                .map(|(i, _)| i)
                .collect();
            let mut conflicted = false;
            if !higher
                && let Some(&first) = bundle_layers.first()
                && let Some(&other) = bundle_layers
                    .iter()
                    .find(|&&i| layers[i].value != layers[first].value)
            {
                conflicted = true;
                out.errors.push(Diagnostic::from_code(
                    "setting-conflict",
                    &[
                        ("setting", &id),
                        ("left", &format!("bundle `{}`", layers[first].file)),
                        ("right", &format!("bundle `{}`", layers[other].file)),
                    ],
                ));
            }
            let winner = layers
                .iter()
                .enumerate()
                .rev()
                .find(|(_, l)| l.valid() && !(conflicted && l.scope == SettingScope::Bundle))
                .map(|(i, _)| i);
            out.settings.insert(
                (pname.clone(), key.clone()),
                Setting {
                    package: pname.clone(),
                    key: key.clone(),
                    decl: decl.clone(),
                    applies,
                    layers,
                    winner,
                },
            );
        }
    }
    out
}
