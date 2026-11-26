//! `dots init`: create a new repo, or clone one with `--from`, then write `machine.toml`.
//!
//! Every recommended answer comes from the repo's data (`recommended` markers in `dots.toml`
//! and bundles, setting defaults); nothing is hard-coded here. Questions go through the
//! [`Prompter`] seam: without a TTY, with `--defaults`, `--no-input` or `--json` there is no
//! prompter and the recommended answers are taken, failing only for `required` settings that
//! have no value anywhere.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use dots_core::blocks::{Block, Tone};
use dots_core::report::Diagnostic;
use dots_core::{ExitCode, Report};
use dots_facts::{Chain, FactOverrides, RealSources, detect};
use dots_model::{MachineConfig, Recommended, Repo, TagDecl, TagRecommended};
use serde_json::{Value, json};

use crate::app::Context;
use crate::context::platform_parents;

pub struct Args {
    pub from: Option<String>,
    pub root: Option<PathBuf>,
    pub defaults: bool,
}

// ------------------------------------------------------------------ prompter

/// The interactive seam. Tests drive it with a script; the binary uses [`StdioPrompter`].
pub trait Prompter {
    fn confirm(&mut self, question: &str, default: bool) -> bool;
    /// Pick any of `options`; `default` is what Enter accepts.
    fn multi(&mut self, question: &str, options: &[String], default: &[String]) -> Vec<String>;
    /// Free text; an empty answer returns `default` (or an empty string).
    fn input(&mut self, question: &str, default: Option<&str>) -> String;
}

/// Line-based prompts on stderr/stdin, recommended answer as the default.
pub struct StdioPrompter;

impl StdioPrompter {
    fn ask(&self, text: &str) -> String {
        eprint!("{text}");
        let _ = std::io::stderr().flush();
        let mut line = String::new();
        let _ = std::io::stdin().lock().read_line(&mut line);
        line.trim().to_string()
    }
}

impl Prompter for StdioPrompter {
    fn confirm(&mut self, question: &str, default: bool) -> bool {
        let hint = if default { "Y/n" } else { "y/N" };
        match self
            .ask(&format!("{question} [{hint}] "))
            .to_lowercase()
            .as_str()
        {
            "y" | "yes" => true,
            "n" | "no" => false,
            _ => default,
        }
    }

    fn multi(&mut self, question: &str, options: &[String], default: &[String]) -> Vec<String> {
        eprintln!("{question}");
        for o in options {
            let mark = if default.contains(o) { "*" } else { " " };
            eprintln!("  {mark} {o}");
        }
        let a = self.ask("Comma-separated names, Enter for the starred ones, `-` for none: ");
        match a.as_str() {
            "" => default.to_vec(),
            "-" => Vec::new(),
            _ => a
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| options.contains(s))
                .collect(),
        }
    }

    fn input(&mut self, question: &str, default: Option<&str>) -> String {
        let shown = default.map(|d| format!(" [{d}]")).unwrap_or_default();
        let a = self.ask(&format!("{question}{shown}: "));
        if a.is_empty() {
            default.unwrap_or_default().to_string()
        } else {
            a
        }
    }
}

// --------------------------------------------------------------------- entry

pub fn run(ctx: &Context, args: &Args) -> Report {
    let interactive = !args.defaults
        && !ctx.opts.no_input
        && !ctx.opts.json
        && std::io::stdin().is_terminal()
        && std::io::stderr().is_terminal();
    let mut stdio = StdioPrompter;
    let prompter: Option<&mut dyn Prompter> = if interactive { Some(&mut stdio) } else { None };
    run_with(ctx, args, prompter)
}

fn fail(errors: Vec<Diagnostic>) -> Report {
    Report::failure("init", ExitCode::Error, errors)
}

fn err(code: &str, args: &[(&str, &str)]) -> Report {
    fail(vec![Diagnostic::from_code(code, args)])
}

pub fn run_with(ctx: &Context, args: &Args, prompter: Option<&mut dyn Prompter>) -> Report {
    let machine_path = ctx.dirs.machine_toml(ctx.opts.machine_config.as_deref());
    if machine_path.exists() {
        return err(
            "init-machine-exists",
            &[("path", &machine_path.display().to_string())],
        );
    }
    let default_root = ctx.dirs.home.join("dotfiles");
    let root = match &args.root {
        Some(r) if r.is_absolute() => r.clone(),
        Some(r) => std::env::current_dir().unwrap_or_default().join(r),
        None => default_root.clone(),
    };
    let facts = detect(&RealSources, &FactOverrides::default());

    // 1. The repo: clone, adopt what is there, or create.
    let mut created: Vec<String> = Vec::new();
    let mut cloned_from: Option<String> = None;
    if let Some(source) = &args.from {
        if dir_not_empty(&root) {
            return err("init-root-exists", &[("path", &root.display().to_string())]);
        }
        if let Err(detail) = clone(source, &root, ctx.opts.offline) {
            return err("clone-failed", &[("source", source), ("detail", &detail)]);
        }
        cloned_from = Some(source.clone());
    } else if !root.join("dots.toml").is_file() {
        match create_repo(&root) {
            Ok(files) => created = files,
            Err(e) => return *e,
        }
    }

    // 2. Load it strictly, and the profile that matches this host.
    let loaded = match Repo::load(&root) {
        Ok(l) => l,
        Err(e) => {
            let mut r = fail(e.errors);
            r.warnings = e.warnings;
            return r;
        }
    };
    let mut warnings = loaded.warnings;
    let repo = loaded.value;
    let profile = match MachineConfig::load(&root, &machine_path, &facts.hostname) {
        Ok(l) => {
            warnings.extend(l.warnings);
            l.value
        }
        Err(e) => {
            let mut r = fail(e.errors);
            r.warnings = e.warnings;
            return r;
        }
    };
    // The effective model: platform tables and overrides applied, like every other command.
    let repo = {
        let m = crate::context::MachineCtx {
            root: root.clone(),
            machine: profile.clone(),
            facts: facts.clone(),
            warnings: Vec::new(),
        };
        let chain_of = |man: &dots_model::RepoManifest| {
            let parents: BTreeMap<String, String> = man
                .platforms
                .iter()
                .filter_map(|(k, d)| Some((k.clone(), d.parent.clone()?)))
                .collect();
            Chain::build(&m.facts, &parents, &m.machine.platform_extra).ordered()
        };
        let locations = crate::context::override_locations(&m, ctx);
        match repo.effective(&chain_of, &locations) {
            Ok(e) => {
                warnings.extend(e.warnings);
                e.value
            }
            Err(e) => {
                let mut r = fail(e.errors);
                r.warnings = e.warnings;
                return r;
            }
        }
    };
    let chain = Chain::build(&facts, &platform_parents(&repo), &[]);

    // 3. Answers.
    let answers = match answer(&repo, &chain, &profile, prompter) {
        Ok(a) => a,
        Err(errors) => {
            let mut r = fail(errors);
            r.warnings = warnings;
            return r;
        }
    };

    // 4. machine.toml.
    let root_line = (root != default_root).then(|| tilde(&root, &ctx.dirs.home));
    let text = machine_toml(root_line.as_deref(), profile.profile.as_deref(), &answers);
    if let Some(parent) = machine_path.parent()
        && let Err(e) = std::fs::create_dir_all(parent)
    {
        return write_failed(parent, &e);
    }
    if let Err(e) =
        dots_config::write_atomic(&machine_path, &text, is_secret(&repo, &answers.settings))
    {
        return write_failed(&machine_path, &std::io::Error::other(e));
    }

    Report::ok(
        "init",
        json!({
            "root": root.display().to_string(),
            "created": !created.is_empty(),
            "files": created,
            "cloned_from": cloned_from,
            "machine_config": machine_path.display().to_string(),
            "profile": profile.profile,
            "bundles": answers.bundles,
            "tags": answers.tags,
            "settings": redact_answers(&repo, &answers.settings),
        }),
    )
    .with_report(
        &root,
        &machine_path,
        &profile,
        &answers,
        cloned_from.is_some(),
        warnings,
    )
}

fn secret_decl(repo: &Repo, pkg: &str, key: &str) -> bool {
    repo.packages
        .get(pkg)
        .and_then(|p| p.settings.get(key))
        .is_some_and(|d| d.secret)
}

fn is_secret(repo: &Repo, answers: &BTreeMap<String, BTreeMap<String, Value>>) -> bool {
    answers
        .iter()
        .any(|(p, kv)| kv.keys().any(|k| secret_decl(repo, p, k)))
}

/// The answers for the JSON report: a secret's value is never printed (`null`).
fn redact_answers(
    repo: &Repo,
    answers: &BTreeMap<String, BTreeMap<String, Value>>,
) -> BTreeMap<String, BTreeMap<String, Value>> {
    answers
        .iter()
        .map(|(p, kv)| {
            let kv = kv
                .iter()
                .map(|(k, v)| {
                    let v = if secret_decl(repo, p, k) {
                        Value::Null
                    } else {
                        v.clone()
                    };
                    (k.clone(), v)
                })
                .collect();
            (p.clone(), kv)
        })
        .collect()
}

fn write_failed(path: &Path, e: &std::io::Error) -> Report {
    err(
        "write-failed",
        &[
            ("path", &path.display().to_string()),
            ("detail", &e.to_string()),
        ],
    )
}

trait InitReport {
    fn with_report(
        self,
        root: &Path,
        machine: &Path,
        profile: &MachineConfig,
        a: &Answers,
        cloned: bool,
        warnings: Vec<Diagnostic>,
    ) -> Report;
}

impl InitReport for Report {
    fn with_report(
        mut self,
        root: &Path,
        machine: &Path,
        profile: &MachineConfig,
        a: &Answers,
        cloned: bool,
        warnings: Vec<Diagnostic>,
    ) -> Report {
        let list = |v: &[String]| {
            if v.is_empty() {
                "(none)".to_string()
            } else {
                v.join(", ")
            }
        };
        let verb = if cloned { "Cloned" } else { "Ready" };
        let text = format!("{verb}: repo at {}", root.display());
        self.headline = Some(text.clone());
        self.blocks.push(Block::headline(Tone::Ok, text));
        self.blocks.push(Block::facts([
            ("machine.toml", machine.display().to_string()),
            (
                "profile",
                profile.profile.clone().unwrap_or_else(|| "-".to_string()),
            ),
            ("bundles", list(&a.bundles)),
            ("tags", list(&a.tags)),
        ]));
        self.blocks.push(Block::hint("dots apply --dry-run"));
        self.warnings = warnings;
        self
    }
}

// ------------------------------------------------------------------ the repo

fn dir_not_empty(p: &Path) -> bool {
    std::fs::read_dir(p).is_ok_and(|mut d| d.next().is_some())
}

/// `dots.toml` plus empty `bundles/` and `packages/` (kept in git by a `.gitkeep`).
fn create_repo(root: &Path) -> Result<Vec<String>, Box<Report>> {
    let name = root
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("dotfiles");
    let manifest = format!(
        "schema = 1\nname = {}\n\n# Priority of providers per platform, tags, and recommended\n\
         # markers go here. See `dots lint` after editing.\n",
        quote(name)
    );
    let files = [
        ("dots.toml", manifest),
        ("bundles/.gitkeep", String::new()),
        ("packages/.gitkeep", String::new()),
    ];
    let mut made = Vec::new();
    for (rel, text) in files {
        let path = root.join(rel);
        if path.exists() {
            continue;
        }
        let r = path
            .parent()
            .map_or(Ok(()), std::fs::create_dir_all)
            .and_then(|()| std::fs::write(&path, text));
        if let Err(e) = r {
            return Err(Box::new(write_failed(&path, &e)));
        }
        made.push(rel.to_string());
    }
    Ok(made)
}

/// Where `--from` points: a local path or URL as is, `owner/repo` as a GitHub repo.
fn resolve_source(source: &str) -> String {
    let shorthand = source.split_once('/').is_some_and(|(o, r)| {
        let ok = |s: &str| {
            !s.is_empty()
                && s.chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        };
        ok(o) && ok(r)
    });
    if shorthand && !Path::new(source).exists() {
        format!("https://github.com/{source}.git")
    } else {
        source.to_string()
    }
}

fn clone(source: &str, root: &Path, offline: bool) -> Result<(), String> {
    let url = resolve_source(source);
    let local = Path::new(&url).exists();
    if offline && !local {
        return Err("`--offline` is set and the source is remote".to_string());
    }
    if let Some(parent) = root.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let out = Command::new("git")
        .args(["clone", "--quiet", "--", &url])
        .arg(root)
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("cannot run git: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        let text = String::from_utf8_lossy(&out.stderr);
        let line = text
            .lines()
            .rev()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("git clone failed");
        Err(line.trim().to_string())
    }
}

// ------------------------------------------------------------------- answers

#[derive(Debug, Default, PartialEq)]
pub struct Answers {
    /// Written to machine.toml (empty when the profile already answers).
    pub bundles: Vec<String>,
    pub tags: Vec<String>,
    pub settings: BTreeMap<String, BTreeMap<String, Value>>,
}

/// What the repo recommends on this platform.
pub struct Recommendations {
    pub bundles: Vec<String>,
    pub tags: Vec<String>,
}

pub fn recommendations(repo: &Repo, chain: &Chain) -> Recommendations {
    let ordered = chain.ordered();
    let mut bundles: BTreeSet<String> = BTreeSet::new();
    if let Some(Recommended::Bundles(list)) = &repo.manifest.recommended {
        bundles.extend(
            list.iter()
                .filter(|b| repo.bundles.contains_key(*b))
                .cloned(),
        );
    }
    for (name, b) in &repo.bundles {
        let on = ordered
            .iter()
            .rev()
            .find_map(|k| b.platform.get(k)?.get("recommended")?.as_bool())
            .unwrap_or(b.recommended);
        if on {
            bundles.insert(name.clone());
        }
    }
    let tags = repo
        .manifest
        .tags
        .iter()
        .filter(|(_, d)| match d {
            TagDecl::Full {
                recommended: Some(TagRecommended::Flag(b)),
                ..
            } => *b,
            TagDecl::Full {
                recommended: Some(TagRecommended::PerPlatform(m)),
                ..
            } => ordered
                .iter()
                .rev()
                .find_map(|k| m.get(k))
                .copied()
                .unwrap_or(false),
            _ => false,
        })
        .map(|(n, _)| n.clone())
        .collect();
    Recommendations {
        bundles: bundles.into_iter().collect(),
        tags,
    }
}

/// Decide bundles, tags and settings. `prompter` is `None` for non-interactive runs.
pub fn answer(
    repo: &Repo,
    chain: &Chain,
    profile: &MachineConfig,
    mut prompter: Option<&mut dyn Prompter>,
) -> Result<Answers, Vec<Diagnostic>> {
    let rec = recommendations(repo, chain);
    let mut a = Answers::default();
    // Anything the matching profile already answers is not asked and not repeated.
    let bundles_known = !profile.bundles.is_empty();
    let tags_known = !profile.tags.is_empty();

    let mut choose = false;
    if let Some(p) = prompter.as_deref_mut()
        && ((!bundles_known && !rec.bundles.is_empty()) || (!tags_known && !rec.tags.is_empty()))
    {
        let shown = [rec.bundles.as_slice(), rec.tags.as_slice()]
            .concat()
            .join(", ");
        choose = !p.confirm(&format!("Use recommended setup? ({shown})"), true);
    }
    if !bundles_known {
        a.bundles = rec.bundles.clone();
        if choose && let Some(p) = prompter.as_deref_mut() {
            let all: Vec<String> = repo.bundles.keys().cloned().collect();
            a.bundles = p.multi("Bundles", &all, &rec.bundles);
        }
    }
    if !tags_known {
        a.tags = rec.tags.clone();
        if choose && let Some(p) = prompter.as_deref_mut() {
            let all: Vec<String> = repo.manifest.tags.keys().cloned().collect();
            a.tags = p.multi("Tags", &all, &rec.tags);
        }
    }

    let effective: Vec<String> = if bundles_known {
        profile.bundles.clone()
    } else {
        a.bundles.clone()
    };
    let mut missing: Vec<String> = Vec::new();
    // The same resolver `dots config` uses. Conditions are not evaluated here: a conditional
    // package may still need its answers.
    let when = |_: &str| Ok(true);
    let mut inputs = dots_config::Inputs::new(repo, profile, &when);
    inputs.bundles = &effective;
    let resolved = dots_config::resolve(&inputs);
    for pkg in dots_config::packages_of(repo, &effective) {
        for s in resolved.of_package(&pkg) {
            // Anything a scope above the default already answers is not asked or repeated.
            if s.answered() {
                continue;
            }
            // `ask` settings are only asked when the user chose to choose.
            if !(s.missing() || (s.decl.ask && choose)) {
                continue;
            }
            let id = s.id();
            let default = s.default().map(crate::config_cmd::plain);
            let Some(p) = prompter.as_deref_mut() else {
                if s.missing() {
                    missing.push(id);
                }
                continue;
            };
            let question = s.decl.question.clone().unwrap_or_else(|| id.clone());
            let mut text = p.input(&question, default.as_deref());
            if text.is_empty() {
                if s.missing() {
                    missing.push(id);
                }
                continue;
            }
            // A bad answer is asked again, with the reason, a few times.
            let mut parsed = s.decl.parse_text(&text);
            for _ in 0..2 {
                let Err(why) = &parsed else { break };
                text = p.input(&format!("{question} ({why})"), default.as_deref());
                parsed = s.decl.parse_text(&text);
            }
            match parsed {
                Ok(v) if s.default() == Some(&v) => {}
                Ok(v) => {
                    a.settings
                        .entry(pkg.clone())
                        .or_default()
                        .insert(s.key.clone(), v);
                }
                Err(detail) => {
                    let detail = if s.secret() {
                        "not a valid value".into()
                    } else {
                        detail
                    };
                    return Err(vec![Diagnostic::from_code(
                        "setting-invalid",
                        &[
                            ("setting", &id),
                            ("origin", "your answer"),
                            ("detail", &detail),
                        ],
                    )]);
                }
            }
        }
    }
    if missing.is_empty() {
        Ok(a)
    } else {
        Err(vec![Diagnostic::from_code(
            "init-answers-needed",
            &[("settings", &missing.join(", "))],
        )])
    }
}

// ---------------------------------------------------------------- machine.toml

fn quote(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_else(|_| "\"\"".into())
}

fn key(k: &str) -> String {
    if !k.is_empty()
        && k.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
    {
        k.to_string()
    } else {
        quote(k)
    }
}

fn tilde(p: &Path, home: &Path) -> String {
    match p.strip_prefix(home) {
        Ok(rest) => format!("~/{}", rest.display()),
        Err(_) => p.display().to_string(),
    }
}

fn list(items: &[String]) -> String {
    let q: Vec<String> = items.iter().map(|s| quote(s)).collect();
    format!("[{}]", q.join(", "))
}

pub fn machine_toml(root: Option<&str>, profile: Option<&str>, a: &Answers) -> String {
    let mut s = String::from("schema = 1\n");
    if let Some(r) = root {
        s.push_str(&format!("root = {}\n", quote(r)));
    }
    if let Some(p) = profile {
        s.push_str(&format!("profile = {}\n", quote(p)));
    }
    if !a.bundles.is_empty() {
        s.push_str(&format!("bundles = {}\n", list(&a.bundles)));
    }
    if !a.tags.is_empty() {
        s.push_str(&format!("tags = {}\n", list(&a.tags)));
    }
    for (pkg, values) in &a.settings {
        s.push_str(&format!("\n[settings.{}]\n", key(pkg)));
        for (k, v) in values {
            s.push_str(&format!("{} = {v}\n", key(k)));
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Script {
        confirm: bool,
        inputs: Vec<String>,
        asked: Vec<String>,
    }

    impl Prompter for Script {
        fn confirm(&mut self, q: &str, _: bool) -> bool {
            self.asked.push(q.to_string());
            self.confirm
        }
        fn multi(&mut self, q: &str, _: &[String], d: &[String]) -> Vec<String> {
            self.asked.push(q.to_string());
            d.to_vec()
        }
        fn input(&mut self, q: &str, d: Option<&str>) -> String {
            self.asked.push(q.to_string());
            if self.inputs.is_empty() {
                d.unwrap_or_default().to_string()
            } else {
                self.inputs.remove(0)
            }
        }
    }

    fn repo(dir: &Path) -> Repo {
        let w = |rel: &str, text: &str| {
            let p = dir.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, text).unwrap();
        };
        w(
            "dots.toml",
            "schema = 1\n[tags]\nwork = { recommended = { macos = true } }\n",
        );
        w(
            "bundles/base.toml",
            "schema = 1\nrecommended = true\npackages = [\"git\"]\n",
        );
        w(
            "packages/git/package.toml",
            "schema = 1\nname = \"git\"\n[settings.email]\ntype = \"string\"\nrequired = true\n[settings.name]\ntype = \"string\"\ndefault = \"me\"\nask = true\n",
        );
        Repo::load(dir).unwrap().value
    }

    fn chain(os: &str) -> Chain {
        let mut f = detect(&RealSources, &FactOverrides::default());
        f.os = os.to_string();
        f.distro = None;
        f.id_like.clear();
        f.is_wsl = false;
        Chain::build(&f, &BTreeMap::new(), &[])
    }

    #[test]
    fn required_without_prompter_fails_with_the_setting_named() {
        let d = tempfile::tempdir().unwrap();
        let r = repo(d.path());
        let e = answer(&r, &chain("macos"), &MachineConfig::default(), None).unwrap_err();
        assert_eq!(e[0].code, "init-answers-needed");
        assert!(e[0].message.contains("git.email"), "{}", e[0].message);
    }

    #[test]
    fn scripted_prompter_answers_required_and_skips_ask_when_recommended() {
        let d = tempfile::tempdir().unwrap();
        let r = repo(d.path());
        let mut p = Script {
            confirm: true,
            inputs: vec!["a@b.c".into()],
            asked: vec![],
        };
        let a = answer(&r, &chain("macos"), &MachineConfig::default(), Some(&mut p)).unwrap();
        assert_eq!(a.bundles, ["base"]);
        assert_eq!(a.tags, ["work"]);
        assert_eq!(a.settings["git"]["email"], json!("a@b.c"));
        assert!(!a.settings["git"].contains_key("name"));
        assert_eq!(p.asked.len(), 2, "{:?}", p.asked);
    }

    #[test]
    fn tag_recommendation_is_per_platform() {
        let d = tempfile::tempdir().unwrap();
        let r = repo(d.path());
        assert_eq!(recommendations(&r, &chain("macos")).tags, ["work"]);
        assert!(recommendations(&r, &chain("linux")).tags.is_empty());
    }

    #[test]
    fn machine_toml_round_trips_through_the_strict_parser() {
        let mut a = Answers {
            bundles: vec!["base".into()],
            tags: vec!["work".into()],
            ..Default::default()
        };
        a.settings
            .entry("git".into())
            .or_default()
            .insert("email".into(), json!("a\"b@c"));
        let text = machine_toml(Some("~/x"), Some("host"), &a);
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("machine.toml");
        std::fs::write(&path, &text).unwrap();
        std::fs::create_dir_all(d.path().join("machines")).unwrap();
        std::fs::write(d.path().join("machines/host.toml"), "schema = 1\n").unwrap();
        let m = MachineConfig::load(d.path(), &path, "other").unwrap().value;
        assert_eq!(m.bundles, ["base"]);
        assert_eq!(m.settings["git"]["email"], json!("a\"b@c"));
    }
}
