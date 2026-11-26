//! The schema of every file kind, as data for [`crate::schema`].

use crate::schema::Ty;

const STRS: Ty = Ty::List(&Ty::Str);
const STR_OR_PLATFORM_MAP: Ty = Ty::Alt(&[Ty::Str, Ty::Map(&Ty::Str)]);
const SETTING_VALUES: Ty = Ty::Map(&Ty::Map(&Ty::Any));

const FILE_MODES: &[&str] = &["link", "copy", "render", "inject", "fragment", "git", "dir"];
const FORMATS: &[&str] = &[
    "toml",
    "json",
    "jsonc",
    "yaml",
    "ini",
    "gitconfig",
    "markdown",
    "text",
];
const SETTING_TYPES: &[&str] = &[
    "bool",
    "int",
    "float",
    "string",
    "enum",
    "list<string>",
    "path",
];
const INTEGRITY: &[&str] = &["warn", "enforce"];

macro_rules! provider {
    ($( ($k:literal, $t:expr) ),* $(,)?) => {
        Ty::Obj(&[
            ("name", Ty::Str), ("bin", Ty::Str), ("bins", STRS),
            ("shim", Ty::Str), ("when", Ty::Str),
            $( ($k, $t), )*
        ])
    };
}

const BREW: Ty = provider!(("cask", Ty::Bool), ("tap", Ty::Str));
const APT: Ty = provider!(("ppa", Ty::Str));
const CARGO: Ty = provider!(
    ("locked", Ty::Bool),
    ("git", Ty::Str),
    ("rev", Ty::Str),
    ("features", STRS)
);
const UV: Ty = provider!(
    ("kind", Ty::Str),
    ("python", Ty::Str),
    ("requirements", Ty::Path)
);
const NPM: Ty = provider!();
const GIT: Ty = provider!(("url", Ty::Str), ("rev", Ty::Str));
const GITHUB_RELEASE: Ty = provider!(
    ("repo", Ty::Str),
    ("asset", Ty::Str),
    ("sha256", Ty::Str),
    ("version", Ty::Str)
);
const SCRIPT: Ty = provider!(
    ("check", Ty::Cmd),
    ("version", Ty::Cmd),
    ("latest", Ty::Cmd),
    ("install", Ty::Cmd),
    ("upgrade", Ty::Cmd),
    ("uninstall", Ty::Cmd),
    ("shell", Ty::Enum(&["bash", "sh", "pwsh"])),
    ("needs_root", Ty::Bool),
    ("interactive", Ty::Bool),
    ("versionless", Ty::Bool),
    ("timeout", Ty::Str)
);
const PLAIN: Ty = provider!(("id", Ty::Str));
const BROWSER_EXT: Ty = provider!(
    ("chrome", Ty::Str),
    ("edge", Ty::Str),
    ("firefox", Ty::Str),
    ("brave", Ty::Str),
    ("chromium", Ty::Str),
    ("browsers", STRS),
    ("profiles", STRS),
    ("method", Ty::Enum(&["external", "policy"])),
    ("managed", Ty::Open)
);
const AGENT_SKILL: Ty = provider!(
    ("src", Ty::Path),
    ("agents", STRS),
    ("frontmatter", Ty::Open)
);

/// Built-in provider ids with typed keys; any other id is an open table.
pub fn provider_ty(id: &str) -> Option<&'static Ty> {
    Some(match id {
        "brew" => &BREW,
        "apt" => &APT,
        "cargo" => &CARGO,
        "uv" => &UV,
        "npm" => &NPM,
        "git" => &GIT,
        "github-release" => &GITHUB_RELEASE,
        "script" => &SCRIPT,
        "winget" | "scoop" | "mas" | "pacman" | "dnf" => &PLAIN,
        "browser-ext" => &BROWSER_EXT,
        "agent-skill" => &AGENT_SKILL,
        _ => return None,
    })
}

/// Built-in provider ids that have typed tables.
pub const BUILTIN_PROVIDERS: &[&str] = &[
    "brew",
    "apt",
    "cargo",
    "uv",
    "npm",
    "git",
    "github-release",
    "script",
    "winget",
    "scoop",
    "mas",
    "pacman",
    "dnf",
    "browser-ext",
    "agent-skill",
];

const ORIGIN: Ty = Ty::Table(&[
    ("repo", Ty::Str),
    ("path", Ty::Str),
    ("rev", Ty::Str),
    ("tree", Ty::Str),
]);

const FILE_LAYER: Ty = Ty::Alt(&[Ty::Path, Ty::Table(&[("src", Ty::Path), ("when", Ty::Str)])]);

const FILE_ENTRY: Ty = Ty::Obj(&[
    ("dst", STR_OR_PLATFORM_MAP),
    ("mode", Ty::Enum(FILE_MODES)),
    ("src", Ty::Path),
    (
        "layers",
        Ty::Alt(&[
            Ty::List(&FILE_LAYER),
            Ty::Table(&[("dir", Ty::Path), ("auto", Ty::Bool)]),
        ]),
    ),
    ("format", Ty::Enum(FORMATS)),
    ("kind", Ty::Str),
    ("template", Ty::Str),
    ("when", Ty::Str),
    ("url", Ty::Str),
    ("rev", Ty::Str),
    ("priority", Ty::Int),
    (
        "set",
        Ty::List(&Ty::Table(&[
            ("path", Ty::Str),
            ("value", Ty::Any),
            ("delete", Ty::Bool),
            ("when", Ty::Str),
        ])),
    ),
    ("merge", Ty::Map(&Ty::Any)),
]);

const REQUIRE: Ty = Ty::Alt(&[
    Ty::Str,
    Ty::Table(&[("name", Ty::Str), ("version", Ty::Str), ("when", Ty::Str)]),
]);

const SETTING_DECL: Ty = Ty::Obj(&[
    ("type", Ty::Enum(SETTING_TYPES)),
    ("default", Ty::Any),
    ("range", Ty::List(&Ty::Float)),
    ("description", Ty::Str),
    ("ask", Ty::Bool),
    ("question", Ty::Str),
    ("when", Ty::Str),
    ("required", Ty::Bool),
    ("secret", Ty::Bool),
    ("validate", Ty::Str),
    ("values", STRS),
]);

pub const PACKAGE: Ty = Ty::Obj(&[
    ("schema", Ty::Int),
    ("name", Ty::Str),
    ("description", Ty::Str),
    ("when", Ty::Str),
    ("bin", Ty::Str),
    ("bins", STRS),
    ("version", Ty::Str),
    ("requires", Ty::List(&REQUIRE)),
    ("after", STRS),
    ("conflicts", STRS),
    ("replaces", STRS),
    ("prefer", STRS),
    ("providers_only", Ty::Bool),
    ("providers", Ty::Providers),
    ("files", Ty::List(&FILE_ENTRY)),
    ("settings", Ty::Map(&SETTING_DECL)),
    (
        "env",
        Ty::Table(&[
            ("path", STRS),
            ("vars", Ty::Map(&Ty::Str)),
            ("source", STRS),
        ]),
    ),
    (
        "hooks",
        Ty::Table(&[
            ("pre_install", Ty::Cmd),
            ("post_install", Ty::Cmd),
            ("pre_files", Ty::Cmd),
            ("post_files", Ty::Cmd),
        ]),
    ),
    (
        "integrity",
        Ty::Alt(&[Ty::Enum(INTEGRITY), Ty::Map(&Ty::Str)]),
    ),
    ("origin", ORIGIN),
]);

pub const BUNDLE: Ty = Ty::Obj(&[
    ("schema", Ty::Int),
    ("description", Ty::Str),
    ("include", STRS),
    (
        "packages",
        Ty::List(&Ty::Alt(&[
            Ty::Str,
            Ty::Obj(&[("name", Ty::Str), ("when", Ty::Str)]),
        ])),
    ),
    ("settings", SETTING_VALUES),
    ("recommended", Ty::Bool),
    ("integrity", Ty::Enum(INTEGRITY)),
]);

const UPSTREAM: Ty = Ty::Table(&[
    ("alias", Ty::Str),
    ("url", Ty::Str),
    ("follow", Ty::Str),
    ("import", STRS),
    ("rename", Ty::Map(&Ty::Str)),
    ("trust", Ty::Enum(&["review", "auto"])),
    ("scripts", Ty::Enum(&["allow", "deny"])),
    ("integrity", Ty::Enum(INTEGRITY)),
]);

pub const REPO: Ty = Ty::Obj(&[
    ("schema", Ty::Int),
    ("requires_dots", Ty::DotsReq),
    ("name", Ty::Str),
    ("priority", Ty::Map(&STRS)),
    (
        "tags",
        Ty::Map(&Ty::Alt(&[
            Ty::Str,
            Ty::Table(&[
                ("description", Ty::Str),
                ("recommended", Ty::Alt(&[Ty::Bool, Ty::Map(&Ty::Bool)])),
            ]),
        ])),
    ),
    ("paths", Ty::Map(&STR_OR_PLATFORM_MAP)),
    ("platforms", Ty::Map(&Ty::Table(&[("parent", Ty::Str)]))),
    ("providers", Ty::Map(&Ty::Open)),
    ("provider", Ty::Map(&Ty::Open)),
    ("settings", SETTING_VALUES),
    ("upstream", Ty::List(&UPSTREAM)),
    ("recommended", Ty::Alt(&[Ty::Bool, STRS])),
]);

pub const MACHINE: Ty = Ty::Table(&[
    ("schema", Ty::Int),
    ("root", Ty::Str),
    ("profile", Ty::Str),
    ("bundles", Ty::ListEdit(&Ty::Str)),
    ("tags", Ty::ListEdit(&Ty::Str)),
    ("exclude", Ty::ListEdit(&Ty::Str)),
    ("platform_extra", Ty::ListEdit(&Ty::Str)),
    ("facts", Ty::Map(&Ty::Any)),
    ("settings", SETTING_VALUES),
]);
