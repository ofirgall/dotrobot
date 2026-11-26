//! The one place a `[providers.<id>]` table becomes a provider [`Spec`].

use std::path::Path;

use dots_model::ProviderConfig;
use dots_providers::Spec;

/// The provider-native package (`native` is the `name` override or the package name) with its
/// typed options. `cfg` is `None` for a package that names no `[providers.<id>]` table.
pub fn spec_for(native: &str, cfg: Option<&ProviderConfig>) -> Spec {
    spec_for_in(native, cfg, None)
}

/// Like [`spec_for`], with the package directory (`pkg_dir` option) that a cargo or uv `path`
/// source is relative to.
pub fn spec_for_in(native: &str, cfg: Option<&ProviderConfig>, pkg_dir: Option<&Path>) -> Spec {
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
            if let Some(r) = &c.repo {
                for (k, v) in [
                    ("repo.ppa", &r.ppa),
                    ("repo.url", &r.url),
                    ("repo.suite", &r.suite),
                    ("repo.key", &r.key),
                    ("repo.fingerprint", &r.fingerprint),
                ] {
                    if let Some(v) = v {
                        s = s.opt(k, v);
                    }
                }
                match &r.components {
                    Some(c) if c.is_empty() => s = s.opt("repo.flat", "true"),
                    Some(c) => s = s.opt("repo.components", &c.join(",")),
                    None => {}
                }
                if r.flat {
                    s = s.opt("repo.flat", "true");
                }
            }
            if let Some(p) = &c.pin {
                if let Some(o) = &p.origin {
                    s = s.opt("pin.origin", o);
                }
                if let Some(n) = p.priority {
                    s = s.opt("pin.priority", &n.to_string());
                }
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
            if let Some(p) = &c.path {
                s = s.opt("path", p);
            }
            if c.git_fetch_with_cli {
                s = s.opt("git_fetch_with_cli", "true");
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
            if let Some(p) = &c.path {
                s = s.opt("path", p);
            }
            if let Some(g) = &c.git {
                s = s.opt("git", g);
            }
            if let Some(r) = &c.rev {
                s = s.opt("rev", r);
            }
            if c.git_fetch_with_cli {
                s = s.opt("git_fetch_with_cli", "true");
            }
            if c.break_system_packages {
                s = s.opt("break_system_packages", "true");
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
    if let Some(d) = pkg_dir
        && (s.opts.contains_key("path") || s.opts.contains_key("requirements"))
    {
        s = s.opt("pkg_dir", &d.to_string_lossy());
    }
    s
}
