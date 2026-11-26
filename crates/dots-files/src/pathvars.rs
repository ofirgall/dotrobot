//! Path variables, `~` and per-platform tables.

use std::collections::BTreeMap;
use std::path::PathBuf;

use dots_model::PlatformString;

use crate::model::FilesCtx;

/// Pick the value for the most specific chain key. `chain` is general to specific; `default`
/// is the fallback.
pub fn pick_platform<'a>(
    map: &'a BTreeMap<String, String>,
    chain: &[String],
) -> Option<&'a String> {
    chain
        .iter()
        .rev()
        .find_map(|k| map.get(k))
        .or_else(|| map.get("default"))
}

/// Resolve `[paths]` tables for a chain into plain strings (still unexpanded).
pub fn resolve_path_vars(
    paths: &BTreeMap<String, PlatformString>,
    chain: &[String],
) -> BTreeMap<String, String> {
    paths
        .iter()
        .filter_map(|(k, v)| {
            let s = match v {
                PlatformString::Plain(s) => Some(s.clone()),
                PlatformString::PerPlatform(m) => pick_platform(m, chain).cloned(),
            };
            s.map(|s| (k.clone(), s))
        })
        .collect()
}

fn builtin(name: &str, ctx: &FilesCtx) -> Option<String> {
    let h = ctx.home.display().to_string();
    Some(match name {
        "home" => h,
        "config" => format!("{h}/.config"),
        "data" => format!("{h}/.local/share"),
        "cache" => format!("{h}/.cache"),
        "state" => format!("{h}/.local/state"),
        "app_support" => match ctx.os.as_str() {
            "macos" => format!("{h}/Library/Application Support"),
            "windows" => format!("{h}/AppData/Roaming"),
            _ => format!("{h}/.config"),
        },
        _ => return None,
    })
}

/// Expand `{var}`, `%VAR%` and a leading `~`. Err carries the unknown variable name.
pub fn expand_path(s: &str, ctx: &FilesCtx) -> Result<PathBuf, String> {
    let mut cur = s.to_string();
    for _ in 0..8 {
        let next = expand_once(&cur, ctx)?;
        if next == cur {
            break;
        }
        cur = next;
    }
    if cur == "~" {
        return Ok(ctx.home.clone());
    }
    if let Some(rest) = cur.strip_prefix("~/") {
        return Ok(ctx.home.join(rest));
    }
    Ok(PathBuf::from(cur))
}

fn expand_once(s: &str, ctx: &FilesCtx) -> Result<String, String> {
    let mut out = String::new();
    let mut rest = s;
    while let Some(i) = rest.find(['{', '%']) {
        out.push_str(&rest[..i]);
        let open = rest.as_bytes()[i];
        let close = if open == b'{' { '}' } else { '%' };
        let after = &rest[i + 1..];
        match after.find(close) {
            Some(j) if !after[..j].is_empty() && !after[..j].contains(char::is_whitespace) => {
                let name = &after[..j];
                let val = ctx
                    .vars
                    .get(name)
                    .cloned()
                    .or_else(|| builtin(name, ctx))
                    .or_else(|| {
                        (open == b'%')
                            .then(|| ctx.vars.get(&name.to_lowercase()).cloned())
                            .flatten()
                    });
                match val {
                    Some(v) => out.push_str(&v),
                    None if open == b'%' => out.push_str(&rest[i..i + j + 2]),
                    None => return Err(name.to_string()),
                }
                rest = &after[j + 1..];
            }
            _ => {
                out.push(open as char);
                rest = after;
            }
        }
    }
    out.push_str(rest);
    // `~` only expands at the start, after variables are substituted.
    Ok(out)
}
