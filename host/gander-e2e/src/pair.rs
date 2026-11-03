// SPDX-License-Identifier: Apache-2.0
//! `gander-e2e pair`: pairs one test device through the test hook (flow B) and hands its token to
//! the caller in a file, for `cargo xtask e2e-android` (the instrumented fixture tests need a
//! token of a device that already holds the share's grant).
//!
//! ```text
//! gander-e2e pair --addr 127.0.0.1:<port> --host-id <id> --name <device name>
//!                 --share <share-id> --perms list,preview,fetch --out <json>
//! ```
//!
//! Waits until `GET /v1/shares` shows `--share` with every permission in `--perms` (the test host
//! applies `--test-grant` asynchronously after the approval), then writes
//! `{"device_id": "...", "token": "..."}` to `--out`. The token is never printed; stdout carries
//! `paired <device_id>` only.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, bail};
use gander_core::pairing::b64url;
use gander_core::protocol::HostId;
use serde_json::{Value, json};

use crate::client::{Client, Req};

/// Options of the `pair` subcommand.
#[derive(Debug)]
pub struct PairOptions {
    pub addr: SocketAddr,
    pub host_id: String,
    pub name: String,
    pub share: String,
    pub perms: Vec<String>,
    pub out: PathBuf,
}

pub fn parse(args: impl Iterator<Item = String>) -> Result<PairOptions> {
    let mut addr = None;
    let mut host_id = None;
    let mut name = None;
    let mut share = None;
    let mut perms = Vec::new();
    let mut out = None;
    let mut it = args;
    while let Some(a) = it.next() {
        let mut value = || it.next().with_context(|| format!("{a} needs a value"));
        match a.as_str() {
            "--addr" => addr = Some(value()?.parse::<SocketAddr>()?),
            "--host-id" => host_id = Some(value()?),
            "--name" => name = Some(value()?),
            "--share" => share = Some(value()?),
            "--perms" => {
                perms = value()?
                    .split(',')
                    .map(str::trim)
                    .filter(|p| !p.is_empty())
                    .map(str::to_owned)
                    .collect();
            }
            "--out" => out = Some(PathBuf::from(value()?)),
            other => bail!("unknown pair argument {other}"),
        }
    }
    let addr = addr.context("--addr is required")?;
    if !addr.ip().is_loopback() {
        bail!("--addr must be a loopback address");
    }
    Ok(PairOptions {
        addr,
        host_id: host_id.context("--host-id is required")?,
        name: name.unwrap_or_else(|| "e2e device".to_owned()),
        share: share.context("--share is required")?,
        perms,
        out: out.context("--out is required")?,
    })
}

fn random_nonce() -> String {
    let mut b = [0u8; 16];
    let rng = ring::rand::SystemRandom::new();
    let _ = ring::rand::SecureRandom::fill(&rng, &mut b);
    b64url(&b)
}

/// True when `shares` (a `SharesResponse`) lists `share` with every permission in `perms`.
fn has_grant(shares: &Value, share: &str, perms: &[String]) -> bool {
    shares["shares"].as_array().is_some_and(|all| {
        all.iter().any(|s| {
            s["id"] == share
                && perms.iter().all(|p| {
                    s["permissions"]
                        .as_array()
                        .is_some_and(|have| have.iter().any(|h| h == p.as_str()))
                })
        })
    })
}

pub async fn run(o: &PairOptions) -> Result<()> {
    let (c, device) = pair_client(o.addr, &o.host_id, &o.name, &o.share, &o.perms).await?;
    let token = c.token.clone().unwrap_or_default();
    let out = json!({ "device_id": device, "token": token });
    std::fs::write(&o.out, serde_json::to_vec(&out)?)
        .with_context(|| format!("write {}", o.out.display()))?;
    println!("paired {device}");
    Ok(())
}

/// Pairs one device through the test hook and waits until `share` shows every permission in
/// `perms`; returns the client (holding the token) and the device id.
pub async fn pair_client(
    addr: SocketAddr,
    host_id: &str,
    name: &str,
    share: &str,
    perms: &[String],
) -> Result<(Client, String)> {
    let mut c = Client::new(addr, HostId::from(host_id))?;
    let body = json!({
        "device": { "name": name, "platform": "e2e", "model": "gander-e2e", "app_version": "0.0.0" },
        "nonce": random_nonce(),
    });
    let r = c
        .send(&Req::new("POST", "/v1/pair").anonymous().json(&body))
        .await?;
    if r.status != 200 {
        bail!("POST /v1/pair: {}", r.describe());
    }
    let id = r
        .json()
        .and_then(|v| v["pairing_id"].as_str().map(str::to_owned))
        .context("PairStarted without pairing_id")?;

    let deadline = Instant::now() + Duration::from_secs(15);
    let (token, device) = loop {
        let r = c
            .send(&Req::get(format!("/v1/pair/{id}")).anonymous())
            .await?;
        match r.status {
            202 if Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
            200 => {
                let v = r.json().context("PairApproved is not JSON")?;
                let token = v["token"].as_str().context("no token")?.to_owned();
                let device = v["device_id"].as_str().context("no device_id")?.to_owned();
                break (token, device);
            }
            _ => bail!("GET /v1/pair/{{id}}: {}", r.describe()),
        }
    };
    c.token = Some(token);

    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let r = c.send(&Req::get("/v1/shares")).await?;
        if r.status == 200 && r.json().is_some_and(|v| has_grant(&v, share, perms)) {
            break;
        }
        if Instant::now() > deadline {
            bail!(
                "GET /v1/shares never showed {share} with {perms:?} ({})",
                r.describe()
            );
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    Ok((c, device))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    #[test]
    fn has_grant_needs_every_permission_on_the_named_share() {
        let v = json!({ "shares": [
            { "id": "shr_a", "permissions": ["list", "preview"] },
            { "id": "shr_b", "permissions": ["list", "preview", "fetch"] },
        ]});
        let perms = |p: &[&str]| p.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
        assert!(super::has_grant(&v, "shr_b", &perms(&["list", "fetch"])));
        assert!(!super::has_grant(&v, "shr_a", &perms(&["fetch"])));
        assert!(!super::has_grant(&v, "shr_c", &perms(&[])));
        assert!(super::has_grant(&v, "shr_a", &perms(&[])));
    }
}
