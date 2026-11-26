# Spec Delta

## Purpose

Defines how selected bundles become a concrete set of packages: the version grammar, how constraints combine, provider choice, and the hard errors for conflicts, collisions and cycles. The result is predictable and every decision can be explained.

## ADDED Requirements

### Requirement: Resolved package set
The package set SHALL be built in this order:
1. Start from the machine's bundles, expanded through `include`.
2. Add the transitive closure of `requires`.
3. Remove `exclude`.
4. Mark packages whose `when` is false as inert (`n/a`).

#### Scenario: Transitive requirement
- **WHEN** bundle `cli` lists `jless` and `jless` requires `rust >=1.80`
- **THEN** `rust` is in the set, and `dots why rust` shows `machine → cli → jless → requires`

### Requirement: Version grammar
A version spec SHALL be one of:
- `latest`
- an exact version `X.Y.Z`
- a prefix `X` or `X.Y`
- a floor `>=X[.Y[.Z]]`
- `git:<ref>`

Any other form SHALL be a parse error.

#### Scenario: Invalid spec
- **WHEN** a package declares `version = "~1.2"`
- **THEN** loading fails with `version-spec-invalid`, and the hint lists the five accepted forms

### Requirement: Constraint intersection
All constraints on a package SHALL be intersected. They come from its own spec, from every dependent's `requires`, and from overrides. The intersection rules are:
- `latest ∩ X = X`
- two floors give the higher floor
- two prefixes give the longer one when it extends the other
- a prefix and a floor give the prefix when they overlap
- an exact version must satisfy the other side
- `git:` refs must be equal
- `git:` with a numeric spec is a conflict

#### Scenario: Compatible prefix and floor
- **WHEN** the constraints are `1.4` and `>=1.2`
- **THEN** the result is `1.4`

#### Scenario: Conflict with provenance
- **WHEN** `kv` requires `rust >=1.80` and an override pins `rust = "1.75"`
- **THEN** resolution fails with `version-conflict`, listing each side with its file and inclusion path

### Requirement: Overrides replace only the own spec
A `version` in an override SHALL replace the package's own spec only. It SHALL NOT replace the constraints that dependents place on it.

#### Scenario: Override cannot loosen a dependent
- **WHEN** an override sets `rust` to `latest` and `kv` requires `>=1.80`
- **THEN** the effective constraint is `>=1.80`

### Requirement: Provider selection
For each active package, dots SHALL choose a provider as follows:
1. Take the declared providers whose `when` is true.
2. Order them by `prefer`, or else by the priority list of the most specific matching platform key.
3. Pick the first one that is available (`Ready` or `Bootstrap`), can satisfy the intersected constraint, and does not need root when `sudo` is false.

#### Scenario: No sudo falls back
- **WHEN** `sudo` is false and `fd` supports apt and cargo
- **THEN** cargo is chosen, and the plan says apt was skipped because it needs root

#### Scenario: Provider cannot satisfy the constraint
- **WHEN** brew offers fd 10.2.0 and the constraint is `8.7`
- **THEN** brew is skipped with that reason, and the next candidate is used

#### Scenario: Nothing satisfies the constraint
- **WHEN** no candidate can deliver the constraint
- **THEN** resolution fails with `no-provider-satisfies`, listing what each candidate offers

### Requirement: Bootstrap injection
When the chosen provider is in the `Bootstrap` state, the package that provides it SHALL be added as a dependency.

#### Scenario: Cargo without rustup
- **WHEN** a cargo package is selected and cargo is not installed
- **THEN** `rustup` is added to the set and ordered before it

### Requirement: Sticky provider choice
When a package is already installed by any of its candidate providers, that provider SHALL stay selected, even if the priority list now prefers another. Changing provider SHALL require `dots migrate <pkg> --to <provider>`, which uninstalls from the old provider and installs with the new one.

#### Scenario: Existing apt install
- **WHEN** `bat` is installed through apt and the priority list prefers cargo
- **THEN** apt stays selected, and the status hint suggests `dots migrate bat --to cargo`

### Requirement: Binary collisions
Two resolved packages claiming the same binary name or shim SHALL be an error (`binary-collision`), unless:
- one declares `conflicts`, which makes selecting both the error; or
- one declares `replaces`, which drops the other from the set and uninstalls it if dots owns it.

#### Scenario: Replaces
- **WHEN** `neovim` declares `replaces = ["vim"]` and both are selected
- **THEN** `vim` is removed from the set, and the plan uninstalls it if dots installed it

### Requirement: Cycles are errors
A cycle in `requires`, in `after`, or in implicit setting references SHALL fail with `dependency-cycle`, printing the cycle.

#### Scenario: Two-node cycle
- **WHEN** `a` requires `b` and `b` requires `a`
- **THEN** the error prints `a → b → a`

### Requirement: Orphans and unmanaged
Only packages that dots installed (according to the journal) and that are no longer declared SHALL be reported as `orphan`. Software installed outside dots SHALL be `unmanaged`, and shown only with `--all`.

#### Scenario: Dependency installed by brew
- **WHEN** brew installed `oniguruma` as a dependency of `jq`
- **THEN** it is not an orphan
