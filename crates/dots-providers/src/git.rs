//! The `git` provider: a package is a repository cloned to a destination at a revision.
//!
//! Repositories are registered on the provider ([`GitProvider::repo`]): the logical package
//! name maps to a clone `url` and a `dest`. `Spec::opts` (`url`, `dest`) override per op.
//! `Op::version` is the revision: a tag, a branch, or a commit hash.
//!
//! `installed()` reads each registered clone from disk with ZERO processes: `<dest>/.git/HEAD`
//! (symbolic or detached), the ref file or `packed-refs`, then tags. The version is the tag at
//! HEAD when there is one (a loose lightweight tag, a packed tag, or a packed annotated tag via
//! its peeled `^sha` line), else the full commit hash. A loose annotated tag would need object
//! parsing; it is not detected and the sha is reported instead. No network except in `latest`.
//! A single `Action` is a single command, so checkouts of a commit hash use `/bin/sh -c` with
//! positional arguments (shown verbatim in the plan); tags, branches and "latest" are direct.
//! `exec()` resolves a bare `git` (from [`GitProvider::new`]) through the known locations.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use dots_facts::Facts;

use crate::cargo::{command_env, exec_action, first_file, resolve_bare};
use crate::{
    Action, Availability, BinEntry, Caps, Cmd, Ctx, Installed, Latest, Op, OpKind, Outcome,
    Provider, ProviderError, Result, Spec, Support,
};

pub(crate) fn resolve_bin(home: &Path) -> Option<PathBuf> {
    #[cfg_attr(not(windows), allow(unused_mut))]
    let mut c = vec![
        home.join(".nix-profile/bin/git"),
        PathBuf::from("/opt/homebrew/bin/git"),
        PathBuf::from("/usr/local/bin/git"),
        PathBuf::from("/usr/bin/git"),
    ];
    // Git for Windows: machine-wide (`ProgramFiles`) or per-user (`LOCALAPPDATA\Programs`).
    #[cfg(windows)]
    {
        c.extend(crate::cargo::windows_dirs(
            &["ProgramFiles", "ProgramW6432", "ProgramFiles(x86)"],
            r"Git\cmd\git",
        ));
        c.extend(crate::cargo::windows_dirs(
            &["LOCALAPPDATA"],
            r"Programs\Git\cmd\git",
        ));
    }
    first_file(c)
}

fn availability(home: &Path) -> Availability {
    match resolve_bin(home) {
        Some(bin_path) => Availability::Ready { bin_path },
        None => Availability::Bootstrap { pkg: "git".into() },
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitRepo {
    pub url: String,
    pub dest: PathBuf,
}

#[derive(Debug, Clone)]
pub struct GitProvider {
    bin: PathBuf,
    repos: BTreeMap<String, GitRepo>,
}

impl Default for GitProvider {
    fn default() -> Self {
        Self::new()
    }
}

fn is_sha(rev: &str) -> bool {
    (7..=40).contains(&rev.len()) && rev.chars().all(|c| c.is_ascii_hexdigit())
}

impl GitProvider {
    /// Plans a bare `git` (not absolute): fine for previews, not for `exec`.
    pub fn new() -> Self {
        GitProvider {
            bin: PathBuf::from("git"),
            repos: BTreeMap::new(),
        }
    }

    pub fn with_bin(bin: impl Into<PathBuf>) -> Self {
        GitProvider {
            bin: bin.into(),
            repos: BTreeMap::new(),
        }
    }

    /// Resolve git from the context (never `PATH`); falls back to a bare `git`.
    pub fn for_ctx(ctx: &Ctx) -> Self {
        match resolve_bin(&ctx.home) {
            Some(b) => Self::with_bin(b),
            None => Self::new(),
        }
    }

    /// Register a managed repository under its package name.
    pub fn repo(mut self, name: &str, url: &str, dest: impl Into<PathBuf>) -> Self {
        self.repos.insert(
            name.to_string(),
            GitRepo {
                url: url.to_string(),
                dest: dest.into(),
            },
        );
        self
    }

    fn resolve(&self, spec: &Spec) -> Result<GitRepo> {
        let o = &spec.opts;
        let reg = self.repos.get(&spec.name);
        let url = o.get("url").cloned().or_else(|| reg.map(|r| r.url.clone()));
        let dest = o
            .get("dest")
            .map(PathBuf::from)
            .or_else(|| reg.map(|r| r.dest.clone()));
        match (url, dest) {
            (Some(url), Some(dest)) => Ok(GitRepo { url, dest }),
            _ => Err(ProviderError::Other(format!(
                "git: no url/dest known for `{}`",
                spec.name
            ))),
        }
    }
}

impl Provider for GitProvider {
    fn id(&self) -> &'static str {
        "git"
    }

    fn caps(&self) -> Caps {
        Caps {
            batch_install: false,
            exact_version: Support::Yes,
            pin: false,
            needs_root: false,
            uninstall: false,
            exclusive_lock: false,
        }
    }

    fn availability(&self, facts: &Facts) -> Availability {
        availability(Path::new(&facts.home))
    }

    fn availability_in(&self, ctx: &Ctx, _facts: &Facts) -> Availability {
        availability(&ctx.home)
    }

    fn installed(&self, _ctx: &Ctx) -> Result<BTreeMap<String, Installed>> {
        let mut map = BTreeMap::new();
        for (name, repo) in &self.repos {
            if let Some((sha, tag)) = read_head(&repo.dest.join(".git")) {
                map.insert(
                    name.clone(),
                    Installed {
                        version: tag.unwrap_or_else(|| sha.clone()),
                        pinned: false,
                        source: Some(sha),
                    },
                );
            }
        }
        Ok(map)
    }

    fn latest(&self, ctx: &Ctx, specs: &[Spec]) -> Result<BTreeMap<String, Latest>> {
        let mut map = BTreeMap::new();
        let Some(bin) = resolve_bin(&ctx.home) else {
            return Ok(map);
        };
        if ctx.offline {
            return Ok(map);
        }
        for s in specs {
            let Ok(repo) = self.resolve(s) else { continue };
            let cmd = Cmd::new(&bin)
                .args(["ls-remote", &repo.url, "HEAD"])
                .envs(&command_env(ctx, bin.parent()))
                .env("GIT_TERMINAL_PROMPT", "0");
            let out = ctx.runner.run(&cmd)?;
            if out.success()
                && let Some(sha) = out.stdout.split_whitespace().next()
            {
                map.insert(
                    s.name.clone(),
                    Latest {
                        version: sha.to_string(),
                    },
                );
            }
        }
        Ok(map)
    }

    fn plan(&self, ops: &[Op]) -> Result<Vec<Action>> {
        let git = self.bin.to_string_lossy().into_owned();
        ops.iter()
            .map(|op| {
                if matches!(op.kind, OpKind::Remove) {
                    return Err(ProviderError::Other(
                        "git cannot uninstall: delete the clone by hand".into(),
                    ));
                }
                if matches!(op.kind, OpKind::Pin | OpKind::Unpin) {
                    return Err(ProviderError::Other("git cannot pin; set a rev".into()));
                }
                let repo = self.resolve(&op.spec)?;
                let dest = repo.dest.to_string_lossy().into_owned();
                let git = git.clone();
                let s = |x: &str| x.to_string();
                // Two commands in one action need a shell; on Windows that is PowerShell
                // (`& 'git'` is rewritten to the resolved path by `exec`).
                #[cfg(windows)]
                let two_step = |script: String| -> Vec<String> {
                    let (sh, flags) = crate::script::platform_shell();
                    let mut v = vec![sh.to_string_lossy().into_owned()];
                    v.extend(flags.iter().map(|f| f.to_string()));
                    v.push(format!("{script}\nexit $LASTEXITCODE"));
                    v
                };
                #[cfg(windows)]
                let pq = |x: &str| format!("'{}'", x.replace('\'', "''"));
                let argv: Vec<String> = match (op.kind, op.version.as_deref()) {
                    (OpKind::Install, None) => vec![git, s("clone"), repo.url, dest],
                    (OpKind::Install, Some(rev)) if !is_sha(rev) => {
                        vec![git, s("clone"), s("--branch"), s(rev), repo.url, dest]
                    }
                    #[cfg(windows)]
                    (OpKind::Install, Some(rev)) => two_step(format!(
                        "& 'git' clone {u} {d}\nif ($LASTEXITCODE -eq 0) {{ & 'git' -C {d} checkout --detach {r} }}",
                        u = pq(&repo.url),
                        d = pq(&dest),
                        r = pq(rev)
                    )),
                    #[cfg(not(windows))]
                    (OpKind::Install, Some(rev)) => vec![
                        s("/bin/sh"),
                        s("-c"),
                        s(r#""$0" clone "$1" "$2" && "$0" -C "$2" checkout --detach "$3""#),
                        git,
                        repo.url,
                        dest,
                        s(rev),
                    ],
                    (_, None) => vec![git, s("-C"), dest, s("pull"), s("--ff-only")],
                    #[cfg(windows)]
                    (_, Some(rev)) => two_step(format!(
                        "& 'git' -C {d} fetch --tags --force origin\nif ($LASTEXITCODE -eq 0) {{\n  & 'git' -C {d} checkout --detach ('origin/' + {r}) 2>$null\n  if ($LASTEXITCODE -ne 0) {{ & 'git' -C {d} checkout --detach {r} }}\n}}",
                        d = pq(&dest),
                        r = pq(rev)
                    )),
                    #[cfg(not(windows))]
                    (_, Some(rev)) => vec![
                        s("/bin/sh"),
                        s("-c"),
                        s(concat!(
                            r#""$0" -C "$1" fetch --tags --force origin && "#,
                            r#"{ "$0" -C "$1" checkout --detach "origin/$2" 2>/dev/null || "#,
                            r#""$0" -C "$1" checkout --detach "$2"; }"#
                        )),
                        git,
                        dest,
                        s(rev),
                    ],
                };
                Ok(Action {
                    kind: op.kind,
                    ops: vec![op.clone()],
                    argv,
                    needs_root: false,
                })
            })
            .collect()
    }

    fn exec(&self, ctx: &Ctx, action: &Action) -> Result<Outcome> {
        let git = resolve_bin(&ctx.home);
        let mut fixed = resolve_bare(action, 0, "git", git.clone())?;
        // `/bin/sh -c <script> git ...`: `$0` is the git binary.
        if fixed.argv.first().map(String::as_str) == Some("/bin/sh") {
            fixed = resolve_bare(&fixed, 3, "git", git.clone())?;
        }
        // Windows two-step actions carry `& 'git'` inside the PowerShell text.
        #[cfg(windows)]
        if let (Some(g), Some(last)) = (&git, fixed.argv.last_mut())
            && last.contains("& 'git'")
        {
            *last = last.replace(
                "& 'git'",
                &format!("& '{}'", g.to_string_lossy().replace('\'', "''")),
            );
        }
        let action = &fixed;
        exec_action(ctx, action, None, &[("GIT_TERMINAL_PROMPT", "0")])
    }

    fn bins(&self, _ctx: &Ctx, _installed: &BTreeMap<String, Installed>) -> Result<Vec<BinEntry>> {
        Ok(Vec::new())
    }
}

fn is_hex_sha(s: &str) -> bool {
    s.len() >= 40 && s.chars().all(|c| c.is_ascii_hexdigit())
}

fn read_trim(p: &Path) -> Option<String> {
    Some(std::fs::read_to_string(p).ok()?.trim().to_string())
}

/// Resolve a full ref name to a sha: the loose file, else `packed-refs`.
fn resolve_ref(gitdir: &Path, name: &str) -> Option<String> {
    if let Some(v) = read_trim(&gitdir.join(name)).filter(|v| is_hex_sha(v)) {
        return Some(v);
    }
    let packed = std::fs::read_to_string(gitdir.join("packed-refs")).ok()?;
    packed.lines().find_map(|l| {
        let (sha, r) = l.split_once(' ')?;
        (r == name && is_hex_sha(sha)).then(|| sha.to_string())
    })
}

fn collect_loose_tags(dir: &Path, prefix: &str, sha: &str, out: &mut Vec<String>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let name = format!("{prefix}{}", e.file_name().to_string_lossy());
        let p = e.path();
        if p.is_dir() {
            collect_loose_tags(&p, &format!("{name}/"), sha, out);
        } else if read_trim(&p).as_deref() == Some(sha) {
            out.push(name);
        }
    }
}

/// `(sha of HEAD, a tag pointing at it)` for the git dir, or `None` when unreadable.
fn read_head(gitdir: &Path) -> Option<(String, Option<String>)> {
    let head = read_trim(&gitdir.join("HEAD"))?;
    let sha = match head.strip_prefix("ref: ") {
        Some(r) => resolve_ref(gitdir, r)?,
        None if is_hex_sha(&head) => head,
        None => return None,
    };
    let mut tags = Vec::new();
    collect_loose_tags(&gitdir.join("refs/tags"), "", &sha, &mut tags);
    if let Ok(packed) = std::fs::read_to_string(gitdir.join("packed-refs")) {
        let mut last_tag: Option<&str> = None;
        for l in packed.lines() {
            if let Some(peeled) = l.strip_prefix('^') {
                if peeled.trim() == sha
                    && let Some(t) = last_tag
                {
                    tags.push(t.to_string());
                }
                continue;
            }
            last_tag = None;
            if let Some((s, r)) = l.split_once(' ')
                && let Some(t) = r.strip_prefix("refs/tags/")
            {
                last_tag = Some(t);
                if s == sha {
                    tags.push(t.to_string());
                }
            }
        }
    }
    tags.sort();
    Some((sha, tags.into_iter().next()))
}
