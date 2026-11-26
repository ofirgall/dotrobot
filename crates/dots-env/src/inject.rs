use std::io;
use std::path::{Path, PathBuf};

use crate::{EnvError, Shell};

pub const MARKER_OPEN: &str = "# >>> dots env >>>";
pub const MARKER_CLOSE: &str = "# <<< dots env <<<";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InjectOutcome {
    Created,
    Updated,
    Unchanged,
}

/// The rc file dots injects into by default: zsh `~/.zshenv`, bash `~/.bashrc`, fish
/// `~/.config/fish/config.fish`, PowerShell's current-user profile.
pub fn default_rc(shell: Shell, home: &Path) -> PathBuf {
    match shell {
        Shell::Zsh => home.join(".zshenv"),
        Shell::Bash => home.join(".bashrc"),
        Shell::Fish => home.join(".config/fish/config.fish"),
        Shell::PowerShell => home.join(".config/powershell/Microsoft.PowerShell_profile.ps1"),
    }
}

/// The line that sources `env_file`, guarded so a missing file is harmless.
pub fn rc_line(shell: Shell, env_file: &Path, home: &Path) -> String {
    let s = env_file.to_string_lossy();
    let h = home.to_string_lossy();
    let (pre, rest) = match s.strip_prefix(h.as_ref()) {
        Some(r) if r.starts_with('/') || r.starts_with('\\') => ("$HOME", r),
        _ => ("", s.as_ref()),
    };
    let q = format!("\"{pre}{rest}\"");
    match shell {
        Shell::Zsh | Shell::Bash => format!("[ -f {q} ] && . {q}"),
        Shell::Fish => format!("test -f {q}; and source {q}"),
        Shell::PowerShell => format!("if (Test-Path {q}) {{ . {q} }}"),
    }
}

/// Ensure exactly one dots block in `rc`. Text outside the markers is preserved byte for byte;
/// duplicate blocks collapse to the first. A symlinked rc is written through the link.
pub fn inject_rc(
    rc: &Path,
    shell: Shell,
    env_file: &Path,
    home: &Path,
) -> Result<InjectOutcome, EnvError> {
    let target = std::fs::canonicalize(rc).unwrap_or_else(|_| rc.to_path_buf());
    let existed = target.exists();
    let text = match std::fs::read_to_string(&target) {
        Ok(t) => t,
        Err(e) if e.kind() == io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e.into()),
    };
    let block = format!(
        "{MARKER_OPEN}\n{}\n{MARKER_CLOSE}\n",
        rc_line(shell, env_file, home)
    );
    let out =
        replace(&text, &block).ok_or_else(|| EnvError::UnterminatedBlock(rc.to_path_buf()))?;
    if out == text && existed {
        return Ok(InjectOutcome::Unchanged);
    }
    dots_files::atomic_write(&target, out.as_bytes())?;
    Ok(if existed {
        InjectOutcome::Updated
    } else {
        InjectOutcome::Created
    })
}

/// Remove every dots block from `rc`, leaving all other text byte for byte. A missing rc or
/// one without a block is `Unchanged`.
pub fn remove_rc_inject(rc: &Path) -> Result<InjectOutcome, EnvError> {
    let target = std::fs::canonicalize(rc).unwrap_or_else(|_| rc.to_path_buf());
    let text = match std::fs::read_to_string(&target) {
        Ok(t) => t,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(InjectOutcome::Unchanged),
        Err(e) => return Err(e.into()),
    };
    let lines: Vec<&str> = text.split_inclusive('\n').collect();
    let mut out = String::new();
    let mut i = 0;
    while i < lines.len() {
        if lines[i].trim() == MARKER_OPEN {
            let end = (i + 1..lines.len())
                .find(|&j| lines[j].trim() == MARKER_CLOSE)
                .ok_or_else(|| EnvError::UnterminatedBlock(rc.to_path_buf()))?;
            i = end + 1;
        } else {
            out.push_str(lines[i]);
            i += 1;
        }
    }
    if out == text {
        return Ok(InjectOutcome::Unchanged);
    }
    dots_files::atomic_write(&target, out.as_bytes())?;
    Ok(InjectOutcome::Updated)
}

/// None when a block is opened but never closed.
fn replace(text: &str, block: &str) -> Option<String> {
    let lines: Vec<&str> = text.split_inclusive('\n').collect();
    let mut out = String::new();
    let mut placed = false;
    let mut i = 0;
    while i < lines.len() {
        if lines[i].trim() == MARKER_OPEN {
            let end = (i + 1..lines.len()).find(|&j| lines[j].trim() == MARKER_CLOSE)?;
            if !placed {
                out.push_str(block);
                placed = true;
            }
            i = end + 1;
        } else {
            out.push_str(lines[i]);
            i += 1;
        }
    }
    if !placed {
        if !out.is_empty() && !out.ends_with('\n') {
            out.push('\n');
        }
        out.push_str(block);
    }
    Some(out)
}

#[cfg(test)]
mod remove_tests {
    use super::*;

    #[test]
    fn remove_rc_inject_keeps_text_outside_the_block() {
        let d = tempfile::tempdir().unwrap();
        let rc = d.path().join(".zshenv");
        std::fs::write(
            &rc,
            format!("a=1\n{MARKER_OPEN}\nline\n{MARKER_CLOSE}\nb=2\n"),
        )
        .unwrap();
        assert_eq!(remove_rc_inject(&rc).unwrap(), InjectOutcome::Updated);
        assert_eq!(std::fs::read_to_string(&rc).unwrap(), "a=1\nb=2\n");
        assert_eq!(remove_rc_inject(&rc).unwrap(), InjectOutcome::Unchanged);
        assert_eq!(
            remove_rc_inject(&d.path().join("none")).unwrap(),
            InjectOutcome::Unchanged
        );
    }
}
