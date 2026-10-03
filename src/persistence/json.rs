//! Serializing values into JSON text, with a *valid JSON* empty fallback.
//!
//! Every store keeps structured values in `TEXT` columns declared `NOT NULL
//! DEFAULT '{}'` (`metadata`), `DEFAULT '[]'` (`aliases`,
//! `related_characters`), or `DEFAULT '[]'` (`vector`). Serialization only
//! fails for values JSON cannot represent — non-string map keys, non-finite
//! floats — which is rare enough that the failure path used to be spelled three
//! different ways for the same operation: `unwrap_or_default()` (the empty
//! string), `"[]"`, and `"{}"`.
//!
//! The empty string is the odd one out: it is **not** valid JSON, so a column
//! written that way disagrees with its own declaration and breaks any reader
//! that parses the column without a fallback of its own. These two helpers make
//! the intended empty value explicit and match the DDL.

use serde::Serialize;

/// Serialize `value` for a JSON `TEXT` column holding an **object**.
///
/// Falls back to `{}` — the same text the column default would have produced —
/// when the value cannot be represented as JSON.
#[must_use]
pub(crate) fn json_object(value: &impl Serialize) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "{}".to_string())
}

/// Serialize `value` for a JSON `TEXT` column holding an **array**.
///
/// Falls back to `[]`, the array-shaped counterpart of [`json_object`].
#[must_use]
pub(crate) fn json_array(value: &impl Serialize) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "[]".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Objective: Verify a representable value is serialized verbatim, so the
    /// helpers are drop-in replacements for the inline `serde_json::to_string`
    /// they replaced.
    /// Invariants: the output is the compact JSON form of the input and an empty
    /// collection keeps its own shape.
    #[test]
    fn serializes_representable_values() {
        assert_eq!(
            json_object(&json!({"a": 1})),
            r#"{"a":1}"#,
            "an object must round-trip to its compact form"
        );
        assert_eq!(
            json_array(&vec![1, 2, 3]),
            "[1,2,3]",
            "an array must round-trip to its compact form"
        );
        assert_eq!(
            json_array(&Vec::<String>::new()),
            "[]",
            "an empty list is still an array, not a fallback"
        );
        assert_eq!(
            json_object(&json!({})),
            "{}",
            "an empty object is still an object, not a fallback"
        );
    }

    /// A value that always refuses to serialize.
    ///
    /// A hand-written `Serialize` that rejects its own input is the only
    /// reliable trigger: serde_json tolerates the cases that look like failures
    /// (integer map keys are stringified, non-finite floats become `null`),
    /// which is exactly why the fallback has to be valid JSON rather than
    /// whatever `String::default()` happens to be.
    struct Unrepresentable;

    impl serde::Serialize for Unrepresentable {
        fn serialize<S: serde::Serializer>(&self, _serializer: S) -> Result<S::Ok, S::Error> {
            Err(<S::Error as serde::ser::Error>::custom(
                "cannot be represented",
            ))
        }
    }

    /// Objective: Verify that when serialization fails, the fallback is valid
    /// JSON of the column's shape. The previous `unwrap_or_default()` wrote the
    /// empty string here, which no JSON reader can parse and which contradicts
    /// the column's `DEFAULT '{}'`.
    /// Invariants: both fallbacks parse as JSON and equal the column default.
    #[test]
    fn fallbacks_stay_valid_json() {
        let object_fallback = json_object(&Unrepresentable);
        assert_eq!(
            object_fallback, "{}",
            "an object falls back to the empty object"
        );
        assert!(
            serde_json::from_str::<serde_json::Value>(&object_fallback).is_ok(),
            "the object fallback {object_fallback:?} must itself be valid JSON"
        );

        let array_fallback = json_array(&Unrepresentable);
        assert_eq!(
            array_fallback, "[]",
            "an array falls back to the empty array"
        );
        assert!(
            serde_json::from_str::<serde_json::Value>(&array_fallback).is_ok(),
            "the array fallback {array_fallback:?} must itself be valid JSON"
        );
    }
}
