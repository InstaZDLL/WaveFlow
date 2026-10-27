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
