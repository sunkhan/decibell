# Third-Party Licenses

This document lists third-party components redistributed with Decibell
binaries, the license under which each is provided, and where to
obtain corresponding sources where required.

---

## Windows builds

### FFmpeg (LGPL v2.1+)

Decibell's Windows installer dynamically links against the following
FFmpeg shared libraries (placed alongside the native addon in the
packaged app's `resources/native/` directory):

- `avcodec-*.dll`
- `avutil-*.dll`
- `avformat-*.dll`
- `swscale-*.dll`
- `swresample-*.dll`

These are built from unmodified upstream FFmpeg sources via
[vcpkg](https://github.com/microsoft/vcpkg) with the `nvcodec`,
`amf`, and `qsv` features enabled. **No GPL-licensed FFmpeg
components (libx264, libx265, libaom) are linked in our distributed
binaries.**

FFmpeg is distributed under the GNU Lesser General Public License v2.1
or later. The full LGPL text is available at:
<https://www.gnu.org/licenses/lgpl-2.1.html>

You may obtain the corresponding FFmpeg source code at:
<https://ffmpeg.org/download.html>

To re-link against a modified FFmpeg, replace the DLLs in the
installed app's `resources/native/` directory. The Decibell native
addon performs runtime DLL discovery via the OS loader's standard
search order and does not pin to any specific FFmpeg minor version
beyond the major API version (currently 8.x).

### Hardware encoder SDKs (linked transitively through FFmpeg)

The FFmpeg DLLs above call into vendor-supplied hardware encoder
runtimes installed on the user's system. These runtimes are not
redistributed by Decibell — they ship with the user's GPU driver.

- **NVIDIA Video Codec SDK (NVENC)** — proprietary, redistributable
  as part of products:
  <https://developer.nvidia.com/nvidia-video-codec-sdk>
- **AMD Advanced Media Framework (AMF)** — proprietary,
  redistributable as part of products:
  <https://github.com/GPUOpen-LibrariesAndSDKs/AMF>
- **Intel oneVPL (Quick Sync Video / QSV)** — Apache License 2.0:
  <https://github.com/intel/libvpl>

---

## All platforms

Other third-party Rust crates and npm packages used by Decibell ship
under permissive open-source licenses (MIT, Apache-2.0, BSD,
ISC, etc.). The full list is generated from `cargo about` /
`license-checker` output and is reproduced in the per-release
GitHub Release notes.
