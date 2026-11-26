//! The state model of the GPUI client: the layer between the shared core
//! (`decibell_core`: connections, protocol, media, crypto) and the views. What the
//! Electron renderer does with zustand stores and `use*Events` hooks happens here with
//! GPUI entities holding plain Rust state, fed by typed core events.
//!
//! `gpui-client/docs/model.md` is the contract: how the core starts, how events flow,
//! which views re-render when a store changes, and how to add a store, route an event,
//! call the API, persist a preference or write a test against the local stack.
//!
//! ```ignore
//! // In the app, once, before the first window:
//! let model = Model::init(cx, core::default_user_data_dir().ok());
//! Model::start_core(cx, CoreOptions { user_data_dir: None, app_version, local_key: Box::new(KeychainKey) }).detach();
//! // …after `Application::run` returns:
//! decibell_model::core::shutdown(Duration::from_secs(3));
//! ```

pub mod actions;
pub mod call;
pub mod core;
pub mod error;
pub mod events;
pub mod ids;
pub mod invite_link;
pub mod local_key;
pub mod pump;
pub mod save_echo;
pub mod stores;

use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use anyhow::Result;
use gpui::{App, AppContext as _, Entity, Global, SharedString, Task};

pub use crate::core::CoreOptions;
pub use crate::error::{CertMismatch, UiError};
pub use crate::ids::{ChannelId, ServerId, Uid};
use crate::pump::{EventQueue, ModelSink, PumpStats};
use crate::stores::auth::AuthStore;
use crate::stores::bus::Answers;
use crate::stores::community::{Communities, Community};
use crate::stores::connections::Connections;
use crate::stores::invites::InviteResolve;
use crate::stores::layout::Layout;
use crate::stores::navigation::Navigation;
use crate::stores::overlays::Overlays;
use crate::stores::permissions::{compute_effective_permissions, has_bits};
use crate::stores::servers::ServersStore;
use crate::stores::settings::{Settings, SettingsEvent};
use crate::stores::toasts::Toasts;
use crate::stores::update::UpdateStore;
use crate::stores::users::UsersStore;

/// Every store, as a GPUI global: `Model::global(cx)` from anywhere on the UI thread.
/// Cheap to clone (entity handles). A feature that adds a store adds a field here and
/// creates it in [`Model::build`].
#[derive(Clone)]
pub struct Model {
    pub auth: Entity<AuthStore>,
    pub users: Entity<UsersStore>,
    pub nav: Entity<Navigation>,
    pub overlays: Entity<Overlays>,
    pub settings: Entity<Settings>,
    pub layout: Entity<Layout>,
    pub servers: Entity<ServersStore>,
    pub connections: Entity<Connections>,
    pub communities: Entity<Communities>,
    pub toasts: Entity<Toasts>,
    pub invites: Entity<InviteResolve>,
    pub update: Entity<UpdateStore>,
    pub answers: Entity<Answers>,
    sink: Arc<ModelSink>,
    _pump: Rc<Task<()>>,
}

impl Global for Model {}

impl Model {
    /// Build the stores and the event pump and install them as the global. `user_data_dir`
    /// is where `ui.json` lives (`None`: nothing is persisted). Synchronous and cheap: the
    /// first window can be built right after, before the core has started.
    pub fn init(cx: &mut App, user_data_dir: Option<PathBuf>) -> Model {
        let model = Model::build(cx, user_data_dir);
        cx.set_global(model.clone());
        // Quit: what is still inside a debounce is written now.
        cx.on_app_quit(|cx| {
            Model::flush(cx);
            async {}
        })
        .detach();
        model
    }

    fn build(cx: &mut App, user_data_dir: Option<PathBuf>) -> Model {
        let (queue, woken) = EventQueue::new();
        let sink = Arc::new(ModelSink::new(queue.clone()));
        let pump = pump::spawn_pump(queue, woken, cx, |batch, cx| {
            let Some(model) = cx.try_global::<Model>().cloned() else {
                batch.clear();
                return;
            };
            for item in batch.drain(..) {
                events::apply(&model, item, cx);
            }
        });
        let settings = cx.new(|_| Settings::new(actions::settings::save_blob));
        let layout = cx.new(|_| Layout::new(user_data_dir));
        // Keep the boot mirror of the appearance in step (the next start opens the
        // window with it, before the core has read config.json).
        cx.subscribe(&settings, {
            let layout = layout.clone();
            move |settings, _: &SettingsEvent, cx| {
                let s = settings.read(cx);
                let (theme, text, row) = (s.theme().as_str(), s.text_size_px(), s.row_scale());
                layout.update(cx, |l, cx| l.mirror_appearance(theme, text, row, cx));
            }
        })
        .detach();
        Model {
            auth: cx.new(|_| AuthStore::default()),
            users: cx.new(|_| UsersStore::default()),
            nav: cx.new(|_| Navigation::default()),
            overlays: cx.new(|_| Overlays::default()),
            settings,
            layout,
            servers: cx.new(|_| ServersStore::default()),
            connections: cx.new(|_| Connections::default()),
            communities: cx.new(|_| Communities::new(Rc::new(actions::servers::send_member_request))),
            toasts: cx.new(|_| Toasts::default()),
            invites: cx.new(|_| InviteResolve::new(Rc::new(actions::servers::resolve_invite))),
            update: cx.new(|_| UpdateStore::default()),
            answers: cx.new(|_| Answers),
            sink,
            _pump: Rc::new(pump),
        }
    }

    pub fn global(cx: &App) -> &Model {
        cx.global::<Model>()
    }

    /// Start the core, then do what the renderer does at boot (`loadSettings.ts`): read
    /// `config.json` into [`Settings`], and sign in with the saved sign-in if there is
    /// one. Resolves once that has been started; `AuthStore::booted` flips with it.
    pub fn start_core(cx: &mut App, options: CoreOptions) -> Task<Result<()>> {
        let model = Model::global(cx).clone();
        let started = core::start(options, model.sink.clone(), cx);
        cx.spawn(async move |cx| {
            let outcome = started.await;
            if let Err(e) = &outcome {
                log::error!("[core] start failed: {e:#}");
            }
            actions::auth::boot(&model, outcome.is_ok(), cx).await;
            outcome
        })
    }

    /// Write what is still inside a debounce (the UI preferences file, a pending
    /// settings save). Called on quit; safe to call any time.
    pub fn flush(cx: &mut App) {
        let Some(model) = cx.try_global::<Model>().cloned() else { return };
        model.layout.update(cx, |layout, _| layout.flush());
        model.settings.update(cx, |settings, _| settings.flush());
    }

    /// Queue counters of the event pump (docs/model.md §3).
    pub fn pump_stats(&self) -> PumpStats {
        self.sink.queue().stats()
    }

    /// The sink the core was (or will be) given: tests push events through its queue.
    pub fn sink(&self) -> &Arc<ModelSink> {
        &self.sink
    }

    // ── Reads that span stores ──

    pub fn community(&self, server: ServerId, cx: &App) -> Option<Community> {
        self.communities.read(cx).get(server).cloned()
    }

    /// The name shown for `uid`: the current name the servers sent, else `fallback` (the
    /// name the caller's record carries), else "Unknown user" is the caller's to draw.
    pub fn name_of(&self, uid: Uid, fallback: &SharedString, cx: &App) -> SharedString {
        self.users.read(cx).name_of(uid, fallback)
    }

    /// The name shown for `uid` within a community (`useDisplayName`): the member's
    /// server nickname when set, otherwise their current name. `server` none (DMs, no
    /// community context) skips the nickname. A member the roster hasn't loaded shows
    /// their name until a lookup brings the row (`Roster::want`).
    pub fn display_name(&self, server: Option<ServerId>, uid: Uid, name: &SharedString, cx: &App) -> SharedString {
        if let Some(community) = server.and_then(|id| self.communities.read(cx).get(id))
            && let Some(nickname) = community.roster.read(cx).nickname(uid)
        {
            return nickname.clone();
        }
        self.name_of(uid, name, cx)
    }

    /// The local user owns `server`.
    pub fn is_owner(&self, server: ServerId, cx: &App) -> bool {
        let me = self.auth.read(cx).uid;
        me != 0
            && self
                .communities
                .read(cx)
                .get(server)
                .is_some_and(|c| c.meta.read(cx).owner == me)
    }

    /// True when the local user holds `perm` (every bit of it) server-wide in `server`
    /// (`usePermission`). The owner always passes. A community that never sent a role
    /// list falls back to owner-only gating, matching its server-side behaviour.
    pub fn has_permission(&self, server: ServerId, perm: u64, cx: &App) -> bool {
        let me = self.auth.read(cx).uid;
        if me == 0 {
            return false;
        }
        let Some(community) = self.communities.read(cx).get(server) else {
            return false;
        };
        if community.meta.read(cx).owner == me {
            return true;
        }
        let roster = community.roster.read(cx);
        let role_ids: &[i64] = roster.member(me).map_or(&[], |m| &m.role_ids);
        has_bits(compute_effective_permissions(community.roles.read(cx).list(), role_ids), perm)
    }

    /// True when the local user holds `perm` in one channel, from the server-resolved
    /// `my_permissions` (`useChannelPermission`). The owner always passes.
    pub fn channel_permission(&self, server: ServerId, channel: &str, perm: u64, cx: &App) -> bool {
        let Some(community) = self.communities.read(cx).get(server) else {
            return false;
        };
        let me = self.auth.read(cx).uid;
        let is_owner = me != 0 && community.meta.read(cx).owner == me;
        community.channels.read(cx).permits(channel, perm, is_owner)
    }
}
