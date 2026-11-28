//! Typed views of the schema files. They are deserialized only after the strict validator
//! has accepted the file, so they can be lenient (`default`, no `deny_unknown_fields`).
//!
//! `[platform."<key>"]` tables are kept as raw partial objects (`serde_json::Value`); merging
//! them is the resolver's job.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub use crate::spans::Spans;

/// Raw `[platform."<key>"]` partial objects, keyed by platform key.
pub type PlatformTables = BTreeMap<String, Value>;
/// `[settings.<pkg>]` value tables: package -> setting -> value.
pub type SettingValues = BTreeMap<String, BTreeMap<String, Value>>;

/// A string, or a per-platform table of strings (`dst`, `[paths]` variables).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum PlatformString {
    Plain(String),
    PerPlatform(BTreeMap<String, String>),
}

// ---------------------------------------------------------------- dots.toml

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct RepoManifest {
    pub schema: u32,
    pub requires_dots: Option<String>,
    pub name: Option<String>,
    pub priority: BTreeMap<String, Vec<String>>,
    pub tags: BTreeMap<String, TagDecl>,
    pub paths: BTreeMap<String, PlatformString>,
    pub platforms: BTreeMap<String, PlatformDecl>,
    /// User-defined providers (open tables, see ARCHITECTURE §5.4).
    pub providers: BTreeMap<String, Value>,
    /// Options for built-in providers.
    pub provider: BTreeMap<String, Value>,
    pub settings: SettingValues,
    pub upstream: Vec<Upstream>,
    pub recommended: Option<Recommended>,
    pub platform: PlatformTables,
    /// Positions of keys, by key path (see [`Spans`]). Set by the loader.
    #[serde(skip)]
    pub spans: Spans,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(untagged)]
pub enum TagDecl {
    Description(String),
    Full {
        description: Option<String>,
        recommended: Option<TagRecommended>,
    },
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(untagged)]
pub enum TagRecommended {
    Flag(bool),
    PerPlatform(BTreeMap<String, bool>),
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(untagged)]
pub enum Recommended {
    Flag(bool),
    Bundles(Vec<String>),
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct PlatformDecl {
    pub parent: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct Upstream {
    pub alias: Option<String>,
    pub url: String,
    pub follow: Option<String>,
    pub import: Vec<String>,
    pub rename: BTreeMap<String, String>,
    pub trust: Option<String>,
    pub scripts: Option<String>,
    pub integrity: Option<String>,
}

// ------------------------------------------------------------------- bundle

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct Bundle {
    /// The file stem; bundles have no `name` key.
    #[serde(skip)]
    pub name: String,
    pub schema: u32,
    pub description: Option<String>,
    pub include: Vec<String>,
    pub packages: Vec<BundleMember>,
    pub settings: SettingValues,
    pub recommended: bool,
    pub integrity: Option<String>,
    pub platform: PlatformTables,
    /// Positions of keys, by key path (see [`Spans`]). Set by the loader.
    #[serde(skip)]
    pub spans: Spans,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(untagged)]
pub enum BundleMember {
    Name(String),
    Conditional {
        name: String,
        #[serde(default)]
        when: Option<String>,
    },
}

impl BundleMember {
    pub fn name(&self) -> &str {
        match self {
            BundleMember::Name(n) | BundleMember::Conditional { name: n, .. } => n,
        }
    }
    pub fn when(&self) -> Option<&str> {
        match self {
            BundleMember::Name(_) => None,
            BundleMember::Conditional { when, .. } => when.as_deref(),
        }
    }
}

// ------------------------------------------------------------------ package

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct Package {
    pub schema: u32,
    pub name: String,
    pub description: Option<String>,
    pub when: Option<String>,
    pub bin: Option<String>,
    pub bins: Vec<String>,
    /// The raw version spec; the grammar is interpreted by the resolver.
    pub version: Option<String>,
    pub requires: Vec<Requirement>,
    pub after: Vec<String>,
    pub conflicts: Vec<String>,
    pub replaces: Vec<String>,
    pub prefer: Vec<String>,
    pub providers_only: bool,
    /// The install needs the admin password (a `.pkg` installer, a kernel extension). Without
    /// `--with-sudo` or `--sudo-only` a plain `dots apply` skips it.
    pub requires_sudo: bool,
    #[serde(skip)]
    pub providers: BTreeMap<String, ProviderConfig>,
    #[serde(rename = "providers")]
    raw_providers: BTreeMap<String, Value>,
    pub files: Vec<FileEntry>,
    /// `[[defaults]]` (macOS), `[[registry]]` (Windows), `[[xdg]]` (Linux): system settings.
    pub defaults: Vec<DefaultsEntry>,
    pub registry: Vec<RegistryEntry>,
    pub xdg: Vec<XdgEntry>,
    /// `[service]`: a background service the package keeps enabled and running.
    pub service: Option<ServiceDecl>,
    pub settings: BTreeMap<String, SettingDecl>,
    pub env: EnvSpec,
    pub hooks: Hooks,
    pub integrity: Option<Integrity>,
    /// Written by `dots add --copy`; never by hand.
    pub origin: Option<Origin>,
    pub platform: PlatformTables,
    /// `manual/*.md`, read by the loader (not part of `package.toml`).
    #[serde(skip)]
    pub manual: Vec<crate::manual::ManualAction>,

    /// The package directory (set by the loader).
    #[serde(skip)]
    pub dir: PathBuf,
    /// `//`-prefixed paths this package uses. Non-empty blocks `add --copy`.
    #[serde(skip)]
    pub escape_hatch_paths: Vec<String>,
    /// Positions of keys, by key path (see [`Spans`]). Set by the loader.
    #[serde(skip)]
    pub spans: Spans,
}

impl Package {
    /// Build the typed `providers` map from the raw tables. Called by the loader.
    pub(crate) fn finish(&mut self) -> Result<(), String> {
        for (id, v) in std::mem::take(&mut self.raw_providers) {
            let cfg = ProviderConfig::from_value(&id, v).map_err(|e| format!("{id}: {e}"))?;
            self.providers.insert(id, cfg);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(untagged)]
pub enum Requirement {
    Name(String),
    Detailed {
        name: String,
        #[serde(default)]
        version: Option<String>,
        #[serde(default)]
        when: Option<String>,
    },
}

impl Requirement {
    pub fn name(&self) -> &str {
        match self {
            Requirement::Name(n) | Requirement::Detailed { name: n, .. } => n,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(untagged)]
pub enum Integrity {
    /// `integrity = "warn" | "enforce"`.
    Policy(String),
    /// `[integrity]` author pins: path -> `sha256:...` (or `tree`).
    Pins(BTreeMap<String, String>),
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct Origin {
    pub repo: Option<String>,
    pub path: Option<String>,
    pub rev: Option<String>,
    pub tree: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct EnvSpec {
    pub path: Vec<String>,
    pub vars: BTreeMap<String, String>,
    pub source: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct Hooks {
    pub pre_install: Option<String>,
    pub post_install: Option<String>,
    pub pre_files: Option<String>,
    pub post_files: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct FileEntry {
    pub dst: Option<PlatformString>,
    pub mode: Option<String>,
    pub src: Option<String>,
    pub layers: Option<Layers>,
    pub format: Option<String>,
    pub kind: Option<String>,
    pub template: Option<String>,
    /// Line-comment leader for directives and generated headers (`//` for jsonc, `#` otherwise).
    pub comment: Option<String>,
    pub when: Option<String>,
    pub url: Option<String>,
    pub rev: Option<String>,
    pub priority: Option<i64>,
    pub set: Vec<FileSet>,
    pub merge: BTreeMap<String, Value>,
    pub platform: PlatformTables,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(untagged)]
pub enum Layers {
    List(Vec<Layer>),
    Auto {
        dir: String,
        #[serde(default)]
        auto: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(untagged)]
pub enum Layer {
    Path(String),
    Conditional {
        src: String,
        #[serde(default)]
        when: Option<String>,
    },
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct FileSet {
    pub path: String,
    pub value: Option<Value>,
    pub delete: bool,
    pub when: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct SettingDecl {
    #[serde(rename = "type")]
    pub ty: Option<String>,
    pub default: Option<Value>,
    pub range: Vec<f64>,
    pub description: Option<String>,
    pub ask: bool,
    pub question: Option<String>,
    pub when: Option<String>,
    pub required: bool,
    pub secret: bool,
    pub validate: Option<String>,
    pub values: Vec<String>,
    pub platform: PlatformTables,
}

// ---------------------------------------------------------------- providers

macro_rules! provider_struct {
    ($name:ident { $( $f:ident : $t:ty ),* $(,)? }) => {
        #[derive(Debug, Clone, Default, PartialEq, Deserialize)]
        #[serde(default)]
        pub struct $name {
            pub name: Option<String>,
            pub bin: Option<String>,
            pub bins: Vec<String>,
            pub shim: Option<String>,
            pub when: Option<String>,
            /// This provider's install needs the admin password (see `Package::requires_sudo`).
            pub requires_sudo: bool,
            $( pub $f: $t, )*
            pub platform: PlatformTables,
        }
    };
}

provider_struct!(Brew { cask: bool, tap: Option<String> });
provider_struct!(Apt { ppa: Option<String>, repo: Option<AptRepo>, pin: Option<AptPin> });
provider_struct!(Cargo { locked: Option<bool>, git: Option<String>, rev: Option<String>, features: Vec<String>, path: Option<String>, git_fetch_with_cli: bool });
provider_struct!(Uv { kind: Option<String>, python: Option<String>, requirements: Option<String>, path: Option<String>, git: Option<String>, rev: Option<String>, git_fetch_with_cli: bool, break_system_packages: bool });
provider_struct!(Npm {});
provider_struct!(GitProvider { url: Option<String>, rev: Option<String> });
provider_struct!(GithubRelease { repo: Option<String>, asset: Option<String>, sha256: Option<String>, version: Option<String> });
provider_struct!(Script {
    check: Option<String>,
    version: Option<String>,
    latest: Option<String>,
    install: Option<String>,
    upgrade: Option<String>,
    uninstall: Option<String>,
    shell: Option<String>,
    needs_root: bool,
    interactive: bool,
    versionless: bool,
    timeout: Option<String>,
    lock: Option<String>,
    fetch: Option<ScriptFetch>,
});

/// `repo = { ppa }` or `{ url, suite, components, key, fingerprint }` of an apt package.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct AptRepo {
    pub ppa: Option<String>,
    pub url: Option<String>,
    pub suite: Option<String>,
    /// `[]` (or `flat = true`) declares a flat repository.
    pub components: Option<Vec<String>>,
    pub flat: bool,
    pub key: Option<String>,
    pub fingerprint: Option<String>,
}

/// `pin = { origin, priority }`: an apt preferences stanza.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct AptPin {
    pub origin: Option<String>,
    pub priority: Option<i64>,
}

/// `fetch = { url, sha256 }` of a script package.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct ScriptFetch {
    pub url: Option<String>,
    pub sha256: Option<String>,
}
// winget also reads `override` (passed as `--override`: installer arguments, e.g. the
// vcbuildtools workloads) and `args` (extra arguments for install and upgrade).
provider_struct!(PlainProvider {
    id: Option<String>,
    installer_override: Option<String>,
    args: Vec<String>,
});
provider_struct!(BrowserExt {
    chrome: Option<String>,
    edge: Option<String>,
    firefox: Option<String>,
    brave: Option<String>,
    chromium: Option<String>,
    browsers: Vec<String>,
    profiles: Vec<String>,
    method: Option<String>,
    managed: BTreeMap<String, Value>,
});
provider_struct!(AgentSkill {
    src: Option<String>,
    agents: Vec<String>,
    frontmatter: BTreeMap<String, Value>,
});

/// One `[providers.<id>]` table: typed for built-in ids, open for anything else.
#[derive(Debug, Clone, PartialEq)]
pub enum ProviderConfig {
    Brew(Brew),
    Apt(Apt),
    Cargo(Cargo),
    Uv(Uv),
    Npm(Npm),
    Git(GitProvider),
    GithubRelease(GithubRelease),
    Script(Script),
    /// `winget`, `scoop`, `mas`, `pacman`, `dnf`.
    Plain {
        id: String,
        config: PlainProvider,
    },
    BrowserExt(BrowserExt),
    AgentSkill(AgentSkill),
    /// A user-defined provider id: the table is kept as written.
    Custom {
        id: String,
        table: Value,
    },
}

impl ProviderConfig {
    /// The provider id as written in `[providers.<id>]`.
    pub fn id(&self) -> &str {
        match self {
            Self::Brew(_) => "brew",
            Self::Apt(_) => "apt",
            Self::Cargo(_) => "cargo",
            Self::Uv(_) => "uv",
            Self::Npm(_) => "npm",
            Self::Git(_) => "git",
            Self::GithubRelease(_) => "github-release",
            Self::Script(_) => "script",
            Self::Plain { id, .. } | Self::Custom { id, .. } => id,
            Self::BrowserExt(_) => "browser-ext",
            Self::AgentSkill(_) => "agent-skill",
        }
    }

    /// The fields every provider table shares: `name`, `bin`, `bins`, `shim`, `when`,
    /// `requires_sudo`.
    pub fn common(&self) -> ProviderCommon {
        macro_rules! c {
            ($p:expr) => {
                ProviderCommon {
                    name: $p.name.clone(),
                    bin: $p.bin.clone(),
                    bins: $p.bins.clone(),
                    shim: $p.shim.clone(),
                    when: $p.when.clone(),
                    requires_sudo: $p.requires_sudo,
                }
            };
        }
        match self {
            Self::Brew(p) => c!(p),
            Self::Apt(p) => c!(p),
            Self::Cargo(p) => c!(p),
            Self::Uv(p) => c!(p),
            Self::Npm(p) => c!(p),
            Self::Git(p) => c!(p),
            Self::GithubRelease(p) => c!(p),
            Self::Script(p) => c!(p),
            // `id` is the package manager's own identifier (`winget`'s `Git.Git`): it names the
            // package there unless `name` says otherwise.
            Self::Plain { config: p, .. } => {
                let mut c = c!(p);
                c.name = c.name.or_else(|| p.id.clone());
                c
            }
            Self::BrowserExt(p) => c!(p),
            Self::AgentSkill(p) => c!(p),
            Self::Custom { table, .. } => {
                let s = |k: &str| table.get(k).and_then(Value::as_str).map(str::to_string);
                ProviderCommon {
                    name: s("name"),
                    bin: s("bin"),
                    bins: table
                        .get("bins")
                        .and_then(Value::as_array)
                        .map(|a| {
                            a.iter()
                                .filter_map(|v| v.as_str().map(str::to_string))
                                .collect()
                        })
                        .unwrap_or_default(),
                    shim: s("shim"),
                    when: s("when"),
                    requires_sudo: table
                        .get("requires_sudo")
                        .and_then(Value::as_bool)
                        .unwrap_or(false),
                }
            }
        }
    }
}

/// See [`ProviderConfig::common`].
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ProviderCommon {
    pub name: Option<String>,
    pub bin: Option<String>,
    pub bins: Vec<String>,
    pub shim: Option<String>,
    pub when: Option<String>,
    pub requires_sudo: bool,
}

impl ProviderConfig {
    fn from_value(id: &str, v: Value) -> Result<Self, serde_json::Error> {
        Ok(match id {
            "brew" => Self::Brew(serde_json::from_value(v)?),
            "apt" => Self::Apt(serde_json::from_value(v)?),
            "cargo" => Self::Cargo(serde_json::from_value(v)?),
            "uv" => Self::Uv(serde_json::from_value(v)?),
            "npm" => Self::Npm(serde_json::from_value(v)?),
            "git" => Self::Git(serde_json::from_value(v)?),
            "github-release" => Self::GithubRelease(serde_json::from_value(v)?),
            "script" => Self::Script(serde_json::from_value(v)?),
            "winget" | "scoop" | "mas" | "pacman" | "dnf" => {
                // `override` is a keyword: the field is `installer_override`.
                let mut v = v;
                if let Some(o) = v.as_object_mut()
                    && let Some(x) = o.remove("override")
                {
                    o.insert("installer_override".into(), x);
                }
                Self::Plain {
                    id: id.to_string(),
                    config: serde_json::from_value(v)?,
                }
            }
            "browser-ext" => Self::BrowserExt(serde_json::from_value(v)?),
            "agent-skill" => Self::AgentSkill(serde_json::from_value(v)?),
            _ => Self::Custom {
                id: id.to_string(),
                table: v,
            },
        })
    }
}

// ------------------------------------------------------------------ machine

/// A list key in a machine file: a plain list, or an `{ add, remove }` edit of the list below.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(untagged)]
pub enum ListSpec {
    Set(Vec<String>),
    Edit {
        #[serde(default)]
        add: Vec<String>,
        #[serde(default)]
        remove: Vec<String>,
    },
}

impl ListSpec {
    /// Apply on top of `base`: a plain list replaces; an edit removes, then appends new items.
    pub fn apply(&self, base: &[String]) -> Vec<String> {
        match self {
            ListSpec::Set(v) => v.clone(),
            ListSpec::Edit { add, remove } => {
                let mut out: Vec<String> = base
                    .iter()
                    .filter(|b| !remove.contains(b))
                    .cloned()
                    .collect();
                for a in add {
                    if !out.contains(a) {
                        out.push(a.clone());
                    }
                }
                out
            }
        }
    }
}

/// One machine file as written (`machine.toml` or `machines/<name>.toml`).
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct MachineFile {
    pub schema: u32,
    pub root: Option<String>,
    pub profile: Option<String>,
    pub bundles: Option<ListSpec>,
    pub tags: Option<ListSpec>,
    pub exclude: Option<ListSpec>,
    pub platform_extra: Option<ListSpec>,
    pub facts: BTreeMap<String, Value>,
    pub settings: SettingValues,
}

// ------------------------------------------------------- system settings, services

/// `[[defaults]]`: one macOS `defaults` value (a dotted `key` edits one entry of a dict).
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct DefaultsEntry {
    pub domain: String,
    pub key: String,
    pub value: Option<Value>,
    /// Apps restarted once, at the end of the run, after a change.
    pub restart: Vec<String>,
    pub root: bool,
    /// `logout`, `reboot` or `restart-app:<name>`: what a change needs before it takes effect.
    pub requires: Vec<String>,
}

/// `[[registry]]`: one Windows registry value.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct RegistryEntry {
    pub key: String,
    pub name: String,
    #[serde(rename = "type")]
    pub ty: Option<String>,
    pub value: Option<Value>,
}

/// `[[xdg]]`: a Linux default application (`mime` + `app`) or an `xdg-settings` value.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct XdgEntry {
    pub mime: Vec<String>,
    pub app: Option<String>,
    pub setting: Option<String>,
    pub value: Option<String>,
}

/// `[service]`: `manager` is `auto`, `brew`, `launchd`, `systemd-user` or `windows-startup`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default)]
pub struct ServiceDecl {
    pub name: String,
    pub manager: String,
    pub enabled: bool,
    pub running: bool,
    /// A file in the package, linked into the manager's unit directory.
    pub unit: Option<String>,
}

impl Default for ServiceDecl {
    fn default() -> Self {
        ServiceDecl {
            name: String::new(),
            manager: "auto".into(),
            enabled: true,
            running: true,
            unit: None,
        }
    }
}
