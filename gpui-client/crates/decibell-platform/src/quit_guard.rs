//! Closing the window while files upload (`window.ts`' `confirmLosingUploads`, `index.ts`'
//! `before-quit`): an upload in flight would be lost with the message it belongs to, so closing asks
//! first, once per close ("Keep uploading" / "Quit anyway"); downloads need no question, they are
//! saved as paused and resume at the next start (`pauseAllForQuit`).
//!
//! The question is the kit's `ConfirmModal` in the window (Electron's was the OS's message box;
//! GPUI has none on Linux). It answers every way of closing: the title bar's close button
//! (`decibell_ui::CloseHandler`) and the desktop's (`Window::on_window_should_close`: Alt+F4, the
//! taskbar's Close). Quitting from code (the development control pipe) does not ask.

use decibell_model::Model;
use decibell_ui::{CloseHandler, ConfirmModal, ConfirmTone};
use gpui::{AnyWindowHandle, App, AppContext as _, ClickEvent, Context, Entity, IntoElement, Render, SharedString, Subscription, Window};

pub struct QuitGuard {
    window: AnyWindowHandle,
    /// The question is showing.
    open: bool,
    /// "Quit anyway" was chosen; reset when nothing uploads any more.
    confirmed: bool,
    /// Uploads that a close would lose, when the question opened.
    uploads: usize,
    _subscriptions: Vec<Subscription>,
}

impl QuitGuard {
    /// True when the window may close now; otherwise the question opens.
    pub fn allow_close(&mut self, cx: &mut Context<Self>) -> bool {
        let uploads = uploads_in_flight(cx);
        if uploads == 0 || self.confirmed {
            return true;
        }
        self.uploads = uploads;
        if !self.open {
            self.open = true;
            crate::marker(format_args!("quit-guard asks ({uploads} uploading)"));
            cx.notify();
        }
        false
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    pub(crate) fn keep_uploading(&mut self, cx: &mut Context<Self>) {
        self.open = false;
        cx.notify();
    }

    fn quit_anyway(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open = false;
        self.confirmed = true;
        cx.notify();
        if window.window_handle() == self.window {
            window.remove_window();
        }
    }
}

fn uploads_in_flight(cx: &App) -> usize {
    cx.try_global::<Model>().map_or(0, |m| m.live.transfers.read(cx).uploads_in_flight())
}

/// Installs the guard on the main window; draw the returned view over the window's content.
pub fn install(window: AnyWindowHandle, cx: &mut App) -> Entity<QuitGuard> {
    let guard = cx.new(|cx: &mut Context<QuitGuard>| {
        let mut subscriptions = Vec::new();
        if let Some(model) = cx.try_global::<Model>().cloned() {
            subscriptions.push(cx.observe(&model.live.transfers, |this: &mut QuitGuard, transfers, cx| {
                if this.confirmed && transfers.read(cx).uploads_in_flight() == 0 {
                    this.confirmed = false;
                }
            }));
            // Running downloads are saved as paused before anything else at quit can stall.
            subscriptions.push(cx.on_app_quit(move |_, cx| {
                if let Some(control) = model.live.transfers.read(cx).control() {
                    control.pause_all_for_quit();
                    crate::marker(format_args!("quit: downloads paused"));
                }
                async {}
            }));
        }
        QuitGuard { window, open: false, confirmed: false, uploads: 0, _subscriptions: subscriptions }
    });
    let for_button = guard.clone();
    cx.set_global(CloseHandler(std::rc::Rc::new(move |window, cx| {
        if for_button.update(cx, |g, cx| g.allow_close(cx)) {
            window.remove_window();
        }
    })));
    let for_desktop = guard.clone();
    let _ = window.update(cx, |_, window, cx| {
        window.on_window_should_close(cx, move |_, cx| for_desktop.update(cx, |g, cx| g.allow_close(cx)));
    });
    guard
}

impl Render for QuitGuard {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let title: SharedString = if self.uploads <= 1 {
            "A file is still uploading".into()
        } else {
            format!("{} files are still uploading", self.uploads).into()
        };
        ConfirmModal::new("quit-guard", self.open, title, "Quit anyway")
            .message("Quitting now cancels the upload, and the message it belongs to won't be sent.")
            .tone(ConfirmTone::Danger)
            .cancel_label("Keep uploading")
            .default_cancel()
            .on_confirm(cx.listener(|this, _: &ClickEvent, window, cx| this.quit_anyway(window, cx)))
            .on_cancel(cx.listener(|this, _: &ClickEvent, _, cx| this.keep_uploading(cx)))
    }
}
