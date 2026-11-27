# Providers

A provider is the one place dots knows how to talk to a package manager. The rest of dots
(planning, status, apply) only sees the `Provider` trait in `crates/dots-providers`. Error codes
are in [errors.md](errors.md).

## The provider contract

Every provider exposes these operations:

| Operation | What it does |
|-----------|--------------|
| `availability` | `Ready { bin_path }`, `Bootstrap { pkg }` (usable after installing `pkg`) or `Unsupported`. The provider finds its own binary in known locations, never through `PATH`. `availability_in` is the same with the execution context. |
| `installed` | ONE batched query returning name, version, pinned and source for everything installed. |
| `latest` | Batched and cacheable: the latest available version per package. |
| `plan` | Pure. Turns `Install`, `Upgrade`, `Remove`, `Pin` and `Unpin` ops into the exact commands (`Action.argv`). `dots plan -v` shows these. It runs nothing and writes nothing. |
| `exec` | Runs exactly `Action.argv`, writes the output to `<log_dir>/<pkg>.<action>.log` and returns the log path. |
| `bins` | The executables each installed package put on disk. Batched. |
| `owner_of` | Optional reverse lookup from a path to the owning package. |

All commands run through `Ctx::runner`, so tests can script outputs and count calls.

### Capabilities

Each provider declares `Caps`:

| Capability | Meaning |
|------------|---------|
| `batch_install` | One action may install several packages. When `false`, `plan` returns one action per op. |
| `exact_version` | `Yes`, `Limited` or `No`. With `No`, planning an install with a version is an error; a provider never silently ignores a version. |
| `pin` | Supports `Pin` and `Unpin`. |
| `needs_root` | Every action is marked `needs_root` (and needs `sudo`). |
| `uninstall` | Supports `Remove`. When `false`, the plan says "cannot uninstall". |
| `exclusive_lock` | Never run two batches of this provider at the same time. |

Behavior that depends on a capability is visible to the user.

### Sudo steps

A step is a *sudo step* when `dots apply` must not run it without being asked: a provider action marked `needs_root` (every `apt` action; a script with `needs_root = true`; a uv `break_system_packages` install into a system directory the user cannot write), a root system setting (`root = true`), or a package that declares

```toml
requires_sudo = true              # the whole package, whatever provider installs it

[providers.brew]
cask = true
requires_sudo = true              # or on one provider block only
```

Use `requires_sudo` for a step that is not run as root by `dots` but still needs the admin password: a brew cask with a `.pkg` installer, a kernel extension. A plain `dots apply` skips sudo steps (and the steps that depend on them); `--with-sudo` runs everything; `--sudo-only` runs only the sudo steps. A package's hooks follow its install step: when the install is skipped so are its hooks. `dots lint` warns when `requires_sudo` is set on a provider that is always root anyway (`apt`, a `needs_root` script). The step's batch never mixes sudo and ordinary packages of one provider, so a cask that needs the password does not hold back the formulae next to it.

A provider may also implement `stale_reason(ctx, spec)`: for an installed package the version rules leave alone, it returns why the package must be upgraded anyway (apt: its declared `repo` is not configured yet, so it still comes from the distribution archive). The plan shows the reason and upgrades the package; the default is no reason. It must read local state only.

### Conformance kit

`dots-testkit` ships the conformance suite every provider must pass: batched `installed`, pure
`plan`, honored capabilities, no `PATH` dependence, `exec` running exactly `Action.argv` with a
log, and an install round trip.

```rust
use std::sync::Arc;
use dots_testkit::{ConformanceFixture, conformance_test};

conformance_test!(my_provider_conforms, {
    let provider = MyProvider::new();
    let fixture = ConformanceFixture::new("installed-pkg", "missing-pkg")
        .exact_version("1.2.3")
        .backend(Arc::new(MyFakeBackend::default())); // or Arc::new(RealRunner)
    (provider, fixture)
});
```

## The `script` provider

The escape hatch, kept honest. One provider instance exists per package, built from its
`[providers.script]` table:

```toml
[providers.script]
check     = "test -x ~/.local/bin/kv"      # REQUIRED: exit 0 = installed
version   = "kv --version | awk '{print $2}'"  # REQUIRED unless versionless = true
latest    = "git ls-remote https://github.com/KoalaVim/kv HEAD | cut -c1-12"
install   = "scripts/install.sh"           # REQUIRED
upgrade   = "scripts/install.sh"           # default: install
uninstall = "scripts/uninstall.sh"         # absent: the plan says "cannot uninstall"
shell     = "bash"                         # bash | sh | pwsh
needs_root = false
interactive = false                        # true: stdin attached, one at a time
timeout   = "20m"
lock      = "pi"                           # optional: same key = never run at the same time
```

`check`, `version`, `latest`, `install`, `upgrade`, `uninstall` and package hooks are shell text, or
a file: a value that is one token naming an existing file in the package dir (`install =
"install.sh"`) runs that file with the script's shell (`bash ./install.sh`), cwd = the package dir.
`lock` serializes script packages that write shared state: packages with the same key never run
concurrently.

After `install` (or `upgrade`) exits 0, `check` must pass. If it does not, the action fails with
`check-failed-after-install` ("install succeeded but check fails") and the log is linked. A script
that runs longer than `timeout` (default 20 minutes) is killed together with every process it
started and fails with `timeout`.

### Script runtime environment

Every script dots runs (install, upgrade, uninstall, check, version, latest) runs with:

- the package dir as the working directory;
- stdin set to `/dev/null`, unless `interactive` (a script that reads stdin sees end-of-file);
- `PATH` set to the `[env] path` directories of every active package, then the inherited `PATH`, plus provider bins (so `check` and `version` see tools in `~/.local/bin`, `~/.bun/bin`, ...);
- all output in `logs/<run-id>/<pkg>.<action>.log`;
- these variables:

| Variable | Value |
|----------|-------|
| `DOTS_OS` | `macos`, `linux` or `windows` |
| `DOTS_ARCH` | `x86_64` or `aarch64` |
| `DOTS_DISTRO` | distro id, empty when none |
| `DOTS_WSL` | `1` under WSL, else `0` |
| `DOTS_SUDO` | `1` when sudo is usable, else `0` |
| `DOTS_TAGS` | the machine's tags, space-separated |
| `DOTS_TAG_<NAME>` | `1` for each tag; the name is upper-cased, other characters become `_` |
| `DOTS_PKG` | the package name |
| `DOTS_PKG_DIR` | the package directory |
| `DOTS_ROOT` | the active repo root |
| `DOTS_STATE` | the dots state directory |
| `DOTS_BIN` | where dots puts executables, `~/.local/bin` |
| `DOTS_LIB` | shared shell helpers |
| `DOTS_VERSION_WANTED` | the requested version, empty for latest |
| `DOTS_DRY_RUN` | `1` for a dry run, else `0` |
| `DOTS_SETTING_<NAME>` | one per resolved setting; `font_size = 15` gives `DOTS_SETTING_FONT_SIZE=15` |

For bash, `set -euo pipefail` is prepended to each command.

A script with `needs_root = true` runs through `sudo -n`, whose default `env_reset` would drop `HOME` (it becomes `/root`), the `DOTS_*` variables (settings and secrets included) and the `[env]` `PATH` directories. So `dots` writes `HOME`, `PATH` and the `DOTS_*` variables to a file in a private temporary directory (mode 0600, directory 0700), and the script starts by sourcing it (`set -a; . <file>; set +a`). Nothing but the file path appears in the command line, so no other user can read your secrets with `ps`. The file is deleted when the action ends, however it ends. This also works when `dots` is already root. Root scripts need the `bash` or `sh` shell; a `needs_root` script with `shell = "pwsh"` is refused with an error.

### Binaries of a script package

`dots bins` and `dots which` attribute a script package's binaries without a `bins` list when
they live in one of the package's own `[env] path` directories and carry the package's declared
`bin` (or an entry of `bins`; the package name when none is declared). `~/.fzf/bin/fzf` belongs
to the `fzf` package when it declares `[env] path = ["~/.fzf/bin"]`. A binary outside those
directories, or one the package does not declare, still shows as an unknown owner.

### Switching providers

When a package's provider changes (the repo now declares `script` where dots installed it with
`apt`), `dots apply` installs with the new provider first, then removes the copy `state.json`
says dots installed with the old one. The plan shows `~ fzf apt -> script` and lists the
removal command. A copy dots did not install (adopted), a native package another declared
package still uses, a provider that cannot uninstall, and a removal that would also remove other
packages are all left in place and reported. The `dots.lock` row of the old provider is replaced
by the new provider's. A copy left behind (the old provider is no longer listed by the package, or
`apply` kept it) is removed explicitly with `dots migrate <pkg> --from <provider> --to <current>`;
`dots which` / `dots bins` print exactly that command on the duplicate copy. It asks first
(`--yes` or `--no-input` skips the question) and refuses when removing it would take other
packages along. Without `--from`, migrating to the provider a package already uses reports
"Nothing to migrate" instead of pretending to move anything. If the new install fails, the old copy is untouched. Plan JSON carries a
`replaces` object on the step; apply JSON lists `removals`.

## Provider options worth knowing

- **cargo `locked`.** `cargo install` runs with `--locked` by default. Set `[providers.cargo] locked = false` for an old crate whose lockfile no longer builds with the current rustc. If a `--locked` install fails with a compile error, the failure message suggests it.
- **uv `kind = "lib", python = "system"`.** On an externally managed python (PEP 668, e.g. Ubuntu 24.04, detected by the `EXTERNALLY-MANAGED` marker in the stdlib dir) the install fails with a message suggesting `kind = "venv"`. To install into the system python anyway, opt in with `break_system_packages = true`: `dots` passes `--break-system-packages` and uses sudo only when the target directory is not writable.

## User-defined providers

Declare a provider in `dots.toml` to support a package manager dots does not know (go, gem, pipx,
nix-env) without writing Rust:

```toml
[providers.go]
available = "command -v go"
bootstrap = "go"                                        # package that provides it
installed = { cmd = "ls ~/go/bin", parse = "lines" }
version   = "go version -m ~/go/bin/{bin} | awk '/^\\tmod/{print $3}'"
install   = "go install {source}@{version|latest}"
batch     = false
```

| Key | Meaning |
|-----|---------|
| `available` | Command that succeeds when the backend is usable. Its first output line, when an absolute path, is the provider binary. It runs at plan time (once per run) with the `[env] path` directories of the active packages on `PATH`. |
| `bootstrap` | Package that provides the backend. When `available` fails, availability becomes `Bootstrap(<pkg>)` (the packages using the provider are planned after it, and the provider's `installed` is not run until then); without it, `Unsupported`. |
| `installed` | A command plus a parse mode (`cmd`, `parse`); a plain string means `lines`. Run once per query. |
| `version` | Command printing one package's version, for listing entries that carry none. |
| `install` | The install command template. |
| `batch` | `true`: one action installs all ops; `install` must contain `{names}`. |

Parse modes for `installed`:

- `lines`: one package per line, `name` or `name version`;
- `json:<path>`: a dotted path to an array (of names, or objects with `name` and `version`) or to
  an object (name to version string, or to an object with `version`); `.` is the root;
- `regex:<re>`: applied to each line, with named groups `name` and `version` (or groups 1 and 2).

Placeholders are filled from the spec and the facts: `{name}`, `{source}` (the package's
`source`, or its name), `{version}`, `{version|latest}` (a default when no version is requested;
a bare `{version}` without one is an error), `{bin}`, `{names}` (batch only), `{os}`, `{arch}`,
`{distro}`, `{home}`, `{user}` and `{hostname}`. Other braces, such as an awk body, are left alone.

A plain command line becomes the action's argv; one using shell syntax runs as
`/bin/sh -c <line>`. For the example above, a package declaring
`[providers.go] source = "github.com/x/y"` plans `go install github.com/x/y@latest`. Custom
providers have no `uninstall`, no pin and no `latest`; the plan says "cannot uninstall".
