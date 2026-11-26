//! The DM-mode sidebar's frame (`features/channels/ConversationSidebar.tsx`): the "Direct
//! Messages" header and the conversation list ([`Slot::DmList`]; the shell's default draws the
//! chat half's `DmList`).

use decibell_common::Watch;
use decibell_model::Model;
use decibell_ui::{ActiveTheme as _, FontRole, Label};
use gpui::{AnyView, AppContext as _, Context, DragMoveEvent, Entity, IntoElement, Render, StyleRefinement, Window, div, prelude::*};

use crate::sidebar::{ResizeDrag, SidebarResize, resize_handle, sidebar_frame};
use crate::slots::{PaneHost, Slot};
use crate::user_panel::PanelHeight;

pub struct ConversationSidebar {
    list: PaneHost,
    panel: Entity<PanelHeight>,
    resize: ResizeDrag,
    _watch: Watch,
}

impl ConversationSidebar {
    pub fn new(panel: Entity<PanelHeight>, cx: &mut Context<Self>) -> Self {
        let model = Model::global(cx).clone();
        let mut watch = Watch::new();
        watch
            .store(&panel, cx)
            .store_if(&model.layout, cx, |this, layout, _| this.resize.width_changed(layout.prefs().sidebar_width as f32));
        let width = model.layout.read(cx).prefs().sidebar_width as f32;
        Self { list: PaneHost::default(), panel, resize: ResizeDrag::new(width), _watch: watch }
    }
}

impl Render for ConversationSidebar {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let rows = |_: &mut Window, cx: &mut gpui::App| -> AnyView { cx.new(crate::dm_rows::DmRows::new).into() };
        let list: AnyView = self.list.view(Slot::DmList, window, cx, Some(&rows));
        let theme = cx.theme();
        let t = crate::ink::chrome(cx);
        sidebar_frame("dm-sidebar", self.resize.width(), self.panel.read(cx).list_padding(), &t)
            .on_drag_move(cx.listener(|this, e: &DragMoveEvent<SidebarResize>, window, cx| this.resize.drag_to(e, window, cx)))
            .child(
                div().flex().flex_none().h_12().items_center().border_b_1().border_color(t.color_border).px_4().child(
                    Label::new("Direct Messages")
                        .font(FontRole::Display)
                        .size(theme.scaled.text_title)
                        .weight(theme.palette.font_weight_emphasis)
                        .tracking(theme.palette.tracking_title)
                        .color(t.color_text_bright),
                ),
            )
            .child(div().flex_1().min_h_0().child(list.cached(StyleRefinement::default().size_full())))
            .child(resize_handle("dm-sidebar-resize", &t))
    }
}
