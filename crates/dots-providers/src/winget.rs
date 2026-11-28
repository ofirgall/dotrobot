//! The `winget` provider (Windows Package Manager).
//!
//! * Packages are named by their winget id (`Git.Git`), written `[providers.winget] id = "..."`.
//! * `installed()` uses `winget export --include-versions`, the one machine-readable listing
//!   (the table `winget list` prints is column-aligned and truncated to the console width). The
//!   table is the fallback when export cannot write its file.
//! * `latest()` reads `winget show --id X --exact`, one call per package.
//! * Every mutating command carries `--exact --silent --disable-interactivity` and accepts the
//!   package and source agreements, so nothing waits for a person. "Already installed" and "no
//!   applicable upgrade" are success.
//! * Per-package options: `override` (`--override`, the installer's own arguments) and `args`
//!   (extra arguments for install and upgrade).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use dots_facts::Facts;

use crate::cargo::{command_env, exec_action_ok, resolve_bare};
use crate::{
    Action, Availability, BinEntry, Caps, Cmd, Ctx, Installed, Latest, Op, OpKind, Outcome,
    Provider, ProviderError, Result, Spec, Support,
};

/// `APPINSTALLER_CLI_ERROR_UPDATE_NOT_APPLICABLE` (0x8A15002B): nothing newer to install.
const NO_APPLICABLE_UPDATE: i32 = -1_978_335_189;
/// `APPINSTALLER_CLI_ERROR_PACKAGE_ALREADY_INSTALLED` (0x8A150061).
const ALREADY_INSTALLED: i32 = -1_978_335_135;
/// Exit statuses that mean the package is in the wanted state.
const SUCCESS: &[i32] = &[NO_APPLICABLE_UPDATE, ALREADY_INSTALLED];

/// Joins the `args` list in a [`Spec`] option (see `dots-resolve`'s `spec_for`).
pub const ARGS_SEP: char = '\u{1f}';

const COMMON_FLAGS: &[&str] = &["--accept-source-agreements", "--disable-interactivity"];
const INSTALL_FLAGS: &[&str] = &[
    "--exact",
    "--silent",
    "--accept-package-agreements",
    "--accept-source-agreements",
    "--disable-interactivity",
];

/// Where winget lives: the per-user app execution alias, or the machine-wide one. Never
/// found through `PATH`.
fn resolve_bin(ctx_home: &Path, env: &BTreeMap<String, String>) -> Option<PathBuf> {
    let mut c: Vec<PathBuf> = Vec::new();
    if let Some(l) = env.get("LOCALAPPDATA").filter(|v| !v.is_empty()) {
        c.push(Path::new(l).join("Microsoft/WindowsApps/winget.exe"));
    }
    c.push(ctx_home.join("AppData/Local/Microsoft/WindowsApps/winget.exe"));
    #[cfg(windows)]
    c.extend(crate::cargo::windows_dirs(
        &["LOCALAPPDATA"],
        r"Microsoft\WindowsApps\winget.exe",
    ));
    // An app execution alias is a reparse point `metadata` cannot follow: its existence is
    // enough.
    c.into_iter()
        .find(|p| p.is_file() || std::fs::symlink_metadata(p).is_ok())
}

#[derive(Debug, Clone)]
pub struct WingetProvider {
    bin: PathBuf,
}

impl Default for WingetProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl WingetProvider {
    /// Plans a bare `winget` (not absolute): fine for previews, not for `exec`.
    pub fn new() -> Self {
        WingetProvider {
            bin: PathBuf::from("winget"),
        }
    }

    pub fn with_bin(bin: impl Into<PathBuf>) -> Self {
        WingetProvider { bin: bin.into() }
    }

    /// Resolve winget from the context; falls back to a bare `winget`.
    pub fn for_ctx(ctx: &Ctx) -> Self {
        match resolve_bin(&ctx.home, &ctx.env) {
            Some(b) => Self::with_bin(b),
            None => Self::new(),
        }
    }

    fn bin_for(&self, ctx: &Ctx) -> Result<PathBuf> {
        if self.bin.is_absolute() {
            return Ok(self.bin.clone());
        }
        resolve_bin(&ctx.home, &ctx.env).ok_or_else(|| ProviderError::Unavailable {
            provider: "winget".into(),
            reason: "winget not found in its known locations".into(),
        })
    }

    fn query(&self, ctx: &Ctx, args: &[&str]) -> Result<crate::Output> {
        let bin = self.bin_for(ctx)?;
        let mut cmd = Cmd::new(bin)
            .args(args.iter().copied())
            .envs(&command_env(ctx, None));
        cmd.timeout = Some(ctx.query_timeout.max(std::time::Duration::from_secs(120)));
        ctx.runner.run(&cmd)
    }
}

impl Provider for WingetProvider {
    fn id(&self) -> &'static str {
        "winget"
    }

    fn caps(&self) -> Caps {
        Caps {
            batch_install: false,
            exact_version: Support::Yes,
            pin: true,
            needs_root: false,
            uninstall: true,
            // Two installers running at once collide on the system installer mutex.
            exclusive_lock: true,
        }
    }

    fn availability(&self, facts: &Facts) -> Availability {
        let env = BTreeMap::new();
        match resolve_bin(Path::new(&facts.home), &env) {
            Some(bin_path) => Availability::Ready { bin_path },
            None => Availability::Unsupported,
        }
    }

    fn availability_in(&self, ctx: &Ctx, _facts: &Facts) -> Availability {
        match resolve_bin(&ctx.home, &ctx.env) {
            Some(bin_path) => Availability::Ready { bin_path },
            None => Availability::Unsupported,
        }
    }

    fn installed(&self, ctx: &Ctx) -> Result<BTreeMap<String, Installed>> {
        let dir = if ctx.cache_dir.as_os_str().is_empty() {
            std::env::temp_dir()
        } else {
            ctx.cache_dir.clone()
        };
        let file = dir.join(format!("winget-export-{}.json", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let _ = std::fs::remove_file(&file);
        let file_s = file.to_string_lossy().into_owned();
        // A non-zero status is common here (packages no source knows are reported and
        // skipped) while the file is still written.
        let out = self.query(
            ctx,
            &[
                "export",
                "-o",
                &file_s,
                "--include-versions",
                "--accept-source-agreements",
                "--disable-interactivity",
            ],
        )?;
        let exported = std::fs::read_to_string(&file).ok();
        let _ = std::fs::remove_file(&file);
        if let Some(map) = exported.as_deref().and_then(parse_export) {
            return Ok(map);
        }
        let _ = out;
        let out = self.query(ctx, &["list", COMMON_FLAGS[0], COMMON_FLAGS[1]])?;
        if !out.success() {
            return Err(ProviderError::CommandFailed {
                command: "winget list".into(),
                status: out.status,
            });
        }
        Ok(parse_list(&out.stdout))
    }

    fn latest(&self, ctx: &Ctx, specs: &[Spec]) -> Result<BTreeMap<String, Latest>> {
        let mut map = BTreeMap::new();
        if ctx.offline {
            return Ok(map);
        }
        for s in specs {
            let out = self.query(
                ctx,
                &[
                    "show",
                    "--id",
                    &s.name,
                    "--exact",
                    COMMON_FLAGS[0],
                    COMMON_FLAGS[1],
                ],
            )?;
            if !out.success() {
                continue;
            }
            if let Some(v) = parse_show_version(&out.stdout) {
                map.insert(s.name.clone(), Latest { version: v });
            }
        }
        Ok(map)
    }

    fn plan(&self, ops: &[Op]) -> Result<Vec<Action>> {
        let bin = self.bin.to_string_lossy().into_owned();
        ops.iter()
            .map(|op| {
                let id = op.spec.name.clone();
                let mut a = vec![bin.clone()];
                match op.kind {
                    OpKind::Install | OpKind::Upgrade => {
                        a.push(
                            if op.kind == OpKind::Install {
                                "install"
                            } else {
                                "upgrade"
                            }
                            .into(),
                        );
                        a.extend(["--id".into(), id]);
                        a.extend(INSTALL_FLAGS.iter().map(|f| f.to_string()));
                        if let Some(v) = &op.version {
                            a.extend(["--version".into(), v.clone()]);
                        }
                        if let Some(o) = op.spec.get("override") {
                            a.extend(["--override".into(), o.to_string()]);
                        }
                        if let Some(x) = op.spec.get("args") {
                            a.extend(
                                x.split(ARGS_SEP)
                                    .filter(|s| !s.is_empty())
                                    .map(String::from),
                            );
                        }
                    }
                    OpKind::Remove => {
                        a.extend(["uninstall".into(), "--id".into(), id]);
                        a.extend(
                            [
                                "--exact",
                                "--silent",
                                "--accept-source-agreements",
                                "--disable-interactivity",
                            ]
                            .map(String::from),
                        );
                    }
                    OpKind::Pin => {
                        a.extend([
                            "pin".into(),
                            "add".into(),
                            "--id".into(),
                            id,
                            "--exact".into(),
                        ]);
                        if let Some(v) = &op.version {
                            a.extend(["--version".into(), v.clone()]);
                        }
                        a.extend(COMMON_FLAGS.iter().map(|f| f.to_string()));
                    }
                    OpKind::Unpin => {
                        a.extend([
                            "pin".into(),
                            "remove".into(),
                            "--id".into(),
                            id,
                            "--exact".into(),
                        ]);
                        a.extend(COMMON_FLAGS.iter().map(|f| f.to_string()));
                    }
                }
                Ok(Action {
                    kind: op.kind,
                    ops: vec![op.clone()],
                    argv: a,
                    needs_root: false,
                })
            })
            .collect()
    }

    fn exec(&self, ctx: &Ctx, action: &Action) -> Result<Outcome> {
        let bin = resolve_bin(&ctx.home, &ctx.env);
        let action = &resolve_bare(action, 0, "winget", bin)?;
        exec_action_ok(ctx, action, None, &[], SUCCESS)
    }

    fn bins(&self, _ctx: &Ctx, _installed: &BTreeMap<String, Installed>) -> Result<Vec<BinEntry>> {
        // winget does not say which executables a package installed.
        Ok(Vec::new())
    }
}

/// `winget export` JSON: `Sources[].Packages[]{PackageIdentifier, Version}`.
fn parse_export(text: &str) -> Option<BTreeMap<String, Installed>> {
    let json: serde_json::Value = serde_json::from_str(text.trim_start_matches('\u{feff}')).ok()?;
    let sources = json.get("Sources")?.as_array()?;
    let mut map = BTreeMap::new();
    for pkg in sources
        .iter()
        .filter_map(|s| s.get("Packages")?.as_array())
        .flatten()
    {
        let Some(id) = pkg.get("PackageIdentifier").and_then(|v| v.as_str()) else {
            continue;
        };
        let version = pkg
            .get("Version")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        map.entry(id.to_string()).or_insert(Installed {
            version: version.to_string(),
            pinned: false,
            source: None,
        });
    }
    Some(map)
}

/// The version line of `winget show`: `Version: 2.47.1`.
fn parse_show_version(text: &str) -> Option<String> {
    text.lines().find_map(|l| {
        let v = l.strip_prefix("Version:")?.trim();
        (!v.is_empty()).then(|| v.to_string())
    })
}

/// The table `winget list` prints: a header (`Name  Id  Version  Available  Source`), a line
/// of dashes, then rows aligned under the header. Columns are cut at the header's own
/// positions, in characters.
fn parse_list(text: &str) -> BTreeMap<String, Installed> {
    let lines: Vec<&str> = text.lines().collect();
    let Some(h) = lines.iter().position(|l| {
        let t = l.trim_start();
        t.starts_with("Name") && t.contains("Id") && t.contains("Version")
    }) else {
        return BTreeMap::new();
    };
    let header: Vec<char> = lines[h].chars().collect();
    let col = |name: &str| -> Option<usize> {
        let pat: Vec<char> = name.chars().collect();
        (0..header.len().saturating_sub(pat.len() - 1))
            .find(|&i| header[i..i + pat.len()] == pat[..] && (i == 0 || header[i - 1] == ' '))
    };
    let (Some(id_at), Some(ver_at)) = (col("Id"), col("Version")) else {
        return BTreeMap::new();
    };
    let end_at = col("Available").or_else(|| col("Source"));
    let slice = |row: &[char], from: usize, to: Option<usize>| -> String {
        let to = to.unwrap_or(row.len()).min(row.len());
        if from >= to {
            return String::new();
        }
        row[from..to].iter().collect::<String>().trim().to_string()
    };
    let mut map = BTreeMap::new();
    for l in lines.iter().skip(h + 1) {
        if l.trim().is_empty() || l.trim_start().starts_with("---") {
            continue;
        }
        let row: Vec<char> = l.chars().collect();
        let id = slice(&row, id_at, Some(ver_at));
        let version = slice(&row, ver_at, end_at);
        // Rows of installed software no source knows have no real id (`ARP\...`, `MSIX\...`).
        if id.is_empty() || id.contains(' ') || id.starts_with("ARP\\") || id.starts_with("MSIX\\")
        {
            continue;
        }
        let version = version
            .trim_start_matches('<')
            .trim_start_matches('>')
            .trim();
        map.entry(id).or_insert(Installed {
            version: if version.is_empty() {
                "unknown".into()
            } else {
                version.to_string()
            },
            pinned: false,
            source: None,
        });
    }
    map
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn export_json_gives_ids_and_versions() {
        let m = parse_export(
            "\u{feff}{\"Sources\":[{\"Packages\":[{\"PackageIdentifier\":\"Git.Git\",\"Version\":\"2.47.1\"},{\"PackageIdentifier\":\"X.Y\"}]}]}",
        )
        .unwrap();
        assert_eq!(m["Git.Git"].version, "2.47.1");
        assert_eq!(m["X.Y"].version, "unknown");
    }

    #[test]
    fn the_list_table_is_cut_at_the_header_columns() {
        let t = "\
  -
Name              Id               Version   Available Source
-------------------------------------------------------------
Git               Git.Git          2.47.1    2.48.0    winget
Some Local Tool   ARP\\Machine\\X   1.0
7-Zip             7zip.7zip        < 24.09
";
        let m = parse_list(t);
        assert_eq!(m["Git.Git"].version, "2.47.1");
        assert_eq!(m["7zip.7zip"].version, "24.09");
        assert_eq!(m.len(), 2, "{m:?}");
    }

    #[test]
    fn show_version_line() {
        assert_eq!(
            parse_show_version("Found Git [Git.Git]\nVersion: 2.48.0\nPublisher: x\n"),
            Some("2.48.0".into())
        );
        assert_eq!(parse_show_version("no version here"), None);
    }
}
