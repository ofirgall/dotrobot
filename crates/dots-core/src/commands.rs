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

/// `dots plan` data (also the `plan` member of `apply`): package steps, the exact provider
/// commands grouped in batches, file steps, and counts.
fn plan_data_schema() -> Value {
    let nullable_str = json!({ "type": ["string", "null"] });
    json!({
        "type": "object",
        "required": ["empty", "steps", "batches", "files", "unplanned", "summary"],
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
            "summary": {
                "type": "object",
                "required": ["packages", "files", "downgrades", "root_actions"],
                "properties": {
                    "packages": { "type": "integer", "minimum": 0 },
                    "files": { "type": "integer", "minimum": 0 },
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
        "required": ["status", "run", "dry_run", "plan", "results", "files", "todos", "summary"],
        "properties": {
            "status": {
                "enum": ["done", "failed", "interrupted", "dry-run",
                         "confirmation-required", "declined", "plan-failed"]
            },
            "run": { "type": "string" },
            "dry_run": { "type": "boolean" },
            "plan": plan_data_schema(),
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
                "required": ["ok", "failed", "skipped", "deferred", "not_run", "files_ok", "files_failed"],
                "properties": {
                    "ok": { "type": "integer", "minimum": 0 },
                    "failed": { "type": "integer", "minimum": 0 },
                    "skipped": { "type": "integer", "minimum": 0 },
                    "deferred": { "type": "integer", "minimum": 0 },
                    "not_run": { "type": "integer", "minimum": 0 },
                    "files_ok": { "type": "integer", "minimum": 0 },
                    "files_failed": { "type": "integer", "minimum": 0 }
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
            "area": { "enum": ["packages", "files", "todos", "git", "bins"] },
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
                "required": ["packages", "files", "todos", "git", "bins"],
                "properties": {
                    "packages": counts, "files": counts, "todos": counts,
                    "git": counts, "bins": counts
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
