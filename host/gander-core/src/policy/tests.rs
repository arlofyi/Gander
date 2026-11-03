// SPDX-License-Identifier: Apache-2.0
//! Policy unit tests with a fake association table (spec, "Tests", first bullet).

use std::path::Path;

use super::*;
use crate::config::memory_store::MemoryStore;
use crate::config::{ConfigFile, HostConfig, ShareConfig, ShareGrant};
use crate::protocol::files::Permission;
use crate::protocol::stream::{StreamRequest, Viewport};
use crate::protocol::{DeviceId, ShareId};

const POWERPNT: &str = "C:\\Program Files\\Microsoft Office\\root\\Office16\\POWERPNT.EXE";
const EDGE: &str = "C:\\Program Files (x86)\\Microsoft\\Edge\\Application\\msedge.exe";
const BLENDER: &str = "C:\\Program Files\\Blender Foundation\\Blender 4.2\\blender.exe";

fn ms() -> Signature {
    Signature::Signed {
        publisher: "Microsoft Corporation".to_owned(),
    }
}

fn catalogue() -> AppCatalogue {
    AppCatalogue::from_toml_str(&format!(
        r#"
[[app]]
id = "powerpoint"
name = "Microsoft PowerPoint"
exe = '{POWERPNT}'
publisher = "Microsoft Corporation"
extensions = ["pptx", "ppsx", "potx", "odp"]
approved_by_owner = "2026-11-02T20:11:03Z"

[[app]]
id = "edge"
name = "Microsoft Edge"
exe = '{EDGE}'
publisher = "Microsoft Corporation"
extensions = ["pdf"]
approved_by_owner = "2026-11-02T20:11:03Z"

[[app]]
id = "blender"
name = "Blender"
exe = '{BLENDER}'
publisher = "Blender Foundation"
extensions = ["blend"]
approved_by_owner = "2026-11-02T20:11:03Z"
"#
    ))
    .unwrap()
}

fn associations() -> StaticAssociations {
    StaticAssociations::new()
        // Canonical form as GetFinalPathNameByHandle returns it.
        .with("pptx", HandlerInfo::new(format!("\\\\?\\{POWERPNT}"), ms()))
        .with("ppsm", HandlerInfo::new(POWERPNT, ms()))
        .with("pdf", HandlerInfo::new(EDGE, ms()))
        .with("htm", HandlerInfo::new(EDGE, ms()))
        .with(
            "obj",
            HandlerInfo::new(
                BLENDER,
                Signature::Signed {
                    publisher: "Blender Foundation".to_owned(),
                },
            ),
        )
        .with(
            "txt",
            HandlerInfo::new("C:\\Windows\\System32\\notepad.exe", ms()),
        )
        .with(
            "xyz",
            HandlerInfo::new("C:\\Tools\\viewer.exe", Signature::Unsigned),
        )
}

fn input(file_name: &str) -> PolicyInput<'_> {
    PolicyInput {
        file_name,
        stream_permitted: true,
        share_default: StreamDecision::Ask,
        requested_app: None,
    }
}

fn eval_with(assoc: &StaticAssociations, cat: &AppCatalogue, file: &str) -> PolicyDecision {
    let env = PolicyEnvironment::default();
    StreamPolicy::new(cat, assoc, &env).evaluate(&input(file))
}

fn eval(file: &str) -> PolicyDecision {
    eval_with(&associations(), &catalogue(), file)
}

#[test]
fn every_step1_extension_is_refused() {
    // Even when an approved app claims the extension.
    let mut cat = catalogue();
    let mut assoc = associations();
    for ext in rules::HARD_REFUSED_EXTENSIONS {
        cat.apps[0].extensions.push((*ext).to_owned());
        assoc.insert(ext, HandlerInfo::new(POWERPNT, ms()));
    }
    for ext in rules::HARD_REFUSED_EXTENSIONS {
        for name in [
            format!("file.{ext}"),
            format!("FILE.{}", ext.to_uppercase()),
        ] {
            let d = eval_with(&assoc, &cat, &name);
            assert_eq!(d.outcome, StreamDecision::Refused, "{name}");
            assert_eq!(d.reason, DecisionReason::HardRefusedExtension, "{name}");
            assert_eq!(d.error_body().unwrap().error.code, ErrorCode::StreamRefused);
        }
    }
}

#[test]
fn every_never_list_handler_is_refused_even_for_a_benign_extension() {
    for pattern in rules::NEVER_LIST {
        let exe_name = pattern.replace('*', "3.12");
        let exe = format!("C:\\Tools\\{exe_name}");
        let assoc = StaticAssociations::new().with("pptx", HandlerInfo::new(&exe, ms()));
        // Even an approved catalogue entry for that exe does not help.
        let mut cat = catalogue();
        cat.apps[0].exe = exe.clone().into();
        let d = eval_with(&assoc, &cat, "deck.pptx");
        assert_eq!(d.outcome, StreamDecision::Refused, "{exe_name}");
        assert_eq!(d.reason, DecisionReason::NeverListedHandler, "{exe_name}");
        // Upper-case spelling too.
        let assoc =
            StaticAssociations::new().with("pptx", HandlerInfo::new(exe.to_uppercase(), ms()));
        assert_eq!(
            eval_with(&assoc, &cat, "deck.pptx").outcome,
            StreamDecision::Refused
        );
    }
}

#[test]
fn system32_handlers_are_refused_except_the_allowlist() {
    let assoc = StaticAssociations::new()
        .with(
            "pptx",
            HandlerInfo::new("C:\\WINDOWS\\system32\\dllhost.exe", ms()),
        )
        .with(
            "txt",
            HandlerInfo::new("C:\\Windows\\System32\\notepad.exe", ms()),
        );
    let d = eval_with(&assoc, &catalogue(), "deck.pptx");
    assert_eq!(d.reason, DecisionReason::SystemHandler);
    // notepad is allowlisted: unknown to the catalogue, so ask.
    let d = eval_with(&assoc, &catalogue(), "notes.txt");
    assert_eq!(d.outcome, StreamDecision::Ask);
}

#[test]
fn approved_catalogue_entry_is_allowed() {
    let d = eval("Deck.pptx");
    assert_eq!(d.outcome, StreamDecision::Allowed);
    assert_eq!(d.reason, DecisionReason::CatalogueApproved);
    assert_eq!(d.app.as_ref().unwrap().id.as_str(), "powerpoint");
    assert!(d.browser.is_none());
    assert!(d.error_body().is_none());
}

#[test]
fn deck_upper_case_pptx_is_allowed() {
    assert_eq!(eval("deck.PPTX").outcome, StreamDecision::Allowed);
}

#[test]
fn new_extension_on_approved_app_asks() {
    let d = eval("model.obj");
    assert_eq!(d.outcome, StreamDecision::Ask);
    assert_eq!(d.reason, DecisionReason::NewExtensionForApprovedApp);
    assert_eq!(d.app.unwrap().id.as_str(), "blender");
}

#[test]
fn unknown_handler_asks_or_follows_share_default() {
    let d = eval("thing.xyz");
    assert_eq!(d.outcome, StreamDecision::Ask);
    assert_eq!(d.reason, DecisionReason::UnknownHandler);
    assert_eq!(d.handler.unwrap().signature.display(), "unsigned");

    let assoc = associations();
    let cat = catalogue();
    let env = PolicyEnvironment::default();
    let policy = StreamPolicy::new(&cat, &assoc, &env);
    let mut inp = input("thing.xyz");
    inp.share_default = StreamDecision::Refused;
    assert_eq!(
        policy.evaluate(&inp).reason,
        DecisionReason::ShareDefaultRefused
    );
    inp.share_default = StreamDecision::Allowed;
    assert_eq!(policy.evaluate(&inp).outcome, StreamDecision::Ask);
}

#[test]
fn suggested_entry_is_not_an_approval() {
    let mut cat = catalogue();
    cat.apps[0].approved_by_owner = None;
    assert_eq!(
        eval_with(&associations(), &cat, "deck.pptx").reason,
        DecisionReason::UnknownHandler
    );
}

#[test]
fn publisher_change_asks_again() {
    let assoc =
        StaticAssociations::new().with("pptx", HandlerInfo::new(POWERPNT, Signature::Unsigned));
    let d = eval_with(&assoc, &catalogue(), "deck.pptx");
    assert_eq!(d.reason, DecisionReason::PublisherChanged);
    assert_eq!(d.outcome, StreamDecision::Ask);
}

#[test]
fn browser_gets_the_isolated_template() {
    let d = eval("Report.pdf");
    assert_eq!(d.outcome, StreamDecision::Allowed);
    let launch = d.browser.expect("browser launch");
    assert_eq!(launch.browser, Browser::Edge);
    let args = launch.args(
        "file:///D:/Share/Report.pdf",
        Path::new("C:\\gander-profile"),
    );
    assert_eq!(
        args,
        vec![
            "--new-window".to_owned(),
            "--app=file:///D:/Share/Report.pdf".to_owned(),
            "--user-data-dir=C:\\gander-profile".to_owned(),
        ]
    );
    assert_eq!(
        launch.phone_notice(),
        "opens in Microsoft Edge (isolated window)"
    );
    // A browser not yet in the catalogue still only gets the isolated launch, after asking.
    let d = eval("page.htm");
    assert_eq!(d.outcome, StreamDecision::Ask);
    assert!(d.browser.is_some());
}

#[test]
fn double_extension_exe_is_refused() {
    let d = eval("A.PDF.EXE");
    assert_eq!(d.outcome, StreamDecision::Refused);
    assert_eq!(d.reason, DecisionReason::HardRefusedExtension);
    assert_eq!(eval("evil.pdf.exe. ").outcome, StreamDecision::Refused);
}

#[test]
fn macro_enabled_office_is_refused_even_with_office_handler() {
    assert_eq!(
        eval("deck.ppsm").reason,
        DecisionReason::HardRefusedExtension
    );
    assert_eq!(
        eval("deck.pptm").reason,
        DecisionReason::HardRefusedExtension
    );
}

#[test]
fn file_without_extension_is_refused() {
    for name in ["Makefile", "README", "trailing.", ".bashrc"] {
        let d = eval(name);
        assert_eq!(d.outcome, StreamDecision::Refused, "{name}");
        assert_eq!(d.reason, DecisionReason::NoExtension, "{name}");
    }
}

#[test]
fn no_handler_and_lookup_failure_fail_closed() {
    let d = eval("data.unknownext");
    assert_eq!(d.reason, DecisionReason::NoHandler);
    assert_eq!(
        d.error_body().unwrap().error.code,
        ErrorCode::AppUnavailable
    );

    struct Failing;
    impl AssociationResolver for Failing {
        fn resolve(&self, _: &str) -> Result<Option<HandlerInfo>, AssocError> {
            Err(AssocError("boom".into()))
        }
        fn inspect(&self, _: &Path) -> Result<HandlerInfo, AssocError> {
            Err(AssocError("boom".into()))
        }
    }
    let cat = catalogue();
    let env = PolicyEnvironment::default();
    let d = StreamPolicy::new(&cat, &Failing, &env).evaluate(&input("deck.pptx"));
    assert_eq!(d.reason, DecisionReason::HandlerLookupFailed);
    assert_eq!(d.outcome, StreamDecision::Refused);
}

#[test]
fn no_permission_is_refused_first() {
    let assoc = associations();
    let cat = catalogue();
    let env = PolicyEnvironment::default();
    let mut inp = input("deck.pptx");
    inp.stream_permitted = false;
    let d = StreamPolicy::new(&cat, &assoc, &env).evaluate(&inp);
    assert_eq!(d.reason, DecisionReason::NoStreamPermission);
    assert_eq!(
        d.error_body().unwrap().error.code,
        ErrorCode::PermissionDenied
    );
}

#[test]
fn requested_app_uses_the_catalogue_entry() {
    let mut assoc = associations();
    assoc.insert_executable(HandlerInfo::new(
        BLENDER,
        Signature::Signed {
            publisher: "Blender Foundation".to_owned(),
        },
    ));
    let cat = catalogue();
    let env = PolicyEnvironment::default();
    let policy = StreamPolicy::new(&cat, &assoc, &env);
    let blender = AppId::from("blender");
    let mut inp = input("scene.blend");
    inp.requested_app = Some(&blender);
    assert_eq!(policy.evaluate(&inp).outcome, StreamDecision::Allowed);
    // Asking PowerPoint to open a .blend: approved app, new extension.
    let ppt = AppId::from("powerpoint");
    inp.requested_app = Some(&ppt);
    assert_eq!(
        policy.evaluate(&inp).reason,
        DecisionReason::NewExtensionForApprovedApp
    );
    let nope = AppId::from("nope");
    inp.requested_app = Some(&nope);
    assert_eq!(policy.evaluate(&inp).reason, DecisionReason::UnknownApp);
    // The step-1 list still wins over a named app.
    let mut inp = input("x.exe");
    inp.requested_app = Some(&blender);
    assert_eq!(
        policy.evaluate(&inp).reason,
        DecisionReason::HardRefusedExtension
    );
}

#[test]
fn catalogue_approval_load_save_roundtrip() {
    let store = MemoryStore::default();
    let dir = Path::new("cfg");
    let env = PolicyEnvironment::default();
    let mut cat = AppCatalogue::load(&store, dir).unwrap();
    assert!(cat.apps.is_empty());

    let viewer = HandlerInfo::new("C:\\Tools\\viewer.exe", Signature::Unsigned);
    let now = time::macros_free_now();
    let id = approve_handler(&mut cat, &env, &viewer, "Viewer", ".XYZ", now).unwrap();
    assert_eq!(id.as_str(), "viewer");
    approve_extension(&mut cat, &id, "abc").unwrap();
    cat.save(&store, dir).unwrap();

    let loaded = AppCatalogue::load(&store, dir).unwrap();
    assert_eq!(loaded, cat);
    let assoc = StaticAssociations::new().with("xyz", viewer.clone());
    assert_eq!(
        eval_with(&assoc, &loaded, "a.xyz").outcome,
        StreamDecision::Allowed
    );

    // Same exe again: reuses the entry. A second exe with the same stem gets a fresh id.
    let id2 = approve_handler(&mut cat, &env, &viewer, "Viewer", "def", now).unwrap();
    assert_eq!(id2, id);
    let other = HandlerInfo::new("D:\\viewer.exe", Signature::Unsigned);
    let id3 = approve_handler(&mut cat, &env, &other, "Viewer", "xyz", now).unwrap();
    assert_eq!(id3.as_str(), "viewer-2");

    // Never-listed handlers and refused extensions cannot be approved.
    let cmd = HandlerInfo::new("C:\\Windows\\System32\\cmd.exe", ms());
    assert_eq!(
        approve_handler(&mut cat, &env, &cmd, "cmd", "txt", now),
        Err(ApprovalError::HandlerNotApprovable)
    );
    assert_eq!(
        approve_handler(&mut cat, &env, &viewer, "Viewer", "exe", now),
        Err(ApprovalError::ExtensionNotApprovable)
    );
    assert_eq!(
        approve_extension(&mut cat, &AppId::from("missing"), "abc"),
        Err(ApprovalError::UnknownApp)
    );
}

#[test]
fn approving_a_suggestion_records_publisher_and_time() {
    let env = PolicyEnvironment::default();
    let mut cat = catalogue();
    cat.apps[2].approved_by_owner = None;
    cat.apps[2].publisher = None;
    let handler = HandlerInfo::new(
        BLENDER,
        Signature::Signed {
            publisher: "Blender Foundation".to_owned(),
        },
    );
    let id = approve_handler(
        &mut cat,
        &env,
        &handler,
        "ignored",
        "obj",
        time::macros_free_now(),
    )
    .unwrap();
    assert_eq!(id.as_str(), "blender");
    let entry = &cat.apps[2];
    assert!(entry.is_approved());
    assert_eq!(entry.publisher.as_deref(), Some("Blender Foundation"));
    assert!(entry.handles_extension("obj"));
}

fn host_config(permissions: Vec<Permission>) -> HostConfig {
    let mut cfg = HostConfig::default();
    cfg.shares.push(ShareConfig {
        id: ShareId::from("shr_a"),
        name: "Projects".to_owned(),
        root: "D:\\Projects".into(),
        show_hidden: false,
        show_path: false,
        hidden_patterns: Vec::new(),
        stream_default: StreamDecision::Ask,
        grants: vec![ShareGrant {
            device: DeviceId::from("dev_phone"),
            permissions,
        }],
    });
    cfg
}

fn stream_request(app: Option<&str>) -> StreamRequest {
    StreamRequest {
        share: ShareId::from("shr_a"),
        path: "Clients/Acme/Deck.pptx".to_owned(),
        app: app.map(AppId::from),
        viewport: Viewport {
            width: 1080,
            height: 2280,
            dpr: 2.625,
        },
        prefer: None,
        resume: None,
    }
}

#[test]
fn post_stream_validates_policy_then_reports_unavailable() {
    let assoc = associations();
    let cat = catalogue();
    let env = PolicyEnvironment::default();
    let policy = StreamPolicy::new(&cat, &assoc, &env);
    let phone = DeviceId::from("dev_phone");
    let cfg = host_config(vec![Permission::List, Permission::Stream]);
    let req = stream_request(None);

    let code = |file: &str, cfg: &HostConfig, dev: &DeviceId, req: &StreamRequest| {
        let err = handle_stream_request(&policy, cfg, dev, req, file).unwrap_err();
        assert_eq!(err.http_status(), err.error.code.http_status());
        err.error.code
    };
    // Allowed and ask: policy passes, Stream itself is unavailable.
    assert_eq!(
        code("Deck.pptx", &cfg, &phone, &req),
        ErrorCode::StreamUnavailable
    );
    assert_eq!(
        code("thing.xyz", &cfg, &phone, &req),
        ErrorCode::StreamUnavailable
    );
    // Refusals come from the policy.
    assert_eq!(
        code("a.pdf.exe", &cfg, &phone, &req),
        ErrorCode::StreamRefused
    );
    assert_eq!(
        code("data.qqq", &cfg, &phone, &req),
        ErrorCode::AppUnavailable
    );
    let mut req_app = stream_request(Some("nope"));
    assert_eq!(
        code("Deck.pptx", &cfg, &phone, &req_app),
        ErrorCode::AppUnavailable
    );
    // Permission and share visibility come before the policy.
    let no_stream = host_config(vec![Permission::List]);
    assert_eq!(
        code("Deck.pptx", &no_stream, &phone, &req),
        ErrorCode::PermissionDenied
    );
    let stranger = DeviceId::from("dev_x");
    assert_eq!(
        code("Deck.pptx", &cfg, &stranger, &req),
        ErrorCode::ShareNotFound
    );
    // Bad viewport.
    req_app.app = None;
    req_app.viewport.dpr = f64::NAN;
    assert_eq!(
        code("Deck.pptx", &cfg, &phone, &req_app),
        ErrorCode::BadRequest
    );

    let err = handle_stream_request(&policy, &cfg, &phone, &req, "Deck.pptx").unwrap_err();
    assert_eq!(err.http_status(), 503);
    assert!(!phase1_stream_capabilities().enabled);
}

#[test]
fn listing_entries_carry_the_outcome() {
    let assoc = associations();
    let cat = catalogue();
    let env = PolicyEnvironment::default();
    let policy = StreamPolicy::new(&cat, &assoc, &env);
    let phone = DeviceId::from("dev_phone");
    let cfg = host_config(vec![Permission::List, Permission::Stream]);
    let mut listing = ListingPolicy::new(policy, &cfg.shares[0], &phone);
    assert_eq!(listing.decision("Deck.pptx"), StreamDecision::Allowed);
    assert_eq!(listing.decision("other.PPTX"), StreamDecision::Allowed);
    assert_eq!(listing.decision("model.obj"), StreamDecision::Ask);
    assert_eq!(listing.decision("setup.exe"), StreamDecision::Refused);
    assert_eq!(listing.decision("Makefile"), StreamDecision::Refused);

    let cfg = host_config(vec![Permission::List]);
    let mut listing = ListingPolicy::new(policy, &cfg.shares[0], &phone);
    assert_eq!(listing.decision("Deck.pptx"), StreamDecision::Refused);
}

#[test]
fn reason_names_are_unique_snake_case() {
    use DecisionReason::*;
    let all = [
        NoStreamPermission,
        NoExtension,
        HardRefusedExtension,
        NoHandler,
        HandlerLookupFailed,
        NeverListedHandler,
        SystemHandler,
        UnknownApp,
        ShareDefaultRefused,
        CatalogueApproved,
        NewExtensionForApprovedApp,
        PublisherChanged,
        UnknownHandler,
    ];
    let names: std::collections::BTreeSet<_> = all.iter().map(|r| r.as_str()).collect();
    assert_eq!(names.len(), all.len());
    for r in all {
        assert_eq!(
            r.error_body().is_some(),
            r.outcome() == StreamDecision::Refused
        );
        assert!(
            r.as_str()
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b == b'_')
        );
    }
}

mod time {
    //! A fixed timestamp (tests must not depend on the clock).
    pub(super) fn macros_free_now() -> crate::protocol::Timestamp {
        ::time::OffsetDateTime::from_unix_timestamp(1_793_650_263).unwrap()
    }
}
