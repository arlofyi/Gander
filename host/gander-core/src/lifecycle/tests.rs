// SPDX-License-Identifier: Apache-2.0
//! State-machine tests (WP-53 "Verify: state-machine unit tests"), including reconnection
//! within and after the grace period (launch-policy spec, "Tests").

use super::*;

fn t0() -> Instant {
    Instant::now()
}

fn facts(locked: bool, attended: bool, busy: bool) -> HostFacts {
    HostFacts {
        locked,
        attended,
        busy,
    }
}

/// A session that went straight to `window_found` (no prompt).
fn found(now: Instant) -> Machine {
    let mut m = Machine::new(None, Timing::default());
    m.apply(Event::Launching, now).unwrap();
    m.apply(Event::WindowFound, now).unwrap();
    m
}

#[test]
fn preconditions_in_spec_order() {
    let t = Timing::default();
    let allow = IdlePromptDefault::Allow;
    // Locked beats everything (fail closed), then busy, then nobody to approve.
    assert_eq!(
        preconditions(facts(true, true, true), StreamDecision::Ask, allow, &t),
        Err(ErrorCode::HostLocked)
    );
    assert_eq!(
        preconditions(facts(false, false, true), StreamDecision::Ask, allow, &t),
        Err(ErrorCode::StreamBusy)
    );
    assert_eq!(
        preconditions(facts(false, false, false), StreamDecision::Ask, allow, &t),
        Err(ErrorCode::HostUnattended)
    );
    assert_eq!(
        preconditions(
            facts(false, false, false),
            StreamDecision::Refused,
            allow,
            &t
        ),
        Err(ErrorCode::StreamRefused)
    );
    // Allowed, nobody there: straight to the launch.
    assert_eq!(
        preconditions(
            facts(false, false, false),
            StreamDecision::Allowed,
            allow,
            &t
        ),
        Ok(None)
    );
    // Ask, someone there: the 60 s approval prompt, unanswered -> host_unattended.
    let p = preconditions(facts(false, true, false), StreamDecision::Ask, allow, &t)
        .unwrap()
        .unwrap();
    assert_eq!(p.kind, PromptKind::Approval);
    assert_eq!(p.timeout, APPROVAL_TIMEOUT);
    assert_eq!(p.on_timeout, TimeoutAnswer::Unattended);
    // Allowed, someone there: the 10 s toast with the configured default.
    let p = preconditions(
        facts(false, true, false),
        StreamDecision::Allowed,
        allow,
        &t,
    )
    .unwrap()
    .unwrap();
    assert_eq!(p.kind, PromptKind::Attended);
    assert_eq!(p.timeout, ATTENDED_PROMPT_TIMEOUT);
    assert_eq!(p.on_timeout, TimeoutAnswer::Allow);
    let p = preconditions(
        facts(false, true, false),
        StreamDecision::Allowed,
        IdlePromptDefault::Deny,
        &t,
    )
    .unwrap()
    .unwrap();
    assert_eq!(p.on_timeout, TimeoutAnswer::Unattended);
}

#[test]
fn messages_name_no_paths() {
    for code in [
        ErrorCode::HostLocked,
        ErrorCode::StreamBusy,
        ErrorCode::HostUnattended,
    ] {
        let m = precondition_message(code);
        assert!(!m.contains('\\') && !m.contains(":/"), "{m}");
    }
}

#[test]
fn happy_path_without_prompt() {
    let now = t0();
    let mut m = Machine::new(None, Timing::default());
    assert_eq!(m.state(), StreamState::Requested);
    assert!(
        m.apply(Event::PromptShown, now).is_err(),
        "no prompt to show"
    );
    let s = m.apply(Event::Launching, now).unwrap();
    assert!(s.report && s.state == StreamState::Launching);
    let s = m.apply(Event::WindowFound, now).unwrap();
    assert!(s.report && s.state == StreamState::WindowFound);
    assert_eq!(m.deadline(), Some(now + NEGOTIATION_TIMEOUT));
    // The engine reports streaming itself: the machine follows without reporting.
    let s = m
        .apply(Event::Engine(EngineReport::Streaming), now)
        .unwrap();
    assert!(!s.report);
    assert_eq!(m.state(), StreamState::Streaming);
    assert_eq!(m.deadline(), None);
    let s = m.apply(Event::End(EndCause::ClientClosed), now).unwrap();
    assert_eq!(s.state, StreamState::Ended);
    assert_eq!(s.reason, Some(StreamReason::ClientClosed));
    assert_eq!(s.ended, Some(EndCause::ClientClosed));
    assert!(m.is_ended());
    assert!(m.apply(Event::End(EndCause::KillSwitch), now).is_err());
}

#[test]
fn approval_prompt_allow_deny_and_timeout() {
    let now = t0();
    let t = Timing::default();
    let prompt = preconditions(
        facts(false, true, false),
        StreamDecision::Ask,
        IdlePromptDefault::Allow,
        &t,
    )
    .unwrap();

    // Launching before the prompt was shown is refused.
    let mut m = Machine::new(prompt, t);
    assert!(m.apply(Event::Launching, now).is_err());
    let s = m.apply(Event::PromptShown, now).unwrap();
    assert!(s.report && s.state == StreamState::Approving);
    assert!(m.apply(Event::Launching, now).is_err(), "not answered yet");
    m.apply(Event::Allowed, now).unwrap();
    assert_eq!(
        m.apply(Event::Launching, now).unwrap().state,
        StreamState::Launching
    );

    let mut m = Machine::new(prompt, t);
    m.apply(Event::PromptShown, now).unwrap();
    let s = m.apply(Event::Denied, now).unwrap();
    assert_eq!(s.ended, Some(EndCause::Denied));
    assert_eq!(
        EndCause::Denied.error().map(|e| e.0),
        Some(ErrorCode::StreamRefused)
    );

    let mut m = Machine::new(prompt, t);
    m.apply(Event::PromptShown, now).unwrap();
    assert!(
        m.tick(now + APPROVAL_TIMEOUT - Duration::from_millis(1))
            .is_none()
    );
    let s = m.tick(now + APPROVAL_TIMEOUT).unwrap();
    assert_eq!(s.ended, Some(EndCause::Unattended));
    assert_eq!(s.reason, Some(StreamReason::Timeout));
    assert_eq!(
        EndCause::Unattended.error().map(|e| e.0),
        Some(ErrorCode::HostUnattended)
    );
}

#[test]
fn attended_prompt_default_allow_launches_and_deny_ends() {
    let now = t0();
    let t = Timing::default();
    let allow = preconditions(
        facts(false, true, false),
        StreamDecision::Allowed,
        IdlePromptDefault::Allow,
        &t,
    )
    .unwrap();
    let mut m = Machine::new(allow, t);
    m.apply(Event::PromptShown, now).unwrap();
    let s = m.tick(now + ATTENDED_PROMPT_TIMEOUT).unwrap();
    assert!(
        s.ended.is_none() && !s.report,
        "default allow is not an end"
    );
    assert_eq!(
        m.apply(Event::Launching, now).unwrap().state,
        StreamState::Launching
    );

    let deny = preconditions(
        facts(false, true, false),
        StreamDecision::Allowed,
        IdlePromptDefault::Deny,
        &t,
    )
    .unwrap();
    let mut m = Machine::new(deny, t);
    m.apply(Event::PromptShown, now).unwrap();
    let s = m.tick(now + ATTENDED_PROMPT_TIMEOUT).unwrap();
    assert_eq!(s.ended, Some(EndCause::Unattended));
}

#[test]
fn pick_then_choice_or_timeout() {
    let now = t0();
    let mut m = Machine::new(None, Timing::default());
    m.apply(Event::Launching, now).unwrap();
    let s = m
        .apply(Event::Pick(vec!["A".into(), "B".into()]), now)
        .unwrap();
    assert_eq!(s.state, StreamState::Pick);
    assert_eq!(s.candidates, vec!["A".to_owned(), "B".to_owned()]);
    assert!(!m.can_resume());
    assert!(
        m.apply(Event::Engine(EngineReport::Streaming), now)
            .is_err()
    );
    let s = m.apply(Event::Picked, now).unwrap();
    assert_eq!(s.state, StreamState::WindowFound);

    let mut m = Machine::new(None, Timing::default());
    m.apply(Event::Launching, now).unwrap();
    m.apply(Event::Pick(vec!["A".into()]), now).unwrap();
    let s = m.tick(now + PICK_TIMEOUT).unwrap();
    assert_eq!(s.ended, Some(EndCause::AppUnavailable));
    assert_eq!(
        EndCause::AppUnavailable.error().map(|e| e.0),
        Some(ErrorCode::AppUnavailable)
    );
}

#[test]
fn negotiation_times_out() {
    let now = t0();
    let mut m = found(now);
    assert!(m.tick(now + NEGOTIATION_TIMEOUT / 2).is_none());
    let s = m.tick(now + NEGOTIATION_TIMEOUT).unwrap();
    assert_eq!(s.ended, Some(EndCause::Timeout));
}

#[test]
fn network_pause_recovers_within_the_grace_period() {
    let now = t0();
    let mut m = found(now);
    m.apply(Event::Engine(EngineReport::Streaming), now)
        .unwrap();
    let s = m
        .apply(
            Event::Engine(EngineReport::Paused(StreamReason::Network)),
            now,
        )
        .unwrap();
    assert!(!s.report, "the engine reported the pause itself");
    assert!(m.in_grace());
    assert_eq!(m.deadline(), Some(now + RESUME_GRACE));
    // A second network pause does not extend the grace.
    let later = now + Duration::from_secs(30);
    m.apply(Event::Engine(EngineReport::Streaming), later)
        .unwrap();
    assert!(!m.in_grace() && m.deadline().is_none());
    m.apply(
        Event::Engine(EngineReport::Paused(StreamReason::Network)),
        later,
    )
    .unwrap();
    assert_eq!(m.deadline(), Some(later + RESUME_GRACE));
    assert!(
        m.tick(later + RESUME_GRACE - Duration::from_secs(1))
            .is_none()
    );
    m.apply(
        Event::Engine(EngineReport::Streaming),
        later + Duration::from_secs(60),
    )
    .unwrap();
    assert_eq!(m.state(), StreamState::Streaming);
    assert!(
        m.tick(later + RESUME_GRACE * 2).is_none(),
        "no deadline while streaming"
    );
}

#[test]
fn other_pauses_have_no_deadline() {
    let now = t0();
    let mut m = found(now);
    for r in [
        StreamReason::PhoneBackground,
        StreamReason::FocusLost,
        StreamReason::LocalUserActive,
    ] {
        m.apply(Event::Engine(EngineReport::Paused(r)), now)
            .unwrap();
        assert_eq!(m.reason(), Some(r));
        assert!(!m.in_grace());
        assert!(m.tick(now + RESUME_GRACE * 10).is_none());
    }
}

#[test]
fn lost_peer_then_resume_within_grace() {
    let now = t0();
    let mut m = found(now);
    m.apply(Event::Engine(EngineReport::Streaming), now)
        .unwrap();
    let s = m
        .apply(Event::Engine(EngineReport::NetworkLost), now)
        .unwrap();
    assert!(s.report, "the host reports a lost peer connection itself");
    assert_eq!(s.state, StreamState::Paused);
    assert_eq!(s.reason, Some(StreamReason::Network));
    assert!(m.engine_lost() && m.can_resume());
    // Whatever the dead engine session says afterwards does not count.
    m.apply(Event::Engine(EngineReport::Streaming), now)
        .unwrap();
    assert!(m.in_grace());

    let at = now + RESUME_GRACE - Duration::from_secs(1);
    assert!(m.tick(at).is_none());
    let s = m.apply(Event::Resumed, at).unwrap();
    assert!(s.report && s.state == StreamState::WindowFound);
    assert!(!m.engine_lost());
    assert_eq!(m.deadline(), Some(at + NEGOTIATION_TIMEOUT));
    m.apply(Event::Engine(EngineReport::Streaming), at).unwrap();
    assert_eq!(m.state(), StreamState::Streaming);
}

#[test]
fn lost_peer_after_the_grace_period_ends_with_timeout() {
    let now = t0();
    let mut m = found(now);
    m.apply(Event::Engine(EngineReport::NetworkLost), now)
        .unwrap();
    let s = m.tick(now + RESUME_GRACE).unwrap();
    assert_eq!(s.ended, Some(EndCause::Timeout));
    assert_eq!(s.reason, Some(StreamReason::Timeout));
    // Too late to resume.
    assert!(!m.can_resume());
    assert!(m.apply(Event::Resumed, now + RESUME_GRACE).is_err());
}

#[test]
fn ends_from_any_live_state() {
    let now = t0();
    for cause in [
        EndCause::KillSwitch,
        EndCause::Revoked,
        EndCause::HostLocked,
        EndCause::WindowClosed,
        EndCause::Error,
    ] {
        // requested, launching, pick, grace
        let mut m = Machine::new(None, Timing::default());
        assert_eq!(m.apply(Event::End(cause), now).unwrap().ended, Some(cause));
        let mut m = Machine::new(None, Timing::default());
        m.apply(Event::Launching, now).unwrap();
        m.apply(Event::Pick(vec![]), now).unwrap();
        assert_eq!(
            m.apply(Event::End(cause), now).unwrap().reason,
            Some(cause.wire())
        );
        let mut m = found(now);
        m.apply(Event::Engine(EngineReport::NetworkLost), now)
            .unwrap();
        assert!(m.apply(Event::End(cause), now).unwrap().report);
    }
    assert_eq!(EndCause::HostLocked.wire(), StreamReason::Error);
    assert_eq!(
        EndCause::HostLocked.error().map(|e| e.0),
        Some(ErrorCode::HostLocked)
    );
    assert_eq!(EndCause::KillSwitch.wire(), StreamReason::HostKillSwitch);
    assert!(EndCause::ClientClosed.error().is_none());
}

#[test]
fn close_on_end_policies() {
    let base = CloseFacts {
        policy: CloseOnEnd::IfLaunchedByUs,
        cause: EndCause::ClientClosed,
        close_app: false,
        launched_by_us: true,
        only_window: true,
    };
    assert_eq!(close_action(base), CloseAction::Close);
    for cause in [
        EndCause::KillSwitch,
        EndCause::HostLocked,
        EndCause::Timeout,
        EndCause::Revoked,
    ] {
        assert_eq!(
            close_action(CloseFacts { cause, ..base }),
            CloseAction::Close
        );
    }
    // Never someone else's app, never with other windows open, nothing when it closed itself.
    assert_eq!(
        close_action(CloseFacts {
            launched_by_us: false,
            close_app: true,
            ..base
        }),
        CloseAction::Leave
    );
    assert_eq!(
        close_action(CloseFacts {
            only_window: false,
            ..base
        }),
        CloseAction::Leave
    );
    assert_eq!(
        close_action(CloseFacts {
            cause: EndCause::WindowClosed,
            ..base
        }),
        CloseAction::Leave
    );
    // never: close_app is not honoured.
    assert_eq!(
        close_action(CloseFacts {
            policy: CloseOnEnd::Never,
            close_app: true,
            ..base
        }),
        CloseAction::Leave
    );
    // ask: only the phone's DELETE ?close_app=true closes.
    let ask = CloseFacts {
        policy: CloseOnEnd::Ask,
        ..base
    };
    assert_eq!(close_action(ask), CloseAction::Leave);
    assert_eq!(
        close_action(CloseFacts {
            close_app: true,
            ..ask
        }),
        CloseAction::Close
    );
    assert_eq!(
        close_action(CloseFacts {
            close_app: true,
            cause: EndCause::KillSwitch,
            ..ask
        }),
        CloseAction::Leave
    );
}

#[test]
fn front_on_connect_needs_unlocked_and_no_unconsenting_local_user() {
    let f = |locked, attended, owner_allowed| {
        front_on_connect(FrontFacts {
            locked,
            attended,
            owner_allowed,
        })
    };
    // Nobody there, unlocked: bring it forward.
    assert!(f(false, false, false));
    assert!(f(false, false, true));
    // Someone there: only with the owner's explicit Allow on the host.
    assert!(!f(false, true, false));
    assert!(f(false, true, true));
    // Locked: never, whatever else holds.
    assert!(!f(true, false, false));
    assert!(!f(true, false, true));
    assert!(!f(true, true, true));
}
