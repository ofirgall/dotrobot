//! Parse, dispatch, render, exit.

use std::ffi::OsString;
use std::io::Write;

use clap::FromArgMatches;
use clap::error::ErrorKind;
use dots_core::commands::{COMMANDS, USAGE};
use dots_core::report::Diagnostic;
use dots_core::{Dirs, ExitCode, GlobalOptions, ProcessEnv, Report};
use dots_ui::{Mode, render};

use crate::cli::{self, Cli};
use crate::handlers;

/// Everything a handler may need besides its own arguments.
pub struct Context {
    pub opts: GlobalOptions,
    pub dirs: Dirs,
}

pub fn run(args: Vec<OsString>) -> i32 {
    let env = ProcessEnv;
    let json_requested = args.iter().any(|a| a == "--json");

    let cli = match cli::command()
        .try_get_matches_from(&args)
        .and_then(|m| Cli::from_arg_matches(&m).map_err(|e| e.format(&mut cli::command())))
    {
        Ok(cli) => cli,
        Err(err) => return handle_parse_error(err, &args, json_requested),
    };

    let opts = cli.global.to_options();
    let dirs = Dirs::resolve(opts.home.as_deref(), &env);
    let ctx = Context { opts, dirs };
    let report = handlers::dispatch(cli.command.as_ref(), &ctx);

    // Resolves flags > env > ui.toml > detection, pages long output, writes stdout/stderr.
    dots_ui::emit(&report, &ctx.opts, &env, &ctx.dirs)
}

fn emit(report: &Report, mode: Mode, opts: &GlobalOptions) {
    let out = render(report, mode, opts);
    // A closed pipe is not our failure; ignore write errors.
    let _ = std::io::stdout().write_all(out.stdout.as_bytes());
    let _ = std::io::stderr().write_all(out.stderr.as_bytes());
}

/// Help and version exit 0. Every real parse failure is a usage error: exit 64, not clap's 2.
fn handle_parse_error(err: clap::Error, args: &[OsString], json: bool) -> i32 {
    if matches!(
        err.kind(),
        ErrorKind::DisplayHelp | ErrorKind::DisplayVersion
    ) {
        let _ = err.print();
        return ExitCode::Success.code();
    }
    if json {
        // Under --json stdout carries exactly one document, so the error goes there.
        let recognised = args
            .iter()
            .filter_map(|a| a.to_str())
            .find(|a| COMMANDS.contains(a));
        let rendered = err.render().to_string();
        let message = clean_message(&rendered);
        let help = match recognised {
            Some(c) => format!("dots {c} --help"),
            None => "dots --help".to_string(),
        };
        let mut diag =
            Diagnostic::from_code("usage-error", &[("detail", &message), ("help", &help)]);
        if let Some(s) = suggestion(&rendered) {
            diag.hint = format!("Did you mean `{s}`?");
        }
        let report = Report::failure(recognised.unwrap_or(USAGE), ExitCode::Usage, vec![diag]);
        emit(
            &report,
            Mode::Json,
            &GlobalOptions {
                json: true,
                ..Default::default()
            },
        );
    } else {
        let _ = err.print();
    }
    ExitCode::Usage.code()
}

/// The first paragraph of clap's text, without the `error:` prefix, on one line.
fn clean_message(rendered: &str) -> String {
    let para = rendered.trim().split("\n\n").next().unwrap_or("");
    let para = para.strip_prefix("error:").unwrap_or(para);
    para.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The value in clap's "a similar subcommand exists: 'x'" tip, if any.
fn suggestion(rendered: &str) -> Option<String> {
    let line = rendered
        .lines()
        .find(|l| l.contains("similar") && l.contains("exists:"))?;
    let rest = line.split("exists:").nth(1)?;
    Some(rest.trim().trim_matches('\'').to_string())
}
