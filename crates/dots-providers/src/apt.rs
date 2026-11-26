//! The `apt` provider (Debian family). Talks to `apt-get`, `apt-mark`, `dpkg-query` and
//! `apt-cache` by absolute path, never via `PATH`.
//!
//! Root: every action is marked `needs_root`; the engine decides how to obtain it (it may put a
//! `sudo` prefix in front of `Action.argv`). Argv therefore carries `DEBIAN_FRONTEND` through
//! `/usr/bin/env`, which survives `sudo`'s environment reset.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use dots_facts::Facts;

use crate::{
    Action, Availability, BinEntry, Caps, Cmd, Ctx, Installed, Latest, Op, OpKind, Outcome,
    Provider, ProviderError, Result, Spec, Support,
};

const APT_GET: &str = "/usr/bin/apt-get";
const APT_MARK: &str = "/usr/bin/apt-mark";
const APT_CACHE: &str = "/usr/bin/apt-cache";
const DPKG_QUERY: &str = "/usr/bin/dpkg-query";
const ENV: &str = "/usr/bin/env";
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

/// `-f` format for the single batched query: name, version, `want+status+eflag`
/// (`ii ` installed, `hi ` installed and held).
const QUERY_FORMAT: &str = "${Package}\\t${Version}\\t${db:Status-Abbrev}\\n";

#[derive(Debug, Clone)]
pub struct AptProvider {
    check_binary: bool,
}

impl Default for AptProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl AptProvider {
    pub fn new() -> Self {
        AptProvider { check_binary: true }
    }

    /// For hermetic tests on hosts without `/usr/bin/apt-get`: skip the filesystem probe.
    pub fn without_binary_check() -> Self {
        AptProvider {
            check_binary: false,
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

    fn plan(&self, ops: &[Op]) -> Result<Vec<Action>> {
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
            if let Some(src) = &op.spec.source {
                return Err(ProviderError::Other(format!(
                    "apt: source `{src}` (PPA) for `{}` is not supported yet",
                    op.spec.name
                )));
            }
        }
        let mut actions = Vec::new();
        for kind in [
            OpKind::Install,
            OpKind::Upgrade,
            OpKind::Remove,
            OpKind::Pin,
            OpKind::Unpin,
        ] {
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
        Ok(actions)
    }

    fn exec(&self, ctx: &Ctx, action: &Action) -> Result<Outcome> {
        if ctx.dry_run {
            return Ok(Outcome {
                success: true,
                log: None,
            });
        }
        let cmd = Self::base_cmd("", ctx);
        let run = |argv: &[String]| -> Result<crate::Output> {
            let mut c =
                Cmd::from_argv(argv).ok_or_else(|| ProviderError::Other("empty argv".into()))?;
            c.env = cmd.env.clone();
            ctx.runner.run(&c)
        };
        let mut log = format!("$ {}\n", action.argv.join(" "));
        let mut out = run(&action.argv)?;
        append(&mut log, &out);

        // A stale package cache: refresh once and retry, unless we are offline.
        let installing = matches!(action.kind, OpKind::Install | OpKind::Upgrade);
        if !out.success()
            && installing
            && !ctx.offline
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
