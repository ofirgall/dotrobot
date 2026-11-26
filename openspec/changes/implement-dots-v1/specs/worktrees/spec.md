# Spec Delta

## Purpose

Lets a user try a change to their setup in a git worktree, switch the real machine to it atomically, and switch back. Switching back leaves no dangling links and decides explicitly what happens to anything the trial installed.

## ADDED Requirements

### Requirement: Active root indirection
`<state>/active` SHALL be a symlink (a junction on Windows) to the active checkout. Switching roots SHALL replace it atomically.

#### Scenario: Atomic switch
- **WHEN** `dots use try-zellij` runs
- **THEN** every link that is unchanged between the two roots resolves to the new root, with no file operation on that link

### Requirement: Try
`dots try <name> [--from <ref>]` SHALL:
- create a worktree on branch `try/<name>`, by default under `<root>.worktrees/<name>`;
- make it the active root;
- print how to switch back.

#### Scenario: Start a trial
- **WHEN** `dots try zellij` runs
- **THEN** branch `try/zellij` exists, it is active, and the output says `dots use main` to return

### Requirement: Use reconciles the difference
`dots use <name|path|->` SHALL:
- create links that exist only in the new root;
- remove owned links that exist only in the old root;
- render generated files per root, so switching back needs no re-render.

`-` SHALL mean the previously active root.

#### Scenario: Switch back
- **WHEN** `dots use -` runs after a trial
- **THEN** the previous root is active, and links that existed only in the trial are removed

### Requirement: Packages installed by a trial
When switching away from a root, dots SHALL offer three choices for each package that was installed in that root and is not declared in the target root:
- `remove`;
- `keep`, which makes it an orphan;
- `adopt`, which copies its definition into the target root.

Version changes SHALL be offered for rollback only when the provider supports exact versions.

#### Scenario: Keep a trial package
- **WHEN** the user chooses `keep` for zellij
- **THEN** status shows zellij as `orphan (installed by worktree try-zellij)`

### Requirement: Roots and diff
- `dots roots` SHALL list the known roots with their path, branch, last-used time and active marker.
- `dots diff <a>..<b>` SHALL show the difference in desired state (packages, files, renders) between two roots, without applying anything.

#### Scenario: Compare roots
- **WHEN** `dots diff main..try-zellij` runs
- **THEN** it lists `+ zellij` and any changed files, and the machine is unchanged

### Requirement: Deleted worktree
When the active root's directory is missing, status SHALL report links as `dangling`, and the hint SHALL be to switch back.

#### Scenario: Worktree removed by hand
- **WHEN** the active worktree directory is deleted
- **THEN** status reports dangling links with the hint `→ dots use main`
