//! [`DmSource`]: the message list over one conversation of `model.dm` (`DmChatPanel`'s list
//! callbacks: paging both ways, jump to present / to a message, edit, delete).

use decibell_chat::{Jump, ListSource, RowNotice};
use decibell_model::chat::{Message, MessageSlice};
use decibell_model::{Model, ServerId, Uid, actions};
use gpui::{App, Entity, SharedString, Task};

/// Central's legacy refusal strings (`DmChatPanel`'s `ERROR_MESSAGES`).
pub const LEGACY_OFFLINE: &str = "This user is currently offline. Your message could not be delivered.";
pub const LEGACY_FRIENDS_ONLY: &str = "This user only accepts direct messages from users in their friends list.";

pub struct DmSource {
    pub peer: Uid,
}

impl ListSource for DmSource {
    fn slice(&self, cx: &mut App) -> Entity<MessageSlice> {
        let model = Model::global(cx).clone();
        if let Some(slice) = model.dm.list.read(cx).slice(self.peer) {
            return slice.clone();
        }
        let name = model.name_of(self.peer, &SharedString::default(), cx);
        model.dm.list.update(cx, |list, cx| list.ensure(self.peer, name, cx).slice.clone())
    }

    fn server(&self) -> Option<ServerId> {
        None
    }

    fn load_older(&self, cx: &mut App) -> Option<Task<Option<bool>>> {
        actions::dm::load_older(self.peer, cx)
    }

    fn load_newer(&self, cx: &mut App) -> Option<Task<Option<bool>>> {
        actions::dm::load_newer(self.peer, cx)
    }

    fn jump_to_present(&self, cx: &mut App) {
        actions::dm::jump_to_present(self.peer, cx);
    }

    fn jump_to_message(&self, id: i64, cx: &mut App) -> Jump {
        match actions::dm::jump_to_message(self.peer, id, cx) {
            Some(actions::chat::JumpStart::Loaded) => Jump::Loaded,
            Some(actions::chat::JumpStart::Fetching(task)) => Jump::Fetching(task),
            Some(actions::chat::JumpStart::AlreadyFetching) => Jump::Waiting,
            None => Jump::Refused,
        }
    }

    fn jump_landed(&self, cx: &mut App) {
        actions::dm::jump_landed(self.peer, cx);
    }

    /// Never in a DM: only our own messages are deletable.
    fn can_delete_others(&self, _: &App) -> bool {
        false
    }

    fn edit(&self, id: i64, content: &str, cx: &mut App) {
        actions::dm::edit_dm(self.peer, id, content, cx);
    }

    /// Central's two legacy refusal strings, echoed back as a DM from us to us, are drawn as an
    /// `ErrorCard` row (`DmChatPanel`'s `ERROR_MESSAGES`). Central now refuses with the sender's
    /// nonce instead (a toast, the bubble withdrawn), so only a refusal whose bubble was already
    /// gone (sent from another device, or after the echo watchdog) lands as such a row.
    fn notice(&self, message: &Message, cx: &App) -> Option<RowNotice> {
        let model = Model::global(cx);
        if message.sender_uid != model.auth.read(cx).uid {
            return None;
        }
        match &*message.content {
            LEGACY_OFFLINE => {
                let fallback = model.dm.list.read(cx).get(self.peer).map(|c| c.username.clone()).unwrap_or_default();
                let peer = model.name_of(self.peer, &fallback, cx);
                Some(RowNotice {
                    lead: SharedString::new_static("User is offline."),
                    text: format!("Your message could not be delivered. It will be sent when {peer} comes back online.").into(),
                })
            }
            LEGACY_FRIENDS_ONLY => Some(RowNotice {
                lead: SharedString::new_static("Can't reach this user."),
                text: SharedString::new_static("They only accept direct messages from users in their friends list."),
            }),
            _ => None,
        }
    }

    fn delete(&self, id: i64, cx: &mut App) {
        actions::dm::delete_dm(self.peer, id, cx);
    }
}
