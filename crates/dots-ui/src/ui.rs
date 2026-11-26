//! The resolved presentation context: tier, theme, icons, hyperlinks, width, pager, motion.
//!
//! Precedence everywhere is flag > environment > `ui.toml` > detection.

use std::io::IsTerminal;

use dots_core::{Env, GlobalOptions};

use crate::config::{IconSet, ThemeChoice, Toggle, UiConfig};
use crate::tier::{Tier, detect_tier_with};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Theme {
    Dark,
    Light,
}

/// What the real terminal looks like. Tests build this by hand.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Term {
    pub stdout_tty: bool,
    pub stderr_tty: bool,
    pub width: Option<usize>,
    pub height: Option<usize>,
}

impl Term {
    pub fn detect() -> Term {
        let size = rustix::termios::tcgetwinsize(std::io::stdout())
            .ok()
            .filter(|w| w.ws_col > 0);
        Term {
            stdout_tty: std::io::stdout().is_terminal(),
            stderr_tty: std::io::stderr().is_terminal(),
            width: size.map(|w| usize::from(w.ws_col)),
            height: size.map(|w| usize::from(w.ws_row)).filter(|h| *h > 0),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ui {
    pub tier: Tier,
    pub theme: Theme,
    pub icons: IconSet,
    pub hyperlinks: bool,
    pub width: usize,
    /// Terminal height; `None` when unknown (never page).
    pub height: Option<usize>,
    pub reduce_motion: bool,
    /// The pager command, or `None` when paging is off (Plain tier, not a TTY, `--no-input`).
    pub pager: Option<String>,
    pub notify: bool,
    pub stderr_tty: bool,
}

/// Below this width every table uses the compact (stacked) layout.
pub const COMPACT_BELOW: usize = 60;
pub const DEFAULT_PAGER: &str = "less -RFX";

impl Ui {
    /// Resolve everything from the flags, the environment, `ui.toml` and the terminal.
    pub fn resolve(opts: &GlobalOptions, env: &dyn Env, cfg: &UiConfig, term: &Term) -> Ui {
        let tier = detect_tier_with(opts.color, cfg.color, env, term.stdout_tty);
        let icons = if tier == Tier::Plain || !utf8_locale(env) {
            IconSet::Ascii
        } else {
            cfg.icons.unwrap_or(IconSet::Unicode)
        };
        let width = term
            .width
            .or_else(|| env.get_str("COLUMNS").and_then(|c| c.parse().ok()))
            .filter(|w| *w > 0)
            .unwrap_or(80);
        let reduce_motion =
            tier == Tier::Plain || env.get_str("DOTS_REDUCE_MOTION").is_some_and(|v| v != "0");
        let paging_ok = term.stdout_tty && tier != Tier::Plain && !opts.no_input;
        let pager = if paging_ok {
            let cmd = env
                .get_str("DOTS_PAGER")
                .or_else(|| env.get_str("PAGER"))
                .or_else(|| cfg.pager.clone())
                .unwrap_or_else(|| DEFAULT_PAGER.to_string());
            (!cmd.trim().is_empty()).then_some(cmd)
        } else {
            None
        };
        Ui {
            tier,
            theme: detect_theme(cfg.theme, env),
            icons,
            hyperlinks: hyperlinks_enabled(tier, cfg.hyperlinks, env),
            width,
            height: term.height,
            reduce_motion,
            pager,
            notify: cfg.notify.unwrap_or(true),
            stderr_tty: term.stderr_tty,
        }
    }

    /// Resolve from the process environment without reading `ui.toml`.
    pub fn from_process(opts: &GlobalOptions, env: &dyn Env) -> Ui {
        Ui::resolve(opts, env, &UiConfig::default(), &Term::detect())
    }

    /// A fixed context for renderer tests and goldens: dark theme, no hyperlinks, no pager.
    pub fn fixed(tier: Tier, width: usize) -> Ui {
        Ui {
            tier,
            theme: Theme::Dark,
            icons: if tier == Tier::Plain {
                IconSet::Ascii
            } else {
                IconSet::Unicode
            },
            hyperlinks: false,
            width,
            height: None,
            reduce_motion: tier == Tier::Plain,
            pager: None,
            notify: true,
            stderr_tty: false,
        }
    }

    pub fn compact(&self) -> bool {
        self.width < COMPACT_BELOW
    }

    /// Animation allowed: not Plain, not reduced motion, stderr is a terminal.
    pub fn animate(&self) -> bool {
        self.tier != Tier::Plain && !self.reduce_motion && self.stderr_tty
    }

    pub fn ellipsis(&self) -> &'static str {
        if self.icons == IconSet::Ascii {
            "..."
        } else {
            "…"
        }
    }
}

fn utf8_locale(env: &dyn Env) -> bool {
    for k in ["LC_ALL", "LC_CTYPE", "LANG"] {
        if let Some(v) = env.get_str(k) {
            let v = v.to_ascii_lowercase();
            return v.contains("utf-8") || v.contains("utf8");
        }
    }
    // Nothing set: modern terminals are UTF-8.
    true
}

/// `ui.toml` `theme` if set, else `COLORFGBG` (`fg;bg` or `fg;default;bg`), else dark.
/// No terminal queries are made: they can block, and the spec does not require them.
pub fn detect_theme(choice: ThemeChoice, env: &dyn Env) -> Theme {
    match choice {
        ThemeChoice::Dark => return Theme::Dark,
        ThemeChoice::Light => return Theme::Light,
        ThemeChoice::Auto => {}
    }
    let bg = env
        .get_str("COLORFGBG")
        .and_then(|v| v.rsplit(';').next().and_then(|b| b.parse::<u8>().ok()));
    match bg {
        Some(7 | 9..=15) => Theme::Light,
        _ => Theme::Dark,
    }
}

fn hyperlinks_enabled(tier: Tier, toggle: Toggle, env: &dyn Env) -> bool {
    if tier == Tier::Plain {
        return false;
    }
    match toggle {
        Toggle::Never => false,
        Toggle::Always => true,
        Toggle::Auto => tier == Tier::Rich && terminal_supports_osc8(env),
    }
}

/// Allowlist from UX §2. Inside tmux links are only passed through when the user opts in with
/// `hyperlinks = "always"`.
fn terminal_supports_osc8(env: &dyn Env) -> bool {
    if env.get_str("TMUX").is_some() {
        return false;
    }
    let prog = env.get_str("TERM_PROGRAM").unwrap_or_default();
    let term = env.get_str("TERM").unwrap_or_default();
    matches!(
        prog.as_str(),
        "ghostty" | "WezTerm" | "iTerm.app" | "vscode"
    ) || env.get_str("KITTY_WINDOW_ID").is_some()
        || env.get_str("WT_SESSION").is_some()
        || term == "xterm-kitty"
        || term == "xterm-ghostty"
        || term.starts_with("foot")
        || env.get_str("KONSOLE_VERSION").is_some()
        || env
            .get_str("VTE_VERSION")
            .and_then(|v| v.parse::<u32>().ok())
            .is_some_and(|v| v >= 5000)
}

#[cfg(test)]
mod tests {
    use super::*;
    use dots_core::MapEnv;

    fn tty() -> Term {
        Term {
            stdout_tty: true,
            stderr_tty: true,
            width: Some(100),
            height: Some(40),
        }
    }

    fn resolve(env: MapEnv, cfg: UiConfig) -> Ui {
        Ui::resolve(&GlobalOptions::default(), &env, &cfg, &tty())
    }

    #[test]
    fn colorfgbg_selects_light() {
        let light = resolve(MapEnv::new().with("COLORFGBG", "0;15"), UiConfig::default());
        assert_eq!(light.theme, Theme::Light);
        let dark = resolve(MapEnv::new().with("COLORFGBG", "15;0"), UiConfig::default());
        assert_eq!(dark.theme, Theme::Dark);
        let three = resolve(
            MapEnv::new().with("COLORFGBG", "0;default;15"),
            UiConfig::default(),
        );
        assert_eq!(three.theme, Theme::Light);
        assert_eq!(
            resolve(MapEnv::new(), UiConfig::default()).theme,
            Theme::Dark
        );
    }

    #[test]
    fn config_theme_overrides_colorfgbg() {
        let cfg = UiConfig {
            theme: ThemeChoice::Dark,
            ..Default::default()
        };
        assert_eq!(
            resolve(MapEnv::new().with("COLORFGBG", "0;15"), cfg).theme,
            Theme::Dark
        );
    }

    #[test]
    fn plain_and_non_utf8_use_ascii_icons() {
        let cfg = UiConfig {
            icons: Some(IconSet::Nerd),
            ..Default::default()
        };
        assert_eq!(resolve(MapEnv::new(), cfg.clone()).icons, IconSet::Nerd);
        assert_eq!(
            resolve(MapEnv::new().with("NO_COLOR", "1"), cfg.clone()).icons,
            IconSet::Ascii
        );
        assert_eq!(
            resolve(MapEnv::new().with("LANG", "C"), cfg).icons,
            IconSet::Ascii
        );
    }

    #[test]
    fn hyperlinks_need_a_rich_allowlisted_terminal() {
        let env = || MapEnv::new().with("COLORTERM", "truecolor");
        assert!(!resolve(env(), UiConfig::default()).hyperlinks);
        assert!(resolve(env().with("TERM_PROGRAM", "ghostty"), UiConfig::default()).hyperlinks);
        assert!(
            !resolve(
                env().with("TERM_PROGRAM", "ghostty").with("TMUX", "x"),
                UiConfig::default()
            )
            .hyperlinks
        );
        let never = UiConfig {
            hyperlinks: Toggle::Never,
            ..Default::default()
        };
        assert!(!resolve(env().with("TERM_PROGRAM", "ghostty"), never).hyperlinks);
    }

    #[test]
    fn reduced_motion_and_plain_disable_animation() {
        assert!(resolve(MapEnv::new(), UiConfig::default()).animate());
        assert!(
            !resolve(
                MapEnv::new().with("DOTS_REDUCE_MOTION", "1"),
                UiConfig::default()
            )
            .animate()
        );
        assert!(!resolve(MapEnv::new().with("CI", "1"), UiConfig::default()).animate());
    }

    #[test]
    fn pager_rules() {
        let p = |env: MapEnv, cfg: UiConfig, opts: GlobalOptions, term: Term| {
            Ui::resolve(&opts, &env, &cfg, &term).pager
        };
        let d = GlobalOptions::default();
        assert_eq!(
            p(MapEnv::new(), UiConfig::default(), d.clone(), tty()).as_deref(),
            Some("less -RFX")
        );
        // env beats ui.toml
        let cfg = UiConfig {
            pager: Some("bat".into()),
            ..Default::default()
        };
        assert_eq!(
            p(MapEnv::new(), cfg.clone(), d.clone(), tty()).as_deref(),
            Some("bat")
        );
        assert_eq!(
            p(
                MapEnv::new().with("PAGER", "more"),
                cfg.clone(),
                d.clone(),
                tty()
            )
            .as_deref(),
            Some("more")
        );
        assert_eq!(
            p(
                MapEnv::new().with("DOTS_PAGER", "x").with("PAGER", "more"),
                cfg,
                d.clone(),
                tty()
            )
            .as_deref(),
            Some("x")
        );
        // off: non-TTY, --no-input, Plain, empty pager
        assert_eq!(
            p(
                MapEnv::new(),
                UiConfig::default(),
                d.clone(),
                Term {
                    stdout_tty: false,
                    ..tty()
                }
            ),
            None
        );
        let no_input = GlobalOptions {
            no_input: true,
            ..Default::default()
        };
        assert_eq!(p(MapEnv::new(), UiConfig::default(), no_input, tty()), None);
        assert_eq!(
            p(
                MapEnv::new().with("NO_COLOR", "1"),
                UiConfig::default(),
                d.clone(),
                tty()
            ),
            None
        );
        let off = UiConfig {
            pager: Some(String::new()),
            ..Default::default()
        };
        assert_eq!(p(MapEnv::new(), off, d, tty()), None);
    }

    #[test]
    fn width_comes_from_terminal_then_columns() {
        let mut t = tty();
        assert_eq!(
            Ui::resolve(
                &GlobalOptions::default(),
                &MapEnv::new(),
                &UiConfig::default(),
                &t
            )
            .width,
            100
        );
        t.width = None;
        let e = MapEnv::new().with("COLUMNS", "50");
        assert_eq!(
            Ui::resolve(&GlobalOptions::default(), &e, &UiConfig::default(), &t).width,
            50
        );
        assert_eq!(
            Ui::resolve(
                &GlobalOptions::default(),
                &MapEnv::new(),
                &UiConfig::default(),
                &t
            )
            .width,
            80
        );
    }
}
