//! `dots adopt` for a setting: write the observed value back into the file that set it.
//!
//! The effective model knows which layer set the value ([`locate`]): the package's own file,
//! one of its `[platform."<key>"]` tables, or a repo, profile or machine override. That is the
//! file [`adopt_in`] edits, so the value that wins is the one that changes.

use std::path::{Path, PathBuf};

use dots_model::{EffectiveInfo, Scope};
use serde_json::Value;
use toml_edit::{DocumentMut, Item, TableLike, value};

use crate::model::Setting;

fn to_toml(v: &Value) -> Result<toml_edit::Value, String> {
    Ok(match v {
        Value::Bool(b) => (*b).into(),
        Value::Number(n) if n.is_f64() => n.as_f64().unwrap_or(0.0).into(),
        Value::Number(n) => n.as_i64().ok_or("integer out of range")?.into(),
        Value::String(s) => s.as_str().into(),
        Value::Array(a) => {
            let mut arr = toml_edit::Array::new();
            for x in a {
                arr.push(to_toml(x)?);
            }
            arr.into()
        }
        Value::Object(m) => {
            let mut t = toml_edit::InlineTable::new();
            for (k, x) in m {
                t.insert(k, to_toml(x)?);
            }
            t.into()
        }
        Value::Null => return Err("null cannot be written to TOML".into()),
    })
}

fn str_of(t: &dyn TableLike, k: &str) -> Option<String> {
    t.get(k).and_then(Item::as_str).map(str::to_string)
}

/// Where an adopted value is written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub file: PathBuf,
    /// The `[platform."<key>"]` table inside the file, when the value came from one.
    pub platform: Option<String>,
}

impl Setting {
    /// The top-level array the entry lives in, and the key an entry of it has in the merged
    /// model (`defaults.<domain>|<key>`).
    fn entry_key(&self) -> (&'static str, String, &'static str) {
        match self {
            Setting::Defaults { domain, key, .. } => {
                ("defaults", format!("{domain}|{key}"), "value")
            }
            Setting::Registry { key, name, .. } => ("registry", format!("{key}|{name}"), "value"),
            Setting::XdgMime { mime, .. } => ("xdg", mime.clone(), "app"),
            Setting::XdgSetting { setting, .. } => ("xdg", setting.clone(), "value"),
        }
    }
}

/// The file and platform table that set `setting`'s value in the effective model of
/// `package`: the highest layer that touched the value, else the package's own file.
pub fn locate(
    info: &EffectiveInfo,
    package: &str,
    package_dir: &Path,
    setting: &Setting,
) -> Target {
    let own = Target {
        file: package_dir.join("package.toml"),
        platform: None,
    };
    let Some(obj) = info.objects.get(&format!("pkg:{package}")) else {
        return own;
    };
    let (array, key, field) = setting.entry_key();
    let path = format!("{array}.{key}.{field}");
    let mime = match setting {
        Setting::XdgMime { mime, .. } => Some(mime.as_str()),
        _ => None,
    };
    let best = obj
        .prov
        .entries()
        .filter(|(k, _)| match mime {
            // `xdg.<a,b>.app`: one entry covers a list of mime types.
            Some(m) => k
                .strip_prefix("xdg.")
                .and_then(|r| r.strip_suffix(".app"))
                .is_some_and(|ks| ks.split(',').any(|x| x == m)),
            None => *k == path || k.strip_prefix(&path).is_some_and(|r| r.starts_with('.')),
        })
        .map(|(_, l)| l.source)
        .max_by_key(|s| (s.scope, s.platform.is_some()));
    let Some(src) = best else { return own };
    let file = match src.scope {
        Scope::Definition => package_dir.join("package.toml"),
        _ => match info
            .overrides_of(&format!("pkg:{package}"))
            .find(|o| o.scope == src.scope && o.file == src.file)
        {
            Some(o) => o.path.clone(),
            None => return own,
        },
    };
    Target {
        file,
        platform: src.platform.clone(),
    }
}

type Matcher = Box<dyn Fn(&dyn TableLike) -> bool>;

/// Where an entry sits inside the array (or keyed table) that holds it.
enum Loc {
    /// `defaults = { "<key>" = { ... } }`.
    Key(String),
    /// The n-th entry of `[[defaults]]`.
    Index(usize),
    /// The n-th entry of `add` / `prepend` inside the keyed table form.
    Listed(&'static str, usize),
}

fn nth(item: &Item, i: usize) -> Option<&dyn TableLike> {
    match item {
        Item::ArrayOfTables(a) => a.get(i).map(|t| t as &dyn TableLike),
        Item::Value(toml_edit::Value::Array(a)) => a
            .get(i)
            .and_then(toml_edit::Value::as_inline_table)
            .map(|t| t as &dyn TableLike),
        _ => None,
    }
}

fn count(item: &Item) -> usize {
    match item {
        Item::ArrayOfTables(a) => a.len(),
        Item::Value(toml_edit::Value::Array(a)) => a.len(),
        _ => 0,
    }
}

fn find(item: &Item, key: &str, matches: &dyn Fn(&dyn TableLike) -> bool) -> Option<Loc> {
    if let Some(t) = item.as_table_like() {
        if t.get(key).is_some_and(|e| e.as_table_like().is_some()) {
            return Some(Loc::Key(key.to_string()));
        }
        for list in ["add", "prepend"] {
            let Some(l) = t.get(list) else { continue };
            if let Some(i) = (0..count(l)).find(|i| nth(l, *i).is_some_and(matches)) {
                return Some(Loc::Listed(list, i));
            }
        }
        return None;
    }
    (0..count(item))
        .find(|i| nth(item, *i).is_some_and(matches))
        .map(Loc::Index)
}

fn nth_mut(item: &mut Item, i: usize) -> Option<&mut dyn TableLike> {
    match item {
        Item::ArrayOfTables(a) => a.get_mut(i).map(|t| t as &mut dyn TableLike),
        Item::Value(toml_edit::Value::Array(a)) => a
            .get_mut(i)
            .and_then(toml_edit::Value::as_inline_table_mut)
            .map(|t| t as &mut dyn TableLike),
        _ => None,
    }
}

fn entry_mut<'a>(item: &'a mut Item, loc: &Loc) -> Option<&'a mut dyn TableLike> {
    match loc {
        Loc::Index(i) => nth_mut(item, *i),
        Loc::Key(k) => item.as_table_like_mut()?.get_mut(k)?.as_table_like_mut(),
        Loc::Listed(list, i) => nth_mut(item.as_table_like_mut()?.get_mut(list)?, *i),
    }
}

/// Set the declared value of `setting` in `package_toml` to `observed` (the value as read, in
/// the form [`Setting::wanted`] uses).
pub fn adopt(package_toml: &Path, setting: &Setting, observed: &Value) -> Result<(), String> {
    adopt_in(
        &Target {
            file: package_toml.to_path_buf(),
            platform: None,
        },
        setting,
        observed,
    )
}

/// [`adopt`] in the file and platform table `at` ([`locate`]).
pub fn adopt_in(at: &Target, setting: &Setting, observed: &Value) -> Result<(), String> {
    let package_toml = &at.file;
    let text = std::fs::read_to_string(package_toml).map_err(|e| e.to_string())?;
    let mut doc: DocumentMut = text.parse().map_err(|e| format!("{e}"))?;
    let (array, key, _) = setting.entry_key();
    let matches: Matcher = match setting {
        Setting::Defaults { domain, key, .. } => {
            let (d, k) = (domain.clone(), key.clone());
            Box::new(move |t| {
                str_of(t, "domain").as_deref() == Some(&d)
                    && str_of(t, "key").as_deref() == Some(&k)
            })
        }
        Setting::Registry { key, name, .. } => {
            let (kk, n) = (key.clone(), name.clone());
            Box::new(move |t| {
                str_of(t, "key").as_deref() == Some(&kk) && str_of(t, "name").as_deref() == Some(&n)
            })
        }
        Setting::XdgMime { mime, .. } => {
            let m = mime.clone();
            Box::new(move |t| {
                t.get("mime")
                    .and_then(Item::as_array)
                    .is_some_and(|a| a.iter().any(|x| x.as_str() == Some(&m)))
            })
        }
        Setting::XdgSetting { setting, .. } => {
            let s = setting.clone();
            Box::new(move |t| str_of(t, "setting").as_deref() == Some(&s))
        }
    };
    let place = match &at.platform {
        Some(k) => format!("[platform.\"{k}\"]"),
        None => String::new(),
    };
    let root: &mut dyn TableLike = match &at.platform {
        None => doc.as_table_mut(),
        Some(k) => doc
            .get_mut("platform")
            .and_then(|p| p.as_table_like_mut())
            .and_then(|p| p.get_mut(k))
            .and_then(Item::as_table_like_mut)
            .ok_or_else(|| format!("{} has no {place} table", package_toml.display()))?,
    };
    let list = root.get_mut(array).ok_or_else(|| {
        format!(
            "{} has no [[{array}]] entries{}",
            package_toml.display(),
            if place.is_empty() {
                String::new()
            } else {
                format!(" in {place}")
            }
        )
    })?;
    let missing = || {
        format!(
            "no [[{array}]] entry for {} in {}",
            setting.label(),
            package_toml.display()
        )
    };
    let loc = find(list, &key, &*matches).ok_or_else(missing)?;
    let t = entry_mut(list, &loc).ok_or_else(missing)?;
    match setting {
        Setting::XdgMime { .. } => {
            let many = t
                .get("mime")
                .and_then(Item::as_array)
                .is_some_and(|a| a.len() > 1);
            if many {
                return Err(
                    "this [[xdg]] entry covers several mime types: split it before adopting".into(),
                );
            }
            t.insert("app", value(observed.as_str().ok_or("not a string")?));
        }
        Setting::XdgSetting { .. } => {
            t.insert("value", value(observed.as_str().ok_or("not a string")?));
        }
        Setting::Registry { ty, .. } => {
            let data = observed
                .get("data")
                .and_then(Value::as_str)
                .ok_or("not a registry value")?;
            let v = match ty.as_str() {
                "REG_DWORD" | "REG_QWORD" => match data.parse::<i64>() {
                    Ok(n) => value(n),
                    Err(_) => value(data),
                },
                _ => value(data),
            };
            t.insert("value", v);
        }
        Setting::Defaults { .. } => {
            t.insert("value", Item::Value(to_toml(observed)?));
        }
    }
    std::fs::write(package_toml, doc.to_string()).map_err(|e| e.to_string())
}
