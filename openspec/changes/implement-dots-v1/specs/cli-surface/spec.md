# Spec Delta

## Purpose

Defines the externally visible command-line contract of `dots`: the command tree, global flags, environment variables, exit codes and the machine-readable JSON envelope that scripts, status bars and agents depend on.

## ADDED Requirements

### Requirement: Single binary named dots
The tool SHALL ship as one executable named `dots`. Running `dots` with no arguments SHALL print the dashboard: the same header and checks as `dots status`, with problems grouped by kind (UX §4.1). Its exit code SHALL be the same as `dots status`.

#### Scenario: Bare invocation
- **WHEN** the user runs `dots` with no arguments
- **THEN** the dashboard is printed (one line per kind of problem, then one collapsed ok line) and the exit code matches `dots status`

### Requirement: Command tree
The CLI SHALL expose exactly these top-level commands in v1:
- `init`, `apply`, `plan`, `status`, `outdated`, `why`, `which`, `bins`, `doctor`, `facts`;
- `upgrade`, `pin`, `unpin`, `remove`, `migrate`, `add`, `search`, `sources`, `pull`, `eject`, `uneject`;
- `todo`, `adopt`, `try`, `use`, `roots`, `diff`, `log`, `verify`, `restore`;
- `config`, `questions`, `show`, `override`, `skill`, `lint`, `lock`, `import`, `completions`.

Unknown commands SHALL fail with a "did you mean" suggestion.

#### Scenario: Typo in a command
- **WHEN** the user runs `dots stauts`
- **THEN** the exit code is 64 and stderr suggests `status`

#### Scenario: Help lists every command
- **WHEN** the user runs `dots --help`
- **THEN** every command listed in this requirement appears, grouped by purpose, each with a one-line description

### Requirement: Command signatures
Each command SHALL accept exactly the positional arguments and flags in the signature table below, and nothing else. An unknown flag is a usage error (exit 64).

#### Scenario: Signature table conformance
- **WHEN** `dots <command> --help` runs for any command
- **THEN** its usage matches this table exactly

| Command | Signature |
|---|---|
| `init` | `[--from <source>] [--root <path>] [--defaults]` |
| `apply` | `[<pkg\|bundle>...] [--files-only\|--packages-only] [--yes] [--dry-run] [--allow-downgrade] [--root <path>] [--home <dir>]` |
| `plan` | `[<pkg\|bundle>...] [--files-only\|--packages-only] [--check]` |
| `status` | `[<pkg>] [--all] [--strict]` |
| `outdated` | `[<pkg>...] [--refresh]` |
| `why` | `<pkg>` |
| `which` | `<bin>` |
| `bins` | `[<pkg\|provider>] [--managed\|--unmanaged\|--unknown\|--shadowed]` |
| `doctor`, `facts` | (none) |
| `upgrade` | `[<pkg>...\|--all] [--provider <id>] [--commit]` |
| `pin` | `<pkg> [<version>] [--commit]` |
| `unpin` | `<pkg> [--commit]` |
| `remove` | `<pkg>` or `bundle\|pkg <item>`, with `[--commit]` |
| `migrate` | `<pkg> --to <provider>` |
| `add` | `bundle\|pkg <source>:<name>` or `bundle\|pkg <name>`, with `[--copy] [--as <name>] [--no-deps] [--dry-run] [--yes] [--exclude <pkg>...] [--commit]` |
| `search` | `<source> [<query>]` |
| `sources` | (none) |
| `pull` | `[<source\|item>] [--dry-run] [--to <rev>] [--commit]` |
| `eject`, `uneject` | `<address> [--commit]` |
| `todo` | `[show <id>\|done <id> [--note <text>]\|skip <id>\|check]` |
| `adopt` | `<dst> [--layer <layer>] [--as-patch] [--machine]` |
| `try` | `<name> [--from <ref>]` |
| `use` | `<name\|path\|->` |
| `roots` | (none) |
| `diff` | `<a>..<b>` |
| `log` | `[<pkg>] [--since <duration>] [--host <name>]` |
| `verify` | `[<item>]` |
| `restore` | `<run-id>` |
| `config` | `list \| get <pkg>.<key> \| set <pkg>.<key> <value> [--repo [--commit]\|--machine] \| unset <pkg>.<key> \| explain <pkg>.<key>` |
| `questions` | (none) |
| `show` | `<address> [--effective]` |
| `override` | `list \| edit <address> [--machine] \| diff <address>` |
| `skill` | `install [--agent <id>...]` |
| `lint` | (none) |
| `lock` | `--fix` |
| `import` | `dotbot <file>... \| scan <dir>` |
| `completions` | `<bash\|zsh\|fish\|pwsh>` |

#### Scenario: Mutually exclusive flags
- **WHEN** the user runs `dots apply --files-only --packages-only`
- **THEN** the exit code is 64 and the error names both flags

#### Scenario: Missing required argument
- **WHEN** the user runs `dots migrate fd` without `--to`
- **THEN** the exit code is 64 and the error shows the full signature

### Requirement: Global flags
Every command SHALL accept these flags:
- `--json`
- `-v`/`-vv` (more detail)
- `-q` (errors only)
- `--color <auto|always|never>`
- `--no-input` (never prompt; any required confirmation fails instead)
- `--offline` (no network)
- `--machine-config <path>` (alternate `machine.toml`)

#### Scenario: No prompts in automation
- **WHEN** `dots apply --no-input` reaches a confirmation without `--yes`
- **THEN** it does not prompt, exits 1, and the error says to pass `--yes`

#### Scenario: Offline mode
- **WHEN** `dots outdated --offline` runs
- **THEN** only cached latest versions are used, and rows with no cache are reported as unknown

### Requirement: Environment variables
The CLI SHALL honor these environment variables:
- `NO_COLOR`, `FORCE_COLOR`, `CLICOLOR_FORCE`;
- `DOTS_UI=plain|basic|rich`, `DOTS_REDUCE_MOTION`;
- `DOTS_CONFIG_DIR` (default `~/.config/dots`), `DOTS_STATE_DIR` (default `~/.local/state/dots`), `DOTS_CACHE_DIR` (default `~/.cache/dots`), `DOTS_DATA_DIR` (default `~/.local/share/dots`). XDG base dirs are respected when set.
- `GITHUB_TOKEN`, optional, for GitHub API calls.

A flag SHALL take precedence over the equivalent environment variable.

#### Scenario: State dir relocation
- **WHEN** `DOTS_STATE_DIR=/tmp/s dots apply` runs
- **THEN** `state.json`, `journal.jsonl`, `logs/`, `backup/` and `gen/` are written under `/tmp/s`

### Requirement: Exit codes
The CLI SHALL use these exit codes:

| Code | Meaning |
|---|---|
| 0 | success |
| 1 | error |
| 2 | drift present (`status`, `verify`) |
| 3 | non-empty plan (`plan --check`) |
| 64 | usage error |
| 75 | another dots run holds the run lock |
| 130 | interrupted |

#### Scenario: Drift detection in a prompt
- **WHEN** `dots status` finds an outdated or modified item and nothing failed
- **THEN** the exit code is 2

#### Scenario: CI gate
- **WHEN** `dots plan --check` finds actions to take
- **THEN** the exit code is 3 and the plan is printed

### Requirement: JSON envelope
With `--json`, every command SHALL write exactly one JSON document to stdout. It is an object with these fields:
- `schema`: `"dots/<command>@<n>"`
- `ok`: bool
- `data`: object
- `warnings`: array
- `errors`: array of `{code, message, hint, provenance}`

Nothing else SHALL be written to stdout. Progress and logs go to stderr or are suppressed.

#### Scenario: Machine-readable status
- **WHEN** `dots status --json` runs
- **THEN** stdout parses as one JSON object with `schema = "dots/status@1"`, and `data` holds per-item `state` values from the shared state vocabulary

#### Scenario: Errors in JSON mode
- **WHEN** `dots apply --json --yes` fails on a version conflict
- **THEN** `ok` is false, and `errors[0].code` is the stable code `version-conflict` with each side's provenance

### Requirement: Stable error codes
Every error SHALL carry a stable kebab-case code, such as:
- `schema-unknown-key`, `version-conflict`, `binary-collision`, `dependency-cycle`;
- `stale-override`, `integrity-mismatch`, `provider-unavailable`, `check-failed-after-install`;
- `lock-busy`, `patch-does-not-apply`, `name-collision`.

The human message, hint and provenance SHALL appear in both rich and JSON output.

#### Scenario: Unknown key in a package
- **WHEN** a `package.toml` contains `vresion = "1"`
- **THEN** the error code is `schema-unknown-key`, the message gives `file:line`, and the hint suggests `version`

### Requirement: Shell completions
`dots completions <shell>` SHALL print a completion script for bash, zsh, fish or pwsh. The script SHALL complete commands, flags, and dynamic values from the current repo (package names, bundle names, addresses, todo ids, roots).

#### Scenario: Completing a package name
- **WHEN** zsh completion runs for `dots why gh` in a repo with a `ghostty` package
- **THEN** `ghostty` is offered
