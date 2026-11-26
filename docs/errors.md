# Error codes

<!-- Generated from crates/dots-core/src/errors.rs. Do not edit; regenerate with `cargo run -p dots-core --example gen-errors-doc`. -->

Every error dots reports carries one of these stable codes (`errors[].code` in `--json`).
`{name}` marks a value filled in at report time.

## `not-implemented`

- Message: `{command}` is not implemented yet.
- Hint: This command is a placeholder in this build; see the roadmap in README.md.

## `usage-error`

- Message: {detail}
- Hint: Run `{help}` to see the accepted arguments.

## `confirmation-required`

- Message: confirmation required: pass --yes
- Hint: Re-run with `--yes` to accept, or run interactively.

## `schema-unknown-key`

- Message: unknown key `{key}` in {file}:{line}
- Hint: Did you mean `{suggestion}`?
- Provenance: `file`, `line`

## `schema-unsupported`

- Message: {file} uses schema {found}, which this dots does not support
- Hint: Use a dots version that supports schema {found}, or lower `schema` in the file.
- Provenance: `file`

## `schema-name-mismatch`

- Message: {file} declares name `{declared}` but its location says `{expected}`
- Hint: Rename the file or directory, or fix `name` in {file}.
- Provenance: `file`

## `path-escapes-package`

- Message: path `{path}` in {file} escapes the package directory
- Hint: Use a path inside the package directory; `..` and absolute sources are not allowed.
- Provenance: `file`, `line`

## `dots-too-old`

- Message: this repo requires dots {required}, but this is dots {current}
- Hint: Upgrade dots, or lower `requires_dots` in dots.toml.
- Provenance: `file`

## `version-conflict`

- Message: `{package}` has conflicting version requirements: {left} vs {right}
- Hint: No version satisfies both. Fix one of them, or override the dependent's `requires`.
- Provenance: `left`, `right`

## `version-unsupported`

- Message: `{provider}` cannot install the version `{version}` of `{package}`
- Hint: Use a version form the provider supports, or switch provider.
- Provenance: `file`

## `version-spec-invalid`

- Message: `{spec}` is not a valid version spec
- Hint: Use one of: `latest`, an exact version (`1.2.3`), a prefix (`1` or `1.2`), a floor (`>=1.2`), or `git:<ref>`.
- Provenance: `file`, `line`

## `no-provider-satisfies`

- Message: no provider can satisfy `{package}`
- Hint: Install or enable one of the candidate providers, or declare another provider for `{package}`.
- Provenance: `file`

## `provider-unavailable`

- Message: provider `{provider}` is not available on this machine
- Hint: Run `dots doctor`, or choose another provider with `dots migrate {package} --to <provider>`.

## `binary-collision`

- Message: `{bin}` is provided by both `{left}` and `{right}`
- Hint: Declare `conflicts` or `replaces` on one package, or remove one of them.
- Provenance: `left`, `right`

## `dependency-cycle`

- Message: dependency cycle: {cycle}
- Hint: Break the cycle by removing one `requires` edge.
- Provenance: `file`

## `name-collision`

- Message: `{name}` is defined more than once ({left}, {right})
- Hint: Rename one definition, or exclude one with `--exclude`.
- Provenance: `left`, `right`

## `unknown-tag`

- Message: unknown tag `{tag}`
- Hint: Declare it under `tags` in dots.toml; did you mean `{suggestion}`?
- Provenance: `file`, `line`

## `unknown-platform`

- Message: unknown platform `{platform}`
- Hint: Did you mean `{suggestion}`? Custom platforms are declared in dots.toml.
- Provenance: `file`, `line`

## `unknown-name`

- Message: unknown name `{name}` in {file}:{line}
- Hint: Check the spelling; run `dots config list` to see available settings.
- Provenance: `file`, `line`

## `setting-no-default`

- Message: setting `{setting}` has no default and no value
- Hint: Give it a default in package.toml, or run `dots config set {setting} <value>`.
- Provenance: `file`

## `setting-conflict`

- Message: setting `{setting}` is set differently by {left} and {right}
- Hint: Set it explicitly with `dots config set {setting} <value> --repo`.
- Provenance: `left`, `right`

## `setting-ref-unresolved`

- Message: `{reference}` does not refer to a declared setting
- Hint: Declare the setting in package.toml, or fix the reference.
- Provenance: `file`, `line`

## `setting-invalid`

- Message: `{setting}` set in {origin} is invalid: {detail}
- Hint: Fix the value in {origin}, or run `dots config set {setting} <value>`.
- Provenance: `origin`

## `setting-required`

- Message: `{setting}` is required and has no value
- Hint: dots config set {setting} <value>

## `setting-unknown`

- Message: no declared setting `{setting}`.{suggestion}
- Hint: Run `dots config list` to see the settings.

## `setting-key-invalid`

- Message: `{key}` is not a setting key
- Hint: Name a setting as <package>.<setting>, like `ghostty.font_size`.

## `setting-stale`

- Message: `{setting}` is set in {origin} but no package declares it
- Hint: Remove it, or run `dots config unset {setting}` for the machine scope.
- Provenance: `origin`

## `setting-inapplicable`

- Message: setting `{setting}` does not exist on this machine (its `when` is false)
- Hint: Settings with a `when` condition can only be set where it holds.

## `setting-secret-scope`

- Message: `{setting}` is secret and can only be set in machine.toml, not in {origin}
- Hint: Use `dots config set {setting} <value>` (machine scope) and remove it from {origin}.
- Provenance: `origin`

## `override-identity-field`

- Message: `{field}` is an identity field and cannot be overridden
- Hint: Use `dots eject {address}` to take ownership instead.
- Provenance: `file`

## `stale-override`

- Message: override `{address}` refers to something that no longer exists
- Hint: Remove it, or run `dots override edit {address}` to update it.
- Provenance: `file`

## `origin-modified`

- Message: `origin` of `{address}` was edited; it is an identity field
- Hint: Restore it, or run `dots eject {address}` to make the item your own.
- Provenance: `file`

## `integrity-mismatch`

- Message: checksum mismatch for `{item}`: expected {expected}, got {actual}
- Hint: If the change is expected run `dots pull`; otherwise investigate before running anything.
- Provenance: `file`

## `check-failed-after-install`

- Message: install of `{package}` succeeded but its check fails
- Hint: See the log: {log}. Fix the package's `check` or `install` command.
- Provenance: `file`

## `timeout`

- Message: `{package}` {action} exceeded its timeout of {timeout}
- Hint: Raise `timeout` in the package, or investigate the hang in {log}.

## `skipped-due-to-dep`

- Message: `{package}` skipped because `{dependency}` failed
- Hint: Fix `{dependency}` and re-run `dots apply {package}`.

## `action-failed`

- Message: `{package}` {action} via {provider} failed: {detail}
- Hint: See the log: {log}. Fix the cause, then re-run `dots apply {package}`.

## `plan-failed`

- Message: cannot plan `{package}`: {detail}
- Hint: Run `dots why {package}` to see the provider choice, or change the package's version or provider.

## `sudo-failed`

- Message: could not get root access for `{package}`
- Hint: Run `sudo -v` in a terminal and retry, or move the package to a provider that needs no root.

## `declined`

- Message: the plan was not confirmed, so nothing was changed
- Hint: Re-run and answer yes, or pass `--yes`.

## `lock-busy`

- Message: another dots run holds the lock (pid {pid})
- Hint: Wait for it to finish, then retry.

## `lock-conflicted`

- Message: dots.lock has merge-conflict markers
- Hint: Run `dots lock --fix` to regenerate it.
- Provenance: `file`

## `lock-invalid`

- Message: dots.lock cannot be read: {detail}
- Hint: Run `dots lock --fix` to regenerate it, keeping every valid row.
- Provenance: `file`

## `upgrade-blocked`

- Message: `{package}` cannot move to {version}: it would violate `{dependent}`'s requirement `{spec}`
- Hint: Relax the requirement in `{dependent}`, or upgrade `{dependent}` first.
- Provenance: `file`

## `lock-write-failed`

- Message: could not write dots.lock: {detail}
- Hint: Check that the repo directory is writable.
- Provenance: `file`

## `commit-failed`

- Message: could not commit: {detail}
- Hint: The files were changed but not committed. Commit them yourself, or fix the git problem and re-run with `--commit`.

## `native-pin-failed`

- Message: `{provider}` could not {action} `{package}`: {detail}
- Hint: The version is set in the package file; apply the provider's own pin by hand if you need it.
- Provenance: `file`

## `pin-unsupported`

- Message: `{package}` cannot be pinned: {detail}
- Hint: Pin a package that has a version to hold, or edit `version` in its package file.
- Provenance: `file`

## `patch-does-not-apply`

- Message: patch layer for `{dst}` no longer applies cleanly
- Hint: Re-create the patch with `dots adopt {dst} --as-patch`.
- Provenance: `file`

## `method-unsupported`

- Message: `{method}` is not supported by `{provider}` on this platform
- Hint: Use a different install method, or another provider.
- Provenance: `file`

## `file-unreadable`

- Message: cannot read {file}: {detail}
- Hint: Check that the file exists and is readable.
- Provenance: `file`

## `schema-syntax`

- Message: {file}:{line} is not valid TOML: {detail}
- Hint: Fix the syntax error at the reported position.
- Provenance: `file`, `line`

## `schema-wrong-type`

- Message: `{key}` in {file}:{line} must be {expected}, found {found}
- Hint: Change the value to {expected}.
- Provenance: `file`, `line`

## `schema-missing-key`

- Message: {file} is missing the required key `{key}`
- Hint: Add `{key}` to {file}.
- Provenance: `file`

## `schema-invalid-value`

- Message: invalid value for `{key}` in {file}:{line}: {detail}
- Hint: {hint}
- Provenance: `file`, `line`

## `escape-hatch-path`

- Message: path `{path}` in {file}:{line} uses the `//` repo-root escape hatch
- Hint: Move the file into the package directory; a package with a `//` path cannot be copied with `dots add --copy`.
- Provenance: `file`, `line`

## `package-missing-manifest`

- Message: {file} has no package.toml; the directory is skipped
- Hint: Add a package.toml (`schema = 1` and `name`), or delete the directory.
- Provenance: `file`

## `machine-profile-missing`

- Message: machine profile `{profile}` not found at {file}
- Hint: Create the profile, or fix `profile` in machine.toml.
- Provenance: `file`

## `unknown-fact`

- Message: unknown fact `{key}` in condition
- Hint: Run `dots facts` to see the available facts.{suggestion}
- Provenance: `file`, `line`

## `condition-syntax`

- Message: {detail}
- Hint: Conditions use cfg() syntax, e.g. `all(os = "linux", not(tag = "remote"))`.
- Provenance: `file`, `line`

## `condition-type`

- Message: {detail}
- Hint: Compare facts to quoted values; only boolean keys can stand alone.
- Provenance: `file`, `line`

## `unknown-package`

- Message: unknown package `{name}`
- Hint: Check the spelling, or add it under packages/.{suggestion}
- Provenance: `file`

## `unknown-bundle`

- Message: unknown bundle `{name}`
- Hint: Check the spelling, or add it under bundles/.{suggestion}
- Provenance: `file`

## `package-conflict`

- Message: `{left}` conflicts with `{right}`, but both are selected
- Hint: Remove one of them from your bundles, or add it to `exclude` in machine.toml.
- Provenance: `left`, `right`

## `excluded-dependency`

- Message: `{package}` is excluded on this machine but `{required_by}` requires it
- Hint: Remove `{package}` from `exclude` in machine.toml, or exclude `{required_by}` too.
- Provenance: `file`

## `script-no-check`

- Message: the script provider of `{package}` has no `check`
- Hint: Add a `check` command to [providers.script] in {file} so dots can tell whether the package is installed.
- Provenance: `file`

## `script-unpinned-fetch`

- Message: a script of `{package}` pipes a download into a shell: `{snippet}`
- Hint: Declare `fetch = { url, sha256 }` in [providers.script] in {file} and run it with `install = "sh {fetched}"` so the installer is pinned.
- Provenance: `file`

## `merge-type-mismatch`

- Message: `{path}` is {left_type} in {left} but {right_type} in {right}
- Hint: Make both layers use the same type, or delete the key in the later layer with "__dots_delete__".
- Provenance: `left`, `right`

## `layer-parse-error`

- Message: {file}:{line} is not valid {format}: {detail}
- Hint: Fix the syntax error at the reported position.
- Provenance: `file`, `line`

## `set-path-not-table`

- Message: `{path}` crosses `{at}`, which is {found}, not a table
- Hint: Fix the key path in `[[files.set]]`, or change the value it crosses.
- Provenance: `file`

## `directive-error`

- Message: {file}:{line}: {detail}
- Hint: Fix the directive at the reported position; directives are `dots:if`, `dots:elif`, `dots:else`, `dots:end`, `dots:if!`, `dots:include` and `@dots(expr)@`.
- Provenance: `file`, `line`

## `fragment-collision`

- Message: fragments from `{left}` and `{right}` both set `{key}` at priority {priority} in {dst}
- Hint: Give one of the fragments a different `priority`, or drop the key from one of them.
- Provenance: `left`, `right`

## `include-unsupported`

- Message: no native include mechanism is known for `{dst}`
- Hint: Drop `kind = "include"` to concatenate the layers, or set `format` to one that has includes.

## `write-failed`

- Message: cannot write {path}: {detail}
- Hint: Check permissions and free space. The previous content is untouched.

## `backup-not-found`

- Message: no backup exists for run `{run_id}`
- Hint: Backups are kept under the state directory in backup/<run-id>/; check the run id.

## `restore-skipped`

- Message: `{path}` was changed after dots wrote it, so it was left as is
- Hint: Move or delete it, then run `dots restore {run_id}` again.

## `setting-failed`

- Message: cannot change {setting}: {detail}
- Hint: Run the command shown in `dots plan` by hand to see why. Nothing else of this run was undone.

## `restart-failed`

- Message: could not restart {app}: {detail}
- Hint: Quit and reopen {app} yourself; the setting is already written.

## `service-failed`

- Message: cannot {action} the service `{service}`: {detail}
- Hint: Run `dots status` for its state, and the manager's own logs (`journalctl --user -u <unit>`, `brew services info`) for why.

## `hook-failed`

- Message: the {event} hook of `{package}` failed: {detail} (log: {log})
- Hint: Fix the command in the package's `[hooks]` and run `dots apply {package}` again; a failed hook runs again until it passes.

## `service-name-invalid`

- Message: the service name `{name}` of package `{package}` is not allowed
- Hint: A service name, and the unit or label it uses, may only contain letters, digits, `.`, `_`, `@` and `-`, and cannot start with `-`. It ends up in commands, so nothing else is accepted.

## `not-initialized`

- Message: no dots repo at {root}
- Hint: Run `dots init` to set this machine up, or `dots init --from <source>` to clone your repo.

## `init-machine-exists`

- Message: a machine config already exists at {path}
- Hint: `dots init` never overwrites it. Edit it, or move it aside and run `dots init` again.

## `init-root-exists`

- Message: {path} already exists and is not empty
- Hint: Pick another location with `--root <path>`, or run `dots init` without `--from` to use the repo already there.

## `clone-failed`

- Message: cannot clone {source}: {detail}
- Hint: Check the source, your network and your credentials, then run `dots init --from {source}` again.

## `init-answers-needed`

- Message: no answer for required setting(s): {settings}
- Hint: Run `dots init` in a terminal to be asked, or add the values under `[settings.<pkg>]` in the repo's dots.toml, then run `dots init` again (without `--from`).

## `bin-not-found`

- Message: `{bin}` is not an executable on PATH
- Hint: Check the spelling, or run `dots bins` to list what is on PATH. If dots installed it, run `dots doctor` to check PATH order.

## `latest-lookup-failed`

- Message: could not look up the latest versions from {provider}: {detail}
- Hint: Run `dots outdated --refresh` again later, or `dots doctor` to check the provider.

## `address-invalid`

- Message: `{address}` is not a valid address: {detail}
- Hint: Addresses look like `pkg:<name>`, `pkg:<name>/file:<dst>` or `bundle:<name>`.

## `address-not-found`

- Message: nothing exists at `{address}`
- Hint: Check the address with `dots status` or `dots override list`.{suggestion}

## `redundant-override`

- Message: override `{address}` sets `{path}` to the value it already has
- Hint: Remove the key from {file}; it changes nothing.
- Provenance: `file`, `line`

## `editor-failed`

- Message: the editor `{editor}` did not finish cleanly
- Hint: Set $EDITOR to a command that exits 0 once the file is saved.

## `adopt-not-managed`

- Message: `{dst}` is not a file that dots manages
- Hint: Run `dots status --all` to list managed files, or pass the destination as it appears there.

## `adopt-not-modified`

- Message: `{dst}` is `{state}`, not `modified`: there is nothing to adopt
- Hint: `dots adopt` takes a managed copy, render or inject file that was edited after apply. See `dots status`.

## `adopt-unsupported`

- Message: cannot adopt `{dst}`: {detail}
- Hint: Edit the source in the repo directly, or pick another layer with `--layer`.

## `adopt-base-unknown`

- Message: cannot adopt `{dst}`: the last rendered version is unavailable ({detail})
- Hint: Run `dots apply` (your version is backed up first), then make the edit in the repo.

## `adopt-conflict`

- Message: your edit to `{dst}` conflicts with {file}: {detail}
- Hint: Nothing was written. Change {file} by hand, or adopt into another layer with `--layer` or `--machine`.
- Provenance: `file`

## `adopt-ineffective`

- Message: adopting into {file} would not reproduce your version of `{dst}`: {detail}
- Hint: A later layer, a directive or a `[[files.set]]` overrides it. Nothing was written. Try `--machine` or a later `--layer`.
- Provenance: `file`

## `adopt-unknown-layer`

- Message: `{layer}` is not a layer of `{dst}` (layers: {layers})
- Hint: Pass one of the listed layers, a platform name such as `macos`, or use `--machine`.
