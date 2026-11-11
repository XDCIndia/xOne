//! JSON diff by pointer. Hex strings compare case-insensitively (geth writes lower case; this is the
//! only normalisation); everything else is exact. A missing key is a difference.

use serde_json::Value;

pub fn diff(want: &Value, got: &Value, path: &str, out: &mut Vec<String>) {
    match (want, got) {
        (Value::Object(a), Value::Object(b)) => {
            for (k, v) in a {
                match b.get(k) {
                    Some(g) => diff(v, g, &format!("{path}/{k}"), out),
                    None => out.push(format!("{path}/{k}: missing, geth has {}", short(v))),
                }
            }
            for k in b.keys().filter(|k| !a.contains_key(*k)) {
                out.push(format!("{path}/{k}: extra, geth has none"));
            }
        }
        (Value::Array(a), Value::Array(b)) => {
            if a.len() != b.len() {
                out.push(format!("{path}: length {} but geth {}", b.len(), a.len()));
            }
            for (i, (x, y)) in a.iter().zip(b).enumerate() {
                diff(x, y, &format!("{path}/{i}"), out);
            }
        }
        (Value::String(a), Value::String(b))
            if a.starts_with("0x") && b.starts_with("0x") && a.eq_ignore_ascii_case(b) => {}
        (a, b) if a == b => {}
        (a, b) => out.push(format!("{path}: have {} but geth {}", short(b), short(a))),
    }
}

fn short(v: &Value) -> String {
    let s = v.to_string();
    if s.len() > 90 {
        format!("{}...", &s[..90])
    } else {
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn d(a: Value, b: Value) -> Vec<String> {
        let mut o = vec![];
        diff(&a, &b, "", &mut o);
        o
    }

    #[test]
    fn equal_and_hex_case() {
        assert!(d(json!({"a": ["0xAB", 1]}), json!({"a": ["0xab", 1]})).is_empty());
    }

    #[test]
    fn shape_and_value_differences_are_found() {
        let o = d(
            json!({"a": 1, "b": [1, 2]}),
            json!({"a": 2, "b": [1], "c": 0}),
        );
        assert_eq!(o.len(), 3, "{o:?}");
        assert!(o.iter().any(|l| l.contains("/c: extra")));
        assert!(d(json!({"k": null}), json!({}))
            .iter()
            .any(|l| l.contains("missing")));
    }
}
