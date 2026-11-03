// SPDX-License-Identifier: Apache-2.0
//! Runs `protocol/test-vectors/policy.json` against the stream launch policy with a fake
//! association table.

use std::path::{Path, PathBuf};

use gander_core::config::{AppCatalogue, AppEntry};
use gander_core::policy::{
    Browser, HandlerInfo, PolicyEnvironment, PolicyInput, Signature, StaticAssociations,
    StreamPolicy,
};
use gander_core::protocol::AppId;
use gander_core::protocol::files::StreamDecision;
use serde_json::Value;

fn vectors_file() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../protocol/test-vectors/policy.json")
        .canonicalize()
        .expect("policy.json exists")
}

fn decision(s: &str) -> StreamDecision {
    serde_json::from_value(Value::String(s.to_owned())).expect("allowed|refused|ask")
}

fn handler(v: &Value) -> HandlerInfo {
    let exe = v["exe"].as_str().expect("handler.exe");
    let signature = match v["signature"].as_str().expect("handler.signature") {
        "signed" => Signature::Signed {
            publisher: v["publisher"].as_str().expect("publisher").to_owned(),
        },
        "unsigned" => Signature::Unsigned,
        "untrusted" => Signature::Untrusted,
        "package" => Signature::Package,
        other => panic!("unknown signature {other}"),
    };
    let mut info = HandlerInfo::new(exe, signature);
    info.package_family_name = v
        .get("package_family_name")
        .and_then(Value::as_str)
        .map(str::to_owned);
    info
}

fn browser(name: &str) -> Browser {
    match name {
        "edge" => Browser::Edge,
        "chrome" => Browser::Chrome,
        "firefox" => Browser::Firefox,
        "brave" => Browser::Brave,
        "opera" => Browser::Opera,
        "vivaldi" => Browser::Vivaldi,
        "arc" => Browser::Arc,
        other => panic!("unknown browser {other}"),
    }
}

#[test]
fn policy_vectors() {
    let text = std::fs::read_to_string(vectors_file()).unwrap();
    let doc: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(doc["version"], 1);

    let environment = PolicyEnvironment {
        system_root: doc["system_root"].as_str().unwrap().into(),
    };
    let apps: Vec<AppEntry> = serde_json::from_value(doc["catalogue"].clone()).unwrap();
    let catalogue = AppCatalogue {
        apps,
        ..AppCatalogue::default()
    };
    let mut base = StaticAssociations::new();
    for (ext, h) in doc["associations"].as_object().unwrap() {
        base.insert(ext, handler(h));
    }
    // Catalogue apps are inspectable by their path (a client may name one).
    for entry in &catalogue.apps {
        let mut info = HandlerInfo::new(
            entry.exe.clone(),
            match (&entry.publisher, &entry.package_family_name) {
                (Some(p), _) => Signature::Signed {
                    publisher: p.clone(),
                },
                (None, Some(_)) => Signature::Package,
                (None, None) => Signature::Unsigned,
            },
        );
        info.package_family_name = entry.package_family_name.clone();
        base.insert_executable(info);
    }

    let vectors = doc["vectors"].as_array().unwrap();
    assert!(!vectors.is_empty());
    let mut failures = Vec::new();
    for v in vectors {
        let id = v["id"].as_str().unwrap();
        let file = v["file"].as_str().unwrap();
        let ext = file.rsplit_once('.').map(|(_, e)| e).unwrap_or("");

        let mut assoc = base.clone();
        let mut no_handler_override = false;
        if let Some(h) = v.get("handler") {
            if h.is_null() {
                no_handler_override = true;
            } else {
                assoc.insert(ext, handler(h));
            }
        }
        let assoc = if no_handler_override {
            // Same table without this extension.
            let mut t = StaticAssociations::new();
            for (e, h) in doc["associations"].as_object().unwrap() {
                if !e.eq_ignore_ascii_case(ext) {
                    t.insert(e, handler(h));
                }
            }
            t
        } else {
            assoc
        };

        let app = v.get("app").and_then(Value::as_str).map(AppId::from);
        let input = PolicyInput {
            file_name: file,
            stream_permitted: v
                .get("stream_permitted")
                .and_then(Value::as_bool)
                .unwrap_or(true),
            share_default: v
                .get("share_default")
                .and_then(Value::as_str)
                .map(decision)
                .unwrap_or(StreamDecision::Ask),
            requested_app: app.as_ref(),
        };
        let got = StreamPolicy::new(&catalogue, &assoc, &environment).evaluate(&input);

        let expect = decision(v["expect"].as_str().unwrap());
        let reason = v["reason"].as_str().unwrap();
        if got.outcome != expect || got.reason.as_str() != reason {
            failures.push(format!(
                "{id}: expected {expect:?}/{reason}, got {:?}/{}",
                got.outcome,
                got.reason.as_str()
            ));
        }
        let want_browser = v.get("browser").and_then(Value::as_str).map(browser);
        let got_browser = got.browser.map(|b| b.browser);
        if want_browser != got_browser {
            failures.push(format!(
                "{id}: expected browser {want_browser:?}, got {got_browser:?}"
            ));
        }
        if got.outcome == StreamDecision::Refused {
            let body = got.error_body().expect("refusal has an error body");
            assert!(!body.error.message.contains('\\'), "{id}: path in message");
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
