# Design

## Context

- **Starting point.** Nothing is implemented. The design lives in `ARCHITECTURE.md` (§0–§21), `UX.md` and `skill/dots/SKILL.md`. The README's console blocks show the intended output.
- **This change.** It turns those documents into contracts. The specs under `specs/` are the requirements. This document covers only the cross-cutting decisions that make the contracts consistent, testable and stable.
- **Motivation.** See proposal.md, section "Why".
- **Constraints:**
  - one static binary for macOS, Linux and Windows;
  - no runtime dependencies beyond the package managers it drives;
  - every surface usable by people, by scripts (exit codes, JSON) and by agents (the skill, `questions`, `--json`).

## Goals / Non-Goals

**Goals:**
- **Freeze the observable contracts before building:** commands, flags, environment variables, exit codes, the JSON envelope and error codes, every file schema, the state-dir layout, the provider behavior contract, and the script runtime environment.
- Make every contract **machine-checkable**: JSON Schemas, golden output files and acceptance scenarios.
- Order the work so each milestone ships a usable tool, and later milestones only add to it.

**Non-Goals:**
- Choosing internal module or type names, or algorithms, beyond what the contracts require. Implementers are free there.
- Everything in `ARCHITECTURE.md` §20: secrets, system configuration beyond packages and files, remote orchestration, guaranteed rollback of system packages, and Nix-level reproducibility.
- A full-screen TUI (`dots ui`). Decided below: it comes later.

## Decisions

### D1. Commands return reports; renderers print them
Every command produces one typed report. Three renderers consume it: rich, plain and JSON. No command writes to the terminal directly.
- **Why:** this is the only way to guarantee `cli-surface` "one JSON document, nothing else on stdout" and the `terminal-ui` shared state vocabulary across 40 commands.
- **Alternative rejected:** per-command printing with a `--json` branch. It drifts, and the two outputs disagree.

### D2. JSON schemas are versioned per command and published
- Each command's output is `dots/<command>@<n>`, described by a JSON Schema file committed under `schemas/`.
- Adding a field does not bump `<n>`. Removing, renaming or changing the meaning of a field does.
- CI validates every golden JSON output against its schema.
- **Why:** status bars and agents parse this output, so breaking them silently is the worst failure.
- **Alternative rejected:** one global schema version. A single field change would then break every consumer.

### D3. Exit codes avoid clashing with "drift = 2"
- Usage errors use 64 (`EX_USAGE`) instead of the conventional 2, because 2 already means "drift present" for prompts and CI.
- A busy lock is 75 (`EX_TEMPFAIL`), and an interrupt is 130.
- **Alternative rejected:** usage = 2 and drift = 10. "Drift = 2" was already documented in `ARCHITECTURE.md` §7.2 and the skill.

### D4. A stable error-code registry
- Every error has a kebab-case code, a message template, a hint template and the provenance fields it carries.
- One registry file feeds three things: `docs/errors.md`, the JSON `errors[].code` value, and a test that every code is reachable from at least one fixture.
- **Why:** agents branch on codes rather than on English text, and the skill's debugging table maps codes to fixes.

### D5. The provider contract is the extension seam, with a conformance kit
- The provider operations in `provider-contract` (`availability`, `installed`, `latest`, `plan`, `exec`, `bins`, `owner_of`, plus capabilities) are a stable internal boundary.
- One shared conformance test suite runs against every provider:
  - real providers run it in containers or macOS CI;
  - a **fake provider** runs it in memory. The fake also drives every engine, status and preview test.
- **Why:** the engine is then tested without touching real package managers, and adding winget, pacman and others later is a contained task.
- **Alternative rejected:** testing the engine against real brew and apt only. It is slow, flaky, and impossible to run for Windows on a laptop.

### D6. The state dir is a contract too
- The layout is fixed:
  - `<state>/state.json`, schema-versioned and rewritten atomically after each action;
  - `journal.jsonl`;
  - `logs/<run-id>/`, `backup/<run-id>/`, `gen/<root-id>/`, `active`, `env.{sh,fish,ps1}`;
  - a `run.lock`.
- The data dir holds `upstreams/<alias>/<rev>/`. The cache dir holds `remotes/` and `latest/`.
- **Why:** `--home` sandboxing, `restore`, worktree switching and crash safety all depend on the layout. Making it a contract lets `doctor` check it and lets tests assert on it.

### D7. The README and specs are the acceptance suite
- Every scenario in `specs/` becomes at least one test.
- Every console block in `README.md` becomes a golden test that runs in a `--home` sandbox with fake providers, once per tier: rich, plain and JSON.
- **Why:** the README promises behavior, and this keeps that promise honest. The `README` judge flagged a spec-only feel, and passing acceptance tests are the answer to it.

### D8. Nothing commits without `--commit`
- Repo-changing commands (`upgrade`, `pin`, `unpin`, `add`, `pull`, `eject`, `uneject`, `remove`, `config set --repo`) edit files and leave them uncommitted, unless `--commit` is passed.
- This **resolves `ARCHITECTURE.md` §21 Q1**, choosing the proposed answer and extending it to `add` and `pull`. That makes the README's "Your repo gains one entry" literally true, and leaves the commit to the user.
- `ARCHITECTURE.md` §12 is updated to match.

### D9. Machine profiles are kept, and the TUI is deferred
- **Resolves §21 Q2:** both committed `machines/<name>.toml` profiles and a local `machine.toml` are supported. The local file wins key by key, as `repo-schema` specifies.
- **Resolves §21 Q3:** a full-screen TUI is out of v1. `--json` first, so a later TUI consumes the same reports.

### D10. Order of trust
- Code from a source never runs during a preview, `search` or `sources`.
- Scripts, hooks, checks and skills from a source are shown before they first run. Changes to them are shown first on every pull.
- `scripts = "deny"` and `integrity = "enforce"` are per-source policies.
- Scripts are not sandboxed, and the docs say so plainly.
- **Alternative rejected:** sandboxing scripts. Not portable, and it gives a false sense of safety.

### D11. Milestones map to capabilities

| Milestone | Capabilities |
|---|---|
| **v0.1 Core** | `cli-surface`, `terminal-ui`, `repo-schema`, `platform-conditions` (facts and chain; no platform tables yet), `package-resolution` (without versions), `provider-contract` (brew, apt, cargo, uv, git, script, custom), `reconcile-engine`, `file-management` (link, copy, render, inject, dir, layers, merges), `status-inventory`, `environment`, `onboarding` (`init`) |
| **v0.2 Customize** | apt repositories, cargo/uv `path`/`git` sources, script `fetch`, the `terminfo` file mode, `system-settings`, `services`, platform tables, `package-settings`, directives and structured patches, `overrides`, `lockfile`, the version grammar and intersection, fragments, `adopt` |
| **v0.3 Share** | `sharing-sources`, `integrity`, patch layers, `manual-actions`, `history`, `worktrees` |
| **v0.4 Breadth** | npm, `archive`, `font`, `vscode-ext`, declarative provider extensions and bundled definitions, mas, `browser-extensions`, `agent-skill-packages`, `dotbot-import`, Windows (winget, scoop, junctions), `skill install` |

Each milestone ends with every spec scenario in its scope passing, and with every README block for those features passing.

### D12. App plugin managers are providers; settings and services are declarations
- An app's plugin manager is a **provider** and each plugin is a **package** (herdr, `ya pkg`, `gh extension`). The app keeps its own plugin directory, lockfile and build steps. dots decides which plugins exist, at which rev, and checks them. This needs general declarative provider extensions (per-source-kind commands, multi-field `json:` projections, provider path variables, a `review` manifest reader, package-scoped providers), not Rust code per app. Common ones ship as bundled definitions enabled by `[providers] use`.
- When the app reads a config file to manage its own plugins (lazy.nvim, tpm), there is **no provider**: the file is a normal `[[files]]` entry and a `post_files` hook runs the manager's sync.
- System settings (`[[defaults]]`, `[[registry]]`, `[[xdg]]`) and services (`[service]`) are **declarations on a package, not providers**. They install nothing and have no `latest`. They are state with drift, ownership and restore, like files, and they run in the pipeline after files (Files, Settings, Services, Hooks, Env, Todos). Account changes (`chsh`, groups) stay manual actions with checks.
- **Why:** providers answer "is this software installed at this version?". A preference value or a running service has a different lifecycle, and forcing them into the provider trait would add fake versions and fake batching.
- **Alternative rejected:** a Rust provider per app plugin manager. It does not scale, and the declarative form is what users already write for go or gem.
- **Alternative rejected:** a `defaults` or `service` provider. It would put drift and restore behavior in the wrong layer.

## Risks / Trade-offs

- **[Scope: 24 capabilities]** → Milestones ship usable subsets. The contracts are frozen up front, so later milestones extend without breaking anything.
- **[brew/apt cannot pin exact versions]** → The lock says so openly (`drift`, `Limited`), and `--strict` turns this into an error for people who care.
- **[Text-config merging is fragile]** → Prefer native includes, then directives, then patch layers that fail loudly. Never fuzzy-merge text.
- **[Many interactive prompts in automated runs]** → `--no-input` and the non-TTY rule fail fast with a hint instead of hanging.
- **[Browser policy locations change between versions]** → Each location sits behind the provider contract with its own conformance tests. `status` re-checks paths that the browser can overwrite.
- **[Windows symlink permissions]** → Fall back to hardlinks and junctions, record the link kind, and have `doctor` explain Developer Mode.
- **[Frozen JSON schemas limit later changes]** → Additive changes are free. Breaking changes need a per-command version bump.

## Migration Plan

There is no existing user base. Migrating the author's `~/dotfiles` follows `ARCHITECTURE.md` §16 and the `dotbot-import` spec, and it is the first real-world acceptance run after v0.4. Rollback is per machine: `dots restore <run-id>` puts back any file dots replaced, and nothing it installs is removed unless the user asks.
