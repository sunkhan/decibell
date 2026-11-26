//! Where a [`Composer`](crate::Composer) sends: a server text channel ([`ChannelTarget`],
//! `ChatPanel.tsx`) or a DM conversation ([`DmTarget`], `DmChatPanel.tsx`). The composer's look and
//! behaviour are the same; the differences are here (what it sends through, its placeholder, which
//! gates apply) and in [`Variant`] (the DM composer has no attach button, no slowmode, and its
//! buttons are `rounded-sm`).
//!
//! The DM pane hosts the same view:
//!
//! ```ignore
//! let target: Rc<dyn ComposerTarget> = Rc::new(DmTarget { peer });
//! let composer = cx.new(|cx| Composer::new(target, Some(list.clone()), window, cx));
//! // under the list: composer.clone() (uncached: it sizes itself);
//! // the pane's key listener: Composer::type_to_focus_hook(&composer)
//! ```

use std::sync::Arc;

use decibell_attachments::upload::UploadSource;
use decibell_model::actions::chat::SendOutcome;
use decibell_model::chat::{DraftKey, Message, MessageSlice, SliceKey};
use decibell_model::stores::permissions::perm;
use decibell_model::{ChannelId, Model, ServerId, Uid, actions};
use gpui::{App, Context, Entity, SharedString, Subscription};

use crate::Composer;

/// Which composer the renderer draws.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Variant {
    /// `ChatPanel`: attach button, slowmode pill, `rounded-md` buttons, the send button's held state.
    Channel,
    /// `DmChatPanel`: no attachments, no slowmode, `rounded-sm` emoji and send buttons, the inline
    /// send error above the card.
    Dm,
}

/// What may be sent here now (`canSend`, `timedOut`, `canAttach`, the slowmode interval).
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Gate {
    /// The channel's name for the placeholder and the notices (`channelName ?? "channel"`).
    pub name: SharedString,
    pub can_send: bool,
    /// Unix seconds the member's timeout ends (0: none).
    pub timed_out_until: i64,
    pub can_attach: bool,
    /// The slowmode interval that applies to us (0: none or bypassed).
    pub slowmode: u32,
}

/// See the module doc.
pub trait ComposerTarget: 'static {
    fn variant(&self) -> Variant;
    fn draft_key(&self) -> DraftKey;
    /// The community (nicknames in the reply bar); `None` in a DM.
    fn server(&self) -> Option<ServerId>;
    /// The conversation's rows: the reply target's parent as it is now, the last own message.
    fn slice(&self, cx: &App) -> Option<Entity<MessageSlice>>;
    fn placeholder(&self, gate: &Gate, cx: &App) -> SharedString;
    fn gate(&self, cx: &App) -> Gate;
    fn send(&self, text: &str, reply_to: i64, cx: &mut App) -> SendOutcome;
    fn send_gif(&self, url: &str, reply_to: i64, cx: &mut App) -> SendOutcome;
    /// The drafts store is the one owner of the draft (`setChannelDraft` / `setDmDraft`).
    fn set_draft(&self, text: SharedString, cx: &mut App);
    /// ArrowUp on an empty composer: the message of ours to edit.
    fn latest_own_editable(&self, cx: &App) -> Option<i64>;
    /// A file for the message box (picked, dropped or pasted). Nothing where attachments don't
    /// exist.
    fn attach(&self, _source: UploadSource, _cx: &mut App) {}
    /// The slowmode window's key (channels).
    fn slowmode_key(&self) -> Option<SliceKey> {
        None
    }
    /// The inline error of the last failed send (DMs: `sendError`).
    fn send_error(&self, _cx: &App) -> Option<SharedString> {
        None
    }
    /// A send's call is in flight: the send button is disabled meanwhile (DMs: `sending`).
    fn sending(&self, _cx: &App) -> bool {
        false
    }
    /// The stores [`gate`](Self::gate) / [`send_error`](Self::send_error) /
    /// [`sending`](Self::sending) read: the composer re-reads them when one of these notifies.
    fn watch(&self, cx: &mut Context<Composer>) -> Vec<Subscription>;
}

fn model(cx: &App) -> Option<Model> {
    cx.try_global::<Model>().cloned()
}

/// The newest row of ours the user may edit, in a slice (`editLatestOwn`: the facts carry the
/// renderer's rule, a sealed message in a plaintext channel excluded).
fn latest_editable(slice: &MessageSlice) -> Option<i64> {
    slice.rows().iter().zip(slice.facts()).rev().find(|(m, f)| f.can_edit && m.id > 0).map(|(m, _)| m.id)
}

/// A server text channel.
pub struct ChannelTarget {
    pub server: ServerId,
    pub channel: ChannelId,
}

impl ComposerTarget for ChannelTarget {
    fn variant(&self) -> Variant {
        Variant::Channel
    }

    fn draft_key(&self) -> DraftKey {
        DraftKey::Channel(self.server, self.channel.clone())
    }

    fn server(&self) -> Option<ServerId> {
        Some(self.server)
    }

    fn slice(&self, cx: &App) -> Option<Entity<MessageSlice>> {
        model(cx)?.chat.channels.read(cx).get(self.server, &self.channel).cloned()
    }

    fn placeholder(&self, gate: &Gate, _: &App) -> SharedString {
        format!("Message #{}", gate.name).into()
    }

    fn gate(&self, cx: &App) -> Gate {
        let Some(model) = model(cx) else { return Gate::default() };
        let community = model.community(self.server, cx);
        let channel = community.as_ref().and_then(|c| c.channels.read(cx).get(&self.channel).cloned());
        let me = model.auth.read(cx).uid;
        let timed_out_until = community.as_ref().and_then(|c| c.roster.read(cx).member(me).map(|m| m.timed_out_until)).unwrap_or(0);
        let is_owner = model.is_owner(self.server, cx);
        Gate {
            name: channel.as_ref().map_or_else(|| self.channel.clone(), |c| c.name.clone()),
            can_send: model.channel_permission(self.server, &self.channel, perm::SEND_MESSAGES, cx),
            timed_out_until,
            can_attach: model.channel_permission(self.server, &self.channel, perm::ATTACH_FILES, cx),
            slowmode: decibell_model::chat::slowmode::interval_for(channel.as_ref(), is_owner),
        }
    }

    fn send(&self, text: &str, reply_to: i64, cx: &mut App) -> SendOutcome {
        actions::chat::send_message(self.server, self.channel.clone(), text, reply_to, cx)
    }

    fn send_gif(&self, url: &str, reply_to: i64, cx: &mut App) -> SendOutcome {
        actions::chat::send_gif(self.server, self.channel.clone(), url, reply_to, cx)
    }

    fn set_draft(&self, text: SharedString, cx: &mut App) {
        actions::chat::set_draft(self.server, self.channel.clone(), text, cx);
    }

    fn latest_own_editable(&self, cx: &App) -> Option<i64> {
        self.slice(cx).and_then(|slice| latest_editable(slice.read(cx)))
    }

    fn attach(&self, source: UploadSource, cx: &mut App) {
        actions::chat::attach_file(self.server, self.channel.clone(), source, cx);
    }

    fn slowmode_key(&self) -> Option<SliceKey> {
        Some((self.server, self.channel.clone()))
    }

    fn watch(&self, cx: &mut Context<Composer>) -> Vec<Subscription> {
        let Some(model) = model(cx) else { return Vec::new() };
        let mut subscriptions = Vec::new();
        if let Some(community) = model.community(self.server, cx) {
            // Permissions, the name, slowmode: channel edits. The timeout: the roster (every
            // delta notifies it; `refresh_gate` compares before drawing). Ownership: the meta.
            subscriptions.push(cx.observe(&community.channels, |this, _, cx| this.refresh_gate(cx)));
            subscriptions.push(cx.observe(&community.roster, |this, _, cx| this.refresh_gate(cx)));
            subscriptions.push(cx.observe(&community.meta, |this, _, cx| this.refresh_gate(cx)));
        }
        // A community that appears later (the composer of a channel opened while connecting).
        subscriptions.push(cx.observe(&model.communities, |this, _, cx| this.refresh_gate(cx)));
        subscriptions
    }
}

/// A DM conversation.
pub struct DmTarget {
    pub peer: Uid,
}

impl ComposerTarget for DmTarget {
    fn variant(&self) -> Variant {
        Variant::Dm
    }

    fn draft_key(&self) -> DraftKey {
        DraftKey::Dm(self.peer)
    }

    fn server(&self) -> Option<ServerId> {
        None
    }

    fn slice(&self, cx: &App) -> Option<Entity<MessageSlice>> {
        model(cx)?.dm.list.read(cx).slice(self.peer).cloned()
    }

    fn placeholder(&self, _: &Gate, cx: &App) -> SharedString {
        let name = model(cx).map(|m| m.name_of(self.peer, &SharedString::default(), cx)).unwrap_or_default();
        let name = if name.is_empty() { SharedString::new_static("Unknown user") } else { name };
        format!("Message @{name}").into()
    }

    fn gate(&self, _: &App) -> Gate {
        Gate { name: SharedString::default(), can_send: true, timed_out_until: 0, can_attach: false, slowmode: 0 }
    }

    fn send(&self, text: &str, reply_to: i64, cx: &mut App) -> SendOutcome {
        actions::dm::send_dm(self.peer, text, reply_to, cx)
    }

    fn send_gif(&self, url: &str, reply_to: i64, cx: &mut App) -> SendOutcome {
        actions::dm::send_gif(self.peer, url, reply_to, cx)
    }

    fn set_draft(&self, text: SharedString, cx: &mut App) {
        actions::dm::set_draft(self.peer, text, cx);
    }

    fn latest_own_editable(&self, cx: &App) -> Option<i64> {
        self.slice(cx).and_then(|slice| latest_editable(slice.read(cx)))
    }

    fn send_error(&self, cx: &App) -> Option<SharedString> {
        let model = model(cx)?;
        let list = model.dm.list.read(cx);
        list.send_error.as_ref().filter(|(peer, _)| *peer == self.peer).map(|(_, error)| error.clone())
    }

    fn sending(&self, cx: &App) -> bool {
        model(cx).is_some_and(|m| m.dm.list.read(cx).sending(self.peer))
    }

    fn watch(&self, cx: &mut Context<Composer>) -> Vec<Subscription> {
        let Some(model) = model(cx) else { return Vec::new() };
        vec![
            cx.observe(&model.dm.list, |this, _, cx| this.refresh_gate(cx)),
            cx.subscribe(&model.users, |this, _, _: &decibell_model::stores::users::NamesChanged, cx| this.refresh_gate(cx)),
        ]
    }
}

/// The parent of a reply as it is now (`messagesById.get(replyingTo.id) ?? replyingTo`).
pub fn reply_parent(target: &dyn ComposerTarget, kept: &Arc<Message>, cx: &App) -> Arc<Message> {
    target.slice(cx).and_then(|slice| slice.read(cx).find(kept.id).cloned()).unwrap_or_else(|| kept.clone())
}
