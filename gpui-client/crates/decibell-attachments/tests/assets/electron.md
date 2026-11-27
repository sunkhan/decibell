# What the Electron client stored for these files

Recorded 2026-11-27 (`media_probe::tests::the_electron_clients_values` asserts it): the Electron client of
`tools/perf/electron-local.sh` (build `fa03ce16`, Electron 33.4.11, signed in as `alice`) against the local
stack, each file dropped on `#off-topic` in Daily Gaming through CDP (`Input.dispatchDragEvent` with the
file's path: the renderer's `useDragDrop` → `chunkSourceFromFile` → `queueUpload` → `probeMetadata`) and
sent with Enter; the values read back from the community's `attachments` table and the thumbnails it
stored. The messages were deleted afterwards. The files are exactly the ones in this directory (and the
media crate's two MP3s); `make.py` regenerates them, but SVT-AV1 is not bit-exact between runs: these
values belong to the committed `sar.webm` (sha256 `c6fe3ae8…`).

| File | width × height | duration_ms | mask | poster frame | placeholder |
|---|---|---|---|---|---|
| rot90.mp4 | 90 × 160 | 1003 | 7 | 2 (block bottom left) | `BwgGBADvDCDkzUOUeWgAAAAAAA==` |
| sar.webm | 240 × 90 | 2000 | 7 | 5 | `CAgGA4DvnGjvvdnNAAAAAAA=` |
| noaudio.mp4 | 160 × 90 | 3000 | 7 | 2 | `BwgGBIDvnGj/vYnevLwAAAAAAA==` |
| ends2.mp4 | 64 × 36 | 2005 | 7 | 5 | `BggGBIDvrHj/zYrevLwAAAAAAA==` |
| ends25.mp4 | 64 × 36 | 2517 | 7 | 6 | `BwgGBIDvrHj/zYrevLwAAAAAAA==` |
| vp9.webm | 120 × 90 | 2000 | 7 | 5 | `CAgGBYDvnGj/vYnevMm7mQAAAAAA` |
| tone1.m4a | 0 × 0 | 1000 | 0 | – | – |
| media's cbr.mp3 | 0 × 0 | 2040 | 0 | – | – |
| media's vbr_noxing.mp3 | 0 × 0 | 2029 | 0 | – | – |

"Poster frame" is the frame number the stored 1280 px thumbnail shows (the grey level of `make.py`'s
pictures).

Also uploaded, not committed (too large, or kept only to find the rules), with the same answers from
this client's probe unless said: H.264 + AAC test patterns of 320 × 180 lasting 2, 2.5, 3, 4 and 10 s
(2005, 2500, 3000, 4000, 10000 ms), MP3s of 2.5 to 6 s with an Info tag (2544, 3024, 3528, 4032,
6024 ms: whole frames), M4As of 1, 1.8 and 2.5 s (1000, 1800, 2500), VP8 / VP9 / H.264-in-Matroska /
AV1-in-MP4 at 25 fps (frame 5, as `vp9.webm`). Sizes and lengths equal everywhere but one:

- `ends25.mp4` (5 KB) stored 2517 where this client says 2500; the 100 KB test pattern of the same
  length and timing stored 2500. Whether Chromium's demuxer reads a clip of 2–2.5 s to its end depends on
  more than its timing; this client reads two seconds past the poster's moment (`READ_AHEAD`). At most one
  audio frame (21–24 ms) apart. `docs/electron-quirks.md` #40.
- The posters of the five test patterns of 320 × 180 with sound were their first frame, not the frame at
  the poster's moment (the same file uploaded twice gave the same answer; the 120 × 90 pattern without
  sound and every grey-level file gave the frame at the moment). The renderer seeks and draws on `seeked`;
  Chromium had not shown the new frame yet. This client always draws the frame at the moment, as the
  renderer's code asks. `docs/electron-quirks.md` #40.
