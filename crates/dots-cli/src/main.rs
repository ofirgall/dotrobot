//! The `dots` binary: argument parsing, dispatch and exit codes.

mod adopt;
mod app;
mod bins;
mod cli;
mod config_cmd;
mod context;
mod doctor;
mod files_phase;
mod handlers;
mod hooks_phase;
mod init;
mod inspect;
mod lint_cmd;
mod lock_cmd;
mod migrate;
mod outdated;
mod override_cmd;
mod pin;
mod providers;
mod questions;
mod reconcile;
mod restore;
mod services_phase;
mod settings_phase;
mod show;
mod status;
mod stream;
mod upgrade;
mod views;
mod why;

fn main() {
    // Interrupt exits 130; `apply` turns the first one into "stop after the current action".
    let _ = ctrlc::set_handler(reconcile::on_interrupt);
    std::process::exit(app::run(std::env::args_os().collect()));
}
