# Spec Delta

## Purpose

Lets a user change anything a repo defines without editing it: their own items, copied items and followed items. Every object has an address and there is one merge grammar. Precedence is fixed, and the result can be inspected, edited, diffed and ejected.

## ADDED Requirements

### Requirement: Addresses
Every object SHALL have an address:
- `pkg:<name>` or `<alias>:<name>`
- `pkg:<name>/file:<dst>`, `pkg:<name>/setting:<key>`, `pkg:<name>/provider:<id>`, `pkg:<name>/manual:<id>`, `pkg:<name>/hook:<event>`
- `bundle:<name>`, `bundle:<name>/member:<item>`
- `provider:<id>`, `priority:<key>`
- `repo:tags`, `repo:paths`, `repo:platforms`, `repo:env`
- `upstream:<alias>`

#### Scenario: Show by address
- **WHEN** `dots show pkg:ghostty/setting:font_size` runs
- **THEN** that setting's definition is printed

### Requirement: Override grammar
An override SHALL be a partial copy of its target:
- scalars replace;
- tables deep-merge;
- arrays replace;
- `{add, remove, prepend}` edits a list;
- keyed entries (files by `dst`, settings, manual actions, providers, members) merge by key, and `remove = true` deletes one;
- `[platform."<key>"]` scopes a change to one platform;
- `when` replaces the target's condition.

`name`, `schema` and `origin` SHALL NOT be overridable.

#### Scenario: Edit a bundle's list
- **WHEN** `overrides/bundles/cli.toml` has `packages = { add = ["zoxide"], remove = ["jless"] }`
- **THEN** the effective `cli` bundle contains zoxide and not jless

#### Scenario: Identity field
- **WHEN** an override sets `name = "x"`
- **THEN** loading fails with `override-identity-field`

### Requirement: Locations and precedence
Overrides SHALL be read from:
- `overrides/`
- `machines/<name>/overrides/`
- `~/.config/dots/overrides/`

Each location uses the same structure: `dots.toml`, `packages/<pkg>/override.toml` with optional extra files, and `bundles/<name>.toml`. They SHALL apply in this order:
1. upstream chain
2. definition
3. platform tables
4. repo overrides
5. machine-profile overrides
6. machine overrides

#### Scenario: Machine-only tweak
- **WHEN** a value is overridden in both the repo and `~/.config/dots/overrides/`
- **THEN** the machine value wins, and the repo file stays unchanged

### Requirement: Effective view with provenance
`dots show <address> --effective` SHALL print the final object. Each key SHALL be annotated with the layer that set it.

#### Scenario: Annotated output
- **WHEN** a key comes from a repo override
- **THEN** that key is annotated `repo override (overrides/packages/x/override.toml)`

### Requirement: Override commands
The `override` command SHALL provide:
- `dots override list`: every override, grouped by scope;
- `dots override edit <address> [--machine]`: creates the right file and opens `$EDITOR`;
- `dots override diff <address>`: the structural diff between the original and the effective object.

#### Scenario: Scaffold
- **WHEN** `dots override edit pkg:delta --machine` runs and no override exists
- **THEN** `~/.config/dots/overrides/packages/delta/override.toml` is created and opened

### Requirement: Stale and redundant overrides
An override whose target no longer exists SHALL be the error `stale-override`, with a hint naming the closest existing address. An override that sets the same value as the layers beneath it SHALL be a warning.

#### Scenario: Renamed upstream file
- **WHEN** upstream renames a file entry that an override targets
- **THEN** lint and pull fail with `stale-override` and suggest the new address

### Requirement: Status shows customization
`dots status` SHALL mark overridden items with the scopes that change them.

#### Scenario: Overridden package
- **WHEN** ghostty has a repo override and a machine override
- **THEN** its row shows `✎ overridden (repo, machine)`

### Requirement: Eject
`dots eject <address>` SHALL:
- write the effective definition, files included, as a local item;
- record `[origin]`;
- remove the overrides that were folded in.

#### Scenario: Eject a followed package
- **WHEN** `dots eject ofirgall:ghostty` runs
- **THEN** `packages/ghostty/` contains the effective definition, and it is no longer imported from the upstream
