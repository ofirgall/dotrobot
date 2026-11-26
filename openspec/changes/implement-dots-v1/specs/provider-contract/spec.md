# Spec Delta

## Purpose

Defines the behavior every package provider must expose, the built-in provider set with its capabilities, user-defined declarative providers, and the runtime contract for scripts. A new package manager can then be added without changing the rest of dots.

## ADDED Requirements

### Requirement: Provider operations
Every provider SHALL expose these operations:
- `availability`: `Ready`, `Bootstrap(<pkg>)` or `Unsupported`
- `installed`: one batched query returning name, version, pinned and source
- `latest`: batched and cacheable
- `plan`: pure, turning install, upgrade, remove, pin and unpin operations into the exact commands
- `exec`
- `bins`: the executables each package installed
- `owner_of`: optional reverse lookup from a path to its package

#### Scenario: Batched query
- **WHEN** `dots status` runs with 60 brew packages
- **THEN** brew is queried for installed packages exactly once

#### Scenario: Plan shows exact commands
- **WHEN** `dots plan -v` runs
- **THEN** each action shows the exact command line `exec` would run

### Requirement: Capabilities are declared
Each provider SHALL declare these capabilities:
- `batch_install`
- `exact_version`: `Yes`, `Limited` or `No`
- `pin`, `needs_root`, `uninstall`, `exclusive_lock`

Behavior that depends on a capability SHALL be visible to the user. When a provider cannot uninstall, the plan SHALL say "cannot uninstall".

#### Scenario: Exclusive lock
- **WHEN** two brew batches and one cargo batch are planned
- **THEN** the brew batches run one after the other, and cargo runs in parallel with them

### Requirement: Provider binary resolution
Providers SHALL find their own binary in known locations rather than relying on `PATH`. After a provider is bootstrapped, availability SHALL be checked again within the same run.

#### Scenario: Fresh Mac
- **WHEN** Homebrew was just installed into `/opt/homebrew` during the run
- **THEN** later brew actions in the same run succeed without a new shell

### Requirement: Built-in providers
v1 SHALL ship these providers:
- `brew`, `apt`, `cargo`, `uv`, `npm`, `git`
- `archive` (alias `github-release`), `font`, `vscode-ext`, `script`
- `winget`, `scoop`, `mas`
- `browser-ext`, `agent-skill`

`pip` SHALL NOT exist as a provider. Python CLIs install through `uv`.

#### Scenario: uv library mode
- **WHEN** a package declares `[providers.uv] kind = "lib", python = "system"`
- **THEN** it installs with uv into the system Python, and that is shown in the plan

### Requirement: Archive and release downloads
`archive` (alias `github-release`) SHALL take a `url` template or a GitHub `source`. It SHALL:
- pick the release asset by `asset` regex, per OS and architecture;
- verify the published checksum when one exists;
- otherwise record the SHA-256 on first install;
- refuse a changed hash under the same tag;
- choose the install step by asset type: archive, single binary, `.deb` (root) or `.vsix` (handed to `vscode-ext`);
- extract into an owned user-level directory (`extract`), without sudo.

#### Scenario: Republished asset
- **WHEN** a release asset for tag `v1.2` hashes differently from the hash in the lock
- **THEN** the install fails with `integrity-mismatch`

#### Scenario: Go from go.dev
- **WHEN** a package declares `[providers.archive] url = "https://go.dev/dl/go{version}.{os}-{arch|go}.tar.gz"` with `extract = { strip = 1, to = "{data}/go" }` and `bins = ["bin/go"]`
- **THEN** `plan` shows the download and the extraction into the owned directory, with no root step
- **THEN** `bin/go` is linked into `~/.local/bin`

#### Scenario: Deb asset needs root
- **WHEN** the selected asset is a `.deb`
- **THEN** the plan shows `dpkg -i` as a root action, grouped into the run's one sudo prompt

#### Scenario: github-release alias
- **WHEN** a package declares `[providers.github-release]`
- **THEN** it behaves exactly as `[providers.archive]` with the same keys

### Requirement: Fonts
`font` SHALL install the font files (`files` globs inside an `archive` source, defaulting to `*.ttf` and `*.otf`) as owned files in the per-user font directory: `~/Library/Fonts` on macOS, `~/.local/share/fonts/dots/<pkg>/` on Linux, `%LOCALAPPDATA%\Microsoft\Windows\Fonts` on Windows. `installed()` SHALL read the ownership ledger and check that the files exist, without running any process. Linux SHALL run `fc-cache -f` once per run. Windows SHALL write owned `HKCU` Fonts registry values.

#### Scenario: Font installed once per run fc-cache
- **WHEN** three font packages are installed on Linux in one run
- **THEN** `fc-cache -f` runs exactly once, after the last font is placed

#### Scenario: Font file removed
- **WHEN** a font file placed by dots is deleted by hand
- **THEN** status shows the font package as `missing` without any process being run

#### Scenario: Font with a brew cask on macOS
- **WHEN** a font package declares both `[providers.font]` and `[providers.brew] cask = true`, and `[priority]` prefers brew on macOS
- **THEN** macOS installs it with brew and Linux installs it with `font`

### Requirement: VS Code extensions
`vscode-ext` SHALL read installed extensions with one `code --list-extensions --show-versions` call, and install with `code --install-extension <id>@<ver>`. `bin = "cursor"` or `"codium"` SHALL select a fork's CLI. Versions SHALL be pinnable.

#### Scenario: Batched read
- **WHEN** ten `vscode-ext` packages are planned
- **THEN** the installed state is read with a single `code --list-extensions --show-versions` call

#### Scenario: Fork binary
- **WHEN** a package declares `[providers.vscode-ext] bin = "cursor"`
- **THEN** the plan shows `cursor --install-extension` commands

### Requirement: apt repositories
An apt package MAY declare `repo`, either `{ ppa = "owner/name" }` or `{ url, suite, components, key, fingerprint }`, and an optional `pin = { origin, priority }`. dots SHALL write owned files `/etc/apt/keyrings/dots-<name>.asc` (only after the fingerprint matches), `/etc/apt/sources.list.d/dots-<name>.sources` (deb822 with `Signed-By`) and, with `pin`, `/etc/apt/preferences.d/dots-<name>`. It SHALL NOT use `apt-key`.

#### Scenario: PPA package installs after one apt-get update
- **WHEN** two packages declare `repo = { ppa = "git-core/ppa" }` and are planned together
- **THEN** the plan shows the repository added, then one `apt-get update`, then one batched `apt-get install`
- **THEN** the root actions share the run's one sudo prompt

#### Scenario: Third-party repository with a key
- **WHEN** a package declares `repo = { url, suite = "stable", components = ["main"], key, fingerprint }`
- **THEN** the plan shows the keyring, `.sources` and, when `pin` is set, the preferences file as owned files

### Requirement: apt repository lifecycle
A PPA SHALL NOT be refused. A new or changed repository SHALL trigger one `apt-get update` before the batched install. A repository SHALL be removed with its last package.

#### Scenario: Repository removed with its last package
- **WHEN** two packages name the same repository and one is removed
- **THEN** the repository files stay
- **WHEN** the other is removed too
- **THEN** the key, source and preferences files are removed

#### Scenario: Unchanged repository
- **WHEN** the repository files already match
- **THEN** no `apt-get update` runs

### Requirement: Cargo and uv path and git sources
`[providers.cargo]` SHALL accept `path` (relative to the package dir, run as `cargo install --path`) and `git` with an optional `rev`. `git_fetch_with_cli = true` SHALL set `CARGO_NET_GIT_FETCH_WITH_CLI` for the install. `[providers.uv]` SHALL accept the same `path` and `git` keys (`uv tool install <path>`, `git+ssh://...`). A `path` source's version SHALL be the crate version plus the repo rev, so a change in the repo makes the package outdated.

#### Scenario: Local crate goes outdated
- **WHEN** a cargo package has `path = "herdr-last-tab"` and a commit changes that directory
- **THEN** status shows the package as `outdated`

#### Scenario: Private git crate
- **WHEN** a cargo package sets `git = "ssh://git@github.com/x/y"` and `git_fetch_with_cli = true`
- **THEN** `plan` shows `cargo install --git ...` with `CARGO_NET_GIT_FETCH_WITH_CLI` set

#### Scenario: uv from git
- **WHEN** a uv package sets `git = "git+ssh://git@github.com/x/y"`
- **THEN** `plan` shows `uv tool install git+ssh://git@github.com/x/y`

### Requirement: User-defined providers
`dots.toml` `[providers.<id>]` SHALL define a provider with these keys:
- `available`, `bootstrap`
- `installed`: a command plus a parse mode, one of `lines`, `json:<path>` or `regex:<re>`
- `version`, `install`, `batch`

Placeholders SHALL be filled from the spec and the facts.

#### Scenario: Go provider
- **WHEN** a `go` provider is defined, and a package declares `[providers.go] source = "github.com/x/y"`
- **THEN** `plan` shows `go install github.com/x/y@latest`

### Requirement: Script provider contract
A `script` provider SHALL require `check` and `install`, and also `version` unless `versionless = true`. It SHALL accept these optional keys:
- `latest`, `upgrade`, `uninstall`
- `shell`, `needs_root`, `interactive`, `timeout`

After `install` exits 0, `check` SHALL pass. Otherwise the action fails with `check-failed-after-install`.

#### Scenario: Install that lies
- **WHEN** `install` exits 0 but `check` exits 1
- **THEN** the action fails with "install succeeded but check fails", and the log is linked

### Requirement: Script runtime environment
Every script dots runs SHALL run with:
- the package dir as the working directory;
- stdin set to `/dev/null`, unless `interactive`;
- `PATH` set to the generated environment plus provider bins;
- the `DOTS_*` variables listed in the scenario below.

All output SHALL go to `logs/<run-id>/<pkg>.<action>.log`.

#### Scenario: Script variables
- **WHEN** any script runs
- **THEN** it sees `DOTS_OS`, `DOTS_ARCH`, `DOTS_DISTRO`, `DOTS_WSL`, `DOTS_SUDO`, `DOTS_TAGS`, `DOTS_TAG_<NAME>`, `DOTS_PKG`, `DOTS_PKG_DIR`, `DOTS_ROOT`, `DOTS_STATE`, `DOTS_BIN`, `DOTS_LIB`, `DOTS_VERSION_WANTED`, `DOTS_DRY_RUN` and `DOTS_SETTING_<NAME>`

#### Scenario: Script waits for input
- **WHEN** a non-interactive script reads stdin
- **THEN** it reads end-of-file immediately instead of hanging

#### Scenario: Setting exposed to a script
- **WHEN** the setting `font_size = 15` is resolved
- **THEN** the script sees `DOTS_SETTING_FONT_SIZE=15`

### Requirement: Script timeout
A script running longer than its `timeout` SHALL be killed and reported as `timeout`. The default timeout is 20 minutes.

#### Scenario: Hung installer
- **WHEN** a script exceeds `timeout = "1m"`
- **THEN** it is terminated, its dependents are skipped, and independent actions continue

### Requirement: Script fetch
A `script` provider MAY set `fetch = { url, sha256 }`. dots SHALL download that file to the package's cache, verify its hash, and run it as `install` with `{fetched}` as its path (for example `install = "sh {fetched} -y"`). An author or lock pin SHALL be required to match; otherwise the hash SHALL be recorded on first use. The fetched installer SHALL appear in the code review like any script, and a changed hash on `pull` SHALL be a review item.

#### Scenario: Pinned installer runs
- **WHEN** a script package sets `fetch = { url = "https://sh.rustup.rs", sha256 = "<h>" }` and the download matches
- **THEN** `install` runs with `{fetched}` pointing at the cached file

#### Scenario: First use records the hash
- **WHEN** `fetch` has no `sha256` and the package is installed for the first time
- **THEN** the hash is recorded in `dots.lock` and checked on every later download

#### Scenario: Unpinned curl pipe sh
- **WHEN** a package script contains `curl https://example.com/i.sh | sh`
- **THEN** `dots lint` warns and suggests `fetch`

### Requirement: Declarative provider extensions
A user-defined provider SHALL additionally accept:
- optional `upgrade`, `uninstall` and `latest`; without `uninstall` the plan shows "cannot uninstall";
- any command as a table keyed by source kind (`github`, `local`, ...), `github` for a bare `owner/repo`;
- `parse = "json:<path>.{id: ..., version: ..., enabled: ...}"`, where `enabled = false` reports `ok`, not `missing`;
- `vars`: path variables usable in a package's `dst`;
- `review = { manifest, commands }`.

#### Scenario: Source-kind command table
- **WHEN** a herdr-plugin package has `source = "local:~/p/x"` and the provider's `install` has `github` and `local` keys
- **THEN** `plan` shows the `local` command, `herdr plugin link ~/p/x`

#### Scenario: Herdr plugin pinned to rev
- **WHEN** a package declares `[providers.herdr-plugin] source = "KoalaVim/herdr-nvim-aware", version = "5e9c1ec"`
- **THEN** `plan` shows `herdr plugin install KoalaVim/herdr-nvim-aware --ref 5e9c1ec --yes`
- **THEN** the lock records `5e9c1ec` as the resolved rev

#### Scenario: One batched read of plugins
- **WHEN** eight herdr plugin packages are planned
- **THEN** installed state is read with a single `herdr plugin list --json`
- **THEN** installs run one plugin per call, because the provider sets `batch = false`

#### Scenario: Plugin id differs from package name
- **WHEN** a package sets `id = "persiyanov.reviewr"` and the list output contains that `plugin_id`
- **THEN** the package is `ok`

#### Scenario: Disabled plugin is not missing
- **WHEN** the list output reports the plugin with `enabled = false` and the package does not ask for it to be disabled
- **THEN** status shows it as `ok`

#### Scenario: Upgrade moves the rev
- **WHEN** `dots upgrade herdr-navigator` runs and the provider defines `upgrade`
- **THEN** the pinned rev moves and the upgrade command is run with the new `--ref`

#### Scenario: Plugin config path variable
- **WHEN** a package of the plugin has `dst = "{plugin_config}/config.sh"`
- **THEN** `{plugin_config}` is the output of the provider's `vars.plugin_config` command

### Requirement: Provider scope
A provider declared in a package's `package.toml` SHALL be visible to that package's dependents only. One declared in `dots.toml` SHALL be visible to every package.

#### Scenario: Package-scoped provider
- **WHEN** a provider is declared in the `herdr` package's `package.toml`
- **THEN** packages that require `herdr` can use it, and an unrelated package that names it is an unknown-provider error

### Requirement: App plugin review
For a provider with `review = { manifest, commands }`, `add` and `pull` SHALL read the manifest at the pinned rev before installing, and list the named commands in the code review, as for package scripts. A changed command on `pull` SHALL be a review item. `scripts = "deny"` SHALL refuse a plugin whose manifest has any listed command.

#### Scenario: Build command shown before install
- **WHEN** a herdr plugin's `herdr-plugin.toml` has `build[0].command = "cargo build --release"`
- **THEN** the review of `dots add` lists that command before any plugin is installed

#### Scenario: Build command changed on pull
- **WHEN** a pull moves a plugin to a rev whose `build` command differs
- **THEN** the changed command is shown first as a review item

#### Scenario: Scripts denied
- **WHEN** a source has `scripts = "deny"` and a plugin's manifest lists a `build` command
- **THEN** the plugin is refused

### Requirement: Plugin manager shapes
An app whose plugins are driven by a config file it reads itself (lazy.nvim, tpm, a zsh plugin list) SHALL NOT need a provider. The config file SHALL be a normal `[[files]]` entry, and the manager's sync SHALL run as a `post_files` hook. An app whose plugin directory is only a repo SHALL use the `git` provider with `dst`.

#### Scenario: Neovim plugins via lazy
- **WHEN** the neovim package links `lazy-lock.json` and has a `post_files` hook running `nvim --headless "+Lazy! restore" +qa`
- **THEN** no provider is involved, and the hook runs after the files are placed

### Requirement: Bundled provider definitions
dots SHALL ship declarative definitions for `gh-ext`, `herdr-plugin`, `pi-package`, `hyprpm` and `psmodule`. A repo SHALL enable them with `[providers] use = [...]`, and MAY override any of their keys. Each SHALL pass the provider conformance suite, driven by recorded fixtures.

#### Scenario: Enabled by use
- **WHEN** `dots.toml` has `[providers] use = ["gh-ext"]` and a package declares `[providers.gh-ext] source = "dlvhdr/gh-dash"`
- **THEN** `plan` shows `gh extension install dlvhdr/gh-dash`

#### Scenario: Not enabled
- **WHEN** a package names a bundled provider that `use` does not list
- **THEN** resolution fails with an unknown-provider error that hints at `[providers] use`

#### Scenario: Override a key
- **WHEN** the repo overrides `[providers.gh-ext]` with `batch = false`
- **THEN** the bundled definition keeps its other keys and uses the override
