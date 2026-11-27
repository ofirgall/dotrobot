//! The error-code registry: the single source for `docs/errors.md`, the JSON `errors[].code`
//! values and the hint text (design D4).

/// One registered error. Templates use `{name}` placeholders filled by [`fill`].
#[derive(Debug, Clone, Copy)]
pub struct ErrorDef {
    pub code: &'static str,
    pub message: &'static str,
    pub hint: &'static str,
    /// Provenance fields this error carries (`file`, `line`, `left`, `right`, ...).
    pub provenance: &'static [&'static str],
}

macro_rules! def {
    ($code:literal, $msg:literal, $hint:literal, [$($p:literal),*]) => {
        ErrorDef { code: $code, message: $msg, hint: $hint, provenance: &[$($p),*] }
    };
}

pub static REGISTRY: &[ErrorDef] = &[
    def!(
        "not-implemented",
        "`{command}` is not implemented yet.",
        "This command is a placeholder in this build; see the roadmap in README.md.",
        []
    ),
    def!(
        "usage-error",
        "{detail}",
        "Run `{help}` to see the accepted arguments.",
        []
    ),
    def!(
        "confirmation-required",
        "confirmation required: pass --yes",
        "Re-run with `--yes` to accept, or run interactively.",
        []
    ),
    def!(
        "schema-unknown-key",
        "unknown key `{key}` in {file}:{line}",
        "Did you mean `{suggestion}`?",
        ["file", "line"]
    ),
    def!(
        "schema-unsupported",
        "{file} uses schema {found}, which this dots does not support",
        "Use a dots version that supports schema {found}, or lower `schema` in the file.",
        ["file"]
    ),
    def!(
        "schema-name-mismatch",
        "{file} declares name `{declared}` but its location says `{expected}`",
        "Rename the file or directory, or fix `name` in {file}.",
        ["file"]
    ),
    def!(
        "path-escapes-package",
        "path `{path}` in {file} escapes the package directory",
        "Use a path inside the package directory; `..` and absolute sources are not allowed.",
        ["file", "line"]
    ),
    def!(
        "dots-too-old",
        "this repo requires dots {required}, but this is dots {current}",
        "Upgrade dots, or lower `requires_dots` in dots.toml.",
        ["file"]
    ),
    def!(
        "version-conflict",
        "`{package}` has conflicting version requirements: {left} vs {right}",
        "No version satisfies both. Fix one of them, or override the dependent's `requires`.",
        ["left", "right"]
    ),
    def!(
        "version-unsupported",
        "`{provider}` cannot install the version `{version}` of `{package}`",
        "Use a version form the provider supports, or switch provider.",
        ["file"]
    ),
    def!(
        "version-spec-invalid",
        "`{spec}` is not a valid version spec",
        "Use one of: `latest`, an exact version (`1.2.3`), a prefix (`1` or `1.2`), a floor (`>=1.2`), or `git:<ref>`.",
        ["file", "line"]
    ),
    def!(
        "no-provider-satisfies",
        "no provider can satisfy `{package}`",
        "Install or enable one of the candidate providers, or declare another provider for `{package}`.",
        ["file"]
    ),
    def!(
        "provider-unavailable",
        "provider `{provider}` is not available on this machine",
        "Run `dots doctor`, or choose another provider with `dots migrate {package} --to <provider>`.",
        []
    ),
    def!(
        "binary-collision",
        "`{bin}` is provided by both `{left}` and `{right}`",
        "Declare `conflicts` or `replaces` on one package, or remove one of them.",
        ["left", "right"]
    ),
    def!(
        "dependency-cycle",
        "dependency cycle: {cycle}",
        "Break the cycle by removing one `requires` edge.",
        ["file"]
    ),
    def!(
        "name-collision",
        "`{name}` is defined more than once ({left}, {right})",
        "Rename one definition, or exclude one with `--exclude`.",
        ["left", "right"]
    ),
    def!(
        "unknown-tag",
        "unknown tag `{tag}`",
        "Declare it under `tags` in dots.toml; did you mean `{suggestion}`?",
        ["file", "line"]
    ),
    def!(
        "unknown-platform",
        "unknown platform `{platform}`",
        "Did you mean `{suggestion}`? Custom platforms are declared in dots.toml.",
        ["file", "line"]
    ),
    def!(
        "unknown-name",
        "unknown name `{name}` in {file}:{line}",
        "Check the spelling; run `dots config list` to see available settings.",
        ["file", "line"]
    ),
    def!(
        "setting-no-default",
        "setting `{setting}` has no default and no value",
        "Give it a default in package.toml, or run `dots config set {setting} <value>`.",
        ["file"]
    ),
    def!(
        "setting-conflict",
        "setting `{setting}` is set differently by {left} and {right}",
        "Set it explicitly with `dots config set {setting} <value> --repo`.",
        ["left", "right"]
    ),
    def!(
        "setting-ref-unresolved",
        "`{reference}` does not refer to a declared setting",
        "Declare the setting in package.toml, or fix the reference.",
        ["file", "line"]
    ),
    def!(
        "setting-invalid",
        "`{setting}` set in {origin} is invalid: {detail}",
        "Fix the value in {origin}, or run `dots config set {setting} <value>`.",
        ["origin"]
    ),
    def!(
        "setting-required",
        "`{setting}` is required and has no value",
        "dots config set {setting} <value>",
        []
    ),
    def!(
        "setting-unknown",
        "no declared setting `{setting}`.{suggestion}",
        "Run `dots config list` to see the settings.",
        []
    ),
    def!(
        "setting-key-invalid",
        "`{key}` is not a setting key",
        "Name a setting as <package>.<setting>, like `ghostty.font_size`.",
        []
    ),
    def!(
        "setting-stale",
        "`{setting}` is set in {origin} but no package declares it",
        "Remove it, or run `dots config unset {setting}` for the machine scope.",
        ["origin"]
    ),
    def!(
        "setting-inapplicable",
        "setting `{setting}` does not exist on this machine (its `when` is false)",
        "Settings with a `when` condition can only be set where it holds.",
        []
    ),
    def!(
        "setting-secret-scope",
        "`{setting}` is secret and can only be set in machine.toml, not in {origin}",
        "Use `dots config set {setting} <value>` (machine scope) and remove it from {origin}.",
        ["origin"]
    ),
    def!(
        "override-identity-field",
        "`{field}` is an identity field and cannot be overridden",
        "Use `dots eject {address}` to take ownership instead.",
        ["file"]
    ),
    def!(
        "stale-override",
        "override `{address}` refers to something that no longer exists",
        "Remove it, or run `dots override edit {address}` to update it.",
        ["file"]
    ),
    def!(
        "origin-modified",
        "`origin` of `{address}` was edited; it is an identity field",
        "Restore it, or run `dots eject {address}` to make the item your own.",
        ["file"]
    ),
    def!(
        "integrity-mismatch",
        "checksum mismatch for `{item}`: expected {expected}, got {actual}",
        "If the change is expected run `dots pull`; otherwise investigate before running anything.",
        ["file"]
    ),
    def!(
        "check-failed-after-install",
        "install of `{package}` succeeded but its check fails",
        "See the log: {log}. Fix the package's `check` or `install` command.",
        ["file"]
    ),
    def!(
        "timeout",
        "`{package}` {action} exceeded its timeout of {timeout}",
        "Raise `timeout` in the package, or investigate the hang in {log}.",
        []
    ),
    def!(
        "skipped-due-to-dep",
        "`{package}` skipped because `{dependency}` failed",
        "Fix `{dependency}` and re-run `dots apply {package}`.",
        []
    ),
    def!(
        "action-failed",
        "`{package}` {action} via {provider} failed: {detail}",
        "See the log: {log}. Fix the cause, then re-run `dots apply {package}`.",
        []
    ),
    def!(
        "plan-failed",
        "cannot plan `{package}`: {detail}",
        "Run `dots why {package}` to see the provider choice, or change the package's version or provider.",
        []
    ),
    def!(
        "sudo-failed",
        "could not get root access for `{package}`",
        "Run `sudo -v` in a terminal and retry `dots apply --with-sudo`, or move the package to a provider that needs no root.",
        []
    ),
    def!(
        "declined",
        "the plan was not confirmed, so nothing was changed",
        "Re-run and answer yes, or pass `--yes`.",
        []
    ),
    def!(
        "lock-busy",
        "another dots run holds the lock (pid {pid})",
        "Wait for it to finish, then retry.",
        []
    ),
    def!(
        "lock-conflicted",
        "dots.lock has merge-conflict markers",
        "Run `dots lock --fix` to regenerate it.",
        ["file"]
    ),
    def!(
        "lock-invalid",
        "dots.lock cannot be read: {detail}",
        "Run `dots lock --fix` to regenerate it, keeping every valid row.",
        ["file"]
    ),
    def!(
        "upgrade-blocked",
        "`{package}` cannot move to {version}: it would violate `{dependent}`'s requirement `{spec}`",
        "Relax the requirement in `{dependent}`, or upgrade `{dependent}` first.",
        ["file"]
    ),
    def!(
        "lock-write-failed",
        "could not write dots.lock: {detail}",
        "Check that the repo directory is writable.",
        ["file"]
    ),
    def!(
        "commit-failed",
        "could not commit: {detail}",
        "The files were changed but not committed. Commit them yourself, or fix the git problem and re-run with `--commit`.",
        []
    ),
    def!(
        "native-pin-failed",
        "`{provider}` could not {action} `{package}`: {detail}",
        "The version is set in the package file; apply the provider's own pin by hand if you need it.",
        ["file"]
    ),
    def!(
        "pin-unsupported",
        "`{package}` cannot be pinned: {detail}",
        "Pin a package that has a version to hold, or edit `version` in its package file.",
        ["file"]
    ),
    def!(
        "patch-does-not-apply",
        "patch layer for `{dst}` no longer applies cleanly",
        "Re-create the patch with `dots adopt {dst} --as-patch`.",
        ["file"]
    ),
    def!(
        "method-unsupported",
        "`{method}` is not supported by `{provider}` on this platform",
        "Use a different install method, or another provider.",
        ["file"]
    ),
    def!(
        "file-unreadable",
        "cannot read {file}: {detail}",
        "Check that the file exists and is readable.",
        ["file"]
    ),
    def!(
        "schema-syntax",
        "{file}:{line} is not valid TOML: {detail}",
        "Fix the syntax error at the reported position.",
        ["file", "line"]
    ),
    def!(
        "schema-wrong-type",
        "`{key}` in {file}:{line} must be {expected}, found {found}",
        "Change the value to {expected}.",
        ["file", "line"]
    ),
    def!(
        "schema-missing-key",
        "{file} is missing the required key `{key}`",
        "Add `{key}` to {file}.",
        ["file"]
    ),
    def!(
        "schema-invalid-value",
        "invalid value for `{key}` in {file}:{line}: {detail}",
        "{hint}",
        ["file", "line"]
    ),
    def!(
        "escape-hatch-path",
        "path `{path}` in {file}:{line} uses the `//` repo-root escape hatch",
        "Move the file into the package directory; a package with a `//` path cannot be copied with `dots add --copy`.",
        ["file", "line"]
    ),
    def!(
        "manual-invalid",
        "{file}: {detail}",
        "Fix the front matter (`id`, `title`, `when`, `after`, `check`, `requires`, `revision`, `interactive`).",
        ["file", "line"]
    ),
    def!(
        "manual-unknown",
        "no manual action `{id}`",
        "Run `dots todo` to see the ids; they look like `<package>/<id>`.",
        []
    ),
    def!(
        "manual-duplicate-id",
        "two manual actions of one package share the id `{id}` (the other is {other})",
        "Give each file under manual/ its own `id`.",
        ["file"]
    ),
    def!(
        "package-missing-manifest",
        "{file} has no package.toml; the directory is skipped",
        "Add a package.toml (`schema = 1` and `name`), or delete the directory.",
        ["file"]
    ),
    def!(
        "machine-profile-missing",
        "machine profile `{profile}` not found at {file}",
        "Create the profile, or fix `profile` in machine.toml.",
        ["file"]
    ),
    def!(
        "unknown-fact",
        "unknown fact `{key}` in condition",
        "Run `dots facts` to see the available facts.{suggestion}",
        ["file", "line"]
    ),
    def!(
        "condition-syntax",
        "{detail}",
        "Conditions use cfg() syntax, e.g. `all(os = \"linux\", not(tag = \"remote\"))`.",
        ["file", "line"]
    ),
    def!(
        "condition-type",
        "{detail}",
        "Compare facts to quoted values; only boolean keys can stand alone.",
        ["file", "line"]
    ),
    def!(
        "unknown-package",
        "unknown package `{name}`",
        "Check the spelling, or add it under packages/.{suggestion}",
        ["file"]
    ),
    def!(
        "unknown-bundle",
        "unknown bundle `{name}`",
        "Check the spelling, or add it under bundles/.{suggestion}",
        ["file"]
    ),
    def!(
        "package-conflict",
        "`{left}` conflicts with `{right}`, but both are selected",
        "Remove one of them from your bundles, or add it to `exclude` in machine.toml.",
        ["left", "right"]
    ),
    def!(
        "excluded-dependency",
        "`{package}` is excluded on this machine but `{required_by}` requires it",
        "Remove `{package}` from `exclude` in machine.toml, or exclude `{required_by}` too.",
        ["file"]
    ),
    def!(
        "script-no-check",
        "the script provider of `{package}` has no `check`",
        "Add a `check` command to [providers.script] in {file} so dots can tell whether the package is installed.",
        ["file"]
    ),
    def!(
        "requires-sudo-redundant",
        "`{package}` sets `requires_sudo` on its `{provider}` provider, which already needs root",
        "Remove `requires_sudo` from {file}: a `{provider}` install is always a sudo step.",
        ["file"]
    ),
    def!(
        "script-unpinned-fetch",
        "a script of `{package}` pipes a download into a shell: `{snippet}`",
        "Declare `fetch = { url, sha256 }` in [providers.script] in {file} and run it with `install = \"sh {fetched}\"` so the installer is pinned.",
        ["file"]
    ),
    def!(
        "merge-type-mismatch",
        "`{path}` is {left_type} in {left} but {right_type} in {right}",
        "Make both layers use the same type, or delete the key in the later layer with \"__dots_delete__\".",
        ["left", "right"]
    ),
    def!(
        "layer-parse-error",
        "{file}:{line} is not valid {format}: {detail}",
        "Fix the syntax error at the reported position.",
        ["file", "line"]
    ),
    def!(
        "set-path-not-table",
        "`{path}` crosses `{at}`, which is {found}, not a table",
        "Fix the key path in `[[files.set]]`, or change the value it crosses.",
        ["file"]
    ),
    def!(
        "directive-error",
        "{file}:{line}: {detail}",
        "Fix the directive at the reported position; directives are `dots:if`, `dots:elif`, `dots:else`, `dots:end`, `dots:if!`, `dots:include` and `@dots(expr)@`.",
        ["file", "line"]
    ),
    def!(
        "fragment-collision",
        "fragments from `{left}` and `{right}` both set `{key}` at priority {priority} in {dst}",
        "Give one of the fragments a different `priority`, or drop the key from one of them.",
        ["left", "right"]
    ),
    def!(
        "include-unsupported",
        "no native include mechanism is known for `{dst}`",
        "Drop `kind = \"include\"` to concatenate the layers, or set `format` to one that has includes.",
        []
    ),
    def!(
        "write-failed",
        "cannot write {path}: {detail}",
        "Check permissions and free space. The previous content is untouched.",
        []
    ),
    def!(
        "backup-not-found",
        "no backup exists for run `{run_id}`",
        "Backups are kept under the state directory in backup/<run-id>/; check the run id.",
        []
    ),
    def!(
        "restore-skipped",
        "`{path}` was changed after dots wrote it, so it was left as is",
        "Move or delete it, then run `dots restore {run_id}` again.",
        []
    ),
    def!(
        "setting-failed",
        "cannot change {setting}: {detail}",
        "Run the command shown in `dots plan` by hand to see why. Nothing else of this run was undone.",
        []
    ),
    def!(
        "restart-failed",
        "could not restart {app}: {detail}",
        "Quit and reopen {app} yourself; the setting is already written.",
        []
    ),
    def!(
        "service-failed",
        "cannot {action} the service `{service}`: {detail}",
        "Run `dots status` for its state, and the manager's own logs (`journalctl --user -u <unit>`, `brew services info`) for why.",
        []
    ),
    def!(
        "hook-failed",
        "the {event} hook of `{package}` failed: {detail} (log: {log})",
        "Fix the command in the package's `[hooks]` and run `dots apply {package}` again; a failed hook runs again until it passes.",
        []
    ),
    def!(
        "service-name-invalid",
        "the service name `{name}` of package `{package}` is not allowed",
        "A service name, and the unit or label it uses, may only contain letters, digits, `.`, `_`, `@` and `-`, and cannot start with `-`. It ends up in commands, so nothing else is accepted.",
        []
    ),
    def!(
        "not-initialized",
        "no dots repo at {root}",
        "Run `dots init` to set this machine up, or `dots init --from <source>` to clone your repo.",
        []
    ),
    def!(
        "init-machine-exists",
        "a machine config already exists at {path}",
        "`dots init` never overwrites it. Edit it, or move it aside and run `dots init` again.",
        []
    ),
    def!(
        "init-root-exists",
        "{path} already exists and is not empty",
        "Pick another location with `--root <path>`, or run `dots init` without `--from` to use the repo already there.",
        []
    ),
    def!(
        "clone-failed",
        "cannot clone {source}: {detail}",
        "Check the source, your network and your credentials, then run `dots init --from {source}` again.",
        []
    ),
    def!(
        "init-answers-needed",
        "no answer for required setting(s): {settings}",
        "Run `dots init` in a terminal to be asked, or add the values under `[settings.<pkg>]` in the repo's dots.toml, then run `dots init` again (without `--from`).",
        []
    ),
    def!(
        "bin-not-found",
        "`{bin}` is not an executable on PATH",
        "Check the spelling, or run `dots bins` to list what is on PATH. If dots installed it, run `dots doctor` to check PATH order.",
        []
    ),
    def!(
        "latest-lookup-failed",
        "could not look up the latest versions from {provider}: {detail}",
        "Run `dots outdated --refresh` again later, or `dots doctor` to check the provider.",
        []
    ),
    def!(
        "address-invalid",
        "`{address}` is not a valid address: {detail}",
        "Addresses look like `pkg:<name>`, `pkg:<name>/file:<dst>` or `bundle:<name>`.",
        []
    ),
    def!(
        "address-not-found",
        "nothing exists at `{address}`",
        "Check the address with `dots status` or `dots override list`.{suggestion}",
        []
    ),
    def!(
        "redundant-override",
        "override `{address}` sets `{path}` to the value it already has",
        "Remove the key from {file}; it changes nothing.",
        ["file", "line"]
    ),
    def!(
        "editor-failed",
        "the editor `{editor}` did not finish cleanly",
        "Set $EDITOR to a command that exits 0 once the file is saved.",
        []
    ),
    def!(
        "adopt-not-managed",
        "`{dst}` is not a file that dots manages",
        "Run `dots status --all` to list managed files, or pass the destination as it appears there.",
        []
    ),
    def!(
        "adopt-not-modified",
        "`{dst}` is `{state}`, not `modified`: there is nothing to adopt",
        "`dots adopt` takes a managed copy, render or inject file that was edited after apply. See `dots status`.",
        []
    ),
    def!(
        "adopt-unsupported",
        "cannot adopt `{dst}`: {detail}",
        "Edit the source in the repo directly, or pick another layer with `--layer`.",
        []
    ),
    def!(
        "adopt-base-unknown",
        "cannot adopt `{dst}`: the last rendered version is unavailable ({detail})",
        "Run `dots apply` (your version is backed up first), then make the edit in the repo.",
        []
    ),
    def!(
        "adopt-conflict",
        "your edit to `{dst}` conflicts with {file}: {detail}",
        "Nothing was written. Change {file} by hand, or adopt into another layer with `--layer` or `--machine`.",
        ["file"]
    ),
    def!(
        "adopt-ineffective",
        "adopting into {file} would not reproduce your version of `{dst}`: {detail}",
        "A later layer, a directive or a `[[files.set]]` overrides it. Nothing was written. Try `--machine` or a later `--layer`.",
        ["file"]
    ),
    def!(
        "adopt-unknown-layer",
        "`{layer}` is not a layer of `{dst}` (layers: {layers})",
        "Pass one of the listed layers, a platform name such as `macos`, or use `--machine`.",
        []
    ),
];

pub fn lookup(code: &str) -> Option<&'static ErrorDef> {
    REGISTRY.iter().find(|d| d.code == code)
}

/// Replace `{key}` with its value; unknown placeholders are left as written.
pub fn fill(template: &str, args: &[(&str, &str)]) -> String {
    let mut s = template.to_string();
    for (k, v) in args {
        s = s.replace(&format!("{{{k}}}"), v);
    }
    s
}

/// The contents of `docs/errors.md`, generated from the registry.
pub fn render_doc() -> String {
    let mut s = String::from(
        "# Error codes\n\n<!-- Generated from crates/dots-core/src/errors.rs. Do not edit; \
         regenerate with `cargo run -p dots-core --example gen-errors-doc`. -->\n\n\
         Every error dots reports carries one of these stable codes (`errors[].code` in `--json`).\n\
         `{name}` marks a value filled in at report time.\n",
    );
    for d in REGISTRY {
        s.push_str(&format!("\n## `{}`\n\n", d.code));
        s.push_str(&format!("- Message: {}\n", d.message));
        s.push_str(&format!("- Hint: {}\n", d.hint));
        if !d.provenance.is_empty() {
            let p: Vec<String> = d.provenance.iter().map(|x| format!("`{x}`")).collect();
            s.push_str(&format!("- Provenance: {}\n", p.join(", ")));
        }
    }
    s
}
