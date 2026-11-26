//! Toasts (`stores/toastStore.ts`). A toast's lifetime is a timer the store holds with
//! the toast: it exists only while the toast is on screen and goes with it.

use std::rc::Rc;
use std::time::Duration;

use gpui::{App, Context, SharedString, Task};

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Error,
    Warning,
    Info,
    Success,
}

/// A button on the toast; running it dismisses the toast.
#[derive(Clone)]
pub struct ToastAction {
    pub label: SharedString,
    pub run: Rc<dyn Fn(&mut App)>,
}

pub struct Toast {
    pub id: u64,
    pub severity: Severity,
    pub title: SharedString,
    pub body: Option<SharedString>,
    pub duration: Duration,
    pub action: Option<ToastAction>,
    _lifetime: Task<()>,
}

/// The default lifetime (`duration ?? 5000`).
pub const DEFAULT_DURATION: Duration = Duration::from_millis(5000);

#[derive(Default)]
pub struct Toasts {
    toasts: Vec<Toast>,
    next_id: u64,
}

impl Toasts {
    pub fn toasts(&self) -> &[Toast] {
        &self.toasts
    }

    /// Show a toast and return its id. An identical toast already on screen (same
    /// severity, title and body) is not stacked: its id is returned instead.
    pub fn push(
        &mut self,
        severity: Severity,
        title: impl Into<SharedString>,
        body: Option<SharedString>,
        duration: Option<Duration>,
        action: Option<ToastAction>,
        cx: &mut Context<Self>,
    ) -> u64 {
        let title = title.into();
        // `(t.body ?? "") === (input.body ?? "")`: no body and an empty one are the same.
        let same_body = |a: &Option<SharedString>, b: &Option<SharedString>| {
            a.as_deref().unwrap_or("") == b.as_deref().unwrap_or("")
        };
        if let Some(existing) = self
            .toasts
            .iter()
            .find(|t| t.severity == severity && t.title == title && same_body(&t.body, &body))
        {
            return existing.id;
        }
        self.next_id += 1;
        let id = self.next_id;
        let duration = duration.unwrap_or(DEFAULT_DURATION);
        let lifetime = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(duration).await;
            let _ = this.update(cx, |this, cx| this.dismiss(id, cx));
        });
        self.toasts.push(Toast {
            id,
            severity,
            title,
            body,
            duration,
            action,
            _lifetime: lifetime,
        });
        cx.notify();
        id
    }

    pub fn dismiss(&mut self, id: u64, cx: &mut Context<Self>) {
        let before = self.toasts.len();
        self.toasts.retain(|t| t.id != id);
        if self.toasts.len() != before {
            cx.notify();
        }
    }

    pub fn error(&mut self, title: impl Into<SharedString>, body: Option<SharedString>, cx: &mut Context<Self>) -> u64 {
        self.push(Severity::Error, title, body, None, None, cx)
    }

    pub fn warning(&mut self, title: impl Into<SharedString>, body: Option<SharedString>, cx: &mut Context<Self>) -> u64 {
        self.push(Severity::Warning, title, body, None, None, cx)
    }

    pub fn info(&mut self, title: impl Into<SharedString>, body: Option<SharedString>, cx: &mut Context<Self>) -> u64 {
        self.push(Severity::Info, title, body, None, None, cx)
    }

    pub fn success(&mut self, title: impl Into<SharedString>, body: Option<SharedString>, cx: &mut Context<Self>) -> u64 {
        self.push(Severity::Success, title, body, None, None, cx)
    }
}

#[cfg(test)]
mod tests {
    use gpui::{AppContext as _, TestAppContext};

    use super::*;

    #[gpui::test]
    fn identical_toasts_do_not_stack_and_a_toast_leaves_with_its_timer(cx: &mut TestAppContext) {
        let toasts = cx.new(|_| Toasts::default());
        let (a, b, c) = toasts.update(cx, |t, cx| {
            let a = t.error("Couldn't join server", Some("Banned".into()), cx);
            let b = t.error("Couldn't join server", Some("Banned".into()), cx);
            let c = t.push(Severity::Info, "Moved", None, Some(Duration::from_secs(20)), None, cx);
            // No body and an empty body are the same toast.
            assert_eq!(t.info("Moved", Some("".into()), cx), c);
            (a, b, c)
        });
        assert_eq!(a, b);
        assert_ne!(a, c);
        toasts.read_with(cx, |t, _| assert_eq!(t.toasts().len(), 2));

        cx.executor().advance_clock(Duration::from_millis(5001));
        cx.run_until_parked();
        toasts.read_with(cx, |t, _| {
            assert_eq!(t.toasts().len(), 1, "the 5 s toast is gone");
            assert_eq!(t.toasts()[0].id, c);
        });
        // Dismissed by hand: its timer goes with it and nothing fires later.
        toasts.update(cx, |t, cx| t.dismiss(c, cx));
        cx.executor().advance_clock(Duration::from_secs(30));
        cx.run_until_parked();
        toasts.read_with(cx, |t, _| assert!(t.toasts().is_empty()));
    }
}
