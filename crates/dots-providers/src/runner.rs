//! The command-runner seam. Every provider runs every external command through
//! `Ctx::runner`, so tests can script outputs and count calls, and real runs use
//! [`RealRunner`]. Providers must pass an absolute program path (resolved by the provider,
//! never via `PATH`).

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::fmt;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use crate::{ProviderError, Result};

/// Default command timeout (matches the script default of 20 minutes).
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(20 * 60);

/// Receives a command's output line by line while it runs (progress display only: the full
/// output is still collected into [`Output`] and the log files).
pub type LineSink = Arc<dyn Fn(&str) + Send + Sync>;

thread_local! {
    static LINE_SINK: RefCell<Option<LineSink>> = const { RefCell::new(None) };
}

/// Run `f` with `sink` receiving the output lines of every command [`RealRunner`] runs on
/// this thread meanwhile. Optional and invisible to providers: nothing changes when no sink
/// is set. The previous sink is restored afterwards.
pub fn with_line_sink<R>(sink: Option<LineSink>, f: impl FnOnce() -> R) -> R {
    struct Restore(Option<LineSink>);
    impl Drop for Restore {
        fn drop(&mut self) {
            LINE_SINK.with(|s| *s.borrow_mut() = self.0.take());
        }
    }
    let _restore = Restore(LINE_SINK.with(|s| s.replace(sink)));
    f()
}

thread_local! {
    static KEEP_TTY: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Run `f` with every command [`RealRunner`] runs on this thread keeping the user's terminal
/// (when `on`). For steps that need sudo without being run through it: a cask whose installer
/// calls `sudo` itself would otherwise be detached from the terminal its sudo ticket is keyed
/// by, and fail with "a terminal is required". The previous setting is restored afterwards.
pub fn with_tty<R>(on: bool, f: impl FnOnce() -> R) -> R {
    struct Restore(bool);
    impl Drop for Restore {
        fn drop(&mut self) {
            KEEP_TTY.with(|k| k.set(self.0));
        }
    }
    let _restore = Restore(KEEP_TTY.with(|k| k.replace(on)));
    f()
}

fn current_line_sink() -> Option<LineSink> {
    LINE_SINK.with(|s| s.borrow().clone())
}

/// Split `chunk` into lines (on `\n` and `\r`, so progress meters that redraw one line count),
/// reporting each complete one, then the unfinished tail so a slow tool still shows life.
fn feed_lines(pending: &mut Vec<u8>, chunk: &[u8], sink: &LineSink) {
    for &b in chunk {
        if b == b'\n' || b == b'\r' {
            if !pending.is_empty() {
                sink(&String::from_utf8_lossy(pending));
                pending.clear();
            }
        } else {
            pending.push(b);
        }
    }
    if !pending.is_empty() {
        sink(&String::from_utf8_lossy(pending));
    }
}

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

/// Does this command need the user's terminal? An interactive command (inherited stdin) does,
/// and so does `sudo`: its credential cache is keyed by the controlling terminal, so a
/// detached `sudo -n` would not see the ticket `sudo -v` just primed. So does anything run
/// inside [`with_tty`] (a sudo step whose tool calls sudo itself).
fn keeps_tty(cmd: &Cmd) -> bool {
    cmd.stdin == Stdin::Inherit
        || cmd.program.file_name().is_some_and(|n| n == "sudo")
        || KEEP_TTY.with(|k| k.get())
}

/// Make git and ssh fail instead of asking. Only variables the user (or the package) has not
/// set themselves, so their own ssh wrapper wins.
///
/// `GIT_SSH_COMMAND` replaces a `core.sshCommand` from the git config, so a user who relies
/// on that must set `GIT_SSH_COMMAND` themselves; `~/.ssh/config` is untouched (`BatchMode`
/// only turns the prompts off).
fn no_prompt_env(c: &mut Command, cmd: &Cmd) {
    let set =
        |k: &str| cmd.env.contains_key(k) || (!cmd.clear_env && std::env::var_os(k).is_some());
    if !set("GIT_TERMINAL_PROMPT") {
        c.env("GIT_TERMINAL_PROMPT", "0");
    }
    if !set("GIT_SSH_COMMAND") && !set("GIT_SSH") {
        c.env("GIT_SSH_COMMAND", "ssh -o BatchMode=yes");
    }
}

/// Whether the child leads its own process group (see `run`). A command run inside
/// [`with_tty`] stays in the terminal's foreground group: the tool it runs may call sudo,
/// and a sudo that has to ask from a background group is stopped by `SIGTTOU`/`SIGTTIN`
/// and hangs, silently, until the timeout.
fn own_group(cmd: &Cmd) -> bool {
    cmd.stdin != Stdin::Inherit && !KEEP_TTY.with(|k| k.get())
}

/// Kill the command's process group (the child leads its own group, see `run`), so a
/// timed-out script does not leave orphans holding its output open. A child in the
/// terminal's group is killed alone (by the caller).
fn kill_tree(child: &std::process::Child, cmd: &Cmd) {
    #[cfg(unix)]
    if own_group(cmd) {
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

/// Run `setsid()` in the child between fork and exec.
#[cfg(unix)]
#[allow(unsafe_code)]
fn detach_from_tty(c: &mut Command) {
    use std::os::unix::process::CommandExt;
    // SAFETY: the closure only calls `setsid(2)`, which is async-signal-safe, and allocates
    // nothing (the error path builds an `io::Error` from an errno, no heap).
    unsafe {
        c.pre_exec(|| {
            rustix::process::setsid()
                .map(|_| ())
                .map_err(std::io::Error::from)
        });
    }
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
        if !keeps_tty(cmd) {
            no_prompt_env(&mut c, cmd);
        }
        if let Some(d) = &cmd.cwd {
            c.current_dir(d);
        }
        c.stdin(match cmd.stdin {
            Stdin::Null => Stdio::null(),
            Stdin::Inherit => Stdio::inherit(),
            Stdin::Bytes(_) => Stdio::piped(),
        });
        c.stdout(Stdio::piped()).stderr(Stdio::piped());
        // A command that is not interactive gets no terminal at all: ssh and git prompt on
        // `/dev/tty`, not stdin, so `/dev/null` as stdin does not stop a host-key or
        // passphrase prompt nobody can see. A new session has no controlling terminal, so
        // opening `/dev/tty` fails at once. It is also its own process group, which is what
        // lets a timeout kill the whole tree. Sudo (terminal-keyed credentials) only gets the
        // process group; an interactive command, or one inside `with_tty` whose tool may
        // prompt for a sudo password, keeps both (a background group reading the terminal
        // would be stopped).
        #[cfg(unix)]
        if !keeps_tty(cmd) {
            detach_from_tty(&mut c);
        } else if own_group(cmd) {
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
        let sink = current_line_sink();
        let reader = |mut r: Box<dyn Read + Send>| {
            let sink = sink.clone();
            thread::spawn(move || {
                let mut buf = Vec::new();
                let mut pending = Vec::new();
                let mut chunk = [0u8; 8192];
                loop {
                    match r.read(&mut chunk) {
                        Ok(0) => break,
                        Ok(n) => {
                            buf.extend_from_slice(&chunk[..n]);
                            if let Some(s) = &sink {
                                feed_lines(&mut pending, &chunk[..n], s);
                            }
                        }
                        Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                        Err(_) => break,
                    }
                }
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
    fn with_tty_keeps_the_terminal_and_restores_after() {
        let plain = Cmd::new("/bin/echo");
        assert!(!keeps_tty(&plain));
        with_tty(true, || {
            assert!(keeps_tty(&plain) && !own_group(&plain));
            with_tty(false, || assert!(!keeps_tty(&plain)));
            assert!(keeps_tty(&plain));
        });
        assert!(!keeps_tty(&plain));
        assert!(own_group(&Cmd::new("sudo")));
    }

    #[test]
    fn a_line_sink_sees_output_lines_while_the_command_runs() {
        use std::sync::Mutex;
        let seen = Arc::new(Mutex::new(Vec::<String>::new()));
        let s = seen.clone();
        let sink: LineSink = Arc::new(move |l: &str| s.lock().unwrap().push(l.to_string()));
        let o = with_line_sink(Some(sink), || {
            RealRunner
                .run(
                    &Cmd::new("/bin/sh")
                        .arg("-c")
                        .arg("echo one; printf 'two\\rthree\\n' >&2"),
                )
                .unwrap()
        });
        // The collected output is untouched.
        assert_eq!(o.stdout, "one\n");
        assert_eq!(o.stderr, "two\rthree\n");
        let seen = seen.lock().unwrap();
        for want in ["one", "two", "three"] {
            assert!(seen.iter().any(|l| l == want), "{seen:?}");
        }
        // Outside the scope there is no sink.
        assert!(current_line_sink().is_none());
    }

    #[cfg(unix)]
    #[test]
    fn a_command_cannot_prompt_on_the_terminal() {
        // ssh and git read /dev/tty, not stdin. Detached, opening it fails at once (when the
        // test itself has no terminal the open fails either way).
        let t = Instant::now();
        let o = RealRunner
            .run(
                &Cmd::new("/bin/sh")
                    .arg("-c")
                    .arg("read x < /dev/tty")
                    .timeout(Some(Duration::from_secs(10))),
            )
            .unwrap();
        assert!(!o.timed_out && !o.success(), "{o:?}");
        assert!(t.elapsed() < Duration::from_secs(5));
    }

    #[cfg(unix)]
    #[test]
    fn a_timeout_still_kills_a_detached_tree() {
        let t = Instant::now();
        let o = RealRunner
            .run(
                &Cmd::new("/bin/sh")
                    .arg("-c")
                    .arg("sleep 30 & sleep 30")
                    .timeout(Some(Duration::from_millis(100))),
            )
            .unwrap();
        assert!(o.timed_out);
        assert!(t.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn prompts_are_off_unless_the_user_chose_otherwise() {
        let probe = |mut cmd: Cmd| {
            // `clear_env` makes the process environment irrelevant to the test.
            cmd.clear_env = true;
            let o = RealRunner
                .run(&cmd.arg("echo \"$GIT_TERMINAL_PROMPT|$GIT_SSH_COMMAND\""))
                .unwrap();
            o.stdout.trim().to_string()
        };
        let sh = || Cmd::new("/bin/sh").arg("-c");
        assert_eq!(probe(sh()), "0|ssh -o BatchMode=yes");
        let c = sh()
            .env("GIT_SSH_COMMAND", "ssh -i k")
            .env("GIT_TERMINAL_PROMPT", "1");
        assert_eq!(probe(c), "1|ssh -i k");
        assert_eq!(probe(sh().env("GIT_SSH", "/x/ssh")), "0|");
    }

    #[test]
    fn stdin_is_null_by_default() {
        let o = RealRunner.run(&Cmd::new("/bin/cat")).unwrap();
        assert!(o.success() && o.stdout.is_empty());
    }
}
