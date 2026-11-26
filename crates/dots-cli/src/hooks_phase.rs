//! The hooks phase: package `[hooks]` commands run through the script provider's runtime, so
//! a hook sees exactly what a script command sees (shell, working directory, `DOTS_*`).

use std::path::Path;

use dots_core::Dirs;
use dots_engine::{HookRequest, HooksPhase};
use dots_facts::Facts;
use dots_providers::script::{ScriptConfig, ScriptEnv, ScriptProvider};
use dots_providers::{Cmd, Ctx};

pub struct RepoHooks<'a> {
    pub facts: &'a Facts,
    pub dirs: &'a Dirs,
    pub root: &'a Path,
    pub tags: &'a [String],
    pub settings: &'a dots_config::Settings,
}

impl HooksPhase for RepoHooks<'_> {
    fn command(&self, req: &HookRequest<'_>, ctx: &Ctx) -> Result<Cmd, String> {
        let env = ScriptEnv {
            home: self.dirs.home.clone(),
            pkg_dir: req.pkg_dir.to_path_buf(),
            root: self.root.to_path_buf(),
            state: self.dirs.state.clone(),
            bin_dir: self.dirs.home.join(".local/bin"),
            lib_dir: self.root.join("lib"),
            tags: self.tags.to_vec(),
            settings: crate::providers::script_settings(self.settings, req.package),
            path_prefix: Vec::new(),
        };
        // Only the runtime (shell, environment) of the script provider is used; the install
        // and check commands are never run.
        let cfg = ScriptConfig {
            check: Some("true".into()),
            install: Some("true".into()),
            versionless: true,
            ..ScriptConfig::default()
        };
        let p =
            ScriptProvider::new(req.package, cfg, env, self.facts).map_err(|e| e.to_string())?;
        let mut cmd = p.hook_cmd(ctx, req.command).map_err(|e| e.to_string())?;
        // Hooks act on the user's files, so `~` is the home dots writes to (`--home` redirects
        // it; `ScriptEnv::home` sets HOME); provider tools keep the real HOME, hooks do not.
        cmd.env.insert("DOTS_HOOK".into(), req.event.into());
        Ok(cmd)
    }
}
