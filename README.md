# dotrobot

A dotfiles manager written in Rust: packages, bundles, config files, versions and manual steps, defined in TOML and reconciled across macOS, Linux and Windows.

> Status: **design phase.** No code yet, just the architecture.

`dots` works like a reconciler. Your repo says what the machine should look like. `dots` compares that with what is actually installed and on disk, shows a plan, applies it, and records every action against the repo commit it came from.

## Highlights

- **Packages and bundles.** A package is one directory holding its install rules, config files, settings and manual steps. A bundle is a list of packages.
- **Providers.** brew, apt, cargo, uv, npm, git, GitHub releases, winget/scoop and scripts, plus user-defined ones. A package defines its name once and overrides it only where a provider differs.
- **Platform hierarchy.** Rules can target a whole family (`linux › debian › ubuntu`) or one release (`ubuntu-22.04`), and the most specific one wins. Architecture and WSL are handled too.
- **Config files.** Each file is linked, rendered from layers, injected as a managed block, or assembled from fragments several packages contribute. Settings are typed and can drive `dots:if` blocks inside config files.
- **Everything is overridable.** Any package, bundle, file, setting or provider option can be changed without editing the original. Overrides can apply to every machine or to one.
- **Reuse other setups.** `dots add` copies someone's packages and remembers where they came from, so you can merge their updates later. `dots fork` follows someone's setup at a pinned commit, and you keep only your overrides.
- **Versions.** Five forms: `latest`, `1.2.3`, `1.2`, `>=1.2`, `git:<ref>`. Constraints from different places are intersected, conflicts are explained, and a per-platform lock file records what was installed.
- **Visibility.** `status`, `outdated`, `plan`, `why`, `bins`/`which` (what's on PATH and who installed it), and `log` (repo history plus a per-machine journal with commit SHAs).
- **Manual actions.** Markdown steps tracked as per-machine todos, with optional self-checks.
- **Worktrees.** `dots try` / `dots use` switch the whole machine to a git worktree in one atomic step, and switching back cleans up what the trial installed.
- **Agent skill.** It teaches coding agents to set up a machine (recommended defaults first), configure it and debug it.

## Docs

- [ARCHITECTURE.md](ARCHITECTURE.md): concepts, schemas, providers, files, versions, overrides, forks, worktrees, history, engine, crate layout, migration from dotbot
- [UX.md](UX.md): terminal output design: capability tiers, visual language, screen mockups, errors, accessibility
- [skill/dots/SKILL.md](skill/dots/SKILL.md): agent skill

## Name

**dotrobot** is named after [dotbot](https://github.com/anishathalye/dotbot). It has run my [dotfiles](https://github.com/ofirgall/dotfiles) since their very first commit in June 2021, more than five years of `install.conf.yaml`. Thanks, dotbot. The CLI is called `dots`.
