# Spec Delta

## Purpose

Defines the read-only commands that show the whole machine at a glance and answer every "why is X like this" question: status, outdated, why, the binary inventory, which, doctor and facts.

## ADDED Requirements

### Requirement: Summary status
`dots status` SHALL print:
- a header with the root, branch, commit, machine, platform and tags;
- per-area counts for packages, files, todos and git sources;
- only the non-ok rows, each with a fix hint.

`--all` SHALL include every row, including inert, excluded and unmanaged ones.

#### Scenario: Attention summary
- **WHEN** 3 packages are outdated and 1 file is modified
- **THEN** the headline reads "4 things need attention", followed by those rows only

### Requirement: Dashboard
`dots` with no arguments SHALL print the same header as `dots status`, then one line per *kind* of problem (for example "3 packages outdated", "1 file modified"), with names truncated and a fix hint per line, then one collapsed line counting what is ok. Its headline SHALL count groups, not items. It SHALL use the same checks and exit code as `dots status`.

#### Scenario: Dashboard counts groups
- **WHEN** 3 packages are outdated and 1 file is modified
- **THEN** `dots` reads "2 things need attention", with one line for the outdated packages and one for the modified file, while `dots status` reads "4 things need attention"

### Requirement: Package deep view
`dots status <pkg>` SHALL show:
- the chosen provider, and why it was chosen;
- the wanted, installed, locked and latest versions;
- the package's files and their states;
- its todos;
- its overrides;
- the most recent entries of its history.

#### Scenario: Provider reason
- **WHEN** fd resolved to cargo because sudo is false
- **THEN** the deep view says "apt skipped: needs root"

### Requirement: Never blocks on the network
`status` SHALL use only cached latest versions. `outdated` SHALL refresh the cache, unless `--offline` is set. The cache TTL SHALL default to 6 hours, and `--refresh` SHALL force a refresh.

#### Scenario: Offline laptop
- **WHEN** there is no network and `dots status` runs
- **THEN** it completes without network calls

### Requirement: Performance target
On a warm cache with 100 packages, `dots status` SHALL complete in under 1.5 seconds using fake providers.

#### Scenario: Benchmark
- **WHEN** the CI benchmark runs status against 100 fake packages
- **THEN** the median runtime is below 1.5 s

### Requirement: Why
`dots why <pkg>` SHALL print every inclusion path as a tree, from the machine through its bundles and requirements down to the package. Each version constraint along a path SHALL be shown with its source.

#### Scenario: Two paths
- **WHEN** rust is required by both kv and jless
- **THEN** both paths are printed

### Requirement: Binary inventory
`dots bins [<pkg|provider>]` SHALL list every executable on PATH, each with its path, owning package, provider, version and a note: shim, shadowed, unmanaged or unknown owner. It SHALL accept the filters `--managed`, `--unmanaged`, `--unknown` and `--shadowed`.

#### Scenario: Unknown owner
- **WHEN** `~/.local/bin/foo` is not owned by any provider
- **THEN** it is listed with package `?` and the note "unknown owner"

### Requirement: Which
`dots which <bin>` SHALL list every copy of `<bin>` on PATH, in PATH order. It SHALL mark the copy that wins, give each copy's owner, and suggest `dots migrate` when the same tool is installed twice.

#### Scenario: Duplicate install
- **WHEN** bat exists from both cargo and apt
- **THEN** the winning copy is marked `▶`, and the other copy's row suggests `dots migrate bat --to cargo`

### Requirement: Doctor
`dots doctor` SHALL report environment problems, each with a fix:
- missing or unavailable providers;
- dangling links;
- PATH order that shadows managed binaries;
- a lock that is stale or has conflicts;
- sudo problems;
- symlink capability on Windows;
- missing upstream checkouts;
- unknown platform keys.

#### Scenario: Shadowing PATH
- **WHEN** `/usr/bin` comes before `~/.cargo/bin` on PATH
- **THEN** doctor reports which managed binaries are shadowed, and how to fix the order

### Requirement: Facts
`dots facts` SHALL print every detected fact, the platform chain and the tags, each with its source (detected, overridden or declared).

#### Scenario: Overridden fact
- **WHEN** `[facts] sudo = false`
- **THEN** facts shows `sudo false (machine.toml)`
