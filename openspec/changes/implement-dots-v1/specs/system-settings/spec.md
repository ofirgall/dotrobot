# Spec Delta

## Purpose

Defines declared system settings that are not files: macOS `defaults` domains, Windows registry values and Linux `xdg` defaults. They are read before they are written, checked for drift like files, and restored to their original value when the declaration is removed.

## ADDED Requirements

### Requirement: Setting declarations
A package SHALL be able to declare:
- `[[defaults]]` (`domain`, `key`, `value`, `restart`, `root`), implying macOS;
- `[[registry]]` (`key`, `name`, `type`, `value`), implying Windows;
- `[[xdg]]` (`mime` and `app`, or `setting` and `value`), implying Linux.

A `defaults` value's type SHALL come from the TOML value. A dotted `key` SHALL edit one entry of a dict. Unknown keys SHALL be errors.

#### Scenario: Implied platform
- **WHEN** a package declares a `[[defaults]]` entry and the machine is Linux
- **THEN** the entry is inert, and `status --all` shows it as `n/a on this platform`

#### Scenario: Dotted key edits one entry
- **WHEN** an entry has `domain = "com.apple.symbolichotkeys"`, `key = "AppleSymbolicHotKeys.64"` and `value = { enabled = false }`
- **THEN** only that entry of the dict is written, and the other entries are untouched

#### Scenario: Unknown key
- **WHEN** a `[[defaults]]` entry has a key `valeu`
- **THEN** parsing fails with `file:line:col` and a "did you mean `value`" hint

### Requirement: Read before write
`plan` SHALL read the current values in one batched pass per domain or registry hive (`defaults read` or export, the registry API, `xdg-mime query`). Only values that differ SHALL become steps.

#### Scenario: Value already set
- **WHEN** `KeyRepeat` is already `2` and the package declares `value = 2`
- **THEN** the plan has no step for it, and status shows `ok`

#### Scenario: One read per domain
- **WHEN** a package declares five keys in `NSGlobalDomain`
- **THEN** the domain is read once

### Requirement: Drift and adoption
Settings SHALL use the file state vocabulary. A value that someone changed SHALL be `drift`, showing the observed and the wanted value, and SHALL offer a `dots adopt` resolution that writes the observed value back into the package.

#### Scenario: Defaults drift adopted
- **WHEN** the user changes `KeyRepeat` to `6` in System Settings and the package says `2`
- **THEN** status shows `drift` with `observed 6, wanted 2`
- **WHEN** `dots adopt` is run for that setting
- **THEN** the package's `value` becomes `6` and the setting is `ok`

#### Scenario: Apply restores the declared value
- **WHEN** the setting has drifted and `dots apply` runs
- **THEN** the declared value is written again

### Requirement: Ownership and restore
The first time dots changes a key, it SHALL record the original value in the ownership ledger, or record that the key did not exist. Removing the declaration SHALL restore the original value, or delete a key that did not exist. `dots restore <run-id>` SHALL restore settings changed by that run.

#### Scenario: Removed declaration restores the original
- **WHEN** dots changed `KeyRepeat` from `6` to `2`, and the declaration is later removed
- **THEN** the next apply writes `6` back

#### Scenario: Key did not exist
- **WHEN** dots created a key that did not exist, and the declaration is removed
- **THEN** the key is deleted

#### Scenario: Restore a run
- **WHEN** `dots restore <run-id>` runs for a run that changed two settings
- **THEN** both settings return to the values they had before that run

### Requirement: Restarts are aggregated
Apps named in `restart` SHALL be collected across the whole run and restarted once each at the end. A setting that only takes effect after logout SHALL create an auto-closing manual action instead of failing silently.

#### Scenario: Restart aggregation
- **WHEN** three changed `[[defaults]]` entries in different packages each list `restart = ["Dock"]`
- **THEN** the Dock is restarted exactly once, after the last change

#### Scenario: Needs logout
- **WHEN** a changed setting only takes effect after logout
- **THEN** a manual action with a logout badge is created, and it closes automatically once the check passes

### Requirement: Root settings
An entry with `root = true` SHALL run with sudo, as part of the run's one sudo prompt.

#### Scenario: Root defaults share the prompt
- **WHEN** a run has one `root = true` defaults entry and an apt install
- **THEN** the user is asked for the sudo password once

### Requirement: Registry and xdg
`[[registry]]` SHALL write the value with its declared `type`. `[[xdg]]` with `mime` SHALL run `xdg-mime default <app> <mime>` for each type, and `setting` SHALL use `xdg-settings`.

#### Scenario: Registry value
- **WHEN** a Windows package declares `key = 'HKCU\Control Panel\Keyboard'`, `name = "KeyboardDelay"`, `type = "REG_SZ"`, `value = "0"`
- **THEN** the value is read first, and written only when it differs

#### Scenario: Default markdown app
- **WHEN** an `[[xdg]]` entry has `mime = ["text/markdown"]` and `app = "gh-markdown-preview.desktop"`
- **THEN** `xdg-mime query default text/markdown` is compared, and `xdg-mime default` runs only when it differs

### Requirement: Out of scope settings
Settings that need MDM or SIP changes SHALL NOT be declarable. They SHALL be manual actions.

#### Scenario: Pointer to a manual action
- **WHEN** a package needs a SIP change
- **THEN** it ships a `manual/*.md` action, and no `[[defaults]]` entry is used
