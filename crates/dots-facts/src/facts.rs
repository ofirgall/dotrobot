//! Fact detection (task 2.3).

use std::collections::BTreeMap;

use crate::sources::FactSources;

/// Where a fact's value came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Source {
    Detected,
    /// Forced by machine config `[facts]`.
    Overridden,
    /// Declared by the user (tags, `platform_extra`, ...).
    Declared,
}

impl Source {
    /// The stable JSON name.
    pub fn as_str(self) -> &'static str {
        match self {
            Source::Detected => "detected",
            Source::Overridden => "overridden",
            Source::Declared => "declared",
        }
    }

    /// The human label (`sudo false (machine.toml)`).
    pub fn label(self) -> &'static str {
        match self {
            Source::Detected => "detected",
            Source::Overridden => "machine.toml",
            Source::Declared => "declared",
        }
    }
}

/// A fact, setting or literal value.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Str(String),
    Bool(bool),
    Num(f64),
}

impl Value {
    pub fn render(&self) -> String {
        match self {
            Value::Str(s) => s.clone(),
            Value::Bool(b) => b.to_string(),
            Value::Num(n) => n.to_string(),
        }
    }

    pub fn to_json(&self) -> serde_json::Value {
        match self {
            Value::Str(s) => serde_json::Value::String(s.clone()),
            Value::Bool(b) => serde_json::Value::Bool(*b),
            Value::Num(n) => serde_json::json!(n),
        }
    }
}

/// Facts that conditions can name (besides `provider.<id>`).
pub const FACT_NAMES: &[&str] = &[
    "os",
    "arch",
    "distro",
    "distro_family",
    "distro_version",
    "wsl",
    "sudo",
    "gui",
    "hostname",
    "user",
    "home",
    "shell",
];

/// `machine.toml` `[facts]` overrides.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FactOverrides {
    pub sudo: Option<bool>,
    pub gui: Option<bool>,
    pub wsl: Option<bool>,
}

/// Detected facts about this machine.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Facts {
    pub os: String,
    pub arch: String,
    pub distro: Option<String>,
    pub distro_family: Option<String>,
    /// Linux `VERSION_ID`, the macOS product version, or the Windows major (`10`/`11`).
    pub distro_version: Option<String>,
    pub is_wsl: bool,
    pub has_sudo: bool,
    pub gui: bool,
    pub hostname: String,
    pub user: String,
    pub home: String,
    pub shell: String,
    /// `ID_LIKE` entries, as written (nearest parent first).
    pub id_like: Vec<String>,
    /// `provider.<id>`: the caller fills this once providers have probed their binaries.
    pub providers: BTreeMap<String, bool>,
    /// Source per fact name (`os`, `sudo`, `provider.apt`, ...).
    pub sources: BTreeMap<String, Source>,
    /// Which rule produced a detected fact (`sudo` -> `group admin`).
    pub notes: BTreeMap<String, String>,
}

impl Facts {
    pub fn source_of(&self, name: &str) -> Source {
        self.sources.get(name).copied().unwrap_or(Source::Detected)
    }

    /// Human label for a fact's source, with the matching rule when known:
    /// `detected (group admin)`, `machine.toml`.
    pub fn source_label(&self, name: &str) -> String {
        let l = self.source_of(name).label();
        match self.notes.get(name) {
            Some(n) => format!("{l} ({n})"),
            None => l.to_string(),
        }
    }

    /// The hook for provider availability.
    pub fn set_provider(&mut self, id: &str, available: bool, source: Source) {
        self.providers.insert(id.to_string(), available);
        self.sources.insert(format!("provider.{id}"), source);
    }

    /// A fact's value. Missing optional facts (`distro` on macOS) read as the empty string.
    pub fn get(&self, name: &str) -> Option<Value> {
        let s = |v: &str| Some(Value::Str(v.to_string()));
        let o = |v: &Option<String>| Some(Value::Str(v.clone().unwrap_or_default()));
        match name {
            "os" => s(&self.os),
            "arch" => s(&self.arch),
            "distro" => o(&self.distro),
            "distro_family" => o(&self.distro_family),
            "distro_version" => o(&self.distro_version),
            "wsl" => Some(Value::Bool(self.is_wsl)),
            "sudo" => Some(Value::Bool(self.has_sudo)),
            "gui" => Some(Value::Bool(self.gui)),
            "hostname" => s(&self.hostname),
            "user" => s(&self.user),
            "home" => s(&self.home),
            "shell" => s(&self.shell),
            _ => name
                .strip_prefix("provider.")
                .and_then(|id| self.providers.get(id))
                .map(|b| Value::Bool(*b)),
        }
    }

    /// Every fact in print order, with its source.
    pub fn entries(&self) -> Vec<(String, Value, Source)> {
        let mut v: Vec<(String, Value, Source)> = FACT_NAMES
            .iter()
            .filter_map(|n| {
                self.get(n)
                    .map(|val| (n.to_string(), val, self.source_of(n)))
            })
            .collect();
        for (id, b) in &self.providers {
            let name = format!("provider.{id}");
            let src = self.source_of(&name);
            v.push((name, Value::Bool(*b), src));
        }
        v
    }
}

/// Parse `/etc/os-release` (`KEY=value`, optionally quoted).
pub fn parse_os_release(text: &str) -> BTreeMap<String, String> {
    let mut m = BTreeMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((k, v)) = line.split_once('=') {
            let v = v.trim();
            let v = if v.len() >= 2
                && ((v.starts_with('"') && v.ends_with('"'))
                    || (v.starts_with('\'') && v.ends_with('\'')))
            {
                &v[1..v.len() - 1]
            } else {
                v
            };
            m.insert(k.trim().to_string(), v.to_string());
        }
    }
    m
}

fn normalize_arch(raw: &str) -> String {
    match raw.trim() {
        "arm64" | "ARM64" | "aarch64" => "aarch64".into(),
        "amd64" | "AMD64" | "x86_64" | "x64" => "x86_64".into(),
        other => other.to_string(),
    }
}

fn first_line(out: Option<crate::sources::CmdOutput>) -> Option<String> {
    let o = out.filter(|o| o.status == 0)?;
    o.stdout
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .map(str::to_string)
}

/// Detect every fact once. `overrides` come from machine config `[facts]`.
pub fn detect(src: &dyn FactSources, overrides: &FactOverrides) -> Facts {
    let mut f = Facts {
        os: src.target_os(),
        ..Default::default()
    };

    // Runtime arch (`uname -m`), so it is right under Rosetta; compile-time as the fallback.
    f.arch = first_line(src.run("uname", &["-m"]))
        .map(|a| normalize_arch(&a))
        .or_else(|| {
            src.env("PROCESSOR_ARCHITECTURE")
                .map(|a| normalize_arch(&a))
        })
        .unwrap_or_else(|| normalize_arch(std::env::consts::ARCH));

    match f.os.as_str() {
        "linux" => {
            if let Some(text) = src.read_file("/etc/os-release") {
                let m = parse_os_release(&text);
                let id = m.get("ID").cloned().filter(|s| !s.is_empty());
                f.id_like = m
                    .get("ID_LIKE")
                    .map(|s| s.split_whitespace().map(str::to_string).collect())
                    .unwrap_or_default();
                if f.id_like.is_empty()
                    && let Some(id) = &id
                {
                    f.id_like = crate::chain::known_ancestors(id);
                }
                f.distro_family = f.id_like.last().cloned().or_else(|| id.clone());
                f.distro = id;
                f.distro_version = m.get("VERSION_ID").cloned().filter(|s| !s.is_empty());
            }
        }
        "macos" => {
            f.distro_version = first_line(src.run("sw_vers", &["-productVersion"]));
        }
        "windows" => {
            f.distro_version = first_line(src.run("cmd", &["/c", "ver"])).and_then(|l| {
                let ver = l.split("Version").nth(1)?.trim().trim_end_matches(']');
                let build: u32 = ver.split('.').nth(2)?.parse().ok()?;
                Some(if build >= 22000 { "11" } else { "10" }.to_string())
            });
        }
        _ => {}
    }

    // wsl
    f.is_wsl = f.os == "linux"
        && src
            .read_file("/proc/sys/kernel/osrelease")
            .is_some_and(|s| s.to_lowercase().contains("microsoft"));
    if let Some(v) = overrides.wsl {
        f.is_wsl = v;
        f.sources.insert("wsl".into(), Source::Overridden);
    }

    // identity
    f.user = src
        .env("USER")
        .or_else(|| src.env("USERNAME"))
        .or_else(|| src.env("LOGNAME"))
        .or_else(|| first_line(src.run("id", &["-un"])))
        .unwrap_or_default();
    f.home = src
        .env("HOME")
        .or_else(|| src.env("USERPROFILE"))
        .unwrap_or_default();
    f.hostname = first_line(src.run("hostname", &[]))
        .or_else(|| src.env("HOSTNAME"))
        .or_else(|| src.env("COMPUTERNAME"))
        .unwrap_or_default();
    f.shell = src
        .env("SHELL")
        .map(|s| {
            let base = s.rsplit(['/', '\\']).next().unwrap_or(&s).to_string();
            base.trim_end_matches(".exe").to_string()
        })
        .unwrap_or_else(|| if f.os == "windows" { "pwsh" } else { "sh" }.to_string());

    // sudo: root, or passwordless/cached sudo, or membership of an admin group.
    f.has_sudo = false;
    if f.os != "windows" {
        let root = first_line(src.run("id", &["-u"])).is_some_and(|u| u == "0") || f.user == "root";
        if root {
            f.has_sudo = true;
            f.notes.insert("sudo".into(), "root".into());
        } else if src
            .run("sudo", &["-n", "true"])
            .is_some_and(|o| o.status == 0)
        {
            f.has_sudo = true;
            f.notes.insert("sudo".into(), "sudo -n".into());
        } else if let Some(g) = first_line(src.run("id", &["-Gn"])).and_then(|l| {
            l.split_whitespace()
                .find(|g| matches!(*g, "sudo" | "wheel" | "admin"))
                .map(str::to_string)
        }) {
            f.has_sudo = true;
            f.notes.insert("sudo".into(), format!("group {g}"));
        }
    }
    if let Some(v) = overrides.sudo {
        f.has_sudo = v;
        f.notes.remove("sudo");
        f.sources.insert("sudo".into(), Source::Overridden);
    }

    // gui
    f.gui = match f.os.as_str() {
        "macos" | "windows" => true,
        _ => {
            src.env("WAYLAND_DISPLAY").is_some()
                || src.env("DISPLAY").is_some()
                || first_line(src.run(
                    "loginctl",
                    &["show-session", "self", "-p", "Type", "--value"],
                ))
                .is_some_and(|t| matches!(t.as_str(), "x11" | "wayland" | "mir"))
        }
    };
    if let Some(v) = overrides.gui {
        f.gui = v;
        f.sources.insert("gui".into(), Source::Overridden);
    }
    f
}

/// "Did you mean": the closest candidate within a small edit distance.
pub fn suggest<'a, I>(name: &str, candidates: I) -> Option<String>
where
    I: IntoIterator<Item = &'a str>,
{
    let limit = (name.chars().count() / 3).clamp(1, 3);
    candidates
        .into_iter()
        .map(|c| (edit_distance(name, c), c))
        .filter(|(d, _)| *d <= limit)
        .min_by_key(|(d, _)| *d)
        .map(|(_, c)| c.to_string())
}

fn edit_distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut prev2 = prev.clone();
    for i in 1..=a.len() {
        let mut cur = vec![i];
        for j in 1..=b.len() {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            let mut d = (prev[j] + 1).min(cur[j - 1] + 1).min(prev[j - 1] + cost);
            if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                d = d.min(prev2[j - 2] + 1); // transposition
            }
            cur.push(d);
        }
        prev2 = std::mem::replace(&mut prev, cur);
    }
    prev[b.len()]
}
