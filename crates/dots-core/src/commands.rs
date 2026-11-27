//! The v1 top-level command list and the per-command JSON Schemas (`schemas/`).

use serde_json::{Value, json};

use crate::report::{SCHEMA_VERSION, schema_id};

/// Every top-level command, in `cli-surface` order.
pub const COMMANDS: &[&str] = &[
    "init",
    "apply",
    "plan",
    "status",
    "outdated",
    "why",
    "which",
    "bins",
    "doctor",
    "facts",
    "upgrade",
    "pin",
    "unpin",
    "remove",
    "migrate",
    "add",
    "search",
    "sources",
    "pull",
    "eject",
    "uneject",
    "todo",
    "adopt",
    "try",
    "use",
    "roots",
    "diff",
    "log",
    "verify",
    "restore",
    "config",
    "questions",
    "show",
    "override",
    "skill",
    "lint",
    "lock",
    "import",
    "completions",
];

/// Pseudo-command for usage errors where no command could be recognised (`dots/usage@1`).
pub const USAGE: &str = "usage";

/// Every command that has a schema file: the real commands plus `usage`.
pub fn schema_commands() -> impl Iterator<Item = &'static str> {
    COMMANDS.iter().copied().chain(std::iter::once(USAGE))
}

pub fn schema_file_name(command: &str) -> String {
    format!("dots-{command}.schema.json")
}

/// JSON Schema for one command's envelope. `data` is open until the command is implemented;
/// later waves tighten it (additive changes do not bump the version).
pub fn schema_for(command: &str) -> Value {
    let diagnostic = json!({
        "type": "object",
        "required": ["code", "message", "hint", "provenance"],
        "properties": {
            "code": { "type": "string", "pattern": "^[a-z][a-z0-9]*(-[a-z0-9]+)*$" },
            "message": { "type": "string" },
            "hint": { "type": "string" },
            "provenance": {
                "type": "array",
                "items": {
                    "type": "object",
                    "required": ["source"],
                    "properties": {
                        "source": { "type": "string" },
                        "file": { "type": ["string", "null"] },
                        "line": { "type": ["integer", "null"] },
                        "col": { "type": ["integer", "null"] }
                    }
                }
            }
        }
    });
    let data = match command {
        "facts" => facts_data_schema(),
        "why" => why_data_schema(),
        "lint" => lint_data_schema(),
        "init" => init_data_schema(),
        "plan" => plan_data_schema(),
        "apply" => apply_data_schema(),
        "status" => status_data_schema(),
        "outdated" => outdated_data_schema(),
        "bins" => bins_data_schema(),
        "which" => which_data_schema(),
        "doctor" => doctor_data_schema(),
        "config" => config_data_schema(),
        "questions" => questions_data_schema(),
        "show" => show_data_schema(),
        "override" => override_data_schema(),
        "upgrade" => upgrade_data_schema(),
        "pin" | "unpin" => pin_data_schema(),
        "lock" => lock_data_schema(),
        "migrate" => migrate_data_schema(),
        "adopt" => adopt_data_schema(),
        _ => json!({ "type": "object" }),
    };
    json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "$id": format!("https://dots.dev/schemas/dots-{command}.schema.json"),
        "title": format!("dots {command} --json (schema version {SCHEMA_VERSION})"),
        "type": "object",
        "required": ["schema", "ok", "data", "warnings", "errors"],
        "properties": {
            "schema": { "const": schema_id(command) },
            "ok": { "type": "boolean" },
            "data": data,
            "warnings": { "type": "array", "items": diagnostic },
            "errors": { "type": "array", "items": diagnostic }
        }
    })
}

/// `dots facts` data: every fact with its source, the platform chain, the arch and the tags.
fn facts_data_schema() -> Value {
    json!({
        "type": "object",
        "required": ["facts", "platform_chain", "arch", "tags"],
        "properties": {
            "facts": {
                "type": "object",
                "additionalProperties": {
                    "type": "object",
                    "required": ["value", "source"],
                    "properties": {
                        "value": { "type": ["string", "boolean", "number"] },
                        "source": { "enum": ["detected", "overridden", "declared"] },
                        "detail": { "type": "string" }
                    }
                }
            },
            "platform_chain": { "type": "array", "items": { "type": "string" } },
            "arch": { "type": "string" },
            "tags": { "type": "array", "items": { "type": "string" } }
        }
    })
}

/// `dots why` data: the package's state, every inclusion path, the provider decision and the
/// version constraints with their sources.
fn why_data_schema() -> Value {
    let step = json!({
        "type": "object",
        "required": ["kind", "label"],
        "properties": {
            "kind": { "enum": ["machine", "bundle", "package", "requires", "bootstrap"] },
            "label": { "type": "string" },
            "name": { "type": "string" },
            "version": { "type": "string" }
        }
    });
    let path = json!({
        "type": "object",
        "required": ["text", "active", "steps", "conditions"],
        "properties": {
            "text": { "type": "string" },
            "active": { "type": "boolean" },
            "steps": { "type": "array", "items": step },
            "conditions": {
                "type": "array",
                "items": {
                    "type": "object",
                    "required": ["on", "when", "result"],
                    "properties": {
                        "on": { "type": "string" },
                        "when": { "type": "string" },
                        "result": { "type": "boolean" }
                    }
                }
            }
        }
    });
    let candidate = json!({
        "type": "object",
        "required": ["id", "outcome"],
        "properties": {
            "id": { "type": "string" },
            "outcome": { "enum": ["chosen", "standby", "skipped"] },
            "skip": { "enum": ["when-false", "unavailable", "needs-root", "cannot-satisfy", "bootstrap"] },
            "reason": { "type": "string" },
            "installed": { "type": ["string", "null"] }
        }
    });
    let provider = json!({
        "type": ["object", "null"],
        "required": ["chosen", "ranking", "candidates", "reasons"],
        "properties": {
            "chosen": {
                "type": ["object", "null"],
                "required": ["id", "name", "sticky"],
                "properties": {
                    "id": { "type": "string" },
                    "name": { "type": "string" },
                    "sticky": { "type": "boolean" },
                    "bootstrap": { "type": ["string", "null"] }
                }
            },
            "ranking": { "type": "string" },
            "candidates": { "type": "array", "items": candidate },
            "reasons": { "type": "array", "items": { "type": "string" } },
            "hint": { "type": ["string", "null"] }
        }
    });
    json!({
        "type": "object",
        "required": ["package", "status", "paths", "provider", "constraints"],
        "properties": {
            "package": { "type": "string" },
            "status": { "enum": ["active", "inert", "excluded", "replaced", "not-included"] },
            "condition": { "type": "string" },
            "replaced_by": { "type": "string" },
            "paths": { "type": "array", "items": path },
            "provider": provider,
            "constraints": {
                "type": "array",
                "items": {
                    "type": "object",
                    "required": ["spec", "source", "file"],
                    "properties": {
                        "spec": { "type": "string" },
                        "source": { "enum": ["own", "requires", "override"] },
                        "from": { "type": ["string", "null"] },
                        "file": { "type": "string" }
                    }
                }
            }
        }
    })
}

/// `dots init` data: where the repo is, what was written and the answers recorded.
fn init_data_schema() -> Value {
    let strings = json!({ "type": "array", "items": { "type": "string" } });
    let full = json!({
        "type": "object",
        "required": ["root", "created", "files", "cloned_from", "machine_config",
                     "profile", "bundles", "tags", "settings"],
        "properties": {
            "root": { "type": "string" },
            "created": { "type": "boolean" },
            "files": strings,
            "cloned_from": { "type": ["string", "null"] },
            "machine_config": { "type": "string" },
            "profile": { "type": ["string", "null"] },
            "bundles": strings,
            "tags": strings,
            "settings": { "type": "object", "additionalProperties": { "type": "object" } }
        }
    });
    // A failed init reports an empty object.
    json!({ "anyOf": [{ "type": "object", "maxProperties": 0 }, full] })
}

/// `dots adopt` data. Files report where the edit went; a system setting reports its package and
/// value. `changed: false` means the file already matched.
fn adopt_data_schema() -> Value {
    json!({
        "type": "object",
        "required": ["changed"],
        "properties": {
            "dst": { "type": "string" },
            "setting": { "type": "string" },
            "package": { "type": "string" },
            "changed": { "type": "boolean" },
            "mode": { "enum": ["copy", "render", "inject"] },
            "method": { "enum": ["copy", "inject", "text-merge", "structural"] },
            "target": { "type": ["string", "null"] },
            "layer": { "type": ["string", "null"] },
            "created": { "type": "boolean" },
            "keys": { "type": "array", "items": { "type": "string" } },
            "notes": { "type": "array", "items": { "type": "string" } },
            "value": {}
        }
    })
}

/// `dots lint` data: what was checked and how many problems were found (the problems
/// themselves are `errors[]` and `warnings[]`).
fn lint_data_schema() -> Value {
    json!({
        "type": "object",
        "required": ["packages", "bundles", "errors", "warnings"],
        "properties": {
            "packages": { "type": "integer", "minimum": 0 },
            "bundles": { "type": "integer", "minimum": 0 },
            "errors": { "type": "integer", "minimum": 0 },
            "warnings": { "type": "integer", "minimum": 0 }
        }
    })
}

/// `dots upgrade` data: the `apply` run that moved the packages (when one ran), plus what moved
/// and what was left alone and why.
fn upgrade_data_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "upgrade": {
                "type": "object",
                "required": ["targets", "skipped"],
                "properties": {
                    "targets": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "required": ["package", "provider", "from", "to"],
                            "properties": {
                                "package": { "type": "string" },
                                "provider": { "type": "string" },
                                "from": { "type": "string" },
                                "to": { "type": ["string", "null"] }
                            }
                        }
                    },
                    "skipped": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "required": ["package", "reason"],
                            "properties": {
                                "package": { "type": "string" },
                                "reason": { "type": "string" }
                            }
                        }
                    },
                    "commit": { "type": ["string", "null"] }
                }
            }
        }
    })
}

/// `dots pin` / `dots unpin` data: the package file edited and the provider's native pin.
fn pin_data_schema() -> Value {
    json!({
        "type": "object",
        "required": ["package", "version", "file", "changed", "native", "commit"],
        "properties": {
            "package": { "type": "string" },
            "version": { "type": ["string", "null"] },
            "file": { "type": "string" },
            "changed": { "type": "boolean" },
            "native": {
                "type": ["object", "null"],
                "required": ["provider", "action", "commands"],
                "properties": {
                    "provider": { "type": "string" },
                    "action": { "enum": ["pin", "unpin"] },
                    "commands": { "type": "array", "items": { "type": "string" } }
                }
            },
            "commit": { "type": ["string", "null"] }
        }
    })
}

/// `dots migrate` data: what moved, the provider commands that ran, and what was persisted.
fn migrate_data_schema() -> Value {
    json!({
        "type": "object",
        // A failed migration has an empty `data`, so nothing is required here.
        "properties": {
            "package": { "type": "string" },
            "from": { "type": "array", "items": { "type": "string" } },
            "to": { "type": "string" },
            "version": { "type": "string" },
            "installed": { "type": "boolean" },
            "removed": { "type": "array", "items": { "type": "string" } },
            "steps": {
                "type": "array",
                "items": {
                    "type": "object",
                    "required": ["provider", "action", "commands"],
                    "properties": {
                        "provider": { "type": "string" },
                        "action": { "enum": ["install", "remove"] },
                        "commands": { "type": "array", "items": { "type": "string" } }
                    }
                }
            },
            "file": { "type": "string" },
            "changed": { "type": "boolean" },
            "lock": { "type": "boolean" }
        }
    })
}

/// `dots lock --fix` data.
fn lock_data_schema() -> Value {
    json!({
        "type": "object",
        "required": ["path", "status", "fixed", "kept", "dropped", "had_markers"],
        "properties": {
            "path": { "type": "string" },
            "status": { "enum": ["missing", "valid", "pruned", "repaired"] },
            "fixed": { "type": "boolean" },
            "kept": { "type": "integer", "minimum": 0 },
            "dropped": { "type": "array", "items": { "type": "string" } },
            "had_markers": { "type": "boolean" },
            "duplicates": { "type": "integer", "minimum": 0 }
        }
    })
}

/// `dots plan` data (also the `plan` member of `apply`): package steps, the exact provider
/// commands grouped in batches, file steps, and counts.
fn plan_data_schema() -> Value {
    let nullable_str = json!({ "type": ["string", "null"] });
    // A provider switch: the old provider's copy that is removed (or kept) after the install.
    let replaces = json!({
        "type": "object",
        "required": ["provider", "name", "version", "removes", "keep", "commands"],
        "properties": {
            "provider": { "type": "string" },
            "name": { "type": "string" },
            "version": nullable_str,
            "removes": { "type": "boolean" },
            "keep": nullable_str,
            "commands": {
                "type": "array",
                "items": {
                    "type": "object",
                    "required": ["argv", "needs_root"],
                    "properties": {
                        "argv": { "type": "array", "items": { "type": "string" } },
                        "needs_root": { "type": "boolean" }
                    }
                }
            }
        }
    });
    json!({
        "type": "object",
        "required": ["empty", "steps", "batches", "files", "settings", "services", "shims", "hooks", "unplanned", "summary"],
        "properties": {
            "empty": { "type": "boolean" },
            "steps": {
                "type": "array",
                "items": {
                    "type": "object",
                    "required": ["package", "provider", "name", "action", "from", "to",
                                 "needs_root", "interactive", "bootstrap", "batch", "after"],
                    "properties": {
                        "package": { "type": "string" },
                        "provider": { "type": "string" },
                        "name": { "type": "string" },
                        "action": { "enum": ["install", "upgrade", "downgrade"] },
                        "from": nullable_str,
                        "to": nullable_str,
                        "needs_root": { "type": "boolean" },
                        "interactive": { "type": "boolean" },
                        "bootstrap": { "type": "boolean" },
                        "note": nullable_str,
                        "replaces": replaces,
                        "batch": { "type": "integer", "minimum": 0 },
                        "after": { "type": "array", "items": { "type": "string" } }
                    }
                }
            },
            "batches": {
                "type": "array",
                "items": {
                    "type": "object",
                    "required": ["id", "provider", "packages", "interactive", "after", "commands"],
                    "properties": {
                        "id": { "type": "integer", "minimum": 0 },
                        "provider": { "type": "string" },
                        "packages": { "type": "array", "items": { "type": "string" } },
                        "interactive": { "type": "boolean" },
                        "after": { "type": "array", "items": { "type": "integer" } },
                        "commands": {
                            "type": "array",
                            "items": {
                                "type": "object",
                                "required": ["argv", "needs_root"],
                                "properties": {
                                    "argv": { "type": "array", "items": { "type": "string" } },
                                    "needs_root": { "type": "boolean" }
                                }
                            }
                        }
                    }
                }
            },
            "files": {
                "type": "array",
                "items": {
                    "type": "object",
                    "required": ["package", "dst", "kind", "detail", "owned"],
                    "properties": {
                        "package": { "type": "string" },
                        "dst": { "type": "string" },
                        "kind": { "type": "string" },
                        "detail": { "type": "string" },
                        "owned": { "type": "boolean" }
                    }
                }
            },
            "settings": {
                "type": "array",
                "items": {
                    "type": "object",
                    "required": ["package", "id", "label", "kind", "action", "detail", "root",
                                 "restart", "requires"],
                    "properties": {
                        "package": { "type": "string" },
                        "id": { "type": "string" },
                        "label": { "type": "string" },
                        "kind": { "enum": ["defaults", "registry", "xdg"] },
                        "action": { "enum": ["set", "restore", "delete"] },
                        "detail": { "type": "string" },
                        "root": { "type": "boolean" },
                        "restart": { "type": "array", "items": { "type": "string" } },
                        "requires": { "type": "array", "items": { "type": "string" } }
                    }
                }
            },
            "services": {
                "type": "array",
                "items": {
                    "type": "object",
                    "required": ["package", "name", "manager", "action", "detail", "commands"],
                    "properties": {
                        "package": { "type": "string" },
                        "name": { "type": "string" },
                        "manager": { "enum": ["brew", "launchd", "systemd-user", "windows-startup"] },
                        "action": { "type": "string" },
                        "detail": { "type": "string" },
                        "commands": { "type": "array", "items": { "type": "string" } }
                    }
                }
            },
            "shims": {
                "type": "array",
                "items": {
                    "type": "object",
                    "required": ["package", "name", "dst", "action", "target"],
                    "properties": {
                        "package": { "type": "string" },
                        "name": { "type": "string" },
                        "dst": { "type": "string" },
                        "action": { "enum": ["create", "update", "remove"] },
                        "target": nullable_str
                    }
                }
            },
            "hooks": {
                "type": "array",
                "items": {
                    "type": "object",
                    "required": ["package", "event", "command", "why"],
                    "properties": {
                        "package": { "type": "string" },
                        "event": { "enum": ["pre_install", "post_install", "pre_files", "post_files"] },
                        "command": { "type": "string" },
                        "why": { "type": "string" }
                    }
                }
            },
            "unplanned": {
                "type": "array",
                "items": {
                    "type": "object",
                    "required": ["package", "reason"],
                    "properties": {
                        "package": { "type": "string" },
                        "reason": { "type": "string" }
                    }
                }
            },
            "lock": {
                "type": "array",
                "description": "dots.lock rows that change or drift (present only when there are any)",
                "items": {
                    "type": "object",
                    "required": ["package", "provider", "platform", "from", "to", "rewrite", "drift"],
                    "properties": {
                        "package": { "type": "string" },
                        "provider": { "type": "string" },
                        "platform": { "type": "string" },
                        "from": nullable_str,
                        "to": nullable_str,
                        "rewrite": { "type": "boolean" },
                        "drift": nullable_str
                    }
                }
            },
            "summary": {
                "type": "object",
                "required": ["packages", "files", "settings", "services", "shims", "hooks", "downgrades", "root_actions"],
                "properties": {
                    "packages": { "type": "integer", "minimum": 0 },
                    "files": { "type": "integer", "minimum": 0 },
                    "settings": { "type": "integer", "minimum": 0 },
                    "services": { "type": "integer", "minimum": 0 },
                    "shims": { "type": "integer", "minimum": 0 },
                    "hooks": { "type": "integer", "minimum": 0 },
                    "downgrades": { "type": "integer", "minimum": 0 },
                    "root_actions": { "type": "integer", "minimum": 0 }
                }
            }
        }
    })
}

/// `dots apply` data: the plan that was (or, with `--dry-run`, would be) run, and what happened
/// to each package and file.
fn apply_data_schema() -> Value {
    let nullable_str = json!({ "type": ["string", "null"] });
    json!({
        "type": "object",
        "required": ["status", "run", "dry_run", "plan", "results", "files", "settings", "restarts",
                     "services", "shims", "hooks", "hooks_skipped", "todos", "summary"],
        "properties": {
            "status": {
                "enum": ["done", "failed", "interrupted", "dry-run",
                         "confirmation-required", "declined", "plan-failed"]
            },
            "run": { "type": "string" },
            "dry_run": { "type": "boolean" },
            "plan": plan_data_schema(),
            "removals": {
                "type": "array",
                "items": {
                    "type": "object",
                    "required": ["package", "provider", "name", "version", "removed", "detail"],
                    "properties": {
                        "package": { "type": "string" },
                        "provider": { "type": "string" },
                        "name": { "type": "string" },
                        "version": nullable_str,
                        "removed": { "type": "boolean" },
                        "detail": nullable_str
                    }
                }
            },
            "results": {
                "type": "array",
                "items": {
                    "type": "object",
                    "required": ["package", "provider", "action", "from", "to", "status",
                                 "log", "error", "dur_ms"],
                    "properties": {
                        "package": { "type": "string" },
                        "provider": { "type": "string" },
                        "action": { "enum": ["install", "upgrade", "downgrade"] },
                        "from": nullable_str,
                        "to": nullable_str,
                        "status": { "enum": ["ok", "failed", "skipped-due-to-dep", "deferred", "not-run"] },
                        "dependency": { "type": "string" },
                        "log": nullable_str,
                        "error": nullable_str,
                        "dur_ms": { "type": "integer", "minimum": 0 }
                    }
                }
            },
            "files": {
                "type": "array",
                "items": {
                    "type": "object",
                    "required": ["package", "dst", "kind", "ok", "detail"],
                    "properties": {
                        "package": { "type": "string" },
                        "dst": { "type": "string" },
                        "kind": { "type": "string" },
                        "ok": { "type": "boolean" },
                        "detail": nullable_str
                    }
                }
            },
            "settings": {
                "type": "array",
                "items": {
                    "type": "object",
                    "required": ["package", "id", "label", "action", "ok", "detail"],
                    "properties": {
                        "package": { "type": "string" },
                        "id": { "type": "string" },
                        "label": { "type": "string" },
                        "action": { "enum": ["set", "restore", "delete"] },
                        "ok": { "type": "boolean" },
                        "detail": nullable_str
                    }
                }
            },
            "restarts": {
                "type": "array",
                "items": {
                    "type": "object",
                    "required": ["app", "ok", "detail"],
                    "properties": {
                        "app": { "type": "string" },
                        "ok": { "type": "boolean" },
                        "detail": nullable_str
                    }
                }
            },
            "services": {
                "type": "array",
                "items": {
                    "type": "object",
                    "required": ["package", "name", "manager", "action", "ok", "detail"],
                    "properties": {
                        "package": { "type": "string" },
                        "name": { "type": "string" },
                        "manager": { "type": "string" },
                        "action": { "type": "string" },
                        "ok": { "type": "boolean" },
                        "detail": nullable_str
                    }
                }
            },
            "shims": {
                "type": "array",
                "items": {
                    "type": "object",
                    "required": ["package", "name", "dst", "action", "ok", "detail"],
                    "properties": {
                        "package": { "type": "string" },
                        "name": { "type": "string" },
                        "dst": { "type": "string" },
                        "action": { "enum": ["create", "update", "remove"] },
                        "ok": { "type": "boolean" },
                        "detail": nullable_str
                    }
                }
            },
            "hooks": {
                "type": "array",
                "items": {
                    "type": "object",
                    "required": ["package", "event", "ok", "detail", "log"],
                    "properties": {
                        "package": { "type": "string" },
                        "event": { "enum": ["pre_install", "post_install", "pre_files", "post_files"] },
                        "ok": { "type": "boolean" },
                        "detail": nullable_str,
                        "log": nullable_str
                    }
                }
            },
            "hooks_skipped": {
                "type": "array",
                "items": {
                    "type": "object",
                    "required": ["package", "event", "reason"],
                    "properties": {
                        "package": { "type": "string" },
                        "event": { "type": "string" },
                        "reason": { "type": "string" }
                    }
                }
            },
            "todos": {
                "type": "array",
                "items": {
                    "type": "object",
                    "required": ["package", "reason"],
                    "properties": {
                        "package": { "type": "string" },
                        "reason": { "type": "string" }
                    }
                }
            },
            "summary": {
                "type": "object",
                "required": ["ok", "failed", "skipped", "deferred", "not_run", "files_ok", "files_failed",
                             "settings_ok", "settings_failed", "services_ok", "services_failed",
                             "shims_ok", "shims_failed", "hooks_ok", "hooks_failed"],
                "properties": {
                    "ok": { "type": "integer", "minimum": 0 },
                    "failed": { "type": "integer", "minimum": 0 },
                    "skipped": { "type": "integer", "minimum": 0 },
                    "deferred": { "type": "integer", "minimum": 0 },
                    "not_run": { "type": "integer", "minimum": 0 },
                    "files_ok": { "type": "integer", "minimum": 0 },
                    "files_failed": { "type": "integer", "minimum": 0 },
                    "settings_ok": { "type": "integer", "minimum": 0 },
                    "settings_failed": { "type": "integer", "minimum": 0 },
                    "services_ok": { "type": "integer", "minimum": 0 },
                    "services_failed": { "type": "integer", "minimum": 0 },
                    "shims_ok": { "type": "integer", "minimum": 0 },
                    "shims_failed": { "type": "integer", "minimum": 0 },
                    "hooks_ok": { "type": "integer", "minimum": 0 },
                    "hooks_failed": { "type": "integer", "minimum": 0 }
                }
            }
        }
    })
}

pub fn schema_text(command: &str) -> String {
    let mut s = serde_json::to_string_pretty(&schema_for(command)).expect("schema serializes");
    s.push('\n');
    s
}

fn state_words() -> Value {
    json!({ "enum": crate::State::ALL.iter().map(|s| s.word()).collect::<Vec<_>>() })
}

/// `dots status` data: the summary and `--all` views (`items`, `counts`) or one package's deep
/// view (`view = "package"`).
fn status_data_schema() -> Value {
    let nullable_str = json!({ "type": ["string", "null"] });
    let item = json!({
        "type": "object",
        "required": ["area", "name", "package", "state", "provider", "want", "have", "latest",
                     "detail", "hints"],
        "properties": {
            "area": { "enum": ["packages", "files", "settings", "services", "todos", "git", "bins"] },
            "name": { "type": "string" },
            "package": { "type": "string" },
            "state": state_words(),
            "provider": nullable_str,
            "want": nullable_str,
            "have": nullable_str,
            "latest": nullable_str,
            "detail": { "type": "string" },
            "hints": { "type": "array", "items": { "type": "string" } }
        }
    });
    let counts =
        json!({ "type": "object", "additionalProperties": { "type": "integer", "minimum": 1 } });
    json!({
        "type": "object",
        "required": ["view", "header"],
        "properties": {
            "view": { "enum": ["summary", "all", "package"] },
            "header": {
                "type": "object",
                "required": ["root", "branch", "commit", "dirty", "machine", "platform", "os",
                             "arch", "tags"],
                "properties": {
                    "root": { "type": "string" },
                    "branch": nullable_str,
                    "commit": nullable_str,
                    "dirty": { "type": "boolean" },
                    "machine": { "type": "string" },
                    "platform": { "type": "array", "items": { "type": "string" } },
                    "os": { "type": "string" },
                    "arch": { "type": "string" },
                    "tags": { "type": "array", "items": { "type": "string" } }
                }
            },
            "attention": { "type": "integer", "minimum": 0 },
            "counts": {
                "type": "object",
                "required": ["packages", "files", "settings", "services", "todos", "git", "bins"],
                "properties": {
                    "packages": counts, "files": counts, "settings": counts, "services": counts,
                    "todos": counts, "git": counts, "bins": counts
                }
            },
            "items": { "type": "array", "items": item },
            "package": { "type": "string" },
            "description": nullable_str,
            "state": state_words(),
            "detail": nullable_str,
            "provider": { "type": ["object", "null"] },
            "versions": {
                "type": "object",
                "required": ["wanted", "installed", "locked", "latest"],
                "properties": {
                    "wanted": { "type": "string" },
                    "installed": nullable_str,
                    "locked": nullable_str,
                    "latest": nullable_str
                }
            },
            "inclusion": nullable_str,
            "files": { "type": "array", "items": item },
            "todos": { "type": "array", "items": item },
            "env": {
                "type": "object",
                "required": ["path", "vars", "source"],
                "properties": {
                    "path": { "type": "array", "items": { "type": "string" } },
                    "vars": { "type": "object", "additionalProperties": { "type": "string" } },
                    "source": { "type": "array", "items": { "type": "string" } }
                }
            },
            "overrides": {
                "type": "array",
                "items": {
                    "type": "object",
                    "required": ["scope", "file"],
                    "properties": {
                        "scope": { "enum": ["repo", "machine"] },
                        "file": { "type": "string" }
                    }
                }
            },
            "history": {
                "type": "array",
                "items": {
                    "type": "object",
                    "required": ["ts", "op"],
                    "properties": {
                        "ts": { "type": "string" },
                        "op": { "type": "string" },
                        "provider": nullable_str,
                        "from": nullable_str,
                        "to": nullable_str,
                        "exit": { "type": ["integer", "null"] },
                        "run": { "type": "string" }
                    }
                }
            }
        }
    })
}

/// `dots outdated` data: every checked package with its cached latest version.
fn outdated_data_schema() -> Value {
    let nullable_str = json!({ "type": ["string", "null"] });
    json!({
        "type": "object",
        "required": ["offline", "refreshed", "packages", "outdated", "unknown"],
        "properties": {
            "offline": { "type": "boolean" },
            "refreshed": { "type": "boolean" },
            "outdated": { "type": "integer", "minimum": 0 },
            "unknown": { "type": "array", "items": { "type": "string" } },
            "packages": {
                "type": "array",
                "items": {
                    "type": "object",
                    "required": ["package", "provider", "wanted", "installed", "latest", "state"],
                    "properties": {
                        "package": { "type": "string" },
                        "provider": { "type": "string" },
                        "wanted": { "type": "string" },
                        "installed": { "type": "string" },
                        "latest": nullable_str,
                        "state": { "enum": ["ok", "outdated", null] }
                    }
                }
            }
        }
    })
}

fn bin_entry_schema() -> Value {
    let nullable_str = json!({ "type": ["string", "null"] });
    json!({
        "type": "object",
        "required": ["bin", "path", "real_path", "package", "native", "provider", "version",
                     "class", "wins", "shadowed", "shim", "notes", "hint"],
        "properties": {
            "bin": { "type": "string" },
            "path": { "type": "string" },
            "real_path": { "type": "string" },
            "package": nullable_str,
            "native": nullable_str,
            "provider": nullable_str,
            "version": nullable_str,
            "class": { "enum": ["managed", "orphan", "unmanaged", "unknown"] },
            "dependency": { "type": "boolean" },
            "superseded_by": nullable_str,
            "wins": { "type": "boolean" },
            "shadowed": { "type": "boolean" },
            "shim": nullable_str,
            "notes": { "type": "array", "items": { "type": "string" } },
            "hint": nullable_str
        }
    })
}

/// `dots bins` data: every executable on PATH with its owner.
fn bins_data_schema() -> Value {
    json!({
        "type": "object",
        "required": ["target", "bins", "summary"],
        "properties": {
            "target": { "type": ["string", "null"] },
            "bins": { "type": "array", "items": bin_entry_schema() },
            "summary": {
                "type": "object",
                "required": ["total", "managed", "unmanaged", "unknown", "shadowed"],
                "properties": {
                    "total": { "type": "integer", "minimum": 0 },
                    "managed": { "type": "integer", "minimum": 0 },
                    "unmanaged": { "type": "integer", "minimum": 0 },
                    "unknown": { "type": "integer", "minimum": 0 },
                    "shadowed": { "type": "integer", "minimum": 0 }
                }
            }
        }
    })
}

/// `dots which` data: every copy of one executable, in PATH order.
fn which_data_schema() -> Value {
    json!({
        "type": "object",
        "required": ["bin", "copies"],
        "properties": {
            "bin": { "type": "string" },
            "copies": { "type": "array", "items": bin_entry_schema() }
        }
    })
}

/// `dots config` data: the action plus its result. A secret setting always has `value: null`
/// and `redacted: true`; its real value never appears.
fn config_data_schema() -> Value {
    let scope = json!({ "enum": ["default", "bundle", "upstream", "repo", "profile", "machine"] });
    let nullable_scope = json!({ "type": ["string", "null"], "enum": ["default", "bundle", "upstream", "repo", "profile", "machine", null] });
    let setting = json!({
        "type": "object",
        "required": ["id", "package", "key", "type", "value", "scope", "default", "required",
                     "ask", "secret", "redacted", "applies", "missing"],
        "properties": {
            "id": { "type": "string" },
            "package": { "type": "string" },
            "key": { "type": "string" },
            "type": { "enum": ["bool", "int", "float", "string", "enum", "list<string>", "path"] },
            "description": { "type": ["string", "null"] },
            "value": {},
            "scope": nullable_scope,
            "default": {},
            "required": { "type": "boolean" },
            "ask": { "type": "boolean" },
            "secret": { "type": "boolean" },
            "redacted": { "type": "boolean" },
            "values": { "type": "array", "items": { "type": "string" } },
            "applies": { "type": "boolean" },
            "missing": { "type": "boolean" },
            "active": { "type": "boolean" }
        },
        "if": { "properties": { "secret": { "const": true } }, "required": ["secret"] },
        "then": { "properties": { "value": { "type": "null" }, "default": { "type": "null" } } }
    });
    let layer = json!({
        "type": "object",
        "required": ["scope", "set", "value", "source", "winner"],
        "properties": {
            "scope": scope,
            "set": { "type": "boolean" },
            "value": {},
            "source": { "type": ["string", "null"] },
            "line": { "type": ["integer", "null"] },
            "winner": { "type": "boolean" },
            "valid": { "type": ["boolean", "null"] },
            "error": { "type": ["string", "null"] }
        }
    });
    json!({
        "type": "object",
        "properties": {
            "action": { "enum": ["list", "get", "set", "unset", "explain"] },
            "settings": { "type": "array", "items": setting },
            "id": { "type": "string" },
            "scope": { "type": ["string", "null"] },
            "value": {},
            "redacted": { "type": "boolean" },
            "secret": { "type": "boolean" },
            "winner": nullable_scope,
            "scopes": { "type": "array", "items": layer },
            "removed": { "type": "boolean" },
            "committed": { "type": "boolean" },
            "file": { "type": "string" },
            "effective_value": {},
            "effective_scope": nullable_scope
        },
        "allOf": [
            { "if": { "properties": { "action": { "const": "list" } }, "required": ["action"] },
              "then": { "required": ["settings"] } },
            { "if": { "properties": { "action": { "const": "explain" } }, "required": ["action"] },
              "then": { "required": ["id", "scopes", "winner", "value", "redacted", "secret"] } },
            { "if": { "properties": { "action": { "const": "get" } }, "required": ["action"] },
              "then": { "required": ["id", "value", "scope", "redacted", "secret"] } },
            { "if": { "properties": { "action": { "const": "set" } }, "required": ["action"] },
              "then": { "required": ["id", "scope", "file", "value", "redacted", "committed"] } },
            { "if": { "properties": { "action": { "const": "unset" } }, "required": ["action"] },
              "then": { "required": ["id", "scope", "removed"] } },
            { "if": { "properties": { "redacted": { "const": true } }, "required": ["redacted"] },
              "then": { "properties": { "value": { "type": "null" } } } }
        ]
    })
}

/// `dots questions` data: every unanswered question with its recommended answer.
fn questions_data_schema() -> Value {
    json!({
        "type": "object",
        "required": ["questions", "unanswered", "required_unanswered"],
        "properties": {
            "unanswered": { "type": "integer", "minimum": 0 },
            "required_unanswered": { "type": "integer", "minimum": 0 },
            "questions": {
                "type": "array",
                "items": {
                    "type": "object",
                    "required": ["id", "kind", "name", "question", "required", "recommended", "answer"],
                    "properties": {
                        "id": { "type": "string", "pattern": "^(setting|bundle|tag):" },
                        "kind": { "enum": ["setting", "bundle", "tag"] },
                        "name": { "type": "string" },
                        "question": { "type": "string" },
                        "description": { "type": ["string", "null"] },
                        "required": { "type": "boolean" },
                        "recommended": {},
                        "answer": { "type": "string" },
                        "package": { "type": "string" },
                        "key": { "type": "string" },
                        "type": { "type": "string" },
                        "values": { "type": "array", "items": { "type": "string" } },
                        "secret": { "type": "boolean" },
                        "reason": { "enum": ["ask", "required"] }
                    },
                    "allOf": [
                        { "if": { "properties": { "kind": { "const": "setting" } }, "required": ["kind"] },
                          "then": { "required": ["package", "key", "type", "secret", "reason"] } },
                        { "if": { "properties": { "kind": { "enum": ["bundle", "tag"] } }, "required": ["kind"] },
                          "then": { "properties": { "recommended": { "const": true } } } },
                        { "if": { "properties": { "secret": { "const": true } }, "required": ["secret"] },
                          "then": { "properties": { "recommended": { "type": "null" } } } }
                    ]
                }
            }
        }
    })
}

/// `dots doctor` data: one entry per check, each with its findings.
fn doctor_data_schema() -> Value {
    json!({
        "type": "object",
        "required": ["checks", "summary"],
        "properties": {
            "checks": {
                "type": "array",
                "items": {
                    "type": "object",
                    "required": ["id", "status", "findings"],
                    "properties": {
                        "id": { "type": "string" },
                        "status": { "enum": ["ok", "warn", "error"] },
                        "findings": {
                            "type": "array",
                            "items": {
                                "type": "object",
                                "required": ["subject", "severity", "message", "fix"],
                                "properties": {
                                    "subject": { "type": "string" },
                                    "severity": { "enum": ["warn", "error"] },
                                    "message": { "type": "string" },
                                    "fix": { "type": "string" }
                                }
                            }
                        }
                    }
                }
            },
            "summary": {
                "type": "object",
                "required": ["errors", "warnings", "passed"],
                "properties": {
                    "errors": { "type": "integer", "minimum": 0 },
                    "warnings": { "type": "integer", "minimum": 0 },
                    "passed": { "type": "integer", "minimum": 0 }
                }
            }
        }
    })
}

fn show_data_schema() -> Value {
    let origin = json!({
        "type": "object",
        "required": ["path", "scope", "source"],
        "properties": {
            "path": { "type": "string" },
            "scope": { "enum": ["definition", "repo", "profile", "machine"] },
            "source": { "type": "string" },
            "file": { "type": ["string", "null"] },
            "line": { "type": ["integer", "null"] }
        }
    });
    json!({
        "type": "object",
        "required": ["address", "effective", "value", "text"],
        "properties": {
            "address": { "type": "string" },
            "effective": { "type": "boolean" },
            "value": {},
            "text": { "type": "array", "items": { "type": "string" } },
            "provenance": { "type": "array", "items": origin },
            "overrides": { "type": "array", "items": { "type": "string" } }
        }
    })
}

fn override_data_schema() -> Value {
    let scope = json!({ "enum": ["repo", "profile", "machine"] });
    json!({
        "type": "object",
        "required": ["action"],
        "properties": {
            "action": { "enum": ["list", "edit", "diff"] },
            "scopes": {
                "type": "array",
                "items": {
                    "type": "object",
                    "required": ["scope", "location", "overrides"],
                    "properties": {
                        "scope": scope,
                        "location": { "type": "string" },
                        "overrides": {
                            "type": "array",
                            "items": {
                                "type": "object",
                                "required": ["target", "file", "keys"],
                                "properties": {
                                    "target": { "type": "string" },
                                    "file": { "type": "string" },
                                    "keys": { "type": "array", "items": { "type": "string" } }
                                }
                            }
                        }
                    }
                }
            },
            "address": { "type": "string" },
            "scope": scope,
            "path": { "type": "string" },
            "created": { "type": "boolean" },
            "changes": {
                "type": "array",
                "items": {
                    "type": "object",
                    "required": ["path", "change"],
                    "properties": {
                        "path": { "type": "string" },
                        "change": { "enum": ["added", "removed", "changed"] },
                        "before": {},
                        "after": {},
                        "source": { "type": ["string", "null"] }
                    }
                }
            }
        }
    })
}
