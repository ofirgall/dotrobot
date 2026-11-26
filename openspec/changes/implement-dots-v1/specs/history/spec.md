# Spec Delta

## Purpose

Defines the two history records: git history of the repo, with dots trailers, and the append-only journal of what happened on each machine. It also defines the merged `dots log` timeline.

## ADDED Requirements

### Requirement: Journal format
`<state>/journal.jsonl` SHALL be append-only, with one JSON object per action. Each line SHALL have these fields:
- `ts`, `run`, `host`, `op`
- `pkg`, `provider`, `from`, `to`
- `root`, `repo_rev`, `repo_dirty`, `upstream_rev`
- `dots`, `exit`, `dur_ms`, `log`

Each line SHALL be written in a single append, so a crash cannot leave a partial line in the middle of the file.

#### Scenario: One line per action
- **WHEN** apply upgrades 2 packages and renders 1 file
- **THEN** 3 lines are appended, all with the same `run`

### Requirement: Journal operations
`op` SHALL be one of:
- `install`, `upgrade`, `downgrade`, `remove`, `pin`, `unpin`
- `link`, `unlink`, `render`, `inject`, `backup`, `restore`
- `todo_done`, `todo_skip`, `use_root`, `adopt`, `migrate_provider`
- `add`, `pull`, `eject`, `verify`

#### Scenario: Adopted install
- **WHEN** apply adopts a version that was already installed
- **THEN** a line with `op = "adopt"` is recorded

### Requirement: Git trailers
Every commit dots creates SHALL carry:
- `Dots-Op` and `Dots-Package` trailers;
- a `Dots-Origin` trailer when an external source is involved.

dots SHALL never commit unless `--commit` is passed. The commands that change repo files (`upgrade`, `pin`, `unpin`, `add`, `pull`, `eject`, `uneject`, `config set --repo`, `remove`) SHALL accept `--commit`. Without it, they leave the changes uncommitted, and `status` shows the repo as dirty.

#### Scenario: Add without commit
- **WHEN** `dots add bundle ofirgall/dotfiles:cli` runs without `--commit`
- **THEN** `dots.toml` and `dots.lock` are modified and nothing is committed

#### Scenario: Upgrade commit
- **WHEN** `dots upgrade --all --commit` runs
- **THEN** one commit is created, with message `dots: upgrade …` and `Dots-Op: upgrade` trailers

### Requirement: Log timeline
`dots log [<pkg>] [--since <duration>] [--host <name>]` SHALL merge two sources into one chronological timeline:
- the git history of the package's directory and lock rows;
- the journal.

Each row SHALL show the time, operation, versions, provider, root and commit, and the machine.

#### Scenario: Package history
- **WHEN** `dots log bottom` runs
- **THEN** the commit that added bottom and every journaled upgrade appear in time order
