//! The words of the live progress: verbs and nouns instead of command lines. A step has a
//! running form (`Installing plantuml, deno (brew)`) and a finished form (`Installed
//! plantuml, deno`). Pure functions, so the wording is tested without a terminal.

use dots_engine::Batch;
use dots_providers::OpKind;

/// How many package names a line shows before `+N more`.
const MAX_NAMES: usize = 6;

/// A verb in its two forms.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Verb {
    /// `Installing`
    pub ing: String,
    /// `Installed`
    pub ed: String,
}

impl Verb {
    fn new(ing: &str, ed: &str) -> Verb {
        Verb {
            ing: ing.into(),
            ed: ed.into(),
        }
    }
}

/// `a, b, c` or `a, b, c, d, e, f +4 more`.
pub fn names(packages: &[String]) -> String {
    if packages.len() <= MAX_NAMES {
        packages.join(", ")
    } else {
        format!(
            "{} +{} more",
            packages[..MAX_NAMES].join(", "),
            packages.len() - MAX_NAMES
        )
    }
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    c.next()
        .map(|f| f.to_uppercase().collect::<String>() + c.as_str())
        .unwrap_or_default()
}

/// Several verbs as one phrase: `Installing and upgrading`, `Installed and upgraded`.
fn join(forms: Vec<String>) -> String {
    let mut out = String::new();
    for (i, f) in forms.iter().enumerate() {
        if i == 0 {
            out.push_str(f);
        } else {
            out.push_str(if i + 1 == forms.len() { " and " } else { ", " });
            out.push_str(&f.to_lowercase());
        }
    }
    out
}

fn op_verb(k: OpKind) -> Verb {
    match k {
        OpKind::Install => Verb::new("Installing", "Installed"),
        OpKind::Upgrade => Verb::new("Upgrading", "Upgraded"),
        OpKind::Remove => Verb::new("Removing", "Removed"),
        OpKind::Pin => Verb::new("Pinning", "Pinned"),
        OpKind::Unpin => Verb::new("Unpinning", "Unpinned"),
    }
}

/// What a batch does, as verbs (a cargo batch may install some and upgrade others).
fn batch_verbs(b: &Batch) -> Verb {
    let mut kinds: Vec<OpKind> = Vec::new();
    for a in b.actions.iter().filter(|a| !a.ops.is_empty()) {
        if !kinds.contains(&a.kind) {
            kinds.push(a.kind);
        }
    }
    if kinds.is_empty() {
        kinds.push(OpKind::Install);
    }
    let verbs: Vec<Verb> = kinds.into_iter().map(op_verb).collect();
    Verb {
        ing: join(verbs.iter().map(|v| v.ing.clone()).collect()),
        ed: join(verbs.iter().map(|v| v.ed.clone()).collect()),
    }
}

/// `Installing plantuml, deno, zsh (brew)`
pub fn batch_running(b: &Batch) -> String {
    format!(
        "{} {} ({})",
        batch_verbs(b).ing,
        names(&b.packages),
        b.provider
    )
}

/// `Installed plantuml, deno, zsh`
pub fn batch_done(b: &Batch, packages: &[String]) -> String {
    format!("{} {}", batch_verbs(b).ed, names(packages))
}

/// The running and finished verbs for a file step kind.
fn file_verb(kind: &str) -> Verb {
    match kind {
        "link" => Verb::new("Linking", "Linked"),
        "unlink" => Verb::new("Unlinking", "Unlinked"),
        "copy" => Verb::new("Copying", "Copied"),
        "render" => Verb::new("Rendering", "Rendered"),
        "inject" => Verb::new("Updating", "Updated"),
        "backup" => Verb::new("Backing up", "Backed up"),
        "remove" => Verb::new("Removing", "Removed"),
        other => Verb::new(&capitalize(other), &capitalize(other)),
    }
}

/// `Linking ~/.zshrc` / `Linked ~/.zshrc`
pub fn file(kind: &str, dst: &str, ok: bool) -> String {
    let v = file_verb(kind);
    format!("{} {dst}", if ok { v.ed } else { v.ing })
}

fn setting_verb(action: &str) -> Verb {
    match action {
        "set" => Verb::new("Setting", "Set"),
        "restore" => Verb::new("Restoring", "Restored"),
        "delete" => Verb::new("Clearing", "Cleared"),
        other => Verb::new(&capitalize(other), &capitalize(other)),
    }
}

/// `Set NSGlobalDomain KeyRepeat`
pub fn setting(action: &str, label: &str, ok: bool) -> String {
    let v = setting_verb(action);
    format!("{} {label}", if ok { v.ed } else { v.ing })
}

/// `Applying 3 settings`: the live note of a phase that reports its steps one by one.
pub fn phase_note(what: &str, n: usize) -> String {
    format!("Applying {n} {what}{}", if n == 1 { "" } else { "s" })
}

fn service_verb(action: &str) -> Verb {
    let one = |a: &str| match a {
        "enable" => Verb::new("Enabling", "Enabled"),
        "disable" => Verb::new("Disabling", "Disabled"),
        "start" => Verb::new("Starting", "Started"),
        "stop" => Verb::new("Stopping", "Stopped"),
        "restart" => Verb::new("Restarting", "Restarted"),
        other => Verb::new(&capitalize(other), &capitalize(other)),
    };
    let parts: Vec<Verb> = action.split('+').map(one).collect();
    Verb {
        ing: join(parts.iter().map(|v| v.ing.clone()).collect()),
        ed: join(parts.iter().map(|v| v.ed.clone()).collect()),
    }
}

/// `Enabled and started service sshd`
pub fn service(action: &str, name: &str, ok: bool) -> String {
    let v = service_verb(action);
    format!("{} service {name}", if ok { v.ed } else { v.ing })
}

/// `Creating shim ~/.local/bin/fd` / `Created shim ...`
pub fn shim(action: &str, dst: &str, ok: bool) -> String {
    let (ing, ed) = match action {
        "create" => ("Creating", "Created"),
        "update" => ("Repairing", "Repaired"),
        "remove" => ("Removing", "Removed"),
        _ => ("Updating", "Updated"),
    };
    format!("{} shim {dst}", if ok { ed } else { ing })
}

/// `Running post_install hook of kv` / `Ran post_install hook of kv`
pub fn hook(event: &str, package: &str, done: bool) -> String {
    format!(
        "{} {event} hook of {package}",
        if done { "Ran" } else { "Running" }
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use dots_providers::{Action, Op, Spec};

    fn batch(provider: &str, pkgs: &[&str], kinds: &[OpKind]) -> Batch {
        Batch {
            id: 0,
            provider: provider.into(),
            instance: None,
            packages: pkgs.iter().map(|s| s.to_string()).collect(),
            actions: kinds
                .iter()
                .map(|k| Action {
                    kind: *k,
                    ops: vec![Op {
                        kind: *k,
                        spec: Spec::new("x"),
                        version: None,
                    }],
                    argv: vec![],
                    needs_root: false,
                })
                .collect(),
            deps: vec![],
            interactive: false,
            sudo: false,
        }
    }

    #[test]
    fn package_batches_read_as_sentences() {
        let b = batch("brew", &["plantuml", "deno", "zsh"], &[OpKind::Install]);
        assert_eq!(batch_running(&b), "Installing plantuml, deno, zsh (brew)");
        assert_eq!(batch_done(&b, &b.packages), "Installed plantuml, deno, zsh");
        let b = batch("cargo", &["kv"], &[OpKind::Upgrade]);
        assert_eq!(batch_running(&b), "Upgrading kv (cargo)");
        let b = batch("brew", &["old"], &[OpKind::Remove]);
        assert_eq!(batch_done(&b, &b.packages), "Removed old");
    }

    #[test]
    fn a_mixed_batch_names_both_verbs() {
        let b = batch("cargo", &["a", "b"], &[OpKind::Install, OpKind::Upgrade]);
        assert_eq!(batch_running(&b), "Installing and upgrading a, b (cargo)");
        assert_eq!(batch_done(&b, &b.packages), "Installed and upgraded a, b");
    }

    #[test]
    fn long_name_lists_are_cut() {
        let pkgs: Vec<String> = (1..=9).map(|n| format!("p{n}")).collect();
        assert_eq!(names(&pkgs), "p1, p2, p3, p4, p5, p6 +3 more");
    }

    #[test]
    fn files_settings_services_shims_and_hooks() {
        assert_eq!(file("link", "~/.zshrc", false), "Linking ~/.zshrc");
        assert_eq!(file("link", "~/.zshrc", true), "Linked ~/.zshrc");
        assert_eq!(
            file("render", "~/.gitconfig", true),
            "Rendered ~/.gitconfig"
        );
        assert_eq!(file("weird", "~/x", true), "Weird ~/x");
        assert_eq!(
            setting("set", "NSGlobalDomain KeyRepeat", true),
            "Set NSGlobalDomain KeyRepeat"
        );
        assert_eq!(setting("restore", "a b", false), "Restoring a b");
        assert_eq!(phase_note("setting", 3), "Applying 3 settings");
        assert_eq!(phase_note("service change", 1), "Applying 1 service change");
        assert_eq!(
            service("enable+start", "sshd", true),
            "Enabled and started service sshd"
        );
        assert_eq!(service("restart", "sshd", false), "Restarting service sshd");
        assert_eq!(
            shim("create", "~/.local/bin/fd", true),
            "Created shim ~/.local/bin/fd"
        );
        assert_eq!(
            hook("post_install", "kv", false),
            "Running post_install hook of kv"
        );
        assert_eq!(
            hook("post_install", "kv", true),
            "Ran post_install hook of kv"
        );
    }
}
