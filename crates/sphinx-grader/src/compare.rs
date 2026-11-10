//! Value equality for grading: JSON equality where numbers compare by value
//! (`1` equals `1.0`), so engines that return integers as floats are not
//! penalised. Object key order is irrelevant; array order matters.

use serde_json::Value;

#[allow(clippy::float_cmp, reason = "exact numeric equality is the grading rule")]
pub fn values_equal(expected: &Value, actual: &Value) -> bool {
    match (expected, actual) {
        (Value::Number(a), Value::Number(b)) => match (a.as_f64(), b.as_f64()) {
            (Some(x), Some(y)) => x == y,
            _ => false,
        },
        (Value::Array(a), Value::Array(b)) => {
            a.len() == b.len() && a.iter().zip(b).all(|(x, y)| values_equal(x, y))
        }
        (Value::Object(a), Value::Object(b)) => {
            a.len() == b.len()
                && a.iter().all(|(k, v)| b.get(k).is_some_and(|w| values_equal(v, w)))
        }
        _ => expected == actual,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn numbers_compare_by_value() {
        assert!(values_equal(&json!(1), &json!(1.0)));
        assert!(!values_equal(&json!(1), &json!(1.5)));
        assert!(!values_equal(&json!(1), &json!("1")));
    }

    #[test]
    fn structures_compare_deeply() {
        assert!(values_equal(&json!({"a": [1, {"b": 2}]}), &json!({"a": [1.0, {"b": 2}]})));
        assert!(!values_equal(&json!({"a": 1}), &json!({"a": 1, "b": 2})));
        assert!(!values_equal(&json!([1, 2]), &json!([2, 1])));
        assert!(values_equal(&json!(null), &json!(null)));
    }
}
