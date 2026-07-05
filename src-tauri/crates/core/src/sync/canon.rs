//! Canonical-form field builders for payload-hash computation.
//!
//! RFC-003 §4 — the `payload_hash` BLAKE3 input is built from the
//! entity's "canonical wire form": every synced field plus
//! `(hlc.wall, hlc.logical, origin_device_id)`, serialised in a
//! deterministic shape (sorted JSON keys, lower-case hex for binary
//! blobs). The actual sorting + serialisation lives in
//! [`crate::sync::payload_hash::canonical_serialize`]; this module
//! is the cheap typed surface that builds the `Map<String, Value>`
//! payload before handing it off.
//!
//! Both the server's apply pipeline AND the desktop's CRUD command
//! sites need the same field map for the same entity shape — without
//! a shared module, divergence is easy: one side adds a field, the
//! other forgets, and every digest comparison silently disagrees on
//! the affected row until a manual re-sync. Putting these helpers in
//! `waveflow-core` makes the desktop emit, the desktop digest, and
//! the server apply share a single source of truth.
//!
//! Each helper inserts a key-value pair into the caller's map; the
//! caller controls insertion ORDER, but `canonical_serialize`
//! re-sorts everything via `BTreeMap` before hashing, so the
//! insertion order doesn't matter for the hash. The helpers exist
//! to nudge call sites toward consistent JSON shapes — every string
//! becomes `Value::String`, every `Option` becomes a real
//! `Value::Null` rather than an absent key.

use serde_json::{Map, Value};

/// Insert `key → Value::String(value)`.
pub fn string(map: &mut Map<String, Value>, key: &str, value: &str) {
    map.insert(key.to_string(), Value::String(value.to_string()));
}

/// Insert `key → Value::String(value) | Value::Null`. The explicit
/// `Null` keeps the field present in the JSON even when absent — a
/// renamed-to-empty payload still hashes differently from a never-
/// emitted one.
pub fn opt_string(map: &mut Map<String, Value>, key: &str, value: Option<&str>) {
    map.insert(
        key.to_string(),
        value
            .map(|s| Value::String(s.to_string()))
            .unwrap_or(Value::Null),
    );
}

/// Insert `key → Value::Number(value)`.
pub fn i64(map: &mut Map<String, Value>, key: &str, value: i64) {
    map.insert(key.to_string(), Value::from(value));
}

/// Insert `key → Value::Number(value) | Value::Null`.
pub fn opt_i64(map: &mut Map<String, Value>, key: &str, value: Option<i64>) {
    map.insert(
        key.to_string(),
        value.map(Value::from).unwrap_or(Value::Null),
    );
}

/// Insert `key → Value::Bool(value)`.
pub fn bool(map: &mut Map<String, Value>, key: &str, value: bool) {
    map.insert(key.to_string(), Value::Bool(value));
}

/// Insert `key → Value::Array([Value::String, …])`. Arrays preserve
/// source order in [`crate::sync::payload_hash::canonical_serialize`]
/// (only object keys are sorted), so a multi-artist tag
/// `[Tyler, Earl]` hashes differently from `[Earl, Tyler]` — exactly
/// what the apply pipeline needs to avoid silently swapping primary
/// and feature artists on a re-emit.
pub fn strings(map: &mut Map<String, Value>, key: &str, values: &[String]) {
    map.insert(
        key.to_string(),
        Value::Array(values.iter().map(|s| Value::String(s.clone())).collect()),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn string_inserts_value_string() {
        let mut m = Map::new();
        string(&mut m, "name", "Alice");
        assert_eq!(m.get("name").unwrap(), &json!("Alice"));
    }

    #[test]
    fn opt_string_inserts_null_for_none() {
        let mut m = Map::new();
        opt_string(&mut m, "description", None);
        assert_eq!(m.get("description").unwrap(), &Value::Null);
    }

    #[test]
    fn opt_string_inserts_value_for_some() {
        let mut m = Map::new();
        opt_string(&mut m, "description", Some("Mix"));
        assert_eq!(m.get("description").unwrap(), &json!("Mix"));
    }

    #[test]
    fn opt_i64_inserts_null_for_none() {
        let mut m = Map::new();
        opt_i64(&mut m, "rating", None);
        assert_eq!(m.get("rating").unwrap(), &Value::Null);
    }

    #[test]
    fn opt_i64_inserts_number_for_some() {
        let mut m = Map::new();
        opt_i64(&mut m, "rating", Some(5));
        assert_eq!(m.get("rating").unwrap(), &json!(5));
    }

    #[test]
    fn bool_inserts_value_bool() {
        let mut m = Map::new();
        bool(&mut m, "is_compilation", true);
        assert_eq!(m.get("is_compilation").unwrap(), &json!(true));
    }

    #[test]
    fn strings_inserts_array_preserving_order() {
        let mut m = Map::new();
        strings(&mut m, "artists", &["Tyler".into(), "Earl".into()]);
        assert_eq!(m.get("artists").unwrap(), &json!(["Tyler", "Earl"]));
        // Distinct from the swap.
        assert_ne!(m.get("artists").unwrap(), &json!(["Earl", "Tyler"]));
    }
}
