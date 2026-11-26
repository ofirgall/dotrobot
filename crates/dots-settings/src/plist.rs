//! A small XML property-list reader and writer, enough for `defaults export <domain> -`
//! output and for the value fragments `defaults write` accepts.
//!
//! Values map to JSON: `dict` to object, `array` to array, `integer` to an integer, `real` to a
//! float, `true`/`false` to a bool, `string` to a string. `date` and `data` keep their text as a
//! string (they cannot be declared, only observed).

use serde_json::{Map, Number, Value};

/// Parse a whole plist document (`<plist><dict>...</dict></plist>`) or a bare value.
pub fn parse(text: &str) -> Result<Value, String> {
    let mut p = Parser { s: text, i: 0 };
    p.skip_prolog()?;
    // `<plist version="1.0">` wrapper
    p.skip_ws();
    if p.starts_with("<plist") {
        p.skip_tag()?;
    }
    let v = p.value()?;
    Ok(v)
}

struct Parser<'a> {
    s: &'a str,
    i: usize,
}

impl Parser<'_> {
    fn rest(&self) -> &str {
        &self.s[self.i..]
    }

    fn starts_with(&self, p: &str) -> bool {
        self.rest().starts_with(p)
    }

    fn skip_ws(&mut self) {
        let t = self.rest().trim_start();
        self.i = self.s.len() - t.len();
    }

    fn skip_tag(&mut self) -> Result<(), String> {
        match self.rest().find('>') {
            Some(n) => {
                self.i += n + 1;
                Ok(())
            }
            None => Err("unterminated tag".into()),
        }
    }

    /// `<?xml ...?>`, `<!DOCTYPE ...>` and comments before the root.
    fn skip_prolog(&mut self) -> Result<(), String> {
        loop {
            self.skip_ws();
            if self.starts_with("<?") {
                match self.rest().find("?>") {
                    Some(n) => self.i += n + 2,
                    None => return Err("unterminated <?...?>".into()),
                }
            } else if self.starts_with("<!--") {
                match self.rest().find("-->") {
                    Some(n) => self.i += n + 3,
                    None => return Err("unterminated comment".into()),
                }
            } else if self.starts_with("<!") {
                self.skip_tag()?;
            } else {
                return Ok(());
            }
        }
    }

    /// Reads `<name>` or `<name/>`; returns (name, self_closing).
    fn open(&mut self) -> Result<(String, bool), String> {
        self.skip_ws();
        if !self.starts_with("<") || self.starts_with("</") {
            return Err(format!("expected a tag at `{}`", self.snippet()));
        }
        let end = self.rest().find('>').ok_or("unterminated tag")?;
        let inner = &self.rest()[1..end];
        let self_closing = inner.ends_with('/');
        let name = inner
            .trim_end_matches('/')
            .split_whitespace()
            .next()
            .unwrap_or("")
            .to_string();
        self.i += end + 1;
        Ok((name, self_closing))
    }

    fn snippet(&self) -> String {
        self.rest().chars().take(24).collect()
    }

    fn close(&mut self, name: &str) -> Result<(), String> {
        self.skip_ws();
        let tag = format!("</{name}>");
        if self.starts_with(&tag) {
            self.i += tag.len();
            Ok(())
        } else {
            Err(format!("expected {tag} at `{}`", self.snippet()))
        }
    }

    fn text_until_close(&mut self, name: &str) -> Result<String, String> {
        let tag = format!("</{name}>");
        let n = self.rest().find(&tag).ok_or(format!("missing {tag}"))?;
        let text = unescape(&self.rest()[..n]);
        self.i += n + tag.len();
        Ok(text)
    }

    fn value(&mut self) -> Result<Value, String> {
        let (name, closed) = self.open()?;
        match name.as_str() {
            "true" => Ok(Value::Bool(true)),
            "false" => Ok(Value::Bool(false)),
            "dict" => {
                let mut m = Map::new();
                if closed {
                    return Ok(Value::Object(m));
                }
                loop {
                    self.skip_ws();
                    if self.starts_with("</dict>") {
                        self.close("dict")?;
                        return Ok(Value::Object(m));
                    }
                    let (k, kc) = self.open()?;
                    if k != "key" {
                        return Err(format!("expected <key>, found <{k}>"));
                    }
                    let key = if kc {
                        String::new()
                    } else {
                        self.text_until_close("key")?
                    };
                    let v = self.value()?;
                    m.insert(key, v);
                }
            }
            "array" => {
                let mut a = Vec::new();
                if closed {
                    return Ok(Value::Array(a));
                }
                loop {
                    self.skip_ws();
                    if self.starts_with("</array>") {
                        self.close("array")?;
                        return Ok(Value::Array(a));
                    }
                    a.push(self.value()?);
                }
            }
            "string" | "date" | "data" => {
                if closed {
                    return Ok(Value::String(String::new()));
                }
                let t = self.text_until_close(&name)?;
                Ok(Value::String(if name == "data" {
                    t.split_whitespace().collect()
                } else {
                    t
                }))
            }
            "integer" => {
                let t = self.text_until_close("integer")?;
                let t = t.trim();
                t.parse::<i64>()
                    .map(|n| Value::Number(n.into()))
                    .or_else(|_| t.parse::<u64>().map(|n| Value::Number(n.into())))
                    .map_err(|_| format!("bad integer `{t}`"))
            }
            "real" => {
                let t = self.text_until_close("real")?;
                let f: f64 = t.trim().parse().map_err(|_| format!("bad real `{t}`"))?;
                Number::from_f64(f)
                    .map(Value::Number)
                    .ok_or_else(|| format!("bad real `{t}`"))
            }
            other => Err(format!("unsupported plist element <{other}>")),
        }
    }
}

fn unescape(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// The XML fragment for `v`, as `defaults write <domain> <key> '<fragment>'` takes it.
/// Floats keep a `<real>`, integers an `<integer>`, so the type survives the round trip.
pub fn fragment(v: &Value) -> String {
    match v {
        Value::Bool(true) => "<true/>".into(),
        Value::Bool(false) => "<false/>".into(),
        Value::Number(n) if n.is_f64() => format!("<real>{}</real>", n.as_f64().unwrap_or(0.0)),
        Value::Number(n) => format!("<integer>{n}</integer>"),
        Value::String(s) => format!("<string>{}</string>", escape(s)),
        Value::Array(a) => format!(
            "<array>{}</array>",
            a.iter().map(fragment).collect::<String>()
        ),
        Value::Object(m) => format!(
            "<dict>{}</dict>",
            m.iter()
                .map(|(k, v)| format!("<key>{}</key>{}", escape(k), fragment(v)))
                .collect::<String>()
        ),
        Value::Null => "<string></string>".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_defaults_export() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>Arr</key>
	<array><string>a &amp; b</string><integer>3</integer></array>
	<key>B</key><true/>
	<key>F</key><real>1</real>
	<key>Empty</key><dict/>
	<key>Top</key>
	<dict><key>64</key><dict><key>enabled</key><false/></dict></dict>
</dict>
</plist>"#;
        let v = parse(xml).unwrap();
        assert_eq!(v["Arr"], json!(["a & b", 3]));
        assert_eq!(v["B"], json!(true));
        assert!(v["F"].is_f64());
        assert_eq!(v["Empty"], json!({}));
        assert_eq!(v["Top"]["64"]["enabled"], json!(false));
    }

    #[test]
    fn fragments_round_trip() {
        let v = json!({"a": [1, 2.5, "x<y"], "b": {"c": true}});
        assert_eq!(parse(&fragment(&v)).unwrap(), v);
        assert_eq!(fragment(&json!(1.0)), "<real>1</real>");
    }

    #[test]
    fn garbage_is_an_error() {
        assert!(parse("not a plist").is_err());
        assert!(parse("<dict><key>a</key>").is_err());
    }
}
