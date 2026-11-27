//! Command tree and argument definitions. Signatures mirror the table in
//! `specs/cli-surface/spec.md` exactly (`override_usage` is that table's text).

use std::path::PathBuf;

use clap::{ArgAction, Args, CommandFactory, Parser, Subcommand, ValueEnum};
use dots_core::{ColorChoice, GlobalOptions};

#[derive(Parser, Debug)]
#[command(
    name = "dots",
    version,
    about = "Manage dotfiles, packages and machines from one repo.",
    disable_help_subcommand = true
)]
pub struct Cli {
    #[command(flatten)]
    pub global: Global,
    #[command(subcommand)]
    pub command: Option<Cmd>,
}

#[derive(Args, Debug)]
pub struct Global {
    /// Write one JSON document to stdout
    #[arg(long, global = true)]
    json: bool,
    /// More detail (-v, -vv)
    #[arg(short, long, global = true, action = ArgAction::Count)]
    verbose: u8,
    /// Errors only
    #[arg(short, long, global = true)]
    quiet: bool,
    /// When to use color
    #[arg(long, global = true, value_enum, value_name = "when")]
    color: Option<ColorArg>,
    /// Never prompt; a required confirmation fails instead
    #[arg(long, global = true)]
    no_input: bool,
    /// No network access
    #[arg(long, global = true)]
    offline: bool,
    /// Use an alternate machine.toml
    #[arg(long, global = true, value_name = "path")]
    machine_config: Option<PathBuf>,
    /// Redirect ~ and every dots directory under <dir>
    #[arg(long, global = true, value_name = "dir")]
    home: Option<PathBuf>,
}

impl Global {
    pub fn to_options(&self) -> GlobalOptions {
        GlobalOptions {
            json: self.json,
            verbose: self.verbose.min(2),
            quiet: self.quiet,
            color: self.color.map(|c| match c {
                ColorArg::Auto => ColorChoice::Auto,
                ColorArg::Always => ColorChoice::Always,
                ColorArg::Never => ColorChoice::Never,
            }),
            no_input: self.no_input,
            offline: self.offline,
            machine_config: self.machine_config.clone(),
            home: self.home.clone(),
        }
    }
}

#[derive(ValueEnum, Clone, Copy, Debug)]
enum ColorArg {
    Auto,
    Always,
    Never,
}

#[derive(Subcommand, Debug)]
pub enum Cmd {
    /// Create a dots repo, or bootstrap from a source
    #[command(override_usage = "dots init [--from <source>] [--root <path>] [--defaults]")]
    Init {
        #[arg(long, value_name = "source")]
        from: Option<String>,
        #[arg(long, value_name = "path")]
        root: Option<PathBuf>,
        #[arg(long)]
        defaults: bool,
    },
    /// Make this machine match the repo
    #[command(
        override_usage = "dots apply [<pkg|bundle>...] [--files-only|--packages-only] [--with-sudo|--sudo-only] [--yes] [--dry-run] [--allow-downgrade] [--root <path>] [--home <dir>]"
    )]
    Apply {
        #[arg(value_name = "pkg|bundle")]
        targets: Vec<String>,
        #[arg(long, conflicts_with = "packages_only")]
        files_only: bool,
        #[arg(long)]
        packages_only: bool,
        #[arg(long)]
        yes: bool,
        #[arg(long)]
        dry_run: bool,
        #[arg(long)]
        allow_downgrade: bool,
        #[arg(long, value_name = "path")]
        root: Option<PathBuf>,
        /// Also run the steps that need sudo, asking for the password once
        #[arg(long, conflicts_with = "sudo_only")]
        with_sudo: bool,
        /// Run only the steps that need sudo (and their hooks), asking for the password once
        #[arg(long)]
        sudo_only: bool,
    },
    /// Show what apply would do
    #[command(
        override_usage = "dots plan [<pkg|bundle>...] [--files-only|--packages-only] [--check]"
    )]
    Plan {
        #[arg(value_name = "pkg|bundle")]
        targets: Vec<String>,
        #[arg(long, conflicts_with = "packages_only")]
        files_only: bool,
        #[arg(long)]
        packages_only: bool,
        #[arg(long)]
        check: bool,
    },
    /// Show what is healthy and what needs attention
    #[command(override_usage = "dots status [<pkg>] [--all] [--strict]")]
    Status {
        pkg: Option<String>,
        #[arg(long)]
        all: bool,
        #[arg(long)]
        strict: bool,
    },
    /// List packages with newer versions available
    #[command(override_usage = "dots outdated [<pkg>...] [--refresh]")]
    Outdated {
        pkgs: Vec<String>,
        #[arg(long)]
        refresh: bool,
    },
    /// Explain why a package is on this machine
    #[command(override_usage = "dots why <pkg>")]
    Why { pkg: String },
    /// Say which package provides an executable
    #[command(override_usage = "dots which <bin>")]
    Which { bin: String },
    /// List executables and who owns them
    #[command(
        override_usage = "dots bins [<pkg|provider>] [--managed|--unmanaged|--unknown|--shadowed]"
    )]
    Bins {
        #[arg(value_name = "pkg|provider")]
        target: Option<String>,
        #[arg(long, group = "bins_filter")]
        managed: bool,
        #[arg(long, group = "bins_filter")]
        unmanaged: bool,
        #[arg(long, group = "bins_filter")]
        unknown: bool,
        #[arg(long, group = "bins_filter")]
        shadowed: bool,
    },
    /// Check the dots setup itself
    #[command(override_usage = "dots doctor")]
    Doctor,
    /// Show what dots detected about this machine
    #[command(override_usage = "dots facts")]
    Facts,
    /// Upgrade packages
    #[command(override_usage = "dots upgrade [<pkg>...|--all] [--provider <id>] [--commit]")]
    Upgrade {
        #[arg(conflicts_with = "all")]
        pkgs: Vec<String>,
        #[arg(long)]
        all: bool,
        #[arg(long, value_name = "id")]
        provider: Option<String>,
        #[arg(long)]
        commit: bool,
    },
    /// Pin a package to a version
    #[command(override_usage = "dots pin <pkg> [<version>] [--commit]")]
    Pin {
        pkg: String,
        version: Option<String>,
        #[arg(long)]
        commit: bool,
    },
    /// Remove a pin
    #[command(override_usage = "dots unpin <pkg> [--commit]")]
    Unpin {
        pkg: String,
        #[arg(long)]
        commit: bool,
    },
    /// Remove a package or an entry from the repo
    #[command(
        override_usage = "dots remove <pkg> [--commit]\n       dots remove bundle|pkg <item> [--commit]"
    )]
    Remove {
        #[arg(num_args = 1..=2, required = true, value_name = "pkg | bundle|pkg <item>")]
        target: Vec<String>,
        #[arg(long)]
        commit: bool,
    },
    /// Move a package to another provider
    #[command(override_usage = "dots migrate <pkg> --to <provider> [--from <provider>] [--yes]")]
    Migrate {
        pkg: String,
        #[arg(long, required = true, value_name = "provider")]
        to: String,
        /// Remove this provider's copy (also when the package no longer lists it)
        #[arg(long, value_name = "provider")]
        from: Option<String>,
        /// Do not ask before removing the `--from` copy
        #[arg(long)]
        yes: bool,
    },
    /// Add a bundle or package from a source
    #[command(
        override_usage = "dots add bundle|pkg <source>:<name> [--copy] [--as <name>] [--no-deps] [--dry-run] [--yes] [--exclude <pkg>...] [--commit]\n       dots add bundle|pkg <name> [--copy] [--as <name>] [--no-deps] [--dry-run] [--yes] [--exclude <pkg>...] [--commit]"
    )]
    Add {
        #[arg(num_args = 2, required = true, value_names = ["bundle|pkg", "source:name | name"])]
        target: Vec<String>,
        #[arg(long)]
        copy: bool,
        #[arg(long = "as", value_name = "name")]
        as_name: Option<String>,
        #[arg(long)]
        no_deps: bool,
        #[arg(long)]
        dry_run: bool,
        #[arg(long)]
        yes: bool,
        #[arg(long, num_args = 1.., value_name = "pkg")]
        exclude: Vec<String>,
        #[arg(long)]
        commit: bool,
    },
    /// Search a source for bundles and packages
    #[command(override_usage = "dots search <source> [<query>]")]
    Search {
        source: String,
        query: Option<String>,
    },
    /// List configured sources
    #[command(override_usage = "dots sources")]
    Sources,
    /// Update followed sources
    #[command(override_usage = "dots pull [<source|item>] [--dry-run] [--to <rev>] [--commit]")]
    Pull {
        #[arg(value_name = "source|item")]
        target: Option<String>,
        #[arg(long)]
        dry_run: bool,
        #[arg(long, value_name = "rev")]
        to: Option<String>,
        #[arg(long)]
        commit: bool,
    },
    /// Take ownership of a followed item
    #[command(override_usage = "dots eject <address> [--commit]")]
    Eject {
        address: String,
        #[arg(long)]
        commit: bool,
    },
    /// Follow the source again
    #[command(override_usage = "dots uneject <address> [--commit]")]
    Uneject {
        address: String,
        #[arg(long)]
        commit: bool,
    },
    /// List and complete manual actions
    #[command(override_usage = "dots todo [show <id>|done <id> [--note <text>]|skip <id>|check]")]
    Todo {
        #[command(subcommand)]
        action: Option<TodoAction>,
    },
    /// Bring a changed file back into the repo
    #[command(override_usage = "dots adopt <dst> [--layer <layer>] [--as-patch] [--machine]")]
    Adopt {
        dst: String,
        #[arg(long, value_name = "layer")]
        layer: Option<String>,
        #[arg(long)]
        as_patch: bool,
        #[arg(long)]
        machine: bool,
    },
    /// Try a package or branch in a scratch worktree
    #[command(override_usage = "dots try <name> [--from <ref>]")]
    Try {
        name: String,
        #[arg(long, value_name = "ref")]
        from: Option<String>,
    },
    /// Switch the active root
    #[command(override_usage = "dots use <name|path|->")]
    Use {
        #[arg(value_name = "name|path|-", allow_hyphen_values = true)]
        root: String,
    },
    /// List known roots
    #[command(override_usage = "dots roots")]
    Roots,
    /// Compare two roots
    #[command(override_usage = "dots diff <a>..<b>")]
    Diff {
        #[arg(value_name = "a>..<b")]
        range: String,
    },
    /// Show the history of changes
    #[command(override_usage = "dots log [<pkg>] [--since <duration>] [--host <name>]")]
    Log {
        pkg: Option<String>,
        #[arg(long, value_name = "duration")]
        since: Option<String>,
        #[arg(long, value_name = "name")]
        host: Option<String>,
    },
    /// Recompute checksums and report drift
    #[command(override_usage = "dots verify [<item>]")]
    Verify { item: Option<String> },
    /// Put back files replaced by a run
    #[command(override_usage = "dots restore <run-id>")]
    Restore { run_id: String },
    /// Read and change package settings
    #[command(
        override_usage = "dots config list | get <pkg>.<key> | set <pkg>.<key> <value> [--repo [--commit]|--machine] | unset <pkg>.<key> | explain <pkg>.<key>"
    )]
    Config {
        #[command(subcommand)]
        action: ConfigAction,
    },
    /// List unanswered settings as structured questions
    #[command(override_usage = "dots questions")]
    Questions,
    /// Show a package, bundle or file
    #[command(override_usage = "dots show <address> [--effective]")]
    Show {
        address: String,
        #[arg(long)]
        effective: bool,
    },
    /// Manage overrides
    #[command(override_usage = "dots override list | edit <address> [--machine] | diff <address>")]
    Override {
        #[command(subcommand)]
        action: OverrideAction,
    },
    /// Install the dots skill for coding agents
    #[command(override_usage = "dots skill install [--agent <id>...]")]
    Skill {
        #[command(subcommand)]
        action: SkillAction,
    },
    /// Check the repo for mistakes
    #[command(override_usage = "dots lint")]
    Lint,
    /// Repair dots.lock
    #[command(override_usage = "dots lock --fix")]
    Lock {
        #[arg(long, required = true)]
        fix: bool,
    },
    /// Import an existing dotfiles setup
    #[command(override_usage = "dots import dotbot <file>... | scan <dir>")]
    Import {
        #[command(subcommand)]
        action: ImportAction,
    },
    /// Print a shell completion script
    #[command(override_usage = "dots completions <bash|zsh|fish|pwsh>")]
    Completions {
        #[arg(value_enum)]
        shell: Shell,
    },
}

impl Cmd {
    /// The top-level command name, as in `dots/<name>@1`.
    pub fn name(&self) -> &'static str {
        match self {
            Cmd::Init { .. } => "init",
            Cmd::Apply { .. } => "apply",
            Cmd::Plan { .. } => "plan",
            Cmd::Status { .. } => "status",
            Cmd::Outdated { .. } => "outdated",
            Cmd::Why { .. } => "why",
            Cmd::Which { .. } => "which",
            Cmd::Bins { .. } => "bins",
            Cmd::Doctor => "doctor",
            Cmd::Facts => "facts",
            Cmd::Upgrade { .. } => "upgrade",
            Cmd::Pin { .. } => "pin",
            Cmd::Unpin { .. } => "unpin",
            Cmd::Remove { .. } => "remove",
            Cmd::Migrate { .. } => "migrate",
            Cmd::Add { .. } => "add",
            Cmd::Search { .. } => "search",
            Cmd::Sources => "sources",
            Cmd::Pull { .. } => "pull",
            Cmd::Eject { .. } => "eject",
            Cmd::Uneject { .. } => "uneject",
            Cmd::Todo { .. } => "todo",
            Cmd::Adopt { .. } => "adopt",
            Cmd::Try { .. } => "try",
            Cmd::Use { .. } => "use",
            Cmd::Roots => "roots",
            Cmd::Diff { .. } => "diff",
            Cmd::Log { .. } => "log",
            Cmd::Verify { .. } => "verify",
            Cmd::Restore { .. } => "restore",
            Cmd::Config { .. } => "config",
            Cmd::Questions => "questions",
            Cmd::Show { .. } => "show",
            Cmd::Override { .. } => "override",
            Cmd::Skill { .. } => "skill",
            Cmd::Lint => "lint",
            Cmd::Lock { .. } => "lock",
            Cmd::Import { .. } => "import",
            Cmd::Completions { .. } => "completions",
        }
    }
}

/// Command groups for `dots --help`, by purpose.
const GROUPS: &[(&str, &[&str])] = &[
    (
        "Set up and change this machine",
        &[
            "init", "apply", "plan", "upgrade", "pin", "unpin", "remove", "migrate",
        ],
    ),
    (
        "Inspect",
        &[
            "status", "outdated", "why", "which", "bins", "doctor", "facts", "show", "log",
            "verify", "diff",
        ],
    ),
    (
        "Share and reuse",
        &["add", "search", "sources", "pull", "eject", "uneject"],
    ),
    (
        "Files and settings",
        &[
            "adopt",
            "restore",
            "config",
            "questions",
            "override",
            "lint",
            "lock",
        ],
    ),
    ("Tasks and roots", &["todo", "try", "use", "roots"]),
    ("Other", &["import", "skill", "completions"]),
];

/// The clap command, with subcommands listed by group in `--help`.
pub fn command() -> clap::Command {
    let mut cmd = Cli::command().color(clap::ColorChoice::Never);
    let mut listing = String::new();
    for (title, names) in GROUPS {
        listing.push_str(&format!("{title}:\n"));
        for name in *names {
            let about = cmd
                .find_subcommand(name)
                .and_then(|s| s.get_about())
                .map(|a| a.to_string())
                .unwrap_or_default();
            listing.push_str(&format!("  {name:<12} {about}\n"));
        }
        listing.push('\n');
    }
    let names: Vec<String> = cmd
        .get_subcommands()
        .map(|s| s.get_name().to_string())
        .collect();
    for n in names {
        cmd = cmd.mut_subcommand(n, |s| s.hide(true));
    }
    cmd.help_template(format!(
        "{{about}}\n\nUsage: {{usage}}\n\n{listing}{{all-args}}\n\nRun `dots <command> --help` for a command's arguments."
    ))
    .override_usage("dots [OPTIONS] [COMMAND]")
}

#[derive(Subcommand, Debug)]
pub enum TodoAction {
    /// Show one manual action
    Show { id: String },
    /// Mark a manual action done
    Done {
        id: String,
        #[arg(long, value_name = "text")]
        note: Option<String>,
    },
    /// Skip a manual action
    Skip { id: String },
    /// Re-run the automatic checks
    Check,
}

#[derive(Subcommand, Debug)]
pub enum ConfigAction {
    /// List all settings
    List,
    /// Print one setting
    Get {
        #[arg(value_name = "pkg>.<key")]
        key: String,
    },
    /// Set a setting
    Set {
        #[arg(value_name = "pkg>.<key")]
        key: String,
        value: String,
        #[arg(long, conflicts_with = "machine")]
        repo: bool,
        #[arg(long, requires = "repo")]
        commit: bool,
        #[arg(long)]
        machine: bool,
    },
    /// Remove a setting value
    Unset {
        #[arg(value_name = "pkg>.<key")]
        key: String,
    },
    /// Explain where a setting's value comes from
    Explain {
        #[arg(value_name = "pkg>.<key")]
        key: String,
    },
}

#[derive(Subcommand, Debug)]
pub enum OverrideAction {
    /// List overrides
    List,
    /// Edit an override
    Edit {
        address: String,
        #[arg(long)]
        machine: bool,
    },
    /// Show an override's effect
    Diff { address: String },
}

#[derive(Subcommand, Debug)]
pub enum SkillAction {
    /// Install the skill
    Install {
        #[arg(long, num_args = 1.., value_name = "id")]
        agent: Vec<String>,
    },
}

#[derive(Subcommand, Debug)]
pub enum ImportAction {
    /// Import dotbot configs
    Dotbot {
        #[arg(required = true, value_name = "file")]
        files: Vec<PathBuf>,
    },
    /// Scan a directory for dotfiles
    Scan { dir: PathBuf },
}

#[derive(ValueEnum, Clone, Copy, Debug)]
pub enum Shell {
    Bash,
    Zsh,
    Fish,
    Pwsh,
}
