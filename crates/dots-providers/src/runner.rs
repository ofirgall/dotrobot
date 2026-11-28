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

/// A Windows job object holding the child and everything it starts: terminating it kills the
/// whole tree, including processes whose parent link `taskkill /T` cannot follow (MSYS
/// shells fork children that way). It does not kill on close: a script that leaves a
/// background process running on purpose keeps it.
#[cfg(windows)]
mod job {
    use std::os::windows::io::AsRawHandle;

    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, TerminateJobObject,
    };

    pub struct Job(HANDLE);

    // SAFETY: a job handle is just a kernel object reference; any thread may use it.
    #[allow(unsafe_code)]
    unsafe impl Send for Job {}
    #[allow(unsafe_code)]
    unsafe impl Sync for Job {}

    impl Job {
        /// A new job containing `child`, or `None` when the system refuses (the caller falls
        /// back to `taskkill`).
        #[allow(unsafe_code)]
        pub fn containing(child: &std::process::Child) -> Option<Job> {
            // SAFETY: plain Win32 calls with a null security descriptor and name; the handle
            // is checked before use and closed in `Drop`. `child`'s handle is valid while
            // the `Child` is alive, which the borrow guarantees.
            unsafe {
                let h = CreateJobObjectW(std::ptr::null(), std::ptr::null());
                if h.is_null() {
                    return None;
                }
                let job = Job(h);
                (AssignProcessToJobObject(h, child.as_raw_handle() as HANDLE) != 0).then_some(job)
            }
        }

        #[allow(unsafe_code)]
        pub fn terminate(&self) {
            // SAFETY: `self.0` is the live job handle created in `containing`.
            unsafe {
                TerminateJobObject(self.0, 1);
            }
        }
    }

    impl Drop for Job {
        #[allow(unsafe_code)]
        fn drop(&mut self) {
            // SAFETY: the handle is owned by this value and closed exactly once.
            unsafe {
                CloseHandle(self.0);
            }
        }
    }
}

/// What groups the child's descendants for a timeout kill: a job object on Windows (when one
/// could be made), nothing elsewhere (unix kills the process group by id).
#[cfg(windows)]
type Group = Option<job::Job>;
#[cfg(not(windows))]
type Group = ();

#[cfg(windows)]
fn join_group(child: &std::process::Child, cmd: &Cmd) -> Group {
    if own_group(cmd) {
        job::Job::containing(child)
    } else {
        None
    }
}
#[cfg(not(windows))]
fn join_group(_child: &std::process::Child, _cmd: &Cmd) -> Group {}

/// Kill the command's process group (the child leads its own group, see `run`), so a
/// timed-out script does not leave orphans holding its output open. A child in the
/// terminal's group is killed alone (by the caller).
#[cfg_attr(not(windows), allow(unused_variables))]
fn kill_tree(child: &std::process::Child, cmd: &Cmd, group: &Group) {
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
    // Windows has no process groups to signal: `taskkill /T` walks the child's process tree
    // (children carry their parent's pid), `/F` forces it.
    #[cfg(windows)]
    if own_group(cmd) {
        // The tree walk first, while the parent links are intact; the job then takes whatever
        // that missed.
        let _ = Command::new("taskkill")
            .args(["/T", "/F", "/PID"])
            .arg(child.id().to_string())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        if let Some(j) = group {
            j.terminate();
        }
    }
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
        // Windows has no controlling-terminal prompt to dodge (ssh and git ask through the
        // console, which `CREATE_NO_WINDOW` leaves the child without), so there is no
        // session to detach: a non-interactive command just gets no console of its own.
        #[cfg(windows)]
        if !keeps_tty(cmd) {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            c.creation_flags(CREATE_NO_WINDOW);
        }
        let mut child = c.spawn().map_err(spawn_err)?;
        #[cfg_attr(not(windows), allow(clippy::let_unit_value))]
        let group = join_group(&child, cmd);
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
                kill_tree(&child, cmd, &group);
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

    /// A command line for the platform's shell: `/bin/sh -c` on unix, `cmd.exe /C` on Windows.
    fn shell(line: &str) -> Cmd {
        #[cfg(unix)]
        return Cmd::new("/bin/sh").arg("-c").arg(line);
        #[cfg(windows)]
        return Cmd::new(system32("cmd.exe")).arg("/C").arg(line);
    }

    #[cfg(windows)]
    fn system32(exe: &str) -> PathBuf {
        PathBuf::from(std::env::var_os("SystemRoot").expect("SystemRoot"))
            .join("System32")
            .join(exe)
    }

    /// Sleeps well past the timeouts the tests use.
    fn sleep_a_while() -> Cmd {
        #[cfg(unix)]
        return Cmd::new("/bin/sleep").arg("5");
        #[cfg(windows)]
        return shell("ping -n 6 127.0.0.1 > nul");
    }

    #[test]
    fn real_runner_runs_and_times_out() {
        let r = RealRunner;
        let o = r.run(&shell("echo hi")).unwrap();
        assert!(o.success() && o.stdout.trim_end() == "hi");
        let o = r
            .run(&sleep_a_while().timeout(Some(Duration::from_millis(50))))
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
        #[cfg(unix)]
        let line = "echo one; printf 'two\\rthree\\n' >&2";
        #[cfg(windows)]
        let line = "echo one& echo two>&2& echo three>&2";
        let o = with_line_sink(Some(sink), || RealRunner.run(&shell(line)).unwrap());
        // The collected output is untouched.
        #[cfg(unix)]
        {
            assert_eq!(o.stdout, "one\n");
            assert_eq!(o.stderr, "two\rthree\n");
        }
        #[cfg(windows)]
        {
            assert_eq!(o.stdout.trim_end(), "one");
            assert_eq!(o.stderr.replace("\r\n", "\n").trim_end(), "two\nthree");
        }
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

    /// Windows: a timeout kills the whole tree (`taskkill /T`), so a grandchild does not hold
    /// the output pipes open until it ends on its own.
    #[cfg(windows)]
    #[test]
    fn a_timeout_kills_the_process_tree() {
        let t = Instant::now();
        // cmd starts ping (a grandchild of the runner) that outlives cmd by 30 seconds.
        let o = RealRunner
            .run(
                &shell("ping -n 30 127.0.0.1 > nul & ping -n 30 127.0.0.1 > nul")
                    .timeout(Some(Duration::from_millis(500))),
            )
            .unwrap();
        assert!(o.timed_out);
        assert!(t.elapsed() < Duration::from_secs(10), "{:?}", t.elapsed());
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
            #[cfg(unix)]
            {
                let o = RealRunner
                    .run(&cmd.arg("echo \"$GIT_TERMINAL_PROMPT|$GIT_SSH_COMMAND\""))
                    .unwrap();
                o.stdout.trim().to_string()
            }
            // `set GIT` lists the `GIT*` variables as `NAME=value` lines.
            #[cfg(windows)]
            {
                let o = RealRunner.run(&cmd.arg("set GIT")).unwrap();
                let get = |name: &str| {
                    o.stdout
                        .lines()
                        .find_map(|l| l.strip_prefix(&format!("{name}=")))
                        .unwrap_or("")
                        .to_string()
                };
                format!("{}|{}", get("GIT_TERMINAL_PROMPT"), get("GIT_SSH_COMMAND"))
            }
        };
        let sh = || {
            #[cfg(unix)]
            return Cmd::new("/bin/sh").arg("-c");
            #[cfg(windows)]
            return Cmd::new(system32("cmd.exe")).arg("/C");
        };
        assert_eq!(probe(sh()), "0|ssh -o BatchMode=yes");
        let c = sh()
            .env("GIT_SSH_COMMAND", "ssh -i k")
            .env("GIT_TERMINAL_PROMPT", "1");
        assert_eq!(probe(c), "1|ssh -i k");
        assert_eq!(probe(sh().env("GIT_SSH", "/x/ssh")), "0|");
    }

    #[test]
    fn stdin_is_null_by_default() {
        #[cfg(unix)]
        {
            let o = RealRunner.run(&Cmd::new("/bin/cat")).unwrap();
            assert!(o.success() && o.stdout.is_empty());
        }
        // PowerShell prints how many characters it could read from stdin.
        #[cfg(windows)]
        {
            let ps = Cmd::new(system32("WindowsPowerShell/v1.0/powershell.exe")).args([
                "-NoProfile",
                "-Command",
                "[Console]::In.ReadToEnd().Length",
            ]);
            let o = RealRunner.run(&ps).unwrap();
            assert!(o.success() && o.stdout.trim() == "0", "{o:?}");
        }
    }
}
