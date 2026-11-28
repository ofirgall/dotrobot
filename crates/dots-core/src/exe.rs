//! Finding executables in a directory: the executable bit on unix, `PATHEXT` on Windows.

use std::path::{Path, PathBuf};

/// The files `name` can be in `dir`. On unix that is `name` itself. On Windows a command is
/// found by appending a `PATHEXT` extension (`bat` finds `bat.exe`), and a name that already
/// carries one (`bat.exe`) is tried as written first.
pub fn candidates(dir: &Path, name: &str) -> Vec<PathBuf> {
    let mut v = vec![dir.join(name)];
    if cfg!(windows) {
        v.extend(
            path_exts()
                .into_iter()
                .map(|e| dir.join(format!("{name}{e}"))),
        );
    }
    v
}

/// A file name as a command is called: on Windows without its `PATHEXT` extension (`bat.exe`
/// is `bat`); unchanged elsewhere.
pub fn command_name(file_name: &str) -> String {
    if cfg!(windows)
        && let Some((stem, ext)) = file_name.rsplit_once('.')
        && !stem.is_empty()
        && path_exts().iter().any(|e| e[1..].eq_ignore_ascii_case(ext))
    {
        return stem.to_string();
    }
    file_name.to_string()
}

/// The first runnable file for `name` in `dir`, as found on disk.
pub fn find_in(dir: &Path, name: &str) -> Option<PathBuf> {
    candidates(dir, name).into_iter().find(|p| is_executable(p))
}

/// Windows' `PATHEXT` (`.COM;.EXE;...`), each with its leading dot. Empty off Windows.
pub fn path_exts() -> Vec<String> {
    if !cfg!(windows) {
        return Vec::new();
    }
    std::env::var("PATHEXT")
        .unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".into())
        .split(';')
        .filter(|e| e.starts_with('.') && e.len() > 1)
        .map(str::to_string)
        .collect()
}

/// A regular file that can be run: any executable bit on unix; on Windows, an extension in
/// `PATHEXT` (there are no permission bits to ask).
pub fn is_executable(p: &Path) -> bool {
    let Ok(m) = std::fs::metadata(p) else {
        return false;
    };
    if !m.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        m.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        let ext = p
            .extension()
            .map(|e| format!(".{}", e.to_string_lossy()))
            .unwrap_or_default();
        path_exts().iter().any(|e| e.eq_ignore_ascii_case(&ext))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_is_not_executable() {
        let d = tempfile::tempdir().unwrap();
        assert!(!is_executable(&d.path().join("nope")));
        assert!(find_in(d.path(), "nope").is_none());
        assert!(!is_executable(d.path()), "a directory is not");
    }

    #[test]
    fn command_name_drops_the_pathext_extension_on_windows_only() {
        assert_eq!(command_name("notes.txt"), "notes.txt");
        assert_eq!(command_name("tool"), "tool");
        let want = if cfg!(windows) { "bat" } else { "bat.exe" };
        assert_eq!(command_name("bat.exe"), want);
    }

    #[cfg(windows)]
    #[test]
    fn a_command_name_finds_its_pathext_file() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("bat.exe"), "").unwrap();
        std::fs::write(d.path().join("notes.txt"), "").unwrap();
        let lower = |p: Option<PathBuf>| p.map(|p| p.to_string_lossy().to_lowercase());
        let want = Some(d.path().join("bat.exe").to_string_lossy().to_lowercase());
        assert_eq!(lower(find_in(d.path(), "bat")), want);
        assert_eq!(lower(find_in(d.path(), "bat.exe")), want);
        assert!(find_in(d.path(), "notes").is_none());
        assert!(!is_executable(&d.path().join("notes.txt")));
    }

    #[cfg(unix)]
    #[test]
    fn the_executable_bit_decides() {
        use std::os::unix::fs::PermissionsExt;
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("tool");
        std::fs::write(&p, "").unwrap();
        assert!(!is_executable(&p));
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(is_executable(&p));
        assert_eq!(find_in(d.path(), "tool"), Some(p));
    }
}
