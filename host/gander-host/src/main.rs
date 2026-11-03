// SPDX-License-Identifier: Apache-2.0
//! The Gander host: a user-session tray application that serves shared folders over the private
//! network. WP-12 provides the listener; the tray and settings UI (`gander_host::ui`, WP-18) run on
//! the main thread when the `ui` feature is on (the default) and the host is not in `--no-ui` or
//! `--test-approve` mode.
//!
//! The binary loads `config.toml` and `devices.toml` (`%APPDATA%\Gander`), the host
//! key (`%LOCALAPPDATA%\Gander\host.key`), writes the audit log to
//! `%LOCALAPPDATA%/Gander/logs/audit.jsonl`, serves `GET /v1/host`, the pairing endpoints,
//! (Windows) the Browse/Fetch/preview API and the `/v1/events` WebSocket (WP-17), all assembled
//! by `gander_host::api::host_routes` (thumbnail and preview caches under
//! `%LOCALAPPDATA%/Gander/cache`) with the token authenticator, and runs until Quit in the tray
//! menu (or Ctrl+C without the UI).
//!
//! Options:
//!
//! - `--config-dir <dir>` / `--data-dir <dir>`: override `%APPDATA%\Gander` /
//!   `%LOCALAPPDATA%\Gander` (integration harnesses use temp directories).
//! - `--port <n>`: with `--test-approve` only, the loopback port (default 0 = any free port).
//! - `--test-approve`: **integration-test hook** (WP-1B, WP-22, WP-29). Accepted only when the
//!   binary is built with `--features test-hooks`; it then binds 127.0.0.1 ONLY (no
//!   private-network address, whatever `config.toml` says), refuses to start if anything else is
//!   bound, prints `TEST HOOK ENABLED`, approves every loopback pairing request without a UI (see
//!   `PairingService::enable_test_approve`), and also accepts `Host: 10.0.2.2[:port]` on its
//!   loopback socket (the Android emulator's alias for the host loopback). It prints
//!   `paired device <id>` for every approved device and reads commands from stdin:
//!   `revoke <device-id>` revokes a device (answers `revoked <id>` or `revoke failed <id>`);
//!   `lock` / `unlock` simulate a session lock for Stream mode (ORed with the real lock state, so
//!   they can only add a lock; answers `locked` / `unlocked`); `attended on|off|real` forces
//!   `host.state attended` (default `off` on the test host, so no prompt waits for a person;
//!   answers `attended <value>`); `kill` presses the kill switch exactly as the tray does (ends
//!   every stream session, closes the pairing window, drops every connection, audits
//!   `kill_switch`; answers `killed`, WP-57).
//! - `--test-stream-app <exe>` (`--test-approve` only): adds an approved, in-memory catalogue
//!   entry `e2e-testwin` for the made-up extension `.gtw` that starts `<exe>` (which must be
//!   `gander-testwin`) as `--self-test-secs 120 --no-focus --title {file}`, and restricts every
//!   Stream launch of this host to that executable.
//! - `--test-grant <share-id>=<perm>[,<perm>...]` (repeatable, `--test-approve` only): every device
//!   the test hook approves is granted these permissions (`list`, `preview`, `fetch`, `stream`) on
//!   that share, in memory only (`config.toml` is not rewritten). The grant is applied right after
//!   approval and announced with `shares.updated`; a client polls `/v1/shares` until the share is
//!   listed.
//! - `--no-ui`: run headless (no tray icon, no windows) until Ctrl+C. Implied by `--test-approve`.
//! - `--ui-smoke [--ui-out <dir>] [--ui-no-native]`: the UI smoke test (WP-18); starts no server and
//!   touches no real configuration; see `gander_host::ui::smoke`.
//!
//! - `--open`: opens the Gander (settings) window as soon as the tray starts, instead of waiting in the
//!   tray. The Start menu entry uses it. Needs the tray UI (refused with `--no-ui`, `--test-approve` or
//!   a build without the `ui` feature).
//!
//! - `--autostart on|off|status`: turns the sign-in Task Scheduler task on or off, or reports it,
//!   as the current user (no elevation), then exits. The MSI runs it at install and uninstall.
//!
//! Ready lines on stdout, in this order: `listening on 127.0.0.1:<port>` (one per socket),
//! `host id <id>`, and in test mode `TEST HOOK ENABLED: ...`.
//!
//! The release build (with the `ui` feature, without `test-hooks`) is a Windows GUI-subsystem
//! program: no console window opens when Task Scheduler or the Start menu starts it. Its output
//! goes to the parent console when launched from a terminal, stays on redirected pipes and files
//! (the UI smoke test and the harnesses), and otherwise goes to `%LOCALAPPDATA%/Gander/logs/host.log`
//! (see `gander_host::diagnostics`). Debug builds and the test-hooks build keep a console.

// A GUI-subsystem release build for the tray application. Headless (`--no-default-features`) and
// test-hooks builds are command-line tools and keep their console.
#![cfg_attr(
    all(
        windows,
        not(debug_assertions),
        feature = "ui",
        not(feature = "test-hooks")
    ),
    windows_subsystem = "windows"
)]

use std::net::{IpAddr, Ipv4Addr};
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context as _, bail};
use gander_core::config::{ConfigFile as _, HostConfig};
use gander_core::net::plan::ListenerPlan;
use gander_core::protocol::ShareId;
use gander_core::protocol::files::Permission;
use gander_core::protocol::host::TrustAdapterKind;
use gander_host::audit::AuditLog;
use gander_host::events::{ConfigShares, Events};
use gander_host::pairing::{DeviceRegistry, PairingService, SharedStore, host_details};
use gander_host::server::gate::ANDROID_EMULATOR_HOST;
use gander_host::server::{
    FixedPlan, HostIdentity, PlanSource, Server, ServerOptions, SystemPlan, selfcheck,
};
#[cfg(feature = "ui")]
use gander_host::server::{ListenerPause, PausablePlan};

/// Tokio worker threads of the host runtime.
const RUNTIME_WORKERS: usize = 2;

fn dir_from_env(var: &str) -> anyhow::Result<PathBuf> {
    let base = std::env::var_os(var).with_context(|| format!("{var} is not set"))?;
    Ok(PathBuf::from(base).join("Gander"))
}

#[cfg(windows)]
fn store() -> SharedStore {
    Arc::new(gander_win::config_store::WindowsConfigStore)
}

#[cfg(not(windows))]
fn store() -> SharedStore {
    // Non-Windows hosts are not built yet; refuse rather than write files without an ACL.
    unimplemented!("owner-only config store for this platform")
}

/// `apps.toml`; a broken file streams nothing rather than stopping the host.
#[cfg(windows)]
fn load_catalogue(
    store: &dyn gander_core::config::SecureFileStore,
    dir: &std::path::Path,
) -> gander_core::config::AppCatalogue {
    gander_core::config::AppCatalogue::load(store, dir).unwrap_or_else(|e| {
        eprintln!("apps.toml ignored: {e}");
        Default::default()
    })
}

/// The test-only catalogue entry (`--test-stream-app`).
#[cfg(windows)]
const TEST_STREAM_APP: &str = "e2e-testwin";

/// The `--test-stream-app` catalogue entry: approved for `.gtw`, closes what it launched.
#[cfg(windows)]
fn test_stream_entry(exe: &std::path::Path) -> gander_core::config::AppEntry {
    use gander_core::config::{AppEntry, CloseOnEnd, LaunchSpec, WindowMatch};
    AppEntry {
        id: gander_core::protocol::AppId::from(TEST_STREAM_APP.to_owned()),
        name: "Gander test window".to_owned(),
        exe: exe.to_path_buf(),
        publisher: None,
        extensions: vec!["gtw".to_owned()],
        approved_by_owner: Some(time::OffsetDateTime::UNIX_EPOCH),
        launch: LaunchSpec::Template(
            "{exe} --self-test-secs 120 --no-focus --title {file}".to_owned(),
        ),
        window_match: WindowMatch::NewWindowOfProcess,
        close_on_end: CloseOnEnd::IfLaunchedByUs,
        slow_start: false,
        package_family_name: None,
    }
}

/// `exe` canonicalised, without the `\\?\` prefix (the form the policy compares).
#[cfg(windows)]
fn plain_canonical(exe: &std::path::Path) -> anyhow::Result<PathBuf> {
    let p = std::fs::canonicalize(exe)
        .with_context(|| format!("--test-stream-app {}", exe.display()))?;
    let s = p.to_string_lossy();
    Ok(PathBuf::from(
        s.strip_prefix(r"\\?\").unwrap_or(&s).to_owned(),
    ))
}

/// What the Browse/Fetch/preview API is built from.
#[cfg(windows)]
struct ApiParts {
    config: HostConfig,
    catalogue: gander_core::config::AppCatalogue,
    audit: Arc<AuditLog>,
    events: Events,
    shares: Arc<ConfigShares>,
    details: gander_core::protocol::host::HostDetails,
}

/// The Browse/Fetch/preview API state with the render services (each optional: a host whose
/// cache or worker is unavailable still browses and fetches) and the events channel, and the
/// caches that opened (the tray's Settings shows their size and can purge them). The cap of each
/// cache is half of `[cache] cap_mib` (default 2048 MiB in total, 1 GiB each); a change applies
/// at the next start.
#[cfg(windows)]
fn build_api(
    parts: ApiParts,
    pairing: &Arc<PairingService>,
    data_dir: &std::path::Path,
) -> (
    Arc<gander_host::api::ApiState>,
    Vec<Arc<gander_win::thumbs::cache::ThumbCache>>,
) {
    use gander_core::protocol::host::PreviewsUnavailable;
    use gander_win::preview::office::OfficeEnv;
    use gander_win::preview::service::PreviewService;
    use gander_win::render::worker::WorkerConfig;
    use gander_win::thumbs::cache::ThumbCache;
    use gander_win::thumbs::service::ThumbService;

    let cache_root = data_dir.join("cache");
    let cache_cap = parts.config.cache.per_cache_bytes();
    let mut caches = Vec::new();
    let mut api = gander_host::api::ApiState::new(
        parts.config,
        Arc::clone(pairing.registry()),
        parts.audit,
        parts.details,
    )
    .with_catalogue(parts.catalogue)
    .with_events(parts.events, parts.shares);
    // The path is always formed; whether the worker is there is a separate question. Without
    // this check a host started by a bare `cargo run -p gander-host` (which does not build
    // gander-render) advertised previews and failed every one of them.
    let worker = WorkerConfig::beside_current_exe().and_then(|w| {
        if w.exe.is_file() {
            Ok(w)
        } else {
            Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("{} does not exist", w.exe.display()),
            ))
        }
    });
    match worker {
        Ok(worker) => {
            match ThumbCache::open(&cache_root, cache_cap) {
                Ok(cache) => {
                    let cache = Arc::new(cache);
                    caches.push(Arc::clone(&cache));
                    api =
                        api.with_thumbs(Arc::new(ThumbService::with_worker(cache, worker.clone())));
                }
                Err(e) => {
                    eprintln!("WARNING: thumbnails unavailable: {e}");
                    tracing::warn!(error = %e, "thumbnail cache unavailable");
                    api = api.with_previews_unavailable(PreviewsUnavailable::CacheUnavailable);
                }
            }
            match ThumbCache::open_in(&cache_root, "previews", cache_cap) {
                Ok(cache) => {
                    let cache = Arc::new(cache);
                    caches.push(Arc::clone(&cache));
                    api = api.with_previews(Arc::new(PreviewService::with_worker(
                        cache,
                        worker,
                        OfficeEnv::detect(),
                    )));
                }
                Err(e) => {
                    eprintln!("WARNING: previews unavailable: {e}");
                    tracing::warn!(error = %e, "preview cache unavailable");
                    api = api.with_previews_unavailable(PreviewsUnavailable::CacheUnavailable);
                }
            }
        }
        Err(e) => {
            let why = PreviewsUnavailable::RenderWorkerMissing;
            eprintln!("WARNING: render worker unavailable ({e})");
            eprintln!("WARNING: {}", why.describe());
            tracing::warn!(error = %e, "render worker missing: previews and thumbnails are off");
            api = api.with_previews_unavailable(why);
        }
    }
    (Arc::new(api), caches)
}

/// The test host's overrides of the computer's state (`lock`, `unlock`, `attended` on stdin)
/// and its kill switch (`kill`).
#[derive(Default, Clone)]
struct TestOverrides {
    probe: Option<Arc<gander_host::events::OverrideProbe>>,
    lock: Option<Arc<gander_host::stream::OverrideLock>>,
    kill: Option<Arc<dyn Fn() + Send + Sync>>,
}

/// The kill switch as the tray's "Stop everything" runs it (`ui::live`): every stream session
/// ends, the pairing window closes, every connection drops, and the audit log says so.
#[cfg(windows)]
fn kill_switch(
    streams: Option<gander_host::lifecycle::StreamSessions>,
    pairing: Arc<PairingService>,
    server: gander_host::server::ServerHandle,
    audit: Arc<AuditLog>,
) -> Arc<dyn Fn() + Send + Sync> {
    Arc::new(move || {
        if let Some(s) = &streams {
            s.kill_all();
        }
        pairing.close_window();
        server.disconnect_all();
        audit.record(gander_host::audit::AuditEvent::new(
            gander_host::audit::kind::KILL_SWITCH,
        ));
    })
}

/// The events channel over the real computer: session lock, input and power state, and
/// `ReadDirectoryChangesW` watches on the configured shares. Also returns the share view the
/// API replaces when the configuration changes. The test host's state probe takes overrides
/// (a simulated lock, a forced attended state; idle by default).
#[cfg(windows)]
fn build_events(
    registry: Arc<DeviceRegistry>,
    config: &HostConfig,
    test_hooks: bool,
) -> anyhow::Result<(Events, Arc<ConfigShares>, TestOverrides)> {
    use gander_host::events::system::{
        WindowsWatch, host_state_monitor, overridable_host_state_monitor,
    };
    use gander_host::events::{EventsOptions, HostStateConfig};

    let shares = ConfigShares::new(config.clone());
    let state_config = HostStateConfig::from_stream_config(&config.stream);
    let mut overrides = TestOverrides::default();
    let host = if test_hooks {
        let (monitor, probe) = overridable_host_state_monitor(&state_config)?;
        probe.set_attended(Some(false));
        monitor.refresh_now();
        overrides.probe = Some(probe);
        monitor
    } else {
        host_state_monitor(&state_config)?
    };
    let events = Events::new(EventsOptions {
        registry,
        shares: shares.clone(),
        host,
        backend: Arc::new(WindowsWatch::new(Arc::clone(&shares))?),
        config: Default::default(),
    });
    Ok((events, shares, overrides))
}

/// Stream mode (WP-50..53): the engine on the listener's addresses, the session lifecycle with
/// the Windows launch/input platform, keep-awake and catalogue approvals, installed on the API
/// and the events channel. `None` (Stream stays `stream_unavailable`) when the session-lock
/// watcher cannot start: without it nothing could stop a stream on lock.
#[cfg(windows)]
#[allow(clippy::too_many_arguments)]
fn build_streams(
    config: &HostConfig,
    events: &Events,
    server: gander_host::server::ServerHandle,
    api: &Arc<gander_host::api::ApiState>,
    audit: Arc<AuditLog>,
    store: SharedStore,
    config_dir: PathBuf,
    data_dir: PathBuf,
    test_app: Option<PathBuf>,
    clipboard: &gander_host::clipboard::HostClipboard,
    overrides: &mut TestOverrides,
) -> Option<gander_host::lifecycle::StreamSessions> {
    use gander_core::policy::{DecisionReason, PolicyDecision, approve_extension, approve_handler};
    use gander_host::lifecycle::windows::{SystemAwake, WindowsPlatform};
    use gander_host::lifecycle::{
        LifecycleSignalling, SessionDeps, SessionsConfig, StreamSessions,
    };
    use gander_host::stream::{EventsSink, OverrideLock, windows_engine};
    use gander_stream::LockSource;

    let real_lock = match gander_stream::win::SessionLock::start() {
        Ok(l) => Arc::new(l),
        Err(e) => {
            eprintln!("Stream mode off: the session-lock watcher did not start ({e})");
            return None;
        }
    };
    let real_probe = gander_win::input::query_lock_probe();
    let (lock, lock_probe): (Arc<dyn LockSource>, gander_win::input::LockProbe) =
        if overrides.probe.is_some() {
            let o = Arc::new(OverrideLock::new(real_lock));
            overrides.lock = Some(Arc::clone(&o));
            let sim = Arc::clone(&o);
            let probe: gander_win::input::LockProbe =
                Arc::new(move || sim.simulated() || real_probe());
            (o, probe)
        } else {
            (real_lock, real_probe)
        };
    let engine = windows_engine(config, events, server, Arc::clone(&lock));
    let names_registry = api.registry();
    let approve_api = Arc::clone(api);
    let approve: gander_host::lifecycle::ApproveAlways =
        Arc::new(move |d: &PolicyDecision| -> Result<(), String> {
            let mut catalogue = (*approve_api.current_catalogue()).clone();
            let ext = d.extension.clone().ok_or("no extension")?;
            match (&d.app, d.reason) {
                (Some(app), DecisionReason::NewExtensionForApprovedApp) => {
                    approve_extension(&mut catalogue, &app.id, &ext).map_err(|e| e.to_string())?;
                }
                _ => {
                    let h = d.handler.as_ref().ok_or("no handler")?;
                    approve_handler(
                        &mut catalogue,
                        approve_api.environment(),
                        h,
                        &h.exe_name(),
                        &ext,
                        time::OffsetDateTime::now_utc(),
                    )
                    .map_err(|e| e.to_string())?;
                }
            }
            gander_core::config::ConfigFile::save(&catalogue, &*store, &config_dir)
                .map_err(|e| e.to_string())?;
            approve_api.set_catalogue(catalogue);
            Ok(())
        });
    let attended_events = events.clone();
    let names: gander_host::lifecycle::DeviceNames = Arc::new(move |id| {
        names_registry
            .get(id)
            .map_or_else(|| id.to_string(), |r| r.name)
    });
    // The clipboard text bridge (WP-56): the owner's live setting, the Windows clipboard, and an
    // observer that audits every request and queues the "copied text" notices for the tray.
    let clipboard_service = gander_core::clipboard::ClipboardService::new(
        clipboard.setting().clone(),
        Arc::new(gander_win::clipboard::SystemClipboard::default()),
        clipboard.observer(Arc::clone(&audit), Arc::clone(&names)),
    );
    let sessions = StreamSessions::new(
        SessionsConfig {
            idle_default: config.stream.idle_prompt_default,
            clipboard: Some(clipboard.setting().clone()),
            ..SessionsConfig::default()
        },
        SessionDeps {
            engine,
            platform: Arc::new(WindowsPlatform {
                data_dir,
                lock_probe,
                only_exe: test_app,
                clipboard: Some(clipboard_service),
            }),
            lock,
            attended: Arc::new(move || attended_events.host_state().attended),
            signal: Arc::new(EventsSink(events.clone())),
            audit,
            awake: Arc::new(SystemAwake),
            names,
            approve_always: Some(approve),
        },
    );
    events.set_stream_signalling(Arc::new(LifecycleSignalling(sessions.clone())));
    sessions.watch_revocations(&api.registry());
    api.set_streams(Some(sessions.clone()));
    events.host_monitor().set_stream_engine(true);
    Some(sessions)
}

#[cfg(not(windows))]
fn build_events(
    _registry: Arc<DeviceRegistry>,
    _config: &HostConfig,
    _test_hooks: bool,
) -> anyhow::Result<(Events, Arc<ConfigShares>, TestOverrides)> {
    // Non-Windows hosts are not built yet (no session, input or directory-watch backend).
    unimplemented!("events backends for this platform")
}

#[derive(Debug, Default)]
struct Args {
    config_dir: Option<PathBuf>,
    data_dir: Option<PathBuf>,
    port: u16,
    test_approve: bool,
    test_grants: Vec<(ShareId, Vec<Permission>)>,
    test_stream_app: Option<PathBuf>,
    no_ui: bool,
    ui_smoke: bool,
    ui_out: Option<PathBuf>,
    ui_no_native: bool,
    autostart: Option<AutostartCommand>,
    open: bool,
}

/// What `--autostart` asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AutostartCommand {
    On,
    Off,
    Status,
}

impl Args {
    /// Whether the tray and windows run: the `ui` feature is on and nothing asked for headless.
    #[cfg_attr(not(feature = "ui"), allow(dead_code))]
    fn wants_ui(&self) -> bool {
        cfg!(feature = "ui") && !self.no_ui && !self.test_approve
    }
}

/// `<share-id>=<perm>[,<perm>...]`.
fn parse_grant(spec: &str) -> anyhow::Result<(ShareId, Vec<Permission>)> {
    let (share, perms) = spec
        .split_once('=')
        .context("--test-grant needs <share-id>=<perm>[,<perm>...]")?;
    if share.is_empty() {
        bail!("--test-grant: empty share id");
    }
    let perms = perms
        .split(',')
        .map(|p| {
            serde_json::from_value::<Permission>(serde_json::Value::String(p.trim().to_owned()))
                .with_context(|| format!("--test-grant: unknown permission {p:?}"))
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    Ok((ShareId::from(share), perms))
}

fn parse_args_from(argv: impl IntoIterator<Item = String>) -> anyhow::Result<Args> {
    let mut args = Args::default();
    let mut it = argv.into_iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--config-dir" => {
                args.config_dir = Some(it.next().context("--config-dir needs a value")?.into())
            }
            "--data-dir" => {
                args.data_dir = Some(it.next().context("--data-dir needs a value")?.into())
            }
            "--port" => args.port = it.next().context("--port needs a value")?.parse()?,
            "--test-approve" => args.test_approve = true,
            "--test-grant" => args.test_grants.push(parse_grant(
                &it.next().context("--test-grant needs a value")?,
            )?),
            "--test-stream-app" => {
                args.test_stream_app =
                    Some(it.next().context("--test-stream-app needs a value")?.into())
            }
            "--no-ui" => args.no_ui = true,
            "--ui-smoke" => args.ui_smoke = true,
            "--ui-out" => args.ui_out = Some(it.next().context("--ui-out needs a value")?.into()),
            "--ui-no-native" => args.ui_no_native = true,
            "--open" => args.open = true,
            "--autostart" => {
                args.autostart = Some(
                    match it
                        .next()
                        .context("--autostart needs on, off or status")?
                        .as_str()
                    {
                        "on" => AutostartCommand::On,
                        "off" => AutostartCommand::Off,
                        "status" => AutostartCommand::Status,
                        other => bail!("--autostart takes on, off or status, not {other}"),
                    },
                )
            }
            other => bail!("unknown argument {other}"),
        }
    }
    if args.test_approve && !cfg!(feature = "test-hooks") {
        bail!("--test-approve needs a build with `--features test-hooks`; refusing");
    }
    if !args.test_grants.is_empty() && !args.test_approve {
        bail!("--test-grant is only accepted together with --test-approve");
    }
    if args.test_stream_app.is_some() && !args.test_approve {
        bail!("--test-stream-app is only accepted together with --test-approve");
    }
    if (args.ui_smoke || args.ui_out.is_some() || args.ui_no_native) && !cfg!(feature = "ui") {
        bail!("the UI smoke test needs a build with the `ui` feature; refusing");
    }
    if args.autostart.is_some() && !cfg!(feature = "ui") {
        bail!("--autostart needs a build with the `ui` feature; refusing");
    }
    if args.open && !args.wants_ui() {
        bail!(
            "--open needs the tray and windows: not with --no-ui, --test-approve or a build without the `ui` feature"
        );
    }
    if !args.ui_smoke && (args.ui_out.is_some() || args.ui_no_native) {
        bail!("--ui-out and --ui-no-native only go with --ui-smoke");
    }
    Ok(args)
}

fn parse_args() -> anyhow::Result<Args> {
    parse_args_from(std::env::args().skip(1))
}

/// Test hook: grants `grants` to `device` in `config` (in memory). Unknown shares are ignored.
#[cfg_attr(not(windows), allow(dead_code))]
fn with_test_grants(
    mut config: HostConfig,
    device: &gander_core::protocol::DeviceId,
    grants: &[(ShareId, Vec<Permission>)],
) -> HostConfig {
    for (share, perms) in grants {
        if let Some(s) = config.shares.iter_mut().find(|s| &s.id == share) {
            s.grants.retain(|g| &g.device != device);
            s.grants.push(gander_core::config::ShareGrant {
                device: device.clone(),
                permissions: perms.clone(),
            });
        }
    }
    config
}

/// Test hook: prints approvals and applies `--test-grant` to every approved device.
#[cfg(windows)]
fn spawn_test_grants(
    pairing: &Arc<PairingService>,
    api: &Arc<gander_host::api::ApiState>,
    grants: Vec<(ShareId, Vec<Permission>)>,
) {
    use gander_host::pairing::PairingEvent;
    use tokio::sync::broadcast::error::RecvError;

    let mut rx = pairing.subscribe();
    let api = Arc::clone(api);
    tokio::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(PairingEvent::Approved(device)) => {
                    if !grants.is_empty() {
                        let config =
                            with_test_grants((*api.current_config()).clone(), &device, &grants);
                        api.set_config(config);
                    }
                    println!("paired device {device}");
                }
                Ok(_) | Err(RecvError::Lagged(_)) => {}
                Err(RecvError::Closed) => break,
            }
        }
    });
}

/// Test hook: `revoke <device-id>`, `lock`, `unlock`, `attended on|off|real`, `kill` and `qr <file>` lines
/// on stdin. `qr` opens the pairing window and writes its flow A URI (secret included) to `<file>`
/// (the rest of the line)
/// for a QR pairing test; the URI is never printed. EOF just stops reading.
fn spawn_stdin_commands(pairing: Arc<PairingService>, events: Events, overrides: TestOverrides) {
    let runtime = tokio::runtime::Handle::current();
    let registry = Arc::clone(pairing.registry());
    std::thread::spawn(move || {
        use std::io::BufRead as _;
        let set_lock = |locked: bool| {
            if let Some(p) = &overrides.probe {
                p.set_locked(locked);
            }
            if let Some(l) = &overrides.lock {
                l.set_simulated(locked);
            }
            events.host_monitor().refresh_now();
        };
        for line in std::io::stdin().lock().lines() {
            let Ok(line) = line else { break };
            let mut words = line.split_whitespace();
            match (words.next(), words.next()) {
                (Some("revoke"), Some(id)) => {
                    let id = gander_core::protocol::DeviceId::from(id);
                    match registry.revoke(&id) {
                        Ok(_) => println!("revoked {id}"),
                        Err(e) => println!("revoke failed {id}: {e}"),
                    }
                }
                (Some("lock"), None) => {
                    set_lock(true);
                    println!("locked");
                }
                (Some("unlock"), None) => {
                    set_lock(false);
                    println!("unlocked");
                }
                (Some("kill"), None) => match &overrides.kill {
                    Some(kill) => {
                        let _rt = runtime.enter();
                        kill();
                        println!("killed");
                    }
                    None => println!("kill unavailable"),
                },
                (Some("attended"), Some(v @ ("on" | "off" | "real"))) => {
                    if let Some(p) = &overrides.probe {
                        p.set_attended(match v {
                            "on" => Some(true),
                            "off" => Some(false),
                            _ => None,
                        });
                    }
                    events.host_monitor().refresh_now();
                    println!("attended {v}");
                }
                (Some("qr"), Some(_)) => {
                    // The rest of the line, so a path with spaces works.
                    let path = line.trim_start()[2..].trim();
                    let uri = {
                        let _rt = runtime.enter();
                        let mut code = pairing.open_window();
                        // The test host listens on loopback only and has no tailnet address.
                        if code.addresses.is_empty() {
                            code.addresses.push(std::net::Ipv4Addr::LOCALHOST.into());
                        }
                        code.to_uri()
                    };
                    match std::fs::write(path, uri) {
                        Ok(()) => println!("qr written"),
                        Err(e) => println!("qr failed: {e}"),
                    }
                }
                (None, _) => {}
                _ => println!("unknown command"),
            }
        }
    });
}

/// The data directory the log goes under, read from the raw arguments before they are parsed (so
/// a bad argument is logged too): `--data-dir <dir>`, else `<LOCALAPPDATA>\Gander`.
fn early_data_dir(
    argv: impl IntoIterator<Item = String>,
    local_app_data: Option<std::ffi::OsString>,
) -> PathBuf {
    let mut it = argv.into_iter();
    while let Some(a) = it.next() {
        if a == "--data-dir"
            && let Some(dir) = it.next()
        {
            return dir.into();
        }
    }
    local_app_data.map_or_else(
        || std::env::temp_dir().join("Gander"),
        |base| PathBuf::from(base).join("Gander"),
    )
}

/// `--autostart on|off|status`: no server, no configuration, no window.
#[cfg(feature = "ui")]
fn run_autostart(command: AutostartCommand) -> anyhow::Result<()> {
    let autostart = gander_host::ui::system_autostart();
    match command {
        AutostartCommand::On => autostart
            .set_enabled(true)
            .map_err(|e| anyhow::anyhow!("{e}"))?,
        AutostartCommand::Off => autostart
            .set_enabled(false)
            .map_err(|e| anyhow::anyhow!("{e}"))?,
        AutostartCommand::Status => {}
    }
    let status = autostart.status();
    println!("autostart: {}", if status.enabled { "on" } else { "off" });
    if let Some(note) = status.note {
        println!("note: {note}");
    }
    Ok(())
}

fn main() -> anyhow::Result<()> {
    let output = gander_host::diagnostics::init(&gander_host::diagnostics::log_dir(
        &early_data_dir(std::env::args().skip(1), std::env::var_os("LOCALAPPDATA")),
    ));
    if matches!(output, gander_host::diagnostics::Output::LogFile(_)) {
        println!(
            "--- gander-host {} started (pid {}) ---",
            env!("CARGO_PKG_VERSION"),
            std::process::id()
        );
    }
    let args = parse_args()?;
    #[cfg(feature = "ui")]
    if let Some(command) = args.autostart {
        return run_autostart(command);
    }
    #[cfg(feature = "ui")]
    if args.ui_smoke {
        // The UI smoke test starts no server and reads no real configuration.
        let options = gander_host::ui::smoke::Options {
            out_dir: args
                .ui_out
                .clone()
                .unwrap_or_else(|| gander_host::ui::smoke::DEFAULT_OUT.into()),
            native: !args.ui_no_native,
        };
        return gander_host::ui::smoke::run(&options);
    }
    // Two workers, not one per logical CPU (24 on the dev laptop): the host serves one owner's
    // few devices, and renders, file reads and the stream pipeline run on their own threads or
    // the blocking pool. Measured in docs/specs/2026-11-02-host-footprint.md.
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(RUNTIME_WORKERS)
        .thread_name("gander-rt")
        .enable_all()
        .build()?;
    runtime.block_on(run(args))
}

async fn run(args: Args) -> anyhow::Result<()> {
    // Decided before `args` is taken apart below.
    #[cfg_attr(not(feature = "ui"), allow(unused_variables))]
    let wants_ui = args.wants_ui();
    #[cfg_attr(not(feature = "ui"), allow(unused_variables))]
    let open_settings = args.open;
    let config_dir = match args.config_dir {
        Some(d) => d,
        None => dir_from_env("APPDATA")?,
    };
    let data_dir = match args.data_dir {
        Some(d) => d,
        None => dir_from_env("LOCALAPPDATA")?,
    };
    // One host per data directory: a second start (the Start menu entry while the tray
    // application runs) asks the first to open its window and ends. Held until the host exits.
    #[cfg(feature = "ui")]
    let _instance = if wants_ui {
        match gander_host::single_instance::acquire(&data_dir) {
            gander_host::single_instance::Instance::First(guard) => {
                guard.on_open(gander_host::ui::request_open_window);
                Some(guard)
            }
            gander_host::single_instance::Instance::Running => {
                let asked = open_settings && gander_host::single_instance::signal_open(&data_dir);
                println!(
                    "Gander is already running{}",
                    if asked {
                        "; its window was asked to open"
                    } else {
                        ""
                    }
                );
                return Ok(());
            }
        }
    } else {
        None
    };
    let store = store();
    let config = HostConfig::load(&*store, &config_dir)?;
    #[cfg(windows)]
    let catalogue = load_catalogue(&*store, &config_dir);
    let identity = HostIdentity::load_or_create(&data_dir)?;
    let audit = Arc::new(AuditLog::open(&data_dir.join("logs"))?);
    #[cfg(windows)]
    let kill_audit = Arc::clone(&audit);
    let registry = Arc::new(DeviceRegistry::load(
        Arc::clone(&store),
        &config_dir,
        config.trust.clone(),
        Arc::clone(&audit),
    )?);
    let pairing = PairingService::new(
        &identity,
        registry,
        Arc::clone(&audit),
        host_details(identity.host_id().clone(), Vec::new(), None),
        config.listener.port,
    );
    #[cfg_attr(not(windows), allow(unused_mut))]
    let (events, shares, mut overrides) =
        build_events(Arc::clone(pairing.registry()), &config, args.test_approve)?;
    #[cfg(windows)]
    let stream_audit = Arc::clone(&audit);
    #[cfg(windows)]
    let mut catalogue = catalogue;
    #[cfg(windows)]
    let test_app = match &args.test_stream_app {
        Some(exe) => {
            let exe = plain_canonical(exe)?;
            catalogue.apps.retain(|a| a.id.as_str() != TEST_STREAM_APP);
            catalogue.apps.push(test_stream_entry(&exe));
            Some(exe)
        }
        None => None,
    };
    #[cfg(windows)]
    #[cfg_attr(not(feature = "ui"), allow(unused_variables))]
    let (api, caches) = build_api(
        ApiParts {
            config: config.clone(),
            catalogue,
            audit,
            events: events.clone(),
            shares,
            details: host_details(identity.host_id().clone(), Vec::new(), None),
        },
        &pairing,
        &data_dir,
    );
    #[cfg(windows)]
    let routes = gander_host::api::host_routes(identity.host_id().clone(), &pairing, &api);
    #[cfg(not(windows))]
    let routes = {
        drop(shares);
        gander_host::server::hello::router(identity.host_id().clone(), pairing.pairing_open_flag())
            .merge(pairing.router())
            .merge(gander_host::events::router(&events))
    };
    let plan: Arc<dyn PlanSource> = if args.test_approve {
        let lo = IpAddr::V4(Ipv4Addr::LOCALHOST);
        Arc::new(FixedPlan(
            ListenerPlan::new(TrustAdapterKind::Generic, args.port, vec![lo], vec![])
                .map_err(|e| anyhow::anyhow!("{e:?}"))?,
        ))
    } else {
        Arc::new(SystemPlan::new(config.listener.clone()))
    };
    // The tray's "Pause listener" closes the private-network sockets through this switch.
    #[cfg(feature = "ui")]
    let pause = ListenerPause::new();
    #[cfg(feature = "ui")]
    let plan: Arc<dyn PlanSource> = Arc::new(PausablePlan::new(plan, Arc::clone(&pause)));
    let mut options = ServerOptions::new(identity.clone(), plan);
    options.authenticator = pairing.authenticator();
    if args.test_approve {
        options.watch_address_changes = false;
    } else {
        #[cfg(windows)]
        {
            options.tailscale = Some(Arc::new(gander_win::net::tailscale::system_tailscale()));
        }
    }
    let server = Server::start(options, routes).await?;
    let bound = server.bound_addrs();
    let addresses = server
        .plan()
        .map(|p| p.network().to_vec())
        .unwrap_or_default();
    let port = bound.first().map_or(config.listener.port, |a| a.port());
    #[cfg(windows)]
    {
        api.set_host_details(host_details(
            identity.host_id().clone(),
            addresses.clone(),
            None,
        ));
        api.set_network(server.network());
    }
    pairing.set_host_details(
        host_details(identity.host_id().clone(), addresses, None),
        port,
    );
    // The owner's clipboard setting and the notices for the tray, shared by the stream sessions
    // and the settings window.
    let clipboard = gander_host::clipboard::HostClipboard::new(config.stream.clipboard);
    #[cfg(windows)]
    let streams = build_streams(
        &config,
        &events,
        server.handle(),
        &api,
        stream_audit,
        Arc::clone(&store),
        config_dir.clone(),
        data_dir.clone(),
        test_app,
        &clipboard,
        &mut overrides,
    );
    if args.test_approve {
        if let Err(e) = pairing.enable_test_approve(&bound) {
            server.shutdown().await;
            bail!("{e}");
        }
        // The emulator reaches the host loopback as 10.0.2.2; its requests arrive on 127.0.0.1.
        server
            .host_allow_list()
            .set_loopback_aliases([ANDROID_EMULATOR_HOST]);
        #[cfg(windows)]
        spawn_test_grants(&pairing, &api, args.test_grants.clone());
        #[cfg(windows)]
        {
            overrides.kill = Some(kill_switch(
                streams.clone(),
                Arc::clone(&pairing),
                server.handle(),
                Arc::clone(&kill_audit),
            ));
        }
        spawn_stdin_commands(Arc::clone(&pairing), events.clone(), overrides);
    }
    // Ready lines last, so a harness that read them can rely on everything above.
    for addr in &bound {
        println!("listening on {addr}");
    }
    println!("host id {}", server.host_id());
    if args.test_approve {
        println!("TEST HOOK ENABLED: loopback pairing requests are approved without a UI");
    } else {
        let report = selfcheck::connectivity_self_check(server.network().as_deref()).await;
        if report.has_problems() {
            println!("connectivity self-check: {report:?}");
        }
    }
    // The tray and windows own the main thread until the owner picks Quit; the server keeps
    // running on the runtime's worker threads meanwhile. Without them (or if no window system is
    // available) the host waits for Ctrl+C.
    #[cfg(feature = "ui")]
    let headless = if wants_ui {
        #[cfg(windows)]
        let caches: Option<Arc<dyn gander_host::ui::live::CacheControl>> =
            Some(Arc::new(gander_host::ui::live::ThumbCaches(caches)));
        #[cfg(not(windows))]
        let caches = None;
        let parts = gander_host::ui::LiveParts {
            server: server.handle(),
            pairing: Arc::clone(&pairing),
            store: Arc::clone(&store),
            config_dir: config_dir.clone(),
            config: config.clone(),
            clipboard: clipboard.clone(),
            pause,
            #[cfg(windows)]
            api: Arc::clone(&api),
            caches,
            autostart: gander_host::ui::system_autostart(),
            runtime: tokio::runtime::Handle::current(),
        };
        if open_settings {
            gander_host::ui::request_open_window();
        }
        match gander_host::ui::run_live(parts) {
            Ok(()) => false,
            Err(e) => {
                eprintln!(
                    "the tray and windows could not start ({e}); running headless, Ctrl+C stops it"
                );
                true
            }
        }
    } else {
        true
    };
    #[cfg(not(feature = "ui"))]
    let headless = true;
    if headless {
        tokio::signal::ctrl_c().await?;
    }
    // End a running stream first: its window is restored (and closed if Gander started it).
    #[cfg(windows)]
    if let Some(s) = &streams {
        s.shutdown().await;
    }
    pairing.close_window();
    server.shutdown().await;
    drop(events);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn the_log_goes_under_the_data_dir_argument_or_local_app_data() {
        let with_arg = early_data_dir(argv(&["--no-ui", "--data-dir", "D:/d"]), None);
        assert_eq!(with_arg, PathBuf::from("D:/d"));
        let without = early_data_dir(argv(&["--no-ui"]), Some("C:/Local".into()));
        assert_eq!(without, PathBuf::from("C:/Local").join("Gander"));
        // A trailing --data-dir with no value falls through to the default.
        let dangling = early_data_dir(argv(&["--data-dir"]), Some("C:/Local".into()));
        assert_eq!(dangling, PathBuf::from("C:/Local").join("Gander"));
    }

    #[test]
    fn autostart_takes_on_off_or_status() {
        let ok = |v: &str| parse_args_from(argv(&["--autostart", v]));
        if cfg!(feature = "ui") {
            assert_eq!(ok("on").unwrap().autostart, Some(AutostartCommand::On));
            assert_eq!(ok("off").unwrap().autostart, Some(AutostartCommand::Off));
            assert_eq!(
                ok("status").unwrap().autostart,
                Some(AutostartCommand::Status)
            );
        }
        assert!(ok("maybe").is_err());
        assert!(parse_args_from(argv(&["--autostart"])).is_err());
    }

    #[test]
    fn open_wants_the_tray_and_windows() {
        // The Start menu entry runs `gander-host --open`.
        let r = parse_args_from(argv(&["--open"]));
        assert_eq!(r.is_ok(), cfg!(feature = "ui"));
        if cfg!(feature = "ui") {
            assert!(r.unwrap().open);
        }
        assert!(parse_args_from(argv(&["--open", "--no-ui"])).is_err());
    }

    #[test]
    fn test_grant_needs_test_approve_and_valid_permissions() {
        assert!(parse_args_from(argv(&["--test-grant", "shr_a=list"])).is_err());
        let grant = parse_grant("shr_a=list,fetch").unwrap();
        assert_eq!(grant.0, ShareId::from("shr_a"));
        assert_eq!(grant.1, vec![Permission::List, Permission::Fetch]);
        assert!(parse_grant("shr_a=list,write").is_err());
        assert!(parse_grant("shr_a").is_err());
        assert!(parse_grant("=list").is_err());
    }

    #[test]
    fn test_approve_refused_without_the_feature() {
        let r = parse_args_from(argv(&["--test-approve"]));
        assert_eq!(r.is_ok(), cfg!(feature = "test-hooks"));
    }

    #[test]
    fn test_grants_replace_only_the_named_device() {
        let device = gander_core::protocol::DeviceId::from("dev_new");
        let other = gander_core::protocol::DeviceId::from("dev_other");
        let mut config = HostConfig::default();
        config.shares.push(gander_core::config::ShareConfig {
            id: ShareId::from("shr_a"),
            name: "A".to_owned(),
            root: PathBuf::from("C:/a"),
            show_hidden: false,
            show_path: false,
            hidden_patterns: Vec::new(),
            stream_default: gander_core::protocol::files::StreamDecision::Ask,
            grants: vec![gander_core::config::ShareGrant {
                device: other.clone(),
                permissions: vec![Permission::List],
            }],
        });
        let grants = vec![
            (
                ShareId::from("shr_a"),
                vec![Permission::List, Permission::Fetch],
            ),
            (ShareId::from("shr_missing"), vec![Permission::List]),
        ];
        let out = with_test_grants(config, &device, &grants);
        let share = out.share(&ShareId::from("shr_a")).unwrap();
        assert_eq!(share.permissions_for(&other), &[Permission::List]);
        assert_eq!(
            share.permissions_for(&device),
            &[Permission::List, Permission::Fetch]
        );
        assert_eq!(out.shares.len(), 1);
    }
}
