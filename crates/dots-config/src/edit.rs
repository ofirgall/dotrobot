//! Writing one setting into `dots.toml` or `machine.toml` with `toml_edit`, so comments,
//! ordering and the rest of the file stay as they were.

use std::path::Path;

use serde_json::Value;
use toml_edit::{Array, DocumentMut, InlineTable, Item, Table, value};

fn to_toml(v: &Value) -> Result<toml_edit::Value, String> {
    Ok(match v {
        Value::Bool(b) => (*b).into(),
        Value::Number(n) => match n.as_i64() {
            Some(i) => i.into(),
            None => n.as_f64().ok_or("number out of range")?.into(),
        },
        Value::String(s) => s.as_str().into(),
        Value::Array(items) => {
            let mut a = Array::new();
            for i in items {
                a.push(to_toml(i)?);
            }
            a.into()
        }
        Value::Object(m) => {
            let mut t = InlineTable::new();
            for (k, i) in m {
                t.insert(k, to_toml(i)?);
            }
            t.into()
        }
        Value::Null => return Err("null has no TOML form".into()),
    })
}

fn read(path: &Path, default: &str) -> Result<DocumentMut, String> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => default.to_string(),
        Err(e) => return Err(e.to_string()),
    };
    text.parse::<DocumentMut>().map_err(|e| e.to_string())
}

/// Write `text` to `path` atomically (temp file in the same directory, then rename).
///
/// The temp file is created with the original's permission bits, or `0600` when `private`
/// (the content holds a secret), so there is never a moment when it is more readable than the
/// file it replaces. A symlink is resolved first: the target is replaced, the link stays.
pub fn write_atomic(path: &Path, text: &str, private: bool) -> Result<(), String> {
    use std::io::Write;
    let target = resolve_link(path);
    if let Some(dir) = target.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let mode = file_mode(&target, private);
    let name = target
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let tmp = target.with_file_name(format!(".{name}.dots-tmp-{}", std::process::id()));
    let _ = std::fs::remove_file(&tmp);
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(mode);
    }
    let _ = mode;
    let res = opts
        .open(&tmp)
        .and_then(|mut f| f.write_all(text.as_bytes()).and_then(|()| f.sync_all()))
        .and_then(|()| std::fs::rename(&tmp, &target));
    res.map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        e.to_string()
    })
}

/// Follow symlinks (also dangling ones) to the file that really holds the content.
fn resolve_link(path: &Path) -> std::path::PathBuf {
    let mut p = path.to_path_buf();
    for _ in 0..32 {
        match std::fs::symlink_metadata(&p) {
            Ok(m) if m.file_type().is_symlink() => match std::fs::read_link(&p) {
                Ok(t) => {
                    p = match p.parent() {
                        Some(d) => d.join(t),
                        None => t,
                    }
                }
                Err(_) => break,
            },
            _ => break,
        }
    }
    p
}

#[cfg(unix)]
fn file_mode(target: &Path, private: bool) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    if private {
        return 0o600;
    }
    std::fs::metadata(target)
        .map(|m| m.permissions().mode() & 0o7777)
        .unwrap_or(0o644)
}

#[cfg(not(unix))]
fn file_mode(_: &Path, _: bool) -> u32 {
    0o644
}

fn write(path: &Path, doc: &DocumentMut, private: bool) -> Result<(), String> {
    write_atomic(path, &doc.to_string(), private)
}

/// Set `[settings.<pkg>] <key> = value` in `path`, creating the file (with `schema = 1`) and
/// the tables when needed (`secret` makes the file `0600`). Everything else in the file is left untouched.
pub fn set_in_file(
    path: &Path,
    pkg: &str,
    key: &str,
    v: &Value,
    secret: bool,
) -> Result<(), String> {
    let mut doc = read(path, "schema = 1\n")?;
    let root = doc.as_table_mut();
    if root.get("settings").is_none() {
        let mut t = Table::new();
        t.set_implicit(true);
        root.insert("settings", Item::Table(t));
    }
    let settings = root["settings"]
        .as_table_like_mut()
        .ok_or("`settings` is not a table")?;
    if settings.get(pkg).is_none() {
        settings.insert(pkg, Item::Table(Table::new()));
    }
    let table = settings
        .get_mut(pkg)
        .and_then(Item::as_table_like_mut)
        .ok_or_else(|| format!("`settings.{pkg}` is not a table"))?;
    let new = to_toml(v)?;
    // Keep the key's comments and spacing when it already exists.
    match table.get_mut(key) {
        Some(Item::Value(old)) => {
            let decor = old.decor().clone();
            let mut n = new;
            *n.decor_mut() = decor;
            *old = n;
        }
        _ => {
            table.insert(key, value(new));
        }
    }
    write(path, &doc, secret)
}

/// Remove `[settings.<pkg>] <key>` from `path`. Returns whether it was there; empty tables
/// left behind are removed too.
pub fn unset_in_file(path: &Path, pkg: &str, key: &str) -> Result<bool, String> {
    if !path.exists() {
        return Ok(false);
    }
    let mut doc = read(path, "")?;
    let Some(settings) = doc.get_mut("settings").and_then(Item::as_table_like_mut) else {
        return Ok(false);
    };
    let Some(table) = settings.get_mut(pkg).and_then(Item::as_table_like_mut) else {
        return Ok(false);
    };
    if table.remove(key).is_none() {
        return Ok(false);
    }
    if table.is_empty() {
        settings.remove(pkg);
    }
    if settings.is_empty() {
        doc.remove("settings");
    }
    write(path, &doc, false)?;
    Ok(true)
}
