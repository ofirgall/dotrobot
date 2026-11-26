# Spec Delta

## Purpose

Defines the detected facts, the general-to-specific platform chain, `[platform."<key>"]` tables, tags, and the `cfg()`-style condition language. Every other capability uses them to decide what applies on a given machine.

## ADDED Requirements

### Requirement: Detected facts
dots SHALL detect these facts once per run:
- `os`, `arch`, `distro`, `distro_family`, `distro_version`
- `wsl`, `sudo`, `gui`
- `hostname`, `user`, `home`, `shell`
- `provider.<id>`

`machine.toml` `[facts]` SHALL be able to override `sudo`, `gui` and `wsl`. `dots facts` SHALL print every fact along with where it came from.

#### Scenario: WSL2 detection
- **WHEN** `/proc/sys/kernel/osrelease` contains `microsoft` in any letter case
- **THEN** `wsl` is true

#### Scenario: Forced no-sudo
- **WHEN** `[facts] sudo = false`
- **THEN** providers that need root are unavailable, even if `sudo -n true` succeeds

### Requirement: Platform chain
dots SHALL build an ordered chain of platform keys, from general to specific:
- `any`, then `unix` (on unix-like systems);
- then the OS;
- then the `ID_LIKE` parents read in reverse, then `ID`, then `ID-<major>`, then `ID-<VERSION_ID>`;
- then `wsl` when detected;
- then any `platform_extra`.

Each level SHALL be followed immediately by its `<key>/<arch>` qualifier.

#### Scenario: Ubuntu 24.04 chain
- **WHEN** dots runs on Ubuntu 24.04 x86_64
- **THEN** `dots facts` prints `any › unix › linux › debian › ubuntu › ubuntu-24 › ubuntu-24.04 (x86_64)`

#### Scenario: Derived distro
- **WHEN** dots runs on Linux Mint 22.1
- **THEN** the chain contains `ubuntu` before `linuxmint`

### Requirement: Custom platforms
`dots.toml` `[platforms.<key>] parent = "<key>"` SHALL insert a custom platform under its parent. A machine SHALL be able to force extra levels with `platform_extra`.

#### Scenario: Missing ID_LIKE
- **WHEN** `[platforms.pop] parent = "ubuntu"` and the OS ID is `pop`
- **THEN** the chain contains `ubuntu` before `pop`

### Requirement: Platform tables
Any object that supports platform tables SHALL deep-merge each `[platform."<key>"]` table whose key is in the chain, in chain order, so that the most specific key wins. Tables SHALL merge; scalars and arrays SHALL replace; `{add, remove, prepend}` SHALL edit lists.

Objects that support platform tables:
- packages, provider sections, bundles and bundle members;
- file entries, settings and manual actions;
- `dots.toml` sections;
- overrides.

#### Scenario: Release-specific override
- **WHEN** a package sets `[platform.ubuntu.providers.apt] repo = { ppa = "x" }` and `[platform."ubuntu-22.04"] prefer = ["github-release"]`
- **THEN** on 24.04 the PPA is used, and on 22.04 github-release is preferred

#### Scenario: Unknown platform key
- **WHEN** a table uses `[platform."ubunut-22.04"]`
- **THEN** lint fails with `unknown-platform`, suggesting `ubuntu-22.04`

### Requirement: Condition language
Conditions SHALL use this grammar:
- `all(...)`, `any(...)`, `not(...)`
- `key = "value"`, `key != "value"`
- ordered comparisons `< <= > >=`, for versions and numbers
- bare boolean keys

Keys SHALL be:
- facts;
- `tag`;
- `platform`, which is true when that key appears anywhere in the chain;
- `setting.<k>` and `settings.<pkg>.<k>`.

Unknown keys and undeclared tags SHALL be errors, never false.

#### Scenario: Is-a platform test
- **WHEN** `when = 'platform = "ubuntu"'` is evaluated on Linux Mint
- **THEN** it is true

#### Scenario: Version comparison
- **WHEN** `all(platform = "ubuntu", distro_version >= "24.04")` is evaluated on 22.04
- **THEN** it is false

#### Scenario: Cross-package setting reference
- **WHEN** a condition reads `settings.ghostty.theme` and `ghostty` is not in the resolved set
- **THEN** lint fails with `setting-ref-unresolved`

### Requirement: Merge before evaluating
An object SHALL be merged through its platform tables first. Its `when` SHALL then be evaluated on the merged result, so a platform table can change `when`.

#### Scenario: Platform changes existence
- **WHEN** a package has `when = 'os = "macos"'` and `[platform.wsl] when = "true"`
- **THEN** the package is active under WSL
