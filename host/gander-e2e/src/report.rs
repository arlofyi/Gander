// SPDX-License-Identifier: Apache-2.0
//! The report: one entry per case, an error-code coverage table and a summary.

use std::collections::BTreeMap;

use gander_core::error::ErrorCode;
use serde_json::{Value, json};

/// What a case checks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// A documented success response, validated against the schema.
    Success,
    /// A documented error code.
    Error,
    /// A `/v1/events` message.
    Event,
}

impl Kind {
    fn as_str(self) -> &'static str {
        match self {
            Kind::Success => "success",
            Kind::Error => "error",
            Kind::Event => "event",
        }
    }
}

/// The outcome of a case.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Pass,
    Fail(String),
    /// The case cannot be produced from outside the host; the reason says why.
    NotReachable(String),
}

#[derive(Debug, Clone)]
pub struct Case {
    pub id: String,
    pub endpoint: String,
    pub kind: Kind,
    pub code: Option<ErrorCode>,
    pub outcome: Outcome,
    pub ms: u128,
}

#[derive(Debug, Default)]
pub struct Report {
    pub cases: Vec<Case>,
}

impl Report {
    pub fn push(&mut self, case: Case) {
        let (tag, extra) = match &case.outcome {
            Outcome::Pass => ("PASS", String::new()),
            Outcome::Fail(why) => ("FAIL", format!(": {why}")),
            Outcome::NotReachable(why) => ("N/A ", format!(": {why}")),
        };
        eprintln!("{tag} {:<44} {}{extra}", case.id, case.endpoint);
        self.cases.push(case);
    }

    fn count(&self, f: impl Fn(&Outcome) -> bool) -> usize {
        self.cases.iter().filter(|c| f(&c.outcome)).count()
    }

    pub fn failed(&self) -> usize {
        self.count(|o| matches!(o, Outcome::Fail(_)))
    }

    /// Per error code: the passing cases that produced it, or why it is not reachable.
    /// A code with neither is `missing` (counted as a failure).
    pub fn coverage(&self) -> BTreeMap<&'static str, Value> {
        let mut out = BTreeMap::new();
        for code in ErrorCode::ALL {
            let mine = || self.cases.iter().filter(move |c| c.code == Some(code));
            let passed: Vec<&str> = mine()
                .filter(|c| c.outcome == Outcome::Pass)
                .map(|c| c.id.as_str())
                .collect();
            let failed: Vec<&str> = mine()
                .filter(|c| matches!(c.outcome, Outcome::Fail(_)))
                .map(|c| c.id.as_str())
                .collect();
            let reason = mine().find_map(|c| match &c.outcome {
                Outcome::NotReachable(r) => Some(r.clone()),
                _ => None,
            });
            let entry = if !failed.is_empty() {
                json!({ "status": "failed", "cases": passed, "failed_cases": failed })
            } else if !passed.is_empty() {
                json!({ "status": "covered", "cases": passed })
            } else if let Some(reason) = reason {
                json!({ "status": "not_reachable", "reason": reason })
            } else {
                json!({ "status": "missing" })
            };
            out.insert(code.as_str(), entry);
        }
        out
    }

    pub fn missing_codes(&self) -> Vec<&'static str> {
        self.coverage()
            .into_iter()
            .filter(|(_, v)| v["status"] == "missing")
            .map(|(k, _)| k)
            .collect()
    }

    pub fn to_json(&self, meta: Value) -> Value {
        let cases: Vec<Value> = self
            .cases
            .iter()
            .map(|c| {
                let (status, detail) = match &c.outcome {
                    Outcome::Pass => ("pass", Value::Null),
                    Outcome::Fail(d) => ("fail", json!(d)),
                    Outcome::NotReachable(d) => ("not_reachable", json!(d)),
                };
                json!({
                    "id": c.id,
                    "endpoint": c.endpoint,
                    "kind": c.kind.as_str(),
                    "code": c.code.map(|c| c.as_str()),
                    "status": status,
                    "detail": detail,
                    "ms": c.ms,
                })
            })
            .collect();
        let coverage = self.coverage();
        let codes_covered = coverage
            .values()
            .filter(|v| v["status"] == "covered")
            .count();
        let codes_not_reachable = coverage
            .values()
            .filter(|v| v["status"] == "not_reachable")
            .count();
        let missing = self.missing_codes();
        let passed = self.count(|o| *o == Outcome::Pass);
        let failed = self.failed();
        let not_reachable = self.count(|o| matches!(o, Outcome::NotReachable(_)));
        json!({
            "meta": meta,
            "summary": {
                "ok": failed == 0 && missing.is_empty(),
                "total": self.cases.len(),
                "passed": passed,
                "failed": failed,
                "not_reachable": not_reachable,
                "error_codes": {
                    "total": ErrorCode::ALL.len(),
                    "covered": codes_covered,
                    "not_reachable": codes_not_reachable,
                    "missing": missing,
                },
            },
            "error_codes": coverage,
            "cases": cases,
        })
    }
}
