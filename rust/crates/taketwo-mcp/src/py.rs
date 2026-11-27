//! Python formatting the ported messages rely on: `repr()` and `str()` of the JSON values a tool sees.

use serde_json::Value;

/// `repr(s)` for a str: single quotes unless the text holds `'` and no `"`; `\\`, the quote, `\n`, `\r`, `\t`
/// and other ASCII control characters escaped.
/// shortcut: non-ASCII non-printable characters (Python escapes them as \x.., \u....) are kept as they are;
/// upgrade if a caller's text with such characters ever shows up in a message.
pub fn repr_str(s: &str) -> String {
    let q = if s.contains('\'') && !s.contains('"') { '"' } else { '\'' };
    let mut out = String::with_capacity(s.len() + 2);
    out.push(q);
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c == q => {
                out.push('\\');
                out.push(c);
            }
            c if (c as u32) < 0x20 || c as u32 == 0x7f => out.push_str(&format!("\\x{:02x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push(q);
    out
}

/// `repr(v)` of the Python object `json.loads` would give.
pub fn repr(v: &Value) -> String {
    match v {
        Value::Null => "None".into(),
        Value::Bool(true) => "True".into(),
        Value::Bool(false) => "False".into(),
        Value::Number(n) => match n.as_f64() {
            // serde keeps "1.0" a float; Python prints a float with at least one decimal, as `{:?}` does.
            Some(f) if n.is_f64() => format!("{f:?}"),
            _ => n.to_string(),
        },
        Value::String(s) => repr_str(s),
        Value::Array(a) => format!("[{}]", a.iter().map(repr).collect::<Vec<_>>().join(", ")),
        Value::Object(o) => {
            format!("{{{}}}", o.iter().map(|(k, v)| format!("{}: {}", repr_str(k), repr(v))).collect::<Vec<_>>().join(", "))
        }
    }
}

/// `str(v)` as an f-string prints it: text bare, everything else as its repr.
pub fn str_(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => repr(other),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn repr_matches_python() {
        assert_eq!(repr_str("Ann"), "'Ann'");
        assert_eq!(repr_str("it's"), "\"it's\"");
        assert_eq!(repr_str("a'b\"c"), "'a\\'b\"c'");
        assert_eq!(repr_str("x\ny\\"), "'x\\ny\\\\'");
        assert_eq!(repr_str("é"), "'é'");
        assert_eq!(repr(&json!(1.5)), "1.5");
        assert_eq!(repr(&json!(1.0)), "1.0");
        assert_eq!(repr(&json!(-1)), "-1");
        assert_eq!(repr(&json!(true)), "True");
        assert_eq!(repr(&json!(null)), "None");
        assert_eq!(repr(&json!([{"a": 1, "b": "x"}])), "[{'a': 1, 'b': 'x'}]");
        assert_eq!(str_(&json!("done")), "done");
    }
}
