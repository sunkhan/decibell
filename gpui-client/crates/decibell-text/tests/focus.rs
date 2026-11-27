//! A field's focus with the window in the background: `text_field_focused` follows the
//! window's focus (GPUI sends no focus event to a window that isn't active), and the typing
//! step stays open over a window switch (GPUI blurs the field then; Chromium's typing command
//! ends only on a selection change). Test platform, no GPU.

use decibell_text::input::{Command, InputState, text_field_focused};
use gpui::{
    AppContext as _, Context, EntityInputHandler as _, Focusable as _, InteractiveElement as _, IntoElement, ParentElement as _,
    Render, Styled as _, TestAppContext, VisualTestContext, Window, div, px,
};

struct Holder {
    input: gpui::Entity<InputState>,
}

impl Render for Holder {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // The field's focus handle in the frame, as its element would put it there.
        div().size(px(200.)).child(div().size(px(100.)).track_focus(&self.input.read(cx).focus_handle(cx)))
    }
}

fn holder(cx: &mut TestAppContext) -> (gpui::Entity<InputState>, &mut VisualTestContext) {
    let mut input = None;
    let (_holder, cx) = cx.add_window_view(|window, cx| {
        let state = cx.new(|cx| InputState::multi_line(window, cx));
        input = Some(state.clone());
        Holder { input: state }
    });
    cx.run_until_parked();
    (input.unwrap(), cx)
}

fn typed(input: &gpui::Entity<InputState>, text: &str, cx: &mut VisualTestContext) {
    let input = input.clone();
    let text = text.to_string();
    cx.update(move |window, cx| input.update(cx, |state, cx| state.replace_text_in_range(None, &text, window, cx)));
    cx.run_until_parked();
}

#[gpui::test]
fn a_field_focused_in_a_background_window_is_focused(cx: &mut TestAppContext) {
    let (input, cx) = holder(cx);
    cx.deactivate_window();
    let focus = cx.update(|_, cx| input.read(cx).focus_handle(cx));
    cx.update(|window, cx| focus.focus(window, cx));
    cx.run_until_parked();
    assert!(cx.update(|window, _| !window.is_window_active() && focus.is_focused(window)));
    assert!(cx.update(|window, _| text_field_focused(window)), "the window's focus is the field's");
    cx.update(|window, cx| window.blur(cx));
    cx.run_until_parked();
    assert!(cx.update(|window, _| !text_field_focused(window)), "nothing focused");
}

#[gpui::test]
fn a_typing_step_stays_open_over_a_window_switch(cx: &mut TestAppContext) {
    let (input, cx) = holder(cx);
    let focus = cx.update(|_, cx| input.read(cx).focus_handle(cx));
    cx.update(|window, cx| {
        window.activate_window();
        focus.focus(window, cx);
    });
    cx.run_until_parked();
    typed(&input, "one ", cx);
    // The desktop's focus goes elsewhere and comes back: GPUI blurs the field and focuses it again.
    cx.deactivate_window();
    cx.update(|window, _| window.activate_window());
    cx.run_until_parked();
    typed(&input, "two", cx);
    let undo = |cx: &mut VisualTestContext| {
        let field = input.clone();
        cx.update(move |window, cx| field.update(cx, |state, cx| state.run(Command::Undo, window, cx)));
        cx.update(|_, cx| input.read(cx).value().to_string())
    };
    assert_eq!(undo(cx), "", "one burst, one step");
    // A real blur (the focus elsewhere in the window) ends it.
    typed(&input, "", cx);
    cx.update(|window, cx| focus.focus(window, cx));
    typed(&input, "three", cx);
    cx.update(|window, cx| window.blur(cx));
    cx.run_until_parked();
    cx.update(|window, cx| focus.focus(window, cx));
    cx.run_until_parked();
    typed(&input, " four", cx);
    assert_eq!(undo(cx), "three", "a blur ends the step");
}
