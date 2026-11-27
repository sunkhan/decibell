#!/usr/bin/env python3
"""Where the bytes of a `decibell` binary go, by crate (what `cargo bloat --crates` reports, without
installing it): every sized symbol of an **unstripped** release build (`CARGO_PROFILE_RELEASE_STRIP=false`),
demangled, attributed to the crate its path starts with (a generic instance counts for the crate
whose function it is), C symbols to the static archive that defines them (FFmpeg's libraries,
dav1d, SVT-AV1), the rest to "[other]". Plus the sections, for the bytes no symbol covers.

    python3 -I packaging/size-by-crate.py <unstripped decibell> [--archives vendor/ffmpeg/lib] [--top 40] [--json out.json]"""
import argparse
import collections
import json
import pathlib
import re
import subprocess

RUST_PATH = re.compile(r"^(?:<)?(?:&(?:mut )?)?(?:dyn )?([A-Za-z_][A-Za-z0-9_]*)::")


def crate_of(name: str) -> str:
    # `<impl Trait for Type>` / `<Type as Trait>::f`: the first path names the code's owner closely enough.
    name = name.lstrip("<&*").removeprefix("mut ").removeprefix("dyn ")
    m = RUST_PATH.match(name)
    return m.group(1) if m else ""


def archive_symbols(directory: pathlib.Path) -> dict:
    owners = {}
    for archive in sorted(directory.glob("*.a")):
        # Local symbols too (most of a codec's functions are static); a name two archives define
        # goes to the first.
        out = subprocess.run(["nm", "--defined-only", str(archive)], capture_output=True, text=True).stdout
        for line in out.splitlines():
            parts = line.split()
            if len(parts) == 3:
                owners.setdefault(parts[2], archive.stem.removeprefix("lib"))
    return owners


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("binary", type=pathlib.Path)
    parser.add_argument("--archives", type=pathlib.Path)
    parser.add_argument("--top", type=int, default=40)
    parser.add_argument("--json", type=pathlib.Path)
    args = parser.parse_args()
    owners = archive_symbols(args.archives) if args.archives else {}
    raw = subprocess.run(["nm", "-S", "--size-sort", str(args.binary)], capture_output=True, text=True).stdout
    demangled = subprocess.run(["nm", "-S", "--size-sort", "-C", str(args.binary)], capture_output=True, text=True).stdout
    sizes = collections.Counter()
    for raw_line, line in zip(raw.splitlines(), demangled.splitlines()):
        parts = line.split(maxsplit=3)
        raw_parts = raw_line.split(maxsplit=3)
        if len(parts) < 4 or parts[2].lower() not in "tdrbv" and parts[2] not in "TtDdRrBbVvWw":
            continue
        size = int(parts[1], 16)
        if parts[2] in "Bb":
            continue  # .bss: no bytes in the file
        name, mangled = parts[3], raw_parts[3] if len(raw_parts) == 4 else parts[3]
        if mangled == "__FRAME_END__":
            key = "[unwind tables: .eh_frame]"
        elif mangled in owners:
            key = f"[C] {owners[mangled]}"
        elif mangled.startswith(("_ZN", "_R")):
            key = crate_of(name) or "[rust: unattributed]"
        elif re.match(r"^(ff_|av_|avpriv_|swr_|sws_|avcodec_|avformat_|avfilter_|avutil_)", mangled):
            key = "[C] ffmpeg (other)"
        elif re.match(r"^(dav1d_|svt_|eb_|Svt)", mangled):
            key = "[C] codecs (other)"
        elif mangled.startswith("ring_core_") or mangled in ("ecp_nistz256_precomputed",):
            key = "ring"
        else:
            key = "[other C / runtime]"
        sizes[key] += size
    total = sum(sizes.values())
    sections = subprocess.run(["size", "-A", str(args.binary)], capture_output=True, text=True).stdout
    print(f"symbols: {total / 1e6:.1f} MB in {len(sizes)} groups")
    for key, size in sizes.most_common(args.top):
        print(f"{size / 1e6:8.2f} MB  {100 * size / total:5.1f} %  {key}")
    print(sections)
    if args.json:
        args.json.write_text(json.dumps(dict(sizes.most_common()), indent=1))


if __name__ == "__main__":
    main()
