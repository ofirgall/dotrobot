//! What a sandboxed test process needs from the host: a minimal `PATH` and, on Windows, the
//! few variables a process cannot start without.

/// The `PATH` separator of the host.
pub const PATH_SEP: char = if cfg!(windows) { ';' } else { ':' };

/// The `PATH` of a sandboxed process: the system tools only. On Windows that is the system
/// directories plus Git for Windows when it is installed (several tests make commits).
pub fn system_path() -> String {
    #[cfg(unix)]
    return "/usr/bin:/bin".to_string();
    #[cfg(windows)]
    {
        let root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
        let mut dirs = vec![format!(r"{root}\System32"), root.clone()];
        for var in ["ProgramFiles", "ProgramW6432"] {
            if let Some(pf) = std::env::var_os(var) {
                let git = std::path::Path::new(&pf).join("Git");
                if git.join("cmd").join("git.exe").is_file() {
                    // `usr\bin` holds the unix tools (touch, test, ...) fixture scripts use.
                    for d in ["cmd", r"usr\bin"] {
                        dirs.push(git.join(d).to_string_lossy().into_owned());
                    }
                    break;
                }
            }
        }
        dirs.join(";")
    }
}

/// Variables to pass through after `env_clear()`: nothing on unix; on Windows `SystemRoot`
/// and the temp directory, without which many programs fail to start.
pub fn system_env() -> Vec<(String, String)> {
    if !cfg!(windows) {
        return Vec::new();
    }
    [
        "SystemRoot",
        "windir",
        "TEMP",
        "TMP",
        "ComSpec",
        "PATHEXT",
        "ProgramFiles",
        "ProgramW6432",
        "LOCALAPPDATA",
    ]
    .iter()
    .filter_map(|k| std::env::var(k).ok().map(|v| ((*k).to_string(), v)))
    .collect()
}

/// `dir` followed by the system `PATH`, joined with the host's separator.
pub fn path_with(dir: impl AsRef<std::path::Path>) -> String {
    format!("{}{PATH_SEP}{}", dir.as_ref().display(), system_path())
}

/// `fs::canonicalize` without the `\\?\` verbatim prefix Windows adds, which most tools
/// (git, shells) do not accept. Same as `canonicalize` elsewhere.
pub fn canonical(p: impl AsRef<std::path::Path>) -> std::path::PathBuf {
    let c = std::fs::canonicalize(p.as_ref()).expect("canonicalize");
    let s = c.to_string_lossy();
    match s.strip_prefix(r"\\?\") {
        Some(rest) if cfg!(windows) && !rest.starts_with("UNC") => rest.into(),
        _ => c,
    }
}

/// On Windows the default shell is PowerShell, but the fixtures written for these tests are
/// bash: give each script provider table `shell = "bash"` and each manual action with a
/// `check` a `shell: bash` front-matter line (Git for Windows' bash). Unchanged elsewhere and
/// for other text.
pub fn bash_on_windows(text: &str) -> String {
    if !cfg!(windows) {
        return text.to_string();
    }
    if text.contains("[providers.script]") && !text.contains("shell = ") {
        return text.replace("[providers.script]", "[providers.script]\nshell = \"bash\"");
    }
    if let Some(rest) = text.strip_prefix("---\n")
        && text.contains("\ncheck:")
        && !text.contains("\nshell:")
    {
        return format!("---\nshell: bash\n{rest}");
    }
    text.to_string()
}
