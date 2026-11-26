//! The seams between the two halves of the model that were written apart: the chat half
//! ("M2": `chat`, `dm`) and the live / people half ("M3": `live`). What one emits and the
//! other acts on, wired once by `Model::init` after both exist (`install`). Each row names
//! the renderer code it mirrors.
//!
//! | From | To | Renderer |
//! |---|---|---|
//! | `live.calls` emits [`ShowConversation`] (we answered a call) | the caller's DM opens (`actions::dm::open_dm`) | `callActions.ts` `acceptCall`: `setActiveDmUser(inc.fromUid)` |
//! | `live.e2ee` emits `E2eeEvent::BecameReady` (unlock / setup) | every DM page reloads (`events::dm::reload_after_unlock`) | `useE2eeEvents.ts` `reloadDms` |
//!
//! Elsewhere, not subscriptions: `live::window_title` reads the DM list itself, the
//! router's `connection_lost` arm ends DM history waits (`events::dm::connection_lost`).
//!
//! Every subscription here lives as long as the model (the app's lifetime). None holds a
//! timer.

use gpui::App;

use crate::Model;
use crate::actions::calls::ShowConversation;
use crate::live::e2ee::E2eeEvent;

pub fn install(model: &Model, cx: &mut App) {
    // A call we answered lands on the caller's DM, where the call panel is.
    cx.subscribe(&model.live.calls, {
        let model = model.clone();
        move |_, ShowConversation(peer): &ShowConversation, cx| {
            let fallback = model.dm.list.read(cx).get(*peer).map(|c| c.username.clone()).unwrap_or_default();
            let name = model.name_of(*peer, &fallback, cx);
            crate::actions::dm::open_dm(*peer, name, cx);
        }
    })
    .detach();

    // Keys became usable: every sealed DM that arrived before holds a placeholder.
    cx.subscribe(&model.live.e2ee, {
        let model = model.clone();
        move |_, event: &E2eeEvent, cx| {
            if *event == E2eeEvent::BecameReady {
                crate::events::dm::reload_after_unlock(&model, cx);
            }
        }
    })
    .detach();
}

#[cfg(test)]
mod tests;
