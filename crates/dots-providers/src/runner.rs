//! The command-runner seam. Every provider runs every external command through
//! `Ctx::runner`, so tests can script outputs and count calls, and real runs use
//! [`RealRunner`]. Providers must pass an absolute program path (resolved by the provider,
//! never via `PATH`).

use std::collections::BTreeMap;
use std::fmt;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use crate::{ProviderError, Result};

/// Default command timeout (matches the script default of 20 minutes).
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(20 * 60);

/// What a command reads on stdin. `Null` (`/dev/null`) is the default so nothing hangs.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Stdin {
    #[default]
    Null,
    Inherit,
    Bytes(Vec<u8>),
}

/// One command to run: program path, arguments, environment, working directory, stdin, timeout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cmd {
    /// Program, an absolute path for provider-owned binaries.
    pub program: PathBuf,
    pub args: Vec<String>,
    /// Variables set on top of the inherited environment (or on top of nothing, see `clear_env`).
    pub env: BTreeMap<String, String>,
    /// Start from an empty environment instead of inheriting the process's.
    pub clear_env: bool,
    pub cwd: Option<PathBuf>,
    pub stdin: Stdin,
    pub timeout: Option<Duration>,
}

impl Cmd {
    pub fn new(program: impl Into<PathBuf>) -> Self {
        Cmd {
            program: program.into(),
            args: Vec::new(),
            env: BTreeMap::new(),
            clear_env: false,
            cwd: None,
            stdin: Stdin::Null,
            timeout: Some(DEFAULT_TIMEOUT),
        }
    }

    /// Build from an `Action.argv`: first element is the program.
    pub fn from_argv(argv: &[String]) -> Option<Self> {
        let (prog, rest) = argv.split_first()?;
        Some(Cmd::new(prog).args(rest))
    }

    pub fn arg(mut self, a: impl Into<String>) -> Self {
        self.args.push(a.into());
        self
    }

    pub fn args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.args.extend(args.into_iter().map(Into::into));
        self
    }

    pub fn env(mut self, k: impl Into<String>, v: impl Into<String>) -> Self {
        self.env.insert(k.into(), v.into());
        self
    }

    pub fn envs(mut self, env: &BTreeMap<String, String>) -> Self {
        self.env
            .extend(env.iter().map(|(k, v)| (k.clone(), v.clone())));
        self
    }

    pub fn cwd(mut self, dir: impl Into<PathBuf>) -> Self {
        self.cwd = Some(dir.into());
        self
    }

    pub fn stdin(mut self, s: Stdin) -> Self {
        self.stdin = s;
        self
    }

    pub fn timeout(mut self, t: Option<Duration>) -> Self {
        self.timeout = t;
        self
    }

    /// The command line as a vector, exactly `program` then `args`.
    pub fn argv(&self) -> Vec<String> {
        std::iter::once(self.program.to_string_lossy().into_owned())
            .chain(self.args.iter().cloned())
            .collect()
    }

    /// Display form of the command line.
    pub fn line(&self) -> String {
        self.argv().join(" ")
    }
}

/// Result of a finished command.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Output {
    /// Exit status; `-1` when the process was killed (signal or timeout).
    pub status: i32,
    pub stdout: String,
    pub stderr: String,
    pub timed_out: bool,
}

impl Output {
    pub fn ok(stdout: &str) -> Self {
        Output {
            status: 0,
            stdout: stdout.into(),
            ..Default::default()
        }
    }

    pub fn fail(status: i32, stderr: &str) -> Self {
        Output {
            status,
            stderr: stderr.into(),
            ..Default::default()
        }
    }

    pub fn success(&self) -> bool {
        self.status == 0 && !self.timed_out
    }
}

/// Runs commands. `Err` means the command could not be started; a non-zero exit is `Ok`.
pub trait Runner: Send + Sync + fmt::Debug {
    fn run(&self, cmd: &Cmd) -> Result<Output>;

    /// True only for the runner that really spawns processes. Hermetic in-memory backends
    /// (the testkit fake) use this to avoid executing fake binaries when given a default `Ctx`.
    fn is_real(&self) -> bool {
        false
    }
}

/// Kill the command's process group (the child leads its own group, see `run`), so a
/// timed-out script does not leave orphans holding its output open.
fn kill_tree(child: &std::process::Child, cmd: &Cmd) {
    #[cfg(unix)]
    if cmd.stdin != Stdin::Inherit {
        let _ = Command::new("/bin/sh")
            .arg("-c")
            .arg(format!("kill -KILL -- -{}", child.id()))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    #[cfg(not(unix))]
    let _ = (child, cmd);
}

/// Spawns real processes.
#[derive(Debug, Default, Clone, Copy)]
pub struct RealRunner;

impl Runner for RealRunner {
    fn is_real(&self) -> bool {
        true
    }

    fn run(&self, cmd: &Cmd) -> Result<Output> {
        let spawn_err =
            |e: std::io::Error| ProviderError::Other(format!("cannot run `{}`: {e}", cmd.line()));
        let mut c = Command::new(&cmd.program);
        c.args(&cmd.args);
        if cmd.clear_env {
            c.env_clear();
        }
        c.envs(&cmd.env);
        if let Some(d) = &cmd.cwd {
            c.current_dir(d);
        }
        c.stdin(match cmd.stdin {
            Stdin::Null => Stdio::null(),
            Stdin::Inherit => Stdio::inherit(),
            Stdin::Bytes(_) => Stdio::piped(),
        });
        c.stdout(Stdio::piped()).stderr(Stdio::piped());
        // Own process group so a timeout can kill the whole tree. Not for interactive
        // commands: a background group reading the terminal would be stopped.
        #[cfg(unix)]
        if cmd.stdin != Stdin::Inherit {
            use std::os::unix::process::CommandExt;
            c.process_group(0);
        }
        let mut child = c.spawn().map_err(spawn_err)?;
        if let (Stdin::Bytes(b), Some(mut si)) = (&cmd.stdin, child.stdin.take()) {
            let b = b.clone();
            thread::spawn(move || {
                let _ = si.write_all(&b);
            });
        }
        let reader = |mut r: Box<dyn Read + Send>| {
            thread::spawn(move || {
                let mut buf = Vec::new();
                let _ = r.read_to_end(&mut buf);
                String::from_utf8_lossy(&buf).into_owned()
            })
        };
        let out_t = child.stdout.take().map(|s| reader(Box::new(s)));
        let err_t = child.stderr.take().map(|s| reader(Box::new(s)));
        let start = Instant::now();
        let mut timed_out = false;
        let status = loop {
            if let Some(st) = child.try_wait().map_err(spawn_err)? {
                break st.code().unwrap_or(-1);
            }
            if cmd.timeout.is_some_and(|t| start.elapsed() >= t) {
                timed_out = true;
                kill_tree(&child, cmd);
                let _ = child.kill();
                let _ = child.wait();
                break -1;
            }
            thread::sleep(Duration::from_millis(5));
        };
        let join = |t: Option<thread::JoinHandle<String>>| {
            t.and_then(|h| h.join().ok()).unwrap_or_default()
        };
        Ok(Output {
            status,
            stdout: join(out_t),
            stderr: join(err_t),
            timed_out,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn real_runner_runs_and_times_out() {
        let r = RealRunner;
        let o = r.run(&Cmd::new("/bin/echo").arg("hi")).unwrap();
        assert!(o.success() && o.stdout == "hi\n");
        let o = r
            .run(
                &Cmd::new("/bin/sleep")
                    .arg("5")
                    .timeout(Some(Duration::from_millis(50))),
            )
            .unwrap();
        assert!(o.timed_out && !o.success());
        assert!(r.run(&Cmd::new("/nonexistent/x")).is_err());
    }

    #[test]
    fn stdin_is_null_by_default() {
        let o = RealRunner.run(&Cmd::new("/bin/cat")).unwrap();
        assert!(o.success() && o.stdout.is_empty());
    }
}
