//! Color tokens (tier x theme) and symbols (unicode / nerd / ascii). Renderers never use raw
//! colors or glyphs; they ask for a token or a state and get the right form for the context.

use dots_core::{Marker, State, Tone};

use crate::Tier;
use crate::config::IconSet;
use crate::ui::{Theme, Ui};

/// `(dark rgb, light rgb, 16-color SGR code)` per token (UX §3.1).
const fn palette(tone: Tone) -> ((u8, u8, u8), (u8, u8, u8), u8) {
    match tone {
        Tone::Ok => ((0x7e, 0xe7, 0x87), (0x1a, 0x7f, 0x37), 32),
        Tone::Warn => ((0xe3, 0xb3, 0x41), (0x9a, 0x67, 0x00), 33),
        Tone::Err => ((0xff, 0x7b, 0x72), (0xcf, 0x22, 0x2e), 31),
        Tone::Info => ((0x79, 0xc0, 0xff), (0x09, 0x69, 0xda), 34),
        Tone::Accent => ((0xd2, 0xa8, 0xff), (0x82, 0x50, 0xdf), 35),
        Tone::Muted => ((0x8b, 0x94, 0x9e), (0x6e, 0x77, 0x81), 90),
        Tone::Hint => ((0x56, 0xd4, 0xdd), (0x0a, 0x7f, 0x8a), 36),
    }
}

/// The SGR parameter string for a token in this context (`None` in the Plain tier).
pub fn color_code(ui: &Ui, tone: Tone) -> Option<String> {
    let (dark, light, basic) = palette(tone);
    match ui.tier {
        Tier::Plain => None,
        Tier::Basic => Some(basic.to_string()),
        Tier::Rich => {
            let (r, g, b) = if ui.theme == Theme::Dark { dark } else { light };
            Some(format!("38;2;{r};{g};{b}"))
        }
    }
}

/// Wrap `text` in the SGR for `tone` (optional) plus bold/dim. Plain tier returns it untouched.
pub fn paint(ui: &Ui, tone: Option<Tone>, bold: bool, dim: bool, text: &str) -> String {
    if ui.tier == Tier::Plain || text.is_empty() {
        return text.to_string();
    }
    let mut codes: Vec<String> = Vec::new();
    if bold {
        codes.push("1".into());
    }
    if dim {
        codes.push("2".into());
    }
    if let Some(c) = tone.and_then(|t| color_code(ui, t)) {
        codes.push(c);
    }
    if codes.is_empty() {
        return text.to_string();
    }
    format!("\x1b[{}m{text}\x1b[0m", codes.join(";"))
}

/// OSC 8 hyperlink when enabled; otherwise the text, unchanged.
pub fn hyperlink(ui: &Ui, target: &str, text: &str) -> String {
    if ui.hyperlinks {
        format!("\x1b]8;;{target}\x1b\\{text}\x1b]8;;\x1b\\")
    } else {
        text.to_string()
    }
}

/// The state symbol for the context's icon set.
pub fn state_symbol(icons: IconSet, s: State) -> &'static str {
    // (unicode, nerd, ascii). Every unicode glyph is one cell wide.
    let (u, n, a) = match s {
        State::Ok => ("✓", "\u{f00c}", "[ok]"),
        State::Outdated => ("↑", "\u{f062}", "[up]"),
        State::Drift => ("≠", "\u{f12a}", "[!=]"),
        State::Modified => ("✎", "\u{f040}", "[mod]"),
        State::Pending => ("○", "\u{f10c}", "[ ]"),
        State::Done => ("●", "\u{f111}", "[x]"),
        State::Missing => ("✗", "\u{f00d}", "[--]"),
        State::Failed => ("✗", "\u{f057}", "[FAIL]"),
        State::Conflict => ("↯", "\u{f0e7}", "[!!]"),
        State::Orphan => ("◌", "\u{f1ce}", "[orph]"),
        State::NotApplicable => ("·", "\u{f068}", "[n/a]"),
        State::Running => ("…", "\u{f110}", "[..]"),
        State::Skipped => ("⊘", "\u{f05e}", "[skip]"),
        State::Unmanaged => ("?", "\u{f128}", "[?]"),
        State::Excluded => ("⊖", "\u{f056}", "[excl]"),
        State::PendingRestart => ("↻", "\u{f021}", "[rst]"),
        State::BlockedByPolicy => ("⊗", "\u{f023}", "[blk]"),
        State::Stale => ("≈", "\u{f017}", "[old]"),
        State::Dangling => ("∅", "\u{f127}", "[dang]"),
        State::ForeignEntries => ("↯", "\u{f0e7}", "[!!]"),
        State::Tampered => ("‼", "\u{f071}", "[TAMP]"),
        State::Auto => ("↺", "\u{f021}", "[auto]"),
    };
    pick(icons, u, n, a)
}

pub fn marker_symbol(icons: IconSet, m: Marker) -> &'static str {
    let (u, a) = match m {
        Marker::Add => ("+", "+"),
        Marker::Change => ("~", "~"),
        Marker::Remove => ("-", "-"),
        Marker::Adopt => ("=", "="),
        Marker::Upgrade => ("↑", "^"),
        Marker::Shadow => ("▲", "!"),
        Marker::Replace => ("↯", "!!"),
        Marker::Blocked => ("✗", "x"),
    };
    pick(icons, u, u, a)
}

pub fn marker_tone(m: Marker) -> Tone {
    match m {
        Marker::Add => Tone::Ok,
        Marker::Change | Marker::Upgrade | Marker::Shadow => Tone::Warn,
        Marker::Remove | Marker::Replace | Marker::Blocked => Tone::Err,
        Marker::Adopt => Tone::Muted,
    }
}

pub fn arrow(icons: IconSet) -> &'static str {
    pick(icons, "→", "\u{f061}", "->")
}

pub fn callout_symbol(icons: IconSet) -> &'static str {
    pick(icons, "▲", "\u{f071}", "[!]")
}

pub fn headline_symbol(icons: IconSet, tone: Tone) -> Option<&'static str> {
    match tone {
        Tone::Ok => Some(state_symbol(icons, State::Ok)),
        Tone::Warn => Some(callout_symbol(icons)),
        Tone::Err => Some(state_symbol(icons, State::Failed)),
        _ => None,
    }
}

pub struct TreeGlyphs {
    pub branch: &'static str,
    pub last: &'static str,
    pub bar: &'static str,
    pub blank: &'static str,
}

pub fn tree_glyphs(icons: IconSet) -> TreeGlyphs {
    if icons == IconSet::Ascii {
        TreeGlyphs {
            branch: "|-- ",
            last: "\\-- ",
            bar: "|   ",
            blank: "    ",
        }
    } else {
        TreeGlyphs {
            branch: "├─ ",
            last: "└─ ",
            bar: "│  ",
            blank: "   ",
        }
    }
}

fn pick(icons: IconSet, u: &'static str, n: &'static str, a: &'static str) -> &'static str {
    match icons {
        IconSet::Unicode => u,
        IconSet::Nerd => n,
        IconSet::Ascii => a,
    }
}

pub const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

#[cfg(test)]
mod tests {
    use super::*;
    use unicode_width::UnicodeWidthStr;

    #[test]
    fn every_unicode_symbol_is_one_cell() {
        for s in State::ALL {
            let sym = state_symbol(IconSet::Unicode, s);
            assert_eq!(UnicodeWidthStr::width(sym), 1, "{s:?} {sym}");
        }
        for sym in SPINNER {
            assert_eq!(UnicodeWidthStr::width(sym), 1);
        }
    }

    #[test]
    fn ascii_symbols_are_ascii() {
        for s in State::ALL {
            assert!(state_symbol(IconSet::Ascii, s).is_ascii());
        }
    }

    #[test]
    fn spec_symbols_and_tokens_are_stable() {
        assert_eq!(state_symbol(IconSet::Ascii, State::Ok), "[ok]");
        assert_eq!(state_symbol(IconSet::Ascii, State::Conflict), "[!!]");
        assert_eq!(State::Outdated.tone(), Tone::Warn);
        assert_eq!(State::Missing.tone(), Tone::Err);
        assert_eq!(State::Orphan.tone(), Tone::Muted);
    }

    #[test]
    fn tokens_depend_on_theme_and_tier() {
        let mut ui = Ui::fixed(Tier::Rich, 80);
        assert_eq!(color_code(&ui, Tone::Ok).unwrap(), "38;2;126;231;135");
        ui.theme = Theme::Light;
        assert_eq!(color_code(&ui, Tone::Ok).unwrap(), "38;2;26;127;55");
        ui.tier = Tier::Basic;
        assert_eq!(color_code(&ui, Tone::Warn).unwrap(), "33");
        ui.tier = Tier::Plain;
        assert_eq!(color_code(&ui, Tone::Warn), None);
    }
}
