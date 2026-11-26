//! The `terminfo` file mode: compile a source with `tic -x -o <dir>`; the entry is `ok` when
//! `infocmp -x -A <dir> <name>` matches what the source compiles to.
//!
//! The tools run through [`Tools`], so tests script `tic` and `infocmp` instead of needing
//! ncurses.

use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;

use crate::model::FileState;

/// What a finished tool printed.
#[derive(Debug, Clone, Default)]
pub struct ToolOutput {
    pub status: i32,
    pub stdout: String,
    pub stderr: String,
}

impl ToolOutput {
    pub fn success(&self) -> bool {
        self.status == 0
    }
}

/// The seam for `tic` and `infocmp`.
pub trait Tools: Send + Sync + fmt::Debug {
    fn run(&self, program: &str, args: &[String]) -> io::Result<ToolOutput>;
}

/// Real processes, found through `PATH`.
#[derive(Debug, Default)]
pub struct SystemTools;

impl Tools for SystemTools {
    fn run(&self, program: &str, args: &[String]) -> io::Result<ToolOutput> {
        let out = Command::new(program)
            .args(args)
            .stdin(Stdio::null())
            .output()?;
        Ok(ToolOutput {
            status: out.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        })
    }
}

pub fn system_tools() -> Arc<dyn Tools> {
    Arc::new(SystemTools)
}

/// The entry's name: the first term of the first definition line (`name|alias|description,`).
pub fn source_name(text: &str) -> Option<String> {
    let line = text.lines().find(|l| {
        !l.trim().is_empty() && !l.trim_start().starts_with('#') && !l.starts_with([' ', '\t'])
    })?;
    let head = line.split([',', '|']).next()?.trim();
    (!head.is_empty()).then(|| head.to_string())
}

/// `tic -x -o <dir> <src>`.
pub fn tic_args(dir: &Path, src: &Path) -> Vec<String> {
    vec![
        "-x".into(),
        "-o".into(),
        dir.display().to_string(),
        src.display().to_string(),
    ]
}

/// The reconstructed entry without the `#` header lines (they name the file it came from).
fn dump(tools: &dyn Tools, dir: &Path, name: &str) -> Option<String> {
    let args = vec![
        "-x".to_string(),
        "-A".to_string(),
        dir.display().to_string(),
        name.to_string(),
    ];
    let out = tools
        .run("infocmp", &args)
        .ok()
        .filter(ToolOutput::success)?;
    Some(
        out.stdout
            .lines()
            .filter(|l| !l.starts_with('#'))
            .collect::<Vec<_>>()
            .join("\n"),
    )
}

/// Observe one entry. `dir` is the terminfo directory, `scratch` a directory to compile the
/// source into for comparison.
pub fn observe(tools: &dyn Tools, dir: &Path, name: &str, src: &Path, scratch: &Path) -> FileState {
    let Some(installed) = dump(tools, dir, name) else {
        return FileState::Missing;
    };
    let _ = fs::remove_dir_all(scratch);
    let compiled = fs::create_dir_all(scratch)
        .ok()
        .and_then(|()| tools.run("tic", &tic_args(scratch, src)).ok())
        .filter(ToolOutput::success)
        .and_then(|_| dump(tools, scratch, name));
    let _ = fs::remove_dir_all(scratch);
    match compiled {
        Some(c) if c == installed => FileState::Ok,
        _ => FileState::Stale,
    }
}

/// Compile for real. The error is the tool's message.
pub fn compile(tools: &dyn Tools, dir: &Path, src: &Path) -> Result<(), String> {
    fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    match tools.run("tic", &tic_args(dir, src)) {
        Ok(o) if o.success() => Ok(()),
        Ok(o) => Err(format!("tic exited {}: {}", o.status, o.stderr.trim())),
        Err(e) => Err(format!("cannot run tic: {e}")),
    }
}

/// Delete the compiled entry (`<dir>/<first letter>/<name>`, or `<dir>/<hex>/<name>` on macOS).
pub fn remove_compiled(dir: &Path, name: &str) -> io::Result<()> {
    let Some(first) = name.chars().next() else {
        return Ok(());
    };
    let candidates: [PathBuf; 2] = [
        dir.join(first.to_string()).join(name),
        dir.join(format!("{:x}", first as u32)).join(name),
    ];
    for p in candidates {
        if p.is_file() {
            fs::remove_file(&p)?;
            if let Some(parent) = p.parent()
                && fs::read_dir(parent).is_ok_and(|mut d| d.next().is_none())
            {
                let _ = fs::remove_dir(parent);
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_the_first_entry() {
        let t = "# comment\n\nherdr-256color|herdr with undercurl,\n\tam, bce,\n";
        assert_eq!(source_name(t).as_deref(), Some("herdr-256color"));
        assert_eq!(source_name("# only comments\n"), None);
        assert_eq!(source_name("xterm,\n\tam,\n").as_deref(), Some("xterm"));
    }
}
