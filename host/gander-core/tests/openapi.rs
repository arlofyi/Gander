// SPDX-License-Identifier: Apache-2.0
//! Keeps `protocol/openapi.yaml` and the Rust DTOs in step.
//!
//! - `protocol_examples_roundtrip`: every `components.examples` entry validates against the schema
//!   named by its `x-gander-schema`, deserialises into the matching Rust type and serialises back
//!   to the identical JSON value.
//! - every `$ref` resolves; every JSON response and request body carries at least one example;
//!   the `ErrorCode` enum in the document equals `ErrorCode::ALL`.
//!
//! The schema validator is `gander_core::protocol::schema` (shared with the WP-1B e2e suite). It
//! covers the subset of JSON Schema the document uses and is stricter than OpenAPI in one way:
//! an example property that its object schema does not declare is an error, so examples and
//! schemas cannot drift apart silently.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use gander_core::error::{ErrorBody, ErrorCode};
use gander_core::protocol::events::{ClientEvent, ServerEvent};
use gander_core::protocol::files::{
    Entry, HashResponse, ListResponse, PreviewInfo, RecentResponse, SearchResponse, SharesResponse,
    ZipPlan,
};
use gander_core::protocol::host::{HostHello, MeResponse, TokenRotateResponse};
use gander_core::protocol::input::InputMessage;
use gander_core::protocol::pairing::{PairApproved, PairPollStatus, PairRequest, PairStarted};
use gander_core::protocol::schema::{self, validate};
use gander_core::protocol::stream::{StreamCreated, StreamRequest};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

type Roundtrip = fn(&Value) -> Result<Value, String>;

fn roundtrip<T: DeserializeOwned + Serialize>(value: &Value) -> Result<Value, String> {
    let typed: T = serde_json::from_value(value.clone()).map_err(|e| format!("decode: {e}"))?;
    serde_json::to_value(&typed).map_err(|e| format!("encode: {e}"))
}

/// Schema name in `components.schemas` -> Rust type.
fn dto_table() -> BTreeMap<&'static str, Roundtrip> {
    let mut t: BTreeMap<&'static str, Roundtrip> = BTreeMap::new();
    t.insert("ErrorBody", roundtrip::<ErrorBody>);
    t.insert("HostHello", roundtrip::<HostHello>);
    t.insert("MeResponse", roundtrip::<MeResponse>);
    t.insert("TokenRotateResponse", roundtrip::<TokenRotateResponse>);
    t.insert("PairRequest", roundtrip::<PairRequest>);
    t.insert("PairStarted", roundtrip::<PairStarted>);
    t.insert("PairPollStatus", roundtrip::<PairPollStatus>);
    t.insert("PairApproved", roundtrip::<PairApproved>);
    t.insert("SharesResponse", roundtrip::<SharesResponse>);
    t.insert("ListResponse", roundtrip::<ListResponse>);
    t.insert("Entry", roundtrip::<Entry>);
    t.insert("SearchResponse", roundtrip::<SearchResponse>);
    t.insert("RecentResponse", roundtrip::<RecentResponse>);
    t.insert("PreviewInfo", roundtrip::<PreviewInfo>);
    t.insert("HashResponse", roundtrip::<HashResponse>);
    t.insert("ZipPlan", roundtrip::<ZipPlan>);
    t.insert("StreamRequest", roundtrip::<StreamRequest>);
    t.insert("StreamCreated", roundtrip::<StreamCreated>);
    t.insert("ServerEvent", roundtrip::<ServerEvent>);
    t.insert("ClientEvent", roundtrip::<ClientEvent>);
    t.insert("InputMessage", roundtrip::<InputMessage>);
    t
}

fn load_openapi() -> Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../protocol/openapi.yaml");
    let text = std::fs::read_to_string(&path).expect("read protocol/openapi.yaml");
    serde_norway::from_str(&text).expect("openapi.yaml parses as YAML")
}

fn resolve<'a>(root: &'a Value, reference: &str) -> &'a Value {
    schema::resolve(root, reference).unwrap_or_else(|| panic!("unresolved $ref {reference}"))
}

/// A per-run random string standing in for token material; examples never carry real secrets.
fn per_run_secret() -> String {
    use std::hash::{BuildHasher, Hasher};
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let state = std::collections::hash_map::RandomState::new();
    (0..43)
        .map(|i| {
            let mut h = state.build_hasher();
            h.write_usize(i);
            ALPHABET[(h.finish() % 64) as usize] as char
        })
        .collect()
}

fn replace_placeholders(value: &mut Value) {
    match value {
        Value::String(s) if s.starts_with("PLACEHOLDER_") => *s = per_run_secret(),
        Value::Array(items) => items.iter_mut().for_each(replace_placeholders),
        Value::Object(map) => map.values_mut().for_each(replace_placeholders),
        _ => {}
    }
}

fn examples(doc: &Value) -> &serde_json::Map<String, Value> {
    doc.pointer("/components/examples")
        .and_then(Value::as_object)
        .expect("components.examples")
}

#[test]
fn protocol_examples_roundtrip() {
    let doc = load_openapi();
    let table = dto_table();
    let mut failures = Vec::new();
    let mut covered = BTreeSet::new();

    for (name, example) in examples(&doc) {
        let schema_name = example
            .get("x-gander-schema")
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("example {name} lacks x-gander-schema"));
        let Some(rt) = table.get(schema_name) else {
            failures.push(format!(
                "{name}: no Rust type mapped for schema {schema_name}"
            ));
            continue;
        };
        covered.insert(schema_name);
        let mut value = example.get("value").cloned().expect("example value");
        replace_placeholders(&mut value);

        let schema = resolve(&doc, &format!("#/components/schemas/{schema_name}"));
        let mut errors = Vec::new();
        validate(&doc, schema, &value, name, &mut errors);
        failures.extend(errors);

        match rt(&value) {
            Ok(back) if back == value => {}
            Ok(back) => failures.push(format!(
                "{name}: round trip changed the JSON\n  spec: {value}\n  rust: {back}"
            )),
            Err(e) => failures.push(format!("{name}: {e}")),
        }
    }

    for schema_name in table.keys() {
        if !covered.contains(schema_name) {
            failures.push(format!("schema {schema_name} has no example"));
        }
    }
    assert!(failures.is_empty(), "\n{}", failures.join("\n"));
}

#[test]
fn every_ref_resolves() {
    fn walk(root: &Value, node: &Value, count: &mut usize) {
        match node {
            Value::Object(map) => {
                if let Some(r) = map.get("$ref").and_then(Value::as_str) {
                    resolve(root, r);
                    *count += 1;
                }
                map.values().for_each(|v| walk(root, v, count));
            }
            Value::Array(items) => items.iter().for_each(|v| walk(root, v, count)),
            _ => {}
        }
    }
    let doc = load_openapi();
    let mut count = 0;
    walk(&doc, &doc, &mut count);
    assert!(count > 50, "suspiciously few refs: {count}");
}

#[test]
fn every_json_body_has_an_example() {
    let doc = load_openapi();
    let paths = doc.get("paths").and_then(Value::as_object).expect("paths");
    let mut missing = Vec::new();
    for (path, item) in paths {
        for (method, op) in item.as_object().expect("path item") {
            let mut bodies: Vec<(String, &Value)> = Vec::new();
            if let Some(rb) = op.get("requestBody") {
                bodies.push(("request".into(), rb));
            }
            if let Some(responses) = op.get("responses").and_then(Value::as_object) {
                for (status, resp) in responses {
                    if resp.get("$ref").is_none() {
                        bodies.push((status.clone(), resp));
                    }
                }
            }
            for (what, body) in bodies {
                if let Some(json) = body.pointer("/content/application~1json") {
                    let has = json
                        .get("examples")
                        .and_then(Value::as_object)
                        .is_some_and(|m| !m.is_empty());
                    if !has {
                        missing.push(format!("{method} {path} {what}"));
                    }
                }
            }
        }
    }
    assert!(missing.is_empty(), "no example: {missing:?}");
}

#[test]
fn error_codes_match_openapi() {
    let doc = load_openapi();
    let in_doc: BTreeSet<String> = doc
        .pointer("/components/schemas/ErrorCode/enum")
        .and_then(Value::as_array)
        .expect("ErrorCode enum")
        .iter()
        .map(|v| v.as_str().expect("string").to_owned())
        .collect();
    let in_rust: BTreeSet<String> = ErrorCode::ALL
        .iter()
        .map(|c| c.as_str().to_owned())
        .collect();
    assert_eq!(in_doc, in_rust);
}

#[test]
fn validator_catches_drift() {
    // Guard against a validator that accepts everything.
    let doc = load_openapi();
    let schema = resolve(&doc, "#/components/schemas/HostHello");
    let mut errors = Vec::new();
    let bad = serde_json::json!({ "protocol": [1], "pairing_open": "no", "extra": 1 });
    validate(&doc, schema, &bad, "bad", &mut errors);
    assert_eq!(errors.len(), 3, "{errors:?}");
}
