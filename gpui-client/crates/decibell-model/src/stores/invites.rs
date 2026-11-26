//! Memo of `resolve_invite_code` lookups for the invite cards in chat, keyed by the
//! upper-cased code (`stores/inviteResolveStore.ts`). A definitive answer — resolved, or
//! central says unknown / expired — sticks for the session; a connectivity failure
//! (central down, timeout) is retried when a card asks again after [`RETRY`], so cards
//! heal once central is back.

use std::collections::{HashMap, VecDeque};
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui::{App, Context, EventEmitter, SharedString};

use crate::error::UiError;

/// `resolve_invite_code`'s answer: the endpoint plus central's directory row for the
/// pre-join preview. `server_id` 0 / empty strings when central has no row for the
/// community.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedInvite {
    pub host: SharedString,
    pub port: u16,
    pub code: SharedString,
    pub server_id: i64,
    pub server_name: SharedString,
    pub server_description: SharedString,
    pub member_count: i32,
    pub picture_version: SharedString,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InviteEntry {
    /// Still asking central.
    pub loading: bool,
    pub resolved: Option<Rc<ResolvedInvite>>,
    /// Error text when `resolved` is none; none while loading.
    pub error: Option<SharedString>,
    /// Central answered and the code is unknown or expired — as opposed to central being
    /// unreachable.
    pub invalid: bool,
    at: Instant,
}

pub const RETRY: Duration = Duration::from_secs(30);
pub const MAX_ENTRIES: usize = 300;

/// `/unknown|expired|not found|invalid/i`.
fn says_invalid(error: &str) -> bool {
    let lower = error.to_ascii_lowercase();
    ["unknown", "expired", "not found", "invalid"].iter().any(|word| lower.contains(word))
}

/// How a lookup leaves: the core call in the app (which answers through
/// [`InviteResolve::finish`]), a recorder in tests.
pub type InviteResolver = Rc<dyn Fn(SharedString, &mut App)>;

/// The code whose entry changed, for a card that shows one invite.
pub struct InviteChanged(pub SharedString);

impl EventEmitter<InviteChanged> for InviteResolve {}

pub struct InviteResolve {
    entries: HashMap<SharedString, InviteEntry>,
    /// Codes in the order they were first asked: the oldest go past [`MAX_ENTRIES`].
    order: VecDeque<SharedString>,
    resolver: InviteResolver,
}

impl InviteResolve {
    pub fn new(resolver: InviteResolver) -> InviteResolve {
        InviteResolve {
            entries: HashMap::new(),
            order: VecDeque::new(),
            resolver,
        }
    }

    /// The entry for a code (as typed: trimmed and upper-cased here).
    pub fn get(&self, code: &str) -> Option<&InviteEntry> {
        self.entries.get(code.trim().to_uppercase().as_str())
    }

    fn put(&mut self, code: SharedString, entry: InviteEntry, cx: &mut Context<Self>) {
        if self.entries.insert(code.clone(), entry).is_none() {
            self.order.push_back(code.clone());
            while self.order.len() > MAX_ENTRIES {
                if let Some(oldest) = self.order.pop_front() {
                    self.entries.remove(&oldest);
                }
            }
        }
        cx.emit(InviteChanged(code));
        cx.notify();
    }

    /// A card wants `code` resolved. No-op while it is loading, once it has a definitive
    /// answer, and for [`RETRY`] after a connectivity failure.
    pub fn request(&mut self, code: &str, cx: &mut Context<Self>) {
        let code: SharedString = code.trim().to_uppercase().into();
        if code.is_empty() {
            return;
        }
        let now = cx.background_executor().now();
        if let Some(cur) = self.entries.get(&code) {
            let retryable = !cur.loading && cur.resolved.is_none() && !cur.invalid;
            if !retryable || now.duration_since(cur.at) < RETRY {
                return;
            }
        }
        self.put(
            code.clone(),
            InviteEntry { loading: true, resolved: None, error: None, invalid: false, at: now },
            cx,
        );
        (self.resolver.clone())(code, cx);
    }

    /// Central's answer (or the failure) for `code`.
    pub fn finish(&mut self, code: SharedString, result: Result<ResolvedInvite, UiError>, cx: &mut Context<Self>) {
        let at = cx.background_executor().now();
        let entry = match result {
            Ok(resolved) => InviteEntry { loading: false, resolved: Some(Rc::new(resolved)), error: None, invalid: false, at },
            Err(e) => InviteEntry {
                loading: false,
                resolved: None,
                invalid: says_invalid(&e.text),
                error: Some(e.plain()),
                at,
            },
        };
        self.put(code, entry, cx);
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use decibell_core::api::ErrorKind;
    use gpui::{AppContext as _, TestAppContext};

    use super::*;

    fn error(text: &str) -> UiError {
        UiError { kind: ErrorKind::Other, text: text.to_string().into(), cert: None }
    }

    fn resolved(code: &str) -> ResolvedInvite {
        ResolvedInvite {
            host: "127.0.0.11".into(),
            port: 8082,
            code: code.to_string().into(),
            server_id: 1,
            server_name: "Daily Gaming".into(),
            server_description: "".into(),
            member_count: 298,
            picture_version: "".into(),
        }
    }

    #[gpui::test]
    fn definitive_answers_stick_and_connectivity_failures_retry_after_30_s(cx: &mut TestAppContext) {
        let asked: Rc<RefCell<Vec<SharedString>>> = Rc::default();
        let store = cx.new(|_| {
            let asked = asked.clone();
            InviteResolve::new(Rc::new(move |code, _| asked.borrow_mut().push(code)))
        });
        store.update(cx, |s, cx| {
            s.request("  abcd1234 ", cx);
            s.request("ABCD1234", cx); // loading: not asked twice
            s.request("   ", cx);
            assert!(s.get("abcd1234").unwrap().loading);
        });
        assert_eq!(*asked.borrow(), ["ABCD1234"]);

        // Central unreachable: retried, but not before 30 s.
        store.update(cx, |s, cx| {
            s.finish("ABCD1234".into(), Err(error("Not connected to central server")), cx);
            let e = s.get("ABCD1234").unwrap();
            assert!(!e.loading && !e.invalid && e.resolved.is_none());
            assert_eq!(e.error.as_deref(), Some("Not connected to central server"));
            s.request("ABCD1234", cx);
        });
        assert_eq!(asked.borrow().len(), 1);
        cx.executor().advance_clock(RETRY);
        store.update(cx, |s, cx| s.request("ABCD1234", cx));
        assert_eq!(asked.borrow().len(), 2);

        // Resolved: sticks for the session.
        store.update(cx, |s, cx| s.finish("ABCD1234".into(), Ok(resolved("ABCD1234")), cx));
        cx.executor().advance_clock(RETRY * 10);
        store.update(cx, |s, cx| {
            s.request("ABCD1234", cx);
            assert_eq!(&*s.get("ABCD1234").unwrap().resolved.as_ref().unwrap().server_name, "Daily Gaming");
        });
        assert_eq!(asked.borrow().len(), 2);

        // Unknown / expired: definitive too.
        store.update(cx, |s, cx| {
            s.request("DEADBEEF", cx);
            s.finish("DEADBEEF".into(), Err(error("Invite code is Unknown or expired")), cx);
            assert!(s.get("DEADBEEF").unwrap().invalid);
        });
        cx.executor().advance_clock(RETRY * 10);
        store.update(cx, |s, cx| s.request("DEADBEEF", cx));
        assert_eq!(asked.borrow().len(), 3);
    }

    #[gpui::test]
    fn the_oldest_entries_go_past_the_cap(cx: &mut TestAppContext) {
        let store = cx.new(|_| InviteResolve::new(Rc::new(|_, _| {})));
        store.update(cx, |s, cx| {
            for i in 0..MAX_ENTRIES + 5 {
                s.request(&format!("CODE{i:04}"), cx);
            }
            assert_eq!(s.entries.len(), MAX_ENTRIES);
            assert!(s.get("CODE0004").is_none());
            assert!(s.get("CODE0005").is_some());
            assert!(s.get(&format!("CODE{:04}", MAX_ENTRIES + 4)).is_some());
        });
    }
}
