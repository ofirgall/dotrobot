# Spec Delta

## Purpose

Defines the migration path from dotbot: converting dotbot YAML configs into dots packages without hiding drift between configs, and proposing declarative packages for install lines found in scripts.

## ADDED Requirements

### Requirement: Import dotbot configs
`dots import dotbot <file>...` SHALL parse every dotbot file given. It SHALL then:
- merge links that are identical across files into one file entry, with a `when` derived from which files contain the link;
- group links into packages by the first path component of each link's source;
- move the files with `git mv`, so their history is kept.

#### Scenario: Same link in two configs
- **WHEN** `common.conf.yaml` and `macos.conf.yaml` both link `~/.config/x`
- **THEN** the result has one file entry, not two

### Requirement: Drift is reported, not hidden
The importer SHALL report each link that exists only on some platforms and ask whether that was intended. It SHALL NOT invent a condition silently.

#### Scenario: Linux-only link
- **WHEN** `~/.config/hunk` is linked only in the Linux config
- **THEN** the report asks "linked on linux only. Intended?"

### Requirement: Shell steps become unverified scripts
Each dotbot `shell:` step SHALL become a `script` package whose `check` is a placeholder. Status SHALL show such packages as `unverified` until a real check is written. Each `create:` step SHALL become a `dir` entry.

#### Scenario: Imported shell step
- **WHEN** a `shell:` step is imported
- **THEN** the new package shows as `unverified` in status

### Requirement: Scan install lines
`dots import scan <dir>` SHALL find `brew`, `apt`, `cargo`, `pip` and `npm` install lines and propose one declarative package for each. Each proposal SHALL require the user's confirmation before it is written.

#### Scenario: Proposed cargo package
- **WHEN** a script contains `cargo install bottom`
- **THEN** scan proposes a `bottom` package with `[providers.cargo]` and waits for confirmation

### Requirement: Scan recognizes sources and repositories
`dots import scan` SHALL recognize, and propose with confirmation:
- `add-apt-repository`, keyring and `sources.list.d` lines: apt `repo`
- `cargo install --path` or `--git`: cargo `path` or `git`
- `pip install --break-system-packages`, `git+ssh://...`: a `uv` package
- tpm, zinit or lazy bootstrap clones: a `git` package, with the plugin list left as a config file
- source builds (zig, tmux, gamescope): a `script` package that needs a real `check`

#### Scenario: PPA line
- **WHEN** a script contains `add-apt-repository ppa:git-core/ppa`
- **THEN** scan proposes an apt package with `repo = { ppa = "git-core/ppa" }` and waits for confirmation

### Requirement: Scan recognizes downloads, fonts and plugins
`dots import scan` SHALL recognize, and propose with confirmation:
- `download_latest_release`, release-download `wget`, go.dev or ziglang tarballs, `.deb` with `dpkg -i`: `archive`
- font downloads, `font-*` casks, a Windows font registry loop: `font`
- a `code --install-extension` loop: one `vscode-ext` package per line
- `gh extension install`, `herdr plugin install`, `pi install`, `hyprpm add`, `Install-Module`: the bundled provider definition
- `curl ... | sh`: `script` with `fetch`

#### Scenario: Herdr plugin install
- **WHEN** a script contains `herdr plugin install owner/x --yes`
- **THEN** scan proposes a package with `[providers.herdr-plugin] source = "owner/x"`, and notes that `[providers] use` must list `herdr-plugin`

#### Scenario: Curl pipe sh
- **WHEN** a script contains `curl -fsSL https://sh.rustup.rs | sh`
- **THEN** scan proposes a `script` package with `fetch` and the hash to be recorded

### Requirement: Scan recognizes settings, services and accounts
`dots import scan` SHALL recognize, and propose with confirmation:
- `defaults write`, `reg add`, `Set-ItemProperty`, `xdg-mime default`: `[[defaults]]`, `[[registry]]` or `[[xdg]]`
- `brew services start`, `systemctl --user`, Startup-folder copies: `[service]`
- `tic -x ...`: a `terminfo` file entry
- `chsh`, `usermod -aG`: a manual action with a check

#### Scenario: Defaults write
- **WHEN** a script contains `defaults write NSGlobalDomain KeyRepeat -int 2`
- **THEN** scan proposes a `[[defaults]]` entry with `domain = "NSGlobalDomain"`, `key = "KeyRepeat"` and `value = 2`

#### Scenario: Chsh
- **WHEN** a script contains `chsh -s "$(command -v zsh)"`
- **THEN** scan proposes a manual action with a `check` on the login shell

### Requirement: Directory links are split
When a dotbot link maps a whole app directory that the app also writes runtime files into, import SHALL propose linking only the config files, and SHALL note that the runtime entries stay in the app directory.

#### Scenario: Directory link split
- **WHEN** a dotbot link maps all of `dotfiles/herdr/` into `~/.config/herdr`
- **THEN** import proposes linking only `config.toml` and the terminfo file, and notes that herdr's runtime entries stay in the app directory
