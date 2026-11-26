# What the core's encoders need from an FFmpeg build

The core opens these FFmpeg components by name. A build without one loses that backend and
nothing else: the probe logs `… is not in this FFmpeg build` and moves on.

| Backend | Needs |
|---|---|
| NVENC (Linux, Windows) | `h264_nvenc`, `hevc_nvenc`, `av1_nvenc` (nv-codec-headers) |
| VAAPI (Linux: AMD, Intel) | `h264_vaapi`, `hevc_vaapi`, `av1_vaapi`; libavfilter with `scale_vaapi` for the GPU conversion (without it the encoder still runs, converting on the CPU) |
| AMF, Quick Sync (Windows) | `*_amf`, `*_qsv` (vcpkg `ffmpeg[amf,qsv]`) |
| Software (both) | `libsvtav1`, i.e. **SVT-AV1 4.0 or later** (`--enable-libsvtav1`). Only 4.2.0 was run. 4.x forces a keyframe on request in low-delay mode; FFmpeg's `libsvtav1.c` notes that earlier versions did not, and the harness's forced-keyframe check fails on a build where that is so |
| Encoder harness decode checks (tests only) | decoders `h264`, `hevc`, `libdav1d` |

Three prefixes exist. What changes in each:

## 1. Development prefix (this crate's tests and measurements)

`crates/decibell-core/tools/build-ffmpeg-encoders.sh` builds
`~/.cache/decibell-dev/ffmpeg-enc/prefix` (static, LGPL-2.1+, no sudo, about 10 minutes) with
everything in the table. Pinned archives, SHA-256 checked:

| Source | Version | SHA-256 |
|---|---|---|
| FFmpeg | 8.1.3 | `7138d28c96d9d3e3af4ee3d8cad72741f8ffb40da90c1112235dea3ecd3178a3` |
| SVT-AV1 | v4.2.0 | `c7b13c4a84bd3751aa35fcc72be13e6875467e7c2216879251a486e5b1e4e740` |
| dav1d | 1.5.3 | `e099f53253f6c247580c554d53a13f1040638f2066edc3c740e4c2f15174ce22` |
| nasm (build tool) | 2.16.03 | `1412a1c760bbd05db026b6c0d1657affd6631cd0a63cddb6f73cc6d4aa616148` |
| nv-codec-headers | n13.0.19.0 | `86d15d1a7c0ac73a0eafdfc57bebfeba7da8264595bf531cf4d8db1c22940116` |

(FFmpeg, dav1d, nasm and nv-codec-headers are the versions and hashes of
`gpui-client/tools/build-ffmpeg.sh`.) Use it with
`PKG_CONFIG_PATH=~/.cache/decibell-dev/ffmpeg-enc/prefix/lib/pkgconfig` and a target directory of
its own (`CARGO_TARGET_DIR=target-enc`: ffmpeg-sys rebuilds when the prefix changes).

Licences: FFmpeg LGPL-2.1+ (the script fails if configure says otherwise); SVT-AV1
BSD-3-Clause-Clear plus the Alliance for Open Media Patent License 1.0; dav1d BSD-2-Clause. The
script copies `LICENSE.md` + `PATENTS.md` (SVT-AV1), `COPYING` (dav1d) and `COPYING.LGPLv2.1` to
`<prefix>/share/licenses/`. A client that ships SVT-AV1 adds it to `THIRD_PARTY_LICENSES.md`
with both files.

## 2. The GPUI client's prefix (`gpui-client/tools/build-ffmpeg.sh`)

**Done (2026-11-26, branch `wip/ffmpeg-sw`)**: the script builds SVT-AV1 v4.2.0 and configures
FFmpeg with the lines below; `gpui-client/vendor/ffmpeg` was rebuilt with it (`gpui-client/HANDOFF.md`
§2, `gpui-client/docs/video-design.md` §9 for the configure line). `decibell-app`'s `build.rs` refuses
an FFmpeg without `libsvtav1` and the software decoders, and `decibell --selftest encoder-software`
encodes and decodes back. The recipe as it was handed over stays here for reference.

That client has no Chromium to fall back to, so it needs the software encoder and the VAAPI
conversion. Lines to fold into the script (it already has nasm, dav1d, the hashes above and the
`fetch` / `unpack` helpers):

```sh
# next to the other pins
SVTAV1_VERSION=v4.2.0     # FFmpeg 8.1's libsvtav1.c handles the 4.x API; 4.0+ is needed for forced keyframes in low delay
SVTAV1_URL="https://gitlab.com/AOMediaCodec/SVT-AV1/-/archive/$SVTAV1_VERSION/SVT-AV1-$SVTAV1_VERSION.tar.gz"
SVTAV1_SHA256=c7b13c4a84bd3751aa35fcc72be13e6875467e7c2216879251a486e5b1e4e740

# in CONFIGURE_FLAGS
  --enable-avfilter
  --enable-libsvtav1
  --enable-encoder=libsvtav1
  --enable-filter=scale,scale_vaapi          # replaces --enable-filter=scale

# add "svt-av1 $SVTAV1_VERSION $SVTAV1_SHA256" to WANT (the stamp), cmake to the tool check,
# and, before the FFmpeg build (after dav1d):
if [ ! -f "$PREFIX/lib/libSvtAv1Enc.a" ]; then
  say "building SVT-AV1 $SVTAV1_VERSION"
  unpack "$(fetch "$SVTAV1_URL" "$SVTAV1_SHA256")" "SVT-AV1-$SVTAV1_VERSION"
  (cd "$BUILD/SVT-AV1-$SVTAV1_VERSION" && rm -rf build-static && cmake -S . -B build-static -G Ninja \
      -DCMAKE_BUILD_TYPE=Release -DBUILD_SHARED_LIBS=OFF -DBUILD_APPS=OFF -DBUILD_TESTING=OFF \
      -DSVT_AV1_LTO=OFF -DCMAKE_POSITION_INDEPENDENT_CODE=ON \
      -DCMAKE_INSTALL_PREFIX="$PREFIX" -DCMAKE_INSTALL_LIBDIR=lib >/dev/null \
    && ninja -C build-static -j"$JOBS" >/dev/null && ninja -C build-static install >/dev/null)
fi

# in the component check after configure
    LIBSVTAV1_ENCODER SCALE_VAAPI_FILTER
```

Notes:

- `-DSVT_AV1_LTO=OFF`: with LTO the static archive holds compiler-specific objects and only
  links with the compiler that made it. `nasm` must be on `PATH` when cmake runs (the script
  already builds one into `$PREFIX/bin`): without it SVT-AV1 builds C-only and is not real-time.
- Size: `libSvtAv1Enc.a` is 9.9 MB; its code and tables are 7.2 MB of the linked binary
  (`size -t`: text 7,153,658 bytes).
- `--force` must also remove `"$BUILD/SVT-AV1-$SVTAV1_VERSION"` and `"$PREFIX/lib/libSvtAv1Enc.a"`.
- SVT-AV1 logs its configuration to stderr at every open (about 25 lines). `SVT_LOG=1` in the
  process environment (set by the client's `main` before any thread starts) keeps it to errors.

## 3. The Electron addon's prefix (`electron-client/native/scripts/build-ffmpeg-linux.sh`)

Changed in this work: `--enable-filter=scale,scale_vaapi` (was `scale`), so an AMD / Intel
machine converts on the GPU. `libavfilter` was already built and linked; `scale_vaapi` is one more
filter in it (`vf_scale_vaapi.c` + `vaapi_vpp.c`).

**Not** added: SVT-AV1. Electron has Chromium's WebCodecs encoder for machines without a
hardware encoder, and SVT-AV1 would add 7 MB to the addon for a path that client does not need.
The addon's probe therefore reports no software backend and the renderer falls back exactly as
before.

A prefix built before this change has no `scale_vaapi`: VAAPI then logs
`GPU convert unavailable (this FFmpeg build has no scale_vaapi filter); converting to NV12 on the CPU`
and works. Rebuild the prefix (delete `electron-client/native/vendor/ffmpeg`, run the script) to
get the GPU conversion; the release workflow builds it fresh.

## Windows (vcpkg)

CI and releases install `ffmpeg[nvcodec,amf,qsv]:x64-windows` from vcpkg pinned at
`e5a1490e1409d175932ef6014519e9ae149ddb7c` (FFmpeg 8.0.1, port version 2). That is enough for
NVENC, AMF and Quick Sync (the default features include avfilter, which the Windows encoders do
not use).

**Software on Windows needs more than a feature name**: the pinned `ffmpeg` port has no
`svt-av1` feature (its features: `aom`, `dav1d`, `openh264`, `x264`, … but not SVT-AV1), and the
pinned `svt-av1` port is 3.1.2, older than the 4.x this was built and measured with (and
probably unable to force keyframes in low delay, see the table above). The GPUI client uses the
overlay ports in **`gpui-client/tools/vcpkg-overlay`** (2026-11-26; setup in
`gpui-client/HANDOFF.md` §2, Windows), installed as `ffmpeg[nvcodec,amf,qsv,dav1d,svt-av1]` with
`--overlay-ports` (`dav1d`: the client's AV1 software decoder, and the harness's AV1 decode check):

1. `svt-av1` at **4.2.0**: upstream vcpkg's newest recipe (the 4.3.1 port of 2026-11-25, its three
   patches apply to 4.2.0 unchanged) with `version-semver` 4.2.0, the SHA512 of GitHub's v4.2.0
   archive (same tree as the GitLab one the Linux script fetches) and the 4.1.0 port's
   Visual Studio 2026 workaround (`no-inline-yy_unpacklo_epi128.diff`, which 4.2.0 still needs).
2. `ffmpeg` 8.0.1 port-version 2 (the pin) plus upstream's own `svt-av1` feature (vcpkg commit
   `905347cd`, which added it to the 8.1.2 port: the feature in `vcpkg.json`, the
   `--enable-libsvtav1` block in `portfile.cmake`, the CMake wrapper's block), **and one patch:
   FFmpeg 8.0.1's `libsvtav1.c` does not compile against SVT-AV1 4.x** (`param->enable_adaptive_quantization`
   was renamed `aq_mode` in 4.0); `0047-libsvtav1-svt-av1-4.patch` is FFmpeg's own fix (commit
   `a5d4c398`, in 8.1). Nothing else in 8.1's `libsvtav1.c` matters to the stream: the rest of the
   difference is two-pass encoding, enum casts and an internal rename; the key frame request path
   (`pict_type` I → `EB_AV1_KEY_PICTURE`) is identical. Checked on Linux: FFmpeg n8.0.1 with the
   port's patches and this one, against SVT-AV1 4.2.0, passes every software test of this crate
   (`cargo test --lib software`: the harness at 2245 kbps / PSNR-Y 58.4 dB like 8.1.3, and the
   NV12 path's forced key frame).

Without the overlay the Windows probe logs `Software (SVT-AV1) … libsvtav1 is not in this FFmpeg
build`, and a Windows machine without a hardware encoder has no native encoder (Electron: the
WebCodecs fallback, as today). The Electron addon's jobs keep the plain `ffmpeg[nvcodec,amf,qsv]`.

Do not move the vcpkg pin for this: FFmpeg 8.1 changed how the D3D11 frame pool allocates
(`hwcontext_d3d11va.c`: a fixed pool of render-target textures became one texture array), which
the NVENC / AMF path refuses on purpose (`encoder.rs`, "texture-array slice"). The pin and that
check are why; re-validate both together.
