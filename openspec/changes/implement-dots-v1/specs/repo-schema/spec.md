# Spec Delta

## Purpose

Defines the layout of a dots repository and the schemas of its TOML files (`dots.toml`, bundles, packages, `machine.toml`). It also defines the strict, versioned parsing rules that make a typo an error instead of silently doing nothing.

## ADDED Requirements

### Requirement: Repository layout
A dots repo SHALL be a git repository containing:
- `dots.toml` at its root;
- optionally `dots.lock`, `bundles/<name>.toml`, `packages/<name>/package.toml`, `overrides/`, `machines/<name>.toml`, `machines/<name>/overrides/` and `lib/`.

The package name SHALL equal its directory name.

#### Scenario: Package name mismatch
- **WHEN** `packages/fd/package.toml` declares `name = "fdx"`
- **THEN** loading fails with `schema-name-mismatch`, naming the file

### Requirement: Self-contained packages
Every path a package references SHALL resolve inside its own directory. A `//`-prefixed repo-root path SHALL be accepted only with a lint warning, and SHALL block `add --copy` of that package.

#### Scenario: Escaping path
- **WHEN** a package file entry uses `src = "../shared/x"`
- **THEN** loading fails with `path-escapes-package`

### Requirement: Strict versioned parsing
Every schema file SHALL declare `schema = 1`. Parsing SHALL reject:
- unknown keys (with a "did you mean" hint);
- wrong value types;
- unsupported schema numbers.

Each error SHALL report `file:line:column`.

#### Scenario: Future schema
- **WHEN** a package declares `schema = 2`
- **THEN** loading fails with `schema-unsupported`, and the hint names the dots version that supports it

### Requirement: Repo manifest dots.toml
`dots.toml` SHALL accept these keys:
- `schema`, `requires_dots`, `name`
- `[priority.<platform-key>]` lists
- `[tags]` (name → description, optional `recommended`)
- `[paths]` (per-platform path variables)
- `[platforms.<key>] parent`
- `[providers.<id>]` (user-defined providers)
- `[provider.<id>]` (built-in provider options)
- `[settings.<pkg>]` (repo-scope setting values)
- `[[upstream]]`
- bundle `recommended` markers

#### Scenario: dots version too old
- **WHEN** `requires_dots = ">=0.3"` and the binary is 0.2.1
- **THEN** every command except `--version` and `doctor` fails with `dots-too-old`

### Requirement: Bundle schema
A bundle file SHALL accept these keys:
- `schema`, `description`
- `include` (other bundles)
- `packages` (names, or `{name, when}` entries)
- `[settings.<pkg>]`
- `recommended`
- `[platform."<key>"]` tables

The resolved package set SHALL be a set. A package included through several paths SHALL be included if any path's condition is true.

#### Scenario: Duplicate inclusion
- **WHEN** both `cli` and `base` list `ripgrep`
- **THEN** the package set contains `ripgrep` once, and `dots why ripgrep` shows both paths

### Requirement: Package schema
A package file SHALL accept these keys:
- `schema`, `name`, `description`, `when`
- `bin`/`bins`, `version`, `requires`, `after`, `conflicts`, `replaces`, `prefer`, `providers_only`
- `[providers.<id>]`, `[[files]]`, `[settings.<key>]`, `[env]`, `[hooks]`, `[integrity]`, `integrity`
- `[platform."<key>"]`
- `[origin]` (written by dots only)

A package with no `[providers]` table SHALL be treated as supported by every provider in the priority list, unless `providers_only = true`.

#### Scenario: Minimal package
- **WHEN** `packages/ripgrep/package.toml` contains only `schema`, `name` and `bin = "rg"`
- **THEN** it resolves with the first available provider in the platform priority list

#### Scenario: Hand-written origin
- **WHEN** a user edits `[origin]`
- **THEN** `dots lint` reports `origin-modified`, because `origin` is an identity field

### Requirement: Machine file
`~/.config/dots/machine.toml` SHALL accept these keys:
- `root`, `profile`, `bundles`, `tags`, `exclude`, `platform_extra`
- `[facts]` overrides
- `[settings.<pkg>]`

This file SHALL never be read from or written into the repo.

#### Scenario: Excluded package
- **WHEN** `exclude = ["spotify"]` and a selected bundle lists `spotify`
- **THEN** `spotify` is not installed, and `status --all` shows it as `excluded`

### Requirement: Committed machine profiles
When `machine.toml` sets `profile = "<name>"`, or the hostname matches `machines/<name>.toml`, that profile SHALL be applied first. The local `machine.toml` SHALL override it key by key.

#### Scenario: Profile plus local tweak
- **WHEN** the profile lists bundles `[base, cli]` and `machine.toml` sets `bundles = { add = ["gaming"] }`
- **THEN** the effective bundles are `base, cli, gaming`

### Requirement: Lint
`dots lint` SHALL check without changing anything:
- schemas;
- unknown tags, facts and platform keys;
- fragment collisions;
- `//` escapes;
- script packages that have no `check`;
- directives in strict JSON;
- links to files that contain directives;
- stale and redundant overrides.

It SHALL exit 1 on errors and 0 when there are only warnings.

#### Scenario: Unknown tag
- **WHEN** a condition references `tag = "wrok"` and `[tags]` declares `work`
- **THEN** lint fails with `unknown-tag` and suggests `work`
