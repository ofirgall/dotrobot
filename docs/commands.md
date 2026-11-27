# Commands

Short reference for the commands implemented so far. Every command accepts the global flags
(`--json`, `--color`, `--quiet`, `--home`, `--machine-config`, ...); with `--json` stdout holds
exactly one document, described by `schemas/dots-<command>.schema.json`. Error codes are in
[errors.md](errors.md).

The repo is found from `root` in `machine.toml` (default `~/dotfiles`).

## `dots lint`

Check the repo without changing anything.

```
dots lint
```

v0.1 checks:

- schemas (unknown keys, wrong types, unsupported schema, name mismatches, escaping paths);
- conditions: syntax, unknown facts (`unknown-fact`), undeclared tags (`unknown-tag`) and
  unknown platform keys (`unknown-platform`), each with a "did you mean" suggestion. This covers
  `when` on packages, requirements, providers, bundle members, files, layers and settings, plus
  the keys of `[priority]` and the parents in `[platforms]`;
- tags in `machine.toml` that `dots.toml` does not declare;
- bundles, includes and requirements that name something that does not exist
  (`unknown-package`, `unknown-bundle`), and cycles in `include` or `requires`
  (`dependency-cycle`);
- warnings: `//` repo-root paths (`escape-hatch-path`) and script providers without a `check`
  (`script-no-check`).

Fragment collisions, directives, links to directive files and stale or redundant overrides are
checked once those features exist.

Exit codes: `0` when there are no errors (warnings are fine), `1` when there is at least one
error.

```
$ dots lint
error[unknown-tag]: unknown tag `wrok`
  at packages/tool/package.toml
  -> Declare it under `tags` in dots.toml; did you mean `work`?
lint: 1 error, 0 warnings
$ echo $?
1
```

`--json` data: `{ packages, bundles, errors, warnings }` (counts); the problems are in
`errors[]` and `warnings[]`.

## Progress while `plan` and `apply` work

Progress goes to stderr, so stdout (and `--json`) stay clean; `--quiet` drops it.

- `dots plan` and the planning part of `dots apply`/`dots upgrade` show one transient line on a terminal while they ask providers what is installed and what is the latest version (`Checking installed packages`, `Looking up latest versions`, `Running 12 script checks`). It is erased when the plan prints. Without a terminal nothing is printed.
- A running `dots apply` on a terminal shows each running step as a sentence with a spinner, the elapsed time and the last line of the tool's output (`Installing plantuml, deno, zsh (brew)  1m 04s  ==> Pouring deno...`), and an overall bar (`Applying  [█████░░░░░░░░░░░░░░░]  9/23 steps · 1 failed · 3m 12s`). Finished steps stay above it (`Installed plantuml, deno, zsh  43s`).
- Without a terminal (ssh without a tty, a pipe, CI) or with `DOTS_REDUCE_MOTION`, the same events are plain ASCII lines (`[..] Installing kv (cargo)`, `[x] Installed kv  3m 05s`), and a step that runs longer than 30 s adds `[..] still running: Installing kv (cargo) (1m 30s)` every 30 s. Set `DOTS_HEARTBEAT_SECS` to change the interval, or `0` to turn it off. The lines are also appended to the run log (`~/.local/state/dots/logs/<run-id>/run.log`).
- Ctrl-C clears the live region; the first press stops after the running steps, the second ends the process at once.

## `dots status`: what it promises

`status` asks the same question `apply` asks (`dots_engine::decide_package` for versions, the plan's sudo rule for root), so a row with a `dots apply <pkg>` hint is one that `apply` acts on:

- **Package drift** is a package step (install, upgrade, downgrade), or a lock row that `apply` rewrites without any step: `locked 2.104.0, installed 2.105.0; apply updates the lock`. That happens when the installed version is newer than the row and the provider cannot install an older one (brew, apt), and for packages built from a git or `path` source (their version comes from the source, so the row cannot be restored by reinstalling). A version installed outside dots is adopted by the next `apply`. A package outside its version spec that the provider cannot move (it cannot install an older version) is drift with no hint: edit the spec.
- **Sudo.** A package, provider entry or setting that needs root (`requires_sudo`, `needs_root`, `root = true`) is badged `needs sudo` in its note (`"sudo": "<reason>"` in `--json` items), with the hint `dots apply --with-sudo <pkg>` (a plain apply skips it; per-package selection works with the flag). The headline counts them: `3 things need attention (2 wait on sudo)`, and a group of them on the bare `dots` screen says `dots apply --sudo-only`. A root setting that cannot be read without a cached sudo credential is `unknown` (root's preferences are a different plist from the user's), also badged.
- **Settings** read a key (or a whole domain) nobody wrote as `unset`: it differs from the declared value, so it is `missing`, not `unknown`. `unknown` and `failed` are for reads that really could not happen (no sudo credential, a tool that is missing, a permission error).

## `dots apply`, `dots upgrade` and sudo steps

`dots apply` has three modes for the steps that need root (see "Sudo steps" in `providers.md`):

| Command | Sudo steps | Password |
|---|---|---|
| `dots apply` | skipped, with everything that depends on them; listed in a warning at the end | never asked |
| `dots apply --with-sudo` | run, with everything else | asked once at the start (if it fails, root steps fail with `sudo-failed`) |
| `dots apply --sudo-only` | the only steps run, with their `pre_install`/`post_install` hooks and root settings | asked once at the start |

`--with-sudo` and `--sudo-only` conflict. Skipped steps are not failures: if everything else succeeded the exit code is 0, and nothing skipped is recorded in `state.json` or `dots.lock`, so the next plain apply skips them and warns again. The warning:

```
[!] 5 steps need sudo and were not applied:
    + orbstack        brew          requires_sudo
    + locale          script        needs_root
    ~ com.apple.foo   defaults setting  root
    + herdr-thing     brew          depends on orbstack
  -> dots apply --sudo-only   (or dots apply --with-sudo for everything)
```

With `--sudo-only`, a sudo step whose ordinary dependency is not installed yet is skipped with the reason "run `dots apply` first"; the ordinary steps are not applied and one line gives their count. Files, ordinary settings and services are left for a plain `dots apply` (run it after `--sudo-only` to finish the sudo packages' files). `dots upgrade` takes the same `--with-sudo` / `--sudo-only` flags (they conflict) with the same meaning: a plain `dots upgrade` skips the packages whose step needs sudo (and their dependents), records nothing for them, warns with the list and `dots upgrade --sudo-only` / `--with-sudo` suggestions, and exits 0. When `dots` runs as root, sudo steps are ordinary: a plain apply runs them and prints no warning.

`--json` (`dots/apply@1`, and `dots/upgrade@1` for `dots upgrade`) carries `sudo: { mode: "skip"|"with"|"only", skipped: [{ name, kind, provider, reason }], not_applied_ordinary }`; skipped package steps appear in `results` with status `skipped-sudo` (or `skipped-due-to-dep`, `not-selected` for `--sudo-only`). `dots plan --json` marks each step with `sudo` (the reason, or null) and counts them in `summary.sudo_steps`; the plan's "Heads up" says that N steps need sudo and a plain apply skips them.

## `dots todo`: manual actions

Steps software cannot do for you (granting a permission, clicking an enable button, changing the
login shell) are one markdown file each, `packages/<pkg>/manual/<id>.md`:

```markdown
---
id: tailscale                 # optional, defaults to the file name; full id is <pkg>/<id>
title: Tailscale CLI with HTTPS certificates
when: 'os = "macos"'          # optional platform condition
after: always                 # install (default) | files | always
check: 'tailscale status --json | grep -q CertDomains'   # optional; exit 0 means done
requires: [logout]            # logout | reboot | restart-app:<name>; shown as badges
revision: 1                   # bump when the steps change: pending again on every machine
interactive: false            # needs a terminal (parsed and badged; apply does not walk them yet)
---

1. Install the CLI from the Tailscale app.
2. Enable HTTPS certificates at https://login.tailscale.com/admin/dns.
```

Unknown front matter keys are errors (`dots lint` reports `manual-invalid`, and
`manual-duplicate-id` for two files with one id; `when` is checked like every other condition).

```
dots todo                          # pending actions, with requires badges
dots todo show <pkg>/<id>          # the markdown, plain text
dots todo done <pkg>/<id> [--note <text>]
dots todo skip <pkg>/<id>          # not listed again until its revision increases
dots todo check                    # run the checks, close what passes
```

An id is `<pkg>/<id>` or the address `pkg:<pkg>/manual:<id>`.

- **When an action exists.** Its package is active, its `when` holds and, for `after: install`,
  the package is installed (or dots owns it). `after: files` and `after: always` need only the
  active package.
- **Per machine.** The record (`status` pending/done/skipped/auto, `revision`, `at`, `repo_rev`,
  `note`) is in `state.json` under `manual`, so doing it on the laptop says nothing about the
  desktop. A record counts only for the revision it was made against: raising `revision` reopens
  the action everywhere, including where it was skipped.
- **Checks** run in two places only: at the end of `dots apply` and on `dots todo check`, each
  through `/bin/sh -c` in the package directory with a 10 second timeout (a timeout is "not
  yet"). A passing check closes the action as `auto`. An `after: always` action closed by its
  check opens again when the check stops passing (login shell, group membership). `status`,
  `plan` and a bare `dots todo` only read the record, so they never run your commands.
- **Where they show.** `dots status` lists them under TODO (`dots status --all` includes the
  closed ones), `dots plan` adds "N manual actions pending, 1 requires logout" to Heads up, and
  `dots apply` ends with the same line (`data.manual` in `--json`).
- The logout / reboot requirements settings raise are listed by `dots todo` too, marked
  `[from a setting]`; they close themselves once the setting reads as declared.

Not done yet: `interactive` actions are not run one by one at the end of `apply`, `show` does
not render the markdown with termimad or offer to open links, and a hook cannot wait on a
manual action.

### Pending `post_install` hooks

`[hooks] post_install` runs when dots installs or upgrades the package. A package dots only
*adopted* (it was already installed) never had its setup run, so that hook is owed:

- `state.json` records `hooks_pending` for an adopted package that declares `post_install`, and
  for a `post_install` that was skipped (an earlier hook of the package failed). `hooks_failed`
  already covered a hook that ran and failed.
- `dots plan` lists the hook (`adopted, never ran` / `failed last run`), `dots apply` runs it and
  clears the entry, `dots status` shows `<pkg>:post_install` as pending.
- A package dots installed before this record existed has no entry and is assumed done: only
  adoptions and failures from now on are owed, so nothing re-runs after an upgrade of dots.
- The hook script is not hashed, so editing it does not run it again.

## `dots why <pkg>`

Explain why a package is (or is not) on this machine: every inclusion path as a tree from the
machine through its bundles and requirements, the chosen provider with the reasons for the
choice, and the version constraints with their sources.

```
dots why <pkg>
```

Exit codes: `0` when the package is known (including when it is inert, excluded or not
included), `1` when the package does not exist in the repo or resolution found an error
(cycle, collision, conflict, unknown name). A package that has no usable provider is a
warning here (`no-provider-satisfies`), because the provider section explains it.

```
$ dots why rust
rust: active
machine
└─ cli
   ├─ jless
   │  └─ requires rust >=1.80
   └─ kv
      └─ requires rust >=1.80

provider: cargo
  apt skipped: needs root
  cargo chosen: first usable in the priority list for `debian`

version constraints:
  >=1.80  from required by jless (packages/jless/package.toml)
  >=1.80  from required by kv (packages/kv/package.toml)
```

A path whose condition is false is shown marked `(inactive: `<when>` is false)`. A package
installed by a provider that is not its preferred one stays on it and `why` prints the
`dots migrate <pkg> --to <provider>` hint.

`--json` data: `package`, `status` (`active`, `inert`, `excluded`, `replaced`,
`not-included`), `paths[]` (each with `text`, `active`, `steps[]` and `conditions[]`),
`provider` (`chosen`, `ranking`, `candidates[]`, `reasons[]`, `hint`) and `constraints[]`.

Provider implementations are not part of this build yet, so every provider currently reports
"not implemented in this build".
