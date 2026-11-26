//! Directives (`dots:if` ...), `@dots()@` and `[[files.set]]`.
mod common;

use common::*;
use dots_files::*;
use serde_json::json;

fn line_of(d: &dots_core::Diagnostic) -> Option<u32> {
    d.provenance.iter().find_map(|p| p.line)
}

#[test]
fn inline_setting() {
    let e = env();
    let out = e
        .one(
            "text",
            "font-family = Mono\nfont-size = @dots(setting.font_size)@\n",
        )
        .unwrap();
    assert!(out.contains("font-size = 15\n"), "{out}");
    // The header names the value and its scope.
    assert!(out.contains("font_size=15(machine)"), "{out}");
    assert!(
        !out.contains("theme="),
        "only used settings are listed: {out}"
    );
}

#[test]
fn unknown_name_reports_file_line_and_suggestion() {
    let e = env();
    let d = e
        .one("text", "a = 1\nfont-size = @dots(setting.nope)@\n")
        .unwrap_err();
    assert_eq!(d.code, "unknown-name");
    assert_eq!(line_of(&d), Some(2));
    assert!(
        d.message.contains("l/one:2") || d.message.contains("one:2"),
        "{}",
        d.message
    );
    let d = e
        .one("text", "x = @dots(setting.font_sise)@\n")
        .unwrap_err();
    assert_eq!(d.code, "unknown-name");
    assert!(d.hint.contains("setting.font_size"), "{}", d.hint);
    for bad in ["fact.nope", "path.nope", "bogus.x", "settings.other.x"] {
        let d = e.one("text", &format!("x = @dots({bad})@\n")).unwrap_err();
        assert_eq!(d.code, "unknown-name", "{bad}");
    }
}

#[test]
fn names_facts_tags_paths_and_other_packages() {
    let mut e = env();
    e.write("l/n", "h=@dots(fact.hostname)@ w=@dots(tag.work)@ x=@dots(tag.nope)@ p=@dots(path.home)@ s=@dots(settings.other.volume)@\n");
    let f = e.file("pkg", "out", &["l/n"], "text");
    let mut ctx = e.ctx("r");
    ctx.settings
        .push(setting("other.volume", json!(7), "machine"));
    let out = render_layers(&f, &ctx).unwrap();
    let home = e.home.display();
    assert!(
        out.contains(&format!("h=box w=true x=false p={home} s=7")),
        "{out}"
    );
    assert!(out.contains("other.volume=7(machine)"), "{out}");
    e.root.clear();
}

#[test]
fn if_elif_else_end() {
    let e = env();
    let src = "\
a
# dots:if setting.theme = \"light\"
light
# dots:elif setting.theme = \"auto\"
auto
# dots:else
dark
# dots:end
z
";
    assert_eq!(body(&e.one("text", src).unwrap()), "a\nauto\nz");
    let src = "# dots:if setting.theme = \"light\"\nlight\n# dots:else\ndark\n# dots:end\n";
    assert_eq!(body(&e.one("text", src).unwrap()), "dark");
}

#[test]
fn conditions_use_the_condition_language() {
    let e = env();
    let src = "\
# dots:if all(os = \"macos\", setting.cursor_shader)
shader
# dots:end
# dots:if os = \"linux\"
linux
# dots:end
# dots:if not(setting.telemetry)
quiet
# dots:end
# dots:if setting.font_size > 12
big
# dots:end
";
    assert_eq!(body(&e.one("text", src).unwrap()), "shader\nquiet\nbig");
}

#[test]
fn nesting_and_inactive_branches_are_not_evaluated() {
    let e = env();
    let src = "\
# dots:if os = \"linux\"
# dots:if setting.nope
x
# dots:end
@dots(setting.nope)@
# dots:else
# dots:if setting.cursor_shader
ok
# dots:end
# dots:end
";
    assert_eq!(body(&e.one("text", src).unwrap()), "ok");
}

#[test]
fn if_bang_uncomments_the_body() {
    let e = env();
    let src = "\
# dots:if! setting.cursor_shader
# custom-shader = a.glsl
#   indented = 1
#
# dots:else
plain
# dots:end
# dots:if! os = \"linux\"
# nope
# dots:end
";
    assert_eq!(
        body(&e.one("text", src).unwrap()),
        "custom-shader = a.glsl\n  indented = 1\n"
    );
    // A body line that is not commented is an error, active or not.
    let d = e
        .one("text", "# dots:if! os = \"linux\"\nplain = 1\n# dots:end\n")
        .unwrap_err();
    assert_eq!(d.code, "directive-error");
    assert_eq!(line_of(&d), Some(2));
}

#[test]
fn comment_syntax_per_format() {
    let e = env();
    let jsonc = "{\n  // dots:if setting.cursor_shader\n  \"a\": 1,\n  // dots:end\n  \"b\": @dots(setting.font_size)@\n}\n";
    let out = e.one("jsonc", jsonc).unwrap();
    assert!(
        out.contains("\"a\": 1") && out.contains("\"b\": 15"),
        "{out}"
    );
    assert!(!out.contains("dots:"));
    let ini = "; dots:if os = \"macos\"\na = 1\n; dots:end\n# dots:if os = \"linux\"\nb = 2\n# dots:end\n";
    assert_eq!(body(&e.one("ini", ini).unwrap()).trim(), "a = 1");
    let md =
        "<!-- dots:if setting.cursor_shader -->\nyes\n<!-- dots:else -->\nno\n<!-- dots:end -->\n";
    assert!(body(&e.one("markdown", md).unwrap()).contains("yes"));
    let md = "<!-- dots:if! setting.cursor_shader -->\n<!-- shown -->\n<!-- dots:end -->\n";
    assert!(e.one("markdown", md).unwrap().contains("\nshown\n"));
    // `comment` on the entry sets the leader for anything unusual.
    e.write(
        "l/lua",
        "-- dots:if setting.cursor_shader\nshader()\n-- dots:end\nother()\n",
    );
    let mut f = e.file("pkg", "out", &["l/lua"], "text");
    f.comment = "--".into();
    let out = render_layers(&f, &e.ctx("r")).unwrap();
    assert!(out.starts_with("-- Generated by dots"), "{out}");
    assert!(out.ends_with("shader()\nother()\n"), "{out}");
}

#[test]
fn comments_that_only_mention_dots_are_left_alone() {
    let e = env();
    let src = "# dots: this is fine\n# --- dots: from x ---\n# see dots:/path\nkeep\n";
    let out = e.one("text", src).unwrap();
    assert!(
        out.contains("# dots: this is fine") && out.contains("keep"),
        "{out}"
    );
}

#[test]
fn include_is_recursive_and_confined() {
    let e = env();
    e.write(
        "l/inc/a",
        "A1\n# dots:include b\n@dots(setting.font_size)@\n",
    );
    e.write(
        "l/inc/b",
        "B\n# dots:if setting.cursor_shader\nB-yes\n# dots:end\n",
    );
    e.write("l/main", "top\n# dots:include inc/a\nbottom\n");
    let f = e.file("pkg", "out", &["l/main"], "text");
    // `b` is relative to `inc/`, the including file's directory.
    let out = render_layers(&f, &e.ctx("r")).unwrap();
    assert_eq!(body(&out), "top\nA1\nB\nB-yes\n15\nbottom");
    for bad in ["../x", "/etc/passwd", "missing"] {
        let d = e
            .one("text", &format!("# dots:include {bad}\n"))
            .unwrap_err();
        assert_eq!(d.code, "directive-error", "{bad}");
        assert_eq!(line_of(&d), Some(1));
    }
    // An error inside an included file names that file.
    e.write("l/inc/bad", "ok\n@dots(setting.nope)@\n");
    let d = e.one("text", "# dots:include inc/bad\n").unwrap_err();
    assert_eq!(d.code, "unknown-name");
    assert!(d.message.contains("inc/bad:2"), "{}", d.message);
    // A cycle ends at the depth limit.
    e.write("l/loop", "# dots:include loop\n");
    let d = e.one("text", "# dots:include loop\n").unwrap_err();
    assert_eq!(d.code, "directive-error");
}

#[test]
fn structural_errors_report_file_and_line() {
    let e = env();
    let cases = [
        ("a\n# dots:end\n", 2),
        ("# dots:if os = \"macos\"\nx\n", 1),
        ("a\n# dots:else\n", 2),
        ("a\n# dots:elif os = \"macos\"\n", 2),
        (
            "# dots:if os = \"macos\"\n# dots:else\n# dots:elif os = \"macos\"\n# dots:end\n",
            3,
        ),
        (
            "# dots:if os = \"macos\"\n# dots:else\n# dots:else\n# dots:end\n",
            3,
        ),
        ("x\n# dots:bogus\n", 2),
        ("# dots:if\n# dots:end\n", 1),
        ("# dots:end now\n", 1),
        ("x = @dots(setting.font_size\n", 1),
        ("x = @dots(setting.font_size | rot13)@\n", 1),
    ];
    for (src, line) in cases {
        let d = e.one("text", src).unwrap_err();
        assert_eq!(d.code, "directive-error", "{src:?}: {}", d.message);
        assert_eq!(line_of(&d), Some(line), "{src:?}");
        assert!(d.message.contains(&format!("one:{line}")), "{}", d.message);
    }
    let deep = "# dots:if os = \"macos\"\n".repeat(9) + &"# dots:end\n".repeat(9);
    assert_eq!(e.one("text", &deep).unwrap_err().code, "directive-error");
    let ok = "# dots:if os = \"macos\"\n".repeat(8) + "x\n" + &"# dots:end\n".repeat(8);
    assert!(e.one("text", &ok).is_ok());
}

#[test]
fn bad_condition_reports_the_line() {
    let e = env();
    let d = e
        .one("text", "a\n# dots:if os =\n# dots:end\n")
        .unwrap_err();
    assert_eq!(d.code, "condition-syntax");
    assert_eq!(line_of(&d), Some(2));
    let d = e
        .one("text", "# dots:if setting.nope\n# dots:end\n")
        .unwrap_err();
    assert_eq!(d.code, "setting-ref-unresolved");
    let d = e
        .one("text", "# dots:if tag.nope\n# dots:end\n")
        .unwrap_err();
    assert!(d.code.contains("unknown"), "{}", d.code);
}

#[test]
fn directives_are_errors_in_strict_json() {
    let e = env();
    let d = e
        .one("json", "{\n  \"a\": @dots(setting.font_size)@\n}\n")
        .unwrap_err();
    assert_eq!(d.code, "directive-error");
    assert_eq!(line_of(&d), Some(2));
    let d = e
        .one("json", "// dots:if os = \"macos\"\n{}\n")
        .unwrap_err();
    assert_eq!(d.code, "directive-error");
    // Plain JSON passes through.
    assert!(e.one("json", "{\"a\": 1}\n").is_ok());
}

#[test]
fn filters() {
    let e = env();
    let src = "\
n=@dots(setting.name)@
j=@dots(setting.name | json)@
t=@dots(setting.name | toml)@
y=@dots(setting.theme | yaml)@
s=@dots(setting.name | shell)@
u=@dots(setting.theme | upper)@
l=@dots(setting.name | upper | lower)@
b=@dots(setting.cursor_shader | json)@ @dots(setting.cursor_shader | shell)@
i=@dots(setting.font_size | toml)@ @dots(setting.font_size | yaml)@
q=@dots(setting.theme|shell)@
";
    let out = body(&e.one("text", src).unwrap());
    assert_eq!(
        out,
        "n=it's me\nj=\"it's me\"\nt=\"it's me\"\ny=auto\ns='it'\\''s me'\nu=AUTO\nl=it's me\nb=true true\ni=15 15\nq=auto"
    );
}

#[test]
fn escape_writes_a_literal() {
    let e = env();
    let out = e
        .one(
            "text",
            "a = @@dots(setting.x)@ b = @dots(setting.font_size)@\n",
        )
        .unwrap();
    assert!(out.contains("a = @dots(setting.x)@ b = 15"), "{out}");
}

#[test]
fn layers_are_preprocessed_before_merging() {
    let e = env();
    e.write(
        "l/base.toml",
        "[ui]\nsize = 1\n# dots:if setting.cursor_shader\nshader = true\n# dots:end\n",
    );
    e.write(
        "l/over.toml",
        "[ui]\nsize = @dots(setting.font_size)@\n# dots:if os = \"linux\"\nextra = 1\n# dots:end\n",
    );
    let f = e.file("pkg", "out.toml", &["l/base.toml", "l/over.toml"], "toml");
    let out = body(&render_layers(&f, &e.ctx("r")).unwrap());
    assert_eq!(out.trim(), "[ui]\nsize = 15\nshader = true");
}

// ----------------------------------------------------------------------------- [[files.set]]

fn patch(
    path: &str,
    value: Option<serde_json::Value>,
    delete: bool,
    when: Option<&str>,
) -> SetPatch {
    SetPatch {
        path: path.into(),
        value,
        delete,
        when: when.map(str::to_string),
    }
}

fn render_set(
    e: &Env,
    format: &str,
    text: &str,
    set: Vec<SetPatch>,
) -> Result<String, dots_core::Diagnostic> {
    e.write("l/s", text);
    let mut f = e.file("pkg", "out", &["l/s"], format);
    f.extras.set = set;
    render_layers(&f, &e.ctx("r"))
}

#[test]
fn delete_by_condition() {
    let e = env();
    let src = r#"{"env": {"DISABLE_TELEMETRY": "1", "KEEP": "1"}, "a": 1}"#;
    let del = || {
        patch(
            "env.DISABLE_TELEMETRY",
            None,
            true,
            Some("not(setting.telemetry)"),
        )
    };
    let out = render_set(&e, "json", src, vec![del()]).unwrap();
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v, json!({"env": {"KEEP": "1"}, "a": 1}));
    // With telemetry on the condition is false and the key stays.
    let mut ctx = e.ctx("r");
    ctx.settings
        .iter_mut()
        .find(|s| s.name == "telemetry")
        .unwrap()
        .value = json!(true);
    e.write("l/s", src);
    let mut f = e.file("pkg", "out", &["l/s"], "json");
    f.extras.set = vec![del()];
    let v: serde_json::Value = serde_json::from_str(&render_layers(&f, &ctx).unwrap()).unwrap();
    assert_eq!(v["env"]["DISABLE_TELEMETRY"], "1");
    // Deleting through a missing parent is a no-op.
    assert!(render_set(&e, "json", "{}", vec![patch("x.y.z", None, true, None)]).is_ok());
}

#[test]
fn set_values_follow_the_setting_type_and_apply_in_order() {
    let e = env();
    let set = vec![
        patch(
            "size",
            Some(json!("@dots(setting.font_size)@")),
            false,
            None,
        ),
        patch("cwd", Some(json!("@dots(path.home)@/bin")), false, None),
        patch(
            "on",
            Some(json!("@dots(setting.cursor_shader)@")),
            false,
            Some("setting.font_size >= 15"),
        ),
        patch("off", Some(json!(1)), false, Some("setting.font_size < 15")),
        patch(
            "deep.er.key",
            Some(json!({"a": [1, "@dots(setting.theme | upper)@"]})),
            false,
            None,
        ),
        patch(
            "size",
            Some(json!("@dots(setting.font_size | json)@")),
            false,
            None,
        ),
        patch("a", None, true, None),
    ];
    let out = render_set(&e, "json", r#"{"a": 1, "keep": true}"#, set.clone()).unwrap();
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["cwd"], format!("{}/bin", e.home.display()));
    assert_eq!(v["on"], true);
    assert!(v.get("off").is_none() && v.get("a").is_none());
    assert_eq!(v["deep"]["er"]["key"], json!({"a": [1, "AUTO"]}));
    assert_eq!(v["keep"], true);
    // The last assignment to `size` wins; with a filter it becomes text.
    assert_eq!(v["size"], "15");
    // The same patches over TOML, YAML and INI.
    let out = render_set(
        &e,
        "toml",
        "# keep me\na = 1\n[x]\nb = 2 # note\n",
        set[..2].to_vec(),
    )
    .unwrap();
    assert!(
        out.contains("# keep me") && out.contains("b = 2 # note") && out.contains("size = 15"),
        "{out}"
    );
    let out = render_set(&e, "yaml", "a: 1\n", set[..1].to_vec()).unwrap();
    assert!(out.contains("size: 15"), "{out}");
    let out = render_set(
        &e,
        "gitconfig",
        "[core]\n\teditor = vi\n",
        vec![
            patch("core.editor", Some(json!("nvim")), false, None),
            patch("user.name", Some(json!("me")), false, None),
        ],
    )
    .unwrap();
    assert!(
        out.contains("editor = nvim") && out.contains("[user]\n\tname = me"),
        "{out}"
    );
}

#[test]
fn quoted_path_segments() {
    let e = env();
    let out = render_set(
        &e,
        "json",
        "{}",
        vec![patch("\"a.b\".c", Some(json!(1)), false, None)],
    )
    .unwrap();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&out).unwrap(),
        json!({"a.b": {"c": 1}})
    );
    let d = render_set(
        &e,
        "json",
        "{}",
        vec![patch("a..b", Some(json!(1)), false, None)],
    )
    .unwrap_err();
    assert_eq!(d.code, "schema-invalid-value");
}

#[test]
fn a_path_crossing_a_non_table_is_an_error() {
    let e = env();
    for (fmt, src) in [
        ("json", r#"{"a": {"b": 1}}"#),
        ("toml", "[a]\nb = 1\n"),
        ("yaml", "a:\n  b: 1\n"),
    ] {
        let d = render_set(
            &e,
            fmt,
            src,
            vec![patch("a.b.c", Some(json!(1)), false, None)],
        )
        .unwrap_err();
        assert_eq!(d.code, "set-path-not-table", "{fmt}");
        assert!(
            d.message.contains("a.b.c") && d.message.contains("a.b"),
            "{}",
            d.message
        );
    }
    let d = render_set(
        &e,
        "json",
        r#"{"a": [1]}"#,
        vec![patch("a.x", None, true, None)],
    )
    .unwrap_err();
    assert_eq!(d.code, "set-path-not-table");
}

#[test]
fn set_needs_a_value_or_delete_and_a_structured_format() {
    let e = env();
    let d = render_set(&e, "json", "{}", vec![patch("a", None, false, None)]).unwrap_err();
    assert_eq!(d.code, "schema-invalid-value");
    let d = render_set(
        &e,
        "text",
        "x\n",
        vec![patch("a", Some(json!(1)), false, None)],
    )
    .unwrap_err();
    assert_eq!(d.code, "schema-invalid-value");
    // A patch whose `when` is false does not make a text file an error.
    assert!(
        render_set(
            &e,
            "text",
            "x\n",
            vec![patch("a", Some(json!(1)), false, Some("os = \"linux\""))]
        )
        .is_ok()
    );
    let d = render_set(
        &e,
        "json",
        "{}",
        vec![patch("a", Some(json!("@dots(setting.nope)@")), false, None)],
    )
    .unwrap_err();
    assert_eq!(d.code, "unknown-name");
}

#[test]
fn markdown_frontmatter_is_patched_as_frontmatter() {
    let e = env();
    let src = "---\nname: x\ndisable-model-invocation: true\n---\nbody\n";
    let out = render_set(
        &e,
        "markdown",
        src,
        vec![
            patch("frontmatter.disable-model-invocation", None, true, None),
            patch(
                "frontmatter.model",
                Some(json!("@dots(setting.theme)@")),
                false,
                None,
            ),
        ],
    )
    .unwrap();
    assert!(out.starts_with("---\n"), "{out}");
    assert!(
        out.contains("name: x") && out.contains("model: auto") && !out.contains("disable-model"),
        "{out}"
    );
    assert!(out.contains("body"));
}

#[test]
fn patches_use_settings_in_the_header() {
    let e = env();
    let out = render_set(
        &e,
        "toml",
        "a = 1\n",
        vec![patch(
            "s",
            Some(json!("@dots(setting.font_size)@")),
            false,
            None,
        )],
    )
    .unwrap();
    assert!(out.contains("font_size=15(machine)"), "{out}");
}

#[test]
fn resolve_entry_carries_set_comment_and_layers() {
    let e = env();
    e.write("pkg/cfg.toml", "a = 1\n");
    let entry: dots_model::FileEntry = serde_json::from_value(json!({
        "dst": "~/.x.toml", "mode": "render", "src": "cfg.toml", "comment": ";",
        "set": [{"path": "a", "value": 2, "when": "os = \"macos\""}, {"path": "b", "delete": true}],
    }))
    .unwrap();
    let f = resolve_entry(
        "pkg",
        &e.root.join("pkg"),
        &entry,
        &e.ctx("r"),
        &|_| Ok(true),
        &[],
    )
    .unwrap()
    .unwrap();
    assert_eq!(f.comment, ";");
    assert_eq!(f.extras.set.len(), 2);
    assert_eq!(f.extras.set[0].value, Some(json!(2)));
    assert!(f.extras.set[1].delete);
    let out = render_layers(&f, &e.ctx("r")).unwrap();
    assert!(out.contains("a = 2"), "{out}");
}
