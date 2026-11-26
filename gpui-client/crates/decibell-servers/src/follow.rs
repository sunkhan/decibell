//! Keeping an open dialog in step with one community's own entities (channels, meta, roles,
//! roster, moderation), subscribed while it shows that community and dropped when it stops:
//! a closed dialog follows nothing.

use decibell_model::{Model, ServerId};
use gpui::{Context, Subscription};

#[derive(Default)]
pub(crate) struct Follow {
    server: Option<ServerId>,
    _subscriptions: Vec<Subscription>,
}

impl Follow {
    /// Follow `server` (or nothing): every change of its entities notifies the view. Returns
    /// whether the community followed changed.
    pub fn set<V: 'static>(&mut self, server: Option<ServerId>, cx: &mut Context<V>) -> bool {
        if self.server == server && (server.is_none() || !self._subscriptions.is_empty()) {
            return false;
        }
        self.server = server;
        self._subscriptions.clear();
        let Some(community) = server.and_then(|s| Model::global(cx).community(s, cx)) else { return true };
        self._subscriptions = vec![
            cx.observe(&community.channels, |_, _, cx| cx.notify()),
            cx.observe(&community.meta, |_, _, cx| cx.notify()),
            cx.observe(&community.roles, |_, _, cx| cx.notify()),
            cx.observe(&community.roster, |_, _, cx| cx.notify()),
            cx.observe(&community.moderation, |_, _, cx| cx.notify()),
        ];
        true
    }
}
