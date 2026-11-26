# Spec Delta

## Purpose

Defines how `dots plan` and `dots apply` compare the desired state with the real machine, show the exact plan, and run it. It covers ordering, batching, one sudo prompt, deferring interactive steps, isolating failures, the run lock, and sandboxed runs.

## ADDED Requirements

### Requirement: Plan is the single source of truth
`plan` SHALL print the exact actions `apply` would take, in order, without side effects. `apply` SHALL execute the same plan. `apply --dry-run` SHALL be equivalent to `plan`.

#### Scenario: Plan then apply
- **WHEN** `dots plan` is followed by `dots apply --yes` with no change in between
- **THEN** apply performs exactly the planned actions

### Requirement: Action ordering
Actions SHALL run in this order:
1. provider bootstraps
2. installs, upgrades and removals, batched per provider
3. file actions
4. system settings
5. services
6. hooks
7. environment generation
8. todo checks

`requires` and `after` SHALL add ordering edges on top of that.

#### Scenario: Config after install
- **WHEN** a package installs software and links its config
- **THEN** the link happens after the install succeeds

#### Scenario: Service after its config
- **WHEN** a package declares a `[service]` and a config file
- **THEN** the plan lists the file action, then the settings, then the service action, then hooks, so the service starts with its config in place

### Requirement: Confirmation policy
`apply` SHALL show the plan and ask before acting. It SHALL skip the question when `--yes` is passed, or when the plan only touches files that dots already owns. It SHALL always ask for these, even with `--yes`:
- downgrades, unless `--allow-downgrade` is passed;
- running scripts from a source the user has not reviewed.

#### Scenario: File-only plan
- **WHEN** the plan only re-renders owned files
- **THEN** apply proceeds without prompting

### Requirement: One sudo prompt
All actions that need root SHALL be grouped together. dots SHALL ask for the sudo password at most once per run and keep that credential alive until the run ends.

#### Scenario: Several apt installs
- **WHEN** three apt packages and one apt PPA are planned
- **THEN** the user is asked for the sudo password once

### Requirement: Interactive actions are deferred
Interactive scripts and manual steps SHALL run last, one at a time, with a prompt before each. Under `--yes` or `--no-input`, they SHALL become todos instead of blocking.

#### Scenario: Unattended run
- **WHEN** `apply --yes` meets an interactive script
- **THEN** the script is not run, and a pending todo is recorded

### Requirement: Failure isolation
A failed action SHALL fail every action that depends on it, which are then reported as `skipped-due-to-dep`. Independent actions SHALL continue. The final summary SHALL list failures with the tail of each log.

#### Scenario: One package fails
- **WHEN** a cargo build fails while unrelated brew installs are planned
- **THEN** the brew installs complete, the exit code is 1, and the summary shows the cargo log tail

### Requirement: Run lock
Only one mutating run (`apply`, `upgrade`, `use`, `pull`, `remove`, `migrate`) SHALL run at a time per state dir. A second mutating run SHALL exit 75 and name the process that holds the lock. Read-only commands SHALL never take the lock.

#### Scenario: Concurrent apply
- **WHEN** `dots apply` is already running and another `dots apply` starts
- **THEN** the second exits 75 with `lock-busy` and the holder's PID

### Requirement: Crash-safe ownership
State SHALL be persisted after each completed action, so that an interrupted run leaves an accurate record of what dots owns.

#### Scenario: Interrupted run
- **WHEN** apply is killed after linking 10 of 20 files
- **THEN** the next status reports exactly those 10 as owned

### Requirement: Sandboxed runs
`--home <dir>` SHALL redirect every user path (`~`, config, state, cache and data dirs) to the given directory. `--root <path>` SHALL use a different repo checkout for this run without switching the active root.

#### Scenario: Test in a sandbox
- **WHEN** `dots apply --files-only --home /tmp/h` runs
- **THEN** no file outside `/tmp/h` is created or changed

### Requirement: Selective apply
Passing packages or bundles to `apply` or `plan` SHALL limit the plan to those items and their `requires` closure. `--files-only` and `--packages-only` SHALL further limit the plan.

#### Scenario: One package
- **WHEN** `dots apply ghostty` runs
- **THEN** only ghostty, its requirements, and their files are acted on
