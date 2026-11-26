//! The picture editor (`components/pictureEditor/`, spec
//! `docs/superpowers/specs/2026-10-28-picture-editor-design.md`): crop a picked file into a
//! profile picture or a server picture.
//!
//! The stage shows the whole picture, veiled outside the frame, with the frame cut in the shape
//! people will see: the theme's avatar shape, or the server tile's corners scaled up. The
//! square's corners stay half lit, because other themes show them. Drag past an edge and it
//! gives, then springs back. Wheel or pinch zooms toward the pointer, the slider and ± around the
//! middle. With `fit` (server pictures) zooming out goes past "fills the frame" down to the whole
//! picture; the gap takes a fill (the picture blurred, or a colour) that is baked into the export.
//! Without it (profile pictures, owner decision 2026-10-28) the picture always fills the frame,
//! and only a transparent one shows a fill. Save renders the crop at the output size and hands the
//! JPEG to the caller, which uploads it.
//!
//! ```ignore
//! // Open (the Settings → Account tab, for the avatar):
//! let editor = cx.new(|cx| PictureEditor::new(
//!     PictureSource::Path(path),                       // or PictureSource::Bytes { bytes, name }
//!     EditorOptions::avatar(uid, name, Presence::Online),
//!     window, cx,
//! ));
//! self._editor_events = cx.subscribe(&editor, |this, editor, event: &PictureEditorEvent, cx| match event {
//!     // 256 × 256 JPEG ≤ 1 MB: upload it, then tell the editor how it went.
//!     PictureEditorEvent::Save(jpeg) => this.upload_avatar(jpeg.clone(), editor, cx),
//!     //   …later: editor.update(cx, |e, cx| e.save_done(Ok(()), cx))   // closes it
//!     //   or:     editor.update(cx, |e, cx| e.save_done(Err("Couldn't save your picture.".into()), cx))
//!     PictureEditorEvent::Closed { .. } => { this.editor = None; cx.notify() }   // drop it: its memory goes
//! });
//! self.editor = Some(editor);
//! // In render: `.children(self.editor.clone())` (it draws its own modal over everything).
//! ```
//!
//! Decoding and the export run off the UI thread; the stage and previews draw one texture of at
//! most 2560 px; everything (decoded original, textures, their atlas tiles) is released when the
//! editor closes and the caller drops it.

pub mod geometry;
pub mod picture;
mod previews;
#[cfg(feature = "fixtures")]
mod screens;
mod view;

pub use picture::{Fill, Picture, PictureError};
pub use previews::{Crop, CropState};
pub use view::{PictureEditor, PictureEditorEvent, PictureSource};

use std::rc::Rc;

use decibell_ui::Presence;
use gpui::{AnyElement, App, SharedString, Window};

/// Which picture is being edited: the frame's shape and its output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PictureShape {
    /// A profile picture: a square frame in the theme's avatar shape, previews beside the stage,
    /// 256 × 256, always filling the frame.
    Avatar,
    /// A server picture: the server tile's shape (128 × 44, its corner scaled up), previews under
    /// the stage, 880 × 302, may zoom out to the whole picture.
    ServerTile,
}

/// The server bar tile's size (`serverTileDimensions.ts`).
pub const TILE_WIDTH: f32 = 128.;
pub const TILE_HEIGHT: f32 = 44.;

impl PictureShape {
    /// Frame width / height.
    pub fn aspect(self) -> f32 {
        match self {
            PictureShape::Avatar => 1.,
            PictureShape::ServerTile => TILE_WIDTH / TILE_HEIGHT,
        }
    }

    /// Zooming out may go past "fills the frame".
    pub fn fit(self) -> bool {
        self == PictureShape::ServerTile
    }

    /// The renderer's output: 256 × 256 (`AvatarCropperModal`), 880 × round(880 / aspect)
    /// (`ServerPictureCropperModal`: 2× the invite banner's 432 px, sharp on HiDPI).
    pub fn output(self) -> (u32, u32) {
        match self {
            PictureShape::Avatar => (256, 256),
            PictureShape::ServerTile => (880, (880. / self.aspect()).round() as u32),
        }
    }
}

/// Builds the previews column from the live crop.
pub type PreviewBuilder = Rc<dyn Fn(&CropState, &mut Window, &mut App) -> AnyElement>;

/// What the editor previews beside (avatar) or under (server) the stage.
#[derive(Clone)]
pub enum Previews {
    /// Profile, Chat and Member list, as `AvatarCropperModal`'s.
    Avatar { uid: i64, name: SharedString, status: Presence },
    /// The server bar's tile resting and selected, and an invite card's banner
    /// (`ServerPictureCropperModal`'s).
    /// `members`: the card's "298 members" (formatted by the caller: `Regional::group`), none
    /// when unknown.
    Server { name: SharedString, members: Option<SharedString> },
    Custom(PreviewBuilder),
    None,
}

/// How an editor is set up.
#[derive(Clone)]
pub struct EditorOptions {
    pub shape: PictureShape,
    pub title: SharedString,
    /// Output size in px; the shape's by default.
    pub output: (u32, u32),
    pub previews: Previews,
}

impl EditorOptions {
    /// "Edit profile picture": 256 × 256.
    pub fn avatar(uid: i64, name: impl Into<SharedString>, status: Presence) -> Self {
        EditorOptions {
            shape: PictureShape::Avatar,
            title: SharedString::new_static("Edit profile picture"),
            output: PictureShape::Avatar.output(),
            previews: Previews::Avatar { uid, name: name.into(), status },
        }
    }

    /// "Edit server picture": 880 × 302. `members`: the invite card's "298 members", if known.
    pub fn server(name: impl Into<SharedString>, members: Option<SharedString>) -> Self {
        EditorOptions {
            shape: PictureShape::ServerTile,
            title: SharedString::new_static("Edit server picture"),
            output: PictureShape::ServerTile.output(),
            previews: Previews::Server { name: name.into(), members },
        }
    }
}

/// The editor's screens (feature `fixtures`): `picture-editor-avatar`, `picture-editor-server`.
pub fn screens() -> Vec<decibell_harness::Screen> {
    #[cfg(feature = "fixtures")]
    return screens::screens();
    #[cfg(not(feature = "fixtures"))]
    Vec::new()
}
