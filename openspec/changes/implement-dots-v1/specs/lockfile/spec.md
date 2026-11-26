# Spec Delta

## Purpose

Defines `dots.lock`: the committed record of resolved versions, revisions and content hashes per platform. It covers the rules that keep machines reproducible without implicit downgrades, and the commands that move the lock.

## ADDED Requirements

### Requirement: Lock format
`dots.lock` SHALL be TOML with `schema = 1` and three entry kinds:
- `[[pkg]]` with `name`, `platform`, `provider`, `version` or `rev`, `source`, `locked_at`, `by_rev`
- `[[upstream]]` with `alias`, `rev`
- `[[content]]` with `item`, `rev`, `tree`, and download hashes

Entries SHALL be sorted deterministically, one block per entry.

#### Scenario: Stable output
- **WHEN** the same resolution is locked twice
- **THEN** the two files are byte-identical

### Requirement: Platform keying
A `[[pkg]]` row SHALL be keyed by the machine's most specific chain key plus its architecture, for example `ubuntu-24.04/x86_64`. Providers whose versions don't depend on the platform (cargo, uv, npm, git, archive) SHALL use `platform = "*"`.

#### Scenario: Two machines share a lock
- **WHEN** a Mac and an Ubuntu box lock `ripgrep`
- **THEN** the lock holds two brew/apt rows with different platform keys, and each machine reads only its own row

### Requirement: Lock versus spec rules
The plan SHALL follow these rules:
- keep the locked version while it satisfies the spec;
- when the spec changes, resolve the newest version that satisfies it, and show the change to the lock row;
- adopt a version installed outside dots if it satisfies the spec;
- report `drift` when the provider can't produce the locked version but its version still satisfies the spec.

#### Scenario: Spec tightened
- **WHEN** the lock has `0.9.6` and the spec changes to `>=0.10`
- **THEN** the plan shows the lock row changing to the newest version that satisfies `>=0.10`

#### Scenario: Limited provider drift
- **WHEN** apt offers 8.7.1 and the lock says 8.7.0
- **THEN** 8.7.1 is installed and status reports `drift`, which `--strict` treats as an error

### Requirement: No implicit downgrades
A downgrade SHALL appear in the plan with a `⚠ downgrade` line. `apply` SHALL ask before every downgrade, even with `--yes`, unless `--allow-downgrade` is passed.

#### Scenario: Unattended downgrade
- **WHEN** `dots apply --yes --no-input` would downgrade a package
- **THEN** it exits 1 and says to pass `--allow-downgrade`

### Requirement: Upgrade command
`dots upgrade [<pkg>...|--all] [--provider <id>]` SHALL:
- move packages to the newest version allowed by the intersected constraints;
- rewrite only this platform's rows;
- journal every change;
- never commit unless `--commit` is passed. `--commit` makes one commit with a generated message and `Dots-Op` trailers.

#### Scenario: Prefix-bounded upgrade
- **WHEN** a package has the spec `0.11` and `0.12.0` exists
- **THEN** upgrade stays within `0.11.x`

#### Scenario: Dependent blocks the upgrade
- **WHEN** upgrading `rust` would violate `kv`'s constraint
- **THEN** the upgrade is refused, naming `kv`

### Requirement: Pin and unpin
`dots pin <pkg> [<version>]` SHALL:
- set `version` in the package file, preserving its formatting;
- apply the provider's native pin where the provider supports one.

`dots unpin <pkg>` SHALL reverse both.

#### Scenario: Native pin
- **WHEN** `dots pin bottom 0.10.2` runs and bottom is installed through brew
- **THEN** `package.toml` has `version = "0.10.2"` and `brew pin bottom` was executed

### Requirement: Lock repair
`dots lock --fix` SHALL regenerate a lock that fails to parse or contains merge conflict markers, keeping every row that is still valid.

#### Scenario: Merge conflict in the lock
- **WHEN** `dots.lock` contains `<<<<<<<` markers
- **THEN** every command except `lock --fix` fails with `lock-conflicted`, and `lock --fix` produces a valid lock
