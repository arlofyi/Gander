// SPDX-License-Identifier: Apache-2.0
//! WP-46 identity spike: from an unelevated process, ask the local tailscaled who a tailnet
//! address belongs to (LocalAPI `whois`, over the Windows named pipe, with the `tailscale` CLI as
//! a fallback) and how each peer is reached (`status`). Throwaway research code: it feeds the
//! trust adapter of WP-12 and is not part of the product.
//!
//! Only the local daemon is contacted. No peer is pinged or connected to, and no Tailscale
//! setting is read for writing or changed.

mod analysis;
mod context;
mod http;
mod localapi;
mod redact;
mod report;

use std::path::PathBuf;
use std::process::ExitCode;

const USAGE: &str = "\
spike-identity [--redact] [--json] [--out <file>] [--pipe <path>] [--bench <n>] [--no-probes]

  --redact       replace logins, names, IPs, keys, tags and DERP regions with placeholders and
                 refuse to emit anything that still contains one
  --json         print the JSON report instead of the text summary
  --out <file>   also write the JSON report to <file>; requires --redact
  --pipe <path>  LocalAPI pipe (default: the Tailscale pipe under ProtectedPrefix)
  --bench <n>    number of timed whois calls over the pipe (default 20, 0 to skip)
  --no-probes    skip the variant, negative and header probes
";

fn main() -> ExitCode {
    let mut opts = report::Options {
        redact: false,
        pipe_path: localapi::PIPE_PATH.to_string(),
        bench: 20,
        probes: true,
    };
    let mut json = false;
    let mut out: Option<PathBuf> = None;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--redact" => opts.redact = true,
            "--json" => json = true,
            "--no-probes" => opts.probes = false,
            "--out" => out = args.next().map(PathBuf::from),
            "--pipe" => match args.next() {
                Some(p) => opts.pipe_path = p,
                None => return usage_error("--pipe needs a value"),
            },
            "--bench" => match args.next().and_then(|v| v.parse().ok()) {
                Some(n) => opts.bench = n,
                None => return usage_error("--bench needs a number"),
            },
            "-h" | "--help" => {
                println!("{USAGE}");
                return ExitCode::SUCCESS;
            }
            other => return usage_error(&format!("unknown argument `{other}`")),
        }
    }
    if out.is_some() && !opts.redact {
        return usage_error("--out writes to disk and therefore requires --redact");
    }

    let outcome = match report::run(&opts) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("spike-identity: {e:#}");
            return ExitCode::from(1);
        }
    };
    if let Some(path) = out {
        let text = match serde_json::to_string_pretty(&outcome.json) {
            Ok(t) => t + "\n",
            Err(e) => {
                eprintln!("spike-identity: {e}");
                return ExitCode::from(1);
            }
        };
        if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty())
            && let Err(e) = std::fs::create_dir_all(dir)
        {
            eprintln!("spike-identity: create {}: {e}", dir.display());
            return ExitCode::from(1);
        }
        if let Err(e) = std::fs::write(&path, text) {
            eprintln!("spike-identity: write {}: {e}", path.display());
            return ExitCode::from(1);
        }
        eprintln!("wrote redacted report to {}", path.display());
    }
    if json {
        match serde_json::to_string_pretty(&outcome.json) {
            Ok(t) => println!("{t}"),
            Err(e) => eprintln!("spike-identity: {e}"),
        }
    } else {
        println!("{}", outcome.summary);
    }
    if outcome.status_parsed && outcome.peers_classified >= 1 {
        ExitCode::SUCCESS
    } else {
        eprintln!("spike-identity: self not parsed or no peer could be classified");
        ExitCode::from(1)
    }
}

fn usage_error(msg: &str) -> ExitCode {
    eprintln!("spike-identity: {msg}\n\n{USAGE}");
    ExitCode::from(2)
}
