//! The one place that writes a report to stdout and stderr.

use std::io::Write;

use dots_core::{Diagnostic, Dirs, Env, ExitCode, GlobalOptions, Report};

use crate::config::UiConfig;
use crate::pager::page;
use crate::render::render_with;
use crate::ui::{Term, Ui};

/// Read `ui.toml` from the config dir. A missing file is the default configuration.
pub fn load_config(dirs: &Dirs) -> Result<UiConfig, Diagnostic> {
    let path = dirs.ui_toml();
    match std::fs::read_to_string(&path) {
        Ok(text) => UiConfig::parse(&text, &path.to_string_lossy()),
        Err(_) => Ok(UiConfig::default()),
    }
}

/// Render `report` for this process and write it. Resolves the context with the precedence
/// flag > env > `ui.toml` > detection, pages long human output, and returns the exit code.
///
/// A broken `ui.toml` (for example an unknown key) is reported as an error and turns a
/// successful run into exit 1; output still renders with default preferences. JSON output
/// never depends on `ui.toml`.
pub fn emit(report: &Report, opts: &GlobalOptions, env: &dyn Env, dirs: &Dirs) -> i32 {
    let mut exit = report.exit;
    let mut with_config_error;
    let mut report = report;
    let cfg = if opts.json {
        UiConfig::default()
    } else {
        match load_config(dirs) {
            Ok(c) => c,
            Err(d) => {
                with_config_error = report.clone();
                with_config_error.errors.push(d);
                if exit == ExitCode::Success {
                    exit = ExitCode::Error;
                    with_config_error.exit = exit;
                }
                report = &with_config_error;
                UiConfig::default()
            }
        }
    };
    let ui = Ui::resolve(opts, env, &cfg, &Term::detect());
    let out = render_with(report, &ui, opts);
    // A closed pipe is not our failure; ignore write errors.
    if opts.json || !page(&ui, &out.stdout) {
        let _ = std::io::stdout().write_all(out.stdout.as_bytes());
    }
    let _ = std::io::stderr().write_all(out.stderr.as_bytes());
    exit.code()
}
