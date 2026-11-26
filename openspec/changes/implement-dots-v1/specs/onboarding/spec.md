# Spec Delta

## Purpose

Defines how a new machine or a new user starts: creating or cloning a repo, answering setup questions where every question has a recommended default and can be skipped, bootstrapping on a fresh machine, and installing the dots agent skill.

## ADDED Requirements

### Requirement: Init a new repo
`dots init` with no `--from` SHALL:
- create a repo containing `dots.toml` and empty `bundles/` and `packages/`;
- write `machine.toml`;
- ask only the questions from `dots questions`, with the recommended answer as each default.

`--defaults` SHALL accept every default without prompting.

#### Scenario: Non-interactive init
- **WHEN** `dots init --defaults --no-input` runs
- **THEN** it finishes without prompting, and `machine.toml` holds the recommended bundles and tags

### Requirement: Init from a remote
`dots init --from <source>` SHALL:
- clone the repo, to `--root` or by default `~/dotfiles`;
- detect a matching machine profile by hostname;
- ask the remaining questions.

#### Scenario: Matching profile
- **WHEN** the hostname is `work-mbp` and `machines/work-mbp.toml` exists
- **THEN** init sets `profile = "work-mbp"` and does not ask again about anything the profile already answers

### Requirement: Recommended defaults are data
Recommended bundles and tags SHALL come from `recommended` markers in `dots.toml`, set per platform. Recommended setting values SHALL be their defaults. No onboarding answer SHALL be hard-coded in the binary.

#### Scenario: Platform-specific recommendation
- **WHEN** the `mac-desktop` bundle is recommended only for `macos`
- **THEN** on Linux it is not offered as a default

### Requirement: Bootstrap installer
The published installer SHALL download the right static binary for the OS and architecture, verify its checksum, and place it in `~/.local/bin`. dots SHALL be able to manage itself as an `archive` (alias `github-release`) package.

#### Scenario: Checksum failure
- **WHEN** the downloaded binary does not match the published checksum
- **THEN** the installer aborts and installs nothing

### Requirement: Skill install
`dots skill install [--agent <id>...]` SHALL install the bundled dots agent skill into each selected agent's skills dir. The default is every detected agent.

#### Scenario: Claude only
- **WHEN** `dots skill install --agent claude` runs
- **THEN** `~/.claude/skills/dots/SKILL.md` exists and matches the bundled skill
