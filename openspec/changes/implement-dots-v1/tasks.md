# Tasks

## 1. Contracts and harness (before any feature)

- [x] 1.1 Create the Rust workspace and a `dots` binary that prints `--version`. Verify with `cargo build` and `dots --version`.
- [x] 1.2 Write the full command tree and every signature from `cli-surface` as argument definitions, with stub handlers that return "not implemented". Verify that `dots <cmd> --help` matches the signature table in a golden test.
- [x] 1.3 Implement the global flags, the environment variables and the exit codes (0/1/2/3/64/75/130). Verify that a typo exits 64 with a suggestion, and that `--files-only --packages-only` exits 64.
- [x] 1.4 Define the report model and the JSON envelope (`schema`, `ok`, `data`, `warnings`, `errors`), and add `schemas/` with one JSON Schema per command. Verify with a test that validates every stub's `--json` output against its schema.
- [x] 1.5 Create the error-code registry and generate `docs/errors.md` from it. Verify with a test that every code has a message and a hint, and that the doc is up to date.
- [x] 1.6 Fix the state, config, cache and data dir layout (D6), honoring `DOTS_*_DIR`, XDG and `--home`. Verify with a test that `--home /tmp/h` writes nothing outside `/tmp/h`.
- [x] 1.7 Build the fake provider and the golden-output harness. The harness runs a command under `--home` with fake providers and snapshots its rich, plain and JSON output. Verify with a smoke golden test of `dots --help`.
- [x] 1.8 Update `ARCHITECTURE.md` §12 and §21 with decisions D8 and D9, and mark §21 resolved. Verify that the docs contain no remaining "open question" for these items.

## 2. v0.1: schemas, facts and resolution

- [x] 2.1 Implement strict parsing for `dots.toml`, bundles, packages and `machine.toml`, with `schema = 1`, "did you mean" hints and `file:line:col`. Verify the `repo-schema` scenarios: unknown key, future schema, name mismatch and escaping path.
- [x] 2.2 Implement machine profiles merged under the local `machine.toml`. Verify the profile-plus-tweak scenario.
- [x] 2.3 Implement fact detection and `dots facts`. Verify the `platform-conditions` fact scenarios (WSL2, forced no-sudo) using fixture `/etc/os-release` and `/proc` files.
- [x] 2.4 Implement the platform chain, including custom platforms and `platform_extra`. Verify the Ubuntu 24.04, Mint and Pop chain scenarios.
- [x] 2.5 Implement the condition language (parse and evaluate, with errors for unknown keys and tags). Verify the is-a, version-comparison and unknown-tag scenarios.
- [x] 2.6 Implement bundle expansion, `requires`, `exclude`, inert packages and `dots why`. Verify the `package-resolution` transitive-requirement and duplicate-inclusion scenarios.
- [x] 2.7 Implement provider selection (prefer, priority, availability, sudo), bootstrap injection and sticky choice. Verify the no-sudo, cargo-without-rustup and existing-apt-install scenarios against the fake provider.
- [x] 2.8 Implement binary collisions (`conflicts`, `replaces`) and cycle detection. Verify the replaces and two-node-cycle scenarios.
- [x] 2.9 Implement `dots lint` for the v0.1 checks. Verify the unknown-tag lint scenario. Document `lint` in `docs/commands.md`.

## 3. v0.1: providers

- [x] 3.1 Write the provider conformance suite (batched `installed`, pure `plan`, capabilities, own-binary resolution). Verify that the fake provider passes it.
- [x] 3.2 Implement `brew`. Verify that the conformance suite passes on macOS CI, and the fresh-Mac re-resolution scenario.
- [x] 3.3 Implement `apt`. Verify that the conformance suite passes in Ubuntu 22.04 and 24.04 containers.
- [x] 3.4 Implement `cargo` and `uv` (including uv library mode). Verify that the conformance suite passes, and the uv library-mode scenario.
- [x] 3.5 Implement `git`. Verify that the conformance suite passes with a local bare repo fixture.
- [x] 3.6 Implement the `script` provider and its runtime contract (working dir, stdin, env vars, logs, timeout, check after install). Verify the "install that lies", script-variables, stdin and hung-installer scenarios.
- [x] 3.7 Implement user-defined declarative providers. Verify the Go provider plan scenario.
- [x] 3.8 Document the provider contract and the script environment in `docs/providers.md`. Verify that every variable in the spec appears in the doc.

## 4. v0.1: engine, files, status, environment

- [x] 4.1 Implement `plan` and `apply` with ordering, batching, per-provider exclusive locks, parallel independent providers and `--dry-run`. Verify the plan-then-apply and config-after-install scenarios.
- [x] 4.2 Implement the confirmation policy, one sudo prompt, interactive deferral, `--no-input` and non-TTY failure. Verify the file-only-plan, several-apt-installs, unattended-run and non-TTY scenarios.
- [x] 4.3 Implement failure isolation, the run lock (exit 75), crash-safe state writes and selective apply. Verify the one-package-fails, concurrent-apply, interrupted-run and one-package scenarios.
- [x] 4.4 Implement file modes (`link` through `active`, `copy`, `render`, `inject`, `dir`), path variables and per-platform `dst`. Verify the per-platform-destination and link-target scenarios.
- [x] 4.5 Implement layers (explicit and auto), format-aware merges (toml, json, jsonc, yaml, ini/gitconfig, markdown, text), merge strategies, delete markers, type-mismatch errors, native includes and generated headers. Verify the array-replace, merge-by-key, ghostty-include and header scenarios.
- [x] 4.6 Implement ownership, atomic writes, backups and `dots restore`. Verify the first-apply-over-existing-file and crash-during-write scenarios.
- [x] 4.7 Implement the file drift states (except `tampered`). Verify the app-rewrote-config scenario.
- [x] 4.8 Implement `status` (summary, `--all`, deep view), `outdated` with the latest-version cache and `--offline`, `bins`, `which` and `doctor`. Verify every `status-inventory` scenario, and the 100-package benchmark under 1.5 s.
- [x] 4.9 Implement env generation (`sh`, `fish`, `ps1`), the single rc inject, and in-run env propagation. Verify every `environment` scenario.
- [x] 4.10 Implement the rendering tiers, the state vocabulary, hints, pager, responsive width and `ui.toml`. Verify every `terminal-ui` scenario, with golden output in all three tiers.
- [x] 4.11 Implement `dots init` (new repo, `--from`, `--defaults`). Verify the non-interactive-init and matching-profile scenarios.
- [x] 4.12 Run the v0.1 README console blocks as golden tests. Verify that they all pass, and update the README roadmap checkbox.

## 5. v0.2: customize

- [x] 5.1 Implement platform tables on every supported object, with merge-before-`when`. Verify the release-specific-override, unknown-platform and platform-changes-existence scenarios.
- [x] 5.2 Implement settings (declarations, types, validation, the 6-scope precedence, secrets and required-blocks-render). Verify every `package-settings` scenario except `questions`.
- [x] 5.3 Implement the `dots config` subcommands and `dots questions`, with stable JSON. Verify the explain and accept-defaults scenarios, and that `questions --json` passes schema validation.
- [x] 5.4 Implement directives (`dots:if`/`elif`/`else`/`end`, `dots:if!`, `dots:include`, `@dots()@` with filters) and `[[files.set]]` structured patches. Verify the inline-setting, unknown-name and delete-by-condition scenarios.
- [x] 5.5 Implement fragments, with collision lint. Verify the gitconfig-composition scenario.
- [x] 5.6 Implement overrides: addresses, the grammar, three locations, the precedence order and the identity-field guard. Verify the edit-a-bundle's-list, identity-field and machine-only-tweak scenarios.
- [x] 5.7 Implement `show --effective` with provenance, the `override list`/`edit`/`diff` commands, the status customization marker, and stale and redundant override lint. Verify the annotated-output, scaffold, overridden-package and renamed-target scenarios.
- [x] 5.8 Implement the version grammar, constraint intersection with provenance errors, and constraint-aware provider selection. Verify the invalid-spec, prefix-and-floor, conflict-with-provenance, provider-cannot-satisfy and override-cannot-loosen scenarios.
- [x] 5.9 Implement `dots.lock` (format, platform keying, the lock-vs-spec rules, no implicit downgrades), `upgrade`, `pin`/`unpin` and `lock --fix`. Verify every `lockfile` scenario.
- [x] 5.10 Implement `dots adopt` (`--layer`, `--machine`, 3-way merge for text). Verify the adopt-into-machine-scope scenario.
- [x] 5.11 Implement `dots migrate` and orphan/unmanaged classification. Verify the brew-dependency-is-not-an-orphan scenario. Run the v0.2 README blocks as golden tests.
- [x] 5.12 Implement apt repositories (`repo` as PPA or url with key and fingerprint, optional `pin`, owned keyring, sources and preferences files, reference-counted removal, one `apt-get update` on change), cargo/uv `path` and `git` sources with `git_fetch_with_cli`, and script `fetch` with the unpinned `curl | sh` lint warning. Verify the PPA-installs-after-one-apt-get-update, fingerprint-mismatch-refuses, repository-removed-with-its-last-package, local-crate-goes-outdated, pinned-installer-runs and unpinned-curl-pipe-sh scenarios.
- [x] 5.13 Implement the `terminfo` file mode (`tic -x -o ~/.terminfo`, `infocmp -x` check). Verify the compile-on-apply, already-installed and source-changed scenarios.
- [x] 5.14 Implement `system-settings` (`[[defaults]]`, `[[registry]]`, `[[xdg]]`): batched read-before-write, drift and adopt, original-value ledger and restore, aggregated restarts, root entries in the single sudo prompt, and the Settings step in the pipeline order. Verify every `system-settings` scenario with fake `defaults`, registry and `xdg-mime` backends.
- [x] 5.15 Implement `services` (`[service]`: manager auto-selection, enabled and running, restart once on config change, status with one call per manager) and the Services pipeline step. Verify every `services` scenario and the `reconcile-engine` service-after-its-config scenario.

## 6. v0.3: share, integrity, history

- [ ] 6.1 Implement source parsing (`owner/repo`, `github:…@ref`, `git+ssh`, local path), the remote mirror cache, and `dots search`. Verify against a local fixture remote.
- [ ] 6.2 Implement `add bundle|pkg` in follow mode: upstream entries, lock pin, namespacing, `--as`, the wrong-kind error and upstream resolution isolation (chains, cycles). Verify the follow-a-bundle, wrong-kind, local-name-taken and chained-upstreams scenarios.
- [ ] 6.3 Implement `add --copy` with `[origin]`. Verify the source-disappears scenario.
- [ ] 6.4 Implement the real-machine preview (package and file verdicts, the headline counts, read-only, `--dry-run`, `--exclude`). Verify the existing-unmanaged-install, replace-with-diff and unknown-copy-on-PATH scenarios.
- [ ] 6.5 Implement the code review (scripts, hooks, checks and skills, with line counts and the `v` viewer), `trust = "review"` and `scripts = "deny"`. Verify the pull-with-changed-script and declarative-only-upstream scenarios.
- [ ] 6.6 Implement `dots sources` and `dots pull` (followed rev bump, 3-way merge for copies, stale-override stop, `--to`). Verify the behind-upstream, stale-override-stops-pull and pin-to-rev scenarios.
- [ ] 6.7 Implement `eject` and `uneject`. Verify the eject-followed-package and free-text-edit-blocks-uneject scenarios.
- [ ] 6.8 Implement patch layers (no fuzz, atomic, the `patch-does-not-apply` error) and `adopt --as-patch`. Verify the stale-patch scenario.
- [ ] 6.9 Implement integrity: content tree hashes in the lock, deployed-file hashes, download verification, author pins, the warn/enforce policy, the `tampered` state, and `dots verify`. Verify every `integrity` scenario.
- [ ] 6.10 Implement manual actions (discovery, per-machine state, revisions, checks, `todo` subcommands, apply summary). Verify every `manual-actions` scenario.
- [ ] 6.11 Implement the journal, git trailers with `--commit` on repo-changing commands, and `dots log`. Verify every `history` scenario, including add-without-commit.
- [ ] 6.12 Implement worktrees (`active` swap, `try`, `use`, `use -`, `roots`, `diff`, trial-package prompts, dangling hint). Verify every `worktrees` scenario. Run the v0.3 README blocks as golden tests.

## 7. v0.4: breadth

- [ ] 7.1 Implement `npm`, `archive` with its `github-release` alias (URL templates, asset regex and types, extraction, checksums, recorded hashes) and `mas`. Verify that the conformance suite passes, and the republished-asset scenario.
- [ ] 7.2 Implement `browser-ext` (keys, external and policy methods, the shared policy file, installed and pending-restart state, the enable todo, latest-only versions). Verify every `browser-extensions` scenario against profile fixtures.
- [ ] 7.3 Implement `agent-skill` (agent targets, frontmatter overrides, patches, review, enforce by default). Verify every `agent-skill-packages` scenario.
- [ ] 7.4 Implement `dots skill install`. Verify the claude-only scenario.
- [ ] 7.5 Implement Windows support: winget and scoop providers, the symlink probe with hardlink and junction fallback, `env.ps1`, and the `doctor` Developer Mode hint. Verify that the conformance suite and the file-mode tests pass on Windows CI.
- [ ] 7.6 Implement `import dotbot` and `import scan`, including the scan proposals for apt repositories, cargo/uv sources, archives, fonts, VS Code extensions, bundled provider commands, `curl | sh`, settings, services, terminfo and account changes. Verify every `dotbot-import` scenario against a copy of real dotbot configs.
- [ ] 7.7 Implement shell completions with dynamic values. Verify the completing-a-package-name scenario for zsh, bash and fish.
- [ ] 7.8 Publish release binaries (macOS universal, Linux musl, Windows MSVC), checksums and the installer, and make dots self-managed as an `archive` package. Verify the installer checksum-failure scenario.
- [ ] 7.9 Implement `font` (per-OS font directories, owned files, one `fc-cache -f` per run, Windows registry values, ledger-based `installed()`). Verify the font-installed-once-per-run-fc-cache, font-file-removed and brew-cask-on-macOS scenarios.
- [ ] 7.10 Implement `vscode-ext` (single batched list call, `bin` for forks, pinned versions). Verify the batched-read and fork-binary scenarios, and the `.vsix` hand-off from `archive`.
- [ ] 7.11 Implement the declarative provider extensions (`upgrade`/`uninstall`/`latest`, per-source-kind command tables, multi-field `json:` projections with `enabled`, provider `vars`, `review` manifests, package-scoped providers), app plugin review in `add`/`pull`, and the bundled definitions `gh-ext`, `herdr-plugin`, `pi-package`, `hyprpm` and `psmodule` enabled by `[providers] use`. Verify the `provider-contract` herdr scenarios (herdr plugin pinned to rev, one batched read, plugin id differs, disabled is not missing, upgrade moves the rev, plugin config path variable, review before install, scripts denied) with a fake `herdr` on `PATH`, and the bundled-definition conformance fixtures.

## 8. Integration

- [ ] 8.1 Run every scenario of all 24 specs as an end-to-end suite in the `--home` sandbox, plus a real-provider smoke run on macOS, Ubuntu 22.04, Ubuntu 24.04 and Windows. Verify that it is green on CI.
- [ ] 8.2 Migrate the author's `~/dotfiles` with `import dotbot` on a scratch machine, until `dots status` is all green. Verify that `status` exits 0 and `verify` exits 0.
- [ ] 8.3 Check that `skill/dots/SKILL.md`, `README.md` and `docs/` match the shipped commands and error codes. Verify with a test that every command and error code named in those files exists.

## Workflow follow-up

- Archive the change once v0.4 is complete and the integration group is green.
- After archiving, confirm that `openspec/specs/` contains all 24 capabilities.
