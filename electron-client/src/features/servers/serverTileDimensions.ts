/// Shared dimensions for the ServerBar tile + matching cropper output.
/// Both the tile (in ServerBar.tsx) and the picture cropper
/// (ServerPictureCropperModal.tsx) reference these constants so the
/// stored image aspect ratio is exactly what gets displayed — no
/// surprise letterboxing or cropping at render time. Keeping them in
/// one file means the two can't drift apart.
///
/// 128×44 (2026-08-27, up from 110×38 when the bar grew from 58 to
/// 64px) fits a short server name + avatar in the no-picture branch and
/// feels right for the picture branch's image-with-overlay treatment.
/// The ~2.9:1 aspect carries through to the cropper viewport and
/// output. Pictures cropped at the old 2.895 aspect differ from the new
/// 2.909 by 0.5% — under `object-cover` that is a sub-pixel crop.
/// The DM tiles and the home / browse buttons share TILE_HEIGHT so the
/// whole bar lines up.

export const TILE_WIDTH = 128;
export const TILE_HEIGHT = 44;
export const TILE_ASPECT = TILE_WIDTH / TILE_HEIGHT;
