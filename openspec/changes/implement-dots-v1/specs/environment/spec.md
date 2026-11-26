# Spec Delta

## Purpose

Defines how packages contribute PATH entries and environment variables, how dots generates one env file per shell, and how dots keeps a single injected source line in the user's shell rc.

## ADDED Requirements

### Requirement: Env contributions
A package's `[env]` table SHALL accept:
- `path`: entries prepended to PATH, ordered by package priority
- `vars`
- `source`: last-resort scripts to source

#### Scenario: Cargo bin on PATH
- **WHEN** the rust package contributes `path = ["~/.cargo/bin"]`
- **THEN** the generated env prepends `~/.cargo/bin`

### Requirement: Generated env files
dots SHALL generate `env.sh`, `env.fish` and `env.ps1` under the state dir from every active package's contributions. A package that becomes inert or is removed SHALL drop out of the next generation.

#### Scenario: Removed package
- **WHEN** a package contributing `NPM_CONFIG_PREFIX` is removed
- **THEN** the next apply regenerates the env without that variable

### Requirement: Single rc inject
dots SHALL keep exactly one marked block, `# >>> dots env >>>` … `# <<< dots env <<<`, in the user's shell rc. The block SHALL source the generated env file. Text outside the markers SHALL never be modified.

#### Scenario: Idempotent inject
- **WHEN** apply runs twice
- **THEN** the rc contains the block exactly once

### Requirement: Same env during runs
Actions in a run SHALL see the environment that the packages installed earlier in the same run produced.

#### Scenario: Fresh tool on PATH
- **WHEN** rustup is installed and a cargo package follows in the same run
- **THEN** the cargo action finds `cargo` without a new shell
