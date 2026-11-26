# Spec Delta

## Purpose

Defines how `dots` renders human output and asks questions: capability tiers, the shared state vocabulary, hints, prompts, and paging. Output then stays readable for a newcomer, scriptable for a power user, and accessible to everyone.

## ADDED Requirements

### Requirement: Rendering tiers
Output SHALL render in one of three tiers:
- **Plain** is used when stdout is not a TTY, or when `NO_COLOR`, `TERM=dumb`, `CI` or `DOTS_UI=plain` is set. It has no ANSI, ASCII symbols and no animation.
- **Basic** is used on a 16-color TTY.
- **Rich** is used when truecolor is detected. It adds hyperlinks, terminal progress and synchronized redraws.

`--color` and `DOTS_UI` SHALL force a tier.

#### Scenario: Piped output
- **WHEN** `dots status | cat` runs
- **THEN** the output has no escape sequences and uses ASCII state markers such as `[ok]` and `[!!]`

#### Scenario: Forced color
- **WHEN** `dots status --color always | less -R` runs
- **THEN** ANSI colors are emitted

### Requirement: Shared state vocabulary
Each state word SHALL always appear with the same symbol and color token, in every command and in JSON.
- Package states: `ok`, `outdated`, `missing`, `drift`, `orphan`, `unmanaged`, `excluded`, `n/a`, `pending-restart`, `blocked-by-policy`.
- File states: `ok`, `missing`, `conflict`, `modified`, `stale`, `dangling`, `foreign-entries`, `tampered`.
- Todo states: `pending`, `done`, `skipped`, `auto`.

No state SHALL be conveyed by color alone.

#### Scenario: Monochrome readability
- **WHEN** `dots status` renders with `NO_COLOR=1`
- **THEN** every non-ok row contains both a symbol and the state word

### Requirement: Answer first, quiet success
Every command's first output line SHALL answer its question, for example "All good", "3 things need attention" or "Plan: 4 installs · 2 file changes". Healthy items SHALL collapse into counts. Only non-ok items SHALL be listed by default.

#### Scenario: Healthy machine
- **WHEN** everything is ok
- **THEN** `dots` prints one headline plus a single collapsed count line, and lists no rows

### Requirement: Every problem has a fix hint
Every non-ok row and every error SHALL end with a hint naming the exact command or edit that resolves it.

#### Scenario: Modified config
- **WHEN** `~/.claude/settings.json` is `modified`
- **THEN** its row ends with `→ dots adopt ~/.claude/settings.json`

### Requirement: Prompts with recommended defaults
Interactive prompts SHALL show the recommended answer as the default, so pressing Enter accepts it.

Prompts SHALL never appear when:
- `--no-input` is set;
- `--yes` covers the confirmation;
- stdin is not a TTY. In that case the command SHALL fail with a hint instead of hanging.

#### Scenario: Non-TTY confirmation
- **WHEN** `echo | dots apply` needs confirmation
- **THEN** it exits 1 with "confirmation required: pass --yes"

### Requirement: Progress and responsiveness
Output SHALL appear within 50 ms. Any wait longer than 200 ms SHALL show what it is waiting for, for example "asking brew…".

Animation SHALL be disabled when:
- the tier is Plain;
- `DOTS_REDUCE_MOTION` is set.

#### Scenario: Reduced motion
- **WHEN** `DOTS_REDUCE_MOTION=1 dots apply --yes` runs
- **THEN** no spinner frames are printed, only one line per state change

### Requirement: Pager and long content
Diffs, rendered manual actions and script previews longer than the screen SHALL open in `$PAGER`. The default pager is `less -R`; `ui.toml` `pager` can change it. In the Plain tier they SHALL print inline.

#### Scenario: Viewing a script before confirming
- **WHEN** the user presses `v` on a "Code that will run" row
- **THEN** the full script opens in the pager, and the confirmation prompt returns afterwards

### Requirement: Responsive width
Layouts SHALL adapt to the terminal width. Below 60 columns, the compact layout SHALL be used. Content SHALL never be truncated without an ellipsis.

#### Scenario: Narrow terminal
- **WHEN** the terminal is 50 columns wide
- **THEN** `dots status` uses the compact layout, with no lines wrapped mid-column

### Requirement: UI preferences file
`~/.config/dots/ui.toml` SHALL accept these keys:
- `color = auto|always|never`
- `icons = unicode|nerd|ascii`
- `theme = auto|dark|light`
- `hyperlinks`, `notify`, `pager`

An unknown key SHALL be a `schema-unknown-key` error.

#### Scenario: Nerd icons opt-in
- **WHEN** `ui.toml` sets `icons = "nerd"`
- **THEN** state symbols use Nerd Font glyphs, and the state words stay
