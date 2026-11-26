//! Presentation blocks (design D1): what commands put in a `Report` so the renderers can draw
//! it. Blocks never appear in the JSON envelope; `Report::data` stays the machine contract.
//!
//! Commands describe *what* to show (state rows, hints, sections, trees); the renderers in
//! `dots-ui` decide symbols, colors, widths and the compact layout.

/// A semantic color token (UX §3.1). Renderers map tokens to colors, never raw values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Ok,
    Warn,
    Err,
    Info,
    Accent,
    Muted,
    Hint,
}

/// The shared state vocabulary: package, file and todo states. A state always renders with the
/// same word, symbol and color, in every command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    // package states
    Ok,
    Outdated,
    Missing,
    Drift,
    Orphan,
    Unmanaged,
    Excluded,
    NotApplicable,
    PendingRestart,
    BlockedByPolicy,
    // file states
    Conflict,
    Modified,
    Stale,
    Dangling,
    ForeignEntries,
    Tampered,
    // todo states
    Pending,
    Done,
    Skipped,
    Auto,
    // transient
    Failed,
    Running,
}

impl State {
    pub const ALL: [State; 22] = [
        State::Ok,
        State::Outdated,
        State::Missing,
        State::Drift,
        State::Orphan,
        State::Unmanaged,
        State::Excluded,
        State::NotApplicable,
        State::PendingRestart,
        State::BlockedByPolicy,
        State::Conflict,
        State::Modified,
        State::Stale,
        State::Dangling,
        State::ForeignEntries,
        State::Tampered,
        State::Pending,
        State::Done,
        State::Skipped,
        State::Auto,
        State::Failed,
        State::Running,
    ];

    /// The state word, identical to the JSON value.
    pub const fn word(self) -> &'static str {
        match self {
            State::Ok => "ok",
            State::Outdated => "outdated",
            State::Missing => "missing",
            State::Drift => "drift",
            State::Orphan => "orphan",
            State::Unmanaged => "unmanaged",
            State::Excluded => "excluded",
            State::NotApplicable => "n/a",
            State::PendingRestart => "pending-restart",
            State::BlockedByPolicy => "blocked-by-policy",
            State::Conflict => "conflict",
            State::Modified => "modified",
            State::Stale => "stale",
            State::Dangling => "dangling",
            State::ForeignEntries => "foreign-entries",
            State::Tampered => "tampered",
            State::Pending => "pending",
            State::Done => "done",
            State::Skipped => "skipped",
            State::Auto => "auto",
            State::Failed => "failed",
            State::Running => "running",
        }
    }

    pub fn from_word(word: &str) -> Option<State> {
        State::ALL.into_iter().find(|s| s.word() == word)
    }

    pub const fn tone(self) -> Tone {
        match self {
            State::Ok | State::Done => Tone::Ok,
            State::Outdated
            | State::Drift
            | State::Modified
            | State::Pending
            | State::Stale
            | State::PendingRestart => Tone::Warn,
            State::Missing
            | State::Failed
            | State::Conflict
            | State::Dangling
            | State::ForeignEntries
            | State::Tampered
            | State::BlockedByPolicy => Tone::Err,
            State::Orphan
            | State::Unmanaged
            | State::Excluded
            | State::NotApplicable
            | State::Skipped
            | State::Auto => Tone::Muted,
            State::Running => Tone::Info,
        }
    }
}

/// Plan and diff markers (UX §4.4, §4.10).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Marker {
    Add,
    Change,
    Remove,
    /// Already on the machine and satisfies the spec.
    Adopt,
    Upgrade,
    /// A copy dots does not control would compete on PATH.
    Shadow,
    /// Replaced, with a backup first.
    Replace,
    Blocked,
}

/// What leads a row: a state (symbol and word), a plan marker, or nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lead {
    None,
    State(State),
    Marker(Marker),
}

impl From<State> for Lead {
    fn from(s: State) -> Self {
        Lead::State(s)
    }
}
impl From<Marker> for Lead {
    fn from(m: Marker) -> Self {
        Lead::Marker(m)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Style {
    Plain,
    Muted,
    /// Names of packages, bundles and settings.
    Accent,
    /// Truncated in the middle; hyperlinked to `file://` where supported.
    Path,
    /// `old → new` with the changed part of the new version in bold.
    Version,
    Tone(Tone),
}

/// One styled piece of text, optionally a hyperlink target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cell {
    pub text: String,
    pub style: Style,
    pub link: Option<String>,
}

impl Cell {
    pub fn new(text: impl Into<String>, style: Style) -> Self {
        Cell {
            text: text.into(),
            style,
            link: None,
        }
    }
    pub fn muted(text: impl Into<String>) -> Self {
        Cell::new(text, Style::Muted)
    }
    pub fn accent(text: impl Into<String>) -> Self {
        Cell::new(text, Style::Accent)
    }
    pub fn path(text: impl Into<String>) -> Self {
        Cell::new(text, Style::Path)
    }
    pub fn tone(text: impl Into<String>, tone: Tone) -> Self {
        Cell::new(text, Style::Tone(tone))
    }
    /// `from → new`; the first changed version component onward is bold.
    pub fn version(from: &str, to: &str) -> Self {
        Cell::new(format!("{from}\u{1f}{to}"), Style::Version)
    }
    pub fn link(mut self, target: impl Into<String>) -> Self {
        self.link = Some(target.into());
        self
    }
}

impl From<&str> for Cell {
    fn from(s: &str) -> Self {
        Cell::new(s, Style::Plain)
    }
}
impl From<String> for Cell {
    fn from(s: String) -> Self {
        Cell::new(s, Style::Plain)
    }
}

#[derive(Debug, Clone)]
pub struct Column {
    pub header: String,
    /// Higher stays longer. Columns with the lowest priority are dropped first as the terminal
    /// narrows. The lead and the name are never dropped.
    pub priority: u8,
    pub style: Style,
}

/// A state/plan row: lead, name, extra cells, then up to two fix hints.
#[derive(Debug, Clone)]
pub struct Row {
    pub lead: Lead,
    pub name: Cell,
    pub cells: Vec<Cell>,
    pub hints: Vec<String>,
}

impl Row {
    pub fn new(lead: impl Into<Lead>, name: impl Into<String>) -> Self {
        Row {
            lead: lead.into(),
            name: Cell::accent(name),
            cells: Vec::new(),
            hints: Vec::new(),
        }
    }
    pub fn named(lead: impl Into<Lead>, name: Cell) -> Self {
        Row {
            lead: lead.into(),
            name,
            cells: Vec::new(),
            hints: Vec::new(),
        }
    }
    pub fn cell(mut self, c: impl Into<Cell>) -> Self {
        self.cells.push(c.into());
        self
    }
    /// Append a fix hint: the exact command or edit, without the arrow.
    pub fn hint(mut self, h: impl Into<String>) -> Self {
        self.hints.push(h.into());
        self
    }
}

#[derive(Debug, Clone, Default)]
pub struct Table {
    pub columns: Vec<Column>,
    pub rows: Vec<Row>,
}

impl Table {
    pub fn new() -> Self {
        Table::default()
    }
    /// A column after the name, in the table's declared order. Cells are positional.
    pub fn column(mut self, header: &str, priority: u8) -> Self {
        self.columns.push(Column {
            header: header.to_string(),
            priority,
            style: Style::Plain,
        });
        self
    }
    /// Like [`Table::column`] with a default style for cells that do not set their own.
    pub fn styled_column(mut self, header: &str, priority: u8, style: Style) -> Self {
        self.columns.push(Column {
            header: header.to_string(),
            priority,
            style,
        });
        self
    }
    pub fn row(mut self, r: Row) -> Self {
        self.rows.push(r);
        self
    }
    pub fn push(&mut self, r: Row) {
        self.rows.push(r);
    }
}

#[derive(Debug, Clone)]
pub struct TreeNode {
    pub label: Cell,
    pub note: Option<String>,
    pub children: Vec<TreeNode>,
}

impl TreeNode {
    pub fn new(label: impl Into<Cell>) -> Self {
        TreeNode {
            label: label.into(),
            note: None,
            children: Vec::new(),
        }
    }
    pub fn note(mut self, n: impl Into<String>) -> Self {
        self.note = Some(n.into());
        self
    }
    pub fn child(mut self, c: TreeNode) -> Self {
        self.children.push(c);
        self
    }
}

#[derive(Debug, Clone)]
pub struct Section {
    pub title: String,
    /// Counts after the title; zero counts are omitted when rendering.
    pub pills: Vec<(State, usize)>,
    pub body: Vec<Block>,
}

impl Section {
    pub fn new(title: &str) -> Self {
        Section {
            title: title.to_string(),
            pills: Vec::new(),
            body: Vec::new(),
        }
    }
    pub fn pill(mut self, state: State, count: usize) -> Self {
        self.pills.push((state, count));
        self
    }
    pub fn block(mut self, b: impl Into<Block>) -> Self {
        self.body.push(b.into());
        self
    }
}

/// One line of the `dots` dashboard: a state symbol, a count sentence, the names it covers
/// (already shortened) and the command that fixes them.
#[derive(Debug, Clone)]
pub struct AttentionLine {
    pub state: State,
    pub text: String,
    pub detail: String,
    pub hint: String,
}

#[derive(Debug, Clone)]
pub enum Block {
    /// `dots status   ~/dotfiles · main @ f452241 · …` (UX §5 context bar).
    Context {
        title: String,
        parts: Vec<String>,
    },
    /// Symbol plus one sentence answering the command's question.
    Headline {
        tone: Tone,
        text: String,
    },
    Section(Section),
    Table(Table),
    /// Key/value layout with a muted key column.
    Facts(Vec<(String, Cell)>),
    Tree(Vec<TreeNode>),
    /// The dashboard's grouped attention lines (UX 4.1), columns aligned.
    Attention(Vec<AttentionLine>),
    /// A standalone `→ command` line.
    Hint(String),
    /// `▲` sentence for a risk. Shown before prompts.
    Callout(String),
    /// A collapsed run of healthy items: `✓ 62 packages ok` with an optional right-hand aside.
    Collapsed {
        state: State,
        text: String,
        aside: Option<String>,
    },
    Text(String),
    /// Preformatted lines (diffs, scripts, manual actions): shown inline, or in the pager when
    /// longer than the screen.
    Code {
        title: Option<String>,
        lines: Vec<String>,
    },
    Blank,
}

impl From<Section> for Block {
    fn from(s: Section) -> Self {
        Block::Section(s)
    }
}
impl From<Table> for Block {
    fn from(t: Table) -> Self {
        Block::Table(t)
    }
}

impl Block {
    pub fn headline(tone: Tone, text: impl Into<String>) -> Block {
        Block::Headline {
            tone,
            text: text.into(),
        }
    }
    pub fn hint(cmd: impl Into<String>) -> Block {
        Block::Hint(cmd.into())
    }
    pub fn callout(text: impl Into<String>) -> Block {
        Block::Callout(text.into())
    }
    pub fn text(text: impl Into<String>) -> Block {
        Block::Text(text.into())
    }
    pub fn collapsed(state: State, text: impl Into<String>) -> Block {
        Block::Collapsed {
            state,
            text: text.into(),
            aside: None,
        }
    }
    pub fn facts<K: Into<String>, V: Into<Cell>>(pairs: impl IntoIterator<Item = (K, V)>) -> Block {
        Block::Facts(
            pairs
                .into_iter()
                .map(|(k, v)| (k.into(), v.into()))
                .collect(),
        )
    }
}

/// "1 package", "2 packages": always-correct plurals (UX §3.3).
pub fn plural(n: usize, singular: &str, plural: &str) -> String {
    format!("{n} {}", if n == 1 { singular } else { plural })
}
