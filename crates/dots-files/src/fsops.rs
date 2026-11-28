//! Filesystem primitives: atomic writes and link swaps, hashing, moving paths aside.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

pub fn sha256_hex(bytes: &[u8]) -> String {
    let d = Sha256::digest(bytes);
    d.iter().map(|b| format!("{b:02x}")).collect()
}

/// Hash of the content at `p` (following symlinks); `None` if unreadable or not a file.
pub fn sha256_path(p: &Path) -> Option<String> {
    fs::read(p).ok().map(|b| sha256_hex(&b))
}

/// Points inside [`atomic_write_hooked`] where a test can inject a failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    /// The temp file is fully written and synced; the rename has not happened.
    AfterTempWrite,
}

fn tmp_name(path: &Path) -> PathBuf {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    path.with_file_name(format!(".{name}.dots-tmp-{}", std::process::id()))
}

/// Write `bytes` to `path` atomically: temp file in the same directory, `fsync`, `rename`.
pub fn atomic_write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    atomic_write_hooked(path, bytes, &|_| Ok(()))
}

/// Follow symlinks (also dangling ones) to the path that really holds the content.
pub fn resolve_link(path: &Path) -> PathBuf {
    let mut p = path.to_path_buf();
    for _ in 0..32 {
        match fs::symlink_metadata(&p) {
            Ok(m) if m.file_type().is_symlink() => match fs::read_link(&p) {
                Ok(t) => {
                    p = match p.parent() {
                        Some(d) => d.join(t),
                        None => t,
                    }
                }
                Err(_) => break,
            },
            _ => break,
        }
    }
    p
}

/// As [`atomic_write`], but a symlink at `path` is resolved first: the file it points to is
/// replaced and the link stays. For files the user owns (repo sources, override layers).
pub fn atomic_write_follow(path: &Path, bytes: &[u8]) -> io::Result<()> {
    atomic_write(&resolve_link(path), bytes)
}

/// As [`atomic_write`], with a failure-injection hook. On error the temp file is removed and the
/// destination is untouched.
pub fn atomic_write_hooked(
    path: &Path,
    bytes: &[u8],
    hook: &dyn Fn(Stage) -> io::Result<()>,
) -> io::Result<()> {
    let tmp = tmp_name(path);
    let res = (|| {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        drop(f);
        if let Ok(meta) = fs::metadata(path) {
            let _ = fs::set_permissions(&tmp, meta.permissions());
        }
        hook(Stage::AfterTempWrite)?;
        fs::rename(&tmp, path)
    })();
    if res.is_err() {
        let _ = fs::remove_file(&tmp);
    } else if let Some(d) = path.parent().and_then(|dir| fs::File::open(dir).ok()) {
        let _ = d.sync_all();
    }
    res
}

#[cfg(unix)]
fn symlink(target: &Path, link: &Path) -> io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}
#[cfg(windows)]
fn symlink(target: &Path, link: &Path) -> io::Result<()> {
    if target.is_dir() {
        std::os::windows::fs::symlink_dir(target, link)
    } else {
        std::os::windows::fs::symlink_file(target, link)
    }
}

/// Create `link` -> `target`, replacing whatever symlink or file is at `link`, atomically
/// (`link.dots-tmp` then `rename`). It does not replace a directory.
///
/// Windows cannot rename over an existing directory symlink, so there an existing link is
/// removed first (the swap is then two steps, not one).
pub fn atomic_symlink(target: &Path, link: &Path) -> io::Result<()> {
    let tmp = tmp_name(link);
    let _ = remove_link(&tmp);
    symlink(target, &tmp)?;
    #[cfg(windows)]
    if fs::symlink_metadata(link).is_ok_and(|m| m.file_type().is_symlink()) {
        remove_link(link)?;
    }
    fs::rename(&tmp, link).inspect_err(|_| {
        let _ = remove_link(&tmp);
    })
}

/// Remove a file or a symlink. On Windows a symlink to a directory is a directory entry and
/// `remove_file` refuses it, so a symlink that `remove_file` could not remove goes through
/// `remove_dir` (which removes the link, never what it points at).
pub fn remove_link(p: &Path) -> io::Result<()> {
    match fs::remove_file(p) {
        Err(e)
            if cfg!(windows)
                && fs::symlink_metadata(p).is_ok_and(|m| m.file_type().is_symlink()) =>
        {
            fs::remove_dir(p).map_err(|_| e)
        }
        r => r,
    }
}

pub fn lexists(p: &Path) -> bool {
    fs::symlink_metadata(p).is_ok()
}

/// Create missing parent directories of `p`, returning the ones that were created.
pub fn ensure_parents(p: &Path) -> io::Result<Vec<PathBuf>> {
    let mut missing = Vec::new();
    let mut cur = p.parent();
    while let Some(d) = cur {
        if d.exists() || d.as_os_str().is_empty() {
            break;
        }
        missing.push(d.to_path_buf());
        cur = d.parent();
    }
    missing.reverse();
    if let Some(d) = p.parent() {
        fs::create_dir_all(d)?;
    }
    Ok(missing)
}

pub fn copy_recursive(from: &Path, to: &Path) -> io::Result<()> {
    let meta = fs::symlink_metadata(from)?;
    if meta.file_type().is_symlink() {
        symlink(&fs::read_link(from)?, to)
    } else if meta.is_dir() {
        fs::create_dir_all(to)?;
        for e in fs::read_dir(from)? {
            let e = e?;
            copy_recursive(&e.path(), &to.join(e.file_name()))?;
        }
        Ok(())
    } else {
        fs::copy(from, to).map(|_| ())
    }
}

pub fn remove_any(p: &Path) -> io::Result<()> {
    match fs::symlink_metadata(p) {
        Ok(m) if m.is_dir() => fs::remove_dir_all(p),
        Ok(_) => remove_link(p),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

/// Move `from` to `to` (creating parents). Falls back to copy and delete across filesystems.
pub fn move_path(from: &Path, to: &Path) -> io::Result<()> {
    if let Some(d) = to.parent() {
        fs::create_dir_all(d)?;
    }
    if fs::rename(from, to).is_ok() {
        return Ok(());
    }
    copy_recursive(from, to)?;
    remove_any(from)
}

/// Where `abs` is kept inside a backup directory: `<backup>/<abs path without the root>`.
pub fn backup_location(backup_dir: &Path, abs: &Path) -> PathBuf {
    let mut p = backup_dir.to_path_buf();
    for c in abs.components() {
        if let std::path::Component::Normal(n) = c {
            p.push(n);
        }
    }
    p
}

#[cfg(all(test, unix))]
mod follow_tests {
    use super::*;

    #[test]
    fn atomic_write_follow_replaces_the_target_and_keeps_the_link() {
        let d = tempfile::tempdir().unwrap();
        let target = d.path().join("real");
        let link = d.path().join("link");
        fs::write(&target, "old").unwrap();
        std::os::unix::fs::symlink("real", &link).unwrap();
        atomic_write_follow(&link, b"new").unwrap();
        assert!(
            fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(fs::read_to_string(&target).unwrap(), "new");
        // atomic_write alone replaces the link.
        atomic_write(&link, b"x").unwrap();
        assert!(
            !fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
    }
}
