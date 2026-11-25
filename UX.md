# dots: terminal output and interaction design

Status: draft v1 (2026-11-25). This is a companion to `ARCHITECTURE.md` and governs everything the binary prints or asks.

The bar: someone who has never heard of dotfiles runs `dots` on a new laptop, and without reading docs they understand what happened, what's wrong and what to type next. A power user gets the same output and finds it dense, fast and scriptable.

---

## 1. Principles

1. **Answer first.** The first line of every command answers its question: "All good", "3 things need attention", "Plan: 4 installs, 2 file changes". Details come after.
2. **Quiet success, loud problems.** Healthy items collapse into a count (`62 ok`). Problems are listed one per row, never buried.
3. **Every problem ends with its fix.** A non-ok row carries a hint line with the exact command to run (`→ dots adopt ~/.claude/settings.json`). No row says something is wrong without saying what to do about it.
4. **Never color-only.** Every state has a symbol **and** a word. The output must read correctly in monochrome, to a colour-blind user and to a screen reader.
5. **One vocabulary.** The same state words, symbols and colors in `status`, `plan`, `apply`, `bins`, `log` and `--json`. `outdated` means the same thing everywhere.
6. **Progressive disclosure.** Summary (`dots`) → area (`dots status`) → item (`dots status ghostty`) → raw (`-v`, log files). Each level links to the next.
7. **Commands never print.** They return a typed `Report`. Renderers turn it into rich text, plain text or JSON (§9). This makes output consistent, and testable by construction.
8. **Fast feedback.** Something is on screen within 50 ms. Anything slower than 200 ms shows a spinner with what it's waiting for ("asking brew…").

---

## 2. Capability tiers (rich where possible, never broken)

`dots` detects the terminal once at startup and renders to the best tier it supports. Every component in §5 has a defined rendering for each tier.

| Tier | When | What you get |
|------|------|--------------|
| **Plain** | not a TTY, `NO_COLOR`, `TERM=dumb`, `CI`, `DOTS_UI=plain` | no ANSI, ASCII symbols (`[ok]`, `[!!]`), no spinners, one line per event, stable for grep and logs |
| **Basic** | TTY with 16 colors (legacy conhost, old xterm, tmux without RGB) | 16-color palette, Unicode symbols, spinners, live region |
| **Rich** | `COLORTERM=truecolor`/`24bit`, or a known modern terminal | truecolor palette tuned for light/dark, OSC 8 hyperlinks, progress in the terminal's own progress indicator (OSC 9;4), synchronized output (mode 2026, flicker-free redraws), window title, desktop notification |

Detection details, each of which matters:

- Color: `NO_COLOR` (any value disables), `FORCE_COLOR` / `CLICOLOR_FORCE` (enable even when piped), then `COLORTERM`, then the `TERM` terminfo `colors` value. On Windows, enable VT mode via `SetConsoleMode`. If that fails, the terminal is legacy conhost and gets Basic.
- **Light vs dark background:** query OSC 11 (background color) with a 50 ms timeout, falling back to `COLORFGBG` and then to dark. Every color token has a light and a dark value. Yellow-on-white is unreadable, so that is the reason this exists.
- **Hyperlinks** (OSC 8) are enabled for ghostty, kitty, WezTerm, iTerm2, VS Code, Windows Terminal, foot, Konsole ≥ 22 and GNOME Terminal (VTE ≥ 0.50). Inside tmux they are passed through only if the client terminal supports them (`tmux -V` ≥ 3.4 with `allow-passthrough`). Otherwise the URL/path is printed as visible text.
- **Unicode**: assumed under a UTF-8 locale. With a non-UTF-8 locale, ASCII symbols are used.
- **Icons**: `ui.icons = "unicode"` (default; works in every font), `"nerd"` (opt-in Nerd Font glyphs, for users like you who already use Nerdwand Mono) or `"ascii"`.
- **Width**: read on every redraw (SIGWINCH). Layouts are responsive (§5.1). Below 60 columns, the compact layout is used.
- **Motion**: `DOTS_REDUCE_MOTION=1` or Plain tier → no spinners or animated bars, only state-change lines.

Overrides live in `~/.config/dots/ui.toml` (`color = auto|always|never`, `icons`, `theme = auto|dark|light`, `hyperlinks`, `notify`, `pager`).

---

## 3. Visual language

### 3.1 Semantic color tokens

Renderers never use raw colors, only tokens. Each token maps to truecolor (light/dark) and a 16-color fallback.

| Token | Dark bg | Light bg | 16-color | Used for |
|-------|---------|----------|----------|----------|
| `ok` | `#7ee787` | `#1a7f37` | green | healthy, success |
| `warn` | `#e3b341` | `#9a6700` | yellow | outdated, drift, pending, modified |
| `err` | `#ff7b72` | `#cf222e` | red | missing, failed, conflict |
| `info` | `#79c0ff` | `#0969da` | blue | running, neutral notices |
| `accent` | `#d2a8ff` | `#8250df` | magenta | package, bundle and setting names |
| `muted` | `#8b949e` | `#6e7781` | bright black / dim | metadata, paths, timestamps, collapsed counts |
| `add` / `remove` / `change` | ok / err / warn | | | plan and diff markers |
| `hint` | `#56d4dd` | `#0a7f8a` | cyan | `→` next-step lines and command snippets |

Styles: **bold** for the thing the row is about (package name, file) and for the changed part of a version. *Dim* for anything secondary. Underline only for hyperlinks, and only where hyperlinks are off (so the user knows it is a URL). No italics, because too many terminals and fonts render them badly. No blinking, no background fills except the single `dots` badge in §4.1.

### 3.2 State symbols

One symbol per state, used everywhere. The word always appears next to the symbol in lists.

| State | Unicode | Nerd | ASCII | Token |
|-------|---------|------|-------|-------|
| ok | `✓` | `` | `[ok]` | ok |
| outdated | `↑` | `` | `[up]` | warn |
| drift | `≠` | `` | `[!=]` | warn |
| modified | `✎` | `` | `[mod]` | warn |
| pending (todo) | `○` | `` | `[ ]` | warn |
| done (todo) | `●` | `` | `[x]` | ok |
| missing | `✗` | `` | `[--]` | err |
| failed | `✗` | `` | `[FAIL]` | err |
| conflict | `⚡` | `` | `[!!]` | err |
| orphan | `◌` | `` | `[orph]` | muted |
| n/a (inert on this platform) | `·` | `` | `[n/a]` | muted |
| running | spinner `⠋⠙⠹…` | same | `[..]` | info |
| skipped | `⊘` | `` | `[skip]` | muted |
| hint | `→` | `` | `->` | hint |
| warning callout | `▲` | `` | `[!]` | warn |

Every Unicode symbol is a single cell wide (checked with `unicode-width`). Emoji are never used: their width varies by terminal and they break alignment.

### 3.3 Formatting conventions

- **Paths**: `$HOME` is shown as `~`. Overlong paths are truncated **in the middle** (`~/.config/…/settings.json`), never at the end, because the file name is the important part. The full path is in the hyperlink target.
- **Versions**: `0.9.6 → 0.10.2` with the **first changed component onward in bold** (`0.`**`10.2`**), so you can tell at a glance whether it's a patch or a major bump. Major bumps get a `warn`-colored `major` tag.
- **Times**: relative for recent ("3 min ago", "yesterday"), absolute date after 7 days ("2026-10-02"). `log` always shows absolute timestamps in the local timezone.
- **Durations**: `850ms`, `12s`, `3m 05s`. Never raw seconds past 60.
- **Counts**: words for small numbers in sentences ("two files"), digits in tables. Plurals are always correct ("1 package", "2 packages"), handled by a helper and never by `"(s)"`.
- **Commands in hints** are printed exactly as typed, in the `hint` color and without decoration, so a triple-click selects a runnable line.
- **Names** of packages, bundles and settings are always `accent`. Users learn to scan for that color.

---

## 4. Screens (mockups)

Mockup legend: `[ok]`/`[warn]`/`[err]`/`[muted]`/`[accent]`/`[hint]` prefixes in the **notes under each mockup** describe styling. The mockup itself is what's on screen.

### 4.1 Bare `dots` (the home screen)

Running `dots` with no arguments is the most common command, so it's a dashboard rather than a help page:

```
 dots  ~/dotfiles · main @ f452241 · work-mbp · macOS arm64

 ▲ 3 things need attention

   ↑ 3 updates available        bottom, delta, yazi             → dots upgrade
   ✎ 1 config changed by app    ~/.claude/settings.json         → dots adopt ~/.claude/settings.json
   ○ 2 manual steps left        karabiner permissions, …        → dots todo

   ✓ 64 packages · 86 files · 3 git sources ok            checked in 0.9s
```

Notes: the `dots` badge is `accent` bold on a subtle background (the only background fill in the app). The context line is `muted`, except `main`, which is `accent`, and a `dirty` marker, which is `warn`. The headline is `warn` when there is attention needed, and when everything is fine it is replaced by `✓ All good` in `ok`. The middle column is `muted`, truncated with "…" to fit. Hints are `hint`.

When everything is fine:

```
 dots  ~/dotfiles · main @ f452241 · work-mbp · macOS arm64

 ✓ All good. 64 packages · 86 files · 3 git sources        checked in 0.8s
```

On a machine that hasn't been set up yet, `dots` offers onboarding (§4.8) instead.

### 4.2 `dots status`

```
 dots status   ~/dotfiles · main @ f452241 (clean) · work-mbp · macOS arm64 · tags work, gui

 PACKAGES   62 ok   3 outdated   1 drift   1 missing   1 orphan

   ↑ outdated  bottom     cargo   0.9.6  → 0.10.2              released 4 days ago · changelog
   ↑ outdated  delta      cargo   0.17.0 → 0.18.2
   ↑ outdated  yazi       brew    25.4.8 → 25.5.1
   ≠ drift     neovim     github  0.11.4   lock wants 0.11.5
               → dots apply neovim
   ✗ missing   herdr      script  not installed
               → dots apply herdr
   ◌ orphan    zellij     brew    0.41.1   installed in worktree try-zellij, Nov 20
               → dots remove zellij   or   dots adopt-package zellij

 FILES      84 ok   1 modified   1 foreign entries

   ✎ modified  ~/.claude/settings.json     render   app changed 2 keys
               → dots adopt ~/.claude/settings.json      (see the change: dots diff ~/.claude/settings.json)
   ⚡ foreign   ~/.config/herdr/            link     herdr.sock, herdr-server.log, sessions/ live in your repo
               → link individual files instead: dots explain foreign-entries

 TODO       2 pending

   ○ karabiner  Grant Input Monitoring + Accessibility        needs logout
   ○ ghostty    Disable Secure Keyboard Entry
               → dots todo

 GIT        koala-config  ✎ 3 changed files · 2 commits ahead of lock
               → dots lock koala-config   (record the new rev)
```

Notes:
- Section titles are bold. The counts after a title are colored pills (`62 ok` in ok, `3 outdated` in warn, and so on). Counts of zero are omitted.
- In each row, the state word takes its state's color, the name is `accent` bold, the provider is `muted`, and the changed part of each version is bold.
- `changelog` is an OSC 8 hyperlink to the release notes. It is derived from the provider: the GitHub release, crates.io, the brew formula homepage or PyPI.
- Every path is a `file://` hyperlink, so Cmd/Ctrl-click opens it.
- Hint lines are indented under their row, so the fix is visually attached to its problem.
- With `--all`, the ok rows are listed too, grouped by bundle and with muted styling.

### 4.3 `dots status <pkg>` (deep view)

```
 ghostty   Terminal emulator                                        ✓ ok

   provider   brew (cask)        chosen because: macOS priority brew > github-release
   version    1.2.3              wants latest · locked 1.2.3 · latest 1.2.3
   bundle     mac-desktop ← base                                   (dots why ghostty)
   origin     ofirgall/dotfiles @ f452241   ✓ in sync with upstream

   settings   font_size     15        machine   (default 13)
              theme         dark      default
              cursor_shader on        repo

   overrides  repo: overrides/ghostty/ (1 extra layer)

   files      ✓ ~/.config/ghostty/config      render   3 layers · rendered 2h ago
              ✓ ~/.config/ghostty/shaders     git      ofirgall/ghostty-cursor-shaders @ 1c2d3e4

   todos      ● disable-secure-input           done Nov 12

   history    2026-11-25  upgrade 1.2.2 → 1.2.3   main @ f452241
              2026-09-14  install 1.2.0           main @ 3e9a771      (dots log ghostty)
```

Notes: this is a key/value layout with a `muted` key column. Setting values set by anything other than the default show their scope in `info` color. Defaults are `muted`.

### 4.4 `dots plan` / `dots apply` (preview)

Grouped by phase, using `+` add, `~` change, `-` remove, `⚠` risk. Callouts come **before** the confirmation prompt, never after.

```
 Plan   4 installs · 1 upgrade · 6 file changes · 1 removal

 Install
   + ripgrep      brew       14.1.1
   + fd           brew       10.2.0
   + kv           cargo      git:9e1c0b7          builds from source · usually ~2m
   + herdr        script     latest               runs packages/herdr/install.sh

 Upgrade
   ~ bottom       cargo      0.9.6 → 0.10.2

 Remove
   - zellij       brew       0.41.1               not declared anymore

 Files
   + ~/.config/hop/config.toml                   link
   ~ ~/.config/ghostty/config                    render   font-size 13 → 15
   ~ ~/.gitconfig                                fragment +2 lines from delta
   ⚡ ~/.config/yazi                              link     existing folder moved to backup first

 Heads up
   ▲ Needs your password once (apt)
   ▲ 1 existing folder will be backed up to ~/.local/state/dots/backup/r-7f3c/
   ▲ 1 step is interactive and will run last: kv (asks for GitHub login)

 ? Apply these changes? › Yes / No / Show details
```

"Show details" expands every row: the exact commands (`brew install ripgrep fd`) and a full diff for every file change (§5.3). The prompt is `cliclack`-style, with arrows to choose and Enter to confirm. `apply --yes` skips it, and the Heads-up block is still printed.

### 4.5 `dots apply` (live progress)

The live region at the bottom is redrawn atomically (synchronized output). Finished tasks scroll up above it as permanent single lines, so the scrollback is a clean log.

```
 ✓ homebrew       ready
 ✓ ripgrep, fd    brew install                          4.2s
 ✓ ~/.config      6 files updated                       0.1s
 ✗ herdr          script failed                         12s   (details below)

 ⠹ kv             cargo  compiling ez-workspaces (142/310)          1m 12s  ━━━━━━━━━━━━╺━━━━━━━━  ~50s left
 ⠹ bottom         cargo  compiling clap_derive                         38s
 ○ yazi           waiting for brew
 ─────────────────────────────────────────────────────────────────────────────────
 6 of 11 done · 1 failed · 1m 20s
```

How each part works:
- **Per-task line**: spinner, name, provider, then the **last line of the tool's own output** (dimmed and truncated to fit), so a long cargo build visibly makes progress instead of looking frozen. Elapsed time is on the right.
- **ETA** comes from the journal: dots knows how long `kv` took the last 3 times on this machine. It is only shown when there is history, and never invented.
- **Bars** appear only when progress is measurable: a download with Content-Length, cargo's `(n/m)` and apt's `Unpacking (n/m)`. Otherwise there is just a spinner. There are no fake progress bars.
- **Terminal progress**: OSC 9;4 sets the tab/taskbar progress in ghostty, Windows Terminal, WezTerm and ConEmu, and the window title shows `dots: 6/11`. Both are cleared on exit, including on Ctrl-C and on panic, via a drop guard.
- **Sudo prompt**: the live region is suspended and cleared, the password prompt appears normally, and the region resumes. The prompt is never drawn over.
- **Interactive tasks** run last, with the live region torn down and a header line (`── kv needs you ──`) before handing over the terminal.
- **Ctrl-C once**: "Stopping after current steps… (Ctrl-C again to force)". Running steps finish, the journal is written, and it ends with a summary of what was and wasn't done. **Twice**: kill the children and record them as `interrupted`.

### 4.6 Apply summary (and failures)

```
 ✗ Done with 1 problem   10 of 11 succeeded · 2m 31s

 ╭─ herdr failed ─────────────────────────────────────────────────────────────────
 │ The install script finished, but herdr still isn't on your PATH.
 │
 │   install.sh: line 14: herdr: command not found
 │   + cp target/release/herdr /usr/local/bin/
 │   cp: /usr/local/bin/herdr: Permission denied
 │
 │ full log  ~/.local/state/dots/logs/r-7f3c/herdr.install.log
 │ → fix the script, then: dots apply herdr
 ╰────────────────────────────────────────────────────────────────────────────────

 Next
   ○ 2 manual steps left               → dots todo
   ▲ Log out and back in to finish karabiner setup
```

Notes: the failure box is the **only** boxed element in the app, so failures can't be missed when scrolling. The box shows the last ~15 relevant lines: lines matching `error|failed|denied|not found` are prioritized, with ± 2 lines of context. The log path is a hyperlink. If the run took over 60 s and the terminal is not focused, a desktop notification is sent (OSC 777/9 where supported, `osascript`/`notify-send` otherwise). Set `ui.notify = false` to turn it off.

### 4.7 `dots todo show <id>` (manual actions)

The markdown is rendered with headings, lists, bold, inline code and code blocks highlighted. Links become hyperlinks. **Action keys** appear at the bottom:

```
 karabiner · Grant Input Monitoring + Accessibility            ○ pending · needs logout

   1. System Settings › Privacy & Security › Input Monitoring
      enable karabiner_grabber
   2. System Settings › Privacy & Security › Accessibility
      enable karabiner_observer

   Verify: press a remapped key.

 [o] open Settings   [c] check now   [d] mark done   [s] skip   [n] next   [q] quit
```

`o` opens deep links the markdown declares (`open: x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility`, `ms-settings:`, URLs), so a non-expert lands on the exact settings pane. `c` runs the self-check and shows `✓ verified` or `✗ not yet`. Plain `dots todo` walks through pending items one at a time in this view.

### 4.8 Onboarding (first run)

This is a clack-style guided flow: a left rail, one question at a time, and the recommended answer preselected. Pressing Enter all the way down gives you the defaults.

```
┌  dots · set up this Mac
│
◇  Use recommended setup?
│  ● Yes, use recommended (base, cli, mac-desktop)
│  ○ Let me choose
│
◆  Git email (needed for commits)
│  ofir@…▌
│
◇  Ready: 41 packages, 63 files, 2 manual steps. Takes about 6 minutes.
│  ● Start   ○ Show plan first
└
```

"Let me choose" expands into a multi-select of bundles (space to toggle, with one-line descriptions), then the `ask` settings grouped on one screen with defaults filled in. Validation errors appear inline under the field and never abort the flow. The flow does not run without a TTY. In that case `dots init` says exactly which settings are required, so they can be passed in a non-interactive way.

### 4.9 `dots bins`

```
 dots bins   412 executables on PATH · 96 from dots · 3 need attention

   ≠ bat     ~/.cargo/bin/bat          bat        cargo   0.24.0
             /usr/bin/batcat           (bat)      apt     0.19.0   shadowed · installed twice
             → dots migrate bat --to cargo   (removes the apt copy)
   ◌ delta   ~/.cargo/bin/delta        –          cargo   0.18.2   not managed by dots
             → dots adopt-package delta
   ? foo     ~/.local/bin/foo          unknown owner

   ✓ 93 more from dots     dots bins --all
```

`dots which rg` shows the same row format for one binary, listing every copy in PATH order with `▶` marking the one that runs.

### 4.10 `dots why`, `dots add` (trees)

```
 kv is installed because

   work-mbp (machine)
   └─ cli (bundle)
      └─ koala (bundle)
         └─ kv              cargo · git:9e1c0b7
            └─ needs rust ≥1.80   (provided by rustup 1.83.0 ✓)
```

`dots add bundle|pkg` uses the same tree to show what will be added, with `+ new`, `= already have (same origin)` and `! name taken` markers. After the tree, there is a **"Code that will run on your machine"** section listing each script with its line count. Press `v` to view each script in the pager before confirming.

### 4.11 `dots log`

```
 History · ghostty

   Nov 25  20:11  ↑ upgrade   1.2.2 → 1.2.3      this Mac    main @ f452241
   Nov 20  14:02  ✎ setting   font_size 13 → 15  this Mac    machine scope
   Sep 14  09:30  + install   1.2.0              this Mac    main @ 3e9a771
   Sep 14  09:12  ◆ commit    "ghostty: add"     repo        3e9a771  ofirgall
```

Commit SHAs are hyperlinks to the remote's commit page (GitHub/GitLab URL derived from the remote).

---

## 5. Components (the toolkit every screen is built from)

| Component | Behavior |
|-----------|----------|
| **Context bar** | One `muted` line: root · branch @ sha · machine · platform. A dirty repo or a worktree root is highlighted (`warn`: "⎇ try-zellij (worktree)"). It always appears on `status`, `plan` and `apply`, so you always know which checkout you're applying from. |
| **Headline** | Symbol plus one sentence answering the question. |
| **Section** | A bold title, then count pills, then the rows, then an "N more ok" collapse line. |
| **Row** | state · name · provider · versions · detail, with columns aligned per section. |
| **Hint** | `→ command` under its row, in `hint` color. A row gets at most two hints. |
| **Callout** | `▲` + sentence for risks ("needs password", "will back up", "downgrade"). Shown before prompts. |
| **Tree** | `├─ └─ │` (ASCII: `|-- \--`). Used by `why`, `add`, `bundles`. |
| **Diff** | §5.3 |
| **Failure box** | §4.6. The only box in the app. |
| **Prompt** | confirm, select, multi-select, text with validation. Always shows the default. Esc cancels the current step, not the whole run. |
| **Live region** | §4.5 |
| **Markdown** | todos, `dots explain <topic>`, release notes |

### 5.1 Responsive layout

Each table declares a priority for every column. As the width shrinks, low-priority columns are dropped first and then middle-truncated. Columns never wrap. Below 60 columns, rows switch to a stacked layout:

```
 ↑ bottom
   cargo · 0.9.6 → 0.10.2
```

The name and state are never dropped. Widths are computed with `unicode-width`, so CJK or emoji in a path never breaks the alignment.

### 5.2 Pager

Output longer than the screen (`status --all`, `log`, `plan` details, diffs) goes through `$DOTS_PAGER`, then `$PAGER`, then `less -RFX`. `-F` means short output doesn't get paged, and `-R` keeps the colors. `--no-pager` disables it. The pager is never used in the Plain tier or under `--json`.

### 5.3 Diffs

File changes in `plan` and `adopt`, and `dots diff`, render as **structural diffs** for structured formats ("`ui.theme`: dark → light", "`keys.bindings`: +2 items"). That is more readable for a non-expert than raw `-`/`+` lines. Line diffs are used for text. Within changed lines, the changed words are highlighted (word-level emphasis via `similar`). If the user has `delta` or `difftastic` configured as git's pager, `ui.diff = "git-pager"` routes the diffs there instead.

---

## 6. Errors (diagnostics, not stack traces)

Errors follow `miette`'s style: a title, the source snippet with a caret, a `help:` line, an error code, and a "did you mean". It covers TOML parse errors, unknown keys, directive errors and condition errors.

```
 ✗ Unknown tag in condition                                         dots::E0204

   ╭─[packages/bluetuith/package.toml:4:28]
 3 │ name = "bluetuith"
 4 │ when = 'all(os = "linux", not(tag = "remot"))'
   ·                            ───────────┬─────
   ·                                       ╰── "remot" is not a declared tag
   ╰────
  help: did you mean "remote"? Declared tags: remote, work, gui (dots.toml)
```

- Every error has a code, and `dots explain E0204` prints a longer explanation with examples.
- A command-line typo gets "did you mean" (`dots stauts` → `did you mean dots status?`).
- Errors from external tools (brew, apt) are not rewritten. They are **framed**: what dots was doing, the tool's actual message, the log path and the next step.
- Panics print one apologetic paragraph, the path of a bug-report file with the redacted context, and a link to report it. A user never sees a raw Rust backtrace unless `RUST_BACKTRACE` is set.

---

## 7. Help that teaches

- `dots help` groups commands by **task** ("Set up a machine", "Keep things updated", "Change a config", "Try something safely", "When something's wrong"), not alphabetically. Each command line has a one-line description.
- Every subcommand's help ends with 2–3 **real examples**.
- `dots explain <topic>` gives short rendered explainers for the concepts (`bundles`, `settings`, `overrides`, `worktrees`, `foreign-entries`, `drift`, `lock`). These are the same pages the agent skill and the hints link to.
- Shell completions complete package names, bundle names, setting keys (`ghostty.font_size`), todo ids and worktree names, not just subcommands.

---

## 8. Verbosity and machine output

| Mode | Shows |
|------|-------|
| `-q` | only problems and the final one-line result |
| default | §4 screens |
| `-v` | plus the exact external commands, prefixed `$` in `muted` |
| `-vv` | plus the full streamed output of each tool, prefixed by the task name (`kv │ Compiling…`) |
| `--json` | one JSON document per command, schema-versioned (`"schema": 1`), with the same state vocabulary. With `apply --json`, NDJSON events (`task_started`, `task_output`, `task_finished`, `summary`) for wrappers and statusbars. |

There are always debug logs (via `tracing`) in `~/.local/state/dots/logs/<run>/dots.log`, whatever the verbosity, so "run it again with -v" is never necessary to diagnose a failure.

---

## 9. Implementation architecture

```
command ──► Report (typed: Headline, Sections<Row>, Hints, Callouts, Tree, Diff, Failure…)
                   │
        ┌──────────┼──────────────┐
   RichRenderer  PlainRenderer  JsonRenderer
        │
   Theme (tokens × light/dark × tier) · Symbols (unicode/nerd/ascii) · Width/Layout engine
```

- A single crate, `dots-ui`, owns stdout and stderr. Only it may write to them, and that is enforced by a clippy lint denying `println!` elsewhere. That rule is what keeps the output consistent across ~40 commands.
- Live progress: the engine emits `Event`s on a channel, and the UI thread owns the live region. Executors never touch the terminal.
- Crates:

  | crate | purpose |
  |-------|---------|
  | `anstream` + `anstyle` | color detection, `NO_COLOR`/`FORCE_COLOR`, Windows VT, stripping when piped |
  | `supports-hyperlinks`, `supports-unicode` | capability detection (plus our own terminal allowlist for OSC 8) |
  | `indicatif` | the multi-progress live region (custom templates); synchronized-output wrapper is ours |
  | `cliclack` | onboarding and prompts (clack style) |
  | `miette` | diagnostics (§6) |
  | `termimad` | markdown rendering |
  | `similar` | line and word diffs |
  | `unicode-width`, `textwrap` | layout |
  | `terminal_size` | width/height + SIGWINCH |
- **Snapshot tests** (`insta`): every screen in §4 is rendered from a fixture Report at widths 50, 80 and 140, in each tier, with dark and light themes. Any output change is a visible diff in review. These snapshots double as the docs screenshots.
- **Performance**: the rich renderer must not add measurable latency. The live region redraws at most 15 times per second. Redraws use synchronized output, and the terminal title and OSC 9;4 updates are throttled to 2 per second.

---

## 10. Accessibility checklist

- [ ] Every state is readable without color (symbol + word).
- [ ] Red and green never carry meaning alone. `ok` and `err` also differ in symbol and word.
- [ ] Contrast of every token against both backgrounds is ≥ 4.5:1 (checked in CI from the token table).
- [ ] `DOTS_UI=plain` gives linear, screen-reader-friendly output with no redraws.
- [ ] `DOTS_REDUCE_MOTION` turns off spinners and animations.
- [ ] All prompts can be answered with keyboard only, and none need mouse or color to understand.
- [ ] No information is conveyed only through hyperlinks: the visible text alone is sufficient.
