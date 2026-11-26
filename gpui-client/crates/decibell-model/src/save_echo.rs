//! A settings save that settles on the server's echo, not on the request going out
//! (`features/servers/saveEcho.ts`; CLAUDE.md "Settings saves settle on the server's
//! echo").
//!
//! Dropping a draft as soon as the request is sent flicks the fields back to the old
//! values for a round trip, and loses an edit the server then refuses. So the draft stays
//! and the save bar reads "Saving…" until one of:
//!  - the store shows what was sent: [`EchoSave::settle`] returns it and the view drops
//!    its draft;
//!  - the server refuses: the view calls [`EchoSave::stop`];
//!  - [`ECHO_WAIT`] passes.
//!
//! The last two leave the draft in place.
//!
//! In a view:
//! ```ignore
//! struct RoleEditor { draft: Option<RoleDraft>, save: EchoSave<RoleDraft>, .. }
//! // Save: send the request, then
//! self.save.start(sent, cx, |this| &mut this.save);
//! // In the `cx.observe(&roles, ..)` callback (the store changed):
//! if self.save.settle(|sent| roles_show(sent)).is_some() { self.draft = None; }
//! // On `role_action_responded` without success: self.save.stop();
//! ```

use std::time::Duration;

use gpui::{Context, Task};

/// How long a save waits for the server's echo before handing the draft back for
/// another try.
pub const ECHO_WAIT: Duration = Duration::from_millis(5000);

pub struct EchoSave<T> {
    pending: Option<Pending<T>>,
}

struct Pending<T> {
    sent: T,
    /// The 5 s limit: exists only while this save waits, dropped with it.
    _limit: Task<()>,
}

impl<T> Default for EchoSave<T> {
    fn default() -> Self {
        EchoSave { pending: None }
    }
}

impl<T: 'static> EchoSave<T> {
    /// The save bar reads "Saving…".
    pub fn saving(&self) -> bool {
        self.pending.is_some()
    }

    /// What was sent, while waiting.
    pub fn sent(&self) -> Option<&T> {
        self.pending.as_ref().map(|p| &p.sent)
    }

    /// The request went out: wait for `sent` to show in the store. `slot` finds this
    /// `EchoSave` in the view again when the limit passes.
    pub fn start<V: 'static>(
        &mut self,
        sent: T,
        cx: &mut Context<V>,
        slot: impl Fn(&mut V) -> &mut EchoSave<T> + 'static,
    ) {
        let limit = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(ECHO_WAIT).await;
            let _ = this.update(cx, |view, cx| {
                slot(view).pending = None;
                cx.notify();
            });
        });
        self.pending = Some(Pending { sent, _limit: limit });
        cx.notify();
    }

    /// The server refused (or the view gave up): the draft stays.
    pub fn stop(&mut self) {
        self.pending = None;
    }

    /// Call when the store may have changed. If it now shows what was sent, the wait
    /// ends and the sent value comes back: drop the draft.
    pub fn settle(&mut self, is_echoed: impl FnOnce(&T) -> bool) -> Option<T> {
        if is_echoed(&self.pending.as_ref()?.sent) {
            return self.pending.take().map(|p| p.sent);
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use gpui::{AppContext as _, TestAppContext};

    use super::*;

    struct Editor {
        draft: Option<String>,
        save: EchoSave<String>,
    }

    fn start(editor: &gpui::Entity<Editor>, cx: &mut TestAppContext) {
        editor.update(cx, |e, cx| {
            let sent = e.draft.clone().unwrap();
            e.save.start(sent, cx, |e| &mut e.save);
        });
    }

    #[gpui::test]
    fn the_echo_ends_the_wait_and_drops_the_draft(cx: &mut TestAppContext) {
        let editor = cx.new(|_| Editor { draft: Some("Moderators".into()), save: EchoSave::default() });
        start(&editor, cx);
        let mut stored = String::from("Mods");
        editor.update(cx, |e, _| {
            assert!(e.save.saving());
            assert_eq!(e.save.sent().map(String::as_str), Some("Moderators"));
            // An unrelated store change: still waiting, the draft stays.
            assert_eq!(e.save.settle(|sent| *sent == stored), None);
            assert!(e.save.saving());
        });
        stored = "Moderators".into();
        editor.update(cx, |e, _| {
            if e.save.settle(|sent| *sent == stored).is_some() {
                e.draft = None;
            }
            assert!(!e.save.saving());
            assert_eq!(e.draft, None);
        });
        // The limit's timer went with the wait: nothing happens at 5 s.
        cx.executor().advance_clock(ECHO_WAIT * 2);
        cx.run_until_parked();
        editor.read_with(cx, |e, _| assert!(!e.save.saving()));
    }

    #[gpui::test]
    fn a_refusal_or_five_seconds_keep_the_draft(cx: &mut TestAppContext) {
        let editor = cx.new(|_| Editor { draft: Some("x".into()), save: EchoSave::default() });
        start(&editor, cx);
        editor.update(cx, |e, _| e.save.stop());
        editor.read_with(cx, |e, _| {
            assert!(!e.save.saving());
            assert_eq!(e.draft.as_deref(), Some("x"));
        });

        start(&editor, cx);
        cx.executor().advance_clock(ECHO_WAIT - Duration::from_millis(1));
        cx.run_until_parked();
        editor.read_with(cx, |e, _| assert!(e.save.saving()));
        cx.executor().advance_clock(Duration::from_millis(2));
        cx.run_until_parked();
        editor.read_with(cx, |e, _| {
            assert!(!e.save.saving(), "5 s passed");
            assert_eq!(e.draft.as_deref(), Some("x"), "the draft is handed back");
        });
    }
}
