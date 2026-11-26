# Spec Delta

## Purpose

Defines SHA-256 integrity for everything dots places: source trees pinned in the lock, deployed files, downloads, and author pins. The policy is warn or enforce, so tampered code that runs or instructs an agent is detected and restored.

## ADDED Requirements

### Requirement: Source content hashes
For each followed or copied item, `dots.lock` SHALL record a tree hash per item and rev in `[[content]]`. `apply`, `pull` and `verify` SHALL fail with `integrity-mismatch` when the content at a locked rev hashes differently.

#### Scenario: Rewritten history
- **WHEN** the cached upstream checkout for a locked rev has different content from the lock's tree hash
- **THEN** apply fails with `integrity-mismatch`, naming the item and both hashes

### Requirement: Deployed file hashes
dots SHALL record the hash of every file it renders, copies or links in its state. `status` SHALL detect changes by checking file metadata first and hashing only files whose metadata changed.

#### Scenario: Fast status
- **WHEN** no file's metadata has changed since the last run
- **THEN** status hashes no files

### Requirement: Download hashes
A download (release asset, `script` `fetch` file, browser extension package, apt repository key) SHALL be verified against, in order of preference:
1. the package's `sha256`;
2. the publisher's checksum file;
3. the hash recorded in the lock on first use.

The download SHALL be verified before it is extracted or run.

#### Scenario: Package-pinned hash mismatch
- **WHEN** a downloaded asset does not match the package's `sha256`
- **THEN** nothing is extracted, and the install fails with `integrity-mismatch`

### Requirement: Author pins
An `[integrity]` table in a package or an override SHALL pin individual files (`"<path>" = "sha256:…"`) or the whole tree (`tree = "sha256:…"`). A mismatch SHALL refuse the install.

#### Scenario: Pinned skill
- **WHEN** an override pins `skill/SKILL.md`, and a pull changes that file
- **THEN** the pull refuses with `integrity-mismatch` until the pin is updated

### Requirement: Policy
`integrity = "warn" | "enforce"` SHALL be settable per package, bundle or upstream. The default SHALL be:
- `enforce` for skills, scripts and hooks;
- `warn` for configs.

Under `enforce`, a changed deployed file SHALL be `tampered`: apply restores it, and `adopt` refuses it unless the item is named explicitly.

#### Scenario: Config edited by its app
- **WHEN** an app rewrites its own config file under the default policy
- **THEN** status shows `modified`, not `tampered`

#### Scenario: Script edited by hand
- **WHEN** an installed hook script is edited by hand
- **THEN** status shows `tampered`, and the next apply restores it

### Requirement: Verify command
`dots verify [<item>]` SHALL recompute every hash: sources, deployed files and downloaded binaries. For each mismatch, it SHALL list the expected hash, the actual hash, and where the expectation came from. It SHALL exit 2 when any mismatch exists.

#### Scenario: Clean verify
- **WHEN** every hash matches
- **THEN** verify prints a single "All N items verified" line and exits 0

### Requirement: Apt key fingerprint
An apt repository key SHALL be written to its keyring only when its fingerprint equals the declared `fingerprint`. A mismatch SHALL fail with `integrity-mismatch` and nothing SHALL be written for that repository.

#### Scenario: Fingerprint mismatch refuses
- **WHEN** the downloaded key's fingerprint differs from the package's `fingerprint`
- **THEN** the install fails with `integrity-mismatch`, no keyring or sources file is written, and the packages from that repository are skipped

### Requirement: Fetched installer hash
A `script` `fetch` file SHALL be verified against the author or lock hash before it runs. When neither exists, the hash SHALL be recorded in the lock on first use.

#### Scenario: Installer changed upstream
- **WHEN** the file at a `fetch` URL hashes differently from the pinned or locked hash
- **THEN** it does not run, and the install fails with `integrity-mismatch`

#### Scenario: First use
- **WHEN** a `fetch` package has no pinned hash and is installed
- **THEN** the lock records the hash, and `verify` checks it afterwards
