//! The three renderers. Commands never print; they return a `Report` and this turns it into text.

use dots_core::blocks::{AttentionLine, Section};
use dots_core::report::{Diagnostic, Provenance};
use dots_core::{Block, Cell, GlobalOptions, Lead, Report, State, Style, Table, Tone, TreeNode};

use crate::config::IconSet;
use crate::layout::{pad, truncate_end, truncate_middle, width, wrap};
use crate::theme::{
    TreeGlyphs, arrow, callout_symbol, color_code, headline_symbol, hyperlink, marker_symbol,
    marker_tone, paint, state_symbol, tree_glyphs,
};
use crate::ui::{Theme, Ui};
use crate::{Mode, Tier};

/// Rendered output, not yet written anywhere.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Rendered {
    pub stdout: String,
    pub stderr: String,
}

/// Render with a context derived from the process environment (width from the terminal or
/// `COLUMNS`, theme from `COLORFGBG`) and the given tier. Prefer [`render_with`] with a
/// resolved [`Ui`], which also knows `ui.toml`.
pub fn render(report: &Report, mode: Mode, opts: &GlobalOptions) -> Rendered {
    match mode {
        Mode::Json => render_json(report),
        Mode::Human(tier) => {
            let mut ui = Ui::from_process(opts, &dots_core::ProcessEnv);
            ui.tier = tier;
            if tier == Tier::Plain {
                ui.icons = IconSet::Ascii;
                ui.hyperlinks = false;
            }
            render_human(report, &ui, opts)
        }
    }
}

/// Render for a resolved context. JSON is chosen with `opts.json`.
pub fn render_with(report: &Report, ui: &Ui, opts: &GlobalOptions) -> Rendered {
    if opts.json {
        render_json(report)
    } else {
        render_human(report, ui, opts)
    }
}

fn render_json(report: &Report) -> Rendered {
    // Exactly one JSON document on stdout; nothing else.
    let mut s = serde_json::to_string_pretty(&report.envelope()).expect("envelope serializes");
    s.push('\n');
    Rendered {
        stdout: s,
        stderr: String::new(),
    }
}

fn render_human(report: &Report, ui: &Ui, opts: &GlobalOptions) -> Rendered {
    let mut out = Rendered::default();
    if !opts.quiet {
        out.stdout = Blocks::new(ui).report(report);
        for w in &report.warnings {
            out.stderr
                .push_str(&diagnostic("warning", Tone::Warn, w, ui));
        }
    }
    for e in &report.errors {
        out.stderr.push_str(&diagnostic("error", Tone::Err, e, ui));
    }
    out
}

fn diagnostic(kind: &str, tone: Tone, d: &Diagnostic, ui: &Ui) -> String {
    let mut s = format!(
        "{}: {}\n",
        paint(ui, Some(tone), false, false, &format!("{kind}[{}]", d.code)),
        d.message
    );
    for p in &d.provenance {
        s.push_str(&format!(
            "  at {}\n",
            paint(ui, Some(Tone::Muted), false, false, &location(p))
        ));
    }
    if !d.hint.is_empty() {
        s.push_str(&format!(
            "  {} {}\n",
            paint(ui, Some(Tone::Hint), false, false, arrow(ui.icons)),
            paint(ui, Some(Tone::Hint), false, false, &d.hint)
        ));
    }
    s
}

fn location(p: &Provenance) -> String {
    match (&p.file, p.line, p.col) {
        (Some(f), Some(l), Some(c)) => format!("{f}:{l}:{c}"),
        (Some(f), Some(l), None) => format!("{f}:{l}"),
        (Some(f), _, _) => f.clone(),
        _ => p.source.clone(),
    }
}

// ------------------------------------------------------------------------------- blocks

/// Left indent of rows, hints and callouts under a headline or section title.
const INDENT: usize = 3;

pub struct Blocks<'a> {
    ui: &'a Ui,
}

/// What leads a row, resolved for the icon set.
struct LeadText {
    symbol: String,
    word: String,
    tone: Option<Tone>,
}

impl<'a> Blocks<'a> {
    pub fn new(ui: &'a Ui) -> Self {
        Blocks { ui }
    }

    /// The plain headline (when there is no tone-aware headline block) followed by every block.
    pub fn report(&self, report: &Report) -> String {
        let mut out = String::new();
        let has_headline_block = report
            .blocks
            .iter()
            .any(|b| matches!(b, Block::Headline { .. }));
        if !has_headline_block && let Some(h) = &report.headline {
            out.push_str(h);
            out.push('\n');
            if !report.blocks.is_empty() {
                out.push('\n');
            }
        }
        out.push_str(&self.blocks(&report.blocks));
        out
    }

    /// Blocks separated by blank lines; the result ends with exactly one newline (or is empty).
    pub fn blocks(&self, blocks: &[Block]) -> String {
        let mut out = String::new();
        let mut prev: Option<&Block> = None;
        for b in blocks {
            let text = self.block(b);
            if text.is_empty() {
                continue;
            }
            if let Some(p) = prev
                && gap_between(p, b)
            {
                out.push('\n');
            }
            out.push_str(&text);
            prev = Some(b);
        }
        out
    }

    fn block(&self, b: &Block) -> String {
        match b {
            Block::Context { title, parts } => self.context(title, parts),
            Block::Headline { tone, text } => self.headline(*tone, text),
            Block::Section(s) => self.section(s),
            Block::Table(t) => self.table(t),
            Block::Facts(f) => self.facts(f),
            Block::Tree(nodes) => self.tree(nodes),
            Block::Attention(lines) => self.attention(lines),
            Block::Hint(cmd) => format!("{}{}\n", " ".repeat(INDENT), self.hint_text(cmd)),
            Block::Callout(text) => self.callout(text),
            Block::Collapsed { state, text, aside } => self.collapsed(*state, text, aside),
            Block::Text(t) => self.text(t),
            Block::Code { title, lines } => self.code(title, lines),
            Block::Blank => "\n".to_string(),
        }
    }

    fn p(&self, tone: Option<Tone>, bold: bool, text: &str) -> String {
        paint(self.ui, tone, bold, false, text)
    }

    fn muted(&self, text: &str) -> String {
        self.p(Some(Tone::Muted), false, text)
    }

    /// Plain-tier text is ASCII: the few typographic characters commands use are folded.
    fn fold(&self, s: &str) -> String {
        if self.ui.icons == IconSet::Ascii {
            s.replace('·', "-")
                .replace('→', "->")
                .replace('≥', ">=")
                .replace('≤', "<=")
                .replace('…', "...")
                .replace('›', ">")
                .replace(['—', '–'], "-")
                .replace('×', "x")
                .replace('▶', ">")
        } else {
            s.to_string()
        }
    }

    fn sep(&self) -> &'static str {
        if self.ui.icons == IconSet::Ascii {
            "-"
        } else {
            "·"
        }
    }

    fn hint_text(&self, cmd: &str) -> String {
        let arrow = arrow(self.ui.icons);
        self.p(Some(Tone::Hint), false, &format!("{arrow} {cmd}"))
    }

    fn context(&self, title: &str, parts: &[String]) -> String {
        let (first, rest) = title.split_once(' ').unwrap_or((title, ""));
        let (badge, badge_w) = match self.ui.tier {
            Tier::Plain => (first.to_string(), width(first)),
            Tier::Basic => (
                self.p(Some(Tone::Accent), true, &format!(" {first} ")),
                width(first) + 2,
            ),
            Tier::Rich => {
                // The one background fill in the app.
                let bg = if self.ui.theme == Theme::Dark {
                    "48;2;48;38;66"
                } else {
                    "48;2;236;228;250"
                };
                let fg = color_code(self.ui, Tone::Accent).unwrap_or_default();
                (
                    format!("\x1b[1;{fg};{bg}m {first} \x1b[0m"),
                    width(first) + 2,
                )
            }
        };
        let mut head = format!(" {badge}");
        let mut head_w = 1 + badge_w;
        if !rest.is_empty() {
            head.push(' ');
            head.push_str(&self.p(None, true, rest));
            head_w += 1 + width(rest);
        }
        let sep = format!(" {} ", self.sep());
        let ctx = self.fold(&parts.join(&sep));
        if ctx.is_empty() {
            return format!("{head}\n");
        }
        if !self.ui.compact() && head_w + 3 + width(&ctx) <= self.ui.width {
            format!("{head}   {}\n", self.muted(&ctx))
        } else {
            let room = self.ui.width.saturating_sub(2);
            let ctx = truncate_end(&ctx, room, self.ui.ellipsis());
            format!("{head}\n {}\n", self.muted(&ctx))
        }
    }

    fn headline(&self, tone: Tone, text: &str) -> String {
        let mut s = String::new();
        for (i, line) in self.fold(text).lines().enumerate() {
            match (i, headline_symbol(self.ui.icons, tone)) {
                (0, Some(sym)) => s.push_str(&format!(
                    " {} {}\n",
                    self.p(Some(tone), true, sym),
                    self.p(Some(tone), true, line)
                )),
                (0, None) => s.push_str(&format!(" {}\n", self.p(None, true, line))),
                _ => s.push_str(&format!("   {line}\n")),
            }
        }
        s
    }

    fn section(&self, sec: &Section) -> String {
        // Pills share the title line while they fit; otherwise they wrap onto their own lines.
        let mut lines = vec![format!(" {}", self.p(None, true, &sec.title))];
        let mut used = 1 + width(&sec.title);
        for (state, n) in sec.pills.iter().filter(|(_, n)| *n > 0) {
            let plain = format!("{n} {}", state.word());
            let styled = self.p(Some(state.tone()), false, &plain);
            if used + 3 + width(&plain) <= self.ui.width {
                let last = lines.last_mut().expect("title line");
                last.push_str(&format!("   {styled}"));
                used += 3 + width(&plain);
            } else {
                lines.push(format!(" {styled}"));
                used = 1 + width(&plain);
            }
        }
        let title = lines.join("\n");
        let body = self.blocks(&sec.body);
        if body.is_empty() {
            format!("{title}\n")
        } else {
            format!("{title}\n\n{body}")
        }
    }

    fn callout(&self, text: &str) -> String {
        let sym = callout_symbol(self.ui.icons);
        let room = self.ui.width.saturating_sub(INDENT + width(sym) + 2);
        let mut s = String::new();
        for (i, line) in wrap(&self.fold(text), room, self.ui.ellipsis())
            .iter()
            .enumerate()
        {
            let lead = if i == 0 {
                self.p(Some(Tone::Warn), false, sym)
            } else {
                " ".repeat(width(sym))
            };
            s.push_str(&format!("{}{lead} {line}\n", " ".repeat(INDENT)));
        }
        s
    }

    fn attention(&self, lines: &[AttentionLine]) -> String {
        let syms: Vec<&str> = lines
            .iter()
            .map(|l| state_symbol(self.ui.icons, l.state))
            .collect();
        let sym_w = syms.iter().map(|s| width(s)).max().unwrap_or(0);
        let text_w = lines
            .iter()
            .map(|l| width(&self.fold(&l.text)))
            .max()
            .unwrap_or(0);
        let detail_w = lines
            .iter()
            .map(|l| width(&self.fold(&l.detail)))
            .max()
            .unwrap_or(0);
        let indent = " ".repeat(INDENT + 2);
        let mut out = String::new();
        for (l, sym) in lines.iter().zip(&syms) {
            let lead = format!(
                "{indent}{}{} ",
                self.p(Some(l.state.tone()), false, sym),
                " ".repeat(sym_w - width(sym))
            );
            let text = self.fold(&l.text);
            let detail = self.fold(&l.detail);
            let hint = self.hint_text(&l.hint);
            let wide = INDENT + 2 + sym_w + 1 + text_w + 2 + detail_w + 2 + width(&l.hint) + 4
                <= self.ui.width;
            if !self.ui.compact() && wide {
                out.push_str(&format!(
                    "{lead}{}  {}  {hint}\n",
                    pad(&text, text_w),
                    self.muted(&pad(&detail, detail_w)),
                ));
            } else {
                out.push_str(&format!("{lead}{text}\n"));
                let pad_in = " ".repeat(INDENT + 2 + sym_w + 1);
                if !detail.is_empty() {
                    out.push_str(&format!("{pad_in}{}\n", self.muted(&detail)));
                }
                out.push_str(&format!("{pad_in}{hint}\n"));
            }
        }
        out
    }

    fn collapsed(&self, state: State, text: &str, aside: &Option<String>) -> String {
        let sym = state_symbol(self.ui.icons, state);
        let room = self.ui.width.saturating_sub(INDENT + width(sym) + 2);
        let text = truncate_end(&self.fold(text), room, self.ui.ellipsis());
        let mut line = format!(
            "{}{} {}",
            " ".repeat(INDENT),
            self.p(Some(state.tone()), false, sym),
            self.muted(&text)
        );
        if let Some(a) = aside.as_ref().map(|a| self.fold(a)) {
            let used = INDENT + width(sym) + 1 + width(&text);
            if !self.ui.compact() && used + 8 + width(&a) <= self.ui.width {
                line.push_str(&" ".repeat(8));
                line.push_str(&self.muted(&a));
            }
        }
        line.push('\n');
        line
    }

    fn text(&self, t: &str) -> String {
        let room = self.ui.width.saturating_sub(INDENT + 1);
        wrap(&self.fold(t), room, self.ui.ellipsis())
            .iter()
            .map(|l| format!("{}{l}\n", " ".repeat(INDENT)))
            .collect()
    }

    fn code(&self, title: &Option<String>, lines: &[String]) -> String {
        let mut s = String::new();
        if let Some(t) = title {
            s.push_str(&format!(" {}\n", self.p(None, true, t)));
        }
        for l in lines {
            s.push_str(&format!("{}{l}\n", " ".repeat(INDENT)));
        }
        s
    }

    // ---- cells

    /// The visible text of a cell (arrows and separators follow the icon set).
    fn plain_text(&self, c: &Cell) -> String {
        match c.style {
            Style::Version => {
                let (from, to) = c.text.split_once('\u{1f}').unwrap_or(("", &c.text));
                format!(
                    "{} {} {}",
                    self.fold(from),
                    arrow(self.ui.icons),
                    self.fold(to)
                )
            }
            _ => self.fold(&c.text),
        }
    }

    /// Fit `c` into `max` cells: paths lose their middle, everything else its end.
    fn fit(&self, c: &Cell, max: usize) -> String {
        let t = self.plain_text(c);
        match c.style {
            Style::Path => truncate_middle(&t, max, self.ui.ellipsis()),
            _ => truncate_end(&t, max, self.ui.ellipsis()),
        }
    }

    /// Style already-fitted text for `c`; `bold` is for the thing the row is about.
    fn style_cell(&self, c: &Cell, fitted: &str, default: Style, bold: bool) -> String {
        let style = if c.style == Style::Plain {
            default
        } else {
            c.style
        };
        let text = match style {
            Style::Version if fitted == self.plain_text(c) => self.version(c),
            Style::Muted => self.muted(fitted),
            Style::Accent => self.p(Some(Tone::Accent), true, fitted),
            Style::Tone(t) => self.p(Some(t), false, fitted),
            Style::Version | Style::Path | Style::Plain => self.p(None, bold, fitted),
        };
        match &c.link {
            Some(l) if !fitted.is_empty() && self.ui.hyperlinks => hyperlink(self.ui, l, &text),
            // Links that are not clickable are underlined so they read as URLs.
            Some(l)
                if !fitted.is_empty() && self.ui.tier != Tier::Plain && l.starts_with("http") =>
            {
                format!("\x1b[4m{text}\x1b[24m")
            }
            _ => text,
        }
    }

    /// `from → new`, with the first changed component of `new` onward in bold.
    fn version(&self, c: &Cell) -> String {
        let (from, to) = c.text.split_once('\u{1f}').unwrap_or(("", &c.text));
        let f: Vec<&str> = from.split('.').collect();
        let t: Vec<&str> = to.split('.').collect();
        let first = t.iter().enumerate().position(|(i, x)| f.get(i) != Some(x));
        let to_styled = match first {
            Some(0) => self.p(None, true, to),
            Some(i) => format!(
                "{}.{}",
                t[..i].join("."),
                self.p(None, true, &t[i..].join("."))
            ),
            None => to.to_string(),
        };
        format!("{from} {} {to_styled}", arrow(self.ui.icons))
    }

    // ---- tables

    fn lead(&self, lead: Lead) -> LeadText {
        match lead {
            Lead::None => LeadText {
                symbol: String::new(),
                word: String::new(),
                tone: None,
            },
            Lead::State(s) => LeadText {
                symbol: state_symbol(self.ui.icons, s).to_string(),
                word: s.word().to_string(),
                tone: Some(s.tone()),
            },
            Lead::Marker(m) => LeadText {
                symbol: marker_symbol(self.ui.icons, m).to_string(),
                word: String::new(),
                tone: Some(marker_tone(m)),
            },
        }
    }

    fn table(&self, t: &Table) -> String {
        if t.rows.is_empty() {
            return String::new();
        }
        if self.ui.compact() {
            return self.table_compact(t);
        }
        let leads: Vec<LeadText> = t.rows.iter().map(|r| self.lead(r.lead)).collect();
        let sym_w = leads.iter().map(|l| width(&l.symbol)).max().unwrap_or(0);
        let word_w = leads.iter().map(|l| width(&l.word)).max().unwrap_or(0);
        let lead_w = if word_w > 0 {
            sym_w + 1 + word_w
        } else {
            sym_w
        };
        let lead_cols = if lead_w > 0 { lead_w + 2 } else { 0 };

        let ncols = t
            .columns
            .len()
            .max(t.rows.iter().map(|r| r.cells.len()).max().unwrap_or(0));
        let texts: Vec<Vec<String>> = t
            .rows
            .iter()
            .map(|r| {
                (0..ncols)
                    .map(|i| {
                        r.cells
                            .get(i)
                            .map(|c| self.plain_text(c))
                            .unwrap_or_default()
                    })
                    .collect()
            })
            .collect();
        let mut col_w: Vec<usize> = (0..ncols)
            .map(|i| texts.iter().map(|r| width(&r[i])).max().unwrap_or(0))
            .collect();
        let mut name_w = t
            .rows
            .iter()
            .map(|r| width(&self.plain_text(&r.name)))
            .max()
            .unwrap_or(0);

        // Drop the lowest-priority columns (rightmost first on ties) until the row fits.
        let prio = |i: usize| t.columns.get(i).map_or(0, |c| c.priority);
        let mut kept: Vec<usize> = (0..ncols).filter(|i| col_w[*i] > 0).collect();
        let cols_w =
            |kept: &[usize], col_w: &[usize]| -> usize { kept.iter().map(|i| 2 + col_w[*i]).sum() };
        while INDENT + lead_cols + name_w + cols_w(&kept, &col_w) > self.ui.width {
            let Some(drop) = kept
                .iter()
                .copied()
                .min_by_key(|i| (prio(*i), std::cmp::Reverse(*i)))
            else {
                break;
            };
            kept.retain(|i| *i != drop);
        }
        // Still too wide: the name shrinks (middle-truncated for paths), never below 8 cells.
        let fixed = INDENT + lead_cols + cols_w(&kept, &col_w);
        if fixed + name_w > self.ui.width {
            name_w = self.ui.width.saturating_sub(fixed).max(8).min(name_w);
        }
        // Whatever is kept but cannot fit next to the name gets an ellipsis too.
        let spare = self.ui.width.saturating_sub(INDENT + lead_cols + name_w);
        if let Some(&last) = kept.last() {
            let others = cols_w(&kept, &col_w) - 2 - col_w[last];
            col_w[last] = col_w[last].min(spare.saturating_sub(others + 2).max(4));
        }

        let mut out = String::new();
        for (r, (row, lead)) in t.rows.iter().zip(&leads).enumerate() {
            let mut line = " ".repeat(INDENT);
            if lead_w > 0 {
                line.push_str(&self.p(lead.tone, false, &lead.symbol));
                line.push_str(&" ".repeat(sym_w - width(&lead.symbol)));
                if word_w > 0 {
                    line.push(' ');
                    line.push_str(&self.p(lead.tone, false, &lead.word));
                    line.push_str(&" ".repeat(word_w - width(&lead.word)));
                }
                line.push_str("  ");
            }
            let fitted = self.fit(&row.name, name_w);
            line.push_str(&self.style_cell(&row.name, &fitted, Style::Accent, true));
            line.push_str(&" ".repeat(name_w.saturating_sub(width(&fitted))));
            for &i in &kept {
                line.push_str("  ");
                let default = t.columns.get(i).map_or(Style::Plain, |c| c.style);
                match row.cells.get(i) {
                    Some(c) if !texts[r][i].is_empty() => {
                        let fitted = self.fit(c, col_w[i]);
                        line.push_str(&self.style_cell(c, &fitted, default, false));
                        line.push_str(&" ".repeat(col_w[i].saturating_sub(width(&fitted))));
                    }
                    _ => line.push_str(&" ".repeat(col_w[i])),
                }
            }
            out.push_str(line.trim_end());
            out.push('\n');
            for h in row.hints.iter().take(2) {
                out.push_str(&format!(
                    "{}{}\n",
                    " ".repeat(INDENT + lead_cols),
                    self.hint_text(h)
                ));
            }
        }
        out
    }

    /// Below 60 columns: stacked rows, name and state never dropped (UX §5.1).
    fn table_compact(&self, t: &Table) -> String {
        let mut out = String::new();
        let sep = format!(" {} ", self.sep());
        for row in &t.rows {
            let lead = self.lead(row.lead);
            let room = self
                .ui
                .width
                .saturating_sub(INDENT + width(&lead.symbol) + 1);
            let name = self.fit(&row.name, room.max(8));
            out.push(' ');
            if !lead.symbol.is_empty() {
                out.push_str(&self.p(lead.tone, false, &lead.symbol));
                out.push(' ');
            }
            out.push_str(&self.style_cell(&row.name, &name, Style::Accent, true));
            out.push('\n');

            // Second line: state word first (so monochrome still names the state), then details.
            let details: Vec<String> = row
                .cells
                .iter()
                .map(|c| self.plain_text(c))
                .filter(|s| !s.is_empty())
                .collect();
            let room = self.ui.width.saturating_sub(INDENT + 1);
            let tail = truncate_end(
                &details.join(&sep),
                room.saturating_sub(if lead.word.is_empty() {
                    0
                } else {
                    width(&lead.word) + 3
                }),
                self.ui.ellipsis(),
            );
            let mut second = String::new();
            if !lead.word.is_empty() {
                second.push_str(&self.p(lead.tone, false, &lead.word));
                if !tail.is_empty() {
                    second.push_str(&self.muted(&format!("{sep}{tail}")));
                }
            } else if !tail.is_empty() {
                second.push_str(&self.muted(&tail));
            }
            if !second.is_empty() {
                out.push_str(&format!("{}{second}\n", " ".repeat(INDENT)));
            }
            for h in row.hints.iter().take(2) {
                out.push_str(&format!("{}{}\n", " ".repeat(INDENT), self.hint_text(h)));
            }
        }
        out
    }

    // ---- facts and trees

    fn facts(&self, facts: &[(String, Cell)]) -> String {
        let key_w = facts.iter().map(|(k, _)| width(k)).max().unwrap_or(0);
        let mut out = String::new();
        for (k, v) in facts {
            if self.ui.compact() {
                out.push_str(&format!("{}{}\n", " ".repeat(INDENT), self.muted(k)));
                let fitted = self.fit(v, self.ui.width.saturating_sub(INDENT + 2));
                out.push_str(&format!(
                    "{}{}\n",
                    " ".repeat(INDENT + 2),
                    self.style_cell(v, &fitted, Style::Plain, false)
                ));
            } else {
                let fitted = self.fit(v, self.ui.width.saturating_sub(INDENT + key_w + 2));
                let line = format!(
                    "{}{}  {}",
                    " ".repeat(INDENT),
                    self.muted(&pad(k, key_w)),
                    self.style_cell(v, &fitted, Style::Plain, false)
                );
                out.push_str(line.trim_end());
                out.push('\n');
            }
        }
        out
    }

    fn tree(&self, roots: &[TreeNode]) -> String {
        fn walk<'n>(
            nodes: &'n [TreeNode],
            prefix: &str,
            g: &TreeGlyphs,
            out: &mut Vec<(String, &'n TreeNode)>,
        ) {
            for (i, n) in nodes.iter().enumerate() {
                let last = i + 1 == nodes.len();
                let (own, child) = if last {
                    (g.last, g.blank)
                } else {
                    (g.branch, g.bar)
                };
                out.push((format!("{prefix}{own}"), n));
                walk(&n.children, &format!("{prefix}{child}"), g, out);
            }
        }
        let g = tree_glyphs(self.ui.icons);
        // (prefix, node) in print order, so the notes can share a column.
        let mut flat: Vec<(String, &TreeNode)> = Vec::new();
        for root in roots {
            flat.push((String::new(), root));
            walk(&root.children, "", &g, &mut flat);
        }
        let note_col = flat
            .iter()
            .map(|(p, n)| width(p) + width(&self.plain_text(&n.label)))
            .max()
            .unwrap_or(0)
            + 2;
        let mut out = String::new();
        for (prefix, n) in &flat {
            let room = self.ui.width.saturating_sub(INDENT + width(prefix));
            let label = self.fit(&n.label, room.max(8));
            let mut line = format!(
                "{}{}{}",
                " ".repeat(INDENT),
                self.muted(prefix),
                self.style_cell(&n.label, &label, Style::Plain, false)
            );
            if let Some(note) = &n.note {
                let used = width(prefix) + width(&label);
                let col = note_col.max(used + 2);
                let room = self.ui.width.saturating_sub(INDENT + col);
                if room >= 6 {
                    line.push_str(&" ".repeat(col - used));
                    line.push_str(&self.muted(&truncate_end(
                        &self.fold(note),
                        room,
                        self.ui.ellipsis(),
                    )));
                }
            }
            out.push_str(&line);
            out.push('\n');
        }
        out
    }
}

/// Blank line between blocks, except runs of hints or callouts and a headline directly above a
/// collapsed count.
fn gap_between(prev: &Block, next: &Block) -> bool {
    !matches!(
        (prev, next),
        (Block::Hint(_), Block::Hint(_))
            | (Block::Callout(_), Block::Callout(_))
            | (Block::Headline { .. }, Block::Collapsed { .. })
            | (Block::Headline { .. }, Block::Attention(_))
            | (Block::Collapsed { .. }, Block::Collapsed { .. })
    )
}
