# dots: architecture

Status: draft v1 (2026-11-25). This defines the design and does not implement it.
The CLI is `dots`.

---

## 0. What we are replacing, and why it hurts

I read `~/dotfiles` before writing this. The problems below are in that repo today. They are not hypothetical.

| # | Problem | Evidence in `~/dotfiles` | Design answer |
|---|---------|--------------------------|---------------|
| P1 | The same link list is copied into 4 YAMLs and the copies have drifted | `common.conf.yaml` has `~/.codex`, `~/.config/hunk`, `~/.config/lumen`, `~/.pi/agent/keybindings.json` and `~/playgrounds`, and `macos.conf.yaml` has none of them. `macos.conf.yaml` has `herdr` and `hop`, and `common` lacks both. | Write each link once, inside the package that owns it. Platform differences become *conditions*, not copies. |
| P2 | Platform is chosen by the entry script (`install`, `install-macos`, `install-windows`) and by marker files | `~/.remote_indicator`, `~/.no_sudo_indicator`, `uname` checks in `helpers.sh` | Detect **facts**, add declared **tags** from `machine.toml`, and let `when = ...` expressions use both. |
| P3 | Install scripts mix package managers, repeat the same packages per OS, and are not idempotent | `cargo install difftastic du-dust git-delta jless igrep bottom` appears in both `install_basic.sh` and `mac/install_basic.sh` | Define the package once, let each provider override only what differs, and give every action a **check**. |
| P4 | Scripts rot because nothing ever checks that they work | `install_nvim.sh`: `if -d ~/.local/share/nvim; then` is missing `[ ]`, so the condition is always false. `install_basic.sh` ends with `newgrp dialout`, which opens a subshell and hangs a non-interactive run. | A script package must declare `check` and `version` commands. Scripts run with stdin closed unless they declare `interactive = true`. |
| P5 | Config merging is ad hoc and writes generated files into the repo | `herdr/generate-config.sh` runs `yq '*+'` with a hand-kept `DEV_REPLACE_PATHS` list and writes `config.toml` into the repo. `ln -sf config.macos dotfiles/ghostty/config.platform` mutates the repo and then needs `.gitignore`. | Format-aware **layered render** with a strategy per path. Output goes to the state dir, never into the repo. |
| P6 | Directory links turn the repo into an app's runtime directory | `dotfiles/herdr/` holds `herdr.sock`, `*.log`, `sessions/` and `session-snapshots/` | Link files, not directories, by default. Directory links must be opted into, and `status` reports unexpected entries under them. |
| P7 | `force: true, relink: true` deletes whatever is at the destination without asking | `defaults.link` in every YAML | Back up before replacing. Never remove a path that `dots` does not own. |
| P8 | Manual steps sit in a README and nobody knows whether they were done | `macREADME/MANUAL.md`, `winREADME/MANUAL.md` | Manual actions are markdown files with frontmatter, tracked per machine, and can be auto-verified with a `check`. |
| P9 | Git sources are submodules plus hooks plus a `wip-submodules` hack | `.gitmodules`, `.githooks/pre-commit` auto-`git add`s submodules, `Makefile update` | A `git` provider with a pinned rev in the lock file. Dirty, ahead and behind show up in `status`. |
| P10 | There is no record of what was installed, when, or from which commit | (nothing) | An append-only journal stamped with the repo SHA, plus the git history of the lock file. |
| P11 | PATH and env fixes are scattered | `source $HOME/.cargo/env`, `eval "$(brew shellenv)"`, `npm config set prefix` and `ln -s batcat` all live in different scripts | Packages declare `env`/`path` contributions. `dots` generates one `env.sh` and injects a single source line. |
| P12 | Some files are local and untracked, and handled by bespoke scripts | `install_zshrc.sh` prepends a source line to an untracked `~/.zshrc` | A first-class `inject` file mode: a managed block with markers inside a file that `dots` does not otherwise own. |

Design principle: **`dots` is a reconciler.** Desired state comes from the repo, the machine profile and the facts. Actual state comes from querying providers and the filesystem. `dots` computes `plan = diff(desired, actual)`, shows it, then executes it and journals the result. Every command is either a read (`status`, `plan`, `log`) or a reconcile (`apply`, `upgrade`). No command means "run these scripts again and hope".

---

## 1. Core concepts

```
Repo (git)         Machine (local)               Engine
─────────────      ─────────────────────         ─────────────────────────────
dots.toml          ~/.config/dots/machine.toml   Facts  (detected: os, arch, distro, wsl, sudo, ...)
packages/<p>/      (bundles, tags, root)         Tags   (declared: work, remote, gui, ...)
bundles/<b>.toml                                 Resolver → package set → provider choice
dots.lock                                        Planner  → Plan (typed actions, DAG)
                                                 Executor → batched, parallel, journaled
                   ~/.local/state/dots/          State: ownership, render hashes, todo status, journal
```

- **Package**: the unit of installation and of copying. It is a directory holding `package.toml` plus everything the package owns: config files, manual-action markdown, scripts.
- **Bundle**: a named list of packages and/or other bundles. Each entry can have a condition. A bundle has no files of its own.
- **Provider**: an adapter for an installation mechanism (brew, apt, cargo, uv, ...). It can query state in batches and install, upgrade or remove in batches.
- **Facts**: things detected about the machine. Read-only.
- **Tags**: user-declared booleans in `machine.toml`. They replace the marker files.
- **Machine profile**: the local, untracked file that says which bundles this machine wants, which tags apply and where the repo lives.
- **Lock**: `dots.lock`, committed. It records the resolved versions and the git revs.
- **Platform**: a chain of increasingly specific keys for this machine (`linux › debian › ubuntu › ubuntu-24.04`). Any object can carry `[platform.<key>]` tables, and more specific keys override more general ones (§4.1).
- **Override**: a partial definition that changes anything defined elsewhere (a package, bundle, file, setting schema, manual action, provider option or priority) without editing the original (§18).
- **Upstream (fork)**: another repo you *extend* at a pinned rev instead of copying. You keep only your overrides (§13.2).
- **State**: local and per machine. It records what `dots` owns (links, rendered files, injected blocks), the todo status and the journal.

---

## 2. Repository layout

```
dotfiles/
├── dots.toml                    # repo manifest (schema, defaults, provider priority, custom providers)
├── dots.lock                    # resolved versions/revs, committed
├── bundles/
│   ├── base.toml
│   ├── cli.toml
│   ├── mac-desktop.toml
│   └── linux-desktop.toml
├── packages/
│   ├── ripgrep/
│   │   └── package.toml                 # install-only package, no files
│   ├── ghostty/
│   │   ├── package.toml
│   │   ├── config/                      # layers
│   │   │   ├── common
│   │   │   ├── macos
│   │   │   └── linux
│   │   └── manual/
│   │       └── disable-secure-input.md
│   ├── herdr/
│   │   ├── package.toml
│   │   └── config/{common.toml,mac.toml,windows.toml,dev.toml}
│   ├── koala-config/                    # git provider (replaces submodule)
│   │   └── package.toml
│   └── karabiner/
│       ├── package.toml
│       ├── karabiner.json
│       └── manual/permissions.md
├── overrides/                   # changes to things defined elsewhere (local, copied or forked), §18
│   ├── dots.toml                        # manifest overrides (priorities, provider options, tags, ...)
│   ├── packages/ghostty/override.toml   # + any extra layers/files it references
│   └── bundles/cli.toml
├── machines/                    # OPTIONAL committed profiles, selected by hostname or explicitly
│   ├── work-mbp.toml
│   └── work-mbp/overrides/              # same structure as overrides/, applied only on that machine
└── lib/                         # shared shell helpers for script packages (sourced via $DOTS_LIB)
```

**Rule: a package is self-contained.** Every file a package references lives under its own directory. That makes `dots add other/repo:pkg` a directory copy and not a graph walk over the whole repo. An escape hatch (`src = "//shared/foo"` for a repo-root path) exists for migration only, and `dots lint` warns on it. A package with a `//` reference cannot be copied until it is internalized, and `dots add` refuses it.

---

## 3. Schemas

All schemas are versioned (`schema = 1`). Parsing is strict: unknown keys are errors, with "did you mean" suggestions. A typo in a dotfiles manager must not silently do nothing.

### 3.1 `dots.toml` (repo manifest)

```toml
schema = 1
requires_dots = ">=0.1"
name = "ofirgall/dotfiles"

# Provider priority per platform: the first provider that the package supports AND is available wins.
# Keyed by platform (§4.1). The MOST SPECIFIC matching key wins (lists replace, they don't merge).
[priority]
any     = ["cargo", "uv", "npm", "github-release", "script"]
macos   = ["brew", "cargo", "uv", "npm", "github-release", "script"]
debian  = ["apt", "cargo", "uv", "npm", "github-release", "brew", "script"]
"ubuntu-22.04" = ["cargo", "apt", "uv", "npm", "github-release", "script"]   # 22.04's apt is too old for most CLIs
windows = ["winget", "scoop", "cargo", "uv", "npm", "github-release", "script"]

# Tags a machine may declare. Declaring them here lets typos be caught.
[tags]
remote = "headless/ssh box: no GUI packages"
work   = "work machine"
gui    = "has a desktop session"

# Path variables usable in file destinations, resolved per platform
[paths]
app_support = { macos = "~/Library/Application Support", linux = "~/.config", windows = "%APPDATA%" }

# User-defined providers, see §5.4
[providers.go]
...
```

### 3.2 `machine.toml` (local, `~/.config/dots/machine.toml`, never committed)

```toml
root = "~/dotfiles"               # main checkout; see §11 for worktrees
profile = "work-mbp"              # optional: pull from machines/work-mbp.toml, then override below
bundles = ["base", "cli", "mac-desktop"]
tags = ["work", "gui"]
exclude = ["spotify"]             # machine-local opt-out, visible in status as "excluded"

[facts]                           # override detection (rarely needed; e.g. forcing sudo=false)
sudo = false
```

### 3.3 Bundle

```toml
# bundles/cli.toml
schema = 1
description = "Terminal tooling"
include = ["base"]                       # other bundles

packages = [
  "ripgrep", "fd", "bat", "delta", "difftastic", "dust", "jless", "bottom", "yazi",
  { name = "wiremix", when = 'os = "linux"' },
  { name = "bluetuith", when = 'all(os = "linux", not(tag = "remote"))' },
]
```

Duplicates across bundles are fine, because the resolved package set is a set. If two bundles apply conflicting conditions to the same package, the package is included when *any* inclusion path is true.

### 3.4 Package

Minimal case, where the name is the same everywhere:

```toml
# packages/ripgrep/package.toml
schema = 1
name = "ripgrep"
bin = "rg"                    # used by the presence fallback and by `dots which`
```

With per-provider overrides (this is the whole point):

```toml
# packages/fd/package.toml
schema = 1
name = "fd"
bin = "fd"
version = "latest"            # latest | 1.2.3 | 1.2 | >=1.2 | git:<ref>  (§8.1)

[providers.apt]
name = "fd-find"
bin  = "fdfind"               # the binary is named differently on Debian
shim = "fd"                   # dots creates ~/.local/bin/fd -> fdfind and owns it

[providers.cargo]
name = "fd-find"
locked = true

[providers.brew]              # an empty table means "supported, defaults apply"
```

A package that supports only some providers lists only those. With no `[providers]` table at all, the package is treated as supported by **every** provider in the priority list, using `name`. Pass `providers_only = true` to forbid that.

Full surface:

```toml
schema = 1
name = "ghostty"
description = "Terminal"
when = 'not(tag = "remote")'           # package-level condition; false = package is inert here
requires = ["nerd-fonts"]              # hard deps: installed first, pulled in transitively
after = ["homebrew"]                   # ordering only, no inclusion
version = "latest"

[providers.brew]
name = "ghostty"
cask = true

[providers.script]                     # see §5.3
check   = "command -v ghostty"
version = "ghostty --version | head -1 | awk '{print $2}'"
install = "scripts/install-linux.sh"
when    = 'os = "linux"'

# --- Files ---
[[files]]
dst  = "~/.config/ghostty/config"
mode = "render"
layers = ["config/common", { src = "config/macos", when = 'os = "macos"' }, { src = "config/linux", when = 'os = "linux"' }]
format = "text"                        # concat with header comments; see §6

[[files]]
dst  = "~/.config/ghostty/shaders"
mode = "git"                           # clone, owned by the git provider (replaces the ad-hoc clone)
url  = "https://github.com/ofirgall/ghostty-cursor-shaders"

# --- Env contributions ---
[env]
path = ["~/.local/bin"]
vars = { GHOSTTY_RESOURCES_DIR = "..." }

# --- Hooks (run around this package's actions; must be idempotent) ---
[hooks]
post_install = "scripts/install-plugins.sh"
post_files   = "touch -h ~/.config/karabiner/karabiner.json"   # e.g. karabiner reload

# --- Provenance (written by `dots add`, never by hand) ---
[origin]
repo = "github:ofirgall/dotfiles"
path = "packages/ghostty"
rev  = "f4522416c0d0..."               # full SHA the copy was taken from
tree = "sha1-of-the-package-tree-at-copy-time"
```

Manual actions are discovered automatically from `manual/*.md` (see §9).

---

## 4. Facts, tags and conditions

**Facts** are detected once per run and cached in memory. `dots facts` prints them.

| Fact | Values | Source |
|------|--------|--------|
| `os` | `macos`, `linux`, `windows` | compile target + runtime |
| `arch` | `aarch64`, `x86_64` | runtime (`uname -m`, so it is right under Rosetta) |
| `distro`, `distro_family`, `distro_version` | `ubuntu`/`debian`, `arch`, `fedora`, ... | `/etc/os-release` |
| `wsl` | bool | `/proc/sys/kernel/osrelease` contains `microsoft` (case-insensitive; the current `helpers.sh` checks `*"Microsoft"*` and misses WSL2) |
| `sudo` | bool | `sudo -n true` succeeds, or the user is root, unless overridden |
| `gui` | bool | macOS: always true. Linux: `$WAYLAND_DISPLAY`/`$DISPLAY`/`loginctl`. Overridable. |
| `hostname`, `user`, `home` | strings | |
| `shell` | `zsh`, `bash`, `pwsh` | `$SHELL` |
| `provider.<id>` | bool | whether the provider's binary resolves (see §5.1) |

### 4.1 Platform hierarchy (general → specific)

The facts produce a **platform chain**, a linear list of keys from most general to most specific. Each key is "a kind of" the one before it:

```
Ubuntu 24.04 on x86_64:   any › unix › linux › debian › ubuntu › ubuntu-24 › ubuntu-24.04
Linux Mint 22.1:          any › unix › linux › debian › ubuntu › linuxmint › linuxmint-22 › linuxmint-22.1
macOS 15.2 on M-series:   any › unix › macos › macos-15 › macos-15.2
Windows 11:               any › windows › windows-11
Ubuntu under WSL:         … › ubuntu-24.04 › wsl
```

- The chain is built from `/etc/os-release`: `ID_LIKE` gives the parents, read in reverse, so Mint gets `debian › ubuntu`. Then `ID`, then `ID-<major>`, then `ID-<VERSION_ID>`. macOS uses `sw_vers`, and Windows uses the build number mapped to 10 or 11.
- **Architecture** is a qualifier on any level: `<key>/<arch>` (`linux/aarch64`, `macos/x86_64`, `ubuntu-24.04/x86_64`). Each qualified key ranks immediately **after** its unqualified level. The full ordered list for that Ubuntu box is: `any, any/x86_64, unix, unix/x86_64, linux, linux/x86_64, debian, …, ubuntu-24.04, ubuntu-24.04/x86_64`.
- **WSL** is appended as the most specific level when it is detected, because WSL quirks (clipboard, `/mnt/c`, no systemd on old builds) override whatever the distro says.
- **Custom platforms or fixes** go in `dots.toml`: `[platforms.pop] parent = "ubuntu"` (for a distro whose `ID_LIKE` is wrong or missing). A machine can also force extra levels: `machine.toml` `platform_extra = ["steamdeck"]`, declared with a parent.
- `dots facts` prints the chain: `platform  any › unix › linux › debian › ubuntu › ubuntu-24 › ubuntu-24.04  (x86_64)`.

**Platform tables, usable on any object.** A `[platform."<key>"]` table holds a partial copy of the object it sits in. Every key in the chain that has a table is deep-merged on top of the base, **in chain order**, so the most specific wins. The merge rules are the same as for overrides (§18.2): tables merge, scalars and arrays replace, and `{ add, remove }` edits a list.

```toml
# packages/neovim/package.toml
schema = 1
name = "neovim"
bin = "nvim"

[providers.apt]
name = "neovim"

[platform.ubuntu.providers.apt]
ppa = "ppa:neovim-ppa/unstable"         # all Ubuntus: use the PPA

[platform."ubuntu-22.04"]
prefer = ["github-release"]             # 22.04 specifically: PPA is dead, use the release tarball

[platform."ubuntu-22.04".providers.github-release]
repo  = "neovim/neovim"
asset = 'nvim-linux-x86_64\.tar\.gz$'

[platform."linux/aarch64".providers.github-release]
asset = 'nvim-linux-arm64\.tar\.gz$'

[platform.wsl]
requires = { add = ["win32yank"] }       # clipboard bridge only under WSL
```

Platform tables are allowed on packages, provider sections, bundles (and per-member), file entries, settings (`[platform.macos.settings.font_size] default = 14`), manual actions, `dots.toml` sections, and inside overrides (§18). An override can therefore change behavior for `ubuntu-22.04` only.

**File layers by platform.** A file entry can use `layers = { dir = "config", auto = true }`. dots then takes `config/common` (if present) followed by `config/<key>` for every key in the chain that has a file, in chain order. For example, ghostty's `config/macos`, or `config/ubuntu-22.04` holding only the one line that differs there. Explicit `layers = [...]` lists still work.

**Choosing between platform tables and `when`:** platform tables *change fields* by specificity. `when` decides whether something *exists* (a package, file, layer or bundle member). An object is first merged through its platform tables, and its `when` is then evaluated on the result. That way a `[platform.x]` table can even change `when`.

**In conditions:** `platform = "ubuntu"` is true when `ubuntu` is **anywhere** in the chain (an is-a test), so it is also true on Mint and on `ubuntu-24.04`. Versions compare with the §8.1 rules: `all(platform = "ubuntu", distro_version >= "24.04")`.

Lint: an unknown platform key is an error, with "did you mean" (`ubunut-22.04`). So is a key whose version doesn't parse.

### 4.2 Tags and the condition language

**Tags** are the declared strings from `machine.toml`. Both feed one condition language.

**The condition language** is Rust `cfg()` syntax, because it is familiar, small and trivially parseable: `all(...)`, `any(...)`, `not(...)`, `key = "value"`, and bare `key` for bool facts.

```
'os = "macos"'
'all(os = "linux", not(tag = "remote"), sudo)'
'any(distro = "ubuntu", distro = "debian")'
```

The parser is written in-house (~200 lines) and does not use the `cfg-expr` crate, which is tied to target triples. Unknown fact keys and undeclared tags are **errors at lint time**, not silently false.

Conditions can also read **package settings** (§17): `setting.font_size`, `setting.theme = "light"`, and bare `setting.cursor_shader` for bools. Inside a package, `setting.x` means that package's own setting. Use `settings.<pkg>.x` to read another package's setting. That creates an implicit ordering edge and fails lint if `<pkg>` is not in the resolved set. Numeric comparison uses `setting.font_size >= 14` (only `= != < <= > >=`; no arithmetic).

Conditions are allowed on: package (`when`), provider override, bundle entry, file entry, file layer, manual action, priority block.

---

## 5. Providers

### 5.1 Trait

```rust
pub trait Provider: Send + Sync {
    fn id(&self) -> &'static str;
    fn caps(&self) -> Caps;   // see below

    /// Is the backend usable here? If not, which package bootstraps it?
    fn availability(&self, facts: &Facts) -> Availability; // Ready{bin_path} | Bootstrap{pkg:"homebrew"} | Unsupported

    /// ONE call returning everything installed, keyed by provider-native name. Never per-package.
    fn installed(&self, ctx: &Ctx) -> Result<BTreeMap<String, Installed>>;   // name, version, pinned, source(rev/url)

    /// Latest available versions. Network; cached (§7.3). Batched.
    fn latest(&self, ctx: &Ctx, names: &[&Spec]) -> Result<BTreeMap<String, Latest>>;

    /// Turn desired ops into concrete commands. Pure: no side effects, used by `dots plan`.
    fn plan(&self, ops: &[Op]) -> Result<Vec<Action>>;    // Op = Install|Upgrade|Remove|Pin|Unpin {spec, version}

    fn exec(&self, ctx: &Ctx, action: &Action) -> Result<Outcome>;

    /// Executables each installed package put on disk: (native pkg name, bin name, absolute path). Batched (§7.4).
    fn bins(&self, ctx: &Ctx, installed: &BTreeMap<String, Installed>) -> Result<Vec<BinEntry>>;

    /// Reverse lookup for paths the forward scan did not claim, e.g. `dpkg -S`, `brew which-formula`. Optional.
    fn owner_of(&self, ctx: &Ctx, paths: &[PathBuf]) -> Result<BTreeMap<PathBuf, String>> { Ok(Default::default()) }
}

pub struct Caps {
    pub batch_install: bool,     // apt/brew: yes. cargo: no (one crate per invocation)
    pub exact_version: Support,  // Yes (cargo, uv, npm, git, github-release) | Limited (apt: only versions in repo) | No (brew: @-formulae only)
    pub pin: bool,               // brew pin / apt-mark hold
    pub needs_root: bool,        // apt, dnf, pacman
    pub uninstall: bool,
    pub exclusive_lock: bool,    // apt/dpkg & brew hold global locks → never run two of the same provider concurrently
}
```

Why it is shaped this way:

- **Batched `installed()`** exists because `status` must be fast. Today, checking 60 packages would mean 60 `brew list` calls, about 30 s. Batched it is one `brew info --json=v2 --installed`, about 1 s.
- **`plan()` is pure**, so `dots plan` and `apply --dry-run` show the exact commands that will run.
- **The provider resolves its own binary path** and does not rely on `PATH`. On a fresh Mac, `brew` is in `/opt/homebrew/bin`, which is not yet on `PATH`. On Intel it is in `/usr/local/bin`. Cargo is in `~/.cargo/bin`. After a provider is bootstrapped, `dots` resolves it again in the same run. This kills `eval "$(brew shellenv)"` and `. ~/.cargo/env` at the top of every script.

### 5.2 Built-in providers (v1)

| id | installed() via | latest() via | install | version pinning | root |
|----|-----------------|--------------|---------|-----------------|------|
| `brew` | `brew info --json=v2 --installed` | `brew outdated --json=v2` (+ `brew update` gated by TTL) | `brew install a b c` / `--cask` | Limited: `name@ver` formulae, `brew pin` | no |
| `apt` | `dpkg-query -W -f='${Package}\t${Version}\t${Status}\n'` | `apt-cache policy` / `apt list --upgradable` | `sudo apt-get install -y a b c` (one call) | Limited: `pkg=ver` if in repo, `apt-mark hold` | yes |
| `cargo` | `~/.cargo/.crates2.json` (parse it; it is exact, and faster than `cargo install --list`) | crates.io sparse index, `git ls-remote` for git crates | `cargo install [--locked] [--git url --rev sha] name@ver` | Yes | no |
| `uv` | `uv tool list --show-version-specifiers` | PyPI JSON API | `uv tool install pkg==ver` | Yes | no |
| `npm` | `npm ls -g --json --depth=0` (prefix is managed by dots, see §10) | `npm outdated -g --json` | `npm i -g a@v b@v` | Yes | no |
| `git` | `git -C dst rev-parse HEAD`, `status --porcelain`, ahead/behind | `git ls-remote` | clone / fetch+checkout rev | Yes (rev) | no |
| `github-release` | version recorded in state + a `version` cmd | GitHub API `/releases/latest` (token optional) | download asset by `asset = "regex"` per os/arch, **verify checksum** if published, extract, place bin | Yes (tag) | no |
| `script` | `check` + `version` cmds | optional `latest` cmd | `install` cmd | whatever the script does | declared |
| `winget`, `scoop` | `winget list` / `scoop export` | `winget upgrade` / `scoop status` | | Limited | no |
| `mas` | `mas list` | `mas outdated` | `mas install id` | No | no |
| `browser-ext` | browser profile dirs (§5.2.1) | AMO API (Firefox); none for the Chrome Web Store | external-extension file or policy | No (stores auto-update) | `policy` method only |
| `pacman`, `dnf` | later; the trait is the same as apt's | | | | yes |

`github-release` replaces `download_latest_release` in `helpers.sh`, together with every `curl ... | sh`. It uses a checksum if the release publishes one. When none is published, `dots` records the SHA-256 of the asset in the lock on first install (trust on first use) and warns if the asset for the same tag later changes.

`pip` is deliberately **not** a provider in the sense of `pip install --break-system-packages`. Python CLIs go through `uv tool` (each tool gets an isolated venv). For libraries that a script needs (for example `libtmux` for a tmux plugin), declare a `uv` package with `[providers.uv] kind = "lib", python = "system"`. That runs `uv pip install --system`, and it is explicit and greppable. `scripts/requirements.txt` becomes a uv-managed venv at `~/.local/share/dots/venvs/scripts` (a `uv` package with `kind = "venv"`, `requirements = "requirements.txt"`).

### 5.2.1 Browser extensions (`browser-ext`)

An extension is a package like any other. The provider writes the file the browser itself reads to install extensions, so it needs no browser automation and no store scraping.

```toml
# packages/vimium/package.toml
schema = 1
name = "vimium"

[providers.browser-ext]
chrome   = "dbepggeogbaibhgnhhndojpepiihcmeb"   # Chrome Web Store id (also used for Brave/Edge/Chromium unless overridden)
edge     = "djnmhfbpocmdjinfoeolkdfnfbhkkgfn"   # optional per-browser id (Edge Add-ons uses different ids)
firefox  = "{d7742d87-e61d-4b78-b8a1-b469842139fa}"   # AMO add-on GUID
browsers = ["chrome", "brave", "firefox"]       # default: every supported browser found on the machine
method   = "external"                           # external (user-level, removable) | policy (admin, force-installed)
```

**Install methods:**

| Browser | `external` (default, no admin) | `policy` (admin) |
|---------|-------------------------------|------------------|
| Chrome / Brave / Edge / Chromium | `<id>.json` = `{"external_update_url": "<store update url>"}` in the browser's *External Extensions* dir. macOS: `~/Library/Application Support/<Vendor>/<Browser>/External Extensions/`. Linux: `/usr/share/<browser>/extensions/`, which is a system dir, so on Linux even `external` needs root. Windows: registry `HKCU\Software\<Vendor>\<Browser>\Extensions\<id>` with `update_url`. | `ExtensionInstallForcelist` policy. macOS: managed-preferences plist. Linux: `/etc/opt/chrome/policies/managed/dots.json` (and the per-browser equivalents). Windows: `HKLM` policy key. |
| Firefox | Firefox no longer sideloads from external locations, so `external` is unavailable here. Selecting it on Firefox is a resolve error that suggests `policy`. | `policies.json` → `ExtensionSettings.<guid>.installation_mode = "normal_installed"` + `install_url` (AMO latest `.xpi`). Linux: `/etc/firefox/policies/`. macOS: `Firefox.app/Contents/Resources/distribution/` (gets overwritten by app updates, so `dots` re-checks it on every `status`). Windows: `HKLM` policy key. |

- The files `dots` writes are ordinary **owned files** (§6.4). Removing the package deletes them, and the browser then uninstalls the extension on its next start. With `policy`, all of a browser's force-installed extensions share **one** `dots`-owned policy file, rendered from every extension package, the same way as fragments (§6.2). It never overwrites policies `dots` doesn't own.
- **`external` needs a one-time click.** Chromium installs the extension *disabled* and asks the user to enable it. The provider therefore attaches a manual action automatically, "Enable \<name\> in \<browser\>". That action's `check` reads the profile's enabled state, so the todo closes itself (§9).
- **Install happens on the browser's next launch**, not immediately. `apply` reports `pending-restart` (`↻` in `status`) until the profile shows the extension.
- **installed():** for each profile of each browser, read the Chromium `Preferences`/`Secure Preferences` (`extensions.settings.<id>`: version, state) or Firefox `extensions.json`. Profiles come from the browser's `Local State` / `profiles.ini`. `profiles = ["Default"]` restricts the provider to certain profiles. The default is every profile.
- **latest():** for Firefox, the AMO API (`/api/v5/addons/addon/<guid>/`). Chromium stores have no public version API, so `latest` stays unknown. That isn't a problem in practice, because the browser auto-updates extensions.
- **Versions:** only `latest` is valid (lint error otherwise). Stores auto-update, so pinning would be a lie. The lock records the observed version for history.
- **Bootstrap:** the browser itself is a dependency, and the provider is `Unsupported` for browsers that aren't installed. A package's `requires = ["google-chrome"]` makes that explicit.

**Extension settings** usually live in browser storage, not in files, so each package handles them according to how the extension stores them:

1. **File-based config** (for example Tridactyl's rc): a normal `[[files]]` entry in the same package.
2. **Native messaging hosts** (brotab, KeePassXC-Browser, Tridactyl native): the host manifest is a `render` file in the browser's `NativeMessagingHosts` dir, with the host binary as a `requires`.
3. **Policy-configurable** extensions (those that ship a managed-storage schema, such as uBlock Origin): with `method = "policy"`, a `[providers.browser-ext.managed]` table is rendered into the policy as `3rdparty.extensions.<id>`, so it is fully declarative and settings/overrides apply.
4. **Everything else** (Tampermonkey userscripts, Vimium options): the package ships the export file and a manual action, "Import `settings.json` via Options › Import". That action has no `check` and stays a tracked todo until the user marks it done, and it reappears on every machine when its `revision` is bumped.

### 5.3 The `script` provider (the escape hatch, kept honest)

```toml
[providers.script]
check     = "test -x ~/.local/bin/kv"                 # REQUIRED. exit 0 = installed
version   = "kv --version | awk '{print $2}'"         # REQUIRED unless versionless = true
latest    = "git ls-remote https://github.com/KoalaVim/kv HEAD | cut -c1-12"  # optional, enables `outdated`
install   = "scripts/install.sh"                      # REQUIRED
upgrade   = "scripts/install.sh"                      # default = install
uninstall = "scripts/uninstall.sh"                    # optional; absent = "cannot uninstall" shown in plan
shell     = "bash"                                    # bash | sh | pwsh; default per-OS
needs_root = false
interactive = false                                    # true = stdin attached, cannot run in parallel
timeout   = "20m"
```

The runtime contract for every script the engine runs:

- cwd = the package dir. `set -euo pipefail` is prepended for bash.
- Environment: `DOTS_OS`, `DOTS_ARCH`, `DOTS_DISTRO`, `DOTS_WSL`, `DOTS_SUDO`, `DOTS_TAGS` (space-separated), `DOTS_TAG_<NAME>=1`, `DOTS_PKG`, `DOTS_PKG_DIR`, `DOTS_ROOT`, `DOTS_STATE`, `DOTS_BIN=~/.local/bin`, `DOTS_LIB` (shared helpers), `DOTS_VERSION_WANTED`, `DOTS_DRY_RUN`.
- `PATH` = the generated env (§10) plus resolved provider bins, so scripts never need to `source` anything.
- stdout and stderr go to `~/.local/state/dots/logs/<run-id>/<pkg>.<action>.log`. The tail is shown on failure.
- stdin is `/dev/null` unless `interactive`. This is what makes `newgrp`-style hangs impossible to ship unnoticed.
- After `install` exits 0, `check` must pass. If it does not, the result is **failure**: "install succeeded but check fails". This one rule catches P4.

### 5.4 User-defined providers (the "etc.")

Covers go, gem, pipx, nix-env, and anything else without writing Rust:

```toml
# dots.toml
[providers.go]
available = "command -v go"
bootstrap = "go"                                   # package that provides it
installed = { cmd = "ls ~/go/bin", parse = "lines" }  # or parse = "json:<jq-path>" / "regex:<re>"
version   = "go version -m ~/go/bin/{bin} | awk '/^\\tmod/{print $3}'"
install   = "go install {source}@{version|latest}"
batch     = false
```

Placeholders are templated by minijinja with the spec and facts in scope. If a declarative provider proves popular, promote it to built-in Rust.

### 5.5 Provider selection algorithm

For each package in the resolved set:

1. If `when` is false, the package is **inert** (shown in `status --all` as `n/a on this platform`).
2. Candidates = the providers declared by the package (or all of them, see §3.4) whose override `when` is true.
3. Order the candidates by the package's `prefer = [...]` if present, else by the `[priority]` list of the most specific platform key in the machine's chain (§4.1).
4. Pick the first candidate whose availability is `Ready` or `Bootstrap` and where `needs_root ⇒ facts.sudo`.
5. If the choice is `Bootstrap{pkg}`, inject `pkg` as a dependency. This is how `rust`/`rustup` becomes a prerequisite of every cargo package with no one writing it down.
6. Record the choice. **The choice is sticky**: if a package is already installed by *any* candidate provider, keep that provider even if the priority list would now prefer another. Switching providers is explicit: `dots migrate fd --to cargo` uninstalls from one and installs with the other. Otherwise you get `bat` installed by both apt and cargo, shadowing each other on `PATH`. `status` flags duplicates either way: "fd: installed by apt (selected) AND cargo (shadow)".

The `no_sudo` case falls out for free: `sudo=false` makes apt unavailable, so `fd` resolves to cargo. No `if $NO_SUDO` branches.

---

## 6. Files: links, renders, merges, injects

Every file entry has a `mode`:

| mode | What it does | Target is | When to use |
|------|--------------|-----------|-------------|
| `link` (default) | symlink dst → source in the active root (§11) | a symlink | the app never rewrites the file, or you *want* app edits to land in the repo (karabiner.json) |
| `copy` | copy once and track the hash | a regular file | the app refuses symlinks or replaces them atomically (some macOS apps) |
| `render` | combine layers + template → write to the state dir → symlink dst → the rendered file | a symlink into `~/.local/state/dots/gen/` | per-platform variants, merging (herdr, ghostty) |
| `inject` | manage a marked block inside a file dots does not own | a regular file | `~/.zshrc` source line, `~/.ssh/config` Include, `/etc/hosts` |
| `fragment` | contribute part of a file that **several packages** assemble | the same as render | `~/.gitconfig` gets `[delta]` from the delta package and `[core]` from base |
| `git` | clone a repo to dst (git provider) | a directory | shaders, plugin managers (tpm), KoalaConfig |
| `dir` | `mkdir -p` | a directory | `~/workspace/personal` (replaces dotbot `create`) |

Per-platform destinations: `dst = { default = "~/.config/ez/config.toml", macos = "{app_support}/ez/config.toml" }`.

### 6.1 Render pipeline

```
layers (filtered by `when`, in order: package layers → repo override layer → machine override layer, §18)
  ──► preprocess directives per layer (dots:if / @dots(...)@, §17.3)
  ──► parse by format ──► merge with strategies ──► structured `set` patches from settings (§17.4)
  ──► serialize ──► write atomically to gen/ ──► record hash ──► ensure dst symlink
```

Full minijinja templating is opt-in per file (`template = "jinja"`). It is not the default, because `{{ }}` collides with too many real config formats (gh-dash, helm-ish YAML, tmux format strings).

Formats: `toml` (via `toml_edit`, which preserves comments and order from the first layer), `json`, `jsonc` (Claude/VS Code settings files contain comments, so a plain JSON parser would corrupt them), `yaml`, `ini`/`gitconfig`, `text`.

Merge semantics for structured formats:

- Tables and objects: deep merge, the later layer wins per key.
- Arrays: **replace by default**. Appending everywhere, as `yq '*+'` does, is the herdr bug. It forced the `DEV_REPLACE_PATHS` workaround.
- Per-path override:
  ```toml
  [files.merge]
  "keys.bindings"           = "append"
  "ui.sidebar.spaces.rows"  = "replace"
  "plugins"                 = "union"      # dedupe by value
  "servers"                 = { by = "name" }  # merge array-of-tables by key
  ```
- Deleting a key in a later layer: the value `"__dots_delete__"` is reserved. It is ugly on purpose, so it is rare and greppable.
- Type mismatch at a path (a string in the base, an array in the overlay) is an **error** with both layer locations shown. It never coerces silently.

For `text`, layers are concatenated, each with a `# --- dots: from packages/ghostty/config/macos ---` header so you can tell where a line came from. **Prefer the app's native include where one exists**: `kind = "include"` renders a stub with `config-file = ...` (ghostty), `[include] path=` (git), `source-file` (tmux) or `source` (zsh). An app's own include semantics beat anything `dots` invents.

### 6.2 Fragments (multi-package composition)

```toml
# packages/delta/package.toml
[[files]]
dst = "~/.gitconfig"
mode = "fragment"
format = "gitconfig"
src = "gitconfig.fragment"
priority = 50                 # lower first; base package uses 0
```

The engine groups all fragments by `dst` and renders them like layers. Two packages setting the same key at the same priority is a **lint error**. A package that is removed takes its fragment out of the composed file, and that is exactly what you want when you uninstall delta.

### 6.3 Drift and adoption

`dots` keeps `{dst, mode, source_rev, rendered_sha256, link_target}` in the state for everything it owns. `status` reports each file as one of:

| state | meaning |
|-------|---------|
| `ok` | matches |
| `missing` | dst absent |
| `conflict` | dst exists and dots does not own it (first apply, or the user replaced it) |
| `modified` | a rendered/copied file was edited after render (the app or the user wrote to it) |
| `stale` | a source layer changed and the file has not been re-rendered |
| `dangling` | the symlink target is gone (typical cause: worktree deleted, see §11) |
| `foreign-entries` | a linked dir contains files not in git (the herdr sockets/logs case) |

For `modified`, run `dots adopt <dst> [--layer macos]`. It computes the structural diff against the render and writes it into the chosen layer (by default the most specific layer whose condition is true). For text it opens a 3-way merge. This is the answer for apps that rewrite their own config.

### 6.4 Safety

- **Ownership**: dots only modifies or removes paths recorded as owned in the state. Removing a package or file entry removes its owned dst. Anything else is reported, never touched.
- **Conflict on first apply**: move the existing file to `~/.local/state/dots/backup/<run-id>/<abs-path>`, record it, then link. `dots restore <run-id>` puts the backups back. This replaces `force: true`.
- **Atomic writes**: write to `tmp` in the same dir, `fsync`, then `rename`. Symlink replacement: create `dst.dots-tmp`, then `rename` over `dst`.
- **Parent dirs**: created. The created ones are recorded, so they are removed on teardown only if empty and created by dots.

### 6.5 Windows specifics

Symlinks need Developer Mode or admin. At startup `dots` probes whether it can create a symlink. If not, file links fall back to **hardlinks** and directory links to **junctions**, and the link kind is recorded per entry. Hardlinks break when an editor saves by atomic rename. That case is detected as `modified` and fixed by relinking, and `dots doctor` tells you to enable Developer Mode. Path vars `%APPDATA%` and `%LOCALAPPDATA%` are expanded. `~` means `%USERPROFILE%`.

---

## 7. Status and visibility

### 7.1 Commands

```
dots status              # summary: counts per state + only the non-ok rows
dots status --all        # every package/file/todo, including inert ones
dots status <pkg>        # deep view: provider choice + why, versions, files, todos, history tail
dots outdated            # packages with newer versions (network; cached)
dots plan                # exact actions apply would take, with commands
dots why <pkg>           # inclusion path: machine → bundle cli → bundle base → requires
dots bins [pkg|provider] # every executable on PATH → owning package/provider/version; shadowing (§7.4)
dots which <bin>         # one binary: every copy on PATH, which one wins, who owns each
dots doctor              # environment problems: missing providers, dangling links, PATH order, stale lock, sudo
dots facts               # detected facts + tags
```

### 7.2 Output

```
$ dots status
root   ~/dotfiles @ main f4522416 (clean)      machine: work-mbp   macos/aarch64   tags: work,gui

PACKAGES  62 ok · 3 outdated · 1 missing · 1 drift · 2 orphan
  bottom        cargo   want latest  have 0.9.6    latest 0.10.2   outdated
  neovim        github  want 0.11.5  have 0.11.4                   drift (lock says 0.11.5)
  wiremix       -                                                  n/a (os = linux)
  zellij        brew    not declared  have 0.41.1                  orphan (installed by worktree try-zellij, 2026-11-20)
  herdr         script  want latest  -                             missing

FILES     84 ok · 1 modified · 1 foreign-entries
  ~/.claude/settings.json   render   modified (app wrote 2 keys)   → dots adopt
  ~/.config/herdr/          link     foreign-entries: herdr.sock, herdr-server.log, sessions/

TODO      2 pending
  karabiner/permissions         Grant Input Monitoring + Accessibility
  ghostty/disable-secure-input  Disable Secure Keyboard Entry

GIT       koala-config  dirty (3 files), 2 ahead of lock
```

Every command takes `--json`, with one stable, schema-versioned document per command. That lets you put a `dots` indicator in sketchybar or agents-status (pending todo count, outdated count) without scraping.

Exit codes: `0` ok, `1` error, `2` drift present (`status`, so CI or a shell prompt can check it), `3` plan non-empty (`plan --check`).

### 7.3 Caching and speed

- `installed()` is never cached. It is local and batched.
- `latest()` is cached in `~/.cache/dots/latest/<provider>.json` with a TTL (default 6 h; `--refresh` forces a fetch). `status` shows `latest` only from cache and never blocks on the network. `outdated` refreshes.
- Providers are queried in parallel (rayon/scoped threads; no tokio, because nothing here needs async).
- Target: `dots status` in under 1.5 s warm on 100 packages. Measure it in CI with fake providers.

### 7.4 Binary inventory (`dots bins`, `dots which`)

The question it answers: "What is on my PATH, and where did each executable come from?" That covers dots-managed packages, packages installed outside dots and things nobody owns.

```
$ dots bins
BIN        PATH                              PACKAGE      PROVIDER  VERSION   NOTE
bat        ~/.cargo/bin/bat                  bat          cargo     0.24.0
           /usr/bin/batcat                   (bat)        apt       0.19.0    shadowed; duplicate install → dots migrate
fd         ~/.local/bin/fd                   fd           apt       8.7.0     shim → /usr/bin/fdfind
rg         /opt/homebrew/bin/rg              ripgrep      brew      14.1.1
kv         ~/.cargo/bin/kv                   kv           cargo     git:9e1c0b7
delta      ~/.cargo/bin/delta                -            cargo     0.18.2    unmanaged (installed outside dots)
foo        ~/.local/bin/foo                  ?            -         -         unknown owner
```

Filters: by package (`dots bins ripgrep`), by provider (`dots bins cargo`), or by state (`--managed`, `--unmanaged`, `--unknown`, `--shadowed`). `--json` emits the same rows.

How it is built. The steps below run in parallel per provider, and the results share the `installed()` cache with `status`:

1. **Forward scan (what each provider says it installed).** Batched per provider:

   | provider | source of bins |
   |----------|----------------|
   | brew | `brew info --json=v2 --installed` gives each formula's keg. List `<keg>/bin` and `<keg>/sbin`, then map to the `$(brew --prefix)/bin` links. Casks: the `binary` artifacts in the cask JSON. |
   | apt | Run `dpkg-query -L` once for the dots-managed packages and keep the paths under PATH dirs. Everything else goes through the reverse lookup (step 3). |
   | cargo | the `bins` arrays in `~/.cargo/.crates2.json` (exact, no subprocess) |
   | uv | `uv tool list --show-paths` (entry points per tool) |
   | npm | the `bin` field of each global package's `package.json` under the dots-managed prefix |
   | github-release / script / shims | the paths dots placed, recorded in `state.json` when it installed them. Scripts may also declare `bins = [...]`. |
   | git | none, unless the package declares `bins` |
   | custom providers | an optional `bins` command template; otherwise the package's declared `bins` |

2. **Walk PATH.** Use the same PATH as the user's shell: dots' generated env plus the live `$PATH`. List every executable file in every PATH dir, in order. Resolve symlinks to their real path. Since the first match on PATH wins, every later copy of the same name is marked **shadowed**.
3. **Attribute the paths.** First match the real path against the forward-scan table. If nothing matches, use the **reverse lookup** for the path's area. Under `/usr/bin` and `/usr/sbin`, one batched `dpkg -S` call (or `rpm -qf`/`pacman -Qo`). Under the brew prefix, the Cellar path segment. Under `~/.cargo/bin`, crates2. What remains is `unknown`.
4. **Join with dots' model.** For each binary, record whether its package is declared and dots-managed, installed but not declared (`unmanaged`), or `orphan` (installed by dots, no longer declared, §8.3f).

What it feeds:

- `status` adds a **BINS** line only when something needs attention: a shadowed copy of a managed binary, a declared package whose expected `bin` is missing from PATH, or the same name owned by two providers.
- `doctor` explains shadowing caused by PATH order (`~/.cargo/bin` after `/usr/bin`) and points at the env file (§10).
- `dots why <bin>` works the same as `dots why <pkg>`: bin → package → bundle chain.
- The registry in §8.3(d) uses declared bins and checks them **statically** at resolve time. `dots bins` checks the **actual** system. Both report the same kinds of collision, at different times.
- A full `dpkg -S` over `/usr/bin` takes about 1 s, so `bins` is a separate command and does not run as part of the default `status`. Only checks against the bins of managed packages run in `status`, and those are cheap.

---

## 8. Versions and the lock

### 8.1 Desired versions

There is **one small grammar** with five forms. It deliberately has no `^`, no `~`, no `||` and no comma-ranges. Every form must be explainable in one line, and two specs must always intersect predictably.

| Spec | Meaning | Example matches |
|------|---------|-----------------|
| `"latest"` (default; also what an omitted `version` means) | No constraint. Install the newest version; afterwards the **lock** decides (§8.2). | anything |
| `"1.2.3"` | **Exact.** | `1.2.3` only |
| `"1.2"` / `"1"` | **Prefix**, matched component by component. The newest matching version wins. | `"1.2"` → `1.2.0`, `1.2.17`; not `1.20.0` or `1.3.0` |
| `">=1.2"` | **Floor.** Any version at or above it. The newest wins. | `1.2.0`, `1.9`, `2.0` |
| `"git:<ref>"` | A git ref (sha, tag or branch), for git-sourced packages only (git, cargo-git, github-release by tag). | `git:v0.4.1`, `git:main`, `git:9e1c0b7` |

That covers every real case: "don't care" (latest), "stay on the 0.11 line" (prefix), "I need a feature from 1.4" (floor), "reproduce exactly" (exact), and "track a repo" (git).

**Version comparison**, the same for every provider:

1. **Normalize** the provider's string to get the *upstream version*. Drop a leading `v` and a Debian epoch (`1:`). Cut at the packaging revision (`-0ubuntu1`, `_1` for brew revisions, `+deb12u1`). The raw string is kept for display and for the lock.
2. Split on `.` and `-`. Compare component by component. Numeric components compare numerically, and an alphanumeric component is lower than a numeric one at the same position, so `1.2.0-rc1 < 1.2.0`.
3. Pre-releases (a component containing letters after the first numeric component, such as `rc`, `beta` or `nightly`) are **never** chosen by `latest`, a prefix or a floor. They are chosen only by an exact spec or `git:`.
4. If a version cannot be parsed (a date stamp or a hash), only `latest`, exact and `git:` apply to it. Prefix and floor are a lint error for that package, never a guess.

### 8.2 `dots.lock`

```toml
# Generated by dots. Do not edit. One block per (package, platform-key).
schema = 1

[[pkg]]
name = "bottom"
platform = "macos-aarch64"
provider = "cargo"
version = "0.10.2"
locked_at = "2026-11-25T20:11:03Z"
by_rev = "f4522416"                # repo commit this lock entry was produced at

[[pkg]]
name = "kv"
platform = "*"                      # git sources are platform-independent
provider = "cargo"
source = "git+https://github.com/KoalaVim/kv"
rev = "9e1c0b7d2f..."

[[pkg]]
name = "koala-config"
platform = "*"
provider = "git"
rev = "3a77c1e..."
```

- It is **keyed by platform** because brew on macOS and apt on Ubuntu resolve the same package to different versions. One lock covers all machines, and each machine reads its own rows. The key is the machine's **most specific chain key plus its arch** (`ubuntu-24.04/x86_64`, `macos-15/aarch64`), because 22.04 and 24.04 apt really do differ. For providers whose versions don't depend on the platform (cargo, uv, npm, git, github-release by tag), the row uses `platform = "*"` and is shared.
- It is sorted deterministically, one block per entry, so merging between branches and worktrees causes few conflicts. `dots lock --fix` regenerates the file after a bad merge.
- `apply` installs the **locked** version where the provider supports exact versions. Where it does not (brew, apt), it installs what the repo offers and reports `drift` if that differs. The lock is honest about what it can guarantee: `dots status --strict` treats `Limited` drift as an error, and by default it is a warning.
- `dots upgrade [pkg...|--all] [--provider cargo]` upgrades, rewrites the lock entries for **this platform** and journals the change. It does not commit. `dots upgrade --commit` makes one commit with a generated message: `dots: upgrade bottom 0.9.6→0.10.2, delta 0.17→0.18 (macos-aarch64)`.
- `dots pin <pkg> [version]` sets `version` in the package TOML (via `toml_edit`, so formatting is preserved) and applies the provider-native pin (`brew pin`, `apt-mark hold`), so `brew upgrade` run behind dots' back does not move it.

### 8.3 Dependencies with versions

`requires` entries can carry a version spec and a condition:

```toml
requires = [
  "nerd-fonts",
  { name = "rust", version = ">=1.80" },               # needs edition 2024 features to build
  { name = "node", version = "22", when = 'os = "linux"' },
]
```

Dependency resolution has to answer a few questions. Each one gets **one** rule, and every rule fails loudly instead of guessing:

**(a) Several constraints on the same package.** Constraints come from the package itself, from every `requires` that points at it and from overrides. The resolver **intersects** them:

| A ∩ B | Result |
|-------|--------|
| `latest` ∩ X | X |
| floor ∩ floor | the higher floor |
| prefix ∩ prefix | the longer prefix, if one extends the other (`1` ∩ `1.4` → `1.4`); else **conflict** |
| prefix ∩ floor | the prefix, if some version under the prefix can be ≥ the floor (`1.4` ∩ `>=1.2` → `1.4`, but `1.4` ∩ `>=1.5` → conflict) |
| exact ∩ X | the exact version, if it satisfies X; else **conflict** |
| `git:` ∩ `git:` | allowed only if both name the same ref; else **conflict** |
| `git:` ∩ numeric | **conflict**: you cannot reason about a sha's version (fix with an override) |

A conflict is an error that shows the full provenance of each side, in the style of `dots why`:

```
error: version conflict for `rust`
  ">=1.80"  from  packages/kv/package.toml        (bundle cli → kv → requires)
  "1.75"    from  overrides/rust/override.toml    (repo override)
  no version satisfies both. Fix one of them, or override `kv`'s requires.
```

**(b) Overrides.** The `version` in an override **replaces** the package's *own* spec. It does not replace the constraints that dependents put on the package, because those are the dependents' business. To loosen what a dependent demands, override the dependent. The rule is boring on purpose: you can always see who constrained what.

**(c) The selected provider cannot deliver the constraint.** Provider selection (§5.5) is constraint-aware. A candidate is skipped when it cannot satisfy the final intersected spec: brew only offers its current version (plus `@`-formulae), apt only offers what `apt-cache policy` lists, and `git:` needs a git-capable provider. The plan says why: "fd: skipped brew (offers 10.2.0, need 8.7), using cargo". If no candidate can satisfy it, that is an error listing what each one offers. There is no silent fallback to "close enough".

**(d) Two packages provide the same binary.** Every package's `bin`/`bins` (plus shims) are registered. If two **resolved** packages claim the same binary name, that is an error, unless one declares `conflicts = ["other"]` (then they are mutually exclusive, and selecting both is the error you want) or `replaces = ["other"]` (then the other one is dropped from the set and uninstalled if dots owns it). This catches `vim` vs `neovim`-as-`vim` and two packages both shimming `fd`.

**(e) Cycles.** A cycle in `requires`, `after` or an implicit setting reference is an error that prints the cycle (`a → b → c → a`). There is no "break the cycle somewhere".

**(f) Dependencies the package manager handles itself.** A formula's brew dependencies, apt's own deps and cargo's crate graph belong to the package manager. `dots` neither declares nor tracks them. That is why orphan detection only counts packages **dots installed** (according to the journal) that are no longer declared. Things installed as a dependency, or by hand, show as `unmanaged` and only in `status --all`. To read "manual vs automatic", dots uses `brew info --installed-on-request` and `apt-mark showmanual`. Without that, every library brew pulled in would show up as an orphan.

**(g) The lock vs the spec.** The plan uses these rules:

1. If the locked version satisfies the current spec, install or keep **the locked version**. Machines stay reproducible even under `latest`.
2. If the spec changed and the lock no longer satisfies it, resolve the **newest version that satisfies it** and rewrite that lock row. The plan shows the row change.
3. If the installed version satisfies the spec but differs from the lock: **upgrades** happen only through `dots upgrade`, and **downgrades are never implicit**. The plan marks every downgrade with a `⚠ downgrade` line, and `apply` asks even with `--yes`, unless `--allow-downgrade` is given.
4. If the version is already installed outside dots and satisfies the spec, **adopt** it. Record it in the lock and the journal (`op: adopt`) and do not reinstall it.
5. If the provider can't produce the locked version (brew or apt has moved on): install what the provider offers, if it satisfies the spec, and report `drift` (§8.2). If it does not satisfy the spec, that is an error under rule (c).

**(h) Upgrades respect constraints.** `dots upgrade X` moves X to the newest version allowed by the intersected constraints (so a prefix `0.11` upgrades within 0.11.x). Leaving the range is done by editing the spec, never by `upgrade`. When X's new version would violate a dependent's constraint, the upgrade is refused and the dependent is named. Upgrading a package whose dependencies have tighter floors upgrades those dependencies first, as part of the same plan, and shows them.

---

## 9. Manual actions

```markdown
<!-- packages/karabiner/manual/permissions.md -->
---
id: permissions                    # unique within package; full id = karabiner/permissions
title: Grant Input Monitoring + Accessibility
when: 'os = "macos"'
after: install                     # install | files | always ; when it becomes pending
check: "sqlite3 ~/Library/Application\\ Support/com.apple.TCC/TCC.db ... | grep -q karabiner"   # optional auto-verify
requires: [logout]                 # logout | reboot | restart-app:<name>  (shown as badges, aggregated)
revision: 2                        # bump when the steps change → becomes pending again on every machine
---

1. System Settings › Privacy & Security › **Input Monitoring** → enable `karabiner_grabber`
2. System Settings › Privacy & Security › **Accessibility** → enable `karabiner_observer`

Verify: press a remapped key.
```

```
dots todo                       # pending items, grouped by package, with badges
dots todo show karabiner/permissions   # renders the markdown (termimad) + `o` opens any URL / settings pane
dots todo done <id> [--note "..."]
dots todo skip <id>             # never ask again on this machine (until revision bumps)
dots todo check                 # run all `check`s; auto-close what passes
```

- Status is per machine in the state: `{id, revision, status: pending|done|skipped|auto, at, repo_rev, note}`.
- A `check` turns a todo into a self-verifying item. `apply` runs the checks at the end and closes what passes. Anything without a check stays pending until a human marks it done.
- `apply` ends with a summary block such as "2 manual actions pending, 1 requires logout". This is where today's `MANUAL.md` content goes, one file per action.
- Both manual items and scripts that need a terminal set `interactive`. `apply` collects them and runs them **last, serially, with a prompt before each**. An unattended `apply --yes` never blocks on them: they become todos instead.

---

## 10. Environment (PATH and vars)

Packages contribute:

```toml
[env]
path = ["~/.cargo/bin"]                # prepended, ordered by package priority
vars = { NPM_CONFIG_PREFIX = "~/.npm-packages" }
source = ["~/.cargo/env"]              # last resort for tools that insist
```

`dots` renders `~/.local/state/dots/env.sh`, `env.fish` and `env.ps1`, and makes sure **one** `inject` block exists in the shell rc (`~/.zshenv` here, since zsh-conf is the setup):

```sh
# >>> dots env >>>
[ -f "$HOME/.local/state/dots/env.sh" ] && . "$HOME/.local/state/dots/env.sh"
# <<< dots env <<<
```

The executor uses the same env internally, so a package installed earlier in a run is visible on `PATH` to later steps, with no reshelling. `dots which` and `doctor` use it to detect shadowing: `/usr/bin/bat` vs `~/.cargo/bin/bat`.

---

## 11. Worktrees

Requirements: you can try a change (new package, new config, a different WM setup) in a git worktree, *activate* it on the real machine, use it, then either merge it or abandon it and get back to exactly where you were. Deleting the worktree must not leave dangling symlinks everywhere.

### 11.1 The active-root indirection

Links do **not** point into a checkout. They point through one pointer:

```
~/.config/ghostty/config ─► ~/.local/state/dots/active/packages/ghostty/config/...   (render: gen/<root-id>/...)
~/.local/state/dots/active ─► ~/dotfiles                 (or ~/dotfiles.worktrees/try-zellij)
```

- `dots use <worktree|branch|path>` makes one atomic `rename` of the `active` symlink. On Windows, `active` is a junction, swapped by remove + create inside the run lock.
- Then it **reconciles the difference** between the two roots: links that exist only in the new tree are created, links that exist only in the old one are removed (owned ones only), and renders are regenerated into `gen/<root-id>/`. Because unchanged links go through `active`, most files switch with zero filesystem operations per file.
- Generated files are stored per root (`gen/<root-id>/`), so switching back is instant and nothing is regenerated.
- `root-id` = a stable hash of the worktree path. `state.json` maps `root-id → {path, branch, last_used}`.

### 11.2 Packages under worktrees

Installing software is not reversible as cheaply as moving a symlink. So:

- Every journal entry records `root-id` + branch + commit SHA.
- `dots use main` (back from `try-zellij`) computes: packages installed by runs in `try-zellij` that are not declared in `main`. It prompts per package: **remove** (uninstall), **keep** (it becomes an orphan and `status` keeps showing it as one), or **adopt** (copy the package dir into the current tree, for when you meant to keep it).
- Version changes made in a worktree (`upgrade` there) are offered for **rollback** only when the provider supports exact versions. Otherwise the plan says "cannot downgrade brew:foo 1.3→1.2; keeping 1.3".

### 11.3 Workflow commands

```
dots try <name> [--from main]   # git worktree add ../dotfiles.worktrees/<name> -b try/<name>; dots use it; print how to get back
dots use <name|path>            # switch active root (reconcile + prompts above)
dots use -                      # switch to previous root
dots diff <a>..<b>              # desired-state diff between two roots (packages, files, renders), without applying
dots plan --root <path>         # plan from any checkout, without activating it
dots try --finish               # back to previous root; offers: merge branch | keep worktree | delete worktree
dots roots                      # list known roots, which one is active, dangling ones
```

`--home <dir>` sandbox: `dots apply --root <wt> --home /tmp/fakehome --files-only` applies every file operation into a throwaway HOME. Use it to test rendering, merging and links without touching anything real. CI uses this too.

### 11.4 Invariants

- There is exactly one active root per machine. `apply` from a cwd inside a non-active worktree refuses with "this is not the active root; run `dots use .` or pass `--root`". This prevents half-applied mixtures.
- **One global run lock**: `flock` on `~/.local/state/dots/lock`. Two terminals running `apply` from different worktrees serialize instead of corrupting the state.
- If the active root's path disappears (`git worktree remove` on the active tree), `status` and `doctor` report `active root missing` and offer `dots use main`. Links resolve again as soon as `active` points somewhere valid, which is the reason for the indirection.
- **Git operations are done by shelling out to `git`**: worktrees, SSH auth, credential helpers and the user's config all just work. Use `gix` only for read-only fast paths later, if profiling justifies it.

---

## 12. History

Two complementary records, each answering a different question.

**A. Repo history ("what did I decide?")**: git itself. Package dirs and `dots.lock` are committed, so the git log of `packages/<p>` and its lock rows tells you when the package was added and when its version moved. Commits made by dots (`upgrade --commit`, `add`, `pin`) carry trailers:

```
dots: add ghostty from ofirgall/dotfiles@f4522416

Dots-Op: add
Dots-Package: ghostty
Dots-Origin: github:ofirgall/dotfiles@f4522416c0d0
```

**B. Machine journal ("what actually happened here?")**: `~/.local/state/dots/journal.jsonl`, append-only, one line per action:

```json
{"ts":"2026-11-25T20:11:03Z","run":"r-7f3c","host":"work-mbp","op":"upgrade","pkg":"bottom","provider":"cargo",
 "from":"0.9.6","to":"0.10.2","root":"main","repo_rev":"f4522416","repo_dirty":false,
 "upstream_rev":null,"dots":"0.1.0","exit":0,"dur_ms":48211,"log":"logs/r-7f3c/bottom.upgrade.log"}
```

Ops: `install, upgrade, downgrade, remove, pin, unpin, link, unlink, render, inject, backup, restore, todo_done, todo_skip, use_root, adopt, migrate_provider`.

`dots log [pkg] [--since 30d] [--host]` merges both sources, git log (filtered by `packages/<p>` + trailers) and the journal, into one chronological timeline:

```
2026-11-25 20:11  upgrade   bottom 0.9.6 → 0.10.2   cargo   main@f4522416         (this machine)
2026-11-20 14:02  install   zellij 0.41.1           brew    try-zellij@ab12cd3    (this machine)
2026-10-02 09:30  commit    add bottom              -       3e9a771 "cli: add bottom"
```

JSONL and not SQLite: it is greppable, `tail -f`-able, survives partial writes (lines are written with `O_APPEND`, one `write` each), and needs no schema migration. The current state (`state.json`) is small and rewritten atomically. If the journal ever gets big, rotate it yearly.

---

## 13. Reusing other people's setups: copy or fork

### 13.1 Copying

```
dots add ofirgall/dotfiles:ghostty                 # package
dots add ofirgall/dotfiles:bundle/cli              # bundle (+ all its packages)
dots add github:owner/repo@v1.2:pkg                # pinned rev/tag/branch
dots add git+ssh://git@host/x.git:pkg
dots add ../other-dots:pkg                         # local path
dots add ofirgall/dotfiles:ghostty --as ghostty-ofir --no-deps
dots search ofirgall/dotfiles [query]              # list packages/bundles in a remote, with descriptions
```

Resolution:

1. Fetch into `~/.cache/dots/remotes/<host>/<owner>/<repo>.git` (a bare mirror, `git fetch` on reuse), and check out the requested rev into a temp worktree.
2. Read their `dots.toml` and check `schema`/`requires_dots` compatibility.
3. Resolve the item: `name` searches packages first. If a bundle has the same name, that is an **error**, and you disambiguate with `pkg/` or `bundle/`.
4. Compute the closure over `requires` and over bundle membership. Show it as a tree, marking what already exists locally (by name *and* by origin).
5. **Show what will execute**: every script, hook and `check` command of the copied packages, in a pager. A copied package runs code on your machine, so the user confirms (`--yes` skips this, explicitly).
6. Copy the dirs into `packages/` and write the `[origin]` table (repo, path, full rev SHA, tree hash). For a bundle, write `bundles/<name>.toml` with `[origin]` as well.
7. Name collision: refuse, or `--as <name>`. Never overwrite silently.
8. Custom providers or tags the package uses that do not exist locally are listed with an offer to add them to `dots.toml`.

Keeping copies in sync (this is why the base rev is recorded):

```
dots origin status                 # for every package with [origin]: upstream changed? local changed? both?
dots origin diff ghostty           # upstream base..head diff for that path
dots origin update ghostty         # 3-way merge: base = origin.rev, ours = local dir, theirs = upstream head (git merge-file per file)
dots origin detach ghostty         # drop [origin]; it becomes yours
```

**This is a copy, not a dependency.** There is no runtime reference to the remote. If the remote vanishes, nothing breaks. Updating is an explicit merge.

### 13.2 Forking (extend instead of copy)

Copying gives you ownership of the files. **Forking** keeps someone else's repo (or part of it) as a pinned **upstream**, and your repo holds only your overrides on top. It's the right choice when you mostly like what they have and want their improvements to keep flowing in.

```toml
# dots.toml
[[upstream]]
alias  = "ofir"
url    = "github:ofirgall/dotfiles"
follow = "main"                                   # what `dots fork update` moves to
import = ["bundle:cli", "bundle:mac-desktop"]     # or ["*"]; the requires-closure comes along automatically
rename = { "pkg:ghostty" = "ghostty-ofir" }       # resolve a name clash with something of yours
trust  = "review"                                 # review (default): script changes need confirmation on update | auto
```

The exact rev is pinned in `dots.lock` (`[[upstream]] alias = "ofir", rev = "<sha>"`). A fork never moves without `dots fork update`.

```
dots fork ofirgall/dotfiles:bundle/cli    # adds/extends an [[upstream]] entry and imports that bundle
dots fork status                          # per upstream: commits behind, which imported items changed, stale overrides
dots fork diff ofir                       # upstream pinned..follow, limited to imported items, scripts first
dots fork update [ofir]                   # fetch, review, check overrides still apply, bump lock rev, show plan
dots fork pin ofir <rev>                  # go back to (or stay on) a specific upstream rev
dots fork vendor ofir [item]              # convert to a copy: copies items with [origin], drops the import
```

How it resolves:

1. **Each upstream is resolved on its own first**: its own upstreams (forks chain, so C → B → A works; a cycle is an error), its definitions and its *committed* overrides. Its machine profiles and machine scope never cross over.
2. The result is checked out **immutably per rev** at `~/.local/share/dots/upstreams/<alias>/<rev>/`. This lives in the data dir and not the cache, because links and renders point into it. Old revs are garbage-collected once nothing dots owns references them.
3. **Namespacing:** every upstream item is addressable as `<alias>:<name>` (`ofir:ghostty`). Imported items are *also* available under their bare name, unless that name is already defined locally, which is an error until you `rename` or stop importing it. Two upstreams exporting the same imported name is also an error naming both. Nothing ever shadows silently.
4. **Layering:** the upstream's `dots.toml` contributions (priorities, provider options, tags, platforms, custom providers, repo-scope setting values) form the **lowest layer**. Your `dots.toml` sits on top, then your `overrides/`. Upstream setting values become an `upstream` scope in the §17.2 precedence table, between bundle and repo.
5. **You customize with ordinary overrides** (§18), addressed the same way whether the item is local, copied or forked. `overrides/packages/ghostty/override.toml` just works.

Updating safely (`dots fork update`):

- Show the changes in imported items, with **new or changed scripts, hooks and checks first**. Under `trust = "review"` they need explicit confirmation, the same as `dots add`.
- **Stale overrides**: an override whose target (file dst, setting, manual id, bundle member) no longer exists upstream is an error. An override that now sets a value identical to upstream is a warning ("this override is now redundant"). A setting whose type changed upstream is an error.
- Then rebuild the plan. The update is only a lock change plus whatever `apply` does next, so `git revert` of the lock commit is a full rollback.
- Offline: pinned revs stay checked out, so `apply` works without the network. `doctor` reports a missing checkout.

**Copy vs fork:**

| | `dots add` (copy) | `dots fork` (extend) |
|---|---|---|
| Who owns the files | you, in `packages/` | upstream, pinned rev |
| How you customize | edit directly, or overrides | overrides only |
| Getting their changes | `dots origin update` (3-way merge per item) | `dots fork update` (bump rev; overrides re-applied) |
| If their repo disappears | nothing breaks | pinned checkout keeps working; you can't update (vendor it) |
| Best for | taking a piece and making it yours | following someone's setup with your tweaks |

Converting between them: `dots fork vendor` turns a fork into a copy. `dots origin to-fork <pkg>` turns a copy back into a fork only if your local edits can be expressed as overrides. dots computes the structural diff against the origin rev and writes it to `overrides/`. If some edit can't be expressed that way (for example free text in a linked file), it shows those edits and refuses.

---

## 14. Execution engine

### 14.1 Pipeline

```
load repo + machine + facts
   │
resolve ──► package set (bundles ∪ requires, minus exclude, `when` applied)
   │        provider choice per package (§5.5), bootstrap deps injected
   │        file set (entries + fragments grouped by dst), todo set
   │
query  ──► provider.installed() ×N in parallel; fs stat of all dsts; state.json
   │
diff   ──► Plan: typed actions with deps
   │         Bootstrap(provider) → Install/Upgrade(batched per provider) → Files → Hooks → Env → Todos(check)
   │
show   ──► `plan` stops here. `apply` prints it and asks unless --yes or the plan only touches files.
   │
exec   ──► DAG scheduler:
             - providers with exclusive_lock run their batch serially; different providers run in parallel
             - all root-needing actions are grouped, and sudo is primed ONCE (`sudo -v`) with a keepalive
             - interactive actions are deferred to the end, serial
             - a failed node fails its dependents; independent branches continue
   │
record ──► journal per action, state.json atomically at the end (and incrementally after each node,
           so a crash mid-run leaves accurate ownership)
   │
summary──► ok / failed (with log tails) / skipped-due-to-dep / todos / requires-logout
```

### 14.2 Commands (complete v1 surface)

```
dots init [--from <remote>]           # create dots.toml + machine.toml; optionally bootstrap from a remote repo
dots apply [pkg|bundle...] [--files-only|--packages-only] [--yes] [--dry-run] [--root P] [--home D]
dots plan  [...same selectors]
dots status | outdated | why | which | bins | doctor | facts
dots upgrade [pkg...|--all] [--provider X] [--commit]
dots pin | unpin <pkg> [ver]
dots remove <pkg>                     # removes from bundles + uninstalls + unlinks (asks)
dots migrate <pkg> --to <provider>
dots add | search | origin ...
dots todo [show|done|skip|check]
dots adopt <dst> [--layer L]
dots try | use | roots | diff
dots log [pkg]
dots restore <run-id>
dots config list|get|set|unset|explain     # §17
dots questions                             # §17/§19
dots show <pkg> [--effective]
dots override list|edit|diff|eject <address>   # §18
dots fork <src> | status | diff | update | pin | vendor   # §13.2
dots origin status|diff|update|detach|to-fork          # §13.1
dots skill install                         # §19
dots lint                             # schema, unknown tags/facts, fragment collisions, // escapes, missing check cmds
dots import dotbot <file.yaml>...     # migration, §16
dots completions <shell>
```

### 14.3 Bootstrap on a fresh machine

`dots` is one static binary (musl on Linux, universal2 on macOS, MSVC on Windows), published as GitHub releases. The bootstrap:

```sh
curl -fsSL https://.../install.sh | sh     # downloads dots binary to ~/.local/bin, verifies checksum
dots init --from ofirgall/dotfiles          # clones to ~/dotfiles, asks bundles/tags, writes machine.toml
dots apply
```

Homebrew, rustup and uv are ordinary packages that act as provider bootstraps. `dots` itself is a package too (provider `github-release`), so it upgrades itself through the same path and its version is recorded in the lock.

---

## 15. Rust implementation layout

```
crates/
  dots-cli/        clap derive, output formatting (tables, --json), prompts
  dots-model/      serde types for dots.toml/package/bundle/machine/lock/manual frontmatter; strict parse; lint
  dots-cond/       cfg() expression parser + evaluator
  dots-facts/      detection per OS
  dots-resolve/    bundle expansion, requires closure, provider selection, cycle detection
  dots-plan/       desired vs actual diff → Plan (pure, heavily unit-tested)
  dots-exec/       DAG scheduler, sudo session, logs, run lock
  dots-providers/  trait + built-ins (one module each) + declarative provider
  dots-files/      link/copy/render/inject/fragment, format adapters, merge strategies, patches, adopt
  dots-directive/  dots:if/elif/else/end + @dots()@ preprocessor (pure, line-based, fuzz-tested)
  dots-settings/   schema, typed values, scope precedence, explain chains; override merging
  dots-state/      state.json, journal.jsonl, backups, ownership
  dots-git/        thin wrapper over the `git` CLI (worktrees, remotes, merge-file, trailers)
  dots-remote/     `add`/`origin` resolution and caching
```

Key crates: `clap`, `serde`, `toml_edit` (for **writing** user files with formatting preserved; `toml` for reading the lock), `serde_json`, `jsonc-parser`, `serde_yaml_ng` (or `yaml-rust2`; check maintenance status at implementation time), `minijinja`, `semver`, `similar` (diffs), `termimad` (rendering manual md), `fs4`/`fd-lock` (flock), `sha2`, `ureq` (blocking HTTP; no tokio), `rayon`, `tracing`.

Testing strategy:

- `dots-plan` is pure. Golden tests: (repo fixture, facts, fake installed state) → expected plan.
- Every provider gets a **fake** implementation for engine tests and a **contract test** suite that real providers run in CI containers (ubuntu: apt/cargo/uv/npm; macOS runner: brew).
- File operations are tested against `--home` temp dirs. Windows CI covers the symlink-fallback path.
- `dots import dotbot` is tested against a copy of the real `~/dotfiles` YAMLs as a fixture.

---

## 16. Migration from `~/dotfiles`

1. `dots import dotbot common.conf.yaml install.conf.yaml macos.conf.yaml windows.conf.yaml`
   - Parse all four. Links with the same `dst → src` across files become **one** entry. Their `when` is derived from which YAMLs contain them (common = everywhere, unless `~/.remote_indicator` handling applies; `install.conf.yaml` = `all(os="linux", not(tag="remote"))`; and so on).
   - The importer reports P1 drift explicitly: "`~/.config/hunk` linked on linux only. Intended?". It does not paper over it.
   - Links are grouped into packages by the first path component of their source (`dotfiles/ghostty` → package `ghostty`) and the files are moved with `git mv`, so history is preserved.
   - `shell:` steps become `script` packages with `check = "false  # TODO"`, so they show as **unverified** in `status` until someone writes a real check. Nothing pretends to be idempotent.
   - `create:` steps become `dir` entries.
2. Hand-convert the hot paths: the `brew install` lines, the `cargo install` lines and the `apt install` lines become declarative packages (`dots import scan install_scripts/` can propose them by grepping the `brew|apt|cargo|pip|npm install` lines, and you confirm each one).
3. Submodules become `git` packages. Remove `dotbot`, the `.githooks` submodule auto-add and the `Makefile update`.
4. `MANUAL.md` sections become `manual/*.md` files under the owning packages.
5. herdr `generate-config.sh` becomes a `render` with `[files.merge]` paths. `DEV_REPLACE_PATHS` turns into `"<path>" = "replace"` lines, and since replace is now the default for arrays, most lines simply go away.
6. Ghostty's `config.platform` symlink becomes an include render. Remove the `.gitignore` entries for `config.platform`/`shaders`.
7. Run `dots status` until it is all green, then delete `install*`, `*.conf.yaml` and `install_scripts/` for what has been converted.

---

## 17. Package settings and `#ifdef`-style config

The problem: a package's config file is "mostly the same" across users and machines, with a handful of knobs (font size, theme, whether to enable the cursor shader, which keyboard layout, work vs personal git email). Today the options are to fork the file, add a platform layer or write a generator script. None of those scale, and none is visible in `status`.

### 17.1 Declaring settings (in `package.toml`)

```toml
[settings.font_size]
type = "int"                       # bool | int | float | string | enum | list<string> | path
default = 13                       # THE recommended value. Every setting must have one (or be `required`, see below)
range = [8, 32]
description = "Terminal font size in points"

[settings.theme]
type = "enum"
values = ["dark", "light", "auto"]
default = "dark"
description = "Color scheme"
ask = true                         # surfaced during onboarding (§19); false = silently defaulted
question = "Dark or light terminal theme?"

[settings.cursor_shader]
type = "bool"
default = false
when = 'os = "macos"'              # the setting doesn't exist elsewhere (cannot be set, not shown)

[settings.git_email]
type = "string"
required = true                    # no sane default → onboarding MUST ask; apply refuses to render until set
secret = false                     # true = never printed in status/log/json, stored only in machine scope
validate = '^[^@]+@[^@]+$'
```

Rules:

- Every setting has a `default` **or** is `required`. "Skip everything and take the recommended defaults" is therefore always possible, except for the `required` ones, and those are kept rare: identity data with no possible default. Lint warns when a package has more than two required settings.
- Settings are typed and validated at load time. A wrong type is an error that names the scope it came from (§17.2).
- Settings are available to:
  - conditions everywhere (`when` on layers, files, providers, manual actions, bundle entries),
  - directives inside config files (§17.3),
  - structured patches (§17.4),
  - scripts as `DOTS_SETTING_<NAME>` (lists are newline-joined; bools are `1`/`0`).
- A setting may choose behaviour beyond files. For example, `[providers.brew] when = 'setting.channel = "stable"'` / `[providers.github-release] when = 'setting.channel = "nightly"'` lets a setting choose between stable and nightly neovim.

### 17.2 Where values come from (precedence, low → high)

| # | Scope | Location | Committed | Typical use |
|---|-------|----------|-----------|-------------|
| 1 | package default | `packages/<p>/package.toml` | yes | recommended value |
| 2 | bundle | `[settings.<pkg>]` in a bundle file | yes | "mac-desktop bundle wants font 14" |
| 3 | upstream | `[settings.<pkg>]` in a forked upstream's `dots.toml` (§13.2) | theirs | values inherited from the setup you forked |
| 4 | repo | `[settings.<pkg>]` in `dots.toml` | yes | your personal choice across all machines |
| 5 | committed machine profile | `machines/<name>.toml` | yes | "work-mbp uses work email" |
| 6 | machine | `~/.config/dots/machine.toml` `[settings.<pkg>]` | no | this box only, secrets |

When two bundles set the same setting differently, the resolve fails with an error naming both bundles. A later bundle silently winning would be unreviewable. Fix it at a higher scope.

```
dots config list [pkg]                 # effective values, with the scope each came from
dots config get ghostty.font_size
dots config set ghostty.font_size 15            # machine scope by default (local, reversible)
dots config set ghostty.font_size 15 --repo     # writes dots.toml (committed)
dots config unset ghostty.font_size             # falls back to next scope
dots config explain ghostty.font_size           # full chain: default 13 ← bundle mac-desktop 14 ← machine 15
dots questions                                  # unanswered ask/required settings (machine-readable for agents)
```

The writes go through `toml_edit`, which preserves comments and ordering. A changed setting marks the affected renders `stale`, so `plan` shows exactly which files change, with a diff.

### 17.3 Directives in config files (the `#ifdef` part)

Directives live in the config file's **own comment syntax**, so the source file stays a valid, editable config of its format. Your editor, linter and syntax highlighting keep working. The comment leader is inferred from the format (`#` for toml/yaml/ini/gitconfig/sh/ghostty/tmux, `//` for jsonc, `--` for lua, `"` for vimrc, `;` for ini/kanata). Set it with `comment = "..."` on the file entry for anything unusual.

```
# ghostty config (packages/ghostty/config/common)
font-family = Nerdwand Mono
font-size = @dots(setting.font_size)@

# dots:if setting.theme = "light"
background = #ffffff
foreground = #000000
# dots:elif setting.theme = "auto"
theme = dark:Builtin Dark,light:Builtin Light
# dots:else
background = #000000
foreground = #ffffff
# dots:end

# dots:if all(os = "macos", setting.cursor_shader)
custom-shader = shaders/cursor_smear.glsl
# dots:end
```

Semantics, precisely:

- `dots:if <cond>` / `dots:elif <cond>` / `dots:else` / `dots:end`. `<cond>` is the §4 condition language, the same language as everywhere else. Nesting is allowed, with a depth limit of 8.
- Lines in inactive branches are **removed** from the output. The directive lines themselves are removed too.
- **Commented-out inactive lines**: if a branch's body lines are prefixed with the comment leader plus one space (`# background = #ffffff`), they are uncommented when the branch is active. This lets the source file show a "default-active" state that works even without dots. To mark that, write `dots:if! <cond>` ("body is commented in source"). Lint checks that every body line of a `dots:if!` really is commented.
- `@dots(expr)@` substitutes a value inline. `expr` is `setting.x`, `settings.<pkg>.x`, `fact.<name>`, `tag.<name>` (`true`/`false`) or `path.<var>`. An unknown name is an error, never an empty string. For strings that must be quoted in the target format, use `@dots(setting.x | json)@` and friends. The only filters are `json`, `toml`, `yaml`, `shell` (shell-quoting), `upper` and `lower`. It is deliberately not a template language.
- `dots:include <relative-path>` inlines a sibling file, with directives processed recursively. Use it sparingly; layers are usually better.
- Unbalanced directives, an unknown directive or an `elif` after `else` are hard errors with `file:line`.
- Strict JSON has no comments, so directives are unavailable there. Use the jsonc format (when the consumer accepts comments) or structured patches (§17.4). Lint refuses directives in a `format = "json"` file.
- Directive processing is applied per **layer** before merging, so an override layer can also contain directives.
- A file with directives but no layers or merges still goes through `render`. A `link` to a file containing `dots:` directives is a lint error, because the app would see raw directives.
- Each rendered file starts with a header comment (`generated by dots from packages/ghostty/config/common; settings: font_size=15(machine), theme=dark(default)`) where the format allows comments. Debugging then starts from the file itself.

### 17.4 Structured patches (when you'd rather not touch the file)

For structured formats, settings can be applied as **key-path assignments** declared in TOML, so the source file needs no directives at all:

```toml
[[files]]
dst = "~/.claude/settings.json"
mode = "render"
format = "jsonc"
layers = ["settings.json"]

[[files.set]]
path  = "statusLine.command"
value = "@dots(path.home)@/.claude/statusline-command.sh"

[[files.set]]
path  = "permissions.defaultMode"
value = "@dots(setting.permission_mode)@"
when  = 'setting.permission_mode != "default"'

[[files.set]]
path  = "env.DISABLE_TELEMETRY"
delete = true
when   = 'not(setting.telemetry)'
```

Patches apply after the merge, in declaration order. The type of `value` is inferred from the setting's type. A path that crosses a non-table is an error.

**Choosing between them:** use layers for per-platform differences in bulk, patches for a few keys in structured files, and directives for text formats or for when the conditional content is easiest to read in place.

---

## 18. Overrides: everything is customizable

**Principle: anything a repo defines, a consumer can change without editing it.** That covers your own packages, packages copied from someone else (§13) and whole forked setups (§13.2). Settings (§17) are the knobs an author *anticipated*. Overrides are for everything else. Keeping changes out of the original definitions is what makes `origin update` and `fork update` merge cleanly, and it lets machine-local tweaks stay uncommitted.

### 18.1 Every object has an address

| Kind | Address | Defined in |
|------|---------|-----------|
| package | `pkg:<name>` (or `<alias>:<name>` for an upstream item) | `packages/<name>/package.toml` |
| file entry | `pkg:<name>/file:<dst>` | the package |
| setting (schema) | `pkg:<name>/setting:<key>` | the package |
| provider section | `pkg:<name>/provider:<id>` | the package |
| manual action | `pkg:<name>/manual:<id>` | `manual/<id>.md` |
| hook | `pkg:<name>/hook:<event>` | the package |
| bundle | `bundle:<name>` | `bundles/<name>.toml` |
| bundle member | `bundle:<name>/member:<pkg-or-bundle>` | the bundle |
| provider options | `provider:<id>` (brew flags, apt sources/PPAs, cargo `--locked` default, npm prefix, custom provider templates) | `dots.toml` |
| priority | `priority:<platform-key>` | `dots.toml` |
| tags, paths, platforms, env | `repo:tags`, `repo:paths`, `repo:platforms`, `repo:env` | `dots.toml` |
| upstream | `upstream:<alias>` (`follow`, `import`, `rename`, `trust`) | `dots.toml` |

Every address can be inspected (`dots show <address>`) and overridden. `dots show <address> --effective` prints the final value with **each key annotated by the layer that set it**: upstream, definition, platform table, repo override, machine-profile override or machine override.

### 18.2 One grammar for every override

An override is a **partial copy of the object it targets**, and the same merge rules apply everywhere:

| In the override | Effect |
|-----------------|--------|
| scalar `x = v` | replace |
| table `[x]` | deep merge |
| array `x = [...]` | replace the whole list |
| `x = { add = [...], remove = [...] }` | edit the list (add goes at the end; `prepend` exists for ordered lists like `priority`) |
| keyed collection entry (files by dst, settings by key, manual by id, providers by id, bundle members by name) | merge into that entry; a new key adds an entry |
| `remove = true` inside a keyed entry | delete that entry |
| `[platform."<key>"]` inside the override | the change applies only on that platform (§4.1) |
| `when = "..."` | replaces the object's condition, e.g. to enable a mac-only package on Linux |

The only things you can't override are identity fields: `name`, `schema` and `origin`. To rename an imported item, use the upstream `rename` field.

### 18.3 Locations and precedence

```
overrides/dots.toml                                   # repo manifest overrides
overrides/packages/<pkg>/override.toml  (+ files)     # package overrides; relative paths resolve here
overrides/bundles/<bundle>.toml                       # bundle overrides
machines/<name>/overrides/...                         # committed, only on that machine profile
~/.config/dots/overrides/...                          # machine-local, never committed (same structure)
```

Layers are applied in this order, lowest first, to every object:

```
upstream chain (each upstream fully resolved) ─► this repo's definition ─► platform tables (general→specific)
  ─► overrides/ (repo) ─► machines/<name>/overrides/ ─► ~/.config/dots/overrides/ (machine)
```

Within each layer, that layer's own `[platform.*]` tables apply in chain order. **Setting values** (§17.2) are resolved after the definitions are final, so an override can change a setting's *default*, its allowed values or its `ask` flag, and the user's chosen *value* still wins on top.

### 18.4 Examples

Package (copied or forked, it doesn't matter):

```toml
# overrides/packages/ghostty/override.toml
schema = 1
prefer  = ["github-release", "brew"]
version = "1.2"                                   # stay on the 1.2 line
requires = { add = ["nerd-fonts-extra"] }

[settings.font_size]
default = 15                                      # change the default; machines can still set their own value
[settings.theme]
values = { add = ["solarized"] }                  # extend an enum; your extra layer handles it with dots:if

[providers.script]
remove = true

[files."~/.config/ghostty/config"]
append_layers = ["config"]                        # → overrides/packages/ghostty/config (may contain dots:if blocks)

[[files.add]]
dst = "~/.config/ghostty/themes/mine"
src = "themes/mine"

[manual.disable-secure-input]
remove = true

[platform."ubuntu-22.04"]
prefer = ["script"]                               # only there
```

Bundle:

```toml
# overrides/bundles/cli.toml
schema = 1
packages = { add = ["zoxide", { name = "atuin", when = 'not(tag = "work")' }], remove = ["igrep"] }
include  = { remove = ["koala"] }

[members.bottom]
when = 'platform = "linux"'                       # keep it, but Linux only

[settings.ghostty]                                # bundle-scope setting values
font_size = 14
```

Repo manifest:

```toml
# overrides/dots.toml
[priority]
macos = { prepend = ["github-release"] }          # prefer release binaries over brew on macOS
[provider.brew]
cask_args = ["--no-quarantine"]
[tags]
gaming = "machines that get Steam etc."
```

### 18.5 Ejecting

When an override grows bigger than the thing it overrides, run `dots override eject <address>`. It writes the **effective** definition as a new local package or bundle (files included), records the original under `[origin]` and deletes the override. The item is then plainly yours. For upstream items, that is the same as `dots fork vendor` for one item.

### 18.6 Visibility and safety

- `dots status` tags customized items: `ghostty  ofir:  ✎ overridden (repo, machine)`. Local divergence is never invisible.
- `dots override list` shows every override grouped by scope, with its target address and a one-line summary of what it changes.
- `dots override edit <address> [--machine]` scaffolds the right file and opens `$EDITOR`. `dots override diff <address>` shows pristine vs effective, structurally.
- `dots adopt` (§6.3) can target an override layer: "put the app's edits into my machine override".
- Lint: an override whose target no longer exists is an **error** ("stale override", typically after `origin update` or `fork update`), and the hint names the closest existing address. A redundant override (same value as underneath) is a **warning**. Type changes are errors.

---

## 19. Agent skill

`dots` ships an agent skill (`skill/dots/SKILL.md`, installable with `dots skill install` into `~/.claude/skills/dots/` and other agent locations). It teaches an agent what the tool does, what the user controls, how to run onboarding with recommended defaults, and how to debug. The draft is in `skill/dots/SKILL.md` in this repo.

The architecture requirements that follow from the skill:

- Every read command has a stable machine-readable form, so an agent never scrapes tables.
- `dots questions` lists unanswered `ask`/`required` settings, unselected recommended bundles and tag choices, each with its recommended answer. Onboarding is data-driven, not hard-coded into the skill.
- `dots.toml` can mark bundles `recommended = true` and tags with `recommended = true|false` per platform. That is what "accept all defaults" means mechanically.
- `config explain`, `why`, `show --effective`, `plan` and `doctor` exist so that every "why is X like this" question has a command that answers it with provenance.

---

## 20. Non-goals (v1)

- **Secrets.** No vault. Templates can call `{{ cmd("op read ...") }}` later behind an explicit `allow_exec_in_templates`. Untracked local overlays (`*.local.toml` layers, gitignored) cover `~/.git_user`-style data for now.
- **System configuration beyond packages and files**: `defaults write`, systemd units and launchd agents. They fit later as **typed resources** with check/apply semantics, so `setup_once.sh` becomes declarative. For v1 they are script packages with checks.
- **Remote orchestration** (applying to N machines over SSH). That is what ansible is for.
- **Guaranteed rollback** of system package versions. The lock is honest about this (`Limited`).
- **Nix-level reproducibility.** This tool is pragmatic convergence on whatever package managers the OS already has.

---

## 21. Open questions (decide before implementing)

1. **Should `apply` auto-commit lock changes?** Proposal: never by default. `--commit` is opt-in. A dirty lock shows in `status`.
2. **Committed machine profiles (`machines/*.toml`)**: are they worth it, or is local-only `machine.toml` enough? Proposal: support both, with the local file overriding the committed one.
3. **Interactive TUI (`dots ui`, ratatui)** for status/todo/plan: v1 or later? Proposal: later. Ship `--json` first, so the TUI and the statusbar both consume the same data.
