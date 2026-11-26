# Spec Delta

## Purpose

Defines how browser extensions are installed and tracked as ordinary packages through the `browser-ext` provider. It works by writing the files and policies browsers already read, with no browser automation.

## ADDED Requirements

### Requirement: Extension package keys
`[providers.browser-ext]` SHALL accept these keys:
- per-browser ids: `chrome`, `edge`, `firefox`, `brave`, `chromium`
- `browsers`: defaults to every supported browser found on the machine
- `profiles`: defaults to every profile
- `method`: `external` (default) or `policy`
- `[providers.browser-ext.managed]`

#### Scenario: Chromium id reuse
- **WHEN** only `chrome` is given and `browsers = ["chrome", "brave"]`
- **THEN** the Chrome id is used for Brave too

### Requirement: Install methods
- `external` SHALL write the browser's external-extension file or registry key.
- `policy` SHALL add the extension to a single dots-owned policy file per browser.

Selecting `external` for Firefox SHALL be a resolve error that suggests `policy`.

#### Scenario: Firefox external
- **WHEN** a package targets Firefox with `method = "external"`
- **THEN** resolution fails with `method-unsupported`

#### Scenario: Shared policy file
- **WHEN** two extension packages use `policy` for Chrome
- **THEN** one dots-owned policy file lists both, and policies dots does not own are left untouched

### Requirement: Installed and pending state
dots SHALL read installed extensions from each profile's preferences (Chromium) or `extensions.json` (Firefox). Until the profile shows the extension, `apply` and `status` SHALL report `pending-restart`.

#### Scenario: Before browser restart
- **WHEN** apply has just written the external-extension file
- **THEN** status shows `↻ pending-restart` for that browser

### Requirement: Enable step is tracked
For the `external` method on Chromium, dots SHALL attach a manual action "Enable <name> in <browser>". Its check reads the profile's enabled state, so the todo closes itself.

#### Scenario: User enables the extension
- **WHEN** the extension becomes enabled in the profile
- **THEN** `dots todo check` marks the action `auto`

### Requirement: Versions and removal
Only `latest` SHALL be a valid version; any other spec is a lint error. Removing the package SHALL delete only the files and policy entries dots owns.

#### Scenario: Pinned extension
- **WHEN** an extension package declares `version = "1.2"`
- **THEN** lint fails with `version-unsupported`
