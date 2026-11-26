//! Task 5.12: `dots lint` warns about an unpinned `curl ... | sh` in a script package.

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

fn warnings(files: &[(&str, &str)]) -> Vec<dots_core::report::Diagnostic> {
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
    .warnings
    .into_iter()
    .filter(|d| d.code == "script-unpinned-fetch")
    .collect()
}

fn pkg(install: &str) -> String {
    format!(
        "schema = 1\nname = \"rustup\"\n[providers.script]\ncheck = \"command -v rustc\"\nversionless = true\ninstall = {install}\n"
    )
}

#[test]
fn curl_pipe_sh_warns_and_suggests_fetch() {
    let w = warnings(&[(
        "packages/rustup/package.toml",
        &pkg("\"curl https://example.com/i.sh | sh\""),
    )]);
    assert_eq!(w.len(), 1, "{w:#?}");
    assert!(w[0].message.contains("curl https://example.com/i.sh | sh"));
    assert!(w[0].hint.contains("fetch"), "{}", w[0].hint);
    assert_eq!(
        w[0].provenance[0].file.as_deref(),
        Some("packages/rustup/package.toml")
    );
}

#[test]
fn the_other_pipe_shapes_and_script_files_are_caught() {
    for text in [
        "curl -fsSL https://x.io/i.sh | sudo bash -s -- -y",
        "wget -qO- https://x.io/i.sh | bash",
        "bash -c \"$(curl -fsSL https://x.io/i.sh)\"",
        "sh <(curl -fsSL https://x.io/i.sh)",
    ] {
        let w = warnings(&[("packages/rustup/package.toml", &pkg(&format!("{text:?}")))]);
        assert_eq!(w.len(), 1, "{text}: {w:#?}");
    }
    // The pipe is inside the package's own script.
    let w = warnings(&[
        (
            "packages/rustup/package.toml",
            &pkg("\"scripts/install.sh\""),
        ),
        (
            "packages/rustup/scripts/install.sh",
            "#!/bin/sh\n# curl https://example.com | sh (just a comment)\ncurl -sSf https://sh.rustup.rs | sh -s -- -y\n",
        ),
    ]);
    assert_eq!(w.len(), 1, "{w:#?}");
    assert!(w[0].message.contains("sh.rustup.rs"));
}

#[test]
fn pinned_and_ordinary_scripts_are_quiet() {
    for install in [
        "\"sh {fetched} -y\"",
        "\"curl -fsSL https://x.io/f.tar.gz -o f.tar.gz && tar xf f.tar.gz\"",
        "\"echo hi | sh\"",
    ] {
        let w = warnings(&[("packages/rustup/package.toml", &pkg(install))]);
        assert!(w.is_empty(), "{install}: {w:#?}");
    }
}

#[test]
fn fetch_without_url_is_a_lint_error_with_a_line() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "dots.toml", "schema = 1\n");
    write(
        dir.path(),
        "packages/rustup/package.toml",
        "schema = 1\nname = \"rustup\"\n[providers.script]\ncheck = \"true\"\ninstall = \"x\"\n[providers.script.fetch]\nsha256 = \"abc\"\n",
    );
    let repo = Repo::load(dir.path()).unwrap().value;
    let chain = Chain::build(&Facts::default(), &BTreeMap::new(), &[]);
    let out = lint(&LintInputs {
        repo: &repo,
        machine: None,
        chain: &chain,
    });
    let d: Vec<_> = out
        .errors
        .iter()
        .filter(|d| d.code == "schema-invalid-value")
        .collect();
    assert_eq!(d.len(), 1, "{:#?}", out.errors);
    assert!(d[0].message.contains("url"), "{}", d[0].message);
    assert!(
        d[0].message.contains(":6") || d[0].message.contains(":5"),
        "{}",
        d[0].message
    );
}
