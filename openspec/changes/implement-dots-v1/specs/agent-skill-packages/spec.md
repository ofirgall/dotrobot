# Spec Delta

## Purpose

Defines agent skills as first-class packages via the `agent-skill` provider. They install into each agent's skills directory, and their options (frontmatter) and bodies can be overridden or patched without editing the source.

## ADDED Requirements

### Requirement: Skill package keys
`[providers.agent-skill]` SHALL accept:
- `src`: the skill directory inside the package;
- `agents`: defaults to every supported agent found on the machine;
- `[providers.agent-skill.frontmatter]`.

#### Scenario: Default agents
- **WHEN** `agents` is omitted and both `~/.claude` and `~/.codex` exist
- **THEN** the skill is installed for both agents

### Requirement: Agent targets
Built-in targets SHALL be:
- `claude`: `~/.claude/skills/<name>/`
- `codex`: `~/.codex/skills/<name>/`

`dots.toml` `[provider.agent-skill.agents]` SHALL be able to add targets.

#### Scenario: Custom agent
- **WHEN** `[provider.agent-skill.agents] myagent = "~/.myagent/skills"`
- **THEN** a package with `agents = ["myagent"]` installs to `~/.myagent/skills/<name>/`

### Requirement: Frontmatter is overridable
`SKILL.md` SHALL be rendered so that its frontmatter equals the source frontmatter deep-merged with the `frontmatter` table, with overrides applied using the standard grammar. Frontmatter keys dots does not know SHALL pass through unchanged.

#### Scenario: Disable model invocation
- **WHEN** an override sets `[providers.agent-skill.frontmatter] disable-model-invocation = true`
- **THEN** the installed `SKILL.md` frontmatter contains `disable-model-invocation: true`, and its body is unchanged

#### Scenario: Removing an allowed tool
- **WHEN** an override sets `allowed-tools = { remove = ["Bash"] }`
- **THEN** `Bash` is absent from the installed frontmatter

### Requirement: Body and script patches
A `*.patch` layer SHALL be able to modify `SKILL.md` or any file in the skill. A patch that does not apply SHALL fail with `patch-does-not-apply` and SHALL never be partially applied.

#### Scenario: Patch goes stale on pull
- **WHEN** `dots pull` brings a `SKILL.md` that the local patch no longer matches
- **THEN** the pull stops, naming the patch and the first hunk that failed

### Requirement: Skills are reviewed and integrity-enforced
New or changed skills SHALL be listed in every `add` and `pull` review, next to scripts. Skill files SHALL default to `integrity = "enforce"`.

#### Scenario: Skill edited on disk
- **WHEN** an installed skill file is edited outside dots
- **THEN** status shows `tampered`, and the next apply restores it
