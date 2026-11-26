# Spec Delta

## Purpose

Defines `[service]`: a package declares that a background service is enabled and running, and dots reconciles it through the right manager, restarts it when its config changes, and reports its state.

## ADDED Requirements

### Requirement: Service declaration
A package SHALL be able to declare one `[service]` with `name`, `manager`, `enabled`, `running` and, for `launchd` and `systemd-user`, `unit`. `manager` SHALL be one of `auto`, `brew`, `launchd`, `systemd-user` or `windows-startup`. `unit` SHALL name a file in the package, linked into the manager's unit directory as an owned file. Unknown keys SHALL be errors.

#### Scenario: Unit file linked
- **WHEN** a package declares `unit = "services/autolock.service"` with `manager = "systemd-user"`
- **THEN** the file is linked into `~/.config/systemd/user/` and enabled

#### Scenario: Unknown manager
- **WHEN** `manager = "runit"`
- **THEN** parsing fails and lists the valid managers

### Requirement: Manager auto-selection
With `manager = "auto"`, dots SHALL choose:
- `brew services` when the package was installed by brew on macOS;
- `launchd`, with an owned `LaunchAgents` plist, for other macOS services;
- `systemd --user` on Linux;
- the Startup folder, or the tool's own autostart (such as `komorebic enable-autostart`), on Windows.

#### Scenario: Brew-installed service on macOS
- **WHEN** `sketchybar` is installed by brew and its service has `manager = "auto"`
- **THEN** the plan shows `brew services start sketchybar`

#### Scenario: Linux
- **WHEN** a service has `manager = "auto"` on Ubuntu
- **THEN** the plan shows `systemctl --user enable --now`

### Requirement: Enabled and running
`enabled = true` SHALL make the service start at login. `running = true` SHALL make it run now. dots SHALL compare the current state first and act only on the difference.

#### Scenario: Linked but never enabled
- **WHEN** the unit file is linked but the service is not enabled
- **THEN** status shows the service as not enabled, and apply enables it

#### Scenario: Already running
- **WHEN** the service is enabled and running
- **THEN** the plan has no service step

### Requirement: Order
Service actions SHALL run after file and settings actions and before hooks, so a service starts with its config in place.

#### Scenario: Start after config
- **WHEN** a package links a config and declares a service
- **THEN** the service starts after the config is placed

### Requirement: Restart on config change
A changed config file or unit of the package SHALL restart the service once per run, however many of its files changed. A service that is not running SHALL NOT be started by a config change unless `running = true`.

#### Scenario: Service config change restarts once
- **WHEN** two config files of the package change in one run
- **THEN** the service is restarted exactly once, after both are written

#### Scenario: Nothing changed
- **WHEN** no file or unit changed
- **THEN** the service is not restarted

### Requirement: Service status
`status` SHALL show `running`, `stopped` or `failed`, read with one call per manager (`brew services list --json`, `launchctl print gui/<uid>`, `systemctl --user show`).

#### Scenario: One call per manager
- **WHEN** four systemd user services are declared
- **THEN** their state is read with a single `systemctl --user show` call

#### Scenario: Failed service
- **WHEN** the service has crashed
- **THEN** status shows `failed`, and the package is not `ok`
