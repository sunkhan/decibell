//! The editor's screens, from generated pictures (no file, no clock): `decibell-capture --screen
//! picture-editor-avatar` / `picture-editor-server`.

use decibell_harness::Screen;
use decibell_ui::{ActiveTheme as _, AssumeClientDecorations, Presence};
use gpui::{App, AppContext as _, Context, Entity, IntoElement, ParentElement as _, Render, Styled as _, Window, div};

use crate::picture::{from_rgba, sample_rgba};
use crate::{EditorOptions, PictureEditor, PictureShape, PictureSource};

struct EditorScreen {
    editor: Entity<PictureEditor>,
}

impl Render for EditorScreen {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div().size_full().bg(cx.theme().color_bg_primary).child(self.editor.clone())
    }
}

fn settle(cx: &mut App) {
    // Captures render one frame on a clock that doesn't run: the picture settles at once.
    if cx.has_global::<AssumeClientDecorations>() {
        cx.set_reduce_motion(true);
    }
}

fn build(shape: PictureShape, window: &mut Window, cx: &mut App) -> gpui::AnyView {
    settle(cx);
    let (options, (w, h)) = match shape {
        PictureShape::Avatar => (EditorOptions::avatar(6, "perf1", Presence::Online), (1200, 1600)),
        PictureShape::ServerTile => (EditorOptions::server("Daily Gaming", Some("298 members".into())), (1600, 900)),
    };
    let picture = from_rgba(sample_rgba(w, h), "sample.png", shape.aspect());
    let editor = cx.new(|cx| {
        PictureEditor::new(PictureSource::Bytes { bytes: Vec::new(), name: String::new() }, options, window, cx).with_picture(picture)
    });
    cx.new(|_| EditorScreen { editor }).into()
}

pub fn screens() -> Vec<Screen> {
    vec![
        Screen {
            name: "picture-editor-avatar",
            about: "The picture editor for a profile picture: round frame, previews beside (generated picture)",
            build: |window, cx| build(PictureShape::Avatar, window, cx),
        },
        Screen {
            name: "picture-editor-server",
            about: "The picture editor for a server picture: the tile's frame, previews under (generated picture)",
            build: |window, cx| build(PictureShape::ServerTile, window, cx),
        },
    ]
}
