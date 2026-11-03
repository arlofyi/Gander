// SPDX-License-Identifier: Apache-2.0
//! Runs `protocol/test-vectors/sas.json` against `gander_core::pairing::sas` (pairing spec, "Tests":
//! SAS derivation vectors). The vectors were produced by an independent implementation (Python
//! `hashlib`), so this test cross-checks the Rust derivation.

use std::path::Path;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use gander_core::pairing::sas::{derive_sas, first_20_bits, sas_digest};
use serde_json::Value;

fn hex(s: &str) -> Vec<u8> {
    assert!(s.len().is_multiple_of(2), "odd hex length");
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("hex"))
        .collect()
}

fn nonce(v: &Value, key: &str) -> [u8; 16] {
    let raw = hex(v[format!("{key}_hex")].as_str().expect("hex nonce"));
    let b64 = URL_SAFE_NO_PAD
        .decode(v[format!("{key}_b64url")].as_str().expect("b64url nonce"))
        .expect("base64url");
    assert_eq!(raw, b64, "{key}: hex and base64url forms disagree");
    raw.try_into().expect("16 bytes")
}

#[test]
fn sas_vectors() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../protocol/test-vectors/sas.json");
    let doc: Value =
        serde_json::from_str(&std::fs::read_to_string(&path).expect("sas.json")).unwrap();
    let vectors = doc["vectors"].as_array().expect("vectors");
    assert!(vectors.len() >= 5);
    let mut by_id = std::collections::HashMap::new();
    for v in vectors {
        let id = v["id"].as_str().unwrap();
        let spki = hex(v["host_spki_der_hex"].as_str().unwrap());
        let dn = nonce(v, "device_nonce");
        let hn = nonce(v, "host_nonce");
        let digest = sas_digest(&spki, &dn, &hn);
        assert_eq!(
            digest.to_vec(),
            hex(v["sha256_hex"].as_str().unwrap()),
            "{id}: digest"
        );
        assert_eq!(
            u64::from(first_20_bits(&digest)),
            v["first_20_bits"].as_u64().unwrap(),
            "{id}: first 20 bits"
        );
        let sas = derive_sas(&spki, &dn, &hn);
        assert_eq!(sas.to_string(), v["sas"].as_str().unwrap(), "{id}: sas");
        assert_eq!(
            sas.digits(),
            v["sas_digits"].as_str().unwrap(),
            "{id}: digits"
        );
        let host_id = URL_SAFE_NO_PAD.encode(ring_sha256(&spki));
        assert_eq!(host_id, v["host_id"].as_str().unwrap(), "{id}: host_id");
        by_id.insert(id.to_owned(), sas);
    }
    assert_ne!(
        by_id["sas-basic"], by_id["sas-mitm-other-spki"],
        "MITM must change the code"
    );
    assert!(
        vectors
            .iter()
            .any(|v| v["first_20_bits"].as_u64().unwrap() >= 1_000_000)
    );
    assert!(
        vectors
            .iter()
            .any(|v| v["sas"].as_str().unwrap().starts_with('0'))
    );
}

fn ring_sha256(data: &[u8]) -> Vec<u8> {
    ring::digest::digest(&ring::digest::SHA256, data)
        .as_ref()
        .to_vec()
}
