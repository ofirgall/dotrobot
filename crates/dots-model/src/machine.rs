//! Machine configuration: a committed profile (`machines/<name>.toml`) overlaid by the local
//! `machine.toml`, key by key.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::defs;
use crate::load::{load_typed, read_source};
use crate::repo::{LoadErrors, Loaded};
use crate::source::Diags;
use crate::types::{ListSpec, MachineFile, SettingValues};

/// The effective machine configuration.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MachineConfig {
    pub root: Option<String>,
    /// The profile name that was applied, if any.
    pub profile: Option<String>,
    pub bundles: Vec<String>,
    pub tags: Vec<String>,
    pub exclude: Vec<String>,
    pub platform_extra: Vec<String>,
    pub facts: BTreeMap<String, Value>,
    pub settings: SettingValues,
    /// `[settings]` of the committed profile alone (scope 5 of the settings precedence).
    pub profile_settings: SettingValues,
    /// `[settings]` of the local `machine.toml` alone (scope 6).
    pub local_settings: SettingValues,
    /// The profile file that was applied.
    pub profile_path: Option<PathBuf>,
}

impl MachineConfig {
    /// Load the effective config.
    ///
    /// The profile is `profile` from the local file when set (it must exist), otherwise
    /// `machines/<hostname>.toml` when that file exists. It is applied first; the local file
    /// (`local_path`, usually `~/.config/dots/machine.toml`, optional) then overrides key by
    /// key. A list is replaced by a plain list or edited by `{ add, remove }`; `settings`
    /// and `facts` merge per key.
    pub fn load(
        repo_root: &Path,
        local_path: &Path,
        hostname: &str,
    ) -> Result<Loaded<MachineConfig>, LoadErrors> {
        let mut diags = Diags::default();

        let local = if local_path.exists() {
            read_source(
                local_path,
                local_path.display().to_string(),
                "machine".into(),
                &mut diags,
            )
            .and_then(|src| load_typed::<MachineFile>(&src, &defs::MACHINE, false, &mut diags))
            .map(|l| l.value)
        } else {
            None
        };
        // A broken local file already produced errors; still try the profile for more errors.
        let explicit = local.as_ref().and_then(|l| l.profile.clone());

        let (profile_name, profile_file, profile_path) =
            Self::load_profile(repo_root, explicit.as_deref(), hostname, &mut diags);

        if !diags.errors.is_empty() {
            return Err(LoadErrors {
                errors: diags.errors,
                warnings: diags.warnings,
            });
        }

        let mut cfg = MachineConfig {
            profile: profile_name,
            profile_path,
            ..Default::default()
        };
        if let Some(p) = &profile_file {
            cfg.overlay(p);
            cfg.profile_settings = p.settings.clone();
        }
        if let Some(l) = &local {
            cfg.overlay(l);
            cfg.local_settings = l.settings.clone();
        }
        Ok(Loaded {
            value: cfg,
            warnings: diags.warnings,
        })
    }

    /// The `root` the local machine file names, if it can be read. Errors are ignored here:
    /// [`MachineConfig::load`] reports them. The root has to come from the local file alone,
    /// because the profiles live inside the root.
    pub fn local_root(local_path: &Path) -> Option<String> {
        if !local_path.exists() {
            return None;
        }
        let mut diags = Diags::default();
        read_source(
            local_path,
            local_path.display().to_string(),
            "machine".into(),
            &mut diags,
        )
        .and_then(|src| load_typed::<MachineFile>(&src, &defs::MACHINE, false, &mut diags))
        .and_then(|l| l.value.root)
    }

    fn load_profile(
        repo_root: &Path,
        explicit: Option<&str>,
        hostname: &str,
        diags: &mut Diags,
    ) -> (Option<String>, Option<MachineFile>, Option<PathBuf>) {
        let name = match explicit {
            Some(n) => n.to_string(),
            None => hostname.to_string(),
        };
        // Profile names become file names; refuse anything that is not a plain name.
        let plain = !name.is_empty() && !name.contains(['/', '\\']) && name != "." && name != "..";
        let path = repo_root.join("machines").join(format!("{name}.toml"));
        if !plain || !path.is_file() {
            if explicit.is_some() {
                let stub = crate::source::SourceFile::new(
                    format!("machines/{name}.toml"),
                    "machine",
                    String::new(),
                );
                diags.file_error("machine-profile-missing", &[("profile", &name)], &stub);
            }
            return (None, None, None);
        }
        let display = format!("machines/{name}.toml");
        let file = read_source(&path, display, format!("machine-profile:{name}"), diags)
            .and_then(|src| load_typed::<MachineFile>(&src, &defs::MACHINE, false, diags))
            .map(|l| l.value);
        (Some(name), file, Some(path))
    }

    fn overlay(&mut self, f: &MachineFile) {
        if let Some(r) = &f.root {
            self.root = Some(r.clone());
        }
        let list = |spec: &Option<ListSpec>, base: &mut Vec<String>| {
            if let Some(s) = spec {
                *base = s.apply(base);
            }
        };
        list(&f.bundles, &mut self.bundles);
        list(&f.tags, &mut self.tags);
        list(&f.exclude, &mut self.exclude);
        list(&f.platform_extra, &mut self.platform_extra);
        for (k, v) in &f.facts {
            self.facts.insert(k.clone(), v.clone());
        }
        for (pkg, values) in &f.settings {
            let entry = self.settings.entry(pkg.clone()).or_default();
            for (k, v) in values {
                entry.insert(k.clone(), v.clone());
            }
        }
    }
}
