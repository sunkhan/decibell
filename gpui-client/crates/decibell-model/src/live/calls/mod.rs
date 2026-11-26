//! The (at most one) P2P DM call (`stores/callStore.ts`). The media session (mute /
//! deafen / speaking / streams / ping) is the voice session's, which runs with no channel
//! and `call_peer` set; this store knows who we are calling and how far the handshake got.
//!
//! ```text
//! idle ──call()──▶ outgoing ──ACCEPT──▶ connecting ──call_connected──▶ active
//! idle ──INVITE──▶ incoming ──accept()─▶ connecting ──call_connected──▶ active
//! any ──REJECT / BUSY / CANCEL / HANGUP / PEER_OFFLINE / NOT_ALLOWED / call_failed /
//!       call_dropped / 45 s──▶ idle
//! incoming ──ANSWERED_ELSEWHERE / DECLINED_ELSEWHERE──▶ idle (+ incoming_ended)
//! ```
//!
//! The ring timer ([`RING_TIMEOUT`]) is held here, armed when an INVITE goes out (caller)
//! or arrives (callee), and exists only while ringing: every transition out of
//! `outgoing` / `incoming` drops it. The transitions with their side effects (signals,
//! the core's `call_*`, sounds, toasts) are `actions::calls` and `events::calls`; how
//! signals and the core calls leave is the [`CallTransport`] given at construction, so
//! the state machine is tested against a fake.
//!
//! **Rhythm**: a handful of changes per call. The DM header, the call stage, the incoming
//! card and the user panel observe it.

use std::rc::Rc;
use std::time::{Duration, Instant};

use decibell_core::events::{CallCandidatePayload, CallStreamMetaPayload};
use futures::future::LocalBoxFuture;
use gpui::{Context, SharedString, Task};

use crate::error::UiError;
use crate::ids::Uid;

/// How long an unanswered call rings before the caller gives up and the callee's prompt
/// goes away.
pub const RING_TIMEOUT: Duration = Duration::from_millis(45_000);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum CallStatus {
    #[default]
    Idle,
    Outgoing,
    Incoming,
    Connecting,
    Active,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum CallPath {
    Host,
    Srflx,
}

/// The pending INVITE while ringing here, kept until Accept.
#[derive(Debug, Clone)]
pub struct IncomingCall {
    pub call_id: SharedString,
    pub from_uid: Uid,
    /// Their name as the signal carried it (drawn through the users store).
    pub from: SharedString,
    pub pub_key: String,
    pub candidates: Vec<CallCandidatePayload>,
    /// The caller's identity signature over the key: opaque, handed to `call_connect`.
    pub pub_key_sig: Option<String>,
    pub key_id: u32,
}

/// An incoming call another device of ours answered or declined: the card shows it for
/// a moment instead of vanishing mid-ring.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct IncomingEnded {
    pub call_id: SharedString,
    pub from_uid: Uid,
    pub from: SharedString,
    /// "Answered on another device" / "Declined on another device".
    pub reason: SharedString,
    pub answered: bool,
}

/// What the incoming card says when another device of ours took (or turned down) the
/// call ringing here; none for any other signal.
pub fn elsewhere_reason(kind: &str) -> Option<&'static str> {
    match kind {
        "ANSWERED_ELSEWHERE" => Some("Answered on another device"),
        "DECLINED_ELSEWHERE" => Some("Declined on another device"),
        _ => None,
    }
}

/// A signal to send (`send_call_signal`).
#[derive(Debug, Clone)]
pub struct OutSignal {
    pub call_id: SharedString,
    pub to_uid: Uid,
    pub to_name: SharedString,
    /// "INVITE" | "RINGING" | "ACCEPT" | "REJECT" | "CANCEL" | "HANGUP" | "BUSY" |
    /// "STREAM_START" | "STREAM_STOP".
    pub kind: &'static str,
    pub pub_key: Option<String>,
    pub candidates: Option<Vec<CallCandidatePayload>>,
    pub stream: Option<CallStreamMetaPayload>,
}

/// `call_prepare`'s answer: our ephemeral key and candidates.
#[derive(Debug, Clone)]
pub struct Prepared {
    pub pub_key: String,
    pub candidates: Vec<CallCandidatePayload>,
}

/// `call_connect`'s arguments.
#[derive(Debug, Clone)]
pub struct ConnectArgs {
    pub call_id: SharedString,
    pub peer_uid: Uid,
    pub peer_name: SharedString,
    pub remote_pub_key: String,
    pub remote_candidates: Vec<CallCandidatePayload>,
    pub remote_pub_key_sig: Option<String>,
    pub remote_key_id: Option<u32>,
}

/// How a call reaches the core (`api::call` in the app, a recorder in tests).
pub trait CallTransport: 'static {
    fn config(&self) -> LocalBoxFuture<'static, Result<(bool, Vec<String>), UiError>>;
    fn prepare(&self, call_id: SharedString, peer_uid: Uid, peer_name: SharedString) -> LocalBoxFuture<'static, Result<Prepared, UiError>>;
    fn send(&self, signal: OutSignal) -> LocalBoxFuture<'static, Result<(), UiError>>;
    fn connect(&self, args: ConnectArgs) -> LocalBoxFuture<'static, Result<(), UiError>>;
    fn end(&self) -> LocalBoxFuture<'static, Result<(), UiError>>;
    fn watch_stream(&self, watch: bool) -> LocalBoxFuture<'static, Result<(), UiError>>;
    /// A fresh call id (`crypto.randomUUID()`).
    fn new_call_id(&self) -> SharedString;
}

pub struct Calls {
    pub status: CallStatus,
    pub call_id: Option<SharedString>,
    /// The other side's account.
    pub peer: Option<Uid>,
    pub is_caller: bool,
    /// Caller: the callee's client acked the INVITE ("Ringing…" rather than "Calling…").
    pub ringing_acked: bool,
    /// When `call_connected` arrived: the call timer's base.
    pub started_at: Option<Instant>,
    pub path: Option<CallPath>,
    /// The peer's call key verified against their E2EE identity (false: they have no
    /// keys). None until connected.
    pub verified: Option<bool>,
    /// Why the last call ended (shown briefly in the DM panel).
    pub end_reason: Option<SharedString>,
    pub incoming: Option<IncomingCall>,
    pub incoming_ended: Option<IncomingEnded>,
    /// The call stage fills the DM panel; `theater_baseline` is the conversation's message
    /// count when it was switched on (the toggle's unread badge is the difference).
    pub theater: bool,
    pub theater_baseline: usize,
    /// Central relays call signals (`get_call_config`): gates the Call button.
    pub call_signaling: bool,
    pub stun_servers: Vec<SharedString>,
    pub(crate) transport: Rc<dyn CallTransport>,
    /// The 45 s ring: only while `outgoing` (after the INVITE left) or `incoming`.
    ring_timer: Option<Task<()>>,
}

impl Calls {
    pub fn new(transport: Rc<dyn CallTransport>) -> Self {
        Calls {
            status: CallStatus::Idle,
            call_id: None,
            peer: None,
            is_caller: false,
            ringing_acked: false,
            started_at: None,
            path: None,
            verified: None,
            end_reason: None,
            incoming: None,
            incoming_ended: None,
            theater: false,
            theater_baseline: 0,
            call_signaling: false,
            stun_servers: Vec::new(),
            transport,
            ring_timer: None,
        }
    }

    /// Replace how calls reach the core (tests).
    pub fn set_transport(&mut self, transport: Rc<dyn CallTransport>) {
        self.transport = transport;
    }

    pub fn transport(&self) -> Rc<dyn CallTransport> {
        self.transport.clone()
    }

    pub fn is(&self, call_id: &str) -> bool {
        self.call_id.as_deref() == Some(call_id)
    }

    pub fn ring_armed(&self) -> bool {
        self.ring_timer.is_some()
    }

    /// Arm the 45 s ring for the current call; `on_timeout` runs once if the call is
    /// still that one and still ringing then.
    pub(crate) fn arm_ring(&mut self, on_timeout: impl FnOnce(&mut gpui::App) + 'static, cx: &mut Context<Self>) {
        let Some(call_id) = self.call_id.clone() else { return };
        self.ring_timer = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(RING_TIMEOUT).await;
            let ringing = this
                .update(cx, |this, _| {
                    let ringing = this.is(&call_id) && matches!(this.status, CallStatus::Outgoing | CallStatus::Incoming);
                    this.ring_timer = None;
                    ringing
                })
                .unwrap_or(false);
            if ringing {
                cx.update(on_timeout);
            }
        }));
    }

    fn set(&mut self, status: CallStatus, cx: &mut Context<Self>) {
        if !matches!(status, CallStatus::Outgoing | CallStatus::Incoming) {
            self.ring_timer = None;
        }
        self.status = status;
        cx.notify();
    }

    pub fn set_config(&mut self, call_signaling: bool, stun_servers: Vec<SharedString>, cx: &mut Context<Self>) {
        if (self.call_signaling, &self.stun_servers) != (call_signaling, &stun_servers) {
            self.call_signaling = call_signaling;
            self.stun_servers = stun_servers;
            cx.notify();
        }
    }

    pub(crate) fn start_outgoing(&mut self, call_id: SharedString, peer: Uid, cx: &mut Context<Self>) {
        self.call_id = Some(call_id);
        self.peer = Some(peer);
        self.is_caller = true;
        self.ringing_acked = false;
        self.started_at = None;
        self.path = None;
        self.verified = None;
        self.end_reason = None;
        self.incoming = None;
        self.incoming_ended = None;
        self.ring_timer = None;
        self.set(CallStatus::Outgoing, cx);
    }

    pub(crate) fn set_incoming(&mut self, call: IncomingCall, cx: &mut Context<Self>) {
        self.call_id = Some(call.call_id.clone());
        self.peer = Some(call.from_uid);
        self.is_caller = false;
        self.ringing_acked = false;
        self.started_at = None;
        self.path = None;
        self.verified = None;
        self.end_reason = None;
        self.incoming = Some(call);
        self.incoming_ended = None;
        self.ring_timer = None;
        self.set(CallStatus::Incoming, cx);
    }

    pub(crate) fn set_ringing_acked(&mut self, cx: &mut Context<Self>) {
        if !self.ringing_acked {
            self.ringing_acked = true;
            cx.notify();
        }
    }

    pub(crate) fn set_connecting(&mut self, cx: &mut Context<Self>) {
        self.incoming = None;
        self.set(CallStatus::Connecting, cx);
    }

    pub(crate) fn set_active(&mut self, path: CallPath, verified: bool, now: Instant, cx: &mut Context<Self>) {
        self.started_at = Some(now);
        self.path = Some(path);
        self.verified = Some(verified);
        self.set(CallStatus::Active, cx);
    }

    /// Back to idle (`reset`), with why it ended.
    pub(crate) fn reset(&mut self, end_reason: Option<SharedString>, cx: &mut Context<Self>) {
        self.call_id = None;
        self.peer = None;
        self.is_caller = false;
        self.ringing_acked = false;
        self.started_at = None;
        self.path = None;
        self.verified = None;
        self.end_reason = end_reason;
        self.incoming = None;
        self.incoming_ended = None;
        self.theater = false;
        self.theater_baseline = 0;
        self.set(CallStatus::Idle, cx);
    }

    /// The ringing call ended on another device of ours.
    pub(crate) fn end_incoming_elsewhere(&mut self, reason: SharedString, answered: bool, cx: &mut Context<Self>) {
        let ended = self.incoming.as_ref().map(|i| IncomingEnded {
            call_id: i.call_id.clone(),
            from_uid: i.from_uid,
            from: i.from.clone(),
            reason: reason.clone(),
            answered,
        });
        self.reset(Some(reason), cx);
        self.incoming_ended = ended;
    }

    /// The incoming card showed the "elsewhere" state long enough.
    pub fn clear_incoming_ended(&mut self, cx: &mut Context<Self>) {
        if self.incoming_ended.take().is_some() {
            cx.notify();
        }
    }

    pub fn set_theater(&mut self, on: bool, baseline: usize, cx: &mut Context<Self>) {
        let baseline = if on { baseline } else { 0 };
        if (self.theater, self.theater_baseline) != (on, baseline) {
            self.theater = on;
            self.theater_baseline = baseline;
            cx.notify();
        }
    }
}
