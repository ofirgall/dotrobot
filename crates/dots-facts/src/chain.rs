//! The platform chain (task 2.4).

use std::collections::BTreeMap;

use crate::facts::Facts;

/// An ordered list of platform keys, general to specific, plus the architecture qualifier.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chain {
    levels: Vec<String>,
    arch: String,
}

impl Chain {
    /// Build the chain from facts.
    ///
    /// `platforms` maps a custom platform key to its parent (`[platforms.<key>] parent`),
    /// `extra` is machine `platform_extra`. A key with a declared parent gets that parent
    /// (and its ancestors) inserted before it unless already present.
    pub fn build(facts: &Facts, platforms: &BTreeMap<String, String>, extra: &[String]) -> Chain {
        let mut base: Vec<String> = vec!["any".into()];
        let unix = facts.os != "windows";
        if unix {
            base.push("unix".into());
        }
        base.push(facts.os.clone());

        match facts.os.as_str() {
            "linux" => {
                for p in facts.id_like.iter().rev() {
                    base.push(p.clone());
                }
                if let Some(id) = &facts.distro {
                    base.push(id.clone());
                    versioned(&mut base, id, facts.distro_version.as_deref());
                }
            }
            os => versioned(&mut base, os, facts.distro_version.as_deref()),
        }
        if facts.is_wsl {
            base.push("wsl".into());
        }
        base.extend(extra.iter().cloned());

        let mut levels: Vec<String> = Vec::new();
        for key in base {
            push_with_ancestors(&mut levels, &key, platforms, 0);
        }
        Chain {
            levels,
            arch: facts.arch.clone(),
        }
    }

    /// The unqualified keys, general to specific.
    pub fn levels(&self) -> &[String] {
        &self.levels
    }

    pub fn arch(&self) -> &str {
        &self.arch
    }

    /// Every key including `<key>/<arch>` qualifiers, each right after its level. This is the
    /// order platform tables merge in.
    pub fn ordered(&self) -> Vec<String> {
        self.levels
            .iter()
            .flat_map(|k| [k.clone(), format!("{k}/{}", self.arch)])
            .collect()
    }

    /// Is-a test: the key (qualified or not) appears anywhere in the chain.
    pub fn contains(&self, key: &str) -> bool {
        match key.split_once('/') {
            Some((k, arch)) => arch == self.arch && self.levels.iter().any(|l| l == k),
            None => self.levels.iter().any(|l| l == key),
        }
    }

    /// `any › unix › linux › … (x86_64)`.
    pub fn pretty(&self) -> String {
        format!("{} ({})", self.levels.join(" › "), self.arch)
    }
}

/// Well-known distro lineage: the direct parent of a distro. Only used to complete ancestry
/// for custom parents and for os-release files without `ID_LIKE`.
pub(crate) fn known_parent(id: &str) -> Option<&'static str> {
    Some(match id {
        "ubuntu" | "raspbian" => "debian",
        "linuxmint" | "pop" | "elementary" => "ubuntu",
        "manjaro" | "endeavouros" => "arch",
        "rocky" | "almalinux" | "centos" => "rhel",
        _ => return None,
    })
}

/// Ancestors from the built-in table, nearest first (`pop` gives `ubuntu`, `debian`).
pub(crate) fn known_ancestors(id: &str) -> Vec<String> {
    let mut v = Vec::new();
    let mut cur = id;
    while let Some(p) = known_parent(cur) {
        v.push(p.to_string());
        cur = p;
    }
    v
}

/// Push `ID-<major>` then `ID-<version>` (deduplicated) when a version is known.
fn versioned(base: &mut Vec<String>, id: &str, version: Option<&str>) {
    let Some(v) = version.filter(|v| !v.is_empty()) else {
        return;
    };
    let major = v.split('.').next().unwrap_or(v);
    base.push(format!("{id}-{major}"));
    if major != v {
        base.push(format!("{id}-{v}"));
    }
}

fn push_with_ancestors(
    out: &mut Vec<String>,
    key: &str,
    platforms: &BTreeMap<String, String>,
    depth: usize,
) {
    if out.iter().any(|k| k == key) {
        return;
    }
    if depth < 16
        && let Some(parent) = platforms.get(key).map(String::as_str).or(if depth > 0 {
            known_parent(key)
        } else {
            None
        })
    {
        push_with_ancestors(out, parent, platforms, depth + 1);
    }
    if !out.iter().any(|k| k == key) {
        out.push(key.to_string());
    }
}
