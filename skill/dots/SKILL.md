---
name: dots
description: Install, configure, inspect and debug a user's machine setup with the `dots` dotfiles manager (packages, bundles, config files, settings, overrides, manual steps, versions, worktree experiments). Use when the user wants to set up a new machine, add/remove/upgrade tools, change a tool's config, understand why something is installed or configured a certain way, or fix a broken dotfiles setup.
---

# dots: a guide for agents

`dots` is a reconciler. The user's **dotfiles repo** says what a machine should look like. `dots` looks at what the machine actually has, shows the difference, and makes the machine match the repo. Nothing in it is "run a script and hope": every change is planned first, then applied, and then recorded.

Your job: help the user get the machine they want with as few questions as possible, keep their changes in the right place, and explain *why* things are the way they are when something looks wrong.

## The mental model (learn this first)

| Thing | What it is | Where it lives |
|-------|-----------|----------------|
| **Package** | One tool and everything it owns: how to install it on each OS/package manager, its config files, its settings, its manual steps. A package is a directory. | `packages/<name>/` in the repo |
| **Bundle** | A named list of packages and/or other bundles. Machines select bundles. | `bundles/<name>.toml` |
| **Provider** | A package manager `dots` drives: brew, apt, cargo, uv, npm, git, GitHub releases, winget/scoop, or a package's own script. `dots` picks the best one for this OS automatically. | built in, plus custom ones in `dots.toml` |
| **Facts** | Detected about the machine: OS, arch, distro, WSL, sudo available, GUI. | detected, read-only |
| **Tags** | Declared by the user per machine: `work`, `remote`, `gui`, ... They switch packages and config on and off. | machine profile |
| **Settings** | Typed knobs a package exposes (font size, theme, email). Each has a recommended default. | defaults in the package. Values can be set per repo or per machine. |
| **Platform** | The machine's chain from general to specific, e.g. linux › debian › ubuntu › ubuntu-24.04 (plus arch, plus wsl). Anything can have per-platform sections, and the most specific one wins, so "ubuntu" covers every Ubuntu and "ubuntu-22.04" fixes just that release. | detected |
| **Overrides** | The user's changes to *anything* (package, bundle, file, setting default, manual step, provider option, priority) without editing the original. One merge grammar everywhere. | `overrides/` (all machines), a committed machine profile's overrides, or the machine-local overrides dir |
| **Followed source** | Someone else's repo whose bundles or packages were added. It stays pinned at a revision, the user keeps only overrides, and the other person's improvements come in on pull. | declared in the repo manifest, pinned in the lock |
| **Manual actions** | Steps a human must do (grant macOS permissions, log out). Tracked as a per-machine todo list. Some verify themselves. | `packages/<pkg>/manual/*.md` |
| **Lock** | The exact versions and git revisions that were installed, per platform. | `dots.lock` in the repo |
| **Journal** | Everything `dots` did on this machine, with timestamps and the repo commit it was running from. | local state |
| **Active root** | The checkout (main or a git worktree) the machine currently follows. All links go through it, so switching between worktrees is instant. | local state |

Config files are either **linked** (the file in the repo *is* the config), **rendered** (`dots` builds the file from layers, settings and conditions and writes the result outside the repo), **injected** (`dots` manages one marked block inside a file it does not own, like `~/.zshrc`) or **fragments** (several packages contribute parts of one file, like `~/.gitconfig`).

## What the user controls, from most to least common

1. **Which bundles this machine gets** (e.g. base + cli + mac-desktop).
2. **Tags for this machine** (work/personal, remote/headless, gui).
3. **Settings** of individual packages, set either for this machine only or for all their machines.
4. **Excluding** a single package on this machine.
5. **Versions**: pin a package to a version, upgrade one or all of them, see what is outdated.
6. **Overrides** for anything a setting doesn't cover.
7. **Manual todos**: mark done, skip, or let `dots` auto-verify.
8. **Experiments**: try changes in a git worktree, switch the machine to it, then come back or keep it.
9. **Reusing someone else's setup**, by **adding** their bundle or package: either *followed* (the default: pinned at a revision, the user overrides whatever they like) or *copied* (the files become the user's own), then pulling their updates later.
10. **Platform specifics**: general rules for a family (all Ubuntu) and narrower fixes for one release or architecture.

**Where should a change go?** Decide this before editing anything:

- "Only on this machine" → a machine-scope setting, a machine override or an exclude. Nothing gets committed.
- "On all my machines" → a repo-scope setting or a repo override, or edit the package if it's the user's own package.
- "This package came from another repo" (copied, it has an origin) or "it's from a followed source" → settings and overrides only, never edit the original, so future updates merge cleanly. If the overrides grow bigger than the thing itself, **eject** it into a plain local package.
- "Only on Ubuntu 22.04 / only on ARM / only under WSL" → a platform section at the *least specific level that's true* (prefer "ubuntu" over "ubuntu-22.04" unless it really is release-specific).
- "Only on macOS / only on Linux / only at work" → a condition (OS fact or tag), not a separate copy of the file.
- Never edit rendered output files directly. They are regenerated. Change the source layer, the setting or the override instead. If an app has already edited a rendered file, use **adopt** to pull those edits back into a layer.

## Onboarding a machine (install interview)

**Assume the user wants to skip everything.** Open with one question:

> "I can set this machine up with the recommended defaults right away, or walk you through the choices (bundles, a few settings). Defaults?"

If they say defaults, or don't care: use the recommended answers from `dots questions` for everything. Ask **only** for `required` settings that have no default (typically identity: name, git email). Then show the plan summary and apply.

If they want to choose, ask in this order. Stop as soon as they say "rest default". Show the recommended answer with every question.

1. **What is this machine?** Personal / work / server (headless, SSH-only). → sets tags such as `work` and `remote`, and decides whether GUI bundles apply. Usually inferable: a Mac with a display is personal or work with GUI. Ask only personal vs work.
2. **Which bundles?** Present the recommended set for this OS, with one-line descriptions. Then offer the optional ones (gaming, extra WMs, editors). Don't list individual packages unless asked.
3. **Anything to leave out?** Only if they show interest. Becomes machine excludes.
4. **Required settings** (cannot be skipped): ask each one, using the package's own question text.
5. **"ask" settings** (have defaults but are worth a glance: theme, font size, ...): group them into one message with defaults pre-filled, e.g. "theme: dark, font size: 13, cursor shader: off. Change anything?"
6. **Sudo**: if the plan includes system package managers (apt), tell them they'll be asked for their password once. If they don't have sudo, say that `dots` will automatically prefer user-level installs (cargo, uv, GitHub releases).
7. **Interactive steps and manual todos**: tell them these are deferred to the end and tracked. They can do them later.

Then:

- Show the **plan**: what gets installed, by which package manager, which files get linked or written, what will be backed up. Summarize it; don't dump it unless asked. Point out anything destructive: existing files that will be moved to a backup, uninstalls.
- **Apply.** It runs installs in parallel, asks for sudo once, and defers interactive steps.
- Report: what succeeded, what failed (with the relevant log tail), pending manual todos, and whether a logout or reboot is needed.
- Walk through the manual todos **only if the user wants to now**. Never mark a todo done yourself unless its self-check passes or the user confirms they did it.

## Day-to-day tasks

- **Add a tool:** check whether a package already exists, in this repo or in a repo they copy from. If it does, add it to the right bundle. If not, create a package: name, and per-provider names only where they differ (e.g. `fd` is `fd-find` on apt). For install-by-script tools, a **check** command is mandatory. Then plan, then apply.
- **Change a tool's config:** first check whether the package has a setting for it. Settings beat file edits. If there isn't one: is the change meant for all machines (edit the source layer, or a repo override for copied packages) or this machine only (machine override)? For platform-specific changes, use a condition (`dots:if os = "macos"` blocks in the file, or a platform layer) rather than a second copy.
- **Upgrade:** show what's outdated first. Upgrade what the user picks. The lock gets updated, and committing the lock is the user's call: ask.
- **Pin a version:** pin it in `dots` (which also holds it in brew/apt), not by hand in the package manager.
- **Remove a tool:** `dots` uninstalls it, removes the links and rendered files it owns, and takes its fragments out of shared files. Confirm with the user first.
- **Try something risky:** create a trial worktree, switch the machine to it, and experiment. Going back offers to remove, keep or adopt anything that was installed during the trial. Suggest this for any big change (new WM, new shell setup).
- **Reuse someone's setup, follow or copy?** Always say which kind you're adding (a bundle or a single package). Ask the user one question: *"Do you want to follow their setup and keep only your tweaks on top, or own a copy and change it freely?"* Recommend **follow** (the default) when they want a whole bundle or setup and expect to take the other person's updates. Recommend **copy** for a single package they plan to reshape. Either way, show what comes along (dependencies) and **every script that will run** before confirming.
- **Pull their updates:** show what changed upstream, scripts first. Any stale override (one pointing at something upstream removed or renamed) must be fixed before the update lands. Explain each one to the user and propose the fix.

## Debugging playbook

Start broad, then narrow. Always look before changing.

1. **Status**: the health of everything: packages (ok / missing / outdated / drift / orphan / duplicate), files (ok / missing / conflict / modified / stale / dangling / foreign entries), todos, git sources, the active root, and whether the repo is dirty.
2. **Doctor**: environment problems: a provider not found, PATH ordering and shadowing, an active root that points nowhere, a stale lock, symlinks unavailable on Windows, sudo.
3. **Facts**: what `dots` thinks this machine is. Many "why isn't X installed" answers are a wrong fact or a missing tag.

Then match the symptom:

| Symptom | Ask `dots` | Usual cause → fix |
|---------|-----------|-------------------|
| "Why is X installed / not installed here?" | **why X** (inclusion path through bundles and dependencies), **show X effective** | Package condition false on this OS/tag, excluded on this machine, or bundle not selected |
| "Wrong package manager installed X" | **show X**: the chosen provider and the reason for the choice | Priority order, no sudo, or a sticky earlier choice → **migrate** it to another provider explicitly |
| "Command runs the wrong binary" | **which `<bin>`** | Two providers installed it, or PATH order → migrate/remove the duplicate; check that the generated env file is sourced |
| "What's on my PATH / where did this come from?" | **bins** (all executables → package, package manager, version; shadowed, unmanaged and unknown ones marked) | Unmanaged = installed outside dots → offer to add it as a package (adopt) or remove it; unknown = nobody owns it, ask the user |
| "Why does this package/bundle/file behave like this?" | **show** it with its effective view: every field labeled with the layer that set it (upstream, definition, platform section, repo override, machine override) | A more specific platform section or a higher override layer wins; fix it at the layer the user intends |
| "Override doesn't apply" / "stale override" after an update | **override list**, lint | The target was renamed or removed upstream; re-point the override at the new address |
| "Wrong thing happens only on one distro/release" | **facts** (look at the platform chain) | An unexpected chain (derivative distro, WSL) → add a platform section for that key, or declare the distro's parent in the manifest |
| "Config value isn't what I set" | **config explain `<pkg>.<setting>`**, then the header comment at the top of the rendered file | A higher scope (machine, or a bundle) overrides it; or the file is stale → apply |
| "My edit to the config file disappeared" | status shows the file as `modified` or `stale` | They edited a rendered file → **adopt** the edit into a layer or override, then apply |
| "File says conflict" | plan | A real file exists where `dots` wants a link. Apply moves it to a backup; tell the user where. |
| A skill or script shows as tampered | status, **verify** | Something edited an integrity-enforced file → apply restores it. If the user made the edit on purpose, turn it into an override or a patch instead |
| "Links are broken everywhere" | status (dangling), roots | The active worktree was deleted → switch the machine back to main |
| "Install failed" | the apply summary's log tail; the full per-action log; **log X** for history | The script's check still fails after install (the script is broken), a network error, or a missing build dependency (add it to `requires`) |
| "Hangs during apply" | | A script waiting on stdin. It must be marked interactive, or fixed to run unattended. |
| "Directory in my repo fills up with junk" | status: foreign entries | The app writes runtime files into a linked dir → switch to linking individual files |
| "Version differs from lock" (drift) | **outdated**, **log X** | brew/apt can't install exact old versions; either accept it or pin. Git sources: check dirty/ahead |
| "What changed recently?" | **log** (optionally for one package) | Repo history + machine journal, combined, with commit SHAs and worktree names |
| "Rendered file looks wrong" | **plan** shows a diff of the render; inspect `dots:if` blocks and `@dots(...)@` values in the source layers | A condition uses the wrong setting/tag; an unknown name (dots errors on these, read the error's file:line) |

Rules while debugging:

- **Never** delete or hand-edit files under the `dots` state directory, or symlinks `dots` owns. Use apply, use (switch root), restore (backups) or remove.
- **Never** run `brew upgrade`, `apt upgrade` or `cargo install` directly to fix a `dots`-managed package. `dots` then loses track and reports drift. Go through `dots`.
- Before any change, show the **plan** and get the user's OK for anything that uninstalls, overwrites, or needs sudo. File-only changes that are fully reversible (backups exist) can proceed once the user has asked for them.
- When reporting failures, quote the actual error and the log location. Don't paraphrase into something softer.
- When the repo is dirty or the machine is on a trial worktree, say so before applying. The user may not realise which checkout the machine follows.

## Authoring cheatsheet (when editing the repo for the user)

- A package is one dir: `package.toml`, plus config files, `manual/*.md` and `scripts/`. It must be self-contained: no references outside its dir, or it can't be copied to another repo.
- Per-provider sections only where something differs: the package name, a binary name, whether it's a cask, cargo `--locked`, a git URL.
- Install-by-script packages need **check** (is it installed?) and **version**. Without them, status can't tell the truth, and `dots` treats "install succeeded but check fails" as a failure.
- Settings: always give a recommended default. Mark one `ask` only if most users would plausibly want a different value. Mark it `required` only when no default can exist.
- Conditional config, in order of preference:
  1. a platform/tag layer (whole chunks differ),
  2. structured key patches in `package.toml` (a few keys in JSON/TOML/YAML),
  3. `dots:if` / `dots:elif` / `dots:else` / `dots:end` comment blocks and `@dots(setting.x)@` values inside the file (text formats, or when it's clearest in place). They use the file's own comment character, so the file stays valid.
- Manual actions: one markdown file per action, with a title, a condition, and a self-check where one is possible. Bump its revision when the steps change, so every machine asks again.
- Run **lint** after editing. It catches typos in tags, settings and facts, colliding fragments, unbalanced `dots:if` blocks and missing checks.
