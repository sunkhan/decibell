# Picture editor

2026-10-28. Owner request: a modern, smooth profile picture picker and cropper in Settings, and the same for the
server picture in Server Settings, fitting every theme. The owner approved the proposal page
(https://claude.ai/artifact/4TbLmAKE12RvwCGdat5d5U) and answered its three decisions (marked **owner** below).

## What was wrong

Two near-copies of one canvas cropper (`AvatarCropperModal`, `ServerPictureCropperModal`). Each of these was
reproduced in the proposal harness:

- The picture could be dragged out of the frame or zoomed to a speck; whatever the frame showed outside it was
  uploaded as black. Transparent PNGs uploaded on black (a JPEG drawn on an unpainted canvas).
- A server picture over 1 MB was refused before cropping (the crop is a few dozen KB), and only JPEG / PNG.
- The avatar frame was a square with 6 px corners; avatars are 26% squircles in four themes and sharp in Console.
  The server Overview previewed a 128 × 44 tile as a 120 px square and asked for square images.
- A drag released over the backdrop closed the avatar cropper. Esc closed Settings under it (SettingsModal
  ignored `defaultPrevented`; the rename and password dialogs closed Settings the same way).
- The canvas was drawn at 1× (soft on HiDPI), and the export was one plain `drawImage` shrink of a 12 MP photo
  (stair-stepped edges).
- Wheel only, 10% per event (a trackpad raced), no slider, keys or reset; the drag stopped at the canvas edge.
- The server cropper closed when the upload was sent, so a refusal arrived as a toast with the crop gone.

## Shape

`components/pictureEditor/`, shared by both pictures; `AvatarCropperModal` and `ServerPictureCropperModal` are thin
wrappers (frame shape, output size, previews, upload).

| File | |
|---|---|
| `PictureEditor.tsx` | The dialog: stage, veil, shaped frame, pointer / wheel / keys, slider, Background swatches, live `Crop` previews, save, the flight |
| `geometry.ts` | Frame pixels: cover / contain, clamping, zoom at a point, rubber band, the log slider |
| `picture.ts` | Reading a file (decode, size limit, transparency, palette) and the export |
| `PictureButton.tsx` | The picture as a button (hover scrim, pencil badge) and `usePictureDrop` for the card around it |

**The stage is the file itself**, an `<img>` placed with transforms, so it is sharp at any zoom and DPR. The view
(scale, x, y in frame pixels) lives in a small store outside React (`useSyncExternalStore`): a drag re-renders only
the stage picture and the previews, which are the same image positioned again at their own sizes.

**The frame is the shape people see**: `--radius-avatar` for profile pictures, the tile's `--radius-md` scaled by
frame width / 128 for server pictures. The rest of the picture stays visible outside it under a veil in the
theme's stage colour (`--crop-veil`, from `--color-bg-darkest`; **owner: the theme's own colour**, so the light
themes get a light veil, as the image viewer's light stage). **Owner: the square's corners outside the shape are
half-lit** (`--crop-corner`), because other themes show them.

**Zoom range** (`minScale`): **owner: profile pictures stop at "fills the frame"** (`fit={false}`). Server pictures
(`fit`) may zoom out to the whole picture, so a square logo can sit whole on the 2.9 : 1 tile; the slider marks
"fills the frame" with a tick and catches on it. The top is 8× past filling the frame, or 1 : 1 with the export.

**Background** (`Fill`): where the frame shows something behind the picture (a server picture zoomed out, or any
transparent picture), a floating row of swatches appears: the picture blurred (default for photos), its main colour,
a deep shade of it (default for transparent pictures), white, black. The fill is painted into the export, so every
theme sees the same picture.

## Input

- Drag moves; past an edge the picture follows less and less (a third of the frame at most) and springs back on
  release (320 ms). Pointer capture keeps the drag going outside the stage.
- Wheel and pinch zoom toward the pointer, proportionally (`exp(-deltaY · k)`); a mouse notch glides (160 ms) and
  notches add up, a trackpad's small deltas and a pinch (ctrl + wheel) track directly. Slider and ± zoom around the
  middle.
- Keys: arrows move 8 px (40 with Shift), + / − zoom, 0 resets (also a double click), Enter saves.
- Esc and a click outside close the editor only (Esc is marked handled; SettingsModal now checks
  `defaultPrevented`). Once the crop has been adjusted they shake the footer instead, as an unsaved settings tab
  does. Cancel always closes. A drag that ends on the backdrop never closes.
- Reduced motion: no settle, glides, spring or flight; the dialog fades.

## Saving

`renderCrop` crops in frame pixels, paints the fill, then resamples only the visible part of the picture once with
`createImageBitmap(…, { resizeQuality: "high" })` and encodes JPEG at 0.9, stepping down to 0.66 if a file would
pass the 1 MB cap. Profile pictures stay 256 × 256 (the avatar cache holds up to 400 decoded pictures; the largest
avatar is 96 px). Server pictures are 880 × 303, twice the old 440 × 151, so a 432 px invite banner is sharp on
HiDPI; a photo is about 30 KB.

The editor stays on "Saving…" until the server answers: `upload_avatar` resolves with central's reply; the server
picture waits for `server_picture_update_responded` for that server (listening before the request goes out, 10 s
timeout), and `useServerEvents` skips its toast while an editor is waiting (`pictureSavesInFlight`). A refusal shows
in the editor and keeps the crop. On success the wrapper gets the JPEG as a blob URL (`onSaved`), which the Settings
card shows until the store has fetched the new picture, and the crop flies (560 ms) from the frame into the element
with the matching `data-picture-target`.

Accepted files: anything Chromium decodes (JPEG, PNG, WebP, GIF, AVIF, BMP), up to 80 megapixels. GIFs keep their
first frame (the editor says so). HEIC gets its own message (export it as JPEG).

## Entry points

- **Settings → Account**: the card is a profile header like the profile popup (user gradient banner, 80 px picture
  with the ring cut into it, name). Click the picture (hover scrim, pencil badge) or "Change picture", or drop a
  picture anywhere on the card.
- **Server Settings → Overview**: the picture moves up after the server's identity, previewed in the tile's shape
  at 2× (256 × 88), click or drop (Manage Server only). The 1 MB and JPEG / PNG pre-checks are gone; Remove asks
  through `ConfirmModal` instead of `window.confirm`.
- The server previews use the real tile: `ServerBar.tsx` exports its picture layers as `TilePictureLayers` and
  `ActiveTileGlow`.

## Not done

Animated avatars (central accepts JPEG only, and the avatar cache doesn't animate), rotation, filters. Editing in
place inside the tab was rejected: a picture saves on its own request, apart from Overview's name / description
draft and its save bar.
