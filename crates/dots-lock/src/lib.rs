//! `dots.lock`: the committed record of resolved versions, revisions and content hashes per
//! platform (ARCHITECTURE section 8.2, `specs/lockfile`).
//!
//! The file is TOML: `schema = 1` and three kinds of array-of-tables entries, `[[pkg]]`,
//! `[[upstream]]` and `[[content]]`. Rendering is deterministic (fixed key order, rows sorted,
//! one block per row), so locking the same resolution twice yields identical bytes and merges
//! between branches conflict rarely.
//!
//! Unknown keys on a row survive a read/write round trip (`extra`), which is how later
//! features (content hashes, task 6.9) add fields without breaking older readers.

mod fix;
pub mod rules;

use std::collections::BTreeMap;
use std::path::Path;

use dots_core::{Diagnostic, Provenance};
use toml_edit::{DocumentMut, Item};

pub use fix::{FixReport, fix_text};

/// The lock file name, at the repo root.
pub const FILE: &str = "dots.lock";
pub const SCHEMA: i64 = 1;

/// Providers whose versions depend on the platform. Everything else (cargo, uv, npm, git,
/// archive, user-defined plugin managers, ...) resolves the same everywhere and shares a
/// `platform = "*"` row.
pub const PLATFORM_SPECIFIC: &[&str] = &["brew", "apt", "script"];

/// The `platform` of a package row: `*` for platform-independent providers, else the machine's
/// most specific chain key plus its architecture (`ubuntu-24.04/x86_64`).
pub fn platform_for(provider: &str, machine_key: &str) -> String {
    if PLATFORM_SPECIFIC.contains(&provider) {
        machine_key.to_string()
    } else {
        "*".to_string()
    }
}

/// `<most specific chain key>/<arch>`.
pub fn machine_key(levels: &[String], arch: &str) -> String {
    format!("{}/{arch}", levels.last().map_or("any", String::as_str))
}

/// Raw TOML text of unknown keys, kept verbatim.
pub type Extra = BTreeMap<String, String>;

/// One `[[pkg]]` row: what a package resolved to on a platform.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PkgRow {
    pub name: String,
    pub platform: String,
    pub provider: String,
    /// The provider's version string for the locked install.
    pub version: Option<String>,
    /// A git revision (git, cargo/uv git or path sources, herdr-style `resolved_commit`s).
    pub rev: Option<String>,
    /// `git:<ref>` the rev was resolved from.
    pub git_ref: Option<String>,
    pub source: Option<String>,
    pub locked_at: String,
    /// The repo commit this row was produced at.
    pub by_rev: Option<String>,
    pub extra: Extra,
}

impl PkgRow {
    pub fn new(name: &str, platform: &str, provider: &str) -> Self {
        PkgRow {
            name: name.into(),
            platform: platform.into(),
            provider: provider.into(),
            version: None,
            rev: None,
            git_ref: None,
            source: None,
            locked_at: String::new(),
            by_rev: None,
            extra: Extra::new(),
        }
    }

    /// The row's identity: what it locks, ignoring when and at which repo commit.
    fn same_lock(&self, o: &PkgRow) -> bool {
        self.provider == o.provider
            && self.version == o.version
            && self.rev == o.rev
            && self.git_ref == o.git_ref
            && self.source == o.source
            && self.extra == o.extra
    }

    /// What to show for the locked version: `0.10.2`, or the rev when there is no version.
    pub fn shown(&self) -> String {
        self.version
            .clone()
            .or_else(|| self.rev.clone())
            .unwrap_or_default()
    }
}

/// `[[upstream]]`: the revision a followed upstream is pinned at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpstreamRow {
    pub alias: String,
    pub rev: String,
    pub extra: Extra,
}

/// `[[content]]`: tree hash per followed or copied item, and download hashes (a script's
/// `fetch` file). Verification is the integrity feature (task 6.9); this crate only stores.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContentRow {
    pub item: String,
    pub rev: Option<String>,
    pub tree: Option<String>,
    pub url: Option<String>,
    pub sha256: Option<String>,
    pub extra: Extra,
}

impl ContentRow {
    pub fn new(item: &str) -> Self {
        ContentRow {
            item: item.into(),
            rev: None,
            tree: None,
            url: None,
            sha256: None,
            extra: Extra::new(),
        }
    }
}

/// The whole file.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Lock {
    pub pkgs: Vec<PkgRow>,
    pub upstream: Vec<UpstreamRow>,
    pub content: Vec<ContentRow>,
}

/// What [`Lock::upsert`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Upsert {
    Added,
    Changed,
    /// The same lock was already there; nothing (not even `locked_at`) was touched.
    Unchanged,
}

fn quote(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn put(out: &mut String, key: &str, v: &str) {
    out.push_str(&format!("{key} = {}\n", quote(v)));
}

fn put_opt(out: &mut String, key: &str, v: &Option<String>) {
    if let Some(v) = v {
        put(out, key, v);
    }
}

fn put_extra(out: &mut String, extra: &Extra) {
    for (k, raw) in extra {
        out.push_str(&format!("{k} = {raw}\n"));
    }
}

impl Lock {
    /// Sort every row kind into the order `render` writes them.
    pub fn normalize(&mut self) {
        self.pkgs
            .sort_by(|a, b| (&a.name, &a.platform).cmp(&(&b.name, &b.platform)));
        self.upstream.sort_by(|a, b| a.alias.cmp(&b.alias));
        self.content
            .sort_by(|a, b| (&a.item, &a.rev).cmp(&(&b.item, &b.rev)));
    }

    pub fn is_empty(&self) -> bool {
        self.pkgs.is_empty() && self.upstream.is_empty() && self.content.is_empty()
    }

    /// Deterministic text.
    pub fn render(&self) -> String {
        let mut out = String::from(
            "# Generated by dots. Do not edit. One block per (package, platform-key).\n",
        );
        out.push_str(&format!("schema = {SCHEMA}\n"));

        let mut pkgs: Vec<&PkgRow> = self.pkgs.iter().collect();
        pkgs.sort_by(|a, b| (&a.name, &a.platform).cmp(&(&b.name, &b.platform)));
        for r in pkgs {
            out.push_str("\n[[pkg]]\n");
            put(&mut out, "name", &r.name);
            put(&mut out, "platform", &r.platform);
            put(&mut out, "provider", &r.provider);
            put_opt(&mut out, "version", &r.version);
            put_opt(&mut out, "rev", &r.rev);
            put_opt(&mut out, "ref", &r.git_ref);
            put_opt(&mut out, "source", &r.source);
            put(&mut out, "locked_at", &r.locked_at);
            put_opt(&mut out, "by_rev", &r.by_rev);
            put_extra(&mut out, &r.extra);
        }
        let mut ups: Vec<&UpstreamRow> = self.upstream.iter().collect();
        ups.sort_by(|a, b| a.alias.cmp(&b.alias));
        for r in ups {
            out.push_str("\n[[upstream]]\n");
            put(&mut out, "alias", &r.alias);
            put(&mut out, "rev", &r.rev);
            put_extra(&mut out, &r.extra);
        }
        let mut cs: Vec<&ContentRow> = self.content.iter().collect();
        cs.sort_by(|a, b| (&a.item, &a.rev).cmp(&(&b.item, &b.rev)));
        for r in cs {
            out.push_str("\n[[content]]\n");
            put(&mut out, "item", &r.item);
            put_opt(&mut out, "rev", &r.rev);
            put_opt(&mut out, "tree", &r.tree);
            put_opt(&mut out, "url", &r.url);
            put_opt(&mut out, "sha256", &r.sha256);
            put_extra(&mut out, &r.extra);
        }
        out
    }

    /// Parse the text of a lock. Strict: any malformed row fails with a message naming it.
    pub fn parse(text: &str) -> Result<Lock, String> {
        let doc: DocumentMut = text.parse().map_err(|e: toml_edit::TomlError| {
            e.to_string()
                .lines()
                .next()
                .unwrap_or("invalid TOML")
                .to_string()
        })?;
        match doc.get("schema").and_then(Item::as_integer) {
            Some(SCHEMA) => {}
            Some(n) if n > SCHEMA => {
                return Err(format!("schema {n} is newer than this dots understands"));
            }
            Some(n) => return Err(format!("unsupported schema {n}")),
            None => return Err("missing `schema = 1`".into()),
        }
        let mut lock = Lock::default();
        for (i, t) in tables(&doc, "pkg").into_iter().enumerate() {
            lock.pkgs
                .push(pkg_row(t).map_err(|e| format!("[[pkg]] #{}: {e}", i + 1))?);
        }
        for (i, t) in tables(&doc, "upstream").into_iter().enumerate() {
            lock.upstream
                .push(upstream_row(t).map_err(|e| format!("[[upstream]] #{}: {e}", i + 1))?);
        }
        for (i, t) in tables(&doc, "content").into_iter().enumerate() {
            lock.content
                .push(content_row(t).map_err(|e| format!("[[content]] #{}: {e}", i + 1))?);
        }
        lock.normalize();
        Ok(lock)
    }

    /// Read `path`. A missing file is an empty lock. A file with merge-conflict markers is
    /// `lock-conflicted`; any other problem is `lock-invalid`. Both point at `lock --fix`.
    pub fn load(path: &Path) -> Result<Lock, Diagnostic> {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Lock::default()),
            Err(e) => return Err(diag("lock-invalid", path, &e.to_string())),
        };
        if has_conflict_markers(&text) {
            return Err(diag("lock-conflicted", path, ""));
        }
        Lock::parse(&text).map_err(|e| diag("lock-invalid", path, &e))
    }

    /// Atomic write (temp file, then rename).
    pub fn save(&self, path: &Path) -> Result<(), Diagnostic> {
        let text = self.render();
        let fail = |e: std::io::Error| diag("lock-write-failed", path, &e.to_string());
        let dir = path.parent().unwrap_or_else(|| Path::new("."));
        let tmp = dir.join(format!(".dots.lock.{}.tmp", std::process::id()));
        std::fs::write(&tmp, text).map_err(fail)?;
        std::fs::rename(&tmp, path).map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            fail(e)
        })
    }

    /// The row for `name` on `platform`.
    pub fn pkg(&self, name: &str, platform: &str) -> Option<&PkgRow> {
        self.pkgs
            .iter()
            .find(|r| r.name == name && r.platform == platform)
    }

    /// Insert or replace the row keyed by `(name, platform)`. When the new row locks exactly
    /// what the old one did, the old row stays untouched, so re-locking is byte-stable.
    pub fn upsert(&mut self, row: PkgRow) -> Upsert {
        match self
            .pkgs
            .iter_mut()
            .find(|r| r.name == row.name && r.platform == row.platform)
        {
            Some(old) if old.same_lock(&row) => Upsert::Unchanged,
            Some(old) => {
                *old = row;
                Upsert::Changed
            }
            None => {
                self.pkgs.push(row);
                Upsert::Added
            }
        }
    }

    pub fn remove_pkg(&mut self, name: &str, platform: &str) -> bool {
        let n = self.pkgs.len();
        self.pkgs
            .retain(|r| !(r.name == name && r.platform == platform));
        self.pkgs.len() != n
    }

    /// The download-hash row for `item` (`fetch:<pkg>`), when recorded.
    pub fn content_for(&self, item: &str) -> Option<&ContentRow> {
        self.content.iter().find(|c| c.item == item)
    }

    /// Record (or replace) a content row.
    pub fn set_content(&mut self, row: ContentRow) {
        match self.content.iter_mut().find(|c| c.item == row.item) {
            Some(old) => *old = row,
            None => self.content.push(row),
        }
    }
}

pub(crate) fn diag(code: &str, path: &Path, detail: &str) -> Diagnostic {
    let file = path
        .file_name()
        .map_or(FILE.to_string(), |n| n.to_string_lossy().into_owned());
    Diagnostic::from_code(code, &[("detail", detail), ("file", &file)]).with_provenance(
        Provenance {
            source: "dots.lock".into(),
            file: Some(file),
            line: None,
            col: None,
        },
    )
}

/// Does the text contain `<<<<<<<` / `=======` / `>>>>>>>` marker lines?
pub fn has_conflict_markers(text: &str) -> bool {
    text.lines()
        .any(|l| l.starts_with("<<<<<<<") || l.starts_with(">>>>>>>") || l == "=======")
}

fn tables<'a>(doc: &'a DocumentMut, key: &str) -> Vec<&'a toml_edit::Table> {
    doc.get(key)
        .and_then(Item::as_array_of_tables)
        .map(|a| a.iter().collect())
        .unwrap_or_default()
}

fn str_of(t: &toml_edit::Table, k: &str) -> Result<Option<String>, String> {
    match t.get(k) {
        None => Ok(None),
        Some(i) => i
            .as_str()
            .map(|s| Some(s.to_string()))
            .ok_or_else(|| format!("`{k}` must be a string")),
    }
}

fn need(t: &toml_edit::Table, k: &str) -> Result<String, String> {
    str_of(t, k)?
        .filter(|s| !s.is_empty())
        .ok_or_else(|| format!("missing `{k}`"))
}

fn extras(t: &toml_edit::Table, known: &[&str]) -> Extra {
    t.iter()
        .filter(|(k, _)| !known.contains(k))
        .filter_map(|(k, v)| {
            v.as_value()
                .map(|v| (k.to_string(), v.to_string().trim().to_string()))
        })
        .collect()
}

pub(crate) fn pkg_row(t: &toml_edit::Table) -> Result<PkgRow, String> {
    const KNOWN: &[&str] = &[
        "name",
        "platform",
        "provider",
        "version",
        "rev",
        "ref",
        "source",
        "locked_at",
        "by_rev",
    ];
    let row = PkgRow {
        name: need(t, "name")?,
        platform: need(t, "platform")?,
        provider: need(t, "provider")?,
        version: str_of(t, "version")?,
        rev: str_of(t, "rev")?,
        git_ref: str_of(t, "ref")?,
        source: str_of(t, "source")?,
        locked_at: str_of(t, "locked_at")?.unwrap_or_default(),
        by_rev: str_of(t, "by_rev")?,
        extra: extras(t, KNOWN),
    };
    if row.version.is_none() && row.rev.is_none() {
        return Err(format!("`{}` has neither `version` nor `rev`", row.name));
    }
    Ok(row)
}

pub(crate) fn upstream_row(t: &toml_edit::Table) -> Result<UpstreamRow, String> {
    Ok(UpstreamRow {
        alias: need(t, "alias")?,
        rev: need(t, "rev")?,
        extra: extras(t, &["alias", "rev"]),
    })
}

pub(crate) fn content_row(t: &toml_edit::Table) -> Result<ContentRow, String> {
    Ok(ContentRow {
        item: need(t, "item")?,
        rev: str_of(t, "rev")?,
        tree: str_of(t, "tree")?,
        url: str_of(t, "url")?,
        sha256: str_of(t, "sha256")?,
        extra: extras(t, &["item", "rev", "tree", "url", "sha256"]),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(name: &str, platform: &str, version: &str) -> PkgRow {
        let mut r = PkgRow::new(name, platform, "brew");
        r.version = Some(version.into());
        r.locked_at = "2026-11-25T20:11:03Z".into();
        r.by_rev = Some("f4522416".into());
        r
    }

    #[test]
    fn render_is_sorted_and_roundtrips() {
        let mut l = Lock::default();
        l.upsert(row("zoxide", "macos-15/aarch64", "0.9.4"));
        l.upsert(row("bat", "macos-15/aarch64", "0.24.0"));
        let mut git = PkgRow::new("kv", "*", "cargo");
        git.rev = Some("9e1c0b7d2f".into());
        git.source = Some("git+https://github.com/KoalaVim/kv".into());
        git.extra.insert("future".into(), "\"x\"".into());
        l.upsert(git);
        let text = l.render();
        assert!(text.find("\"bat\"").unwrap() < text.find("\"kv\"").unwrap());
        assert!(text.find("\"kv\"").unwrap() < text.find("\"zoxide\"").unwrap());
        let back = Lock::parse(&text).unwrap();
        assert_eq!(back.render(), text);
        assert_eq!(back.pkg("kv", "*").unwrap().extra["future"], "\"x\"");
    }

    #[test]
    fn upsert_keeps_untouched_rows_byte_stable() {
        let mut l = Lock::default();
        l.upsert(row("bat", "p", "1.0"));
        let before = l.render();
        let mut again = row("bat", "p", "1.0");
        again.locked_at = "2030-01-01T00:00:00Z".into();
        assert_eq!(l.upsert(again), Upsert::Unchanged);
        assert_eq!(l.render(), before);
        assert_eq!(l.upsert(row("bat", "p", "1.1")), Upsert::Changed);
    }

    #[test]
    fn platform_keys() {
        assert_eq!(platform_for("cargo", "ubuntu-24.04/x86_64"), "*");
        assert_eq!(
            platform_for("apt", "ubuntu-24.04/x86_64"),
            "ubuntu-24.04/x86_64"
        );
        let levels: Vec<String> = ["any", "unix", "macos", "macos-15"]
            .map(String::from)
            .to_vec();
        assert_eq!(machine_key(&levels, "aarch64"), "macos-15/aarch64");
    }

    #[test]
    fn rejects_bad_rows_and_future_schema() {
        assert!(Lock::parse("schema = 2\n").unwrap_err().contains("newer"));
        assert!(Lock::parse("[[pkg]]\nname = \"a\"\n").is_err());
        let no_version = "schema = 1\n[[pkg]]\nname=\"a\"\nplatform=\"*\"\nprovider=\"cargo\"\n";
        assert!(Lock::parse(no_version).unwrap_err().contains("neither"));
    }
}
