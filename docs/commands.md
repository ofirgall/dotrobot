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
