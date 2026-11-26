# Spec Delta

## Purpose

Defines manual steps that software cannot do for the user, such as granting permissions or clicking an enable button. They are written as markdown files with frontmatter, tracked per machine, optionally verified by a self-check, and re-opened when their instructions change.

## ADDED Requirements

### Requirement: Manual action files
A package's `manual/*.md` files SHALL be discovered as manual actions. Their frontmatter SHALL accept:
- `id`, unique within the package; the full id is `<pkg>/<id>`
- `title`
- `when`
- `after`: `install`, `files` or `always`
- `check`
- `requires`: `logout`, `reboot` or `restart-app:<name>`
- `revision`
- `interactive`

Unknown frontmatter keys SHALL be errors.

#### Scenario: Discovery
- **WHEN** `packages/karabiner/manual/permissions.md` exists with valid frontmatter
- **THEN** `dots todo` lists `karabiner/permissions`

### Requirement: Per-machine tracking
Each action's state SHALL be recorded per machine. A record contains `id`, `revision`, `status`, `at`, `repo_rev` and `note`, where `status` is `pending`, `done`, `skipped` or `auto`.

#### Scenario: Done on one machine only
- **WHEN** an action is marked done on the laptop
- **THEN** it is still pending on the desktop

### Requirement: Revisions reopen actions
When an action's `revision` increases, the action SHALL become pending again on every machine, including machines where it was done or skipped.

#### Scenario: Steps changed
- **WHEN** `revision` goes from 1 to 2
- **THEN** the action is pending again, everywhere

### Requirement: Self-checks
When an action has a `check`, it SHALL be run:
- at the end of `apply`;
- on `dots todo check`.

The action SHALL be closed as `auto` when its check exits 0. Actions without a check SHALL stay pending until they are marked done.

#### Scenario: Auto close
- **WHEN** the karabiner permission check passes
- **THEN** `dots todo check` marks the action `auto`

### Requirement: Todo commands
`dots todo` SHALL provide:
- `dots todo`: lists pending actions grouped by package, with `requires` badges;
- `dots todo show <id>`: renders the markdown and offers to open its URLs and settings panes;
- `dots todo done <id> [--note]`;
- `dots todo skip <id>`;
- `dots todo check`.

#### Scenario: Skip until revision
- **WHEN** an action is skipped
- **THEN** it is not listed again until its revision increases

### Requirement: Apply summary
Each `apply` SHALL end with the number of pending manual actions, combining their requirements. For example: "2 manual actions pending, 1 requires logout".

#### Scenario: Logout badge
- **WHEN** a pending action has `requires: [logout]`
- **THEN** the apply summary mentions that a logout is required

### Requirement: Account and session changes
Account and session changes (changing the login shell, adding the user to groups) SHALL be modeled as manual actions with a `check`, not performed by dots. Such an action that needs a re-login SHALL set `requires: [logout]`.

#### Scenario: Login shell
- **WHEN** a manual action `chsh` has a `check` that compares the user's login shell with `command -v zsh`
- **THEN** it is pending while they differ, and `dots todo check` closes it automatically once they match

#### Scenario: Group membership
- **WHEN** an action has `check: 'id -nG | grep -qw dialout'` and `requires: [logout]`
- **THEN** the apply summary lists it with a logout badge, and dots never runs `usermod` itself
