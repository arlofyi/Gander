// SPDX-License-Identifier: Apache-2.0
//! A small JSON Schema checker for `protocol/openapi.yaml` (already parsed into a
//! [`serde_json::Value`]), shared by the document test (`tests/openapi.rs`) and the WP-1B e2e
//! conformance suite (`gander-e2e`).
//!
//! It covers the subset of JSON Schema the document uses: `$ref` (local only), `type` including
//! type arrays, `properties`, `required`, `items`, `enum`, `const`, `oneOf`, `minimum`,
//! `maximum`. It is stricter than OpenAPI in one way: a property its object schema does not
//! declare is an error, so examples (and host responses) cannot drift from the schemas silently.
//! Clients must still ignore unknown fields; this strictness is for the project's own checks.

use serde_json::Value;

/// Resolves a local `$ref` (`#/components/...`) in `root`.
pub fn resolve<'a>(root: &'a Value, reference: &str) -> Option<&'a Value> {
    root.pointer(reference.strip_prefix('#')?)
}

/// The schema `components.schemas.<name>` of `root`.
pub fn named<'a>(root: &'a Value, name: &str) -> Option<&'a Value> {
    root.pointer(&format!("/components/schemas/{name}"))
}

fn type_matches(ty: &str, value: &Value) -> Result<bool, String> {
    Ok(match ty {
        "null" => value.is_null(),
        "boolean" => value.is_boolean(),
        "string" => value.is_string(),
        "integer" => value.is_i64() || value.is_u64(),
        "number" => value.is_number(),
        "object" => value.is_object(),
        "array" => value.is_array(),
        other => return Err(format!("schema uses unsupported type {other}")),
    })
}

/// Validates `value` against `schema` (inside document `root`), appending one message per
/// violation to `errors`, each prefixed with `at` (a path such as `body.entries[0]`). Problems
/// with the schema itself (unresolved `$ref`, unsupported keyword values) are reported as errors
/// too.
pub fn validate(root: &Value, schema: &Value, value: &Value, at: &str, errors: &mut Vec<String>) {
    if let Some(reference) = schema.get("$ref").and_then(Value::as_str) {
        match resolve(root, reference) {
            Some(target) => validate(root, target, value, at, errors),
            None => errors.push(format!("{at}: unresolved $ref {reference}")),
        }
        return;
    }
    if let Some(variants) = schema.get("oneOf").and_then(Value::as_array) {
        let matching = variants
            .iter()
            .filter(|v| {
                let mut e = Vec::new();
                validate(root, v, value, at, &mut e);
                e.is_empty()
            })
            .count();
        if matching != 1 {
            errors.push(format!(
                "{at}: matches {matching} oneOf variants, expected 1"
            ));
        }
    }
    if let Some(ty) = schema.get("type") {
        let ok = match ty {
            Value::String(t) => type_matches(t, value),
            Value::Array(ts) => ts.iter().try_fold(false, |acc, t| {
                let t = t
                    .as_str()
                    .ok_or_else(|| format!("bad type keyword at {at}"))?;
                Ok(acc || type_matches(t, value)?)
            }),
            _ => Err(format!("bad type keyword at {at}")),
        };
        match ok {
            Ok(true) => {}
            Ok(false) => {
                errors.push(format!("{at}: expected type {ty}, got {value}"));
                return;
            }
            Err(e) => {
                errors.push(format!("{at}: {e}"));
                return;
            }
        }
    }
    if let Some(c) = schema.get("const")
        && c != value
    {
        errors.push(format!("{at}: expected const {c}, got {value}"));
    }
    if let Some(options) = schema.get("enum").and_then(Value::as_array)
        && !options.contains(value)
    {
        errors.push(format!("{at}: {value} not in enum"));
    }
    if let Some(n) = value.as_f64() {
        if let Some(min) = schema.get("minimum").and_then(Value::as_f64)
            && n < min
        {
            errors.push(format!("{at}: {n} below minimum {min}"));
        }
        if let Some(max) = schema.get("maximum").and_then(Value::as_f64)
            && n > max
        {
            errors.push(format!("{at}: {n} above maximum {max}"));
        }
    }
    if let (Some(items), Some(array)) = (schema.get("items"), value.as_array()) {
        for (i, item) in array.iter().enumerate() {
            validate(root, items, item, &format!("{at}[{i}]"), errors);
        }
    }
    if let Some(object) = value.as_object() {
        if let Some(required) = schema.get("required").and_then(Value::as_array) {
            for key in required.iter().filter_map(Value::as_str) {
                if !object.contains_key(key) {
                    errors.push(format!("{at}: missing required {key}"));
                }
            }
        }
        if let Some(properties) = schema.get("properties").and_then(Value::as_object) {
            for (key, child) in object {
                match properties.get(key) {
                    Some(prop_schema) => {
                        validate(root, prop_schema, child, &format!("{at}.{key}"), errors)
                    }
                    None => errors.push(format!("{at}: undeclared property {key}")),
                }
            }
        }
    }
}

/// Validates `value` against `components.schemas.<name>`; `Err` lists every violation.
pub fn validate_named(root: &Value, name: &str, value: &Value) -> Result<(), Vec<String>> {
    let Some(schema) = named(root, name) else {
        return Err(vec![format!("no schema named {name}")]);
    };
    let mut errors = Vec::new();
    validate(root, schema, value, name, &mut errors);
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn doc() -> Value {
        json!({ "components": { "schemas": {
            "Kind": { "type": "string", "enum": ["a", "b"] },
            "Obj": {
                "type": "object",
                "required": ["k"],
                "properties": {
                    "k": { "$ref": "#/components/schemas/Kind" },
                    "n": { "type": ["integer", "null"], "minimum": 1, "maximum": 3 },
                    "l": { "type": "array", "items": { "type": "boolean" } }
                }
            }
        }}})
    }

    #[test]
    fn accepts_and_rejects() {
        let d = doc();
        assert!(validate_named(&d, "Obj", &json!({ "k": "a", "n": null, "l": [true] })).is_ok());
        let errs = validate_named(&d, "Obj", &json!({ "n": 9, "l": [1], "x": 0 })).unwrap_err();
        assert_eq!(errs.len(), 4, "{errs:?}");
        assert!(validate_named(&d, "Obj", &json!({ "k": "c" })).is_err());
        assert!(validate_named(&d, "Missing", &json!({})).is_err());
    }
}
