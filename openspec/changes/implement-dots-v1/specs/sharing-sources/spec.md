# Spec Delta

## Purpose

Defines how a user reuses someone else's bundles and packages. Items can be followed at a pinned commit with local overrides, or copied with provenance. It covers previewing the effect on the real machine, reviewing code that will run, and pulling updates safely.

## ADDED Requirements

### Requirement: Add names the kind
`dots add` SHALL accept these forms:
- `dots add bundle <source>:<name>`
- `dots add pkg <source>:<name>`

A source SHALL be one of:
- `owner/repo`
- `github:owner/repo[@ref]`
- `git+ssh://…`
- a local path

`add bundle <name>` with no source SHALL enable one of the user's own bundles on this machine. If the name exists only as the other kind, the error SHALL suggest the right command.

#### Scenario: Wrong kind
- **WHEN** `dots add pkg ofirgall/dotfiles:cli` runs and `cli` is a bundle there
- **THEN** it fails, suggesting `dots add bundle ofirgall/dotfiles:cli`

### Requirement: Follow is the default
Without `--copy`, `add` SHALL:
- add or extend an `[[upstream]]` entry (`alias`, `url`, `follow`, `import`, `rename`, `trust`, `scripts`, `integrity`);
- pin the rev in `dots.lock`;
- copy no files into the repo.

#### Scenario: Follow a bundle
- **WHEN** `dots add bundle ofirgall/dotfiles:cli` is confirmed
- **THEN** `dots.toml` gains `import = ["bundle:cli"]` under alias `ofirgall`, and `dots.lock` gains its rev

### Requirement: Copy mode
`add --copy` SHALL copy the item's directories into the repo and write `[origin]` with `repo`, `path`, `rev` and `tree`. A copy SHALL have no runtime dependency on the source.

#### Scenario: Source disappears
- **WHEN** the source repo of a copied package is deleted
- **THEN** status and apply are unaffected, and pull reports the source as unreachable

### Requirement: Namespacing and collisions
Followed items SHALL be addressable as `<alias>:<name>` and also by their bare name. A bare name that collides with a local item or with another upstream's item SHALL be the error `name-collision` until `--as` renames it.

#### Scenario: Local name taken
- **WHEN** a local `ghostty` exists and `dots add pkg ofirgall/dotfiles:ghostty` runs
- **THEN** it fails with `name-collision`, suggesting `--as`

### Requirement: Real-machine preview
Before asking for confirmation, `add` SHALL show the plan `apply` would run. `--dry-run` SHALL stop after the preview.

Each package SHALL get one verdict:
- `+ new`
- `= adopt`
- `↑ upgrade`
- `⚠ shadow`
- `✗ conflict`

Each file SHALL get one verdict:
- `+ new`
- `= same`
- `⚡ replace`
- `~ edit`
- `✗ blocked`

The headline SHALL count each verdict. The preview SHALL be read-only and SHALL run no code from the source.

#### Scenario: Existing unmanaged install
- **WHEN** ripgrep was installed by brew outside dots, and the bundle wants ripgrep
- **THEN** its verdict is `= adopt`, and nothing is reinstalled

#### Scenario: Existing config would be replaced
- **WHEN** `~/.config/yazi/` exists, is not owned by dots, and differs from the bundle's version
- **THEN** its verdict is `⚡ replace`, `d` shows a diff, and the preview says it will be backed up

#### Scenario: Unknown copy on PATH
- **WHEN** `~/.local/bin/fzf` exists with no owner, and the bundle installs fzf through brew
- **THEN** its verdict is `⚠ shadow`, naming which copy wins on PATH and offering `--exclude fzf`

### Requirement: Code review before running
Every `add` and every `pull` SHALL list each script, hook, check command and skill that will run or be installed, with line counts, and offer to view each one. New and changed code SHALL be listed first.

#### Scenario: Pull with a changed script
- **WHEN** an upstream changes `install-plugins.sh`
- **THEN** pull shows its diff before anything else, and requires confirmation under `trust = "review"`

### Requirement: Script policy
`scripts = "deny"` on an upstream SHALL drop that upstream's script-provider packages, hooks and render-time commands. Those items SHALL show `blocked-by-policy` in status.

#### Scenario: Declarative-only upstream
- **WHEN** `scripts = "deny"` and the bundle contains a script package
- **THEN** the package is not installed, and status explains why

### Requirement: Search and sources
- `dots search <source> [<query>]` SHALL list the source's bundles and packages, with descriptions.
- `dots sources` SHALL list, for every source: commits behind, changed items, local edits and stale overrides.

#### Scenario: Behind upstream
- **WHEN** the upstream has 14 new commits that touch 3 imported items
- **THEN** `dots sources` shows `14 behind · 3 changed`

### Requirement: Pull
`dots pull [<source|item>] [--dry-run] [--to <rev>]` SHALL:
- for followed items, move the pinned rev and re-check every override;
- for copied items, run a 3-way merge between `origin.rev`, the local copy and the source head.

Stale overrides SHALL stop the pull. Merge conflicts SHALL be left as standard markers and shown in status. Reverting the lock commit SHALL fully undo a followed pull.

#### Scenario: Stale override stops the pull
- **WHEN** a pull removes a setting that a local override targets
- **THEN** the lock is unchanged, and the error names the override

#### Scenario: Pin to a specific rev
- **WHEN** `dots pull ofirgall --to abc123` runs
- **THEN** the lock pins `abc123`, including when that rev is older than the current pin

### Requirement: Eject and uneject
- `dots eject <item>` SHALL turn a followed item into a local copy, with the effective definition.
- `dots uneject <item>` SHALL turn a copy back into a followed item when every local edit can be expressed as an override. Otherwise it SHALL refuse and list the edits it cannot express.

#### Scenario: Free-text edit blocks uneject
- **WHEN** a copied package's linked text file was edited by hand
- **THEN** `uneject` refuses and shows that edit

### Requirement: Upstream resolution isolation
Each upstream SHALL be resolved independently, together with its own upstreams and its committed overrides. Its machine profiles and machine scope SHALL never apply. A cycle between upstreams SHALL be an error.

#### Scenario: Chained upstreams
- **WHEN** C follows B, and B follows A
- **THEN** C sees B's effective items, built on top of A
