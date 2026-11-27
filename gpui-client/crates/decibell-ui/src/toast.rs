//! Toasts (`stores/toastStore.ts` + `components/ToastStack.tsx`): a store anyone can push to and
//! the stack the app draws at the top of the window.

use std::rc::Rc;
use std::time::Duration;

use gpui::{
    Animation, AnimationExt as _, App, AppContext as _, Context, ElementId, Entity, Global,
    IntoElement, RenderOnce, SharedString, Task, Window, div, prelude::*, px,
};

use crate::popover::{Placed, Placement, visible_window_bounds};
use crate::{ActiveTheme as _, IconButton, IconName, Label, LinkButton, motion};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToastSeverity {
    Error,
    Warning,
    Info,
    Success,
}

/// What a toast's action runs.
pub type ActionHandler = Rc<dyn Fn(&mut Window, &mut App) + 'static>;

/// A button on a toast; running it dismisses the toast.
#[derive(Clone)]
pub struct ToastAction {
    pub label: SharedString,
    pub run: ActionHandler,
}

/// What to show. `duration` defaults to 5 s.
#[derive(Clone)]
pub struct Toast {
    pub severity: ToastSeverity,
    pub title: SharedString,
    pub body: Option<SharedString>,
    pub duration: Duration,
    pub action: Option<ToastAction>,
}

impl Toast {
    fn new(severity: ToastSeverity, title: impl Into<SharedString>) -> Self {
        Self {
            severity,
            title: title.into(),
            body: None,
            duration: Duration::from_secs(5),
            action: None,
        }
    }

    pub fn error(title: impl Into<SharedString>) -> Self {
        Self::new(ToastSeverity::Error, title)
    }

    pub fn warning(title: impl Into<SharedString>) -> Self {
        Self::new(ToastSeverity::Warning, title)
    }

    pub fn info(title: impl Into<SharedString>) -> Self {
        Self::new(ToastSeverity::Info, title)
    }

    pub fn success(title: impl Into<SharedString>) -> Self {
        Self::new(ToastSeverity::Success, title)
    }

    pub fn body(mut self, body: impl Into<SharedString>) -> Self {
        self.body = Some(body.into());
        self
    }

    pub fn duration(mut self, duration: Duration) -> Self {
        self.duration = duration;
        self
    }

    pub fn action(mut self, label: impl Into<SharedString>, run: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.action = Some(ToastAction {
            label: label.into(),
            run: Rc::new(run),
        });
        self
    }
}

/// A shown toast's id.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ToastId(u64);

struct Shown {
    id: ToastId,
    toast: Toast,
    /// The dismiss timer: it exists only while the toast is shown (dropping it cancels it).
    _timer: Task<()>,
}

/// The toasts on screen. Push with [`Toasts::push`] from anywhere that has the app.
pub struct ToastStore {
    shown: Vec<Shown>,
    next: u64,
}

struct ToastsGlobal(Entity<ToastStore>);

impl Global for ToastsGlobal {}

/// The toast store's API (the renderer's `toast.error(…)` & co.).
///
/// ```ignore
/// Toasts::push(cx, Toast::error("Voice moderation failed").body(error.to_string()));
/// Toasts::push(cx, Toast::info("Copied").duration(Duration::from_secs(2)));
/// ```
pub struct Toasts;

impl Toasts {
    /// The store, made on first use.
    pub fn store(cx: &mut App) -> Entity<ToastStore> {
        if let Some(global) = cx.try_global::<ToastsGlobal>() {
            return global.0.clone();
        }
        let store = cx.new(|_| ToastStore {
            shown: Vec::new(),
            next: 1,
        });
        cx.set_global(ToastsGlobal(store.clone()));
        store
    }

    /// Shows a toast, or returns the id of the identical one already shown (same severity, title
    /// and body: a flapping event source doesn't stack copies).
    pub fn push(cx: &mut App, toast: Toast) -> ToastId {
        Self::store(cx).update(cx, |store, cx| store.push(toast, cx))
    }

    pub fn dismiss(cx: &mut App, id: ToastId) {
        Self::store(cx).update(cx, |store, cx| store.dismiss(id, cx))
    }

    /// Dismisses every toast (signing out, closing the window they belong to).
    pub fn dismiss_all(cx: &mut App) {
        Self::store(cx).update(cx, |store, cx| {
            if !store.shown.is_empty() {
                store.shown.clear();
                cx.notify();
            }
        })
    }

    /// How many toasts are shown (and so how many dismiss timers exist).
    pub fn count(cx: &mut App) -> usize {
        Self::store(cx).read(cx).shown.len()
    }

    /// Whether toast `id` is still shown (not dismissed by its ✕, its action or its timer).
    pub fn is_shown(cx: &mut App, id: ToastId) -> bool {
        Self::store(cx).read(cx).shown.iter().any(|shown| shown.id == id)
    }
}

impl ToastStore {
    fn push(&mut self, toast: Toast, cx: &mut Context<Self>) -> ToastId {
        if let Some(existing) = self.shown.iter().find(|shown| {
            shown.toast.severity == toast.severity
                && shown.toast.title == toast.title
                && shown.toast.body == toast.body
        }) {
            return existing.id;
        }
        let id = ToastId(self.next);
        self.next += 1;
        let duration = toast.duration;
        let timer = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(duration).await;
            this.update(cx, |store, cx| store.dismiss(id, cx)).ok();
        });
        self.shown.push(Shown {
            id,
            toast,
            _timer: timer,
        });
        cx.notify();
        id
    }

    fn dismiss(&mut self, id: ToastId, cx: &mut Context<Self>) {
        let before = self.shown.len();
        self.shown.retain(|shown| shown.id != id);
        if self.shown.len() != before {
            cx.notify();
        }
    }
}

/// The toast stack: `fixed inset-x-0 top-6 flex flex-col items-center gap-2 px-4`, each card
/// `max-w-md rounded-lg border bg-bg-light px-4 py-3 shadow-float` with the severity's tinted
/// border and icon disc, entering with `toastIn` (0.25 s `ease`, from 8 px up and transparent).
/// A toast leaves at once when dismissed or timed out (the renderer has no exit animation).
/// Draw one in the root view; it reads the store, so pushing a toast redraws it.
///
/// ```ignore
/// div().size_full().child(content).child(ToastStack::new())
/// ```
#[derive(IntoElement, Default)]
pub struct ToastStack;

impl ToastStack {
    pub fn new() -> Self {
        Self
    }
}

impl RenderOnce for ToastStack {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let store = Toasts::store(cx);
        let toasts: Vec<(ToastId, Toast)> = store
            .read(cx)
            .shown
            .iter()
            .map(|shown| (shown.id, shown.toast.clone()))
            .collect();
        if toasts.is_empty() {
            return div().into_any_element();
        }
        let bounds = visible_window_bounds(window, cx);
        let cards: Vec<_> = toasts
            .into_iter()
            .map(|(id, toast)| toast_card(id, toast, cx))
            .collect();
        crate::layer::layer(
            crate::layer::TOAST_PRIORITY,
            Placed::new(Placement::At(bounds.origin + gpui::point(px(0.), px(24.)))).child(
                div()
                    .w(bounds.size.width)
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap_2()
                    .px_4()
                    .children(cards),
            ),
        )
        .into_any_element()
    }
}

fn toast_card(id: ToastId, toast: Toast, cx: &App) -> impl IntoElement {
    let theme = cx.theme();
    let tone = match toast.severity {
        ToastSeverity::Error => theme.color_error,
        ToastSeverity::Warning => theme.color_warning,
        ToastSeverity::Info => theme.color_accent_bright,
        ToastSeverity::Success => theme.color_success,
    };
    let icon = match toast.severity {
        ToastSeverity::Error | ToastSeverity::Warning => IconName::AlertCircle,
        ToastSeverity::Info => IconName::Info,
        ToastSeverity::Success => IconName::Checkmark,
    };
    let element_id = ElementId::Integer(id.0);
    div()
        .id(element_id.clone())
        .occlude()
        .flex()
        .w_full()
        .max_w(px(448.))
        .items_start()
        .gap_3()
        .rounded(theme.radius_lg)
        .border_1()
        .border_color(tone.opacity(0.15))
        .bg(theme.color_bg_light)
        .px_4()
        .py_3()
        .shadow(theme.shadow_float.clone())
        .child(
            div()
                .mt_0p5()
                .flex()
                .flex_none()
                .size_8()
                .items_center()
                .justify_center()
                .rounded_full()
                .bg(tone.opacity(0.1))
                .child(crate::Icon::new(icon).size(px(16.)).color(tone)),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_w_0()
                .child(
                    Label::new(toast.title)
                        .size(13.)
                        .weight(500.)
                        .color(theme.color_text_primary),
                )
                .when_some(toast.body, |this, body| {
                    this.child(
                        div().mt_0p5().child(
                            Label::new(body)
                                .size(11.)
                                .line_height(11. * 1.55)
                                .color(theme.color_text_muted),
                        ),
                    )
                })
                .when_some(toast.action, |this, action| {
                    let probe = decibell_harness::probes::probe(SharedString::from(format!("toast-action:{}", action.label)));
                    this.child(
                        div().mt_1p5().flex().child(
                            // A flex box: the probe's canvas (absolute, no insets) sits on the link.
                            div()
                                .relative()
                                .flex()
                                .child(LinkButton::new((element_id.clone(), "action"), action.label).on_click(
                                    move |_, window, cx| {
                                        Toasts::dismiss(cx, id);
                                        (action.run)(window, cx);
                                    },
                                ))
                                .children(probe),
                        ),
                    )
                }),
        )
        .child(
            div().mt_0p5().child(
                IconButton::new((element_id.clone(), "dismiss"), IconName::Close)
                    .small()
                    .on_click(move |_, _, cx| Toasts::dismiss(cx, id)),
            ),
        )
        .with_animation(
            (element_id, "toast-in"),
            Animation::new(Duration::from_millis(250)).with_easing(motion::EASE.easing()),
            |card, t| card.opacity(t).top(px(-8. * (1. - t))),
        )
}
