//! What a package declares, in the shape the planner works with.

use dots_model::Package;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// One declared (or recorded) system setting.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum Setting {
    /// A macOS `defaults` value. A dotted `key` edits one entry of a dict.
    Defaults {
        domain: String,
        key: String,
        value: Value,
        #[serde(default)]
        root: bool,
        #[serde(default)]
        restart: Vec<String>,
        #[serde(default)]
        requires: Vec<String>,
    },
    /// A Windows registry value.
    Registry {
        key: String,
        name: String,
        ty: String,
        value: Value,
    },
    /// `xdg-mime default <app> <mime>`, one mime type.
    XdgMime { mime: String, app: String },
    /// `xdg-settings set <setting> <value>`.
    XdgSetting { setting: String, value: String },
}

/// A setting and the package that declares it.
#[derive(Debug, Clone, PartialEq)]
pub struct Declared {
    pub package: String,
    pub setting: Setting,
}

impl Setting {
    /// Stable identity, also the `dots adopt` argument.
    pub fn id(&self) -> String {
        match self {
            Setting::Defaults { domain, key, .. } => format!("defaults:{domain}:{key}"),
            Setting::Registry { key, name, .. } => format!("registry:{key}\\{name}"),
            Setting::XdgMime { mime, .. } => format!("xdg-mime:{mime}"),
            Setting::XdgSetting { setting, .. } => format!("xdg-setting:{setting}"),
        }
    }

    /// `NSGlobalDomain KeyRepeat`, as the plan names it.
    pub fn label(&self) -> String {
        match self {
            Setting::Defaults { domain, key, .. } => format!("{domain} {key}"),
            Setting::Registry { key, name, .. } => format!("{key}\\{name}"),
            Setting::XdgMime { mime, .. } => format!("default app for {mime}"),
            Setting::XdgSetting { setting, .. } => format!("xdg-settings {setting}"),
        }
    }

    pub fn kind(&self) -> &'static str {
        match self {
            Setting::Defaults { .. } => "defaults",
            Setting::Registry { .. } => "registry",
            Setting::XdgMime { .. } | Setting::XdgSetting { .. } => "xdg",
        }
    }

    pub fn root(&self) -> bool {
        matches!(self, Setting::Defaults { root: true, .. })
    }

    pub fn restart(&self) -> &[String] {
        match self {
            Setting::Defaults { restart, .. } => restart,
            _ => &[],
        }
    }

    pub fn requires(&self) -> &[String] {
        match self {
            Setting::Defaults { requires, .. } => requires,
            _ => &[],
        }
    }

    /// The wanted value in the form observations are compared in. Registry values are
    /// `{ "type", "data" }` with the data in a canonical text form.
    pub fn wanted(&self) -> Value {
        match self {
            Setting::Defaults { value, .. } => value.clone(),
            Setting::Registry { ty, value, .. } => {
                json!({ "type": ty, "data": crate::sys::reg_canon(ty, value) })
            }
            Setting::XdgMime { app, .. } => json!(app),
            Setting::XdgSetting { value, .. } => json!(value),
        }
    }

    /// A value of this setting as the user reads it (`6`, `"0"`, `gh.desktop`).
    pub fn show(&self, v: &Value) -> String {
        match (self, v) {
            (Setting::Registry { .. }, Value::Object(o)) => o
                .get("data")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            (_, Value::String(s)) => s.clone(),
            (_, v) => v.to_string(),
        }
    }
}

fn platform_of(kind: &str) -> &'static str {
    match kind {
        "defaults" => "macos",
        "registry" => "windows",
        _ => "linux",
    }
}

/// The settings that apply on this OS, and the inert ones as `(id, label)`.
pub type Split = (Vec<Declared>, Vec<(String, String)>);

/// Everything `pkg` declares, split into the settings that apply on `os` and the ones that
/// are inert there (`(id, label)`, shown as `n/a on this platform`).
pub fn declared(name: &str, pkg: &Package, os: &str) -> Result<Split, String> {
    let mut all: Vec<Setting> = Vec::new();
    for d in &pkg.defaults {
        if d.domain.is_empty() || d.key.is_empty() {
            return Err(format!(
                "`{name}`: a [[defaults]] entry needs `domain` and `key`"
            ));
        }
        let Some(value) = &d.value else {
            return Err(format!(
                "`{name}`: [[defaults]] {} {} has no `value`",
                d.domain, d.key
            ));
        };
        all.push(Setting::Defaults {
            domain: d.domain.clone(),
            key: d.key.clone(),
            value: value.clone(),
            root: d.root,
            restart: d.restart.clone(),
            requires: d.requires.clone(),
        });
    }
    for r in &pkg.registry {
        if r.key.is_empty() || r.name.is_empty() {
            return Err(format!(
                "`{name}`: a [[registry]] entry needs `key` and `name`"
            ));
        }
        let Some(value) = &r.value else {
            return Err(format!(
                "`{name}`: [[registry]] {}\\{} has no `value`",
                r.key, r.name
            ));
        };
        all.push(Setting::Registry {
            key: r.key.clone(),
            name: r.name.clone(),
            ty: r.ty.clone().unwrap_or_else(|| "REG_SZ".into()),
            value: value.clone(),
        });
    }
    for x in &pkg.xdg {
        match (&x.app, &x.setting, &x.value) {
            (Some(app), None, None) if !x.mime.is_empty() => {
                for m in &x.mime {
                    all.push(Setting::XdgMime {
                        mime: m.clone(),
                        app: app.clone(),
                    });
                }
            }
            (None, Some(s), Some(v)) if x.mime.is_empty() => all.push(Setting::XdgSetting {
                setting: s.clone(),
                value: v.clone(),
            }),
            _ => {
                return Err(format!(
                    "`{name}`: an [[xdg]] entry is either `mime` + `app`, or `setting` + `value`"
                ));
            }
        }
    }
    let mut active = Vec::new();
    let mut inert = Vec::new();
    for s in all {
        if platform_of(s.kind()) == os {
            active.push(Declared {
                package: name.to_string(),
                setting: s,
            });
        } else {
            inert.push((s.id(), s.label()));
        }
    }
    Ok((active, inert))
}
