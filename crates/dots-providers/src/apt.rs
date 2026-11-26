//! The `apt` provider (Debian family). Talks to `apt-get`, `apt-mark`, `dpkg-query` and
//! `apt-cache` by absolute path, never via `PATH`.
//!
//! Root: every action is marked `needs_root`; the engine decides how to obtain it (it may put a
//! `sudo` prefix in front of `Action.argv`). Argv therefore carries `DEBIAN_FRONTEND` through
//! `/usr/bin/env`, which survives `sudo`'s environment reset.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use dots_facts::Facts;

use crate::apt_repo::{self, AptRepo, Source};
use crate::{
    Action, Availability, BinEntry, Caps, Cmd, Ctx, Installed, Latest, Op, OpKind, Outcome,
    Provider, ProviderError, Result, Spec, Support,
};

const APT_GET: &str = "/usr/bin/apt-get";
const APT_MARK: &str = "/usr/bin/apt-mark";
const APT_CACHE: &str = "/usr/bin/apt-cache";
const DPKG_QUERY: &str = "/usr/bin/dpkg-query";
const GPG: &str = "/usr/bin/gpg";
const SH: &str = "/bin/sh";
const ENV: &str = "/usr/bin/env";
/// `argv[0]` (after a `sudo` prefix) of the actions that add or remove a repository. `exec`
/// turns them into a real command; `plan -v` shows them as they are.
const REPO_CMD: &str = "dots:apt-repo";
const CHANGED: &str = "DOTS_REPO_CHANGED";
/// Env assignment in an `apt-get update` action naming the repository it refreshes.
const UPDATE_FOR: &str = "DOTS_APT_REPO";

/// Runs as root: write the key, sources and preferences files that differ, say whether
/// anything changed. Arguments: id keyring sources prefs sources-text prefs-text stale-keyring.
/// The verified key arrives on stdin: there is no path a user could swap between the check and
/// the copy. `stale-keyring` is the same key in the other format (`.asc` vs `.gpg`), removed
/// when the key's format changed.
const ADD_SCRIPT: &str = r#"set -eu
id=$1 keyring=$2 sources=$3 prefs=$4 srctext=$5 preftext=$6 stale=${7:-}
changed=0
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
cat > "$tmp/key"
[ -s "$tmp/key" ] || { echo "dots: no key on stdin" >&2; exit 1; }
put() {
  if [ -f "$1" ] && cmp -s "$1" "$2"; then return 0; fi
  mkdir -p "$(dirname "$1")"
  install -m 0644 "$2" "$1"
  changed=1
}
printf '%s
' "$srctext" > "$tmp/sources"
put "$keyring" "$tmp/key"
if [ -n "$stale" ] && [ -f "$stale" ]; then rm -f "$stale"; changed=1; fi
put "$sources" "$tmp/sources"
if [ -n "$preftext" ]; then
  printf '%s
' "$preftext" > "$tmp/prefs"
  put "$prefs" "$tmp/prefs"
elif [ -f "$prefs" ] && head -n 1 "$prefs" | grep -q '^# Managed by dots'; then
  rm -f "$prefs"
  changed=1
fi
if [ "$changed" = 1 ]; then echo DOTS_REPO_CHANGED; else echo DOTS_REPO_UNCHANGED; fi
"#;

/// Runs as root: remove a repository's files, but only when its sources file is ours.
const REMOVE_SCRIPT: &str = r#"set -eu
if [ -f "$2" ] && head -n 1 "$2" | grep -q '^# Managed by dots'; then
  rm -f "$1" "${1%.asc}.gpg" "$2" "$3"
fi
"#;
const SYSTEM_PATH: &str = "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin";
const BIN_DIRS: &[&str] = &[
    "/usr/bin",
    "/bin",
    "/usr/sbin",
    "/sbin",
    "/usr/local/bin",
    "/usr/local/sbin",
    "/usr/games",
];

/// Where apt keeps its package lists. `DOTS_APT_LISTS=<dir>` in the context env redirects the
/// freshness probe (tests); without it the probe only runs where `apt-get` is probed for.
const LISTS_DIR: &str = "/var/lib/apt/lists";
/// Lists older than this are refreshed once before the first install of a run.
/// `DOTS_APT_LISTS_MAX_AGE_SECS` overrides it.
const LISTS_MAX_AGE_SECS: u64 = 24 * 60 * 60;

/// `-f` format for the single batched query: name, version, `want+status+eflag`
/// (`ii ` installed, `hi ` installed and held).
const QUERY_FORMAT: &str = "${Package}\\t${Version}\\t${db:Status-Abbrev}\\n";

#[derive(Debug, Clone)]
pub struct AptProvider {
    check_binary: bool,
    /// Repositories whose files changed and whose `apt-get update` has not succeeded yet.
    /// Mirrored under `<cache_dir>/apt-needs-update/<id>` so a failed update is retried by
    /// the next run, not forgotten.
    pending_update: Arc<Mutex<BTreeSet<String>>>,
    /// Repositories whose preparation failed in this run: their packages are not installed.
    failed_repos: Arc<Mutex<BTreeSet<String>>>,
    /// The package lists were checked (and refreshed if stale) in this run.
    lists_checked: Arc<AtomicBool>,
    /// A stale-cache `apt-get update` already succeeded in this run (one is enough: the
    /// per-package retries after a failed batch do not refresh again).
    cache_refreshed: Arc<AtomicBool>,
}

impl Default for AptProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl AptProvider {
    pub fn new() -> Self {
        AptProvider {
            check_binary: true,
            pending_update: Arc::default(),
            failed_repos: Arc::default(),
            lists_checked: Arc::default(),
            cache_refreshed: Arc::default(),
        }
    }

    /// For hermetic tests on hosts without `/usr/bin/apt-get`: skip the filesystem probe.
    pub fn without_binary_check() -> Self {
        AptProvider {
            check_binary: false,
            pending_update: Arc::default(),
            failed_repos: Arc::default(),
            lists_checked: Arc::default(),
            cache_refreshed: Arc::default(),
        }
    }

    fn base_cmd(program: &str, ctx: &Ctx) -> Cmd {
        let mut env = ctx.env.clone();
        let path = env.get("PATH").cloned().unwrap_or_default();
        let path = if path.is_empty() {
            SYSTEM_PATH.to_string()
        } else {
            format!("{path}:{SYSTEM_PATH}")
        };
        env.insert("PATH".into(), path);
        env.insert("LC_ALL".into(), "C".into());
        env.insert("DEBIAN_FRONTEND".into(), "noninteractive".into());
        Cmd::new(program).envs(&env)
    }

    /// Are the package lists missing, empty or older than the threshold? `None` when this
    /// provider does not probe (no `apt-get` check and no `DOTS_APT_LISTS`).
    fn lists_stale(&self, ctx: &Ctx) -> Option<bool> {
        let dir = match ctx.env.get("DOTS_APT_LISTS").filter(|d| !d.is_empty()) {
            Some(d) => PathBuf::from(d),
            None if self.check_binary => PathBuf::from(LISTS_DIR),
            None => return None,
        };
        let max_age = ctx
            .env
            .get("DOTS_APT_LISTS_MAX_AGE_SECS")
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(LISTS_MAX_AGE_SECS);
        let Ok(rd) = std::fs::read_dir(&dir) else {
            return Some(true);
        };
        // A usable list is a `*_Packages*` or `*_InRelease` file; `lock` and `partial/` are not.
        let has_lists = rd.flatten().any(|e| {
            let n = e.file_name();
            let n = n.to_string_lossy();
            n.contains("_Packages") || n.ends_with("_InRelease")
        });
        if !has_lists {
            return Some(true);
        }
        let age = std::fs::metadata(&dir)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok());
        Some(age.is_none_or(|a| a.as_secs() > max_age))
    }

    fn query(ctx: &Ctx, cmd: Cmd) -> Result<crate::Output> {
        let out = ctx.runner.run(&cmd)?;
        if !out.success() {
            return Err(ProviderError::CommandFailed {
                command: cmd.line(),
                status: out.status,
            });
        }
        Ok(out)
    }
}

fn valid_name(n: &str) -> bool {
    let mut cs = n.chars();
    cs.next().is_some_and(|c| c.is_ascii_alphanumeric())
        && cs.all(|c| c.is_ascii_alphanumeric() || "+-.:_".contains(c))
}

fn valid_version(v: &str) -> bool {
    !v.is_empty()
        && v.chars()
            .all(|c| c.is_ascii_alphanumeric() || "+-.:~_".contains(c))
}

fn parse_installed(stdout: &str) -> BTreeMap<String, Installed> {
    let mut map = BTreeMap::new();
    for line in stdout.lines() {
        let mut f = line.trim_end_matches('\r').split('\t');
        let (Some(name), Some(version), Some(abbrev)) = (f.next(), f.next(), f.next()) else {
            continue;
        };
        let mut st = abbrev.chars();
        let (want, status) = (st.next(), st.next());
        if status != Some('i') || !matches!(want, Some('i' | 'h')) || name.is_empty() {
            continue;
        }
        map.insert(
            name.to_string(),
            Installed {
                version: version.to_string(),
                pinned: want == Some('h'),
                source: None,
            },
        );
    }
    map
}

fn parse_policy(stdout: &str) -> BTreeMap<String, Latest> {
    let mut map = BTreeMap::new();
    let mut current: Option<String> = None;
    for line in stdout.lines() {
        if !line.starts_with(' ') {
            current = line.strip_suffix(':').map(str::to_string);
        } else if let (Some(name), Some(v)) = (&current, line.trim().strip_prefix("Candidate:")) {
            let v = v.trim();
            if !v.is_empty() && v != "(none)" {
                map.insert(name.clone(), Latest { version: v.into() });
            }
        }
    }
    map
}

/// The packages named on the `Remv` lines of `apt-get -s remove` (`Remv libfoo [1.2-3] (...)`,
/// possibly `libfoo:amd64`).
fn parse_simulated_removals(stdout: &str) -> Vec<String> {
    stdout
        .lines()
        .filter_map(|l| l.strip_prefix("Remv "))
        .filter_map(|l| l.split_whitespace().next())
        .map(str::to_string)
        .collect()
}

fn needs_update(output: &str) -> bool {
    [
        "Unable to locate package",
        "has no installation candidate",
        "Unable to correct missing",
        "Version '",
    ]
    .iter()
    .any(|m| output.contains(m))
        || output.contains("404  Not Found")
}

impl Provider for AptProvider {
    fn id(&self) -> &'static str {
        "apt"
    }

    fn caps(&self) -> Caps {
        Caps {
            batch_install: true,
            exact_version: Support::Limited,
            pin: true,
            needs_root: true,
            uninstall: true,
            exclusive_lock: true,
        }
    }

    fn availability(&self, facts: &Facts) -> Availability {
        let debian = facts.os == "linux"
            && [facts.distro.as_deref(), facts.distro_family.as_deref()]
                .into_iter()
                .flatten()
                .any(|d| matches!(d, "debian" | "ubuntu"));
        let present = !self.check_binary || Path::new(APT_GET).is_file();
        if debian && present {
            Availability::Ready {
                bin_path: PathBuf::from(APT_GET),
            }
        } else {
            Availability::Unsupported
        }
    }

    fn installed(&self, ctx: &Ctx) -> Result<BTreeMap<String, Installed>> {
        let cmd = Self::base_cmd(DPKG_QUERY, ctx).args(["-W", &format!("-f={QUERY_FORMAT}")]);
        Ok(parse_installed(&Self::query(ctx, cmd)?.stdout))
    }

    fn roots(&self, ctx: &Ctx) -> Result<Option<std::collections::BTreeSet<String>>> {
        let cmd = Self::base_cmd("apt-mark", ctx).args(["showmanual"]);
        let out = Self::query(ctx, cmd)?;
        Ok(Some(
            out.stdout
                .lines()
                .map(|l| l.trim().split(':').next().unwrap_or("").to_string())
                .filter(|l| !l.is_empty())
                .collect(),
        ))
    }

    fn latest(&self, ctx: &Ctx, specs: &[Spec]) -> Result<BTreeMap<String, Latest>> {
        if specs.is_empty() {
            return Ok(BTreeMap::new());
        }
        let names: BTreeSet<&str> = specs.iter().map(|s| s.name.as_str()).collect();
        if let Some(bad) = names.iter().find(|n| !valid_name(n)) {
            return Err(ProviderError::Other(format!(
                "invalid apt package name `{bad}`"
            )));
        }
        let cmd = Self::base_cmd(APT_CACHE, ctx)
            .arg("policy")
            .args(names.iter().copied());
        let mut map = parse_policy(&Self::query(ctx, cmd)?.stdout);
        map.retain(|k, _| names.contains(k.as_str()));
        Ok(map)
    }

    /// A package that declares a `repo` while that repository's sources file is absent still
    /// comes from the distribution archive (`git` preinstalled on Ubuntu, `repo = { ppa }` in
    /// the package): upgrade it so the repository is added and the package taken from it.
    /// Once dots has written the file this is `None`, so the next plan is empty.
    fn stale_reason(&self, ctx: &Ctx, spec: &Spec) -> Option<String> {
        let dir = match ctx
            .env
            .get("DOTS_APT_SOURCES_DIR")
            .filter(|d| !d.is_empty())
        {
            Some(d) => PathBuf::from(d),
            None if self.check_binary => PathBuf::from(apt_repo::SOURCES_DIR),
            None => return None,
        };
        let repo = AptRepo::from_spec(spec).ok()??;
        let file = format!("dots-{}.sources", repo.id);
        if dir.join(&file).exists() {
            return None;
        }
        let shown = match &repo.source {
            Source::Ppa { owner, name } => format!("{owner}/{name}"),
            Source::Url { url, .. } => url.clone(),
        };
        Some(format!("repo {shown} not configured"))
    }

    fn plan(&self, ops: &[Op]) -> Result<Vec<Action>> {
        self.plan_inner(ops, None)
    }

    fn plan_with_wanted(&self, ops: &[Op], wanted: &[Spec]) -> Result<Vec<Action>> {
        self.plan_inner(ops, Some(wanted))
    }

    fn exec(&self, ctx: &Ctx, action: &Action) -> Result<Outcome> {
        if ctx.dry_run {
            return Ok(Outcome {
                success: true,
                log: None,
            });
        }
        if let Some(at) = action.argv.iter().position(|a| a == REPO_CMD) {
            return self.exec_repo(ctx, action, at);
        }
        if is_update(&action.argv) {
            return self.exec_update(ctx, action);
        }
        // Installing, upgrading, holding or releasing a package of a repository that could not
        // be prepared would only fail: skip it. A removal still runs.
        if !matches!(action.kind, OpKind::Remove)
            && let Some(id) = self.failed_repo_of(action)
        {
            let log = format!(
                "$ {}\n# skipped: repository `{id}` could not be prepared\n",
                action.argv.join(" ")
            );
            let kind = format!("{:?}", action.kind).to_lowercase();
            return Ok(Outcome {
                success: false,
                log: Some(write_log(
                    ctx,
                    &format!("apt-{id}"),
                    &format!("{kind}-skipped"),
                    &log,
                )?),
            });
        }
        let cmd = Self::base_cmd("", ctx);
        let run = |argv: &[String]| -> Result<crate::Output> {
            let mut c =
                Cmd::from_argv(argv).ok_or_else(|| ProviderError::Other("empty argv".into()))?;
            c.env = cmd.env.clone();
            ctx.runner.run(&c)
        };
        let mut log = String::new();
        // The first install of a run on a machine without (fresh) package lists: refresh once.
        // A failed refresh is logged and the install tried anyway.
        if matches!(action.kind, OpKind::Install | OpKind::Upgrade)
            && !ctx.offline
            && !self.lists_checked.swap(true, Ordering::SeqCst)
            && self.lists_stale(ctx) == Some(true)
            && let Some(at) = action.argv.iter().position(|a| a == ENV)
        {
            let mut update: Vec<String> = action.argv[..at].to_vec();
            update.extend(
                [ENV, "DEBIAN_FRONTEND=noninteractive", APT_GET, "update"].map(String::from),
            );
            log.push_str(&format!(
                "# package lists are missing or stale; refreshing\n$ {}\n",
                update.join(" ")
            ));
            let upd = run(&update)?;
            append(&mut log, &upd);
            log.push('\n');
        }
        log.push_str(&format!("$ {}\n", action.argv.join(" ")));
        let mut out = run(&action.argv)?;
        append(&mut log, &out);

        // A stale package cache: refresh once and retry, unless we are offline.
        let installing = matches!(action.kind, OpKind::Install | OpKind::Upgrade);
        if !out.success()
            && installing
            && !ctx.offline
            && !self.cache_refreshed.load(Ordering::SeqCst)
            && needs_update(&format!("{}{}", out.stdout, out.stderr))
            && let Some(at) = action.argv.iter().position(|a| a == ENV)
        {
            let mut update: Vec<String> = action.argv[..at].to_vec();
            update.extend(
                [ENV, "DEBIAN_FRONTEND=noninteractive", APT_GET, "update"].map(String::from),
            );
            log.push_str(&format!(
                "\n# package cache looks stale; refreshing\n$ {}\n",
                update.join(" ")
            ));
            let upd = run(&update)?;
            append(&mut log, &upd);
            if upd.success() {
                self.cache_refreshed.store(true, Ordering::SeqCst);
                log.push_str(&format!("\n# retrying\n$ {}\n", action.argv.join(" ")));
                out = run(&action.argv)?;
                append(&mut log, &out);
            }
        }

        let stem = match action.ops.as_slice() {
            [one] => one.spec.name.clone(),
            _ => "apt".to_string(),
        };
        let kind = format!("{:?}", action.kind).to_lowercase();
        let path = ctx.log_dir.join(format!("{stem}.{kind}.log"));
        std::fs::create_dir_all(&ctx.log_dir)
            .and_then(|()| std::fs::write(&path, log))
            .map_err(|e| ProviderError::Other(format!("cannot write {}: {e}", path.display())))?;
        Ok(Outcome {
            success: out.success(),
            log: Some(path),
        })
    }

    /// One `apt-get install` of several packages fails as a whole when a single name is
    /// unknown: install them one by one instead, so only the bad one fails.
    fn retry_each(&self, action: &Action) -> Option<Vec<Action>> {
        if action.ops.len() < 2
            || !matches!(action.kind, OpKind::Install | OpKind::Upgrade)
            || action.argv.iter().any(|a| a == REPO_CMD)
            || is_update(&action.argv)
        {
            return None;
        }
        let mut each = Vec::new();
        for op in &action.ops {
            push_groups(&mut each, std::slice::from_ref(op), &[action.kind]);
        }
        Some(each)
    }

    fn removal_collateral(&self, ctx: &Ctx, action: &Action) -> Result<Vec<String>> {
        if action.kind != OpKind::Remove
            || action.argv.iter().any(|a| a == REPO_CMD)
            || !action.argv.iter().any(|a| a == APT_GET)
        {
            return Ok(Vec::new());
        }
        let mut argv: Vec<String> = [
            ENV,
            "DEBIAN_FRONTEND=noninteractive",
            APT_GET,
            "-s",
            "remove",
        ]
        .map(String::from)
        .to_vec();
        argv.extend(action.ops.iter().map(|o| o.spec.name.clone()));
        let mut cmd =
            Cmd::from_argv(&argv).ok_or_else(|| ProviderError::Other("empty argv".into()))?;
        cmd.env = Self::base_cmd("", ctx).env;
        let out = Self::query(ctx, cmd)?;
        Ok(parse_simulated_removals(&out.stdout)
            .into_iter()
            .filter(|n| {
                let bare = n.split(':').next().unwrap_or(n);
                !action
                    .ops
                    .iter()
                    .any(|o| o.spec.name == *n || o.spec.name == bare)
            })
            .collect())
    }

    fn bins(&self, ctx: &Ctx, installed: &BTreeMap<String, Installed>) -> Result<Vec<BinEntry>> {
        if installed.is_empty() {
            return Ok(Vec::new());
        }
        // One call for all packages; the loop labels each package's file list.
        let script = "for p in \"$@\"; do printf '== %s\\n' \"$p\"; \
                      /usr/bin/dpkg-query -L \"$p\" 2>/dev/null; done";
        let cmd = Self::base_cmd("/bin/sh", ctx)
            .args(["-c", script, "sh"])
            .args(installed.keys().cloned());
        let out = Self::query(ctx, cmd)?;
        let mut entries = Vec::new();
        let mut seen = BTreeSet::new();
        let mut pkg = String::new();
        for line in out.stdout.lines() {
            if let Some(p) = line.strip_prefix("== ") {
                pkg = p.to_string();
                continue;
            }
            let path = Path::new(line);
            let in_bin_dir = path
                .parent()
                .is_some_and(|d| BIN_DIRS.iter().any(|b| d == Path::new(b)));
            let Some(file) = path.file_name().and_then(|f| f.to_str()) else {
                continue;
            };
            if !in_bin_dir || path.is_dir() || !seen.insert((pkg.clone(), line.to_string())) {
                continue;
            }
            entries.push(BinEntry {
                package: pkg.clone(),
                bin: file.to_string(),
                path: path.to_path_buf(),
            });
        }
        Ok(entries)
    }

    fn owner_of(&self, ctx: &Ctx, paths: &[PathBuf]) -> Result<BTreeMap<PathBuf, String>> {
        if paths.is_empty() {
            return Ok(BTreeMap::new());
        }
        let cmd = Self::base_cmd(DPKG_QUERY, ctx)
            .arg("-S")
            .args(paths.iter().map(|p| p.to_string_lossy().into_owned()));
        // `-S` exits 1 when any path has no owner, but still prints the ones it found.
        let out = ctx.runner.run(&cmd)?;
        let mut map = BTreeMap::new();
        for line in out.stdout.lines() {
            let Some((owners, path)) = line.split_once(": ") else {
                continue;
            };
            let first = owners.split(", ").next().unwrap_or(owners);
            let name = first.split(':').next().unwrap_or(first);
            let path = PathBuf::from(path);
            if paths.contains(&path) {
                map.entry(path).or_insert_with(|| name.to_string());
            }
        }
        Ok(map)
    }
}

impl AptProvider {
    /// Repository id -> names of the specs in `active` that install from it. The engine can
    /// use this to see which repositories still have users.
    pub fn repo_users(&self, active: &[Spec]) -> BTreeMap<String, Vec<String>> {
        let mut users: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for spec in active {
            if let Ok(Some(repo)) = AptRepo::from_spec(spec) {
                users.entry(repo.id).or_default().push(spec.name.clone());
            }
        }
        users
    }

    /// `wanted`: every spec still wanted after this run. A repository is dropped by a
    /// `Remove` only when no wanted spec (and no op in this batch that installs) names it.
    /// `None` means the caller does not know; then only the `repo_keep` option protects it.
    fn plan_inner(&self, ops: &[Op], wanted: Option<&[Spec]>) -> Result<Vec<Action>> {
        for op in ops {
            if !valid_name(&op.spec.name) {
                return Err(ProviderError::Other(format!(
                    "invalid apt package name `{}`",
                    op.spec.name
                )));
            }
            if let Some(v) = &op.version
                && !valid_version(v)
            {
                return Err(ProviderError::Other(format!(
                    "invalid apt version `{v}` for `{}`",
                    op.spec.name
                )));
            }
        }
        let still_used = wanted.map(|w| self.repo_users(w)).unwrap_or_default();
        // Repositories: a repository is added for the packages that install from it and
        // dropped with the last package that names it.
        let mut add: BTreeMap<String, AptRepo> = BTreeMap::new();
        let mut drop: BTreeMap<String, AptRepo> = BTreeMap::new();
        // Install/Upgrade ops by the repository they need; the rest install from the archive.
        let mut by_repo: BTreeMap<String, Vec<Op>> = BTreeMap::new();
        let mut plain: Vec<Op> = Vec::new();
        for op in ops {
            let repo = AptRepo::from_spec(&op.spec)?;
            let installing = matches!(op.kind, OpKind::Install | OpKind::Upgrade);
            match (&repo, op.kind) {
                (Some(repo), OpKind::Install | OpKind::Upgrade) => {
                    by_repo.entry(repo.id.clone()).or_default().push(op.clone());
                    add.insert(repo.id.clone(), repo.clone());
                }
                (Some(repo), OpKind::Remove)
                    if !op.spec.flag("repo_keep", false) && !still_used.contains_key(&repo.id) =>
                {
                    drop.insert(repo.id.clone(), repo.clone());
                }
                (None, _) if installing => plain.push(op.clone()),
                _ => {}
            }
        }
        drop.retain(|id, _| !add.contains_key(id));
        let mut actions = Vec::new();
        // Archive packages first: a broken repository cannot hold them back.
        push_groups(&mut actions, &plain, &[OpKind::Install, OpKind::Upgrade]);
        // Each repository: prepare, refresh, install its own packages. Every one of these
        // carries only that repository's ops, so a failure affects just those packages.
        for (id, repo) in &add {
            let own = &by_repo[id];
            let mut a = repo_action("add", repo);
            a.ops = own.clone();
            actions.push(a);
            actions.push(Action {
                kind: OpKind::Install,
                ops: own.clone(),
                argv: [
                    ENV,
                    "DEBIAN_FRONTEND=noninteractive",
                    &format!("{UPDATE_FOR}={id}"),
                    APT_GET,
                    "update",
                ]
                .map(String::from)
                .to_vec(),
                needs_root: true,
            });
            push_groups(&mut actions, own, &[OpKind::Install, OpKind::Upgrade]);
        }
        let rest: Vec<Op> = ops
            .iter()
            .filter(|o| !matches!(o.kind, OpKind::Install | OpKind::Upgrade))
            .cloned()
            .collect();
        push_groups(&mut actions, &rest, &[OpKind::Remove]);
        // Holds are grouped per repository, so one that failed to prepare skips only its own.
        for kind in [OpKind::Pin, OpKind::Unpin] {
            let mut groups: BTreeMap<Option<String>, Vec<Op>> = BTreeMap::new();
            for op in rest.iter().filter(|o| o.kind == kind) {
                let repo = AptRepo::from_spec(&op.spec)?.map(|r| r.id);
                groups.entry(repo).or_default().push(op.clone());
            }
            for group in groups.values() {
                push_groups(&mut actions, group, &[kind]);
            }
        }
        for repo in drop.values() {
            actions.push(repo_action("remove", repo));
        }
        Ok(actions)
    }

    /// The repository whose preparation failed in this run and that `action`'s ops need.
    fn failed_repo_of(&self, action: &Action) -> Option<String> {
        let failed = self.failed_repos.lock().ok()?;
        action.ops.iter().find_map(|op| {
            let repo = AptRepo::from_spec(&op.spec).ok()??;
            failed.contains(&repo.id).then_some(repo.id)
        })
    }

    fn mark_failed(&self, id: &str) {
        if let Ok(mut f) = self.failed_repos.lock() {
            f.insert(id.to_string());
        }
    }

    fn marker_dir(ctx: &Ctx) -> Option<PathBuf> {
        (!ctx.cache_dir.as_os_str().is_empty()).then(|| ctx.cache_dir.join("apt-needs-update"))
    }

    /// Record that `id`'s files changed and an `apt-get update` must succeed before its
    /// packages are installed. Survives this process.
    fn mark_needs_update(&self, ctx: &Ctx, id: &str) {
        if let Ok(mut p) = self.pending_update.lock() {
            p.insert(id.to_string());
        }
        if let Some(dir) = Self::marker_dir(ctx) {
            let _ = std::fs::create_dir_all(&dir).and_then(|()| std::fs::write(dir.join(id), ""));
        }
    }

    fn needs_update_ids(&self, ctx: &Ctx) -> BTreeSet<String> {
        let mut ids = self
            .pending_update
            .lock()
            .map(|p| p.clone())
            .unwrap_or_default();
        if let Some(dir) = Self::marker_dir(ctx)
            && let Ok(rd) = std::fs::read_dir(dir)
        {
            ids.extend(
                rd.flatten()
                    .filter_map(|e| e.file_name().into_string().ok()),
            );
        }
        ids
    }

    /// Cleared only after a successful `apt-get update`, which refreshes every repository.
    fn clear_needs_update(&self, ctx: &Ctx) {
        if let Ok(mut p) = self.pending_update.lock() {
            p.clear();
        }
        if let Some(dir) = Self::marker_dir(ctx) {
            let _ = std::fs::remove_dir_all(dir);
        }
    }

    /// `apt-get update` for added or changed repositories, whenever an earlier update for
    /// one of them has not yet succeeded.
    fn exec_update(&self, ctx: &Ctx, action: &Action) -> Result<Outcome> {
        let mut log = format!("$ {}\n", action.argv.join(" "));
        let repo_id = action
            .argv
            .iter()
            .find_map(|a| a.strip_prefix(&format!("{UPDATE_FOR}=")))
            .map(str::to_string);
        if self.needs_update_ids(ctx).is_empty() {
            log.push_str("# repositories unchanged; no update needed\n");
            return Ok(Outcome {
                success: true,
                log: Some(write_log(ctx, "apt", "update", &log)?),
            });
        }
        let mut c = Cmd::from_argv(&action.argv)
            .ok_or_else(|| ProviderError::Other("empty argv".into()))?;
        c.env = Self::base_cmd("", ctx).env;
        let out = ctx.runner.run(&c)?;
        append(&mut log, &out);
        if out.success() {
            // `apt-get update` refreshes every list.
            self.lists_checked.store(true, Ordering::SeqCst);
            self.clear_needs_update(ctx);
        } else if let Some(id) = repo_id {
            self.mark_failed(&id);
        }
        Ok(Outcome {
            success: out.success(),
            log: Some(write_log(ctx, "apt", "update", &log)?),
        })
    }

    fn exec_repo(&self, ctx: &Ctx, action: &Action, at: usize) -> Result<Outcome> {
        let repo = AptRepo::from_tokens(&action.argv[(at + 2).min(action.argv.len())..])?;
        let res = self.exec_repo_inner(ctx, action, at, &repo);
        let verb = action.argv.get(at + 1).map(String::as_str).unwrap_or("");
        if verb == "add" && !ctx.dry_run && !matches!(&res, Ok(o) if o.success) {
            self.mark_failed(&repo.id);
        }
        res
    }

    fn exec_repo_inner(
        &self,
        ctx: &Ctx,
        action: &Action,
        at: usize,
        repo: &AptRepo,
    ) -> Result<Outcome> {
        let verb = action.argv.get(at + 1).map(String::as_str).unwrap_or("");
        if ctx.dry_run {
            return Ok(Outcome {
                success: true,
                log: None,
            });
        }
        let prefix = action.argv[..at].to_vec();
        let mut log = format!("$ {}\n", action.argv.join(" "));
        let mut argv = prefix;
        argv.extend([SH, "-c"].map(String::from));
        let (script, kind) = match verb {
            "add" => (ADD_SCRIPT, "repo-add"),
            "remove" => (REMOVE_SCRIPT, "repo-remove"),
            other => {
                return Err(ProviderError::Other(format!(
                    "apt: unknown repository action `{other}`"
                )));
            }
        };
        argv.push(script.to_string());
        argv.push("dots-apt-repo".into());
        let mut stdin = crate::Stdin::Null;
        if verb == "add" {
            let (key, suite, binary) = self.prepare_key(ctx, repo, &mut log)?;
            // The verified bytes go to the root script on stdin.
            stdin = crate::Stdin::Bytes(key);
            // A binary (dearmored) key is a `.gpg` keyring, an armored one `.asc`.
            let (keyring, stale) = if binary {
                (repo.binary_keyring_path(), repo.keyring_path())
            } else {
                (repo.keyring_path(), repo.binary_keyring_path())
            };
            argv.extend([
                repo.id.clone(),
                keyring.clone(),
                repo.sources_path(),
                repo.prefs_path(),
                repo.sources_text_for(suite.as_deref(), &keyring)
                    .trim_end()
                    .to_string(),
                repo.prefs_text().unwrap_or_default().trim_end().to_string(),
                stale,
            ]);
        } else {
            argv.extend([repo.keyring_path(), repo.sources_path(), repo.prefs_path()]);
        }
        let mut c =
            Cmd::from_argv(&argv).ok_or_else(|| ProviderError::Other("empty argv".into()))?;
        c.env = Self::base_cmd("", ctx).env;
        c.stdin = stdin;
        let out = ctx.runner.run(&c)?;
        append(&mut log, &out);
        if verb == "add" && out.success() && out.stdout.contains(CHANGED) {
            self.mark_needs_update(ctx, &repo.id);
        }
        Ok(Outcome {
            success: out.success(),
            log: Some(write_log(ctx, &format!("apt-{}", repo.id), kind, &log)?),
        })
    }

    /// Download the repository key and check its fingerprint, as the user. Nothing under
    /// `/etc` is touched here: a mismatch fails with `integrity-mismatch` before any write.
    /// The key never touches a path anyone could swap: gpg reads it on stdin, and the
    /// caller hands the same bytes to the root step on stdin. Returns the key, for a PPA the
    /// release codename, and whether the key is binary (dearmored) rather than ASCII-armored.
    fn prepare_key(
        &self,
        ctx: &Ctx,
        repo: &AptRepo,
        log: &mut String,
    ) -> Result<(Vec<u8>, Option<String>, bool)> {
        if ctx.offline {
            return Err(ProviderError::Other(format!(
                "apt repository `{}` needs its key downloaded, but dots is offline",
                repo.id
            )));
        }
        let (key_url, expected, suite) = match &repo.source {
            Source::Url {
                key, fingerprint, ..
            } => (key.clone(), fingerprint.clone(), None),
            Source::Ppa { owner, name } => {
                let api =
                    format!("https://api.launchpad.net/devel/~{owner}/+archive/ubuntu/{name}");
                let json = ctx.fetcher.fetch(&api)?;
                let fp = apt_repo::launchpad_fingerprint(&String::from_utf8_lossy(&json))
                    .ok_or_else(|| {
                        ProviderError::Other(format!(
                            "Launchpad did not report a signing key for ppa:{owner}/{name}"
                        ))
                    })?;
                let cmd = Self::base_cmd(SH, ctx).args([
                    "-c",
                    ". /etc/os-release && printf '%s' \"${UBUNTU_CODENAME:-$VERSION_CODENAME}\"",
                ]);
                let out = Self::query(ctx, cmd)?;
                let codename = out.stdout.trim().to_string();
                if codename.is_empty() {
                    return Err(ProviderError::Other(
                        "cannot tell this release's codename for the PPA".into(),
                    ));
                }
                let url = format!(
                    "https://keyserver.ubuntu.com/pks/lookup?op=get&options=mr&search=0x{fp}"
                );
                (url, fp, Some(codename))
            }
        };
        let key = ctx.fetcher.fetch(&key_url)?;
        let armored = String::from_utf8_lossy(&key).contains("BEGIN PGP PUBLIC KEY BLOCK");
        if !armored && !is_binary_public_key(&key) {
            return Err(ProviderError::Other(format!(
                "the key at {key_url} is neither an ASCII-armored nor a binary OpenPGP public key"
            )));
        }
        let cmd = Self::base_cmd(GPG, ctx)
            .args(["--batch", "--with-colons", "--show-keys", "-"])
            .stdin(crate::Stdin::Bytes(key.clone()));
        let out = Self::query(ctx, cmd)?;
        let got = apt_repo::primary_fingerprints(&out.stdout);
        log.push_str(&format!(
            "# key {key_url}: fingerprint {}\n",
            got.join(", ")
        ));
        if got.is_empty() || got.iter().any(|g| *g != expected) {
            let got = if got.is_empty() {
                "no key".to_string()
            } else {
                got.join(", ")
            };
            let _ = write_log(ctx, &format!("apt-{}", repo.id), "repo-add", log);
            return Err(ProviderError::Coded {
                code: "integrity-mismatch",
                message: format!(
                    "checksum mismatch for `{key_url}`: expected {expected}, got {got}"
                ),
                log: None,
            });
        }
        Ok((key, suite, !armored))
    }
}

/// One action per kind in `kinds` for the ops of that kind, in `kinds` order.
fn push_groups(actions: &mut Vec<Action>, ops: &[Op], kinds: &[OpKind]) {
    for &kind in kinds {
        let group: Vec<Op> = ops.iter().filter(|o| o.kind == kind).cloned().collect();
        if group.is_empty() {
            continue;
        }
        let pkgs = group.iter().map(|o| match (&o.version, kind) {
            (Some(v), OpKind::Install | OpKind::Upgrade) => format!("{}={v}", o.spec.name),
            _ => o.spec.name.clone(),
        });
        let apt_get = |sub: &[&str]| -> Vec<String> {
            [ENV, "DEBIAN_FRONTEND=noninteractive", APT_GET]
                .into_iter()
                .chain(sub.iter().copied())
                .map(String::from)
                .collect()
        };
        let mut argv = match kind {
            OpKind::Install => apt_get(&["install", "-y"]),
            OpKind::Upgrade => apt_get(&["install", "-y", "--only-upgrade"]),
            OpKind::Remove => apt_get(&["remove", "-y"]),
            OpKind::Pin => vec![APT_MARK.into(), "hold".into()],
            OpKind::Unpin => vec![APT_MARK.into(), "unhold".into()],
        };
        argv.extend(pkgs);
        actions.push(Action {
            kind,
            ops: group,
            argv,
            needs_root: true,
        });
    }
}

/// Does `bytes` start with an OpenPGP public-key packet (tag 6), the first packet of a
/// dearmored key? Old format headers are `10tttt..`, new format `11tttttt`.
fn is_binary_public_key(bytes: &[u8]) -> bool {
    match bytes.first() {
        Some(b) if b & 0xC0 == 0xC0 => b & 0x3F == 6,
        Some(b) if b & 0xC0 == 0x80 => (b >> 2) & 0x0F == 6,
        _ => false,
    }
}

/// `... apt-get update` exactly (a package named `update` follows `install`, not `apt-get`).
fn is_update(argv: &[String]) -> bool {
    argv.iter()
        .position(|a| a == APT_GET)
        .is_some_and(|i| argv.len() == i + 2 && argv[i + 1] == "update")
}

fn repo_action(verb: &str, repo: &AptRepo) -> Action {
    let mut argv = vec![REPO_CMD.to_string(), verb.to_string()];
    argv.extend(repo.to_tokens());
    argv.extend(repo.owned_paths().into_iter().map(|p| format!("file={p}")));
    Action {
        kind: if verb == "add" {
            OpKind::Install
        } else {
            OpKind::Remove
        },
        ops: Vec::new(),
        argv,
        needs_root: true,
    }
}

fn write_log(ctx: &Ctx, stem: &str, kind: &str, log: &str) -> Result<PathBuf> {
    let path = ctx.log_dir.join(format!("{stem}.{kind}.log"));
    std::fs::create_dir_all(&ctx.log_dir)
        .and_then(|()| std::fs::write(&path, log))
        .map_err(|e| ProviderError::Other(format!("cannot write {}: {e}", path.display())))?;
    Ok(path)
}

fn append(log: &mut String, out: &crate::Output) {
    log.push_str(&out.stdout);
    log.push_str(&out.stderr);
    log.push_str(&format!("# exit status {}\n", out.status));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_installed_and_hold() {
        let m = parse_installed("a\t1.0\tii \nb\t2\thi \nc\t3\trc \nd\t4\tun \nbad\n");
        assert_eq!(m.len(), 2);
        assert!(!m["a"].pinned && m["b"].pinned && m["b"].version == "2");
    }

    #[test]
    fn parses_policy() {
        let m = parse_policy(
            "hello:\n  Installed: (none)\n  Candidate: 2.10-2\n  Version table:\nnope:\n  Candidate: (none)\n",
        );
        assert_eq!(m.len(), 1);
        assert_eq!(m["hello"].version, "2.10-2");
    }
}
