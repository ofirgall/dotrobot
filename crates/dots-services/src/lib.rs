//! Services: a package declares one background service as enabled and running, and dots
//! reconciles it through the right manager (`brew services`, a `launchd` agent, `systemd --user`
//! or Windows startup), restarts it once when its config changed, and reports its state.
//!
//! State is read with one call per manager. Every OS call goes through a
//! [`dots_providers::Runner`], so tests script the machine.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use dots_providers::{Cmd, Output, Runner};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Where the service tools are. `DOTS_SYSTEM_TOOLS=<dir>` redirects them to `<dir>/<name>`.
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
            "brew" => {
                if std::path::Path::new("/opt/homebrew/bin/brew").exists() {
                    "/opt/homebrew/bin/brew".into()
                } else if std::path::Path::new("/usr/local/bin/brew").exists() {
                    "/usr/local/bin/brew".into()
                } else {
                    "/opt/homebrew/bin/brew".into()
                }
            }
            "launchctl" => "/bin/launchctl".into(),
            "systemctl" => "/usr/bin/systemctl".into(),
            "reg" => r"C:\Windows\System32\reg.exe".into(),
            "tasklist" => r"C:\Windows\System32\tasklist.exe".into(),
            "taskkill" => r"C:\Windows\System32\taskkill.exe".into(),
            "powershell" => r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe".into(),
            other => other.into(),
        }
    }
}

/// Runner, tool locations, the user and home the manager commands refer to.
pub struct Sys<'a> {
    pub runner: &'a dyn Runner,
    pub tools: Tools,
    pub uid: u32,
    pub home: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Manager {
    Brew,
    Launchd,
    SystemdUser,
    WindowsStartup,
}

impl Manager {
    pub fn parse(s: &str) -> Option<Manager> {
        Some(match s {
            "brew" => Manager::Brew,
            "launchd" => Manager::Launchd,
            "systemd-user" => Manager::SystemdUser,
            "windows-startup" => Manager::WindowsStartup,
            _ => return None,
        })
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Manager::Brew => "brew",
            Manager::Launchd => "launchd",
            Manager::SystemdUser => "systemd-user",
            Manager::WindowsStartup => "windows-startup",
        }
    }

    fn os(self) -> &'static str {
        match self {
            Manager::Brew | Manager::Launchd => "macos",
            Manager::SystemdUser => "linux",
            Manager::WindowsStartup => "windows",
        }
    }
}

/// The manager for a `manager = "..."` declaration on `os`. `provider` is the id of the
/// provider chosen for the package (brew on macOS selects `brew services`). `None` means the
/// declaration is inert on this platform.
pub fn select_manager(os: &str, declared: &str, provider: Option<&str>) -> Option<Manager> {
    let m = match declared {
        "brew" => Manager::Brew,
        "launchd" => Manager::Launchd,
        "systemd-user" => Manager::SystemdUser,
        "windows-startup" => Manager::WindowsStartup,
        _ => match os {
            "macos" if provider == Some("brew") => Manager::Brew,
            "macos" => Manager::Launchd,
            "linux" => Manager::SystemdUser,
            "windows" => Manager::WindowsStartup,
            _ => return None,
        },
    };
    (m.os() == os).then_some(m)
}

/// Where a package's unit file is linked, for the managers that have one.
pub fn unit_dst(manager: Manager, home: &std::path::Path, unit: &str) -> Option<PathBuf> {
    let file = unit.rsplit('/').next().filter(|f| !f.is_empty())?;
    match manager {
        Manager::Launchd => Some(home.join("Library/LaunchAgents").join(file)),
        Manager::SystemdUser => Some(home.join(".config/systemd/user").join(file)),
        _ => None,
    }
}

/// The unit directory shown in plans (`~/.config/systemd/user/`).
pub fn unit_dir(manager: Manager) -> Option<&'static str> {
    match manager {
        Manager::Launchd => Some("~/Library/LaunchAgents"),
        Manager::SystemdUser => Some("~/.config/systemd/user"),
        _ => None,
    }
}

/// One declared service, with its manager already chosen.
#[derive(Debug, Clone, PartialEq)]
pub struct Service {
    pub package: String,
    pub name: String,
    pub manager: Manager,
    pub enabled: bool,
    pub running: bool,
    /// The unit file in the package, as declared (`services/autolock.service`).
    pub unit: Option<String>,
}

impl Service {
    /// The unit file name, or `<name>.plist` / `<name>.service`.
    fn file_name(&self) -> String {
        match &self.unit {
            Some(u) => u.rsplit('/').next().unwrap_or(u).to_string(),
            None => match self.manager {
                Manager::Launchd => format!("{}.plist", self.name),
                _ => format!("{}.service", self.name),
            },
        }
    }

    /// The manager's own name for it: a launchd label, a systemd unit, a brew formula.
    pub fn handle(&self) -> String {
        match self.manager {
            Manager::Launchd => {
                let f = self.file_name();
                f.strip_suffix(".plist").unwrap_or(&f).to_string()
            }
            Manager::SystemdUser => {
                let f = self.file_name();
                if f.contains('.') {
                    f
                } else {
                    format!("{f}.service")
                }
            }
            _ => self.name.clone(),
        }
    }

    pub fn key(&self) -> String {
        format!("{}:{}", self.package, self.name)
    }
}

/// A name that is not safe to put in a command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NameError {
    pub package: String,
    /// The offending value.
    pub name: String,
    /// Which value: `name`, `unit file name`.
    pub what: &'static str,
}

/// `^[A-Za-z0-9._@-]+$`, and not starting with `-` (it would read as an option).
pub fn valid_name(s: &str) -> bool {
    !s.is_empty()
        && !s.starts_with('-')
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '@' | '-'))
}

/// Service names, and the unit or label derived from them, end up in `systemctl`,
/// `launchctl` and Windows commands, so only plain names are accepted. Checked at plan time
/// (and by `status`), before anything is built from them.
pub fn check_names(services: &[Service]) -> Result<(), NameError> {
    for s in services {
        let check = |what: &'static str, v: &str| {
            if valid_name(v) {
                Ok(())
            } else {
                Err(NameError {
                    package: s.package.clone(),
                    name: v.to_string(),
                    what,
                })
            }
        };
        check("name", &s.name)?;
        check("unit file name", &s.file_name())?;
        check("handle", &s.handle())?;
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Run {
    Running,
    Stopped,
    Failed,
}

impl Run {
    pub fn as_str(self) -> &'static str {
        match self {
            Run::Running => "running",
            Run::Stopped => "stopped",
            Run::Failed => "failed",
        }
    }
}

/// What a manager says about one service.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct State {
    pub enabled: bool,
    pub run: Run,
    /// launchd: the job is bootstrapped into the domain (a job can be loaded and disabled, or
    /// running without being enabled). Other managers: same as `enabled`.
    pub loaded: bool,
}

impl State {
    /// Not known to the manager at all.
    pub const GONE: State = State {
        enabled: false,
        run: Run::Stopped,
        loaded: false,
    };
}

fn run(sys: &Sys, argv: &[String]) -> Result<Output, String> {
    let cmd = Cmd::from_argv(argv).ok_or("empty command")?;
    sys.runner.run(&cmd).map_err(|e| e.to_string())
}

/// The state of every service, with one call per manager in use.
pub fn observe(sys: &Sys, services: &[Service]) -> Result<BTreeMap<String, State>, String> {
    let mut out = BTreeMap::new();
    let managers: BTreeSet<Manager> = services.iter().map(|s| s.manager).collect();
    for m in managers {
        let mine: Vec<&Service> = services.iter().filter(|s| s.manager == m).collect();
        let read = match m {
            Manager::Brew => read_brew(sys, &mine),
            Manager::Launchd => read_launchd(sys, &mine),
            Manager::SystemdUser => read_systemd(sys, &mine),
            Manager::WindowsStartup => read_windows(sys, &mine),
        };
        match read {
            Ok(states) => out.extend(states),
            // The manager itself is not installed (yet: it may be installed by this very
            // run): nothing is enabled or running, and the step will say what is missing.
            Err(e) if e.starts_with("cannot run") => {
                out.extend(mine.iter().map(|s| (s.key(), State::GONE)))
            }
            Err(e) => return Err(e),
        }
    }
    Ok(out)
}

fn read_brew(sys: &Sys, svcs: &[&Service]) -> Result<Vec<(String, State)>, String> {
    let o = run(
        sys,
        &[
            sys.tools.path("brew"),
            "services".into(),
            "list".into(),
            "--json".into(),
        ],
    )?;
    if !o.success() {
        return Err(format!("`brew services list` failed: {}", o.stderr.trim()));
    }
    let list: Value = serde_json::from_str(&o.stdout).map_err(|e| format!("brew services: {e}"))?;
    let mut by_name: BTreeMap<String, String> = BTreeMap::new();
    for e in list.as_array().into_iter().flatten() {
        if let (Some(n), Some(s)) = (e["name"].as_str(), e["status"].as_str()) {
            by_name.insert(n.to_string(), s.to_string());
        }
    }
    Ok(svcs
        .iter()
        .map(|s| {
            let st = match by_name.get(&s.handle()).map(String::as_str) {
                Some("started") => State {
                    enabled: true,
                    run: Run::Running,
                    loaded: true,
                },
                Some("error") => State {
                    enabled: true,
                    run: Run::Failed,
                    loaded: true,
                },
                Some("stopped") => State {
                    enabled: true,
                    run: Run::Stopped,
                    loaded: true,
                },
                _ => State::GONE,
            };
            (s.key(), st)
        })
        .collect())
}

fn read_launchd(sys: &Sys, svcs: &[&Service]) -> Result<Vec<(String, State)>, String> {
    let domain = format!("gui/{}", sys.uid);
    let o = run(sys, &[sys.tools.path("launchctl"), "print".into(), domain])?;
    if !o.success() {
        return Err(format!("`launchctl print` failed: {}", o.stderr.trim()));
    }
    let (loaded, disabled) = parse_launchctl_print(&o.stdout);
    Ok(svcs
        .iter()
        .map(|s| {
            let h = s.handle();
            let st = match loaded.get(&h) {
                None => State::GONE,
                Some((pid, status)) => State {
                    loaded: true,
                    enabled: !disabled.contains(&h),
                    run: if pid.is_some() {
                        Run::Running
                    } else if *status != 0 {
                        Run::Failed
                    } else {
                        Run::Stopped
                    },
                },
            };
            (s.key(), st)
        })
        .collect())
}

/// Loaded launchd services: label -> (pid, last exit status).
pub type LoadedServices = BTreeMap<String, (Option<u32>, i32)>;

/// `services = { pid status label }` rows (label -> pid, last exit status) and the labels in
/// `disabled services`.
pub fn parse_launchctl_print(text: &str) -> (LoadedServices, BTreeSet<String>) {
    let mut loaded = BTreeMap::new();
    let mut disabled = BTreeSet::new();
    let mut section = "";
    for l in text.lines() {
        let t = l.trim();
        if t.ends_with("= {") {
            section = match t {
                "services = {" => "services",
                "disabled services = {" => "disabled",
                _ => "",
            };
            continue;
        }
        if t == "}" {
            section = "";
            continue;
        }
        match section {
            "services" => {
                let f: Vec<&str> = t.split_whitespace().collect();
                if f.len() >= 3 {
                    let pid = f[0].parse::<u32>().ok();
                    let status = f[1].parse::<i32>().unwrap_or(0);
                    loaded.insert(f[2].to_string(), (pid, status));
                }
            }
            "disabled" => {
                if let Some((label, val)) = t.split_once("=>")
                    && val.trim() == "disabled"
                {
                    disabled.insert(label.trim().trim_matches('"').to_string());
                }
            }
            _ => {}
        }
    }
    (loaded, disabled)
}

fn read_systemd(sys: &Sys, svcs: &[&Service]) -> Result<Vec<(String, State)>, String> {
    let mut a = vec![
        sys.tools.path("systemctl"),
        "--user".into(),
        "show".into(),
        "--property=Id,LoadState,ActiveState,UnitFileState".into(),
        "--".into(),
    ];
    a.extend(svcs.iter().map(|s| s.handle()));
    let o = run(sys, &a)?;
    if !o.success() {
        return Err(format!(
            "`systemctl --user show` failed: {}",
            o.stderr.trim()
        ));
    }
    let mut by_id: BTreeMap<String, State> = BTreeMap::new();
    for block in o.stdout.split("\n\n") {
        let mut id = String::new();
        let (mut active, mut file) = (String::new(), String::new());
        for l in block.lines() {
            match l.split_once('=') {
                Some(("Id", v)) => id = v.into(),
                Some(("ActiveState", v)) => active = v.into(),
                Some(("UnitFileState", v)) => file = v.into(),
                _ => {}
            }
        }
        if id.is_empty() {
            continue;
        }
        let run = match active.as_str() {
            "active" | "activating" | "reloading" => Run::Running,
            "failed" => Run::Failed,
            _ => Run::Stopped,
        };
        let enabled = matches!(
            file.as_str(),
            "enabled" | "enabled-runtime" | "static" | "alias"
        );
        by_id.insert(
            id,
            State {
                enabled,
                run,
                loaded: enabled,
            },
        );
    }
    Ok(svcs
        .iter()
        .map(|s| {
            (
                s.key(),
                by_id.get(&s.handle()).copied().unwrap_or(State::GONE),
            )
        })
        .collect())
}

const RUN_KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";

fn read_windows(sys: &Sys, svcs: &[&Service]) -> Result<Vec<(String, State)>, String> {
    let o = run(
        sys,
        &[sys.tools.path("reg"), "query".into(), RUN_KEY.into()],
    )?;
    let entries = if o.success() { o.stdout } else { String::new() };
    let t = run(
        sys,
        &[
            sys.tools.path("tasklist"),
            "/FO".into(),
            "CSV".into(),
            "/NH".into(),
        ],
    )?;
    let procs: BTreeSet<String> = t
        .stdout
        .lines()
        .filter_map(|l| l.split(',').next())
        .map(|n| n.trim_matches('"').to_ascii_lowercase())
        .collect();
    Ok(svcs
        .iter()
        .map(|s| {
            let enabled = entries.lines().any(|l| {
                l.trim_start()
                    .to_ascii_lowercase()
                    .starts_with(&s.name.to_ascii_lowercase())
            });
            let exe = format!("{}.exe", s.name.to_ascii_lowercase());
            let running = procs.contains(&exe);
            (
                s.key(),
                State {
                    enabled,
                    run: if running { Run::Running } else { Run::Stopped },
                    loaded: enabled,
                },
            )
        })
        .collect())
}

/// One planned service action: the commands to run, in order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Step {
    pub package: String,
    pub name: String,
    pub manager: Manager,
    /// `enable`, `start`, `enable+start`, `restart`, `disable` or `stop`.
    pub action: String,
    pub argv: Vec<Vec<String>>,
    /// Why: `not enabled`, `config changed`, ...
    pub why: String,
    /// Set when the step enables the service: dots now owns that, and takes it down again
    /// when the declaration goes ([`Ledger`]).
    pub owned: Option<OwnedService>,
    /// Set when the step takes down a service dots enabled: the ledger key to forget.
    pub release: Option<String>,
}

/// A service dots enabled, as `<state>/services.json` remembers it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OwnedService {
    pub package: String,
    pub name: String,
    pub manager: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
}

impl OwnedService {
    pub fn of(s: &Service) -> OwnedService {
        OwnedService {
            package: s.package.clone(),
            name: s.name.clone(),
            manager: s.manager.as_str().to_string(),
            unit: s.unit.clone(),
        }
    }

    pub fn key(&self) -> String {
        format!("{}:{}", self.package, self.name)
    }

    /// The service as it would be declared, to read and to take down.
    pub fn service(&self) -> Option<Service> {
        Some(Service {
            package: self.package.clone(),
            name: self.name.clone(),
            manager: Manager::parse(&self.manager)?,
            enabled: true,
            running: true,
            unit: self.unit.clone(),
        })
    }
}

pub const LEDGER_SCHEMA: u32 = 1;

/// `<state>/services.json`: the services dots enabled (so it can disable them when their
/// package goes), and a fingerprint of each service's config sources at its last converged
/// run (so a change to a linked file, which never produces a files step, still restarts it).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ledger {
    pub schema: u32,
    #[serde(default)]
    pub owned: BTreeMap<String, OwnedService>,
    #[serde(default)]
    pub config: BTreeMap<String, String>,
    #[serde(skip)]
    path: PathBuf,
}

impl Ledger {
    pub fn path(state: &Path) -> PathBuf {
        state.join("services.json")
    }

    /// A missing or unreadable ledger is an empty one.
    pub fn load(state: &Path) -> Ledger {
        let path = Self::path(state);
        let mut l: Ledger = std::fs::read_to_string(&path)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or(Ledger {
                schema: LEDGER_SCHEMA,
                owned: BTreeMap::new(),
                config: BTreeMap::new(),
                path: PathBuf::new(),
            });
        l.path = path;
        l
    }

    pub fn save(&self) -> Result<(), String> {
        if let Some(d) = self.path.parent() {
            std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
        }
        let tmp = self.path.with_extension("json.tmp");
        let mut text = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        text.push('\n');
        std::fs::write(&tmp, text).map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, &self.path).map_err(|e| e.to_string())
    }

    /// The services whose key is not in `declared`: their declaration is gone (the package is
    /// inactive, left the repo, or no longer declares a service).
    pub fn orphans(&self, declared: &BTreeSet<String>) -> Vec<OwnedService> {
        self.owned
            .iter()
            .filter(|(k, _)| !declared.contains(*k))
            .map(|(_, o)| o.clone())
            .collect()
    }

    /// The service keys whose config sources differ from the recorded fingerprint. A service
    /// with no fingerprint yet is not reported (there is nothing to compare with).
    pub fn config_changed(&self, now: &BTreeMap<String, String>) -> BTreeSet<String> {
        now.iter()
            .filter(|(k, h)| self.config.get(*k).is_some_and(|old| old != *h))
            .map(|(k, _)| k.clone())
            .collect()
    }
}

/// A fingerprint of config source files (FNV-1a over each path relative to `base` and its
/// bytes; a directory contributes everything under it, sorted). Missing paths count as such.
pub fn config_hash(base: &Path, sources: &[PathBuf]) -> String {
    fn eat(h: &mut u64, bytes: &[u8]) {
        for b in bytes {
            *h ^= u64::from(*b);
            *h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    fn walk(h: &mut u64, base: &Path, p: &Path, depth: usize) {
        let rel = p.strip_prefix(base).unwrap_or(p);
        eat(h, rel.to_string_lossy().as_bytes());
        if let Ok(rd) = std::fs::read_dir(p)
            && depth < 16
        {
            let mut entries: Vec<PathBuf> = rd.flatten().map(|e| e.path()).collect();
            entries.sort();
            for e in entries {
                walk(h, base, &e, depth + 1);
            }
        } else if let Ok(bytes) = std::fs::read(p) {
            eat(h, &bytes);
        } else {
            eat(h, b"<missing>");
        }
    }
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let mut sorted: Vec<&PathBuf> = sources.iter().collect();
    sorted.sort();
    for p in sorted {
        walk(&mut h, base, p, 0);
    }
    format!("{h:016x}")
}

/// What needs doing for `s`, given what the manager reports. `changed` names the services whose
/// config changes in this run: a package name (any of its services) or a service key
/// (`package:name`).
pub fn plan(
    sys: &Sys,
    services: &[Service],
    states: &BTreeMap<String, State>,
    changed: &BTreeSet<String>,
) -> Vec<Step> {
    let mut steps = Vec::new();
    for s in services {
        let st = states.get(&s.key()).copied().unwrap_or(State::GONE);
        let cfg_changed = changed.contains(&s.package) || changed.contains(&s.key());
        let need_enable = s.enabled && !st.enabled;
        let need_start = s.running && st.run != Run::Running;
        let need_restart = !need_start && cfg_changed && st.run == Run::Running;
        if !(need_enable || need_start || need_restart) {
            continue;
        }
        let (action, why) = match (need_enable, need_start, need_restart) {
            (true, true, _) => ("enable+start", "not enabled, not running"),
            (true, false, true) => ("enable+restart", "not enabled, config changed"),
            (true, false, false) => ("enable", "not enabled"),
            (false, true, _) => (
                "start",
                if st.run == Run::Failed {
                    "failed"
                } else {
                    "not running"
                },
            ),
            (false, false, true) => ("restart", "config changed"),
            _ => continue,
        };
        steps.push(Step {
            package: s.package.clone(),
            name: s.name.clone(),
            manager: s.manager,
            action: action.into(),
            argv: commands(
                sys,
                s,
                &Want {
                    enable: need_enable,
                    start: need_start,
                    restart: need_restart,
                    changed: cfg_changed,
                    state: st,
                },
            ),
            why: why.into(),
            owned: need_enable.then(|| OwnedService::of(s)),
            release: None,
        });
    }
    steps
}

/// What a service gets taken down when its declaration is gone, for the services dots enabled
/// that are still enabled, loaded or running.
pub fn plan_teardown(
    sys: &Sys,
    owned: &[OwnedService],
    states: &BTreeMap<String, State>,
) -> Vec<Step> {
    let mut steps = Vec::new();
    for o in owned {
        let Some(s) = o.service() else { continue };
        let st = states.get(&s.key()).copied().unwrap_or(State::GONE);
        if !(st.enabled || st.loaded || st.run == Run::Running) {
            continue;
        }
        steps.push(Step {
            package: s.package.clone(),
            name: s.name.clone(),
            manager: s.manager,
            action: "disable".into(),
            argv: teardown_commands(sys, &s),
            why: "no longer declared".into(),
            owned: None,
            release: Some(s.key()),
        });
    }
    steps
}

fn teardown_commands(sys: &Sys, s: &Service) -> Vec<Vec<String>> {
    let h = s.handle();
    match s.manager {
        Manager::Brew => vec![vec![
            sys.tools.path("brew"),
            "services".into(),
            "stop".into(),
            h,
        ]],
        Manager::SystemdUser => vec![vec![
            sys.tools.path("systemctl"),
            "--user".into(),
            "disable".into(),
            "--now".into(),
            "--".into(),
            h,
        ]],
        Manager::Launchd => vec![vec![
            sys.tools.path("launchctl"),
            "bootout".into(),
            format!("gui/{}/{h}", sys.uid),
        ]],
        Manager::WindowsStartup => vec![
            vec![
                sys.tools.path("taskkill"),
                "/IM".into(),
                format!("{}.exe", s.name),
                "/F".into(),
            ],
            vec![
                sys.tools.path("reg"),
                "delete".into(),
                RUN_KEY.into(),
                "/v".into(),
                s.name.clone(),
                "/f".into(),
            ],
        ],
    }
}

/// What a step has to do for one service.
struct Want {
    enable: bool,
    start: bool,
    restart: bool,
    /// The package's config (or unit) changes in this run.
    changed: bool,
    /// What the manager says now.
    state: State,
}

fn commands(sys: &Sys, s: &Service, w: &Want) -> Vec<Vec<String>> {
    let (enable, start, restart, changed) = (w.enable, w.start, w.restart, w.changed);
    let h = s.handle();
    let mut out: Vec<Vec<String>> = Vec::new();
    match s.manager {
        Manager::Brew => {
            let b = sys.tools.path("brew");
            let verb = if restart { "restart" } else { "start" };
            out.push(vec![b, "services".into(), verb.into(), h]);
        }
        Manager::SystemdUser => {
            let c = sys.tools.path("systemctl");
            let user = |rest: &[&str]| {
                let mut v = vec![c.clone(), "--user".to_string()];
                v.extend(rest.iter().map(|x| x.to_string()));
                v
            };
            if s.unit.is_some() && (changed || enable) {
                out.push(user(&["daemon-reload"]));
            }
            match (enable, start, restart) {
                (true, true, _) => out.push(user(&["enable", "--now", "--", &h])),
                (true, false, true) => {
                    out.push(user(&["enable", "--", &h]));
                    out.push(user(&["restart", "--", &h]));
                }
                (true, false, false) => out.push(user(&["enable", "--", &h])),
                (false, true, _) => out.push(user(&["start", "--", &h])),
                (false, false, true) => out.push(user(&["restart", "--", &h])),
                _ => {}
            }
        }
        Manager::Launchd => {
            let c = sys.tools.path("launchctl");
            let domain = format!("gui/{}", sys.uid);
            let target = format!("{domain}/{h}");
            let plist = sys
                .home
                .join("Library/LaunchAgents")
                .join(s.file_name())
                .to_string_lossy()
                .into_owned();
            let mut loaded = w.state.loaded;
            let mut reloaded = false;
            // A running job whose plist (or config) changed is reloaded: out of the domain,
            // then bootstrapped again from the new plist. Without a unit of ours there is no
            // plist to reload, so it is just kicked.
            if restart && loaded && s.unit.is_some() {
                out.push(vec![c.clone(), "bootout".into(), target.clone()]);
                loaded = false;
                reloaded = true;
            }
            // A disabled job (loaded or not) is enabled first.
            if enable {
                out.push(vec![c.clone(), "enable".into(), target.clone()]);
            }
            // Bootstrap only a job that is not loaded, and always before a kickstart.
            if !loaded && (enable || start || restart) {
                out.push(vec![c.clone(), "bootstrap".into(), domain, plist]);
            }
            if start || reloaded {
                out.push(vec![c, "kickstart".into(), target]);
            } else if restart {
                out.push(vec![c, "kickstart".into(), "-k".into(), target]);
            }
        }
        Manager::WindowsStartup => {
            if enable {
                out.push(vec![
                    sys.tools.path("reg"),
                    "add".into(),
                    RUN_KEY.into(),
                    "/v".into(),
                    s.name.clone(),
                    "/d".into(),
                    s.name.clone(),
                    "/f".into(),
                ]);
            }
            if restart {
                out.push(vec![
                    sys.tools.path("taskkill"),
                    "/IM".into(),
                    format!("{}.exe", s.name),
                    "/F".into(),
                ]);
            }
            if start || restart {
                // No `cmd /c start`: the name never goes through a shell. PowerShell's
                // `Start-Process` detaches the program, so the run does not wait for it.
                out.push(vec![
                    sys.tools.path("powershell"),
                    "-NoProfile".into(),
                    "-NonInteractive".into(),
                    "-Command".into(),
                    "Start-Process".into(),
                    "-FilePath".into(),
                    format!("{}.exe", s.name),
                ]);
            }
        }
    }
    out
}

/// Run a step's commands in order; the first failure stops it.
pub fn apply(sys: &Sys, step: &Step) -> Result<(), String> {
    for a in &step.argv {
        let o = run(sys, a)?;
        if !o.success() {
            return Err(format!("`{}` failed: {}", a.join(" "), o.stderr.trim()));
        }
    }
    Ok(())
}

/// [`apply`], then record what it did in the ledger (saved right away): a service dots enabled
/// is owned, one it took down is forgotten.
pub fn apply_recorded(sys: &Sys, ledger: &mut Ledger, step: &Step) -> Result<(), String> {
    apply(sys, step)?;
    if let Some(o) = &step.owned {
        ledger.owned.insert(o.key(), o.clone());
    }
    if let Some(k) = &step.release {
        ledger.owned.remove(k);
        ledger.config.remove(k);
    }
    ledger.save()
}

/// How `status` sees a service.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Health {
    Ok,
    NotEnabled,
    Stopped,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub package: String,
    pub name: String,
    pub manager: Manager,
    pub health: Health,
    pub state: State,
}

pub fn status(services: &[Service], states: &BTreeMap<String, State>) -> Vec<Row> {
    services
        .iter()
        .map(|s| {
            let st = states.get(&s.key()).copied().unwrap_or(State::GONE);
            let health = if st.run == Run::Failed {
                Health::Failed
            } else if s.enabled && !st.enabled {
                Health::NotEnabled
            } else if s.running && st.run != Run::Running {
                Health::Stopped
            } else {
                Health::Ok
            };
            Row {
                package: s.package.clone(),
                name: s.name.clone(),
                manager: s.manager,
                health,
                state: st,
            }
        })
        .collect()
}
