# Third-party licenses

## FFmpeg (LGPL v2.1+)

Decibell's Linux native stream encoder statically links a custom build of
**FFmpeg** (libavcodec, libavutil, libavformat, libavfilter, libswscale,
libswresample) for hardware video encoding (NVENC / VAAPI).

- Upstream: https://ffmpeg.org
- License: **GNU Lesser General Public License, version 2.1 or later**
  (https://www.gnu.org/licenses/lgpl-2.1.html). Our build is configured
  **without** `--enable-gpl` and without any GPL-only external codec
  (notably no libx264) so the linked FFmpeg is LGPL, not GPL.

### How it is built

The exact FFmpeg used is reproducible from source via
`native/scripts/build-ffmpeg-linux.sh`, which downloads the pinned FFmpeg
release tarball and FFmpeg's `nv-codec-headers`, then configures and
compiles the static libraries. The configure flags (encoders, version,
licence-relevant options) live in that script.

### LGPL §6 — relinking

Because FFmpeg is statically linked into the native addon
(`index.linux-*.node`), the LGPL grants recipients the right to relink the
application against a modified FFmpeg. The materials to do so are
available:

- FFmpeg source: the pinned release at https://ffmpeg.org/releases/ (see
  the version in `build-ffmpeg-linux.sh`), built with the flags in that
  script.
- The addon is rebuilt from the published source in this repository; the
  static FFmpeg it links can be substituted by re-running
  `build-ffmpeg-linux.sh` with a modified FFmpeg checkout and rebuilding.

A copy of the LGPL v2.1 text is distributed at the URL above. On request we
provide the corresponding FFmpeg object files / static archives for
relinking.

## Windows FFmpeg

The Windows build links FFmpeg dynamically from vcpkg (same LGPL terms);
the DLLs are shipped alongside the binary.
