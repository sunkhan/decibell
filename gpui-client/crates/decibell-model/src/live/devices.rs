//! The account's devices and the password change (`stores/devicesStore.ts`,
//! `features/settings/tabs/DevicesTab.tsx`, `ChangePasswordModal.tsx`,
//! `features/settings/deviceLabels.ts`; spec `2026-10-19-devices-and-password-design.md`).
//!
//! The renderer kept only a revision here (bumped by `devices_changed`) and the list in
//! the tab's own state; this store holds the list too, so the tab is a plain view of it,
//! and the sign-out and change-password flows' busy / error / confirming states. The
//! "last active 5 min ago" ticker is the open tab's own (a 60 s timer while it is shown).
//!
//! **Rhythm**: a load per open tab and per `devices_changed`, a flow step per click. Only
//! Settings → Devices and the change-password dialog observe it.

use decibell_core::api::devices::DeviceEntry;
use decibell_core::events::DevicePayload;
use gpui::{Context, SharedString};

pub const MIN_PASSWORD_CHARS: usize = 8;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Device {
    pub id: i64,
    /// The OS name it reported (never the hostname).
    pub name: SharedString,
    /// "linux" | "windows" | "macos".
    pub platform: SharedString,
    pub app_version: SharedString,
    pub address: SharedString,
    pub signed_in_at: i64,
    pub last_active_at: i64,
    pub online: bool,
    /// This device.
    pub current: bool,
}

impl From<DeviceEntry> for Device {
    fn from(d: DeviceEntry) -> Self {
        Device {
            id: d.id,
            name: d.name.into(),
            platform: d.platform.into(),
            app_version: d.app_version.into(),
            address: d.address.into(),
            signed_in_at: d.signed_in_at,
            last_active_at: d.last_active_at,
            online: d.online,
            current: d.current,
        }
    }
}

impl From<&DevicePayload> for Device {
    fn from(d: &DevicePayload) -> Self {
        Device {
            id: d.id,
            name: d.name.clone().into(),
            platform: d.platform.clone().into(),
            app_version: d.app_version.clone().into(),
            address: d.address.clone().into(),
            signed_in_at: d.signed_in_at,
            last_active_at: d.last_active_at,
            online: d.online,
            current: d.current,
        }
    }
}

pub fn platform_label(platform: &str) -> &'static str {
    match platform {
        "linux" => "Linux",
        "windows" => "Windows",
        "macos" => "macOS",
        _ => "Unknown system",
    }
}

/// What the list and the new sign-in notice call a device: its OS name, else its platform.
pub fn device_title(name: &str, platform: &str) -> SharedString {
    if name.is_empty() { platform_label(platform).into() } else { name.to_string().into() }
}

/// The sign-out confirmation the tab shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Confirming {
    One(i64),
    AllOthers { count: usize },
}

/// The change-password dialog's state while open.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PasswordFlow {
    pub busy: bool,
    pub error: Option<SharedString>,
}

/// Check the three fields before anything is sent.
pub fn validate_password_change(current: &str, new: &str, repeat: &str) -> Result<(), SharedString> {
    if current.is_empty() {
        return Err("Enter your current password.".into());
    }
    // JS `length`: UTF-16 units.
    if new.encode_utf16().count() < MIN_PASSWORD_CHARS {
        return Err(format!("Your new password must be at least {MIN_PASSWORD_CHARS} characters.").into());
    }
    if new != repeat {
        return Err("The new passwords don't match.".into());
    }
    if new == current {
        return Err("That's already your password.".into());
    }
    Ok(())
}

#[derive(Default)]
pub struct Devices {
    /// Bumped by `devices_changed`: an open tab reloads.
    pub revision: u64,
    /// None until loaded.
    pub list: Option<Vec<Device>>,
    pub error: Option<SharedString>,
    pub busy: bool,
    pub confirming: Option<Confirming>,
    pub password: Option<PasswordFlow>,
    /// The tab is open (a `devices_changed` then reloads at once).
    pub(crate) watching: usize,
}

impl Devices {
    pub fn current(&self) -> Option<&Device> {
        self.list.as_ref()?.iter().find(|d| d.current)
    }

    pub fn others(&self) -> impl Iterator<Item = &Device> {
        self.list.iter().flatten().filter(|d| !d.current)
    }

    pub(crate) fn bump(&mut self, cx: &mut Context<Self>) {
        self.revision += 1;
        cx.notify();
    }

    /// The loaded list, or why it could not be loaded (the core's answer; screens seed it).
    pub fn set_list(&mut self, result: Result<Vec<Device>, SharedString>, cx: &mut Context<Self>) {
        match result {
            Ok(list) => {
                self.list = Some(list);
                self.error = None;
            }
            Err(e) => self.error = Some(e),
        }
        cx.notify();
    }

    pub fn confirm(&mut self, confirming: Option<Confirming>, cx: &mut Context<Self>) {
        if self.confirming != confirming {
            self.confirming = confirming;
            cx.notify();
        }
    }

    pub(crate) fn set_busy(&mut self, busy: bool, cx: &mut Context<Self>) {
        if self.busy != busy {
            self.busy = busy;
            cx.notify();
        }
    }

    pub fn open_password(&mut self, cx: &mut Context<Self>) {
        self.password = Some(PasswordFlow::default());
        cx.notify();
    }

    pub fn close_password(&mut self, cx: &mut Context<Self>) {
        if self.password.as_ref().is_some_and(|p| !p.busy) {
            self.password = None;
            cx.notify();
        }
    }

    pub(crate) fn set_password(&mut self, flow: Option<PasswordFlow>, cx: &mut Context<Self>) {
        if self.password != flow {
            self.password = flow;
            cx.notify();
        }
    }

    pub(crate) fn reset(&mut self, cx: &mut Context<Self>) {
        let revision = self.revision;
        *self = Devices { revision, ..Default::default() };
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn titles_and_password_checks() {
        assert_eq!(device_title("", "macos").as_ref(), "macOS");
        assert_eq!(device_title("Fedora Linux 41", "linux").as_ref(), "Fedora Linux 41");
        assert_eq!(device_title("", "beos").as_ref(), "Unknown system");
        assert_eq!(validate_password_change("", "x", "x").unwrap_err().as_ref(), "Enter your current password.");
        assert_eq!(
            validate_password_change("old", "short", "short").unwrap_err().as_ref(),
            "Your new password must be at least 8 characters."
        );
        assert_eq!(validate_password_change("old", "longer-one", "longer-two").unwrap_err().as_ref(), "The new passwords don't match.");
        assert_eq!(validate_password_change("same-pass", "same-pass", "same-pass").unwrap_err().as_ref(), "That's already your password.");
        assert!(validate_password_change("old-pass", "new-pass!", "new-pass!").is_ok());
    }
}
