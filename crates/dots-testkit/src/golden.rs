//! Golden-output harness: runs the `dots` binary in a temp `--home` and snapshots its
//! plain, rich and JSON output. Set `DOTS_UPDATE_GOLDEN=1` to (re)write the golden files.

use std::path::{Path, PathBuf};
use std::process::Command;

use tempfile::TempDir;

use crate::sys::{system_env, system_path};

pub struct RunOutput {
    pub stdout: String,
    pub stderr: String,
    pub code: i32,
}

pub struct Golden {
    bin: PathBuf,
    dir: PathBuf,
}

impl Golden {
    /// `bin` is the `dots` binary (`env!("CARGO_BIN_EXE_dots")`), `dir` holds the golden files.
    pub fn new(bin: impl Into<PathBuf>, dir: impl Into<PathBuf>) -> Self {
        Golden {
            bin: bin.into(),
            dir: dir.into(),
        }
    }

    /// Run `dots <args>` in a fresh sandbox with a clean environment. `extra_env` is added last.
    pub fn run(&self, args: &[&str], extra_env: &[(&str, &str)]) -> RunOutput {
        let sandbox = TempDir::new().expect("tempdir");
        let home = sandbox.path().join("home");
        std::fs::create_dir_all(&home).unwrap();
        let mut cmd = Command::new(&self.bin);
        cmd.env_clear()
            .envs(system_env())
            .env("PATH", system_path())
            // The real HOME points at an empty sentinel: nothing should ever touch it.
            .env("HOME", sandbox.path().join("sentinel"))
            .current_dir(sandbox.path())
            .arg("--home")
            .arg(&home)
            .args(args);
        for (k, v) in extra_env {
            cmd.env(k, v);
        }
        let out = cmd.output().expect("run dots");
        let norm = |b: &[u8]| {
            String::from_utf8_lossy(b)
                .replace(sandbox.path().to_string_lossy().as_ref(), "<SANDBOX>")
        };
        RunOutput {
            stdout: norm(&out.stdout),
            stderr: norm(&out.stderr),
            code: out.status.code().unwrap_or(-1),
        }
    }

    /// Snapshot the three output forms of one command as `<name>.{plain,rich,json}.txt`.
    pub fn snapshot(&self, name: &str, args: &[&str]) {
        let plain = self.run(&[args, &["--color", "never"]].concat(), &[]);
        let rich = self.run(
            &[args, &["--color", "always"]].concat(),
            &[("COLORTERM", "truecolor")],
        );
        let json = self.run(&[args, &["--json"]].concat(), &[]);
        for (kind, out) in [("plain", plain), ("rich", rich), ("json", json)] {
            let text = format!(
                "exit: {}\n--- stdout ---\n{}--- stderr ---\n{}",
                out.code, out.stdout, out.stderr
            );
            self.compare(&self.dir.join(format!("{name}.{kind}.txt")), &text);
        }
    }

    pub fn compare(&self, path: &Path, actual: &str) {
        if std::env::var("DOTS_UPDATE_GOLDEN").is_ok_and(|v| v == "1") {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, actual).unwrap();
            return;
        }
        let expected = std::fs::read_to_string(path).unwrap_or_default();
        assert_eq!(
            expected,
            actual,
            "golden mismatch: {} (rerun with DOTS_UPDATE_GOLDEN=1 to update)",
            path.display()
        );
    }
}
