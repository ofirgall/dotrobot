//! Package setting declarations: what a `[settings.<key>]` entry may look like, and whether
//! a value fits it (ARCHITECTURE §17.1). Pure checks; precedence lives in `dots-config`.

use regex::Regex;
use serde_json::Value;

use crate::types::SettingDecl;

/// What is wrong with a declaration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeclProblem {
    /// Neither `default` nor `required = true`.
    NoDefault,
    /// Anything else (both `default` and `required`, a bad default, no `values` for an enum).
    Invalid(String),
}

/// The setting types (`list` is shorthand for `list<string>`).
pub const KNOWN_TYPES: [&str; 8] = [
    "bool",
    "int",
    "float",
    "string",
    "enum",
    "list",
    "list<string>",
    "path",
];

impl SettingDecl {
    /// The declared type name, `string` when it was left out.
    pub fn type_name(&self) -> &str {
        self.ty.as_deref().unwrap_or("string")
    }

    /// Check the declaration on its own: exactly one of `default` / `required`, an enum with
    /// values, a `validate` that compiles, and a default that satisfies the declaration.
    pub fn check_decl(&self) -> Result<(), DeclProblem> {
        if self.default.is_none() && !self.required {
            return Err(DeclProblem::NoDefault);
        }
        if self.default.is_some() && self.required {
            return Err(DeclProblem::Invalid(
                "a setting has either a `default` or `required = true`, not both".into(),
            ));
        }
        if !KNOWN_TYPES.contains(&self.type_name()) {
            return Err(DeclProblem::Invalid(format!(
                "unknown setting type `{}`; use one of {}",
                self.type_name(),
                KNOWN_TYPES.join(", ")
            )));
        }
        if self.type_name() == "enum" && self.values.is_empty() {
            return Err(DeclProblem::Invalid(
                "an `enum` setting needs `values`".into(),
            ));
        }
        if let Some(p) = &self.validate
            && let Err(e) = Regex::new(p)
        {
            return Err(DeclProblem::Invalid(format!(
                "`validate` is not a valid regex: {e}"
            )));
        }
        if !self.range.is_empty() && self.range.len() != 2 {
            return Err(DeclProblem::Invalid(
                "`range` takes exactly two numbers, [min, max]".into(),
            ));
        }
        if let Some(d) = &self.default
            && let Err(e) = self.check_value(d)
        {
            return Err(DeclProblem::Invalid(format!("the default is invalid: {e}")));
        }
        Ok(())
    }

    /// The type a value is expected to have, for messages: `int`, `enum (dark, light)`.
    pub fn expected(&self) -> String {
        match self.type_name() {
            "enum" => format!("enum ({})", self.values.join(", ")),
            t => t.to_string(),
        }
    }

    /// Check `v` against the type, the enum values, the range and the `validate` pattern.
    pub fn check_value(&self, v: &Value) -> Result<(), String> {
        // A secret's value never appears in a message.
        let secret = self.secret;
        let wrong = |what: &str| {
            if secret {
                Err(format!("expected {}, got {what}", self.expected()))
            } else {
                Err(format!(
                    "expected {}, got {what} {}",
                    self.expected(),
                    show(v)
                ))
            }
        };
        match self.type_name() {
            "bool" => {
                if !v.is_boolean() {
                    return wrong(kind(v));
                }
            }
            "int" => {
                if !(v.is_i64() || v.is_u64()) {
                    return wrong(kind(v));
                }
                self.check_range(v.as_f64().unwrap_or_default())?;
            }
            "float" => {
                if !v.is_number() {
                    return wrong(kind(v));
                }
                self.check_range(v.as_f64().unwrap_or_default())?;
            }
            "string" | "path" => match v {
                Value::String(s) => self.check_pattern(s)?,
                _ => return wrong(kind(v)),
            },
            "enum" => match v {
                Value::String(s) if self.values.contains(s) => self.check_pattern(s)?,
                Value::String(_) => {
                    return Err(if secret {
                        format!("value is not one of {}", self.values.join(", "))
                    } else {
                        format!("{} is not one of {}", show(v), self.values.join(", "))
                    });
                }
                _ => return wrong(kind(v)),
            },
            "list<string>" | "list" => match v {
                Value::Array(items) => {
                    for item in items {
                        match item {
                            Value::String(s) => self.check_pattern(s)?,
                            other => {
                                return Err(if secret {
                                    format!("expected list<string>, found {}", kind(other))
                                } else {
                                    format!(
                                        "expected list<string>, found {} {}",
                                        kind(other),
                                        show(other)
                                    )
                                });
                            }
                        }
                    }
                }
                _ => return wrong(kind(v)),
            },
            other => return Err(format!("unknown setting type `{other}`")),
        }
        Ok(())
    }

    fn check_range(&self, n: f64) -> Result<(), String> {
        if let [lo, hi] = self.range[..]
            && (n < lo || n > hi)
        {
            return Err(if self.secret {
                format!("value is outside the range {lo}..{hi}")
            } else {
                format!("{n} is outside the range {lo}..{hi}")
            });
        }
        Ok(())
    }

    fn check_pattern(&self, s: &str) -> Result<(), String> {
        if let Some(p) = &self.validate
            && let Ok(re) = Regex::new(p)
            && !re.is_match(s)
        {
            return Err(if self.secret {
                format!("value does not match `{p}`")
            } else {
                format!("`{s}` does not match `{p}`")
            });
        }
        Ok(())
    }

    /// Turn command-line text into a value of the declared type. `list<string>` splits on
    /// commas. Fails with the type that was expected.
    pub fn parse_text(&self, text: &str) -> Result<Value, String> {
        let bad = || {
            if self.secret {
                Err(format!("expected {}", self.expected()))
            } else {
                Err(format!("expected {}, got `{text}`", self.expected()))
            }
        };
        let v = match self.type_name() {
            "bool" => match text {
                "true" | "yes" | "on" | "1" => Value::Bool(true),
                "false" | "no" | "off" | "0" => Value::Bool(false),
                _ => return bad(),
            },
            "int" => match text.trim().parse::<i64>() {
                Ok(n) => Value::from(n),
                Err(_) => return bad(),
            },
            "float" => match text.trim().parse::<f64>() {
                Ok(n) => serde_json::Number::from_f64(n).map_or(Value::Null, Value::Number),
                Err(_) => return bad(),
            },
            "list<string>" | "list" => Value::Array(
                text.split(',')
                    .map(|s| s.trim())
                    .filter(|s| !s.is_empty())
                    .map(|s| Value::String(s.to_string()))
                    .collect(),
            ),
            _ => Value::String(text.to_string()),
        };
        self.check_value(&v)?;
        Ok(v)
    }
}

fn kind(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Number(n) if n.is_i64() || n.is_u64() => "int",
        Value::Number(_) => "float",
        Value::String(_) => "string",
        Value::Array(_) => "list",
        Value::Object(_) => "table",
    }
}

fn show(v: &Value) -> String {
    match v {
        Value::String(s) => format!("`{s}`"),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn decl(ty: &str, secret: bool) -> SettingDecl {
        SettingDecl {
            ty: Some(ty.into()),
            secret,
            required: true,
            ..Default::default()
        }
    }

    #[test]
    fn secret_values_never_appear_in_messages() {
        let mut d = decl("string", true);
        d.validate = Some("^ghp_".into());
        let e = d.check_value(&json!("real-secret")).unwrap_err();
        assert!(!e.contains("real-secret"), "{e}");
        assert!(e.contains("^ghp_"));
        let mut l = decl("list<string>", true);
        l.validate = Some("^ghp_".into());
        let e = l
            .check_value(&json!(["ghp_ok", "real-secret"]))
            .unwrap_err();
        assert!(!e.contains("real-secret"), "{e}");
        let e = l.check_value(&json!(["ghp_ok", 12345])).unwrap_err();
        assert!(!e.contains("12345"), "{e}");
        let e = decl("int", true)
            .check_value(&json!("real-secret"))
            .unwrap_err();
        assert!(!e.contains("real-secret"), "{e}");
        let mut en = decl("enum", true);
        en.values = vec!["a".into()];
        let e = en.check_value(&json!("real-secret")).unwrap_err();
        assert!(!e.contains("real-secret"), "{e}");
        let e = decl("int", true).parse_text("real-secret").unwrap_err();
        assert!(!e.contains("real-secret"), "{e}");
        // A plain setting still shows what was wrong.
        let mut p = decl("string", false);
        p.validate = Some("^ghp_".into());
        assert!(p.check_value(&json!("nope")).unwrap_err().contains("nope"));
    }

    #[test]
    fn list_is_list_of_string() {
        let d = decl("list", false);
        assert!(d.check_decl().is_ok());
        assert!(d.check_value(&json!(["a", "b"])).is_ok());
        assert!(d.check_value(&json!(["a", 1])).is_err());
        assert_eq!(d.parse_text("a, b").unwrap(), json!(["a", "b"]));
    }

    #[test]
    fn unknown_type_is_rejected_even_when_required() {
        let e = decl("integer", false).check_decl().unwrap_err();
        assert!(matches!(e, DeclProblem::Invalid(m) if m.contains("integer")));
        let mut d = decl("integer", false);
        d.required = false;
        d.default = Some(json!(1));
        assert!(d.check_decl().is_err());
    }
}
