//! [`DmSource`]: the message list over one conversation of `model.dm` (`DmChatPanel`'s list
//! callbacks: paging both ways, jump to present / to a message, edit, delete).

use decibell_chat::{Jump, ListSource};
use decibell_model::chat::MessageSlice;
use decibell_model::{Model, ServerId, Uid, actions};
use gpui::{App, Entity, SharedString, Task};

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

    fn delete(&self, id: i64, cx: &mut App) {
        actions::dm::delete_dm(self.peer, id, cx);
    }
}
