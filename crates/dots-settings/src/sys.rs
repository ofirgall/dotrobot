//! The only code that talks to the OS: reads (`defaults export`, `reg query`, `xdg-mime query`,
//! `xdg-settings get`) and the commands that write. Everything goes through a [`Runner`], so
//! tests script the machine.

use std::collections::BTreeMap;
use std::path::PathBuf;

use dots_providers::{Cmd, Output, Runner};
use serde_json::{Map, Value, json};

use crate::model::Setting;
use crate::plist;

/// Where the system tools are. `DOTS_SYSTEM_TOOLS=<dir>` redirects all of them to `<dir>/<name>`
/// (sandboxed runs and tests); otherwise the well-known absolute paths are used.
#[derive(Debug, Clone, Default)]
pub struct Tools {
    dir: Option<PathBuf>,
}

impl Tools {
    pub fn new(dir: Option<PathBuf>) -> Self {
        Tools { dir }
    }

    pub fn from_env(env: &BTreeMap<String, String>) -> Self {
        Tools {
            dir: env
                .get("DOTS_SYSTEM_TOOLS")
                .filter(|v| !v.is_empty())
                .map(PathBuf::from),
        }
    }

    pub fn path(&self, name: &str) -> String {
        if let Some(d) = &self.dir {
            return d.join(name).to_string_lossy().into_owned();
        }
        match name {
            "defaults" | "killall" | "xdg-mime" | "xdg-settings" | "sudo" => {
                format!("/usr/bin/{name}")
            }
            "reg" => r"C:\Windows\System32\reg.exe".into(),
            other => other.into(),
        }
    }
}

/// How commands that need root get it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Sudo {
    /// The run's one prompt primed sudo: root commands go through `sudo -n`.
    Primed,
    /// dots already runs as root: root commands run as they are.
    Unneeded,
    /// There is no root: sudo was declined, failed, or was never asked for. A root command
    /// fails with a clear error instead of running as the user. Reads still try `sudo -n`
    /// (it never prompts), which works when a credential is cached.
    #[default]
    Unavailable,
}

/// Runner plus tool locations, and how root commands get root.
pub struct Sys<'a> {
    pub runner: &'a dyn Runner,
    pub tools: Tools,
    pub sudo: Sudo,
}

impl Sys<'_> {
    /// `sudo -n` as a command prefix, for the commands that read what only root can read.
    fn read_prefix(&self) -> Vec<String> {
        match self.sudo {
            Sudo::Unneeded => Vec::new(),
            Sudo::Primed | Sudo::Unavailable => {
                vec![self.tools.path("sudo"), "-n".to_string()]
            }
        }
    }
}

/// The key a domain's dict is kept under: root's preferences are a different plist from the
/// user's, even for the same domain name.
pub(crate) fn dkey(domain: &str, root: bool) -> String {
    if root {
        format!("root:{domain}")
    } else {
        domain.to_string()
    }
}

/// What a setting is right now.
#[derive(Debug, Clone, PartialEq)]
pub enum Cur {
    Absent,
    Value(Value),
}

impl Cur {
    pub fn value(&self) -> Option<&Value> {
        match self {
            Cur::Value(v) => Some(v),
            Cur::Absent => None,
        }
    }
}

/// A key split into the top-level key and the path of a dotted edit inside it.
#[derive(Debug, Clone, PartialEq)]
pub struct KeyPath {
    pub top: String,
    pub path: Vec<String>,
}

/// A key naming a top-level entry of the domain stays literal (`com.apple.x`). Otherwise
/// `a.b.c` is the entry `b.c` of the dict `a`, when `a` is a dict.
pub fn resolve_key(domain: &Map<String, Value>, key: &str) -> KeyPath {
    if !domain.contains_key(key)
        && let Some((first, rest)) = key.split_once('.')
        && domain.get(first).is_some_and(Value::is_object)
    {
        return KeyPath {
            top: first.to_string(),
            path: rest.split('.').map(str::to_string).collect(),
        };
    }
    KeyPath {
        top: key.to_string(),
        path: Vec::new(),
    }
}

pub fn get_path<'v>(top: &'v Value, path: &[String]) -> Option<&'v Value> {
    path.iter().try_fold(top, |v, k| v.get(k))
}

pub fn set_path(top: &mut Value, path: &[String], new: Value) {
    let Some((k, rest)) = path.split_first() else {
        *top = new;
        return;
    };
    if !top.is_object() {
        *top = Value::Object(Map::new());
    }
    let m = top.as_object_mut().expect("just made an object");
    let slot = m.entry(k.clone()).or_insert(Value::Null);
    set_path(slot, rest, new);
}

pub fn remove_path(top: &mut Value, path: &[String]) {
    match path {
        [] => {}
        [k] => {
            if let Some(m) = top.as_object_mut() {
                m.remove(k);
            }
        }
        [k, rest @ ..] => {
            if let Some(v) = top.get_mut(k) {
                remove_path(v, rest);
            }
        }
    }
}

/// Registry data in the one text form both sides are compared in.
pub fn reg_canon(ty: &str, v: &Value) -> String {
    let text = |v: &Value| match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    };
    match ty {
        "REG_DWORD" | "REG_QWORD" => {
            let s = text(v);
            let s = s.trim();
            let n = s
                .strip_prefix("0x")
                .or_else(|| s.strip_prefix("0X"))
                .map_or_else(
                    || s.parse::<u64>().ok(),
                    |h| u64::from_str_radix(h, 16).ok(),
                );
            n.map_or_else(|| s.to_string(), |n| n.to_string())
        }
        "REG_MULTI_SZ" => match v {
            Value::Array(a) => a.iter().map(text).collect::<Vec<_>>().join("\\0"),
            other => text(other),
        },
        "REG_BINARY" => match v {
            Value::Array(a) => a
                .iter()
                .filter_map(Value::as_u64)
                .map(|b| format!("{b:02x}"))
                .collect(),
            other => text(other).to_ascii_lowercase(),
        },
        _ => text(v),
    }
}

fn run(sys: &Sys, argv: &[String]) -> Result<Output, String> {
    let cmd = Cmd::from_argv(argv).ok_or("empty command")?;
    sys.runner.run(&cmd).map_err(|e| e.to_string())
}

fn line(argv: &[&str]) -> Vec<String> {
    argv.iter().map(|s| s.to_string()).collect()
}

/// Everything read for one plan, so each domain, key and mime type is read exactly once.
#[derive(Debug, Default)]
pub struct Observed {
    defaults: BTreeMap<String, Result<Map<String, Value>, String>>,
    registry: BTreeMap<String, Result<BTreeMap<String, Value>, String>>,
    xdg: BTreeMap<String, Result<Option<String>, String>>,
}

impl Observed {
    /// One `defaults export` per domain, one `reg query` per key, one `xdg-mime query` per
    /// mime type (and one `xdg-settings get` per setting).
    pub fn read<'s>(sys: &Sys, settings: impl IntoIterator<Item = &'s Setting>) -> Observed {
        let mut o = Observed::default();
        for s in settings {
            match s {
                Setting::Defaults { domain, root, .. } => {
                    o.defaults
                        .entry(dkey(domain, *root))
                        .or_insert_with(|| read_domain(sys, domain, *root));
                }
                Setting::Registry { key, .. } => {
                    o.registry
                        .entry(key.clone())
                        .or_insert_with(|| read_reg_key(sys, key));
                }
                Setting::XdgMime { mime, .. } => {
                    o.xdg.entry(s.id()).or_insert_with(|| {
                        read_xdg(sys, &["xdg-mime", "query", "default", mime.as_str()])
                    });
                }
                Setting::XdgSetting { setting, .. } => {
                    o.xdg.entry(s.id()).or_insert_with(|| {
                        read_xdg(sys, &["xdg-settings", "get", setting.as_str()])
                    });
                }
            }
        }
        o
    }

    /// The current value of `s`, or why it could not be read.
    pub fn get(&self, s: &Setting) -> Result<Cur, String> {
        match s {
            Setting::Defaults {
                domain, key, root, ..
            } => {
                let d = self
                    .defaults
                    .get(&dkey(domain, *root))
                    .ok_or("domain was not read")?;
                let d = d.as_ref().map_err(Clone::clone)?;
                let kp = resolve_key(d, key);
                Ok(d.get(&kp.top)
                    .and_then(|top| get_path(top, &kp.path))
                    .map_or(Cur::Absent, |v| Cur::Value(v.clone())))
            }
            Setting::Registry { key, name, .. } => {
                let k = self.registry.get(key).ok_or("key was not read")?;
                let k = k.as_ref().map_err(Clone::clone)?;
                Ok(k.get(&name.to_ascii_lowercase())
                    .map_or(Cur::Absent, |v| Cur::Value(v.clone())))
            }
            Setting::XdgMime { .. } | Setting::XdgSetting { .. } => {
                let r = self.xdg.get(&s.id()).ok_or("setting was not read")?;
                let r = r.as_ref().map_err(Clone::clone)?;
                Ok(r.as_ref().map_or(Cur::Absent, |v| Cur::Value(json!(v))))
            }
        }
    }

    /// The whole dict of a domain (a working copy for planning dotted writes).
    pub fn domain(&self, domain: &str, root: bool) -> Map<String, Value> {
        self.defaults
            .get(&dkey(domain, root))
            .and_then(|r| r.as_ref().ok())
            .cloned()
            .unwrap_or_default()
    }
}

/// The read error of a root entry that `sudo -n` refused (no cached credential). It is not a
/// failure of the setting: [`crate::status`] reports it as `Health::Unknown`.
pub const NEEDS_SUDO_READ: &str = "needs sudo to read";

/// The read error of an `xdg-mime` / `xdg-settings` entry whose tool cannot be started (it
/// is installed by a package of this very run). It is not a failure of the setting: the step
/// is deferred until the packages are in, and [`crate::status`] reports `Health::Unknown`.
pub const TOOL_MISSING: &str = "tool not installed";

/// Is `e` the [`TOOL_MISSING`] read error?
pub fn is_tool_missing(e: &str) -> bool {
    e.starts_with(TOOL_MISSING)
}

/// A root entry is read as root (`sudo -n defaults export`): what it is compared with, and
/// the original value recorded for it, is what root's preferences hold.
fn read_domain(sys: &Sys, domain: &str, root: bool) -> Result<Map<String, Value>, String> {
    let mut argv = if root { sys.read_prefix() } else { Vec::new() };
    argv.extend([
        sys.tools.path("defaults"),
        "export".into(),
        domain.into(),
        "-".into(),
    ]);
    let out = run(sys, &argv)?;
    if !out.success() {
        // sudo's own refusals start with `sudo:`; `defaults` never prints that.
        if root && sys.sudo != Sudo::Unneeded && out.stderr.trim_start().starts_with("sudo:") {
            return Err(NEEDS_SUDO_READ.to_string());
        }
        let who = if root { "as root " } else { "" };
        return Err(format!(
            "`defaults export {domain}` {who}failed: {}",
            out.stderr.trim()
        ));
    }
    match plist::parse(&out.stdout)? {
        Value::Object(m) => Ok(m),
        _ => Err(format!("`defaults export {domain}` did not return a dict")),
    }
}

fn read_xdg(sys: &Sys, argv: &[&str]) -> Result<Option<String>, String> {
    let mut a = line(argv);
    a[0] = sys.tools.path(argv[0]);
    // `run` only fails when the command cannot be started: the tool is not there (yet).
    let out = run(sys, &a).map_err(|e| format!("{TOOL_MISSING}: {e}"))?;
    if !out.success() {
        return Err(format!(
            "`{}` failed: {}",
            argv.join(" "),
            out.stderr.trim()
        ));
    }
    let v = out.stdout.trim();
    Ok((!v.is_empty()).then(|| v.to_string()))
}

fn read_reg_key(sys: &Sys, key: &str) -> Result<BTreeMap<String, Value>, String> {
    let out = run(sys, &[sys.tools.path("reg"), "query".into(), key.into()])?;
    if !out.success() {
        let e = format!("{}{}", out.stdout, out.stderr).to_ascii_lowercase();
        if e.contains("unable to find") || e.contains("cannot find") {
            return Ok(BTreeMap::new());
        }
        return Err(format!("`reg query {key}` failed: {}", out.stderr.trim()));
    }
    Ok(parse_reg_query(&out.stdout))
}

/// `    Name    REG_SZ    data` lines of one key, keyed by lowercase name.
pub fn parse_reg_query(text: &str) -> BTreeMap<String, Value> {
    let mut m = BTreeMap::new();
    for l in text.lines() {
        if !l.starts_with("    ") {
            continue;
        }
        let body = l.trim_start();
        let Some((name, rest)) = body.split_once("    ") else {
            continue;
        };
        let rest = rest.trim_start();
        let (ty, data) = rest.split_once("    ").unwrap_or((rest, ""));
        if !ty.starts_with("REG_") {
            continue;
        }
        let data = reg_canon(ty, &json!(data.trim()));
        m.insert(
            name.trim().to_ascii_lowercase(),
            json!({ "type": ty, "data": data }),
        );
    }
    m
}

/// How an entry changes a value: set it (`Some`) or remove it (`None`).
pub struct Write<'a> {
    pub setting: &'a Setting,
    pub value: Option<&'a Value>,
    /// Dotted dict entries merge into the existing entry instead of replacing it.
    pub merge: bool,
}

/// The commands that bring `w.setting` to `w.value`. `working` is the in-memory copy of the
/// setting's domain, updated as writes are planned so two entries of one dict compose.
pub fn commands(
    sys: &Sys,
    w: &Write,
    working: &mut BTreeMap<String, Map<String, Value>>,
) -> Vec<Vec<String>> {
    match w.setting {
        Setting::Defaults {
            domain, key, root, ..
        } => {
            let dict = working.entry(dkey(domain, *root)).or_default();
            defaults_commands(sys, domain, key, w, dict)
        }
        Setting::Registry { key, name, ty, .. } => match w.value {
            Some(v) => {
                // `v` is the canonical `{type,data}` form when restoring, or the raw
                // declared value when setting.
                let (ty, data) = match v {
                    Value::Object(o) if o.contains_key("type") => (
                        o["type"].as_str().unwrap_or(ty).to_string(),
                        o["data"].as_str().unwrap_or_default().to_string(),
                    ),
                    other => (ty.clone(), reg_canon(ty, other)),
                };
                vec![vec![
                    sys.tools.path("reg"),
                    "add".into(),
                    key.clone(),
                    "/v".into(),
                    name.clone(),
                    "/t".into(),
                    ty,
                    "/d".into(),
                    data,
                    "/f".into(),
                ]]
            }
            None => vec![vec![
                sys.tools.path("reg"),
                "delete".into(),
                key.clone(),
                "/v".into(),
                name.clone(),
                "/f".into(),
            ]],
        },
        Setting::XdgMime { mime, .. } => match w.value.and_then(Value::as_str) {
            Some(app) => vec![vec![
                sys.tools.path("xdg-mime"),
                "default".into(),
                app.into(),
                mime.clone(),
            ]],
            // There is no command to unset a default application.
            None => Vec::new(),
        },
        Setting::XdgSetting { setting, .. } => match w.value.and_then(Value::as_str) {
            Some(v) => vec![vec![
                sys.tools.path("xdg-settings"),
                "set".into(),
                setting.clone(),
                v.into(),
            ]],
            None => Vec::new(),
        },
    }
}

fn defaults_commands(
    sys: &Sys,
    domain: &str,
    key: &str,
    w: &Write,
    dict: &mut Map<String, Value>,
) -> Vec<Vec<String>> {
    let bin = sys.tools.path("defaults");
    let kp = resolve_key(dict, key);
    let write = |args: &[&str], tail: String| {
        let mut v = vec![bin.clone(), "write".to_string(), domain.to_string()];
        v.extend(args.iter().map(|s| s.to_string()));
        v.push(tail);
        v
    };
    if kp.path.is_empty() {
        return match w.value {
            Some(v) => {
                dict.insert(kp.top.clone(), v.clone());
                vec![write(&[&kp.top], plist::fragment(v))]
            }
            None => {
                dict.remove(&kp.top);
                vec![vec![bin, "delete".into(), domain.into(), kp.top]]
            }
        };
    }
    let top = dict
        .entry(kp.top.clone())
        .or_insert_with(|| Value::Object(Map::new()));
    match w.value {
        Some(v) => {
            let new = match (w.merge, v, get_path(top, &kp.path)) {
                (true, Value::Object(want), Some(Value::Object(have))) => {
                    let mut m = have.clone();
                    m.extend(want.clone());
                    Value::Object(m)
                }
                _ => v.clone(),
            };
            set_path(top, &kp.path, new.clone());
            if kp.path.len() == 1 {
                vec![write(
                    &[&kp.top, "-dict-add", &kp.path[0]],
                    plist::fragment(&new),
                )]
            } else {
                vec![write(&[&kp.top], plist::fragment(top))]
            }
        }
        None => {
            remove_path(top, &kp.path);
            vec![write(&[&kp.top], plist::fragment(top))]
        }
    }
}

/// `killall <app>`, once. An app that is not running is not an error.
pub fn restart_app(sys: &Sys, app: &str) -> Result<(), String> {
    let out = run(sys, &[sys.tools.path("killall"), app.into()])?;
    if out.success() || out.stderr.contains("No matching processes") {
        Ok(())
    } else {
        Err(format!("`killall {app}` failed: {}", out.stderr.trim()))
    }
}

/// Run one command. A `root` command goes through `sudo -n` (or runs as is when dots is root
/// already); with no sudo at all it fails, it never runs as the user.
pub fn exec(sys: &Sys, argv: &[String], root: bool) -> Result<(), String> {
    let mut full = Vec::new();
    if root {
        match sys.sudo {
            Sudo::Primed => {
                full.push(sys.tools.path("sudo"));
                full.push("-n".to_string());
            }
            Sudo::Unneeded => {}
            Sudo::Unavailable => {
                return Err(format!(
                    "`{}` needs root, but sudo is not available (the password prompt failed or was declined)",
                    argv.join(" ")
                ));
            }
        }
    }
    full.extend(argv.iter().cloned());
    let out = run(sys, &full)?;
    if out.success() {
        Ok(())
    } else {
        Err(format!(
            "`{}` failed: {}",
            argv.join(" "),
            out.stderr.trim()
        ))
    }
}
