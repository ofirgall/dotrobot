//! The `uv` provider: `uv tool` for CLIs, `uv pip` for libraries (library mode).
//!
//! Per-package options are `Spec::opts` keys:
//! `kind=tool` (default) | `kind=lib` with `python=system|<interpreter>`. `kind=venv` is not
//! supported yet and is refused at plan time.
//!
//! `installed()` makes ONE `uv tool list` call, plus, only when lib packages are registered
//! ([`UvProvider::lib`]), ONE `uv pip list --format json` call per distinct python target
//! (`--system` or `--python <path>`): at most one call per backend target, never one per
//! package. Registered libs are reported under their registered name, so a second apply is a
//! no-op. `exec()` resolves a bare `uv` in `argv[0]` through the known locations, never `PATH`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use dots_facts::Facts;

use crate::cargo::{command_env, exec_action, first_file, pep440_req, resolve_bare};
use crate::{
    Action, Availability, BinEntry, Caps, Cmd, Ctx, Installed, Latest, Op, OpKind, Outcome,
    Provider, ProviderError, Result, Spec, Support,
};

fn resolve_bin(home: &Path) -> Option<PathBuf> {
    first_file([
        home.join(".local/bin/uv"),
        home.join(".cargo/bin/uv"),
        PathBuf::from("/opt/homebrew/bin/uv"),
        PathBuf::from("/usr/local/bin/uv"),
        PathBuf::from("/usr/bin/uv"),
    ])
}

fn availability(home: &Path) -> Availability {
    match resolve_bin(home) {
        Some(bin_path) => Availability::Ready { bin_path },
        None => Availability::Bootstrap { pkg: "uv".into() },
    }
}

fn tool_dir(ctx: &Ctx) -> PathBuf {
    if let Some(d) = ctx.env.get("UV_TOOL_DIR").filter(|d| !d.is_empty()) {
        return PathBuf::from(d);
    }
    if let Some(x) = ctx.env.get("XDG_DATA_HOME").filter(|d| !d.is_empty()) {
        return PathBuf::from(x).join("uv/tools");
    }
    ctx.home.join(".local/share/uv/tools")
}

#[derive(Debug, Clone)]
pub struct UvProvider {
    bin: PathBuf,
    /// Registered library packages: name -> python target (`system` or an interpreter path).
    libs: BTreeMap<String, String>,
}

impl Default for UvProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl UvProvider {
    /// Plans a bare `uv` (not absolute): fine for previews, not for `exec`.
    pub fn new() -> Self {
        UvProvider {
            bin: PathBuf::from("uv"),
            libs: BTreeMap::new(),
        }
    }

    pub fn with_bin(bin: impl Into<PathBuf>) -> Self {
        UvProvider {
            bin: bin.into(),
            libs: BTreeMap::new(),
        }
    }

    /// Register a `kind=lib` package so `installed()` reports it. `python` is `system` or an
    /// interpreter path.
    pub fn lib(mut self, name: &str, python: &str) -> Self {
        self.libs.insert(name.to_string(), python.to_string());
        self
    }

    /// Resolve uv from the context (never `PATH`); falls back to a bare `uv`.
    pub fn for_ctx(ctx: &Ctx) -> Self {
        match resolve_bin(&ctx.home) {
            Some(b) => Self::with_bin(b),
            None => Self::new(),
        }
    }
}

impl Provider for UvProvider {
    fn id(&self) -> &'static str {
        "uv"
    }

    fn caps(&self) -> Caps {
        Caps {
            batch_install: false,
            exact_version: Support::Yes,
            pin: false,
            needs_root: false,
            uninstall: true,
            exclusive_lock: false,
        }
    }

    fn availability(&self, facts: &Facts) -> Availability {
        availability(Path::new(&facts.home))
    }

    fn availability_in(&self, ctx: &Ctx, _facts: &Facts) -> Availability {
        availability(&ctx.home)
    }

    fn installed(&self, ctx: &Ctx) -> Result<BTreeMap<String, Installed>> {
        let bin = resolve_bin(&ctx.home).ok_or_else(|| ProviderError::Unavailable {
            provider: "uv".into(),
            reason: "uv not found".into(),
        })?;
        let cmd = Cmd::new(&bin)
            .args(["tool", "list", "--show-version-specifiers"])
            .envs(&command_env(ctx, bin.parent()));
        let out = ctx.runner.run(&cmd)?;
        if !out.success() {
            return Err(ProviderError::CommandFailed {
                command: cmd.line(),
                status: out.status,
            });
        }
        let mut map = parse_tool_list(&out.stdout);
        let targets: std::collections::BTreeSet<&String> = self.libs.values().collect();
        for py in targets {
            let mut cmd = Cmd::new(&bin).args(["pip", "list"]);
            cmd = if py == "system" {
                cmd.arg("--system")
            } else {
                cmd.args(["--python", py])
            };
            let cmd = cmd
                .args(["--format", "json"])
                .envs(&command_env(ctx, bin.parent()));
            let out = ctx.runner.run(&cmd)?;
            if !out.success() {
                return Err(ProviderError::CommandFailed {
                    command: cmd.line(),
                    status: out.status,
                });
            }
            let json: serde_json::Value = serde_json::from_str(&out.stdout)
                .map_err(|e| ProviderError::Other(format!("uv pip list: bad JSON: {e}")))?;
            let pkgs: BTreeMap<String, String> = json
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|p| {
                    Some((
                        norm(p.get("name")?.as_str()?),
                        p.get("version")?.as_str()?.to_string(),
                    ))
                })
                .collect();
            for (name, _) in self.libs.iter().filter(|(_, p)| *p == py) {
                if let Some(version) = pkgs.get(&norm(name)) {
                    map.entry(name.clone()).or_insert(Installed {
                        version: version.clone(),
                        pinned: false,
                        source: None,
                    });
                }
            }
        }
        Ok(map)
    }

    fn latest(&self, ctx: &Ctx, specs: &[Spec]) -> Result<BTreeMap<String, Latest>> {
        // `uv` has no "latest version" query; a dry-run upgrade would install. PyPI's JSON API
        // is the planned source; until an HTTP client exists, report nothing.
        let _ = (ctx, specs);
        Ok(BTreeMap::new())
    }

    fn plan(&self, ops: &[Op]) -> Result<Vec<Action>> {
        let bin = self.bin.to_string_lossy().into_owned();
        ops.iter()
            .map(|op| {
                let name = &op.spec.name;
                let o = &op.spec.opts;
                let req = match &op.version {
                    Some(v) => format!("{name}{}", pep440_req(v)),
                    None => name.clone(),
                };
                let argv: Vec<String> = match o.get("kind").map(String::as_str).unwrap_or("tool") {
                    "tool" => match op.kind {
                        OpKind::Install => v(&[&bin, "tool", "install", &req]),
                        OpKind::Upgrade if op.version.is_some() => {
                            v(&[&bin, "tool", "install", "--force", &req])
                        }
                        OpKind::Upgrade => v(&[&bin, "tool", "upgrade", name]),
                        OpKind::Remove => v(&[&bin, "tool", "uninstall", name]),
                        OpKind::Pin | OpKind::Unpin => return Err(no_pin()),
                    },
                    "lib" => {
                        let target = match o.get("python").map(String::as_str) {
                            None | Some("system") => v(&["--system"]),
                            Some(py) => v(&["--python", py]),
                        };
                        let mut a = v(&[&bin, "pip"]);
                        match op.kind {
                            OpKind::Install => {
                                a.push("install".into());
                                a.extend(target);
                                a.push(req);
                            }
                            OpKind::Upgrade => {
                                a.push("install".into());
                                a.extend(target);
                                a.push("--upgrade".into());
                                a.push(req);
                            }
                            OpKind::Remove => {
                                a.push("uninstall".into());
                                a.extend(target);
                                a.push(name.clone());
                            }
                            OpKind::Pin | OpKind::Unpin => return Err(no_pin()),
                        }
                        a
                    }
                    other => {
                        return Err(ProviderError::Other(format!(
                            "uv: kind `{other}` is not supported yet (use `tool` or `lib`)"
                        )));
                    }
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
        let action = &resolve_bare(action, 0, "uv", resolve_bin(&ctx.home))?;
        let bin_dir = Path::new(&action.argv[0]).parent().map(Path::to_path_buf);
        exec_action(ctx, action, bin_dir.as_deref(), &[])
    }

    fn bins(&self, ctx: &Ctx, installed: &BTreeMap<String, Installed>) -> Result<Vec<BinEntry>> {
        let dir = tool_dir(ctx);
        let mut out = Vec::new();
        for name in installed.keys() {
            let Ok(text) = std::fs::read_to_string(dir.join(name).join("uv-receipt.toml")) else {
                continue;
            };
            for line in text.lines() {
                if let (Some(bin), Some(path)) = (
                    quoted_after(line, "{ name = \""),
                    quoted_after(line, "install-path = \""),
                ) {
                    out.push(BinEntry {
                        package: name.clone(),
                        bin,
                        path: PathBuf::from(path),
                    });
                }
            }
        }
        out.sort_by(|a, b| (&a.package, &a.bin).cmp(&(&b.package, &b.bin)));
        Ok(out)
    }
}

/// PEP 503 name normalisation.
fn norm(n: &str) -> String {
    n.to_lowercase().replace(['_', '.'], "-")
}

fn v(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|s| s.to_string()).collect()
}

fn no_pin() -> ProviderError {
    ProviderError::Other("uv cannot pin packages".into())
}

fn quoted_after(line: &str, marker: &str) -> Option<String> {
    let rest = &line[line.find(marker)? + marker.len()..];
    Some(rest[..rest.find('"')?].to_string())
}

/// Parse `uv tool list --show-version-specifiers`: `name vX.Y.Z [required: spec]` lines,
/// each followed by `- bin` lines.
pub(crate) fn parse_tool_list(text: &str) -> BTreeMap<String, Installed> {
    let mut map = BTreeMap::new();
    for line in text.lines() {
        if line.starts_with(['-', ' ']) || line.trim().is_empty() {
            continue;
        }
        let mut it = line.splitn(3, ' ');
        let (Some(name), Some(ver)) = (it.next(), it.next()) else {
            continue;
        };
        let Some(version) = ver.strip_prefix('v') else {
            continue;
        };
        let source = it
            .next()
            .and_then(|r| r.trim().strip_prefix("[required: "))
            .and_then(|r| r.strip_suffix(']'))
            .map(str::to_string);
        map.insert(
            name.to_string(),
            Installed {
                version: version.to_string(),
                pinned: false,
                source,
            },
        );
    }
    map
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_tool_list() {
        let m = parse_tool_list(
            "agents-status v0.1.0 [required: file:///x]\n- agents-status-server\nruff v0.4.0\n- ruff\n",
        );
        assert_eq!(m["ruff"].version, "0.4.0");
        assert_eq!(m["agents-status"].source.as_deref(), Some("file:///x"));
        assert_eq!(m.len(), 2);
    }
}
