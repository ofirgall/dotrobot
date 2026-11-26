//! `~/.config/dots/ui.toml` (terminal-ui "UI preferences file").

use dots_core::report::Provenance;
use dots_core::{ColorChoice, Diagnostic};
use toml_edit::{DocumentMut, Item};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum IconSet {
    #[default]
    Unicode,
    Nerd,
    Ascii,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ThemeChoice {
    #[default]
    Auto,
    Dark,
    Light,
}

/// `auto|always|never`; `true`/`false` are accepted for the boolean-looking keys.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Toggle {
    #[default]
    Auto,
    Always,
    Never,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct UiConfig {
    pub color: Option<ColorChoice>,
    pub icons: Option<IconSet>,
    pub theme: ThemeChoice,
    pub hyperlinks: Toggle,
    pub notify: Option<bool>,
    /// `Some("")` disables paging.
    pub pager: Option<String>,
}

pub const KEYS: [&str; 6] = ["color", "icons", "theme", "hyperlinks", "notify", "pager"];

fn closest(key: &str) -> String {
    fn dist(a: &str, b: &str) -> usize {
        let b: Vec<char> = b.chars().collect();
        let mut prev: Vec<usize> = (0..=b.len()).collect();
        for (i, ca) in a.chars().enumerate() {
            let mut cur = vec![i + 1];
            for (j, cb) in b.iter().enumerate() {
                cur.push(
                    (prev[j] + usize::from(ca != *cb))
                        .min(prev[j + 1] + 1)
                        .min(cur[j] + 1),
                );
            }
            prev = cur;
        }
        prev[b.len()]
    }
    KEYS.iter()
        .min_by_key(|k| dist(key, k))
        .map(|k| k.to_string())
        .unwrap_or_default()
}

impl UiConfig {
    /// Parse the file's text. `file` is only used for diagnostics.
    pub fn parse(text: &str, file: &str) -> Result<UiConfig, Diagnostic> {
        let doc: DocumentMut = text.parse().map_err(|e: toml_edit::TomlError| {
            let line = e.span().map_or(1, |s| line_of(text, s.start));
            invalid("(syntax)", file, line, e.message(), "Fix the TOML syntax.")
        })?;
        let mut cfg = UiConfig::default();
        for (key, item) in doc.iter() {
            let line = text
                .lines()
                .position(|l| l.trim_start().starts_with(key))
                .map_or(1, |i| i + 1);
            let bad = |expected: &str| {
                invalid(
                    key,
                    file,
                    line,
                    &format!("expected {expected}"),
                    &format!("Set `{key}` to {expected}."),
                )
            };
            match key {
                "color" => {
                    cfg.color = Some(match str_of(item) {
                        Some("auto") => ColorChoice::Auto,
                        Some("always") => ColorChoice::Always,
                        Some("never") => ColorChoice::Never,
                        _ => return Err(bad("auto, always or never")),
                    })
                }
                "icons" => {
                    cfg.icons = Some(match str_of(item) {
                        Some("unicode") => IconSet::Unicode,
                        Some("nerd") => IconSet::Nerd,
                        Some("ascii") => IconSet::Ascii,
                        _ => return Err(bad("unicode, nerd or ascii")),
                    })
                }
                "theme" => {
                    cfg.theme = match str_of(item) {
                        Some("auto") => ThemeChoice::Auto,
                        Some("dark") => ThemeChoice::Dark,
                        Some("light") => ThemeChoice::Light,
                        _ => return Err(bad("auto, dark or light")),
                    }
                }
                "hyperlinks" => {
                    cfg.hyperlinks = match (str_of(item), item.as_bool()) {
                        (Some("auto"), _) => Toggle::Auto,
                        (Some("always"), _) | (_, Some(true)) => Toggle::Always,
                        (Some("never"), _) | (_, Some(false)) => Toggle::Never,
                        _ => return Err(bad("auto, always, never, true or false")),
                    }
                }
                "notify" => {
                    cfg.notify = Some(item.as_bool().ok_or_else(|| bad("true or false"))?);
                }
                "pager" => {
                    cfg.pager = Some(
                        str_of(item)
                            .ok_or_else(|| bad("a command string"))?
                            .to_string(),
                    );
                }
                other => {
                    let mut d = Diagnostic::from_code(
                        "schema-unknown-key",
                        &[
                            ("key", other),
                            ("file", file),
                            ("line", &line.to_string()),
                            ("suggestion", &closest(other)),
                        ],
                    );
                    d.provenance.push(prov(file, line));
                    return Err(d);
                }
            }
        }
        Ok(cfg)
    }
}

fn str_of(item: &Item) -> Option<&str> {
    item.as_str()
}

fn line_of(text: &str, offset: usize) -> usize {
    text[..offset.min(text.len())].matches('\n').count() + 1
}

fn prov(file: &str, line: usize) -> Provenance {
    Provenance {
        source: "ui.toml".into(),
        file: Some(file.to_string()),
        line: u32::try_from(line).ok(),
        col: None,
    }
}

fn invalid(key: &str, file: &str, line: usize, detail: &str, hint: &str) -> Diagnostic {
    let mut d = Diagnostic::from_code(
        "schema-invalid-value",
        &[
            ("key", key),
            ("file", file),
            ("line", &line.to_string()),
            ("detail", detail),
            ("hint", hint),
        ],
    );
    d.provenance.push(prov(file, line));
    d
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_all_keys() {
        let c = UiConfig::parse(
            "color = \"never\"\nicons = \"nerd\"\ntheme = \"light\"\nhyperlinks = false\nnotify = false\npager = \"bat\"\n",
            "ui.toml",
        )
        .unwrap();
        assert_eq!(c.color, Some(ColorChoice::Never));
        assert_eq!(c.icons, Some(IconSet::Nerd));
        assert_eq!(c.theme, ThemeChoice::Light);
        assert_eq!(c.hyperlinks, Toggle::Never);
        assert_eq!(c.notify, Some(false));
        assert_eq!(c.pager.as_deref(), Some("bat"));
    }

    #[test]
    fn unknown_key_is_a_registered_error_with_suggestion() {
        let d = UiConfig::parse("# c\ncolour = \"auto\"\n", "ui.toml").unwrap_err();
        assert_eq!(d.code, "schema-unknown-key");
        assert!(d.message.contains("ui.toml:2"), "{}", d.message);
        assert!(d.hint.contains("`color`"), "{}", d.hint);
    }

    #[test]
    fn bad_value_is_reported() {
        let d = UiConfig::parse("icons = \"emoji\"", "ui.toml").unwrap_err();
        assert_eq!(d.code, "schema-invalid-value");
    }
}
