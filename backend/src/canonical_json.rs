//! One deterministic JSON encoding for hashes, authored files, merge ordering, and
//! cache keys. `serde_json::Value` cannot contain non-finite numbers, so this
//! conversion is infallible and differs from ordinary JSON only by sorting
//! object keys recursively and omitting insignificant whitespace.

use serde_json::{Number, Value};

/// JSON number spelling is not authored meaning. SQLite and serde may retain
/// whether an integral value arrived as `1` or `1.0`, while the score grammar
/// necessarily parses both to the same mathematical value. Collapse that
/// representation detail without rounding non-integral floats or large
/// integers.
fn number_to_string(number: &Number) -> String {
    if let Some(value) = number.as_i64() {
        return value.to_string();
    }
    if let Some(value) = number.as_u64() {
        return value.to_string();
    }

    let value = number
        .as_f64()
        .expect("serde_json numbers are finite and representable as a numeric kind");
    if value == 0.0 {
        return "0".to_owned();
    }
    if value.fract() == 0.0 {
        if value > 0.0 {
            let integer = value as u64;
            if integer as f64 == value {
                return integer.to_string();
            }
        } else {
            let integer = value as i64;
            if integer as f64 == value {
                return integer.to_string();
            }
        }
    }
    value.to_string()
}

pub(crate) fn to_string(value: &Value) -> String {
    match value {
        Value::Null => "null".to_owned(),
        Value::Bool(value) => value.to_string(),
        Value::Number(value) => number_to_string(value),
        Value::String(value) => serde_json::to_string(value).expect("JSON string serialization"),
        Value::Array(values) => format!(
            "[{}]",
            values.iter().map(to_string).collect::<Vec<_>>().join(",")
        ),
        Value::Object(values) => {
            let mut entries: Vec<_> = values.iter().collect();
            entries.sort_by(|left, right| left.0.cmp(right.0));
            format!(
                "{{{}}}",
                entries
                    .into_iter()
                    .map(|(key, value)| format!(
                        "{}:{}",
                        serde_json::to_string(key).expect("JSON key serialization"),
                        to_string(value)
                    ))
                    .collect::<Vec<_>>()
                    .join(",")
            )
        }
    }
}

pub(crate) fn equivalent(left: &Value, right: &Value) -> bool {
    to_string(left) == to_string(right)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recursively_sorts_object_keys_without_reordering_arrays() {
        let left = serde_json::json!({"z": [{"b": 2, "a": 1}], "a": true});
        let right = serde_json::json!({"a": true, "z": [{"a": 1, "b": 2}]});
        assert_eq!(to_string(&left), to_string(&right));
        assert_eq!(to_string(&left), r#"{"a":true,"z":[{"a":1,"b":2}]}"#);
    }

    #[test]
    fn normalizes_only_insignificant_json_number_representation() {
        let integer = serde_json::json!({"value": 1});
        let integral_float = serde_json::json!({"value": 1.0});
        let negative_zero = serde_json::json!({"value": -0.0});
        let zero = serde_json::json!({"value": 0});
        let large_integer = serde_json::json!({"value": 9_007_199_254_740_993_u64});

        assert!(equivalent(&integer, &integral_float));
        assert!(equivalent(&negative_zero, &zero));
        assert_eq!(to_string(&large_integer), r#"{"value":9007199254740993}"#);
        assert_eq!(to_string(&serde_json::json!(1.25)), "1.25");
    }
}
