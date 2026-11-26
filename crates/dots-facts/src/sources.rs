//! Where raw facts come from. Detection only talks to a [`FactSources`], so tests feed
//! fixture files and fake command output.

use std::collections::BTreeMap;
use std::process::{Command, Stdio};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CmdOutput {
    pub status: i32,
    pub stdout: String,
}

pub trait FactSources {
    /// Compile-target OS: `macos`, `linux`, `windows` (or whatever `std::env::consts::OS` says).
    fn target_os(&self) -> String;
    fn read_file(&self, path: &str) -> Option<String>;
    /// Run a command. `None` means it could not be started.
    fn run(&self, program: &str, args: &[&str]) -> Option<CmdOutput>;
    fn env(&self, key: &str) -> Option<String>;
}

/// The real machine.
pub struct RealSources;

impl FactSources for RealSources {
    fn target_os(&self) -> String {
        std::env::consts::OS.to_string()
    }

    fn read_file(&self, path: &str) -> Option<String> {
        std::fs::read_to_string(path).ok()
    }

    fn run(&self, program: &str, args: &[&str]) -> Option<CmdOutput> {
        let out = Command::new(program)
            .args(args)
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .ok()?;
        Some(CmdOutput {
            status: out.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        })
    }

    fn env(&self, key: &str) -> Option<String> {
        std::env::var(key).ok().filter(|v| !v.is_empty())
    }
}

/// An in-memory machine for tests. Commands are keyed by the full command line
/// (`"sw_vers -productVersion"`); an unregistered command cannot be started.
#[derive(Debug, Clone, Default)]
pub struct FakeSources {
    os: String,
    files: BTreeMap<String, String>,
    commands: BTreeMap<String, CmdOutput>,
    env: BTreeMap<String, String>,
}

impl FakeSources {
    pub fn new(os: &str) -> Self {
        FakeSources {
            os: os.to_string(),
            ..Default::default()
        }
    }

    pub fn file(mut self, path: &str, content: &str) -> Self {
        self.files.insert(path.to_string(), content.to_string());
        self
    }

    pub fn command(mut self, line: &str, status: i32, stdout: &str) -> Self {
        self.commands.insert(
            line.to_string(),
            CmdOutput {
                status,
                stdout: stdout.to_string(),
            },
        );
        self
    }

    pub fn env(mut self, key: &str, value: &str) -> Self {
        self.env.insert(key.to_string(), value.to_string());
        self
    }
}

impl FactSources for FakeSources {
    fn target_os(&self) -> String {
        self.os.clone()
    }

    fn read_file(&self, path: &str) -> Option<String> {
        self.files.get(path).cloned()
    }

    fn run(&self, program: &str, args: &[&str]) -> Option<CmdOutput> {
        let mut line = program.to_string();
        for a in args {
            line.push(' ');
            line.push_str(a);
        }
        self.commands.get(&line).cloned()
    }

    fn env(&self, key: &str) -> Option<String> {
        self.env.get(key).cloned().filter(|v| !v.is_empty())
    }
}
