//! Manual actions: `packages/<pkg>/manual/<id>.md` (ARCHITECTURE section 9, spec
//! `manual-actions`). Steps software cannot do for the user (grant a permission, click an
//! enable button) are a markdown file with a YAML front matter; the body is what the user
//! reads. This module only parses and validates the file; what is pending on a machine is
//! decided by the engine state and the CLI.

use std::path::{Path, PathBuf};

use serde::Deserialize;

/// When an action becomes pending for a package that is active on this machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum After {
    /// Once the package is installed (the default).
    #[default]
    Install,
    /// Once the package's files are in place: while the package is active.
    Files,
    /// Always, and checked on every run: a closed action whose check fails opens again. This
    /// is what account and session changes (login shell, group membership) use.
    Always,
}

impl After {
    pub fn word(self) -> &'static str {
        match self {
            After::Install => "install",
            After::Files => "files",
            After::Always => "always",
        }
    }
}

/// What finishing an action needs from the user beyond the steps themselves; shown as a badge
/// and combined in the apply summary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Requires {
    Logout,
    Reboot,
    RestartApp(String),
}

impl Requires {
    fn parse(s: &str) -> Result<Requires, String> {
        match s {
            "logout" => Ok(Requires::Logout),
            "reboot" => Ok(Requires::Reboot),
            _ => match s.strip_prefix("restart-app:") {
                Some(app) if !app.trim().is_empty() => Ok(Requires::RestartApp(app.trim().into())),
                _ => Err(format!(
                    "`{s}` is not a requirement; use `logout`, `reboot` or `restart-app:<name>`"
                )),
            },
        }
    }

    /// The badge text: `logout`, `reboot`, `restart Safari`.
    pub fn badge(&self) -> String {
        match self {
            Requires::Logout => "logout".into(),
            Requires::Reboot => "reboot".into(),
            Requires::RestartApp(a) => format!("restart {a}"),
        }
    }
}

/// One parsed manual action.
#[derive(Debug, Clone, PartialEq)]
pub struct ManualAction {
    pub package: String,
    /// Unique within the package; the full id is `<pkg>/<id>`.
    pub id: String,
    pub title: String,
    /// A platform condition (`os = "macos"`): the action only exists where it holds.
    pub when: Option<String>,
    pub after: After,
    /// A shell command; exit 0 means the step is done.
    pub check: Option<String>,
    pub requires: Vec<Requires>,
    /// Bump it when the steps change: the action is pending again on every machine.
    pub revision: u32,
    /// Needs a terminal: `apply` leaves it as a todo instead of blocking an unattended run.
    pub interactive: bool,
    /// The markdown after the front matter.
    pub body: String,
    /// The file, for messages and `check`'s working directory (its package directory).
    pub path: PathBuf,
}

impl ManualAction {
    /// `<pkg>/<id>`.
    pub fn full_id(&self) -> String {
        format!("{}/{}", self.package, self.id)
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Front {
    id: Option<String>,
    title: Option<String>,
    when: Option<String>,
    after: Option<String>,
    check: Option<String>,
    #[serde(default)]
    requires: Vec<String>,
    revision: Option<u32>,
    #[serde(default)]
    interactive: bool,
}

/// What is wrong with a manual file, with the 1-based line when it is known.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManualProblem {
    pub line: Option<u32>,
    pub detail: String,
}

fn problem(line: Option<u32>, detail: impl Into<String>) -> ManualProblem {
    ManualProblem {
        line,
        detail: detail.into(),
    }
}

/// Split `---\n<yaml>\n---\n<body>` into the YAML and the body.
fn split(text: &str) -> Option<(&str, &str)> {
    let rest = text
        .strip_prefix("---\n")
        .or(text.strip_prefix("---\r\n"))?;
    let mut offset = 0;
    for line in rest.split_inclusive('\n') {
        if line.trim_end() == "---" {
            return Some((&rest[..offset], &rest[offset + line.len()..]));
        }
        offset += line.len();
    }
    None
}

/// Parse one manual file of `package`; `stem` (the file name without `.md`) is the id when the
/// front matter has none.
pub fn parse(
    package: &str,
    stem: &str,
    path: &Path,
    text: &str,
) -> Result<ManualAction, ManualProblem> {
    let Some((yaml, body)) = split(text) else {
        return Err(problem(
            Some(1),
            "the file must start with a `---` front matter block, closed by another `---`",
        ));
    };
    let front: Front = serde_yaml_ng::from_str(yaml).map_err(|e| {
        // The YAML starts on line 2 of the file.
        problem(e.location().map(|l| l.line() as u32 + 1), e.to_string())
    })?;
    let id = front.id.unwrap_or_else(|| stem.to_string());
    let valid_id = !id.is_empty()
        && id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_');
    if !valid_id {
        return Err(problem(
            Some(2),
            format!("id `{id}` must be lowercase letters, digits, `-` or `_`"),
        ));
    }
    let title = match front.title.map(|t| t.trim().to_string()) {
        Some(t) if !t.is_empty() => t,
        _ => return Err(problem(Some(2), "`title` is required")),
    };
    let after = match front.after.as_deref() {
        None | Some("install") => After::Install,
        Some("files") => After::Files,
        Some("always") => After::Always,
        Some(other) => {
            return Err(problem(
                Some(2),
                format!("`after: {other}` is not one of `install`, `files`, `always`"),
            ));
        }
    };
    let requires = front
        .requires
        .iter()
        .map(|r| Requires::parse(r))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| problem(Some(2), e))?;
    if front.check.as_deref().is_some_and(|c| c.trim().is_empty()) {
        return Err(problem(Some(2), "`check` is empty"));
    }
    let revision = front.revision.unwrap_or(1);
    if revision == 0 {
        return Err(problem(Some(2), "`revision` starts at 1"));
    }
    Ok(ManualAction {
        package: package.to_string(),
        id,
        title,
        when: front.when,
        after,
        check: front.check,
        requires,
        revision,
        interactive: front.interactive,
        body: body.trim_start_matches('\n').to_string(),
        path: path.to_path_buf(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(text: &str) -> Result<ManualAction, ManualProblem> {
        parse(
            "karabiner",
            "permissions",
            Path::new("permissions.md"),
            text,
        )
    }

    #[test]
    fn parses_the_documented_shape() {
        let a = p("---\nid: permissions\ntitle: Grant Input Monitoring\nwhen: 'os = \"macos\"'\nafter: files\ncheck: \"true\"\nrequires: [logout, restart-app:Safari]\nrevision: 2\n---\n\n1. Do it\n").unwrap();
        assert_eq!(a.full_id(), "karabiner/permissions");
        assert_eq!(a.after, After::Files);
        assert_eq!(a.revision, 2);
        assert_eq!(a.when.as_deref(), Some("os = \"macos\""));
        assert_eq!(
            a.requires,
            [Requires::Logout, Requires::RestartApp("Safari".into())]
        );
        assert_eq!(a.body, "1. Do it\n");
    }

    #[test]
    fn defaults_are_install_revision_one_and_the_file_stem() {
        let a = p("---\ntitle: T\n---\nbody").unwrap();
        assert_eq!(
            (a.id.as_str(), a.after, a.revision),
            ("permissions", After::Install, 1)
        );
        assert!(!a.interactive);
    }

    #[test]
    fn unknown_keys_and_bad_values_are_errors() {
        let e = p("---\ntitle: T\nbogus: 1\n---\n").unwrap_err();
        assert!(e.detail.contains("bogus"), "{}", e.detail);
        assert_eq!(e.line, Some(3));
        assert!(p("---\ntitle: T\nafter: sometime\n---\n").is_err());
        assert!(p("---\ntitle: T\nrequires: [dance]\n---\n").is_err());
        assert!(p("---\ntitle: T\nrevision: 0\n---\n").is_err());
        assert!(p("---\ntitle: T\nid: Bad Id\n---\n").is_err());
        assert!(p("---\nid: x\n---\n").is_err(), "a title is required");
        assert!(p("no front matter\n").is_err());
        assert!(p("---\ntitle: T\n").is_err(), "unclosed front matter");
    }
}
