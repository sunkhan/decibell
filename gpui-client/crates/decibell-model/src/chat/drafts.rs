//! In-memory drafts of unsent messages, so the composer survives a channel or DM switch
//! (`stores/draftsStore.ts`). Keys: `(server, channel)` for channels (bare channel ids
//! collide across communities), the peer's uid for DMs. Empty drafts are removed. Never
//! written to disk.
//!
//! A keystroke changes one draft: [`DraftsEvent::Changed`] names it, so only the composer
//! (and the send button, the preview) of that key re-render; they subscribe and filter
//! rather than observe.

use std::collections::HashMap;

use gpui::{Context, EventEmitter, SharedString};

use crate::ids::{ChannelId, ServerId, Uid};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum DraftKey {
    Channel(ServerId, ChannelId),
    Dm(Uid),
}

pub enum DraftsEvent {
    Changed(DraftKey),
    /// A draft was put back from outside the composer (a refused or unconfirmed send):
    /// the open composer of that key shows it (`restored` / `restoredDm`).
    Restored(DraftKey),
}

impl EventEmitter<DraftsEvent> for Drafts {}

#[derive(Default)]
pub struct Drafts {
    drafts: HashMap<DraftKey, SharedString>,
}

impl Drafts {
    pub fn get(&self, key: &DraftKey) -> SharedString {
        self.drafts.get(key).cloned().unwrap_or_default()
    }

    pub fn len(&self) -> usize {
        self.drafts.len()
    }

    pub fn is_empty(&self) -> bool {
        self.drafts.is_empty()
    }

    pub fn set(&mut self, key: DraftKey, value: SharedString, cx: &mut Context<Self>) {
        let changed = if value.is_empty() {
            self.drafts.remove(&key).is_some()
        } else if self.drafts.get(&key) == Some(&value) {
            false
        } else {
            self.drafts.insert(key.clone(), value);
            true
        };
        if changed {
            cx.emit(DraftsEvent::Changed(key));
            cx.notify();
        }
    }

    pub fn clear(&mut self, key: &DraftKey, cx: &mut Context<Self>) {
        if self.drafts.remove(key).is_some() {
            cx.emit(DraftsEvent::Changed(key.clone()));
            cx.notify();
        }
    }

    /// Put the text of a refused message back, unless something new was typed since; true
    /// when it was put back. Channels count any draft as "typed"; DMs ignore a blank one
    /// (`restoreChannelDraft` vs `restoreDmDraft`, mirrored as they are).
    pub fn restore(&mut self, key: DraftKey, value: SharedString, cx: &mut Context<Self>) -> bool {
        if value.is_empty() {
            return false;
        }
        let typed = match (&key, self.drafts.get(&key)) {
            (_, None) => false,
            (DraftKey::Channel(..), Some(d)) => !d.is_empty(),
            (DraftKey::Dm(_), Some(d)) => !d.trim().is_empty(),
        };
        if typed {
            return false;
        }
        self.drafts.insert(key.clone(), value);
        cx.emit(DraftsEvent::Changed(key.clone()));
        cx.emit(DraftsEvent::Restored(key));
        cx.notify();
        true
    }

    /// The renderer kept DM drafts on sign-out only by accident of `setState`: everything
    /// goes (`resetForLogout`'s drafts line).
    pub fn reset_for_logout(&mut self, cx: &mut Context<Self>) {
        if !self.drafts.is_empty() {
            self.drafts.clear();
            cx.notify();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use gpui::{AppContext as _, TestAppContext};

    use super::*;

    #[gpui::test]
    fn drafts_keyed_per_channel_and_peer_with_restores(cx: &mut TestAppContext) {
        let drafts = cx.new(|_| Drafts::default());
        let events: Rc<RefCell<Vec<String>>> = Rc::default();
        let _sub = cx.update(|cx| {
            let events = events.clone();
            cx.subscribe(&drafts, move |_, e: &DraftsEvent, _| {
                events.borrow_mut().push(match e {
                    DraftsEvent::Changed(k) => format!("changed {k:?}"),
                    DraftsEvent::Restored(k) => format!("restored {k:?}"),
                })
            })
        });
        let a = DraftKey::Channel(ServerId::from_central(1), "general".into());
        let b = DraftKey::Channel(ServerId::from_central(2), "general".into());
        let dm = DraftKey::Dm(3);
        drafts.update(cx, |d, cx| {
            d.set(a.clone(), "hello".into(), cx);
            d.set(a.clone(), "hello".into(), cx); // unchanged: no event
            assert_eq!(&*d.get(&a), "hello");
            assert_eq!(&*d.get(&b), "", "same slug, other community");
            // Something typed: a refused send doesn't overwrite it.
            assert!(!d.restore(a.clone(), "old".into(), cx));
            // Nothing typed: it comes back.
            assert!(d.restore(b.clone(), "refused".into(), cx));
            // A DM with only blanks typed counts as empty; a channel's doesn't.
            d.set(dm.clone(), "  ".into(), cx);
            assert!(d.restore(dm.clone(), "back".into(), cx));
            d.set(a.clone(), " ".into(), cx);
            assert!(!d.restore(a.clone(), "x".into(), cx));
            d.set(a.clone(), "".into(), cx);
            assert_eq!(d.len(), 2, "empty drafts are removed");
            d.clear(&dm, cx);
            assert_eq!(d.len(), 1);
        });
        let events = events.borrow();
        assert_eq!(events.iter().filter(|e| e.starts_with("restored")).count(), 2);
        assert_eq!(events.len(), 9);
    }
}
