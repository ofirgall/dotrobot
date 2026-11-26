//! Width-aware text helpers. Everything is measured with `unicode-width`, so CJK paths never
//! break alignment; nothing is cut without an ellipsis.

use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

pub fn width(s: &str) -> usize {
    UnicodeWidthStr::width(s)
}

pub fn pad(s: &str, to: usize) -> String {
    let w = width(s);
    format!("{s}{}", " ".repeat(to.saturating_sub(w)))
}

fn take_width(s: &str, max: usize, from_end: bool) -> String {
    let mut used = 0;
    let mut out: Vec<char> = Vec::new();
    let chars: Box<dyn Iterator<Item = char>> = if from_end {
        Box::new(s.chars().rev())
    } else {
        Box::new(s.chars())
    };
    for c in chars {
        let w = c.width().unwrap_or(0);
        if used + w > max {
            break;
        }
        used += w;
        out.push(c);
    }
    if from_end {
        out.reverse();
    }
    out.into_iter().collect()
}

/// Cut the end: `long-name` -> `long-n…`.
pub fn truncate_end(s: &str, max: usize, ellipsis: &str) -> String {
    if width(s) <= max {
        return s.to_string();
    }
    let e = width(ellipsis);
    if max <= e {
        return take_width(ellipsis, max, false);
    }
    format!("{}{ellipsis}", take_width(s, max - e, false))
}

/// Cut the middle, keeping the end (the file name matters most): `~/.config/…/settings.json`.
pub fn truncate_middle(s: &str, max: usize, ellipsis: &str) -> String {
    if width(s) <= max {
        return s.to_string();
    }
    let e = width(ellipsis);
    if max <= e + 1 {
        return truncate_end(s, max, ellipsis);
    }
    let avail = max - e;
    // The tail (file name) matters most: it gets two thirds.
    let tail = (avail * 2).div_ceil(3);
    let head = avail - tail;
    format!(
        "{}{ellipsis}{}",
        take_width(s, head, false),
        take_width(s, tail, true)
    )
}

/// Greedy word wrap on display width. A word longer than `max` is cut with an ellipsis.
pub fn wrap(text: &str, max: usize, ellipsis: &str) -> Vec<String> {
    let max = max.max(8);
    let mut lines = Vec::new();
    for para in text.split('\n') {
        let mut line = String::new();
        for word in para.split_whitespace() {
            let word = truncate_end(word, max, ellipsis);
            if line.is_empty() {
                line = word;
            } else if width(&line) + 1 + width(&word) <= max {
                line.push(' ');
                line.push_str(&word);
            } else {
                lines.push(std::mem::take(&mut line));
                line = word;
            }
        }
        lines.push(line);
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn middle_keeps_file_name() {
        let s = truncate_middle("~/.config/ghostty/themes/settings.json", 24, "…");
        assert!(width(&s) <= 24);
        assert!(s.ends_with("settings.json"), "{s}");
        assert!(s.contains('…'));
    }

    #[test]
    fn cjk_is_measured_by_cells() {
        assert_eq!(width("日本"), 4);
        assert!(width(&truncate_end("日本語のパス", 7, "…")) <= 7);
    }

    #[test]
    fn short_text_is_untouched() {
        assert_eq!(truncate_end("abc", 3, "…"), "abc");
        assert_eq!(truncate_middle("abc", 3, "…"), "abc");
    }
}
