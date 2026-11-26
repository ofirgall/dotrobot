//! The `dots` binary: argument parsing, dispatch and exit codes.

mod app;
mod bins;
mod cli;
mod context;
mod doctor;
mod files_phase;
mod handlers;
mod init;
mod inspect;
mod lint_cmd;
mod outdated;
mod providers;
mod reconcile;
mod restore;
mod status;
mod views;
mod why;

fn main() {
    // Interrupt exits 130; `apply` turns the first one into "stop after the current action".
    let _ = ctrlc::set_handler(reconcile::on_interrupt);
    std::process::exit(app::run(std::env::args_os().collect()));
}
