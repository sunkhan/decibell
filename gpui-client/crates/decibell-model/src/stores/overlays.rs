//! What is open over the app: the modal, the anchored popups, and the notices that open
//! one (`uiStore`'s `activeModal`, profile popup, context menu, `authError`,
//! `certMismatch`, `membershipRevocationNotice`).

use std::rc::Rc;

use gpui::{App, Context, SharedString};

use crate::error::CertMismatch;
use crate::ids::{ChannelId, ServerId, Uid};

/// The renderer's modal ids (`uiStore.openModal(id)`), kept as they are so a ported view
/// greps to its original. A modal a later feature adds is just another id.
pub mod modal {
    pub const SETTINGS: &str = "settings";
    pub const SERVER_SETTINGS: &str = "server-settings";
    pub const CHANNEL_SETTINGS: &str = "channel-settings";
    pub const USER_PROFILE: &str = "user-profile";
    pub const CUSTOM_STATUS: &str = "custom-status";
    pub const DELETE_MESSAGE_CONFIRM: &str = "delete-message-confirm";
    pub const INVITE_FRIENDS: &str = "invite-friends";
    pub const INVITE_MANAGE: &str = "invite-manage";
    pub const LEAVE_SERVER_CONFIRM: &str = "leave-server-confirm";
    /// The E2EE prompts (`features/e2ee`): set up, unlock, change the passphrase, reset.
    pub const E2EE_SETUP: &str = "setup";
    pub const E2EE_UNLOCK: &str = "unlock";
    pub const E2EE_CHANGE: &str = "change";
    pub const E2EE_RESET: &str = "reset";
}

/// A point in window coordinates a popup opens at.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Anchor {
    pub x: f32,
    pub y: f32,
}

/// Whom an anchored popup (profile popup, user context menu) is for.
#[derive(Debug, Clone, PartialEq)]
pub struct UserPopup {
    pub uid: Uid,
    pub anchor: Anchor,
    /// The community it was opened in, so it can show the nickname and roles.
    pub server: Option<ServerId>,
}

/// A community refused our authentication (`community_auth_responded` without success).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthErrorNotice {
    pub server: ServerId,
    pub message: SharedString,
    pub error_code: SharedString,
}

/// What to run again once a certificate was re-trusted.
pub type Retry = Rc<dyn Fn(&mut App)>;

/// A TLS connection was refused because the server's certificate no longer matches the
/// pinned / central-reported fingerprint. Drives the re-trust prompt; `retry` re-runs
/// the action after a deliberate re-trust (`api::servers::trust_certificate`).
#[derive(Clone)]
pub struct CertMismatchNotice {
    pub cert: CertMismatch,
    pub retry: Option<Retry>,
}

/// We were kicked or banned (`membership_revoked`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MembershipRevocationNotice {
    pub server: ServerId,
    pub action: SharedString,
    pub reason: SharedString,
    pub actor: SharedString,
    /// Bans: unix seconds when the ban lifts (0 = permanent).
    pub expires_at: i64,
}

#[derive(Default)]
pub struct Overlays {
    pub active_modal: Option<SharedString>,
    /// Which channel the channel-settings modal targets.
    pub channel_settings_channel: Option<ChannelId>,
    /// The profile modal's person and the community it was opened from.
    pub user_profile_user: Option<Uid>,
    pub user_profile_server: Option<ServerId>,
    pub profile_popup: Option<UserPopup>,
    pub context_menu: Option<UserPopup>,
    pub auth_error: Option<AuthErrorNotice>,
    pub cert_mismatch: Option<CertMismatchNotice>,
    pub membership_revocation: Option<MembershipRevocationNotice>,
}

impl Overlays {
    pub fn open_modal(&mut self, id: &'static str, cx: &mut Context<Self>) {
        self.active_modal = Some(SharedString::new_static(id));
        cx.notify();
    }

    pub fn close_modal(&mut self, cx: &mut Context<Self>) {
        if self.active_modal.take().is_some() {
            cx.notify();
        }
    }

    pub fn is_open(&self, id: &str) -> bool {
        self.active_modal.as_deref() == Some(id)
    }

    pub fn open_channel_settings(&mut self, channel: ChannelId, cx: &mut Context<Self>) {
        self.active_modal = Some(SharedString::new_static(modal::CHANNEL_SETTINGS));
        self.channel_settings_channel = Some(channel);
        cx.notify();
    }

    pub fn open_user_profile(&mut self, uid: Uid, server: Option<ServerId>, cx: &mut Context<Self>) {
        self.active_modal = Some(SharedString::new_static(modal::USER_PROFILE));
        self.user_profile_user = Some(uid);
        self.user_profile_server = server;
        // The anchored popup is what opened us; it has no place under a modal.
        self.profile_popup = None;
        cx.notify();
    }

    pub fn open_profile_popup(&mut self, uid: Uid, anchor: Anchor, server: Option<ServerId>, cx: &mut Context<Self>) {
        self.profile_popup = Some(UserPopup { uid, anchor, server });
        cx.notify();
    }

    pub fn close_profile_popup(&mut self, cx: &mut Context<Self>) {
        if self.profile_popup.take().is_some() {
            cx.notify();
        }
    }

    pub fn open_context_menu(&mut self, uid: Uid, anchor: Anchor, server: Option<ServerId>, cx: &mut Context<Self>) {
        self.context_menu = Some(UserPopup { uid, anchor, server });
        cx.notify();
    }

    pub fn close_context_menu(&mut self, cx: &mut Context<Self>) {
        if self.context_menu.take().is_some() {
            cx.notify();
        }
    }

    pub fn set_auth_error(&mut self, notice: Option<AuthErrorNotice>, cx: &mut Context<Self>) {
        self.auth_error = notice;
        cx.notify();
    }

    pub fn set_cert_mismatch(&mut self, notice: Option<CertMismatchNotice>, cx: &mut Context<Self>) {
        self.cert_mismatch = notice;
        cx.notify();
    }

    pub fn set_membership_revocation(&mut self, notice: Option<MembershipRevocationNotice>, cx: &mut Context<Self>) {
        self.membership_revocation = notice;
        cx.notify();
    }

    /// Nothing open carries over to the next account (`logged_out`): the modal, the
    /// popup, the menu.
    pub fn close_all_for_logout(&mut self, cx: &mut Context<Self>) {
        self.active_modal = None;
        self.profile_popup = None;
        self.context_menu = None;
        cx.notify();
    }
}
