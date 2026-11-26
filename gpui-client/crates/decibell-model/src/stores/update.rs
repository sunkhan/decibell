//! The updater's state (`stores/updateStore.ts`): the shape only. Nothing feeds it until
//! the updater exists (the last phase); the About tab and the user panel read it.

use gpui::{Context, SharedString};

#[derive(Debug, Clone, PartialEq, Default)]
pub enum UpdateStatus {
    #[default]
    Idle,
    Checking,
    NotAvailable { checked_at: i64 },
    Available { version: SharedString },
    Downloading { pct: f32, version: SharedString },
    Downloaded { version: SharedString },
    Error { message: SharedString },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum UpdateMode {
    SelfUpdate,
    NotifyOnly,
    #[default]
    Disabled,
}

#[derive(Default)]
pub struct UpdateStore {
    pub status: UpdateStatus,
    pub mode: UpdateMode,
    pub current_version: SharedString,
}

impl UpdateStore {
    pub fn set(&mut self, status: UpdateStatus, mode: UpdateMode, current_version: SharedString, cx: &mut Context<Self>) {
        self.status = status;
        self.mode = mode;
        self.current_version = current_version;
        cx.notify();
    }
}
