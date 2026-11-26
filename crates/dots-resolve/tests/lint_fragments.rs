//! Task 5.5: `dots lint` reports two fragments that set the same key at the same priority.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use dots_facts::{Chain, Facts};
use dots_model::Repo;
use dots_resolve::{LintInputs, lint};

fn write(root: &Path, rel: &str, text: &str) {
    let p = root.join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, text).unwrap();
}

fn pkg(name: &str, priority: i64) -> String {
    format!(
        "schema = 1\nname = \"{name}\"\n[[files]]\ndst = \"~/.gitconfig\"\nmode = \"fragment\"\nformat = \"gitconfig\"\nsrc = \"g.fragment\"\npriority = {priority}\n"
    )
}

fn collisions(files: &[(&str, &str)]) -> Vec<dots_core::report::Diagnostic> {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "dots.toml", "schema = 1\n");
    for (rel, text) in files {
        write(dir.path(), rel, text);
    }
    let repo = Repo::load(dir.path())
        .unwrap_or_else(|e| panic!("{:#?}", e.errors))
        .value;
    let chain = Chain::build(&Facts::default(), &BTreeMap::new(), &[]);
    lint(&LintInputs {
        repo: &repo,
        machine: None,
        chain: &chain,
    })
    .errors
    .into_iter()
    .filter(|d| d.code == "fragment-collision")
    .collect()
}

#[test]
fn same_key_same_priority_is_an_error() {
    let c = collisions(&[
        ("packages/base/package.toml", &pkg("base", 0)),
        ("packages/base/g.fragment", "[core]\neditor = vi\n"),
        ("packages/delta/package.toml", &pkg("delta", 0)),
        (
            "packages/delta/g.fragment",
            "[core]\neditor = nvim\n[delta]\nnavigate = true\n",
        ),
    ]);
    assert_eq!(c.len(), 1, "{c:#?}");
    assert!(c[0].message.contains("core.editor"), "{}", c[0].message);
    assert!(c[0].message.contains("`base`") && c[0].message.contains("`delta`"));
}

#[test]
fn different_priorities_or_keys_are_fine() {
    let c = collisions(&[
        ("packages/base/package.toml", &pkg("base", 0)),
        ("packages/base/g.fragment", "[core]\neditor = vi\n"),
        ("packages/delta/package.toml", &pkg("delta", 50)),
        ("packages/delta/g.fragment", "[core]\neditor = nvim\n"),
        ("packages/other/package.toml", &pkg("other", 0)),
        ("packages/other/g.fragment", "[core]\npager = less\n"),
    ]);
    assert!(c.is_empty(), "{c:#?}");
}
