//! Long output goes through the pager (terminal-ui "Pager and long content").

use std::io::Write;
use std::process::{Command, Stdio};

use crate::ui::Ui;

/// Show `text` in the pager when there is one (TTY, not Plain, not `--no-input`) and the text is
/// longer than the screen. Returns `false` when nothing was paged, so the caller prints inline.
pub fn page(ui: &Ui, text: &str) -> bool {
    let (Some(cmd), Some(height)) = (&ui.pager, ui.height) else {
        return false;
    };
    // Leave a line for the prompt.
    if text.lines().count() < height {
        return false;
    }
    run_pager(cmd, text)
}

/// Pipe `text` into `cmd`. Returns `false` if the pager could not be started.
pub fn run_pager(cmd: &str, text: &str) -> bool {
    // A bare `less` would show raw escape codes.
    let cmd = if cmd.trim() == "less" {
        "less -RFX"
    } else {
        cmd
    };
    #[cfg(unix)]
    let mut c = {
        let mut c = Command::new("sh");
        c.arg("-c").arg(cmd);
        c
    };
    #[cfg(windows)]
    let mut c = {
        use std::os::windows::process::CommandExt;
        let mut c = Command::new("cmd");
        // `raw_arg`: cmd.exe does its own parsing; Rust's quoting would escape the quotes
        // in a pager command line with backslashes cmd does not understand.
        c.arg("/C").raw_arg(cmd);
        c
    };
    let Ok(mut child) = c.stdin(Stdio::piped()).spawn() else {
        return false;
    };
    if let Some(mut stdin) = child.stdin.take() {
        // The user may quit the pager before the end: a closed pipe is fine.
        let _ = stdin.write_all(text.as_bytes());
    }
    let _ = child.wait();
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Tier;

    #[test]
    fn plain_and_short_output_are_never_paged() {
        let mut ui = Ui::fixed(Tier::Basic, 80);
        ui.height = Some(10);
        let long = "x\n".repeat(50);
        assert!(!page(&ui, &long), "no pager configured");
        ui.pager = Some("false".into());
        assert!(!page(&ui, "short\n"), "fits on screen");
        ui.height = None;
        assert!(!page(&ui, &long), "unknown height");
    }

    #[test]
    fn long_output_goes_through_the_configured_pager() {
        let dir = tempfile::tempdir().unwrap();
        let sink = dir.path().join("out.txt");
        let mut ui = Ui::fixed(Tier::Basic, 80);
        ui.height = Some(10);
        // `cat` does not exist on Windows; `findstr "^"` copies stdin to stdout there.
        let copy = if cfg!(windows) {
            "findstr \"^\""
        } else {
            "cat"
        };
        ui.pager = Some(format!("{copy} > \"{}\"", sink.display()));
        let long = "line\n".repeat(50);
        assert!(page(&ui, &long));
        let got = std::fs::read_to_string(sink).unwrap();
        assert_eq!(got.replace("\r\n", "\n"), long);
    }
}
