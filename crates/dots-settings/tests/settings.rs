//! Every `system-settings` scenario against in-memory `defaults`, registry and `xdg-mime`
//! backends. Nothing touches the machine.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Mutex;

use dots_providers::{Cmd, Output, Result as PResult, Runner};
use dots_settings::{
    Declared, Health, Ledger, Op, Setting, Sudo, Sys, Tools, adopt, apply_step, declared, needs,
    plan, plist, restarts, restore_run, status,
};
use serde_json::{Map, Value, json};

// ------------------------------------------------------------------ fake machine

#[derive(Default)]
struct Machine {
    /// domain -> top-level dict
    defaults: BTreeMap<String, Map<String, Value>>,
    /// key -> lowercase name -> (type, data)
    registry: BTreeMap<String, BTreeMap<String, (String, String)>>,
    xdg: BTreeMap<String, String>,
    calls: Vec<String>,
    /// `sudo` itself fails (no credential).
    sudo_fails: bool,
    /// `defaults export` of a domain nobody wrote fails ("Domain ... does not exist") instead of
    /// printing an empty dict (older macOS, `defaults read` wording).
    strict_domains: bool,
}

#[derive(Default)]
struct Fake(Mutex<Machine>);

impl std::fmt::Debug for Fake {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Fake")
    }
}

impl Fake {
    fn lines(&self) -> Vec<String> {
        self.0.lock().unwrap().calls.clone()
    }

    fn clear(&self) {
        self.0.lock().unwrap().calls.clear();
    }

    fn count(&self, prefix: &str) -> usize {
        self.lines()
            .iter()
            .filter(|l| l.starts_with(prefix))
            .count()
    }

    fn set_defaults(&self, domain: &str, v: Value) {
        self.0
            .lock()
            .unwrap()
            .defaults
            .insert(domain.into(), v.as_object().unwrap().clone());
    }

    fn set_root_defaults(&self, domain: &str, v: Value) {
        self.set_defaults(&format!("root:{domain}"), v);
    }

    fn root_defaults(&self, domain: &str) -> Value {
        self.defaults(&format!("root:{domain}"))
    }

    fn defaults(&self, domain: &str) -> Value {
        Value::Object(
            self.0
                .lock()
                .unwrap()
                .defaults
                .get(domain)
                .cloned()
                .unwrap_or_default(),
        )
    }
}

impl Runner for Fake {
    fn run(&self, cmd: &Cmd) -> PResult<Output> {
        let mut m = self.0.lock().unwrap();
        let mut a = cmd.argv();
        m.calls.push(a.join(" "));
        // `sudo -n` runs the rest as root: root's preferences are a separate set of domains.
        let mut root = false;
        if a[0].ends_with("sudo") {
            if m.sudo_fails {
                return Ok(Output::fail(1, "sudo: a password is required"));
            }
            a.drain(..2);
            root = true;
        }
        let scope = |d: &str| format!("{}{d}", if root { "root:" } else { "" });
        let prog = a[0].rsplit(['/', '\\']).next().unwrap().to_string();
        let args = &a[1..];
        Ok(match (prog.as_str(), args.first().map(String::as_str)) {
            ("defaults", Some("export"))
                if m.strict_domains && !m.defaults.contains_key(&scope(&args[1])) =>
            {
                Output::fail(1, &format!("Domain {} does not exist", args[1]))
            }
            ("defaults", Some("export")) => {
                let d = m
                    .defaults
                    .get(&scope(&args[1]))
                    .cloned()
                    .unwrap_or_default();
                Output::ok(&format!(
                    "<?xml version=\"1.0\"?>\n<plist version=\"1.0\">{}</plist>",
                    plist::fragment(&Value::Object(d))
                ))
            }
            ("defaults", Some("write")) => {
                let domain = scope(&args[1]);
                let key = args[2].clone();
                let d = m.defaults.entry(domain).or_default();
                if args[3] == "-dict-add" {
                    let v = plist::parse(&args[5]).unwrap();
                    let top = d.entry(key).or_insert_with(|| json!({}));
                    top.as_object_mut().unwrap().insert(args[4].clone(), v);
                } else {
                    d.insert(key, plist::parse(&args[3]).unwrap());
                }
                Output::ok("")
            }
            ("defaults", Some("delete")) => {
                if let Some(d) = m.defaults.get_mut(&scope(&args[1])) {
                    d.remove(&args[2]);
                }
                Output::ok("")
            }
            ("xdg-mime", Some("query")) => Output::ok(&format!(
                "{}\n",
                m.xdg.get(&args[2]).cloned().unwrap_or_default()
            )),
            ("xdg-mime", Some("default")) => {
                m.xdg.insert(args[2].clone(), args[1].clone());
                Output::ok("")
            }
            ("reg.exe", Some("query")) => match m.registry.get(&args[1]) {
                None => Output::fail(
                    1,
                    "ERROR: The system was unable to find the specified registry key or value.",
                ),
                Some(vals) => {
                    let mut s = format!("\n{}\n", args[1]);
                    for (n, (t, d)) in vals {
                        let shown = if t == "REG_DWORD" {
                            format!("0x{:x}", d.parse::<u64>().unwrap())
                        } else {
                            d.clone()
                        };
                        s.push_str(&format!("    {n}    {t}    {shown}\n"));
                    }
                    Output::ok(&s)
                }
            },
            ("reg.exe", Some("add")) => {
                // add KEY /v NAME /t TYPE /d DATA /f
                let (k, n, t, d) = (&args[1], &args[3], &args[5], &args[7]);
                m.registry
                    .entry(k.clone())
                    .or_default()
                    .insert(n.to_ascii_lowercase(), (t.clone(), d.clone()));
                Output::ok("")
            }
            ("reg.exe", Some("delete")) => {
                if let Some(v) = m.registry.get_mut(&args[1]) {
                    v.remove(&args[3].to_ascii_lowercase());
                }
                Output::ok("")
            }
            ("killall", _) => Output::ok(""),
            _ => Output::fail(127, "unknown command"),
        })
    }
}

fn sys(f: &Fake) -> Sys<'_> {
    Sys {
        runner: f,
        tools: Tools::default(),
        sudo: Sudo::Primed,
    }
}

// ------------------------------------------------------------------ declarations

fn repo_with(body: &str) -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    std::fs::write(root.join("dots.toml"), "schema = 1\nname = \"t\"\n").unwrap();
    std::fs::create_dir_all(root.join("packages/p")).unwrap();
    std::fs::write(
        root.join("packages/p/package.toml"),
        format!("schema = 1\nname = \"p\"\n{body}"),
    )
    .unwrap();
    tmp
}

fn load_pkg(toml: &str) -> dots_model::Package {
    let tmp = repo_with(toml);
    dots_model::Repo::load(tmp.path())
        .unwrap_or_else(|e| panic!("{:?}", e.errors))
        .value
        .packages["p"]
        .clone()
}

fn decl(toml: &str, os: &str) -> Vec<Declared> {
    declared("p", &load_pkg(toml), os).unwrap().0
}

fn one(toml: &str, os: &str) -> Declared {
    let mut d = decl(toml, os);
    assert_eq!(d.len(), 1);
    d.remove(0)
}

fn orphans(ledger: &Ledger, d: &[Declared]) -> Vec<dots_settings::Owned> {
    ledger
        .owned
        .values()
        .filter(|o| !d.iter().any(|x| x.setting.id() == o.setting.id()))
        .cloned()
        .collect()
}

fn run_all(f: &Fake, ledger: &mut Ledger, d: &[Declared], run: &str) -> Vec<dots_settings::Step> {
    let (steps, _) = plan(&sys(f), d, &orphans(ledger, d)).unwrap();
    for s in &steps {
        apply_step(&sys(f), ledger, run, s).unwrap();
    }
    steps
}

const KEYREPEAT: &str =
    "[[defaults]]\ndomain = \"NSGlobalDomain\"\nkey = \"KeyRepeat\"\nvalue = 2\n";

fn fresh() -> (tempfile::TempDir, Ledger) {
    let tmp = tempfile::tempdir().unwrap();
    let l = Ledger::load(tmp.path());
    (tmp, l)
}

fn ledger_at(dir: &Path) -> Ledger {
    Ledger::load(dir)
}

// ------------------------------------------------------------------ declaration

#[test]
fn defaults_entry_is_inert_on_linux() {
    let (active, inert) = declared("p", &load_pkg(KEYREPEAT), "linux").unwrap();
    assert!(active.is_empty());
    assert_eq!(inert[0].0, "defaults:NSGlobalDomain:KeyRepeat");
    let (active, inert) = declared("p", &load_pkg(KEYREPEAT), "macos").unwrap();
    assert_eq!((active.len(), inert.len()), (1, 0));
}

#[test]
fn unknown_key_is_a_parse_error_with_a_hint() {
    let tmp = repo_with("[[defaults]]\ndomain = \"d\"\nkey = \"k\"\nvaleu = 2\n");
    let e = dots_model::Repo::load(tmp.path()).expect_err("fails");
    let d = &e.errors[0];
    assert_eq!(d.code, "schema-unknown-key");
    assert!(d.hint.contains("value"), "{}", d.hint);
    assert_eq!(d.provenance[0].line, Some(6));
}

#[test]
fn other_tables_reject_unknown_keys_too() {
    for body in [
        "[[registry]]\nkey = 'HKCU\\x'\nname = 'n'\nvlaue = 1\n",
        "[[xdg]]\nmime = ['a/b']\nap = 'x'\n",
    ] {
        let tmp = repo_with(body);
        let e = dots_model::Repo::load(tmp.path()).expect_err("fails");
        assert_eq!(e.errors[0].code, "schema-unknown-key", "{body}");
    }
}

#[test]
fn xdg_entry_needs_mime_and_app_or_setting_and_value() {
    let pkg = load_pkg("[[xdg]]\nmime = ['a/b']\nsetting = 's'\n");
    assert!(declared("p", &pkg, "linux").is_err());
}

// ------------------------------------------------------------------ dotted key

const HOTKEYS: &str = "[[defaults]]\ndomain = \"com.apple.symbolichotkeys\"\nkey = \"AppleSymbolicHotKeys.64\"\nvalue = { enabled = false }\n";

#[test]
fn dotted_key_edits_one_entry_of_a_dict() {
    let f = Fake::default();
    f.set_defaults(
        "com.apple.symbolichotkeys",
        json!({"AppleSymbolicHotKeys": {
            "64": {"enabled": true, "value": {"parameters": [32, 49, 1048576]}},
            "65": {"enabled": true}
        }}),
    );
    let d = decl(HOTKEYS, "macos");
    let (_t, mut l) = fresh();
    let steps = run_all(&f, &mut l, &d, "r1");
    assert_eq!(steps.len(), 1);
    // one entry is written, with `-dict-add`, not the whole dict
    let write = f
        .lines()
        .into_iter()
        .find(|l| l.contains(" write "))
        .unwrap();
    assert!(
        write.contains("AppleSymbolicHotKeys -dict-add 64 "),
        "{write}"
    );
    let now = f.defaults("com.apple.symbolichotkeys");
    assert_eq!(now["AppleSymbolicHotKeys"]["65"], json!({"enabled": true}));
    assert_eq!(now["AppleSymbolicHotKeys"]["64"]["enabled"], json!(false));
    // keys of the entry that the declaration does not mention are kept
    assert_eq!(
        now["AppleSymbolicHotKeys"]["64"]["value"],
        json!({"parameters": [32, 49, 1048576]})
    );
    // and it converged
    let (again, _) = plan(&sys(&f), &d, &[]).unwrap();
    assert!(again.is_empty());
}

#[test]
fn literal_dotted_top_level_key_is_not_split() {
    let f = Fake::default();
    f.set_defaults(
        "NSGlobalDomain",
        json!({"com.apple.swipescrolldirection": true}),
    );
    let d = one(
        "[[defaults]]\ndomain = \"NSGlobalDomain\"\nkey = \"com.apple.swipescrolldirection\"\nvalue = false\n",
        "macos",
    );
    let (steps, _) = plan(&sys(&f), &[d], &[]).unwrap();
    assert_eq!(steps.len(), 1);
    assert!(steps[0].argv[0].contains(&"com.apple.swipescrolldirection".to_string()));
    assert!(!steps[0].argv[0].contains(&"-dict-add".to_string()));
}

#[test]
fn types_come_from_the_toml_value() {
    let f = Fake::default();
    let d = decl(
        "[[defaults]]\ndomain = \"d\"\nkey = \"i\"\nvalue = 2\n[[defaults]]\ndomain = \"d\"\nkey = \"f\"\nvalue = 1.0\n[[defaults]]\ndomain = \"d\"\nkey = \"b\"\nvalue = true\n[[defaults]]\ndomain = \"d\"\nkey = \"s\"\nvalue = \"x\"\n[[defaults]]\ndomain = \"d\"\nkey = \"a\"\nvalue = [1, \"two\"]\n[[defaults]]\ndomain = \"d\"\nkey = \"c\"\nvalue = { red = 1.0, alpha = 1.0 }\n",
        "macos",
    );
    let (_t, mut l) = fresh();
    run_all(&f, &mut l, &d, "r1");
    let now = f.defaults("d");
    assert!(now["i"].is_i64() && now["f"].is_f64() && now["b"].is_boolean());
    assert_eq!(now["a"], json!([1, "two"]));
    assert!(now["c"]["red"].is_f64());
    // an integer on the machine is not a float: type differences are drift
    f.set_defaults("d", json!({"f": 1}));
    let (steps, _) = plan(&sys(&f), &d[1..2], &[]).unwrap();
    assert_eq!(steps.len(), 1);
}

// ------------------------------------------------------------------ read before write

#[test]
fn value_already_set_has_no_step_and_status_ok() {
    let f = Fake::default();
    f.set_defaults("NSGlobalDomain", json!({"KeyRepeat": 2}));
    let d = decl(KEYREPEAT, "macos");
    let (steps, _) = plan(&sys(&f), &d, &[]).unwrap();
    assert!(steps.is_empty());
    let (_t, l) = fresh();
    let rows = status(&sys(&f), &l, &d);
    assert_eq!(rows[0].health, Health::Ok);
    assert_eq!(f.count("/usr/bin/defaults write"), 0);
}

#[test]
fn a_domain_is_read_once_however_many_keys() {
    let f = Fake::default();
    let mut toml = String::new();
    for (k, v) in [("a", 1), ("b", 2), ("c", 3), ("d", 4), ("e", 5)] {
        toml.push_str(&format!(
            "[[defaults]]\ndomain = \"NSGlobalDomain\"\nkey = \"{k}\"\nvalue = {v}\n"
        ));
    }
    let d = decl(&toml, "macos");
    plan(&sys(&f), &d, &[]).unwrap();
    assert_eq!(f.count("/usr/bin/defaults export"), 1);
    let (_t, l) = fresh();
    status(&sys(&f), &l, &d);
    assert_eq!(f.count("/usr/bin/defaults export"), 2);
}

// ------------------------------------------------------------------ drift and adoption

#[test]
fn drift_shows_observed_and_wanted_and_apply_restores() {
    let f = Fake::default();
    let (_t, mut l) = fresh();
    let d = decl(KEYREPEAT, "macos");
    run_all(&f, &mut l, &d, "r1");
    f.set_defaults("NSGlobalDomain", json!({"KeyRepeat": 6}));
    let rows = status(&sys(&f), &l, &d);
    assert_eq!(rows[0].health, Health::Drift);
    assert_eq!(rows[0].observed.as_deref(), Some("6"));
    assert_eq!(rows[0].wanted, "2");
    // apply writes the declared value again
    let steps = run_all(&f, &mut l, &d, "r2");
    assert_eq!(steps.len(), 1);
    assert_eq!(f.defaults("NSGlobalDomain")["KeyRepeat"], json!(2));
}

#[test]
fn adopt_writes_the_observed_value_into_the_package() {
    let f = Fake::default();
    let (tmp, mut l) = fresh();
    let d = decl(KEYREPEAT, "macos");
    run_all(&f, &mut l, &d, "r1");
    f.set_defaults("NSGlobalDomain", json!({"KeyRepeat": 6}));
    let row = &status(&sys(&f), &l, &d)[0];
    let toml = tmp.path().join("package.toml");
    std::fs::write(
        &toml,
        format!("schema = 1\nname = \"p\"\n# keep me\n{KEYREPEAT}"),
    )
    .unwrap();
    adopt(&toml, &row.setting, row.observed_value.as_ref().unwrap()).unwrap();
    let text = std::fs::read_to_string(&toml).unwrap();
    assert!(
        text.contains("value = 6") && text.contains("# keep me"),
        "{text}"
    );
    // the package now wants 6, and the setting is ok
    let repo = repo_with(text.split_once("name = \"p\"\n").unwrap().1);
    let pkg = dots_model::Repo::load(repo.path()).unwrap().value.packages["p"].clone();
    let d2 = declared("p", &pkg, "macos").unwrap().0;
    assert_eq!(status(&sys(&f), &l, &d2)[0].health, Health::Ok);
}

#[test]
fn never_applied_differing_value_is_missing_not_drift() {
    let f = Fake::default();
    f.set_defaults("NSGlobalDomain", json!({"KeyRepeat": 6}));
    let (_t, l) = fresh();
    let rows = status(&sys(&f), &l, &decl(KEYREPEAT, "macos"));
    assert_eq!(rows[0].health, Health::Missing);
}

// ------------------------------------------------------------------ ownership and restore

#[test]
fn removed_declaration_restores_the_original() {
    let f = Fake::default();
    f.set_defaults("NSGlobalDomain", json!({"KeyRepeat": 6}));
    let (tmp, mut l) = fresh();
    let d = decl(KEYREPEAT, "macos");
    run_all(&f, &mut l, &d, "r1");
    assert_eq!(f.defaults("NSGlobalDomain")["KeyRepeat"], json!(2));
    let id = "defaults:NSGlobalDomain:KeyRepeat";
    assert_eq!(l.owned[id].original.value, Some(json!(6)));
    // the ledger is on disk
    assert!(ledger_at(tmp.path()).owned.contains_key(id));
    // a second change keeps the first original
    f.set_defaults("NSGlobalDomain", json!({"KeyRepeat": 9}));
    run_all(&f, &mut l, &d, "r2");
    assert_eq!(l.owned[id].original.value, Some(json!(6)));
    // declaration removed
    let steps = run_all(&f, &mut l, &[], "r3");
    assert_eq!(steps.len(), 1);
    assert_eq!(steps[0].op, Op::Restore);
    assert_eq!(f.defaults("NSGlobalDomain")["KeyRepeat"], json!(6));
    assert!(l.owned.is_empty());
    // and the next plan is empty
    let (steps, _) = plan(&sys(&f), &[], &orphans(&l, &[])).unwrap();
    assert!(steps.is_empty());
}

#[test]
fn a_key_that_did_not_exist_is_deleted_again() {
    let f = Fake::default();
    let (_t, mut l) = fresh();
    let d = decl(KEYREPEAT, "macos");
    run_all(&f, &mut l, &d, "r1");
    assert!(!l.owned["defaults:NSGlobalDomain:KeyRepeat"].original.exists);
    let steps = run_all(&f, &mut l, &[], "r2");
    assert_eq!(steps[0].op, Op::Delete);
    assert!(f.defaults("NSGlobalDomain").get("KeyRepeat").is_none());
}

#[test]
fn a_dotted_entry_that_did_not_exist_is_removed_from_the_dict() {
    let f = Fake::default();
    f.set_defaults(
        "com.apple.symbolichotkeys",
        json!({"AppleSymbolicHotKeys": {"65": {"enabled": true}}}),
    );
    let (_t, mut l) = fresh();
    let d = decl(HOTKEYS, "macos");
    run_all(&f, &mut l, &d, "r1");
    run_all(&f, &mut l, &[], "r2");
    assert_eq!(
        f.defaults("com.apple.symbolichotkeys")["AppleSymbolicHotKeys"],
        json!({"65": {"enabled": true}})
    );
}

#[test]
fn restore_a_run_with_two_settings() {
    let f = Fake::default();
    f.set_defaults("NSGlobalDomain", json!({"KeyRepeat": 6}));
    let (_t, mut l) = fresh();
    let d = decl(
        &format!(
            "{KEYREPEAT}[[defaults]]\ndomain = \"com.apple.dock\"\nkey = \"autohide\"\nvalue = true\n"
        ),
        "macos",
    );
    run_all(&f, &mut l, &d, "run-a");
    assert_eq!(f.defaults("NSGlobalDomain")["KeyRepeat"], json!(2));
    assert_eq!(f.defaults("com.apple.dock")["autohide"], json!(true));
    // dry run touches nothing
    let r = restore_run(&sys(&f), &mut l, "run-a", true)
        .unwrap()
        .unwrap();
    assert_eq!(r.restored.len(), 2);
    assert_eq!(f.defaults("NSGlobalDomain")["KeyRepeat"], json!(2));
    let r = restore_run(&sys(&f), &mut l, "run-a", false)
        .unwrap()
        .unwrap();
    assert_eq!(r.restored.len(), 2);
    assert_eq!(f.defaults("NSGlobalDomain")["KeyRepeat"], json!(6));
    assert!(f.defaults("com.apple.dock").get("autohide").is_none());
    assert!(l.owned.is_empty() && l.history.is_empty());
    assert!(
        restore_run(&sys(&f), &mut l, "run-a", false)
            .unwrap()
            .is_none()
    );
    assert!(
        restore_run(&sys(&f), &mut l, "nope", false)
            .unwrap()
            .is_none()
    );
}

#[test]
fn restore_leaves_a_setting_changed_after_the_run() {
    let f = Fake::default();
    let (_t, mut l) = fresh();
    run_all(&f, &mut l, &decl(KEYREPEAT, "macos"), "run-a");
    f.set_defaults("NSGlobalDomain", json!({"KeyRepeat": 9}));
    let r = restore_run(&sys(&f), &mut l, "run-a", false)
        .unwrap()
        .unwrap();
    assert!(r.restored.is_empty() && r.skipped.len() == 1);
    assert_eq!(f.defaults("NSGlobalDomain")["KeyRepeat"], json!(9));
}

// ------------------------------------------------------------------ restarts, logout, root

#[test]
fn restarts_are_aggregated_once_per_app() {
    let f = Fake::default();
    let mk = |k: &str| {
        format!(
            "[[defaults]]\ndomain = \"com.apple.dock\"\nkey = \"{k}\"\nvalue = true\nrestart = [\"Dock\"]\n"
        )
    };
    let d = decl(&format!("{}{}{}", mk("a"), mk("b"), mk("c")), "macos");
    let (steps, _) = plan(&sys(&f), &d, &[]).unwrap();
    assert_eq!(steps.len(), 3);
    let refs: Vec<&dots_settings::Step> = steps.iter().collect();
    assert_eq!(restarts(&refs), ["Dock"]);
}

#[test]
fn requires_feeds_the_same_aggregated_list() {
    let f = Fake::default();
    let d = decl(
        "[[defaults]]\ndomain = \"d\"\nkey = \"a\"\nvalue = 1\nrestart = [\"Dock\"]\nrequires = [\"restart-app:Dock\", \"restart-app:Finder\", \"logout\"]\n",
        "macos",
    );
    let (steps, _) = plan(&sys(&f), &d, &[]).unwrap();
    let refs: Vec<&dots_settings::Step> = steps.iter().collect();
    assert_eq!(restarts(&refs), ["Dock", "Finder"]);
    assert_eq!(needs(&refs), [("p".to_string(), "logout".to_string())]);
}

#[test]
fn root_entries_run_through_sudo() {
    let f = Fake::default();
    let d = one(
        "[[defaults]]\ndomain = \"com.apple.universalaccess\"\nkey = \"cursorFill\"\nvalue = { red = 1.0 }\nroot = true\n",
        "macos",
    );
    let (steps, _) = plan(&sys(&f), std::slice::from_ref(&d), &[]).unwrap();
    assert!(steps[0].root());
    let (_t, mut l) = fresh();
    apply_step(&sys(&f), &mut l, "r1", &steps[0]).unwrap();
    let w = f
        .lines()
        .into_iter()
        .find(|l| l.contains(" write "))
        .unwrap();
    assert!(
        w.starts_with("/usr/bin/sudo -n /usr/bin/defaults write"),
        "{w}"
    );
    // a root entry is read as root too: that is what it is compared with
    assert!(
        f.lines()[0]
            .starts_with("/usr/bin/sudo -n /usr/bin/defaults export com.apple.universalaccess"),
        "{:?}",
        f.lines()
    );
}

fn sys_with(f: &Fake, sudo: Sudo) -> Sys<'_> {
    Sys {
        runner: f,
        tools: Tools::default(),
        sudo,
    }
}

const ROOT_KEY: &str =
    "[[defaults]]\ndomain = \"com.apple.x\"\nkey = \"k\"\nvalue = 2\nroot = true\n";

#[test]
fn root_entries_are_compared_and_recorded_against_roots_domain() {
    let f = Fake::default();
    // the user's own domain holds 2 already; root's holds 5
    f.set_defaults("com.apple.x", json!({ "k": 2 }));
    f.set_root_defaults("com.apple.x", json!({ "k": 5 }));
    let d = decl(ROOT_KEY, "macos");
    let (steps, _) = plan(&sys(&f), &d, &[]).unwrap();
    assert_eq!(
        steps.len(),
        1,
        "root's value 5 differs, the user's 2 is irrelevant"
    );
    assert_eq!(steps[0].before, dots_settings::Orig::of(json!(5)));
    let (_t, mut l) = fresh();
    apply_step(&sys(&f), &mut l, "r1", &steps[0]).unwrap();
    assert_eq!(f.root_defaults("com.apple.x")["k"], json!(2));
    assert_eq!(
        l.owned["defaults:com.apple.x:k"].original,
        dots_settings::Orig::of(json!(5))
    );
    // dropping the entry restores root's original value into root's domain
    run_all(&f, &mut l, &[], "r2");
    assert_eq!(f.root_defaults("com.apple.x")["k"], json!(5));
    assert_eq!(
        f.defaults("com.apple.x")["k"],
        json!(2),
        "the user's domain is untouched"
    );
}

#[test]
fn a_root_entry_never_runs_unprivileged_when_sudo_is_unavailable() {
    let f = Fake::default();
    let d = decl(ROOT_KEY, "macos");
    let (_t, mut l) = fresh();
    // plan time: no credential yet -> planned as deferred, not an error
    f.0.lock().unwrap().sudo_fails = true;
    let (steps, _) = plan(&sys_with(&f, Sudo::Unavailable), &d, &[]).unwrap();
    assert_eq!(steps.len(), 1);
    assert!(steps[0].deferred);
    // apply time, sudo declined: the step fails with the sudo error, nothing ran as the user
    f.clear();
    let e = apply_step(&sys_with(&f, Sudo::Unavailable), &mut l, "r1", &steps[0]).unwrap_err();
    assert!(e.contains("sudo is not available"), "{e}");
    assert_eq!(f.count("/usr/bin/defaults"), 0, "{:?}", f.lines());
    // even a fully planned step does not fall back to running as the user
    f.0.lock().unwrap().sudo_fails = false;
    let (ok_steps, _) = plan(&sys(&f), &d, &[]).unwrap();
    assert!(!ok_steps[0].deferred);
    f.clear();
    let e = apply_step(&sys_with(&f, Sudo::Unavailable), &mut l, "r1", &ok_steps[0]).unwrap_err();
    assert!(e.contains("needs root"), "{e}");
    assert_eq!(f.count("/usr/bin/defaults write"), 0, "{:?}", f.lines());
    assert!(l.owned.is_empty());
}

#[test]
fn a_deferred_root_step_is_read_and_applied_once_sudo_is_primed() {
    let f = Fake::default();
    f.set_root_defaults("com.apple.x", json!({ "k": 5 }));
    let d = decl(ROOT_KEY, "macos");
    f.0.lock().unwrap().sudo_fails = true;
    let (steps, _) = plan(&sys_with(&f, Sudo::Unavailable), &d, &[]).unwrap();
    assert_eq!(steps[0].detail(), "? -> 2");
    f.0.lock().unwrap().sudo_fails = false;
    let (_t, mut l) = fresh();
    apply_step(&sys(&f), &mut l, "r1", &steps[0]).unwrap();
    assert_eq!(f.root_defaults("com.apple.x")["k"], json!(2));
    assert_eq!(
        l.owned["defaults:com.apple.x:k"].original,
        dots_settings::Orig::of(json!(5))
    );
}

#[test]
fn restore_never_runs_a_root_setting_unprivileged() {
    let f = Fake::default();
    f.set_root_defaults("com.apple.x", json!({ "k": 5 }));
    let d = decl(ROOT_KEY, "macos");
    let (_t, mut l) = fresh();
    run_all(&f, &mut l, &d, "r1");
    f.clear();
    let r = restore_run(&sys_with(&f, Sudo::Unavailable), &mut l, "r1", false)
        .unwrap()
        .unwrap();
    assert!(r.restored.is_empty(), "{r:?}");
    assert_eq!(r.skipped.len(), 1);
    assert!(r.skipped[0].1.contains("needs root"), "{r:?}");
    assert_eq!(f.count("/usr/bin/defaults write"), 0);
    assert_eq!(f.root_defaults("com.apple.x")["k"], json!(2));
}

// ------------------------------------------------------------------ registry and xdg

const REG: &str = "[[registry]]\nkey = 'HKCU\\Control Panel\\Keyboard'\nname = \"KeyboardDelay\"\ntype = \"REG_SZ\"\nvalue = \"0\"\n";

#[test]
fn registry_value_is_read_first_and_written_only_when_it_differs() {
    let f = Fake::default();
    let d = decl(REG, "windows");
    let (_t, mut l) = fresh();
    let steps = run_all(&f, &mut l, &d, "r1");
    assert_eq!(steps.len(), 1);
    let lines = f.lines();
    assert!(
        lines[0].contains("reg.exe query HKCU\\Control Panel\\Keyboard"),
        "{lines:?}"
    );
    assert!(
        lines[1].contains(
            "reg.exe add HKCU\\Control Panel\\Keyboard /v KeyboardDelay /t REG_SZ /d 0 /f"
        ),
        "{lines:?}"
    );
    // second pass: read only
    f.clear();
    let (again, _) = plan(&sys(&f), &d, &[]).unwrap();
    assert!(again.is_empty());
    assert_eq!(f.lines().len(), 1);
    // removing it deletes the value (it did not exist)
    run_all(&f, &mut l, &[], "r2");
    assert!(f.lines().iter().any(|l| l.contains("reg.exe delete")));
}

#[test]
fn registry_dword_compares_numerically() {
    let f = Fake::default();
    let d = decl(
        "[[registry]]\nkey = 'HKCU\\X'\nname = \"N\"\ntype = \"REG_DWORD\"\nvalue = 2\n",
        "windows",
    );
    let (_t, mut l) = fresh();
    run_all(&f, &mut l, &d, "r1");
    // the fake reports 0x2, the declaration says 2: converged
    let (again, _) = plan(&sys(&f), &d, &[]).unwrap();
    assert!(again.is_empty());
}

#[test]
fn default_markdown_app_runs_xdg_mime_only_when_it_differs() {
    let f = Fake::default();
    let toml = "[[xdg]]\nmime = [\"text/markdown\"]\napp = \"gh-markdown-preview.desktop\"\n";
    let d = decl(toml, "linux");
    let (_t, mut l) = fresh();
    let steps = run_all(&f, &mut l, &d, "r1");
    assert_eq!(steps.len(), 1);
    assert_eq!(
        f.lines(),
        [
            "/usr/bin/xdg-mime query default text/markdown",
            "/usr/bin/xdg-mime default gh-markdown-preview.desktop text/markdown"
        ]
    );
    f.clear();
    let (again, _) = plan(&sys(&f), &d, &[]).unwrap();
    assert!(again.is_empty());
    assert_eq!(f.lines(), ["/usr/bin/xdg-mime query default text/markdown"]);
}

#[test]
fn each_mime_type_is_its_own_setting() {
    let d = decl(
        "[[xdg]]\nmime = [\"a/b\", \"c/d\"]\napp = \"x.desktop\"\n",
        "linux",
    );
    assert_eq!(d.len(), 2);
    assert_eq!(d[1].setting.id(), "xdg-mime:c/d");
}

#[test]
fn read_failure_is_an_error_not_a_silent_skip() {
    struct Broken;
    impl std::fmt::Debug for Broken {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("Broken")
        }
    }
    impl Runner for Broken {
        fn run(&self, _: &Cmd) -> PResult<Output> {
            Ok(Output::fail(1, "boom"))
        }
    }
    let s = Sys {
        runner: &Broken,
        tools: Tools::default(),
        sudo: Sudo::Unneeded,
    };
    let e = plan(&s, &decl(KEYREPEAT, "macos"), &[]).err().unwrap();
    assert!(e.contains("boom"), "{e}");
    let (_t, l) = fresh();
    let rows = status(&s, &l, &decl(KEYREPEAT, "macos"));
    assert_eq!(rows[0].health, Health::Failed);
}

#[test]
fn setting_ids_are_stable() {
    let s = Setting::XdgSetting {
        setting: "default-web-browser".into(),
        value: "x.desktop".into(),
    };
    assert_eq!(s.id(), "xdg-setting:default-web-browser");
}

// ------------------------------------------------------------------ xdg has no unset

const MD: &str = "[[xdg]]\nmime = [\"text/markdown\"]\napp = \"x.desktop\"\n";

#[test]
fn dropping_an_xdg_entry_does_not_claim_it_was_deleted() {
    let f = Fake::default();
    let d = decl(MD, "linux");
    let (_t, mut l) = fresh();
    run_all(&f, &mut l, &d, "r1");
    assert_eq!(l.owned.len(), 1);
    f.clear();
    let (steps, _) = plan(&sys(&f), &[], &orphans(&l, &[])).unwrap();
    assert_eq!(steps.len(), 1);
    assert_eq!(steps[0].op, Op::Keep);
    assert_eq!(steps[0].detail(), "left as is: xdg-mime has no unset");
    assert!(steps[0].argv.is_empty());
    apply_step(&sys(&f), &mut l, "r2", &steps[0]).unwrap();
    // the value stays, dots stops owning it, and nothing ran
    assert!(l.owned.is_empty());
    assert_eq!(f.count("/usr/bin/xdg-mime default"), 0);
    let (again, _) = plan(&sys(&f), &[], &orphans(&l, &[])).unwrap();
    assert!(again.is_empty());
}

#[test]
fn restoring_a_run_does_not_claim_an_xdg_default_was_unset() {
    let f = Fake::default();
    let d = decl(MD, "linux");
    let (_t, mut l) = fresh();
    run_all(&f, &mut l, &d, "r1");
    let r = restore_run(&sys(&f), &mut l, "r1", false).unwrap().unwrap();
    assert!(r.restored.is_empty(), "{r:?}");
    assert!(r.skipped[0].1.contains("no unset"), "{r:?}");
}

// ------------------------------------------------------------------ logout / reboot settle

#[test]
fn settled_reports_the_logout_entries_that_now_read_as_declared() {
    let f = Fake::default();
    let d = decl(
        "[[defaults]]\ndomain = \"d\"\nkey = \"a\"\nvalue = 1\nrequires = [\"logout\"]\n[[defaults]]\ndomain = \"d\"\nkey = \"b\"\nvalue = 1\n",
        "macos",
    );
    // nothing applied yet: nothing is settled
    assert!(dots_settings::settled(&sys(&f), &d).is_empty());
    let (_t, mut l) = fresh();
    run_all(&f, &mut l, &d, "r1");
    let s = dots_settings::settled(&sys(&f), &d);
    assert_eq!(s.len(), 1, "only the entry that requires a logout: {s:?}");
    assert_eq!(s[0].package, "p");
    assert_eq!(s[0].reason, dots_settings::todo_reason("logout", "d a"));
    // drift reopens it
    f.set_defaults("d", json!({ "a": 9, "b": 1 }));
    assert!(dots_settings::settled(&sys(&f), &d).is_empty());
}

// ------------------------------------------------------------------ adopt follows the layer that set the value

fn effective_repo(files: &[(&str, &str)]) -> (tempfile::TempDir, dots_model::Repo) {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    std::fs::write(root.join("dots.toml"), "schema = 1\nname = \"t\"\n").unwrap();
    for (rel, text) in files {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }
    let repo = dots_model::Repo::load(root)
        .unwrap_or_else(|e| panic!("{:?}", e.errors))
        .value;
    let locations = dots_model::OverrideLocation::standard(
        root,
        None,
        root.join("machine-overrides"),
        "~/.config/dots/overrides".into(),
    );
    let eff = repo
        .effective(&|_| vec!["macos".to_string()], &locations)
        .unwrap_or_else(|e| panic!("{:?}", e.errors))
        .value;
    (tmp, eff)
}

const BASE: &str =
    "schema = 1\nname = \"p\"\n[[defaults]]\ndomain = \"d\"\nkey = \"k\"\nvalue = 1\n";

fn adopt_via_effective(repo: &dots_model::Repo, observed: Value) {
    let pkg = &repo.packages["p"];
    let (active, _) = declared("p", pkg, "macos").unwrap();
    let setting = &active[0].setting;
    // the effective model already holds the winning value
    assert!(matches!(setting, Setting::Defaults { .. }));
    let info = repo.effective.as_ref().expect("effective info");
    let at = dots_settings::locate(info, "p", &pkg.dir, setting);
    dots_settings::adopt_in(&at, setting, &observed).unwrap();
}

fn reload(root: &Path) -> Value {
    let repo = dots_model::Repo::load(root).unwrap().value;
    let locations = dots_model::OverrideLocation::standard(
        root,
        None,
        root.join("machine-overrides"),
        "x".into(),
    );
    let eff = repo
        .effective(&|_| vec!["macos".to_string()], &locations)
        .unwrap()
        .value;
    let (a, _) = declared("p", &eff.packages["p"], "macos").unwrap();
    match &a[0].setting {
        Setting::Defaults { value, .. } => value.clone(),
        _ => unreachable!(),
    }
}

#[test]
fn adopt_edits_the_override_that_set_the_value() {
    let ov = "schema = 1\n[[defaults]]\ndomain = \"d\"\nkey = \"k\"\nvalue = 3\n";
    let (tmp, repo) = effective_repo(&[
        ("packages/p/package.toml", BASE),
        ("overrides/packages/p/override.toml", ov),
    ]);
    adopt_via_effective(&repo, json!(7));
    let root = tmp.path();
    // the package file is untouched; the override that wins now says 7
    assert_eq!(
        std::fs::read_to_string(root.join("packages/p/package.toml")).unwrap(),
        BASE
    );
    assert!(
        std::fs::read_to_string(root.join("overrides/packages/p/override.toml"))
            .unwrap()
            .contains("value = 7")
    );
    assert_eq!(reload(root), json!(7));
}

#[test]
fn adopt_edits_a_machine_override_in_the_keyed_table_form() {
    let ov = "schema = 1\n[defaults.\"d|k\"]\nvalue = 3\n";
    let (tmp, repo) = effective_repo(&[
        ("packages/p/package.toml", BASE),
        ("machine-overrides/packages/p/override.toml", ov),
    ]);
    adopt_via_effective(&repo, json!(8));
    let root = tmp.path();
    assert_eq!(
        std::fs::read_to_string(root.join("packages/p/package.toml")).unwrap(),
        BASE
    );
    assert_eq!(reload(root), json!(8));
}

#[test]
fn adopt_edits_the_platform_table_that_set_the_value() {
    let pkg =
        format!("{BASE}[[platform.macos.defaults]]\ndomain = \"d\"\nkey = \"k\"\nvalue = 2\n");
    let (tmp, repo) = effective_repo(&[("packages/p/package.toml", &pkg)]);
    adopt_via_effective(&repo, json!(6));
    let text = std::fs::read_to_string(tmp.path().join("packages/p/package.toml")).unwrap();
    // the base entry keeps its value; the platform table's entry changed
    assert!(text.contains("value = 1"), "{text}");
    assert!(text.contains("value = 6"), "{text}");
    assert_eq!(reload(tmp.path()), json!(6));
}

#[test]
fn status_of_a_root_entry_without_a_cached_sudo_is_unknown_not_failed() {
    let f = Fake::default();
    f.0.lock().unwrap().sudo_fails = true;
    let d = decl(ROOT_KEY, "macos");
    let (_t, l) = fresh();
    let rows = status(&sys_with(&f, Sudo::Unavailable), &l, &d);
    assert_eq!(rows[0].health, Health::Unknown, "{:?}", rows[0]);
    assert_eq!(
        rows[0].error.as_deref(),
        Some(dots_settings::NEEDS_SUDO_READ)
    );
    // a user-domain read failure stays `Failed`
    let user = decl(KEYREPEAT, "macos");
    f.0.lock().unwrap().sudo_fails = false;
    assert_ne!(status(&sys(&f), &l, &user)[0].health, Health::Unknown);
}

/// Bug 36: a key (or a whole domain) nobody wrote is unset, not unreadable: it differs from the
/// declared value, so the setting is `missing` and `apply` plans the write.
#[test]
fn a_key_that_does_not_exist_reads_as_unset_not_unknown() {
    const UA: &str =
        "[[defaults]]\ndomain = \"com.apple.universalaccess\"\nkey = \"cursorFill\"\nvalue = 1\n";
    let (_t, l) = fresh();
    for strict in [false, true] {
        let f = Fake::default();
        f.0.lock().unwrap().strict_domains = strict;
        let d = decl(UA, "macos");
        // the domain does not exist at all
        let rows = status(&sys(&f), &l, &d);
        assert_eq!(rows[0].health, Health::Missing, "strict={strict}");
        assert_eq!(rows[0].observed.as_deref(), Some("unset"));
        let (steps, _) = plan(&sys(&f), &d, &[]).unwrap();
        assert_eq!(steps.len(), 1, "strict={strict}");
        // the domain exists, the key does not
        f.set_defaults("com.apple.universalaccess", json!({ "other": 1 }));
        let rows = status(&sys(&f), &l, &d);
        assert_eq!(rows[0].health, Health::Missing, "strict={strict}");
        assert_eq!(rows[0].observed.as_deref(), Some("unset"));
    }
}

/// A root entry's domain that does not exist is unset too, once root can be read.
#[test]
fn a_missing_root_domain_is_unset_and_an_unreadable_one_stays_unknown() {
    let (_t, l) = fresh();
    let f = Fake::default();
    f.0.lock().unwrap().strict_domains = true;
    let d = decl(ROOT_KEY, "macos");
    assert_eq!(status(&sys(&f), &l, &d)[0].health, Health::Missing);
    // no credential: root's preferences cannot be read, which is not "unset"
    f.0.lock().unwrap().sudo_fails = true;
    let rows = status(&sys_with(&f, Sudo::Unavailable), &l, &d);
    assert_eq!(rows[0].health, Health::Unknown);
}

/// Real read failures stay failures: only a missing domain is "unset".
#[test]
fn a_permission_error_reading_defaults_is_still_a_failure() {
    #[derive(Debug)]
    struct Denied;
    impl Runner for Denied {
        fn run(&self, _: &Cmd) -> PResult<Output> {
            Ok(Output::fail(1, "Operation not permitted"))
        }
    }
    let s = Sys {
        runner: &Denied,
        tools: Tools::default(),
        sudo: Sudo::Unneeded,
    };
    let (_t, l) = fresh();
    assert_eq!(
        status(&s, &l, &decl(KEYREPEAT, "macos"))[0].health,
        Health::Failed
    );
}

// ------------------------------------------------------------------ xdg tool not installed yet

/// `xdg-mime` comes from a package of the same run: until then it cannot even be started.
struct XdgLater<'a> {
    fake: &'a Fake,
    installed: std::sync::atomic::AtomicBool,
}

impl std::fmt::Debug for XdgLater<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("XdgLater")
    }
}

impl Runner for XdgLater<'_> {
    fn run(&self, cmd: &Cmd) -> PResult<Output> {
        let missing = !self.installed.load(std::sync::atomic::Ordering::SeqCst);
        if missing && cmd.argv()[0].ends_with("xdg-mime") {
            return Err(dots_providers::ProviderError::Other(format!(
                "cannot run `{}`: No such file or directory (os error 2)",
                cmd.line()
            )));
        }
        self.fake.run(cmd)
    }
}

#[test]
fn xdg_with_the_tool_not_installed_yet_is_deferred_for_that_entry_only() {
    let f = Fake::default();
    let later = XdgLater {
        fake: &f,
        installed: false.into(),
    };
    let s = Sys {
        runner: &later,
        tools: Tools::default(),
        sudo: Sudo::Unavailable,
    };
    let toml = "[[xdg]]\nmime = [\"text/markdown\"]\napp = \"gh-markdown-preview.desktop\"\n\
                [[xdg]]\nmime = [\"application/pdf\"]\napp = \"zathura.desktop\"\n";
    let d = decl(toml, "linux");
    assert_eq!(d.len(), 2);
    // Planning does not fail: each xdg-mime entry is a deferred step.
    let (steps, _) = plan(&s, &d, &[]).expect("a missing xdg-mime must not fail the plan");
    assert_eq!(steps.len(), 2);
    let mime = steps
        .iter()
        .find(|s| s.id() == "xdg-mime:text/markdown")
        .unwrap();
    assert!(mime.deferred && mime.argv.is_empty(), "{mime:?}");
    assert!(steps.iter().all(|s| s.deferred));

    // `status` reports it as unknown, not failed.
    let (_t, mut l) = fresh();
    let rows = status(&s, &l, &d);
    let row = rows
        .iter()
        .find(|r| r.setting.id() == "xdg-mime:text/markdown")
        .unwrap();
    assert_eq!(row.health, Health::Unknown);

    // Applied while the tool is still missing: skipped with a note, no error, no ledger entry.
    let note = dots_settings::apply_step_noted(&s, &mut l, "r1", mime).unwrap();
    assert!(note.unwrap().contains("xdg-mime is still not installed"));
    assert!(!l.owned.contains_key("xdg-mime:text/markdown"));
    assert_eq!(f.count("/usr/bin/xdg-mime default"), 0);

    // The package installs the tool; the same step is now read and applied for real.
    later
        .installed
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let note = dots_settings::apply_step_noted(&s, &mut l, "r1", mime).unwrap();
    assert_eq!(note, None);
    assert_eq!(
        f.count("/usr/bin/xdg-mime default gh-markdown-preview.desktop text/markdown"),
        1
    );
    assert!(l.owned.contains_key("xdg-mime:text/markdown"));
}
