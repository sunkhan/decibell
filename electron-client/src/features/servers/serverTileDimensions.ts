/// Shared dimensions for the ServerBar tile + matching cropper output.
/// Both the tile (in ServerBar.tsx) and the picture cropper
/// (ServerPictureCropperModal.tsx) reference these constants so the
/// stored image aspect ratio is exactly what gets displayed — no
/// surprise letterboxing or cropping at render time. Keeping them in
/// one file means the two can't drift apart.
///
/// 130×38 was chosen as a comfortable size that fits a short server
/// name + avatar in the no-picture branch and feels visually right
/// for the picture branch's image-with-overlay treatment. The 3.42:1
/// aspect ratio carries through to the cropper viewport and output.

export const TILE_WIDTH = 130;
export const TILE_HEIGHT = 38;
export const TILE_ASPECT = TILE_WIDTH / TILE_HEIGHT;
