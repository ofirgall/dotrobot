//! Name and condition resolution for directives and `[[files.set]]`: `setting.x`,
//! `settings.<pkg>.x`, `fact.<name>`, `tag.<name>` and `path.<var>`, plus the condition language.
//!
//! One [`Scope`] belongs to one file of one package. It records the settings it hands out, so
//! the generated header can list the values a render really used.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};

use dots_facts::{Chain, CondError, Condition, EvalCtx, Facts, SettingLookup, Value, suggest};
use serde_json::Value as Json;

use crate::model::{CondEnv, FilesCtx, SettingUse};

/// Why a name did not resolve.
pub(crate) struct UnknownName {
    /// As written, e.g. `setting.nope`.
    pub name: String,
    pub suggestion: Option<String>,
}

pub(crate) struct Scope<'a> {
    ctx: &'a FilesCtx,
    package: &'a str,
    /// Settings handed out so far, keyed by display name.
    used: RefCell<BTreeMap<String, SettingUse>>,
}

/// `Some(key)` when `name` belongs to `package` (`<package>.<key>`) or is unqualified.
fn own_key<'n>(name: &'n str, package: &str) -> Option<&'n str> {
    match name.strip_prefix(package).and_then(|r| r.strip_prefix('.')) {
        Some(k) => Some(k),
        None => (!name.contains('.')).then_some(name),
    }
}

fn to_cond(v: &Json) -> Value {
    match v {
        Json::String(s) => Value::Str(s.clone()),
        Json::Bool(b) => Value::Bool(*b),
        Json::Number(n) => Value::Num(n.as_f64().unwrap_or(0.0)),
        other => Value::Str(other.to_string()),
    }
}

impl<'a> Scope<'a> {
    pub fn new(ctx: &'a FilesCtx, package: &'a str) -> Self {
        Scope {
            ctx,
            package,
            used: RefCell::new(BTreeMap::new()),
        }
    }

    /// The settings looked up so far.
    pub fn used(&self) -> Vec<SettingUse> {
        self.used.borrow().values().cloned().collect()
    }

    /// Look a setting up (`package` `None` is this file's own package).
    fn setting(&self, package: Option<&str>, key: &str) -> Option<Json> {
        let own = package.is_none() || package == Some(self.package);
        let pkg = package.unwrap_or(self.package);
        let qualified = format!("{pkg}.{key}");
        let hit = self
            .ctx
            .settings
            .iter()
            .find(|s| s.name == qualified)
            .or_else(|| {
                own.then(|| self.ctx.settings.iter().find(|s| s.name == key))
                    .flatten()
            })?;
        let shown = if own { key.to_string() } else { qualified };
        self.used.borrow_mut().insert(
            shown.clone(),
            SettingUse {
                name: shown,
                ..hit.clone()
            },
        );
        Some(hit.value.clone())
    }

    fn setting_names(&self) -> Vec<String> {
        self.ctx
            .settings
            .iter()
            .filter_map(|s| own_key(&s.name, self.package).map(str::to_string))
            .collect()
    }

    fn tag_set(&self) -> (BTreeSet<String>, BTreeSet<String>) {
        if let Some(c) = &self.ctx.cond {
            return (c.tags.clone(), c.declared_tags.clone());
        }
        let tags: BTreeSet<String> = self
            .ctx
            .template_vars
            .get("tags")
            .and_then(Json::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|t| t.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        (tags.clone(), tags)
    }

    /// Evaluate a condition in the §4 language.
    pub fn eval_cond(&self, src: &str) -> Result<bool, CondError> {
        let fallback;
        let env: &CondEnv = match &self.ctx.cond {
            Some(e) => e,
            None => {
                let facts = Facts {
                    os: self.ctx.os.clone(),
                    ..Facts::default()
                };
                let chain = Chain::build(&facts, &BTreeMap::new(), &self.ctx.chain);
                let (tags, declared_tags) = self.tag_set();
                fallback = CondEnv {
                    facts,
                    chain,
                    tags,
                    declared_tags,
                };
                &fallback
            }
        };
        let cond = Condition::parse(src)?;
        cond.eval(&EvalCtx {
            facts: &env.facts,
            chain: &env.chain,
            tags: &env.tags,
            declared_tags: &env.declared_tags,
            settings: &Lookup(self),
        })
    }

    /// The value of a `@dots(expr)@` expression.
    pub fn value(&self, expr: &str) -> Result<Json, UnknownName> {
        let unknown = |suggestion: Option<String>| UnknownName {
            name: expr.to_string(),
            suggestion,
        };
        let (ns, rest) = expr.split_once('.').unwrap_or((expr, ""));
        match ns {
            "setting" if !rest.is_empty() => self.setting(None, rest).ok_or_else(|| {
                let names = self.setting_names();
                unknown(
                    suggest(rest, names.iter().map(String::as_str)).map(|s| format!("setting.{s}")),
                )
            }),
            "settings" => {
                let (pkg, key) = rest.split_once('.').unwrap_or(("", ""));
                if pkg.is_empty() || key.is_empty() {
                    return Err(unknown(None));
                }
                self.setting(Some(pkg), key).ok_or_else(|| unknown(None))
            }
            "fact" if !rest.is_empty() => {
                let from_env = self
                    .ctx
                    .cond
                    .as_ref()
                    .and_then(|c| c.facts.get(rest))
                    .map(|v| v.to_json());
                let from_vars = || self.ctx.template_vars.get("facts")?.get(rest).cloned();
                from_env.or_else(from_vars).ok_or_else(|| {
                    let mut names: Vec<String> = dots_facts::FACT_NAMES
                        .iter()
                        .map(|s| s.to_string())
                        .collect();
                    if let Some(m) = self
                        .ctx
                        .template_vars
                        .get("facts")
                        .and_then(Json::as_object)
                    {
                        names.extend(m.keys().cloned());
                    }
                    unknown(
                        suggest(rest, names.iter().map(String::as_str))
                            .map(|s| format!("fact.{s}")),
                    )
                })
            }
            "tag" if !rest.is_empty() => {
                let (tags, declared) = self.tag_set();
                if tags.contains(rest) {
                    Ok(Json::Bool(true))
                } else if self.ctx.cond.is_none() || declared.contains(rest) {
                    Ok(Json::Bool(false))
                } else {
                    Err(unknown(
                        suggest(rest, declared.iter().map(String::as_str))
                            .map(|s| format!("tag.{s}")),
                    ))
                }
            }
            "path" if !rest.is_empty() => {
                crate::pathvars::expand_path(&format!("{{{rest}}}"), self.ctx)
                    .map(|p| Json::String(p.display().to_string()))
                    .map_err(|_| {
                        let mut names: Vec<&str> =
                            vec!["home", "config", "data", "cache", "state", "app_support"];
                        names.extend(self.ctx.vars.keys().map(String::as_str));
                        unknown(suggest(rest, names).map(|s| format!("path.{s}")))
                    })
            }
            _ => Err(unknown(None)),
        }
    }
}

struct Lookup<'a, 'b>(&'a Scope<'b>);

impl SettingLookup for Lookup<'_, '_> {
    fn resolves(&self, package: Option<&str>, key: &str) -> bool {
        self.0.setting(package, key).is_some()
    }
    fn value(&self, package: Option<&str>, key: &str) -> Option<Value> {
        self.0.setting(package, key).map(|v| to_cond(&v))
    }
}
