# Spec Delta

## Purpose

Defines typed package settings: the knobs a package author anticipates, each with a recommended default. It covers how values are layered across scopes, and the commands that read, set and explain them.

## ADDED Requirements

### Requirement: Setting declarations
A `[settings.<key>]` entry SHALL declare:
- `type`: one of `bool`, `int`, `string`, `enum`, `list`
- exactly one of `default` or `required = true`
- optionally `values` (for `enum`), `description`, `ask`, `question`, `secret`, `validate` (a regex) and `when`

#### Scenario: Neither default nor required
- **WHEN** a setting has no `default` and no `required`
- **THEN** loading fails with `setting-no-default`

### Requirement: Precedence scopes
Setting values SHALL resolve from lowest to highest precedence:
1. package default
2. bundle
3. upstream
4. repo (`dots.toml`)
5. committed machine profile
6. machine (`machine.toml`)

Two bundles setting different values for the same setting SHALL be an error.

#### Scenario: Machine wins
- **WHEN** the repo sets `font_size = 14` and the machine sets `15`
- **THEN** the effective value is 15

#### Scenario: Conflicting bundles
- **WHEN** bundle `a` sets `font_size = 13` and bundle `b` sets `14`
- **THEN** resolution fails with `setting-conflict`, naming both bundles

### Requirement: Validation
Every value SHALL be checked against the setting's type, its enum values and its `validate` pattern. A violation SHALL be an error that names the scope the value came from.

#### Scenario: Wrong type at repo scope
- **WHEN** `dots.toml` sets `font_size = "big"`
- **THEN** the error names `dots.toml` and the expected type `int`

### Requirement: Required settings block rendering
A file that uses a `required` setting with no value SHALL not be rendered. The plan SHALL show the setting as missing, with a hint to run `dots config set`.

#### Scenario: Missing git email
- **WHEN** `git_email` is required and unset
- **THEN** `~/.gitconfig` is skipped, and the plan shows `→ dots config set git.git_email <value>`

### Requirement: Secret settings
A setting with `secret = true` SHALL be stored only in the machine scope. Its value SHALL appear as `•••` in human output, JSON and logs.

#### Scenario: Secret in JSON
- **WHEN** `dots config list --json` runs
- **THEN** the secret's value field is `null`, and `redacted` is true

### Requirement: Config commands
`dots config` SHALL provide:
- `list`
- `get <pkg>.<key>`
- `set <pkg>.<key> <value> [--repo|--machine]`, where machine is the default
- `unset`
- `explain <pkg>.<key>`, which shows the value at every scope and which one wins

`set --repo` SHALL edit `dots.toml` and preserve its formatting.

#### Scenario: Explain
- **WHEN** `dots config explain ghostty.font_size` runs
- **THEN** each scope is listed with its value or `-`, and the winning scope is marked

### Requirement: Questions
`dots questions` SHALL list, each with its recommended answer:
- unanswered `ask` settings;
- unanswered `required` settings;
- recommended bundles that are not selected;
- recommended tags.

Its JSON form SHALL be stable, so agents can use it for onboarding.

#### Scenario: Accept defaults
- **WHEN** every question is answered with its recommended value
- **THEN** only `required` settings with no default remain unanswered
