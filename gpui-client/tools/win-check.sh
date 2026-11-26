#!/usr/bin/env bash
# Type-checks the client for Windows (x86_64-pc-windows-msvc) on a Linux machine: every
# `cfg(windows)` line of the workspace, of crates/decibell-core and of the patched gpui_windows
# goes through rustc against the real `windows` crate. Nothing is linked and nothing runs, so
# no Windows SDK is needed; the Windows Native Check workflow stays the real build.
#
#   tools/win-check.sh                       # the whole workspace, all targets
#   tools/win-check.sh -p gpui_windows       # any cargo-check arguments instead
#   tools/win-check.sh --shaders             # also compile gpui_windows' HLSL (needs wine)
#
# What it needs, none of it root:
# - a rustup toolchain with the Windows target's standard library (the system Rust has none).
#   Installed on first use under ~/.cache/decibell-dev/rustup-win, at the system rustc's
#   version; RUSTUP_HOME points there, CARGO_HOME (the registry) stays the user's;
# - clang-cl and llvm-lib (the `clang` and `llvm` packages): the C of ring, opus and friends
#   is compiled for the target so their build scripts pass;
# - Wine's headers (/usr/include/wine: the `wine` package) standing in for the Windows SDK's
#   and the CRT's, for that C and for bindgen (ffmpeg-sys-next) — which reads the Linux
#   FFmpeg prefix's headers (vendor/ffmpeg), the same API as the vcpkg build.
# Its own target directory (~/.cache/decibell-dev/rustup-win/target, ~6 GB).
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
home="${DECIBELL_WIN_CHECK_HOME:-$HOME/.cache/decibell-dev/rustup-win}"
target=x86_64-pc-windows-msvc

shaders=0
args=()
for arg in "$@"; do
    if [ "$arg" = "--shaders" ]; then shaders=1; else args+=("$arg"); fi
done

for tool in clang-cl llvm-lib; do
    command -v "$tool" >/dev/null || { echo "win-check: $tool not found" >&2; exit 2; }
done
[ -d /usr/include/wine/windows ] || { echo "win-check: /usr/include/wine/windows not found (install wine)" >&2; exit 2; }

if [ ! -x "$home/cargo/bin/cargo" ]; then
    version="$(rustc --version | awk '{print $2}')"
    echo "win-check: installing Rust $version with the $target standard library under $home"
    mkdir -p "$home"
    curl -sSf -o "$home/rustup-init" https://static.rust-lang.org/rustup/dist/x86_64-unknown-linux-gnu/rustup-init
    chmod +x "$home/rustup-init"
    RUSTUP_HOME="$home/rustup" CARGO_HOME="$home/cargo" "$home/rustup-init" -y --no-modify-path \
        --profile minimal --default-toolchain "$version" -t "$target" >/dev/null
fi

export RUSTUP_HOME="$home/rustup"
export PATH="$home/cargo/bin:$PATH"          # cargo must find this toolchain's rustc, not /usr/bin's
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$home/target}"
export CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-6}"
export PKG_CONFIG_ALLOW_CROSS=1
headers="/usr/include/wine/msvcrt /usr/include/wine/windows"
export BINDGEN_EXTRA_CLANG_ARGS="$(printf -- '-isystem %s ' $headers)"
export CC_x86_64_pc_windows_msvc=clang-cl CXX_x86_64_pc_windows_msvc=clang-cl
export AR_x86_64_pc_windows_msvc=llvm-lib
export CFLAGS_x86_64_pc_windows_msvc="$(printf -- '-imsvc %s ' $headers)"
export CXXFLAGS_x86_64_pc_windows_msvc="$CFLAGS_x86_64_pc_windows_msvc"

cd "$here"
if [ ${#args[@]} -eq 0 ]; then
    args=(--locked --workspace --all-targets)
fi
"$home/cargo/bin/cargo" check --target "$target" "${args[@]}"

if [ "$shaders" = 1 ]; then
    # gpui_windows compiles shaders.hlsl at run time in debug builds and with fxc.exe in
    # release builds, so `cargo check` never sees it. Wine's d3dcompiler_47 is another HLSL
    # compiler (vkd3d), not fxc: it catches syntax and type errors, not fxc's own complaints.
    command -v winegcc >/dev/null || { echo "win-check: winegcc not found" >&2; exit 2; }
    work="$home/hlsl"
    mkdir -p "$work"
    cat > "$work/hlslc.c" <<'C'
#define COBJMACROS
#include <windows.h>
#include <d3dcompiler.h>
#include <stdio.h>
int main(int argc, char **argv) {
    WCHAR path[1024];
    int fails = 0;
    MultiByteToWideChar(CP_UNIXCP, 0, argv[1], -1, path, 1024);
    for (int i = 2; i + 1 < argc; i += 2) {
        ID3DBlob *code = NULL, *err = NULL;
        HRESULT hr = D3DCompileFromFile(path, NULL, D3D_COMPILE_STANDARD_FILE_INCLUDE, argv[i], argv[i + 1], 0, 0, &code, &err);
        printf("%s %s: %s\n", argv[i], argv[i + 1], FAILED(hr) ? "FAILED" : "ok");
        if (err) printf("%.*s\n", (int)ID3D10Blob_GetBufferSize(err), (char *)ID3D10Blob_GetBufferPointer(err));
        if (FAILED(hr)) fails++;
    }
    return fails;
}
C
    (cd "$work" && winegcc -o hlslc hlslc.c -ld3dcompiler -luuid)
    entries=()
    for module in quad opaque_quad shadow path_tile underline monochrome_sprite subpixel_sprite polychrome_sprite surface; do
        entries+=("${module}_vertex" vs_4_1 "${module}_fragment" ps_4_1)
    done
    cd "$here/vendor/gpui-src/crates/gpui_windows/src"
    WINEPREFIX="$home/wine" WINEDEBUG=-all WINEDLLOVERRIDES="winemenubuilder.exe=d;mscoree=d;mshtml=d" \
        DISPLAY= WAYLAND_DISPLAY= wine "$work/hlslc.exe.so" shaders.hlsl "${entries[@]}" 2>/dev/null
fi
