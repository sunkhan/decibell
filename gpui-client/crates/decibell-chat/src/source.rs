//! What a [`MessageList`](crate::MessageList) shows and what it may ask for: a slice of the
//! model plus a few capabilities. A server text channel and a DM conversation differ only here.

use decibell_model::chat::MessageSlice;
use decibell_model::{ChannelId, Model, ServerId, actions};
use gpui::{App, Entity, Task};

/// How a jump to a message starts.
pub enum Jump {
    /// The row is in the slice: land now.
    Loaded,
    /// A window around it is being fetched: land on the commit that holds it. The task ends
    /// with `Some(false)` / `None` when it never came (deleted, or no answer).
    Fetching(Task<Option<bool>>),
    /// A window around it is on its way already.
    Waiting,
    /// Nothing to do (no such channel).
    Refused,
}

/// See the module doc.
pub trait ListSource: 'static {
    fn slice(&self, cx: &mut App) -> Entity<MessageSlice>;
    /// The community, for nicknames and the roster; `None` in a DM.
    fn server(&self) -> Option<ServerId>;
    /// The list neared its top / its bottom while windowed. `Some(false)` from the task: the
    /// request got no answer (look at the edges again later).
    fn load_older(&self, cx: &mut App) -> Option<Task<Option<bool>>>;
    fn load_newer(&self, cx: &mut App) -> Option<Task<Option<bool>>>;
    /// Drop the window and reload the newest page.
    fn jump_to_present(&self, cx: &mut App);
    fn jump_to_message(&self, id: i64, cx: &mut App) -> Jump;
    fn jump_landed(&self, cx: &mut App);
    /// The local user may delete other people's messages here.
    fn can_delete_others(&self, cx: &App) -> bool;
    fn edit(&self, id: i64, content: &str, cx: &mut App);
    fn delete(&self, id: i64, cx: &mut App);
}

/// A server text channel (`ChatPanel`).
pub struct ChannelSource {
    pub server: ServerId,
    pub channel: ChannelId,
}

impl ListSource for ChannelSource {
    fn slice(&self, cx: &mut App) -> Entity<MessageSlice> {
        let model = Model::global(cx).clone();
        let own = model.auth.read(cx).uid;
        let encrypted = model
            .community(self.server, cx)
            .and_then(|community| community.channels.read(cx).get(&self.channel).map(|channel| channel.encrypted))
            .unwrap_or(false);
        model.chat.channels.update(cx, |cache, cx| cache.ensure(self.server, &self.channel, own, encrypted, cx))
    }

    fn server(&self) -> Option<ServerId> {
        Some(self.server)
    }

    fn load_older(&self, cx: &mut App) -> Option<Task<Option<bool>>> {
        actions::chat::load_older(self.server, self.channel.clone(), cx)
    }

    fn load_newer(&self, cx: &mut App) -> Option<Task<Option<bool>>> {
        actions::chat::load_newer(self.server, self.channel.clone(), cx)
    }

    fn jump_to_present(&self, cx: &mut App) {
        actions::chat::jump_to_present(self.server, self.channel.clone(), cx);
    }

    fn jump_to_message(&self, id: i64, cx: &mut App) -> Jump {
        match actions::chat::jump_to_message(self.server, self.channel.clone(), id, cx) {
            Some(actions::chat::JumpStart::Loaded) => Jump::Loaded,
            Some(actions::chat::JumpStart::Fetching(task)) => Jump::Fetching(task),
            Some(actions::chat::JumpStart::AlreadyFetching) => Jump::Waiting,
            None => Jump::Refused,
        }
    }

    fn jump_landed(&self, cx: &mut App) {
        actions::chat::jump_landed(self.server, &self.channel, cx);
    }

    fn can_delete_others(&self, cx: &App) -> bool {
        actions::chat::can_delete_others(self.server, cx)
    }

    fn edit(&self, id: i64, content: &str, cx: &mut App) {
        actions::chat::edit_message(self.server, self.channel.clone(), id, content, cx);
    }

    fn delete(&self, id: i64, cx: &mut App) {
        actions::chat::delete_message(self.server, self.channel.clone(), id, cx);
    }
}
