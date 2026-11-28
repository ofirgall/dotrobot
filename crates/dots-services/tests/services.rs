//! Every `services` scenario at the crate level, against scripted `brew`, `launchctl`,
//! `systemctl` and `reg` output. Nothing touches the machine.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use dots_providers::Output;
use dots_services::{
    Health, Manager, Run, Service, State, Sys, Tools, apply, check_names, observe, plan,
    select_manager, status, unit_dir, unit_dst,
};
use dots_testkit::RecordingRunner;

fn svc(package: &str, name: &str, manager: Manager) -> Service {
    Service {
        package: package.into(),
        name: name.into(),
        manager,
        enabled: true,
        running: true,
        unit: None,
    }
}

fn sys(r: &RecordingRunner) -> Sys<'_> {
    Sys {
        runner: r,
        tools: Tools::default(),
        uid: 501,
        home: PathBuf::from("/home/u"),
    }
}

fn none() -> BTreeSet<String> {
    BTreeSet::new()
}

fn changed(p: &str) -> BTreeSet<String> {
    BTreeSet::from([p.to_string()])
}

fn states(v: &[(&Service, bool, Run)]) -> BTreeMap<String, State> {
    v.iter()
        .map(|(s, enabled, run)| {
            (
                s.key(),
                State {
                    enabled: *enabled,
                    run: *run,
                    loaded: *enabled,
                },
            )
        })
        .collect()
}

fn show(id: &str, active: &str, file: &str) -> String {
    format!("Id={id}\nLoadState=loaded\nActiveState={active}\nUnitFileState={file}\n")
}

// ------------------------------------------------------------------ declaration and manager

#[test]
fn unit_file_is_linked_into_the_managers_unit_directory() {
    assert_eq!(
        unit_dst(
            Manager::SystemdUser,
            std::path::Path::new("/h"),
            "services/autolock.service"
        ),
        Some(PathBuf::from("/h/.config/systemd/user/autolock.service"))
    );
    assert_eq!(
        unit_dir(Manager::SystemdUser),
        Some("~/.config/systemd/user")
    );
    assert_eq!(
        unit_dst(Manager::Launchd, std::path::Path::new("/h"), "x/a.plist"),
        Some(PathBuf::from("/h/Library/LaunchAgents/a.plist"))
    );
    assert_eq!(
        unit_dst(Manager::Brew, std::path::Path::new("/h"), "x"),
        None
    );
}

#[test]
fn an_unknown_manager_fails_parsing_and_lists_the_valid_ones() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("dots.toml"), "schema = 1\nname = \"t\"\n").unwrap();
    std::fs::create_dir_all(tmp.path().join("packages/p")).unwrap();
    std::fs::write(
        tmp.path().join("packages/p/package.toml"),
        "schema = 1\nname = \"p\"\n[service]\nname = \"p\"\nmanager = \"runit\"\n",
    )
    .unwrap();
    let e = dots_model::Repo::load(tmp.path()).expect_err("fails");
    let d = &e.errors[0];
    let text = format!("{} {}", d.message, d.hint);
    for m in ["auto", "brew", "launchd", "systemd-user", "windows-startup"] {
        assert!(text.contains(m), "{text}");
    }
    // and unknown keys in [service] are errors too
    std::fs::write(
        tmp.path().join("packages/p/package.toml"),
        "schema = 1\nname = \"p\"\n[service]\nname = \"p\"\nrunning_ = true\n",
    )
    .unwrap();
    let e = dots_model::Repo::load(tmp.path()).expect_err("fails");
    assert_eq!(e.errors[0].code, "schema-unknown-key");
}

#[test]
fn service_table_parses_with_defaults() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("dots.toml"), "schema = 1\nname = \"t\"\n").unwrap();
    std::fs::create_dir_all(tmp.path().join("packages/p/services")).unwrap();
    std::fs::write(tmp.path().join("packages/p/services/a.service"), "").unwrap();
    std::fs::write(
        tmp.path().join("packages/p/package.toml"),
        "schema = 1\nname = \"p\"\n[service]\nname = \"a\"\nunit = \"services/a.service\"\n",
    )
    .unwrap();
    let repo = dots_model::Repo::load(tmp.path()).unwrap().value;
    let s = repo.packages["p"].service.clone().unwrap();
    assert_eq!(
        (s.manager.as_str(), s.enabled, s.running),
        ("auto", true, true)
    );
}

#[test]
fn auto_picks_the_manager_for_the_platform() {
    // brew services when brew installed it on macOS
    assert_eq!(
        select_manager("macos", "auto", Some("brew")),
        Some(Manager::Brew)
    );
    // launchd with an owned plist for other macOS services
    assert_eq!(
        select_manager("macos", "auto", Some("script")),
        Some(Manager::Launchd)
    );
    assert_eq!(
        select_manager("macos", "auto", None),
        Some(Manager::Launchd)
    );
    // systemd --user on Linux, even when brew installed it
    assert_eq!(
        select_manager("linux", "auto", Some("brew")),
        Some(Manager::SystemdUser)
    );
    assert_eq!(
        select_manager("linux", "auto", None),
        Some(Manager::SystemdUser)
    );
    assert_eq!(
        select_manager("windows", "auto", None),
        Some(Manager::WindowsStartup)
    );
    // an explicit manager of another OS is inert
    assert_eq!(select_manager("linux", "launchd", None), None);
    assert_eq!(select_manager("macos", "systemd-user", None), None);
    assert_eq!(
        select_manager("macos", "brew", Some("script")),
        Some(Manager::Brew)
    );
}

// ------------------------------------------------------------------ auto selection plans

#[test]
fn brew_installed_service_on_macos_plans_brew_services_start() {
    let r = RecordingRunner::new();
    r.on(
        &["/opt/homebrew/bin/brew", "services", "list", "--json"],
        Output::ok(r#"[{"name":"sketchybar","status":"none","user":null,"file":null}]"#),
    );
    let s = svc("sketchybar", "sketchybar", Manager::Brew);
    let st = observe(&sys(&r), std::slice::from_ref(&s)).unwrap();
    let steps = plan(&sys(&r), std::slice::from_ref(&s), &st, &none());
    assert_eq!(steps.len(), 1);
    assert_eq!(
        steps[0].argv,
        [["/opt/homebrew/bin/brew", "services", "start", "sketchybar"]]
    );
}

#[test]
fn linux_service_plans_systemctl_enable_now() {
    let r = RecordingRunner::new();
    r.on(
        &["/usr/bin/systemctl", "--user", "show"],
        Output::ok(&show("autolock.service", "inactive", "disabled")),
    );
    let mut s = svc("autolock", "autolock", Manager::SystemdUser);
    s.unit = Some("services/autolock.service".into());
    let st = observe(&sys(&r), std::slice::from_ref(&s)).unwrap();
    let steps = plan(&sys(&r), std::slice::from_ref(&s), &st, &none());
    assert_eq!(steps.len(), 1);
    let cmds: Vec<String> = steps[0].argv.iter().map(|a| a.join(" ")).collect();
    // the unit was just linked, so systemd rereads its units first
    assert_eq!(
        cmds,
        [
            "/usr/bin/systemctl --user daemon-reload",
            "/usr/bin/systemctl --user enable --now -- autolock.service"
        ]
    );
    assert_eq!(steps[0].action, "enable+start");
}

#[test]
fn windows_startup_enables_through_the_run_key() {
    let r = RecordingRunner::new();
    r.on(
        &["C:\\Windows\\System32\\reg.exe", "query"],
        Output::ok("\nHKEY\n"),
    );
    r.on(
        &["C:\\Windows\\System32\\tasklist.exe"],
        Output::ok("\"explorer.exe\",\"1\"\n"),
    );
    let s = svc("komorebi", "komorebi", Manager::WindowsStartup);
    let st = observe(&sys(&r), std::slice::from_ref(&s)).unwrap();
    assert_eq!(st[&s.key()], State::GONE);
    let steps = plan(&sys(&r), std::slice::from_ref(&s), &st, &none());
    let cmds: Vec<String> = steps[0].argv.iter().map(|a| a.join(" ")).collect();
    assert!(
        cmds[0].contains(
            "reg.exe add HKCU\\Software\\Microsoft\\Windows\\CurrentVersion\\Run /v komorebi"
        ),
        "{cmds:?}"
    );
    // never through a shell: no `cmd /c start`
    assert!(!cmds.iter().any(|c| c.contains("cmd")), "{cmds:?}");
    assert!(
        cmds[1].ends_with("-Command Start-Process -FilePath komorebi.exe"),
        "{cmds:?}"
    );
}

// ------------------------------------------------------------------ enabled and running

#[test]
fn linked_but_never_enabled_shows_not_enabled_and_apply_enables() {
    let r = RecordingRunner::new();
    r.on(
        &["/usr/bin/systemctl", "--user", "show"],
        Output::ok(&show("autolock.service", "inactive", "disabled")),
    );
    let mut s = svc("autolock", "autolock", Manager::SystemdUser);
    s.running = false;
    let st = observe(&sys(&r), std::slice::from_ref(&s)).unwrap();
    let rows = status(std::slice::from_ref(&s), &st);
    assert_eq!(rows[0].health, Health::NotEnabled);
    let steps = plan(&sys(&r), std::slice::from_ref(&s), &st, &none());
    assert_eq!(steps[0].action, "enable");
    assert_eq!(
        steps[0].argv,
        [[
            "/usr/bin/systemctl",
            "--user",
            "enable",
            "--",
            "autolock.service"
        ]]
    );
    apply(&sys(&r), &steps[0]).unwrap();
    assert!(
        r.lines()
            .contains(&"/usr/bin/systemctl --user enable -- autolock.service".to_string())
    );
}

#[test]
fn already_enabled_and_running_has_no_step() {
    let r = RecordingRunner::new();
    r.on(
        &["/usr/bin/systemctl", "--user", "show"],
        Output::ok(&show("autolock.service", "active", "enabled")),
    );
    let s = svc("autolock", "autolock", Manager::SystemdUser);
    let st = observe(&sys(&r), std::slice::from_ref(&s)).unwrap();
    assert!(plan(&sys(&r), std::slice::from_ref(&s), &st, &none()).is_empty());
    assert_eq!(status(std::slice::from_ref(&s), &st)[0].health, Health::Ok);
}

#[test]
fn enabled_but_stopped_is_started() {
    let s = svc("a", "a", Manager::SystemdUser);
    let st = states(&[(&s, true, Run::Stopped)]);
    let r = RecordingRunner::new();
    let steps = plan(&sys(&r), std::slice::from_ref(&s), &st, &none());
    assert_eq!(steps[0].action, "start");
    assert_eq!(
        status(std::slice::from_ref(&s), &st)[0].health,
        Health::Stopped
    );
}

// ------------------------------------------------------------------ restart on config change

#[test]
fn config_change_restarts_a_running_service_once() {
    let s = svc("a", "a", Manager::SystemdUser);
    let st = states(&[(&s, true, Run::Running)]);
    let r = RecordingRunner::new();
    // however many files changed, the package is in `changed` once
    let steps = plan(&sys(&r), std::slice::from_ref(&s), &st, &changed("a"));
    assert_eq!(steps.len(), 1);
    assert_eq!(steps[0].action, "restart");
    assert_eq!(
        steps[0].argv,
        [["/usr/bin/systemctl", "--user", "restart", "--", "a.service"]]
    );
}

#[test]
fn nothing_changed_means_no_restart() {
    let s = svc("a", "a", Manager::SystemdUser);
    let st = states(&[(&s, true, Run::Running)]);
    let r = RecordingRunner::new();
    assert!(plan(&sys(&r), std::slice::from_ref(&s), &st, &none()).is_empty());
    // another package's change does not restart this one
    assert!(plan(&sys(&r), std::slice::from_ref(&s), &st, &changed("b")).is_empty());
}

#[test]
fn a_stopped_service_is_not_started_by_a_config_change() {
    let mut s = svc("a", "a", Manager::SystemdUser);
    s.running = false;
    let st = states(&[(&s, true, Run::Stopped)]);
    let r = RecordingRunner::new();
    assert!(plan(&sys(&r), std::slice::from_ref(&s), &st, &changed("a")).is_empty());
    // with running = true it is started, not restarted
    s.running = true;
    let steps = plan(&sys(&r), std::slice::from_ref(&s), &st, &changed("a"));
    assert_eq!(steps[0].action, "start");
}

#[test]
fn running_false_is_never_started_but_a_running_one_is_restarted_on_change() {
    let mut s = svc("a", "a", Manager::SystemdUser);
    s.running = false;
    let r = RecordingRunner::new();
    let st = states(&[(&s, true, Run::Running)]);
    assert!(plan(&sys(&r), std::slice::from_ref(&s), &st, &none()).is_empty());
    let steps = plan(&sys(&r), std::slice::from_ref(&s), &st, &changed("a"));
    assert_eq!(steps[0].action, "restart");
}

#[test]
fn a_changed_unit_reloads_systemd_before_the_restart() {
    let mut s = svc("a", "a", Manager::SystemdUser);
    s.unit = Some("services/a.service".into());
    let st = states(&[(&s, true, Run::Running)]);
    let r = RecordingRunner::new();
    let steps = plan(&sys(&r), std::slice::from_ref(&s), &st, &changed("a"));
    let cmds: Vec<String> = steps[0].argv.iter().map(|a| a.join(" ")).collect();
    assert_eq!(
        cmds,
        [
            "/usr/bin/systemctl --user daemon-reload",
            "/usr/bin/systemctl --user restart -- a.service"
        ]
    );
}

fn launchd_cmds(s: &Service, st: State, ch: &BTreeSet<String>) -> Vec<String> {
    let r = RecordingRunner::new();
    let m = BTreeMap::from([(s.key(), st)]);
    plan(&sys(&r), std::slice::from_ref(s), &m, ch)
        .first()
        .map(|p| p.argv.iter().map(|a| a[1..].join(" ")).collect())
        .unwrap_or_default()
}

const LOADED_RUNNING: State = State {
    enabled: true,
    run: Run::Running,
    loaded: true,
};

#[test]
fn launchd_changed_plist_boots_out_and_back_in_instead_of_kickstart_k() {
    let mut s = svc("a", "com.x.a", Manager::Launchd);
    s.unit = Some("services/com.x.a.plist".into());
    let cmds = launchd_cmds(&s, LOADED_RUNNING, &changed("a"));
    assert_eq!(
        cmds,
        [
            "bootout gui/501/com.x.a",
            &bootstrap_line(),
            "kickstart gui/501/com.x.a",
        ]
    );
}

#[test]
fn launchd_without_a_unit_of_ours_just_kicks_a_changed_job() {
    let s = svc("a", "com.x.a", Manager::Launchd);
    assert_eq!(
        launchd_cmds(&s, LOADED_RUNNING, &changed("a")),
        ["kickstart -k gui/501/com.x.a"]
    );
}

#[test]
fn launchd_bootstraps_only_a_job_that_is_not_loaded_and_before_kickstart() {
    let s = svc("a", "com.x.a", Manager::Launchd);
    // not loaded at all: enable, bootstrap, then kickstart (never the other way round)
    assert_eq!(
        launchd_cmds(&s, State::GONE, &none()),
        [
            "enable gui/501/com.x.a",
            &bootstrap_line(),
            "kickstart gui/501/com.x.a",
        ]
    );
    // loaded and enabled, just stopped: only a kickstart, no second bootstrap
    let stopped = State {
        enabled: true,
        run: Run::Stopped,
        loaded: true,
    };
    assert_eq!(
        launchd_cmds(&s, stopped, &none()),
        ["kickstart gui/501/com.x.a"]
    );
}

#[test]
fn launchd_loaded_but_disabled_is_enabled_without_a_second_bootstrap() {
    let s = svc("a", "com.x.a", Manager::Launchd);
    let disabled = State {
        enabled: false,
        run: Run::Running,
        loaded: true,
    };
    assert_eq!(
        launchd_cmds(&s, disabled, &none()),
        ["enable gui/501/com.x.a"]
    );
}

#[test]
fn launchd_not_enabled_but_wanted_running_is_bootstrapped_before_it_is_kicked() {
    let mut s = svc("a", "com.x.a", Manager::Launchd);
    s.enabled = false;
    let cmds = launchd_cmds(&s, State::GONE, &none());
    assert_eq!(cmds, [&bootstrap_line(), "kickstart gui/501/com.x.a",]);
}

#[test]
fn a_plist_that_changed_while_the_job_is_not_loaded_is_only_bootstrapped() {
    let mut s = svc("a", "com.x.a", Manager::Launchd);
    s.unit = Some("services/com.x.a.plist".into());
    s.running = false;
    let cmds = launchd_cmds(&s, State::GONE, &changed("a"));
    assert!(!cmds.iter().any(|c| c.starts_with("bootout")), "{cmds:?}");
    assert_eq!(
        cmds.iter().filter(|c| c.starts_with("bootstrap")).count(),
        1
    );
}

// ------------------------------------------------------------------ names are never shell input

fn named(name: &str, manager: Manager) -> Service {
    svc("p", name, manager)
}

#[test]
fn hostile_service_names_are_rejected_before_any_command_is_built() {
    for bad in [
        "x & calc", "x|y", "a b", "x;rm", "$(id)", "`id`", "a\"b", "x^y", "-rf", "--user", "",
        "a/b", "x%PATH%",
    ] {
        let e = check_names(&[named(bad, Manager::WindowsStartup)]).unwrap_err();
        assert_eq!(e.name, bad);
        assert_eq!(e.package, "p");
    }
    for good in ["komorebi", "com.example.auto-lock", "a_b@1", "X.Y"] {
        assert!(
            check_names(&[named(good, Manager::WindowsStartup)]).is_ok(),
            "{good}"
        );
    }
}

#[test]
fn the_unit_file_name_and_label_are_checked_too() {
    let mut s = named("fine", Manager::SystemdUser);
    s.unit = Some("services/evil name;x.service".into());
    let e = check_names(&[s]).unwrap_err();
    assert_eq!(e.what, "unit file name");
    let mut s = named("fine", Manager::Launchd);
    s.unit = Some("services/$(x).plist".into());
    assert!(check_names(&[s]).is_err());
}

#[test]
fn systemctl_names_come_after_a_double_dash() {
    let r = RecordingRunner::new();
    r.on(
        &["/usr/bin/systemctl", "--user", "show"],
        Output::ok(&show("a.service", "inactive", "disabled")),
    );
    let s = svc("a", "a", Manager::SystemdUser);
    observe(&sys(&r), std::slice::from_ref(&s)).unwrap();
    let shown = r
        .lines()
        .into_iter()
        .find(|l| l.contains(" show "))
        .unwrap();
    assert!(shown.ends_with("-- a.service"), "{shown}");
    let st = states(&[(&s, false, Run::Stopped)]);
    for step in plan(&sys(&r), std::slice::from_ref(&s), &st, &none()) {
        for a in &step.argv {
            if a.iter().any(|x| x == "a.service") {
                assert_eq!(a[a.len() - 2], "--", "{a:?}");
            }
        }
    }
}

// ------------------------------------------------------------------ dropping a package takes its service down

fn owned_of(s: &Service) -> dots_services::OwnedService {
    dots_services::OwnedService::of(s)
}

#[test]
fn a_service_dots_enabled_is_disabled_when_its_package_goes() {
    let r = RecordingRunner::new();
    let cases = [
        (
            svc("a", "a", Manager::SystemdUser),
            vec!["/usr/bin/systemctl --user disable --now -- a.service"],
        ),
        (
            svc("a", "com.x.a", Manager::Launchd),
            vec!["/bin/launchctl bootout gui/501/com.x.a"],
        ),
        (
            svc("a", "a", Manager::Brew),
            vec!["/opt/homebrew/bin/brew services stop a"],
        ),
        (
            svc("a", "komorebi", Manager::WindowsStartup),
            vec![
                "C:\\Windows\\System32\\taskkill.exe /IM komorebi.exe /F",
                "C:\\Windows\\System32\\reg.exe delete HKCU\\Software\\Microsoft\\Windows\\CurrentVersion\\Run /v komorebi /f",
            ],
        ),
    ];
    for (s, want) in cases {
        let st = states(&[(&s, true, Run::Running)]);
        let steps = dots_services::plan_teardown(&sys(&r), &[owned_of(&s)], &st);
        assert_eq!(steps.len(), 1, "{:?}", s.manager);
        let got: Vec<String> = steps[0].argv.iter().map(|a| a.join(" ")).collect();
        if s.manager != Manager::Brew {
            assert_eq!(got, want, "{:?}", s.manager);
        } else {
            assert!(got[0].ends_with("brew services stop a"), "{got:?}");
        }
        assert_eq!(steps[0].action, "disable");
        assert_eq!(steps[0].release, Some(s.key()));
    }
    // one that is already gone from the manager needs nothing
    let s = svc("a", "a", Manager::SystemdUser);
    let gone = states(&[(&s, false, Run::Stopped)]);
    assert!(dots_services::plan_teardown(&sys(&r), &[owned_of(&s)], &gone).is_empty());
}

#[test]
fn the_ledger_owns_what_dots_enabled_and_forgets_what_it_took_down() {
    let tmp = tempfile::tempdir().unwrap();
    let r = RecordingRunner::new();
    let s = svc("a", "a", Manager::SystemdUser);
    // enabling claims it
    let st = states(&[(&s, false, Run::Stopped)]);
    let steps = plan(&sys(&r), std::slice::from_ref(&s), &st, &none());
    let mut l = dots_services::Ledger::load(tmp.path());
    dots_services::apply_recorded(&sys(&r), &mut l, &steps[0]).unwrap();
    let l2 = dots_services::Ledger::load(tmp.path());
    assert_eq!(l2.owned.keys().collect::<Vec<_>>(), ["a:a"]);
    assert!(tmp.path().join("services.json").is_file());
    // a service that was already enabled is never claimed
    let st = states(&[(&s, true, Run::Stopped)]);
    let steps = plan(&sys(&r), std::slice::from_ref(&s), &st, &none());
    assert!(steps[0].owned.is_none());
    // the package goes: its key is not declared any more
    let orphans = l2.orphans(&BTreeSet::new());
    assert_eq!(orphans.len(), 1);
    assert!(l2.orphans(&BTreeSet::from(["a:a".to_string()])).is_empty());
    let st = states(&[(&s, true, Run::Running)]);
    let down = dots_services::plan_teardown(&sys(&r), &orphans, &st);
    let mut l3 = l2.clone();
    dots_services::apply_recorded(&sys(&r), &mut l3, &down[0]).unwrap();
    assert!(dots_services::Ledger::load(tmp.path()).owned.is_empty());
}

// ------------------------------------------------------------------ per-service config signal

#[test]
fn a_config_signal_can_name_one_service_of_a_package() {
    let a = svc("p", "a", Manager::SystemdUser);
    let b = svc("p", "b", Manager::SystemdUser);
    let st = states(&[(&a, true, Run::Running), (&b, true, Run::Running)]);
    let r = RecordingRunner::new();
    let steps = plan(
        &sys(&r),
        &[a.clone(), b.clone()],
        &st,
        &BTreeSet::from([a.key()]),
    );
    assert_eq!(steps.len(), 1);
    assert_eq!(steps[0].name, "a");
}

#[test]
fn a_changed_linked_source_changes_the_config_fingerprint() {
    let tmp = tempfile::tempdir().unwrap();
    let f = tmp.path().join("files/a.conf");
    std::fs::create_dir_all(f.parent().unwrap()).unwrap();
    std::fs::write(&f, "1").unwrap();
    let srcs = vec![f.clone()];
    let h1 = dots_services::config_hash(tmp.path(), &srcs);
    assert_eq!(h1, dots_services::config_hash(tmp.path(), &srcs));
    std::fs::write(&f, "2").unwrap();
    let h2 = dots_services::config_hash(tmp.path(), &srcs);
    assert_ne!(h1, h2);
    // the ledger reports only services that have a baseline and differ from it
    let mut l = dots_services::Ledger::load(tmp.path());
    let now = BTreeMap::from([("p:a".to_string(), h2.clone()), ("p:b".to_string(), h2)]);
    assert!(l.config_changed(&now).is_empty(), "no baseline, no signal");
    l.config.insert("p:a".into(), h1);
    assert_eq!(
        l.config_changed(&now).into_iter().collect::<Vec<_>>(),
        ["p:a"]
    );
}

// ------------------------------------------------------------------ status

#[test]
fn four_systemd_services_are_read_with_one_call() {
    let r = RecordingRunner::new();
    let names = ["a", "b", "c", "d"];
    let out: String = names
        .iter()
        .map(|n| show(&format!("{n}.service"), "active", "enabled"))
        .collect::<Vec<_>>()
        .join("\n");
    r.on(&["/usr/bin/systemctl", "--user", "show"], Output::ok(&out));
    let svcs: Vec<Service> = names
        .iter()
        .map(|n| svc(n, n, Manager::SystemdUser))
        .collect();
    let st = observe(&sys(&r), &svcs).unwrap();
    assert_eq!(r.count(), 1, "{:?}", r.lines());
    let rows = status(&svcs, &st);
    assert!(rows.iter().all(|x| x.health == Health::Ok));
}

#[test]
fn one_call_per_manager_when_several_are_in_use() {
    let r = RecordingRunner::new();
    r.on(
        &["/opt/homebrew/bin/brew", "services", "list", "--json"],
        Output::ok(r#"[{"name":"a","status":"started"},{"name":"b","status":"error"}]"#),
    );
    r.on(
        &["/bin/launchctl", "print"],
        Output::ok("gui/501 = {\n\tservices = {\n\t\t   9      0 \tc\n\t}\n}\n"),
    );
    let svcs = vec![
        svc("a", "a", Manager::Brew),
        svc("b", "b", Manager::Brew),
        svc("c", "c", Manager::Launchd),
    ];
    let st = observe(&sys(&r), &svcs).unwrap();
    assert_eq!(r.count(), 2);
    assert_eq!(st[&svcs[0].key()].run, Run::Running);
    assert_eq!(st[&svcs[1].key()].run, Run::Failed);
    assert_eq!(st[&svcs[2].key()].run, Run::Running);
}

#[test]
fn a_crashed_service_is_failed_and_not_ok() {
    let r = RecordingRunner::new();
    r.on(
        &["/usr/bin/systemctl", "--user", "show"],
        Output::ok(&show("a.service", "failed", "enabled")),
    );
    let s = svc("a", "a", Manager::SystemdUser);
    let st = observe(&sys(&r), std::slice::from_ref(&s)).unwrap();
    let rows = status(std::slice::from_ref(&s), &st);
    assert_eq!(rows[0].health, Health::Failed);
    assert_eq!(rows[0].state.run.as_str(), "failed");
    // a failed service is started again
    assert_eq!(
        plan(&sys(&r), std::slice::from_ref(&s), &st, &none())[0].why,
        "failed"
    );
}

#[test]
fn launchctl_disabled_and_stopped_states() {
    let text = "gui/501 = {\n\tservices = {\n\t\t   -      0 \tcom.a\n\t\t   -      3 \tcom.b\n\t}\n\tdisabled services = {\n\t\t\"com.a\" => disabled\n\t\t\"com.z\" => enabled\n\t}\n}\n";
    let r = RecordingRunner::new();
    r.on(&["/bin/launchctl", "print"], Output::ok(text));
    let svcs = vec![
        svc("a", "com.a", Manager::Launchd),
        svc("b", "com.b", Manager::Launchd),
        svc("c", "com.c", Manager::Launchd),
    ];
    let st = observe(&sys(&r), &svcs).unwrap();
    assert_eq!(
        st[&svcs[0].key()],
        State {
            enabled: false,
            run: Run::Stopped,
            loaded: true
        }
    );
    assert_eq!(
        st[&svcs[1].key()],
        State {
            enabled: true,
            run: Run::Failed,
            loaded: true
        }
    );
    assert_eq!(st[&svcs[2].key()], State::GONE);
}

#[test]
fn a_manager_that_is_not_installed_yet_reads_as_not_enabled() {
    // brew itself may be installed by this very run
    #[derive(Debug)]
    struct Missing;
    impl dots_providers::Runner for Missing {
        fn run(&self, c: &dots_providers::Cmd) -> dots_providers::Result<Output> {
            Err(dots_providers::ProviderError::Other(format!(
                "cannot run `{}`: not found",
                c.line()
            )))
        }
    }
    let s = svc("a", "a", Manager::Brew);
    let sys = Sys {
        runner: &Missing,
        tools: Tools::default(),
        uid: 1,
        home: PathBuf::new(),
    };
    let st = observe(&sys, std::slice::from_ref(&s)).unwrap();
    assert_eq!(st[&s.key()], State::GONE);
}

#[test]
fn a_failing_step_stops_at_the_first_failed_command() {
    let r = RecordingRunner::new();
    r.on(
        &["/usr/bin/systemctl", "--user", "daemon-reload"],
        Output::fail(1, "no bus"),
    );
    let mut s = svc("a", "a", Manager::SystemdUser);
    s.unit = Some("services/a.service".into());
    let st = states(&[(&s, false, Run::Stopped)]);
    let steps = plan(&sys(&r), std::slice::from_ref(&s), &st, &none());
    let e = apply(&sys(&r), &steps[0]).unwrap_err();
    assert!(e.contains("no bus"), "{e}");
    assert_eq!(r.count(), 1);
}

/// The `bootstrap` command line for the sandbox home's plist; the path is built with the
/// platform's separator.
fn bootstrap_line() -> String {
    let plist = std::path::Path::new("/home/u")
        .join("Library/LaunchAgents")
        .join("com.x.a.plist");
    format!("bootstrap gui/501 {}", plist.display())
}
