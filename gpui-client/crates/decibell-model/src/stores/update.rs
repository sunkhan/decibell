//! The updater's state (`stores/updateStore.ts`), fed by the app's updater (`decibell-app`
//! `updater.rs`); the About tab and the user panel read it.

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
    /// A downloaded version didn't start and the previous one was kept (the trial start,
    /// `decibell_release::apply`): About says so beside the status, whatever the checks report,
    /// until it has been shown once. Electron has no trial start, so no such line.
    pub failed_notice: Option<SharedString>,
}

impl UpdateStore {
    pub fn set(&mut self, status: UpdateStatus, mode: UpdateMode, current_version: SharedString, cx: &mut Context<Self>) {
        self.status = status;
        self.mode = mode;
        self.current_version = current_version;
        cx.notify();
    }

    pub fn set_failed_notice(&mut self, notice: Option<SharedString>, cx: &mut Context<Self>) {
        if self.failed_notice != notice {
            self.failed_notice = notice;
            cx.notify();
        }
    }
}
