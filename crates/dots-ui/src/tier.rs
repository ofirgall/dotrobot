//! Capability tier detection (terminal-ui "Rendering tiers").

use dots_core::{ColorChoice, Env};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Tier {
    Plain,
    Basic,
    Rich,
}

/// [`detect_tier_with`] without a `ui.toml`.
pub fn detect_tier(color: Option<ColorChoice>, env: &dyn Env, stdout_is_tty: bool) -> Tier {
    detect_tier_with(color, None, env, stdout_is_tty)
}

/// Precedence (flag > env > ui.toml > detection): the `--color` flag, `DOTS_UI`, plain triggers
/// (`NO_COLOR`, `TERM=dumb`, `CI`), `FORCE_COLOR`/`CLICOLOR_FORCE` when piped, then `ui.toml`
/// `color` (`always`/`never`), then the TTY and `COLORTERM` capabilities.
pub fn detect_tier_with(
    color: Option<ColorChoice>,
    config_color: Option<ColorChoice>,
    env: &dyn Env,
    stdout_is_tty: bool,
) -> Tier {
    let truecolor = matches!(
        env.get_str("COLORTERM").as_deref(),
        Some("truecolor" | "24bit")
    );
    let colored = if truecolor { Tier::Rich } else { Tier::Basic };

    match color {
        Some(ColorChoice::Never) => return Tier::Plain,
        Some(ColorChoice::Always) => return colored,
        _ => {}
    }
    match env.get_str("DOTS_UI").as_deref() {
        Some("plain") => return Tier::Plain,
        Some("basic") => return Tier::Basic,
        Some("rich") => return Tier::Rich,
        _ => {}
    }
    if env.get("NO_COLOR").is_some()
        || env.get_str("TERM").as_deref() == Some("dumb")
        || env.get_str("CI").is_some()
    {
        return Tier::Plain;
    }
    let forced = |k: &str| env.get_str(k).is_some_and(|v| v != "0");
    if forced("FORCE_COLOR") || forced("CLICOLOR_FORCE") {
        return colored;
    }
    match config_color {
        Some(ColorChoice::Never) => return Tier::Plain,
        Some(ColorChoice::Always) => return colored,
        _ => {}
    }
    if stdout_is_tty { colored } else { Tier::Plain }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dots_core::MapEnv;

    #[test]
    fn flag_beats_env() {
        let env = MapEnv::new()
            .with("NO_COLOR", "1")
            .with("COLORTERM", "truecolor");
        assert_eq!(
            detect_tier(Some(ColorChoice::Always), &env, false),
            Tier::Rich
        );
        assert_eq!(
            detect_tier(Some(ColorChoice::Never), &MapEnv::new(), true),
            Tier::Plain
        );
    }

    #[test]
    fn plain_triggers() {
        let tty = |e: MapEnv| detect_tier(None, &e, true);
        assert_eq!(tty(MapEnv::new().with("NO_COLOR", "1")), Tier::Plain);
        assert_eq!(tty(MapEnv::new().with("TERM", "dumb")), Tier::Plain);
        assert_eq!(tty(MapEnv::new().with("CI", "true")), Tier::Plain);
        assert_eq!(tty(MapEnv::new().with("DOTS_UI", "plain")), Tier::Plain);
        assert_eq!(detect_tier(None, &MapEnv::new(), false), Tier::Plain);
    }

    #[test]
    fn capable_terminals() {
        assert_eq!(
            detect_tier(None, &MapEnv::new().with("COLORTERM", "24bit"), true),
            Tier::Rich
        );
        assert_eq!(
            detect_tier(None, &MapEnv::new().with("TERM", "xterm"), true),
            Tier::Basic
        );
        let forced = MapEnv::new().with("FORCE_COLOR", "1");
        assert_eq!(detect_tier(None, &forced, false), Tier::Basic);
        assert_eq!(detect_tier(None, &forced, true), Tier::Basic);
        assert_eq!(
            detect_tier(None, &MapEnv::new().with("DOTS_UI", "rich"), false),
            Tier::Rich
        );
    }

    #[test]
    fn precedence_is_flag_then_env_then_config() {
        let tty = |flag, cfg, env: MapEnv| detect_tier_with(flag, cfg, &env, true);
        let never = Some(ColorChoice::Never);
        let always = Some(ColorChoice::Always);
        // config applies when nothing above it decides
        assert_eq!(tty(None, never, MapEnv::new()), Tier::Plain);
        assert_eq!(tty(None, always, MapEnv::new()), Tier::Basic);
        assert_eq!(
            detect_tier_with(None, always, &MapEnv::new(), false),
            Tier::Basic
        );
        // env beats config
        assert_eq!(
            tty(None, always, MapEnv::new().with("NO_COLOR", "1")),
            Tier::Plain
        );
        assert_eq!(
            tty(None, never, MapEnv::new().with("DOTS_UI", "basic")),
            Tier::Basic
        );
        // flag beats env and config
        assert_eq!(
            tty(always, never, MapEnv::new().with("NO_COLOR", "1")),
            Tier::Basic
        );
    }
}
