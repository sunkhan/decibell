//! Who is signed in on this device, and the sign-in page's state (`stores/authStore.ts`).

use gpui::{Context, SharedString};

use crate::ids::Uid;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegisterResult {
    pub success: bool,
    pub message: SharedString,
}

#[derive(Default)]
pub struct AuthStore {
    pub username: Option<SharedString>,
    /// Our account uid: every "is this me?" check compares it. 0 while signed out.
    pub uid: Uid,
    /// Central accepts username changes (`LoginResponse.renames`).
    pub renames: bool,
    /// Central serves the device list and password changes (`LoginResponse.devices`).
    pub devices: bool,
    pub is_authenticated: bool,
    pub is_logging_in: bool,
    pub login_error: Option<SharedString>,
    /// Why the sign-in page is showing when it wasn't our own Log out (this device was
    /// signed out elsewhere, or the password changed). Cleared by the next sign-in attempt.
    pub login_notice: Option<SharedString>,
    pub is_registering: bool,
    pub register_result: Option<RegisterResult>,
    /// The account a saved sign-in is for (`load_config`); the password never leaves the
    /// core. Informational: the automatic sign-in at boot already used it.
    pub saved_username: Option<SharedString>,
    /// `load_config` has been applied (and the automatic sign-in started, if any): until
    /// then the root view shows neither the sign-in page nor the app.
    pub booted: bool,
}

impl AuthStore {
    pub fn login(&mut self, username: SharedString, renames: bool, uid: Uid, devices: bool, cx: &mut Context<Self>) {
        self.username = Some(username);
        self.uid = uid;
        self.renames = renames;
        self.devices = devices;
        self.is_authenticated = true;
        self.is_logging_in = false;
        self.login_error = None;
        self.login_notice = None;
        cx.notify();
    }

    /// As the renderer's `logout()`: `renames`, `devices`, `login_notice` and
    /// `is_registering` are left as they are.
    pub fn logout(&mut self, cx: &mut Context<Self>) {
        self.username = None;
        self.uid = 0;
        self.is_authenticated = false;
        self.is_logging_in = false;
        self.login_error = None;
        self.register_result = None;
        cx.notify();
    }

    pub fn set_logging_in(&mut self, on: bool, cx: &mut Context<Self>) {
        self.is_logging_in = on;
        self.login_error = None;
        self.login_notice = None;
        cx.notify();
    }

    pub fn set_login_error(&mut self, message: Option<SharedString>, cx: &mut Context<Self>) {
        self.login_error = message;
        self.is_logging_in = false;
        cx.notify();
    }

    pub fn set_login_notice(&mut self, message: Option<SharedString>, cx: &mut Context<Self>) {
        if self.login_notice != message {
            self.login_notice = message;
            cx.notify();
        }
    }

    pub fn set_registering(&mut self, on: bool, cx: &mut Context<Self>) {
        self.is_registering = on;
        self.register_result = None;
        cx.notify();
    }

    pub fn set_register_result(&mut self, result: Option<RegisterResult>, cx: &mut Context<Self>) {
        self.register_result = result;
        self.is_registering = false;
        cx.notify();
    }

    /// Our own rename (`user_renamed` with `self`).
    pub fn set_username(&mut self, username: SharedString, cx: &mut Context<Self>) {
        if self.username.as_ref() != Some(&username) {
            self.username = Some(username);
            cx.notify();
        }
    }

    pub fn set_booted(&mut self, saved_username: Option<SharedString>, cx: &mut Context<Self>) {
        self.saved_username = saved_username;
        self.booted = true;
        cx.notify();
    }

    /// "Is this me?"
    pub fn is_me(&self, uid: Uid) -> bool {
        self.uid != 0 && self.uid == uid
    }
}
