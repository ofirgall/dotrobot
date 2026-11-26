//! The one place a `[providers.<id>]` table becomes a provider [`Spec`].

use dots_model::ProviderConfig;
use dots_providers::Spec;

/// The provider-native package (`native` is the `name` override or the package name) with its
/// typed options. `cfg` is `None` for a package that names no `[providers.<id>]` table.
pub fn spec_for(native: &str, cfg: Option<&ProviderConfig>) -> Spec {
    let mut s = Spec::new(native);
    let b = |v: bool| if v { "true" } else { "false" };
    match cfg {
        Some(ProviderConfig::Brew(c)) => {
            if c.cask {
                s = s.opt("cask", "true");
            }
            if let Some(t) = &c.tap {
                s = s.source(t);
            }
        }
        Some(ProviderConfig::Apt(c)) => {
            if let Some(p) = &c.ppa {
                s = s.source(p);
            }
        }
        Some(ProviderConfig::Cargo(c)) => {
            if let Some(l) = c.locked {
                s = s.opt("locked", b(l));
            }
            if let Some(g) = &c.git {
                s = s.opt("git", g);
            }
            if let Some(r) = &c.rev {
                s = s.opt("rev", r);
            }
            if !c.features.is_empty() {
                s = s.opt("features", &c.features.join(","));
            }
        }
        Some(ProviderConfig::Uv(c)) => {
            if let Some(k) = &c.kind {
                s = s.opt("kind", k);
            }
            if let Some(p) = &c.python {
                s = s.opt("python", p);
            }
            if let Some(r) = &c.requirements {
                s = s.opt("requirements", r);
            }
        }
        Some(ProviderConfig::Git(c)) => {
            if let Some(u) = &c.url {
                s = s.opt("url", u);
            }
            if let Some(r) = &c.rev {
                s = s.opt("rev", r);
            }
        }
        _ => {}
    }
    s
}
