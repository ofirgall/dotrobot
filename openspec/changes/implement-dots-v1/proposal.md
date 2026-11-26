# Proposal

## Why

`dots` exists only as a design: `ARCHITECTURE.md`, `UX.md`, the agent skill and the README. There is no code yet. Before anyone writes Rust, every surface a user, script or agent can touch has to be pinned down as a testable contract. That covers commands, flags, file schemas, JSON output, exit codes, state files and provider behavior. Without that, an implementation would drift from a design that took many rounds to converge. This change turns the design into that set of contracts and a phased plan to build all of it.

## What Changes

- Define the complete `dots` CLI contract:
  - every command and subcommand, with its arguments and flags;
  - global flags and environment variables;
  - exit codes;
  - one versioned JSON output envelope shared by all commands.
- Define the on-disk contracts and their strict validation rules:
  - `dots.toml`, `package.toml`, bundle files, `machine.toml`, `dots.lock`;
  - override files and manual-action frontmatter;
  - the state dir (`state.json`, `journal.jsonl`, logs, backups, `gen/`).
- Define the observable behavior of these pieces as scenarios that become acceptance tests:
  - the platform chain and the condition language;
  - package resolution and the version grammar;
  - provider selection and the provider contract (built-in and user-defined providers, the script runtime contract, browser extensions, agent skills);
  - the plan/apply engine;
  - file modes, merges, directives, patches and fragments;
  - system settings (`defaults`, registry, `xdg`) and services;
  - app plugin managers as declarative providers, bundled provider definitions, apt repositories, fonts, archives and VS Code extensions;
  - settings, overrides, sharing (`add`/`pull`/`eject`), integrity, status and inventory, manual actions, history, worktrees, environment, onboarding and dotbot import;
  - terminal rendering.
- Phase the build into milestones:
  - **v0.1 Core**: packages, bundles, brew/apt/cargo/uv, links and renders, `status`/`plan`/`apply`.
  - **v0.2 Customize**: settings, overrides, platform tables, lock.
  - **v0.3 Share**: `add`/`pull`, manual steps, worktrees, history, integrity.
  - **v0.4 Breadth**: remaining providers, browser extensions, agent skills, dotbot import, Windows.
- Resolve the three open questions in `ARCHITECTURE.md` §21. The decisions are recorded in design.md.

There are no breaking changes, because nothing has shipped. The contracts become the compatibility baseline: from v1 on, every change to a JSON schema, exit code or file schema has to be versioned.

## Capabilities

### New Capabilities

- `cli-surface`: command tree, global flags, environment variables, exit codes, the JSON envelope, shell completions.
- `terminal-ui`: rendering tiers, color/symbol vocabulary, hints, prompts, pager, notifications. These are the observable rules from `UX.md`.
- `repo-schema`: the repo layout and the `dots.toml`, bundle, package and `machine.toml` schemas, with strict parsing and schema versioning.
- `platform-conditions`: facts, the platform chain, `[platform."<key>"]` tables, tags, and the `cfg()`-style condition language.
- `package-resolution`: bundle expansion, `requires`/`after`, the version grammar and intersection, binary collisions, cycles, provider selection.
- `provider-contract`: the behavior every provider must expose, the built-in provider set (including `archive`, `font`, `vscode-ext`, apt repositories and cargo/uv `path`/`git` sources), user-defined declarative providers and their extensions, bundled provider definitions, app plugin review, and the script runtime contract (including `fetch`).
- `browser-extensions`: the `browser-ext` provider (install methods, pending restart, the auto-attached enable step).
- `agent-skill-packages`: the `agent-skill` provider (agent targets, frontmatter overrides, body patches).
- `lockfile`: `dots.lock` format, platform keying, lock-vs-spec rules, `upgrade`, `pin`/`unpin`, `lock --fix`.
- `reconcile-engine`: `plan` and `apply`, including action ordering, batching, one sudo prompt, interactive deferral, failure propagation, the run lock, dry-run and the `--home` sandbox.
- `file-management`: file modes, layers, format-aware merges, directives, patch layers, fragments, drift states, `adopt`, backups and `restore`.
- `package-settings`: setting declarations, types, precedence scopes, `config` commands, `questions`.
- `overrides`: object addresses, the override grammar, locations and precedence, `show --effective`, the `override` commands, `eject`, stale-override lint.
- `sharing-sources`: `add bundle|pkg`, follow vs `--copy`, `search`, `sources`, `pull`, `eject`/`uneject`, the real-machine preview, trust and script policy.
- `integrity`: content hashes in the lock, deployed-file hashes, author pins, the warn/enforce policy, `tampered`, `verify`.
- `status-inventory`: `dots`, `status`, `outdated`, `why`, `bins`, `which`, `doctor`, `facts`.
- `manual-actions`: manual-action files, `todo` commands, self-checks, revisions.
- `history`: the journal JSONL schema, git trailers, `log`.
- `worktrees`: the active-root indirection, `try`/`use`/`roots`/`diff`, reconciling on switch.
- `environment`: generated env files, the single rc inject, PATH ordering.
- `onboarding`: `init`, `questions`, recommended defaults, `skill install`.
- `dotbot-import`: `import dotbot` and `import scan`, including the proposals for repositories, fonts, plugins, settings and services.
- `system-settings`: `[[defaults]]`, `[[registry]]` and `[[xdg]]`: read-before-write, drift, adopt, restore of original values, restart aggregation.
- `services`: `[service]`: manager auto-selection, enabled and running, restart on config change, status.

### Modified Capabilities

None. `openspec/specs/` is empty.

## Impact

- **Code**: a new Rust workspace (binary `dots`). Nothing exists today.
- **Docs**:
  - `ARCHITECTURE.md` §21 gets its decisions.
  - `README.md`'s console blocks become acceptance tests.
  - `skill/dots/SKILL.md` must stay consistent with the command contracts.
- **External systems**: drives brew, apt (including its repositories), cargo, uv, npm, git, the GitHub releases API, `defaults`, the registry, `xdg-mime`, brew services, launchd, systemd, app plugin CLIs such as `herdr`, winget/scoop/mas, browser profile and policy files, and agent skill dirs. It never assumes a provider is on `PATH`.
- **Compatibility baseline**: JSON schemas (`dots/<command>@<n>`), exit codes and file `schema = 1` are frozen at v1. Changing any of them later requires a version bump.
