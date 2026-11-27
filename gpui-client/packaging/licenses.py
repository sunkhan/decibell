#!/usr/bin/env python3
"""Writes THIRD_PARTY_LICENSES.md for a build of the GPUI client: every Rust crate linked into the
`decibell` binary of the given flavour and target (from `cargo metadata`, normal dependencies only:
build scripts and tests ship nothing), with each crate's own licence files; the C libraries linked
statically (FFmpeg, dav1d, SVT-AV1, the NVIDIA and Vulkan headers compiled into FFmpeg) or shipped
beside the executable (Windows: vcpkg's DLLs), with their licence texts and what the LGPL asks of
a statically linked FFmpeg; and the bundled assets (fonts, Twemoji, emoji data, KaTeX, icons).

    python3 -I packaging/licenses.py --target x86_64-unknown-linux-gnu --c-licenses vendor/ffmpeg \\
        --out THIRD_PARTY_LICENSES.md          # from gpui-client/
    python3 -I packaging/licenses.py --target x86_64-pc-windows-msvc \\
        --vcpkg "$VCPKG_ROOT/installed/x64-windows" --out THIRD_PARTY_LICENSES.md

Fails (exit 1) if a third-party crate has no licence at all or only a copyleft one (GPL / AGPL /
LGPL / SSPL without a permissive alternative): such a crate must not be in the binary unnoticed."""

import argparse
import hashlib
import json
import pathlib
import re
import subprocess
import sys

HERE = pathlib.Path(__file__).resolve().parent
CLIENT = HERE.parent
REPO = CLIENT.parent

COPYLEFT = re.compile(r"\b(A?GPL|LGPL|SSPL)")
LICENSE_FILE = re.compile(r"^(LICEN[CS]E|COPYING|NOTICE|COPYRIGHT|UNLICENSE)([-._].*)?$", re.I)
# Our own crates (path dependencies of this repository): the project's licence, not a third party's.
OURS = re.compile(r"^decibell(-|_|$)")


def metadata(target: str, features: str) -> dict:
    cmd = ["cargo", "metadata", "--format-version", "1", "--locked", "--filter-platform", target,
           "--no-default-features", "--features", features]
    # Bytes, decoded as UTF-8 here: `text=True` decodes with the locale's code page (cp1252 on the Windows
    # runner, which failed on the metadata's first non-ASCII author name).
    return json.loads(subprocess.run(cmd, cwd=CLIENT, check=True, capture_output=True).stdout.decode("utf-8"))


def linked_packages(meta: dict) -> list:
    packages = {p["id"]: p for p in meta["packages"]}
    nodes = {n["id"]: n for n in meta["resolve"]["nodes"]}
    root = next(p["id"] for p in meta["packages"] if p["name"] == "decibell-app")
    seen, stack = set(), [root]
    while stack:
        pid = stack.pop()
        if pid in seen:
            continue
        seen.add(pid)
        for dep in nodes[pid]["deps"]:
            if any(kind["kind"] is None for kind in dep["dep_kinds"]):
                stack.append(dep["pkg"])
    return sorted((packages[i] for i in seen), key=lambda p: (p["name"], p["version"]))


def license_files(package: dict) -> list:
    root = pathlib.Path(package["manifest_path"]).parent
    files = []
    if package.get("license_file"):
        files.append(root / package["license_file"])
    for path in sorted(root.iterdir()):
        if path.is_file() and LICENSE_FILE.match(path.name) and path not in files:
            files.append(path)
    return files


def read(path: pathlib.Path) -> str:
    return path.read_text(encoding="utf-8", errors="replace").strip()


def rust_section(packages: list) -> tuple:
    problems, texts, rows = [], {}, []
    for p in packages:
        if OURS.match(p["name"]):
            continue
        expression = p["license"] or ""
        if not expression and not p.get("license_file"):
            problems.append(f"{p['name']} {p['version']}: no licence")
        permissive_choice = any(not COPYLEFT.search(part) for part in re.split(r"\s+OR\s+|/", expression)) if expression else True
        if not permissive_choice:
            problems.append(f"{p['name']} {p['version']}: copyleft only ({expression})")
        refs = []
        for path in license_files(p):
            text = read(path)
            key = hashlib.sha256(text.encode()).hexdigest()[:12]
            texts.setdefault(key, (text, []))[1].append(f"{p['name']} {p['version']}")
            refs.append(key)
        rows.append((p["name"], p["version"], expression or f"see {p.get('license_file')}", p.get("repository") or "", refs))
    out = ["## Rust crates", "",
           f"{len(rows)} crates from crates.io (and GPUI's, from Zed at `gpui-client/GPUI_PIN`) are compiled into the binary. "
           "Where a crate offers a choice of licences, Decibell uses it under the permissive one (e.g. `self_cell`: Apache-2.0). "
           "Each crate's own licence files follow the table, once per distinct text.", "",
           "| Crate | Version | Licence | Source | Texts |", "|---|---|---|---|---|"]
    for name, version, expression, repo, refs in rows:
        out.append(f"| {name} | {version} | {expression} | {repo} | {', '.join(f'[{r}](#t-{r})' for r in refs) or '—'} |")
    out += ["", "### Licence texts", ""]
    for key, (text, users) in sorted(texts.items(), key=lambda kv: kv[1][1][0]):
        shown = ", ".join(users[:12]) + (f" and {len(users) - 12} more" if len(users) > 12 else "")
        out += [f'<a id="t-{key}"></a>', f"#### {key}: {shown}", "", "```text", text, "```", ""]
    return out, problems


LGPL_TERMS = """\
### What the LGPL asks of this binary, and how this package answers it

FFmpeg is licensed under the GNU Lesser General Public License, version 2.1 or later (the text is
below). The Linux `decibell` executable links it **statically**; the Windows package ships it as
**DLLs** beside `decibell.exe`. The build is configured LGPL-only: no `--enable-gpl`,
`--enable-nonfree` or `--enable-version3` (`gpui-client/tools/build-ffmpeg.sh` refuses them and checks
FFmpeg's `config.h`; the Windows overlay ports in `gpui-client/tools/vcpkg-overlay` request no GPL
feature).

For a work that uses the Library linked into one executable (LGPL-2.1 §6), the distributor must:

1. give **prominent notice** with each copy that the Library is used in it and is covered by the LGPL,
   and supply a **copy of the licence** — this section and the text below, installed with the
   package (`/usr/share/licenses/decibell-native/`, the AppImage's and tarball's `licenses/`, the
   Windows install folder);
2. if the work displays copyright notices while it runs, include the Library's among them — the
   client shows none (About shows only its own name and version);
3. **let the user modify the Library and relink** the work with the modified Library, by one of:
   (a) accompanying the work with the complete source of the Library and the work in a form that
   can be relinked; (b) a shared-library mechanism (the Windows package: the FFmpeg DLLs can be
   replaced in place); (c) a written offer, valid for three years, to give those materials;
   (d) offering equivalent access to copy them from the same place as the binary. **This package uses
   (d) on Linux**: every release on the same GitHub release page carries
   - the exact FFmpeg, dav1d and SVT-AV1 source archives the binary was built from
     (`ffmpeg-<version>.tar.xz`, `dav1d-<version>.tar.bz2`, `SVT-AV1-<version>.tar.gz`, checked against the
     SHA-256 values pinned in `tools/build-ffmpeg.sh`), and the script with the exact configure line;
   - the complete source of the client at the release's tag, in the same repository, from which the
     executable is rebuilt against a modified FFmpeg: put the modified FFmpeg in `gpui-client/vendor/ffmpeg`
     (`tools/build-ffmpeg.sh` builds it from a source tree), then
     `cargo build --release -p decibell-app --no-default-features --features ship`
     (`gpui-client/PACKAGING.md`, "Building it yourself");
4. **not forbid** modification of the work for the user's own use or **reverse engineering** for
   debugging such modifications — nothing in Decibell's terms does;
5. not impose further restrictions on the recipients' exercise of these rights.

The project's own licence is the owner's to state: the repository has no `LICENSE` file (see
`gpui-client/PACKAGING.md`, "Decisions for the owner"). Whatever it is, it must allow points 3–5 for
the parts that link FFmpeg.
"""


def header_comment(path: pathlib.Path) -> str:
    """The licence comment at the top of a C header."""
    text = path.read_text(encoding="utf-8", errors="replace")
    match = re.match(r"\s*/\*(.*?)\*/", text, re.S)
    if not match:
        return ""
    return "\n".join(line.strip().lstrip("*").rstrip() for line in match.group(1).splitlines()).strip()


def c_section_linux(prefix: pathlib.Path) -> list:
    stamp = {}
    stamp_file = prefix / ".decibell-ffmpeg-stamp"
    if stamp_file.is_file():
        for line in stamp_file.read_text(encoding="utf-8").splitlines():
            parts = line.split()
            if len(parts) == 3 and not line.startswith("--"):
                stamp[parts[0]] = (parts[1], parts[2])
    lic = prefix / "share/licenses"
    out = ["## C libraries linked statically (Linux)", ""]

    def version(name):
        v = stamp.get(name)
        return f"{v[0]} (source SHA-256 `{v[1]}`)" if v else "(see tools/build-ffmpeg.sh)"

    out += [f"### FFmpeg {version('ffmpeg')} — LGPL-2.1-or-later", "",
            "libavcodec, libavformat, libavfilter, libavutil, libswscale, libswresample: decode of streams "
            "(Vulkan Video, NVDEC, VA-API hwaccels; H.264 / HEVC software), the NVENC / VA-API / SVT-AV1 encoders, "
            "attachment playback demuxers and audio decoders. https://ffmpeg.org", "", LGPL_TERMS, "",
            "```text", read(lic / "ffmpeg/COPYING.LGPLv2.1"), "```", ""]
    out += [f"### dav1d {version('dav1d')} — BSD-2-Clause", "", "The AV1 software decoder. https://code.videolan.org/videolan/dav1d", "",
            "```text", read(lic / "dav1d/COPYING"), "```", ""]
    out += [f"### SVT-AV1 {version('svt-av1')} — BSD-3-Clause-Clear, with the Alliance for Open Media Patent License 1.0", "",
            "The software screen-share encoder of a machine without a working hardware encoder. https://gitlab.com/AOMediaCodec/SVT-AV1", "",
            "```text", read(lic / "SVT-AV1/LICENSE.md"), "```", "", "```text", read(lic / "SVT-AV1/PATENTS.md"), "```", ""]
    nv = prefix / "include/ffnvcodec/nvEncodeAPI.h"
    if nv.is_file():
        out += [f"### NVIDIA codec headers {stamp.get('nv-codec-headers', ('',))[0]} — MIT", "",
                "Compiled into FFmpeg's NVENC / NVDEC code (the driver's libraries are loaded at run time, not shipped).", "",
                "```text", header_comment(nv), "```", ""]
    vk = prefix / "include/vulkan/vulkan_core.h"
    if vk.is_file():
        out += [f"### Vulkan headers {stamp.get('vulkan-headers', ('',))[0]} — Apache-2.0 OR MIT", "",
                "Compiled into FFmpeg's Vulkan Video code.", "", "```text", header_comment(vk), "```", ""]
    out += ["### Not shipped", "",
            "Linked dynamically from the system, never bundled: libc, libm, libgcc_s, libxkbcommon(-x11), libxcb, "
            "libva / libva-drm, libdrm, libopus, libpipewire-0.3, libasound; loaded at run time: the Vulkan loader, "
            "the GPU driver (NVIDIA's libnvidia-encode / libnvcuvid for NVENC / NVDEC), libwayland, libEGL.", ""]
    return out


def c_section_windows(installed: pathlib.Path) -> list:
    out = ["## Libraries shipped beside decibell.exe (Windows, from vcpkg)", "",
           "FFmpeg is linked dynamically on Windows: its DLLs are in the install folder and can be replaced by "
           "a modified build (LGPL-2.1 §6(b)).", "", LGPL_TERMS, ""]
    share = installed / "share"
    for port in sorted(p.name for p in share.iterdir() if (p / "copyright").is_file()) if share.is_dir() else []:
        if port.startswith("vcpkg-"):
            continue
        out += [f"### {port}", "", "```text", read(share / port / "copyright"), "```", ""]
    return out


def assets_section() -> list:
    a = CLIENT / "assets"
    items = [
        ("Inter, Mona Sans, JetBrains Mono (fonts; Decibell Inter / Sans / Mono are instances of them)", "SIL OFL 1.1",
         [a / "fonts/OFL-Inter.txt", a / "fonts/OFL-MonaSans.txt", a / "fonts/OFL-JetBrainsMono.txt"]),
        ("KaTeX fonts", "SIL OFL 1.1 / MIT", [a / "fonts/katex/LICENSE.txt"]),
        ("Twemoji 15.0.0 graphics", "CC-BY 4.0 — Twemoji by Twitter, Inc and other contributors, licensed under CC-BY 4.0 (https://creativecommons.org/licenses/by/4.0/)",
         [a / "twemoji/LICENSE-package.txt"]),
        ("@emoji-mart/data 1.2.1 (the emoji picker's dataset)", "MIT", [a / "emoji-data/LICENSE"]),
        ("Lucide icons", "ISC (+ MIT for Feather-derived icons)", [a / "icons/lucide/LICENSE"]),
        ("highlight.js grammars (ported to Rust in decibell-highlight)", "BSD-3-Clause", [CLIENT / "crates/decibell-highlight/LICENSE-highlight.js"]),
        ("KaTeX 0.18.4 (ported to Rust in decibell-tex)", "MIT", [CLIENT / "crates/decibell-tex/LICENSE-KATEX"]),
    ]
    out = ["## Bundled assets", ""]
    for what, licence, files in items:
        out += [f"### {what} — {licence}", ""]
        for f in files:
            if f.is_file():
                out += ["```text", read(f), "```", ""]
            else:
                out += [f"(licence file `{f.relative_to(REPO)}` not present in this checkout: run `node gpui-client/tools/fetch-assets.mjs`)", ""]
    return out


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--target", required=True)
    parser.add_argument("--features", default="decibell-app/ship")
    parser.add_argument("--c-licenses", type=pathlib.Path, help="the Linux FFmpeg prefix (vendor/ffmpeg)")
    parser.add_argument("--vcpkg", type=pathlib.Path, help="vcpkg's installed/<triplet> directory (Windows)")
    parser.add_argument("--version", default="")
    parser.add_argument("--out", type=pathlib.Path, required=True)
    args = parser.parse_args()

    packages = linked_packages(metadata(args.target, args.features))
    rust, problems = rust_section(packages)
    out = ["# Third-party licences", "",
           f"Decibell {args.version} for `{args.target}`: what the `decibell` executable contains or ships with "
           "that others wrote, and under which terms. Generated by `gpui-client/packaging/licenses.py`; do not edit.", ""]
    if args.c_licenses:
        out += c_section_linux(args.c_licenses)
    if args.vcpkg:
        out += c_section_windows(args.vcpkg)
    out += assets_section()
    out += rust
    args.out.write_text("\n".join(out) + "\n", encoding="utf-8")
    third = sum(1 for p in packages if not OURS.match(p["name"]))
    print(f"licenses: {third} crates, {args.out} ({args.out.stat().st_size // 1024} KiB)")
    if problems:
        print("licenses: refused:", *problems, sep="\n  ", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
