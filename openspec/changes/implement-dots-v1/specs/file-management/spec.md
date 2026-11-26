# Spec Delta

## Purpose

Defines how dots places and composes configuration files: file modes, platform layers, format-aware merges, inline directives, patch layers, multi-package fragments, drift states, adoption of app edits, ownership, and backups.

## ADDED Requirements

### Requirement: File modes
A `[[files]]` entry SHALL set `mode` to one of:
- `link` (the default)
- `copy`
- `render`
- `inject`
- `fragment`
- `git`
- `dir`
- `terminfo`

`dst` SHALL accept a per-platform table. Path variables from `[paths]` SHALL expand.

#### Scenario: Per-platform destination
- **WHEN** `dst = { default = "~/.config/ez/c.toml", macos = "{app_support}/ez/c.toml" }`
- **THEN** on macOS the file is placed under `~/Library/Application Support/ez/`

### Requirement: Terminfo mode
A `terminfo` entry SHALL compile its source with `tic -x -o ~/.terminfo`. The entry SHALL be `ok` when `infocmp -x <name>` matches the compiled source, and drifted otherwise.

#### Scenario: Compile on apply
- **WHEN** a package has a `terminfo` entry for `herdr-256color` that is not yet compiled
- **THEN** `plan` shows `tic -x -o ~/.terminfo` and apply compiles it

#### Scenario: Already installed
- **WHEN** `infocmp -x herdr-256color` matches the source
- **THEN** the entry is `ok` and nothing runs

#### Scenario: Source changed
- **WHEN** the source file changes
- **THEN** the entry shows as changed and apply recompiles it

### Requirement: Links go through the active root
A link SHALL point at `<state>/active/...`, never directly into a checkout. Rendered files SHALL live under `<state>/gen/<root-id>/`.

#### Scenario: Link target
- **WHEN** ghostty's config is linked
- **THEN** `readlink ~/.config/ghostty/config` points into `~/.local/state/dots/active/`

### Requirement: Layers
A render SHALL combine its layers in this order:
1. the package's layers, filtered by `when`
2. the repo override layer
3. the machine-profile layer
4. the machine override layer

`layers = { dir = "config", auto = true }` SHALL pick `common` and then every chain key that has a file.

#### Scenario: Auto layers
- **WHEN** `config/common` and `config/ubuntu-22.04` exist, and dots runs on Ubuntu 22.04
- **THEN** the output is `common` followed by `ubuntu-22.04`

### Requirement: Format-aware merge
Formats SHALL be:
- `toml` and `yaml`, preserving comments and order from the first layer
- `json` and `jsonc`
- `ini`/`gitconfig`
- `markdown`, which merges the YAML frontmatter and treats the body as text
- `text`

Tables SHALL deep-merge. Arrays SHALL be replaced unless `[files.merge]` sets `append`, `union` or `{by = "<key>"}` for that path. `"__dots_delete__"` SHALL delete a key. A type mismatch SHALL be an error that shows both layers.

#### Scenario: Array replace by default
- **WHEN** a base layer has `paths = ["a"]` and an override has `paths = ["b"]`
- **THEN** the output has `paths = ["b"]`

#### Scenario: Merge by key
- **WHEN** `"servers" = { by = "name" }` and both layers define a server named `x`
- **THEN** the two `x` entries are merged into one

### Requirement: Text layering prefers native includes
For `text` files, `kind = "include"` SHALL render a stub that uses the application's own include mechanism. Otherwise, layers SHALL be concatenated, each preceded by a header comment naming its source.

#### Scenario: Ghostty include
- **WHEN** a ghostty file entry sets `kind = "include"`
- **THEN** the rendered file contains `config-file = ` lines that point at each layer

### Requirement: Directives
Lines in a layer's own comment syntax SHALL drive conditional output:
- `dots:if <cond>`, `dots:elif`, `dots:else`, `dots:end`
- `dots:if!`, which uncomments the body when the condition holds
- `dots:include <path>`
- `@dots(expr)@` for inline values, with the filters `json`, `toml`, `yaml`, `shell`, `upper` and `lower`

Unbalanced or unknown directives, unknown names, and directives inside strict JSON SHALL be errors that report `file:line`.

#### Scenario: Inline setting
- **WHEN** a layer contains `font-size = @dots(setting.font_size)@` and `font_size` is 15
- **THEN** the output contains `font-size = 15`

#### Scenario: Unknown name
- **WHEN** a layer references `@dots(setting.nope)@`
- **THEN** the render fails with `unknown-name` and reports `file:line`

### Requirement: Structured patches
`[[files.set]]` entries SHALL set or delete a key path after merging, in declaration order. Each entry accepts `path`, `value` or `delete`, and `when`. A path that crosses a value that is not a table SHALL be an error.

#### Scenario: Delete by condition
- **WHEN** `path = "env.DISABLE_TELEMETRY", delete = true, when = 'not(setting.telemetry)'` and telemetry is false
- **THEN** the key is absent from the output

### Requirement: Patch layers
A `*.patch` layer SHALL apply a unified diff, with no fuzz, to the result of the layers before it. If it fails to apply, the render SHALL fail with `patch-does-not-apply`, naming the first hunk that failed. The file SHALL never be left partially patched.

#### Scenario: Stale patch
- **WHEN** an upstream change removes the lines a patch targets
- **THEN** the render fails, and the file on disk is unchanged

### Requirement: Fragments
Several packages SHALL be able to contribute to one `dst` through `mode = "fragment"` and a `priority`. Two fragments setting the same key at the same priority SHALL be a lint error. Removing a package SHALL remove its fragment.

#### Scenario: Gitconfig composition
- **WHEN** the `delta` and `base` packages both contribute to `~/.gitconfig`
- **THEN** the file contains both sections, and removing `delta` drops only `[delta]`

### Requirement: Generated header
Every rendered file whose format allows comments SHALL begin with a header naming its source layers and each setting value it used, together with the scope that value came from.

#### Scenario: Header contents
- **WHEN** ghostty's config is rendered with `font_size` set at machine scope
- **THEN** the header includes `font_size=15(machine)`

### Requirement: Drift states
For each file it owns, status SHALL report exactly one of:
- `ok`, `missing`, `conflict`, `modified`, `stale`
- `dangling`, `foreign-entries`, `tampered`

#### Scenario: App rewrote its config
- **WHEN** an app edits a rendered file after apply
- **THEN** status shows `modified` with a hint to run `dots adopt`

### Requirement: Adopt
`dots adopt <dst>` SHALL write the difference between the file and its render back into a layer:
- `--layer` picks the layer; the default is the most specific active layer;
- `--machine` writes to the machine override layer;
- `--as-patch` writes a patch layer.

For text files, it SHALL open a 3-way merge.

#### Scenario: Adopt into machine scope
- **WHEN** `dots adopt ~/.claude/settings.json --machine` runs
- **THEN** the changed keys are written under `~/.config/dots/overrides/`, and the file returns to `ok`

### Requirement: Ownership and backups
dots SHALL modify or remove only paths recorded as owned. A path that dots does not own SHALL be moved to `backup/<run-id>/` before dots replaces it. `dots restore <run-id>` SHALL put those backups back.

#### Scenario: First apply over an existing file
- **WHEN** `~/.config/yazi/` exists and is not owned
- **THEN** it is backed up first, and `dots restore <run-id>` brings it back

### Requirement: Atomic writes
Every file write and symlink swap SHALL be atomic, so a reader never sees a partially written file.

#### Scenario: Crash during write
- **WHEN** dots is killed during a render
- **THEN** the destination holds either the old content or the new content
