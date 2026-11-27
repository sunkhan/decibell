#!/usr/bin/env python3
"""Builds the .deb from a staged root without dpkg-deb (so it builds and is checked the same way on
any Linux): `debian-binary`, `control.tar.xz` and `data.tar.xz` in an `ar` archive, files owned by
root, times fixed (SOURCE_DATE_EPOCH, default 0).

    mkdeb.py --root <staged /> --version <v> --out decibell-native_<v>_amd64.deb

The dependencies are the shared libraries `decibell` links (packaging/check-ship.sh keeps that list)
by Ubuntu 24.04's / Debian 13's package names, with the pre-t64 names as alternatives, plus the
Vulkan loader (loaded at run time)."""
import argparse
import io
import os
import pathlib
import tarfile
import time

DEPENDS = [
    "libc6 (>= 2.39)",
    "libgcc-s1",
    "libxkbcommon0",
    "libxkbcommon-x11-0",
    "libxcb1",
    "libva2",
    "libva-drm2",
    "libdrm2",
    "libopus0",
    "libpipewire-0.3-0t64 | libpipewire-0.3-0",
    "libasound2t64 | libasound2",
    "libvulkan1",
]
RECOMMENDS = ["mesa-vulkan-drivers | nvidia-driver-libs | libnvidia-gl"]


def tar_xz(entries, mtime: int) -> bytes:
    buffer = io.BytesIO()
    with tarfile.open(fileobj=buffer, mode="w:xz", format=tarfile.GNU_FORMAT) as tar:
        for info, data in entries:
            info.mtime = mtime
            info.uid = info.gid = 0
            info.uname = info.gname = "root"
            tar.addfile(info, io.BytesIO(data) if data is not None else None)
    return buffer.getvalue()


def data_entries(root: pathlib.Path):
    entries, size = [], 0
    for path in sorted(root.rglob("*")):
        rel = "./" + path.relative_to(root).as_posix()
        info = tarfile.TarInfo(rel)
        if path.is_symlink():
            info.type = tarfile.SYMTYPE
            info.linkname = os.readlink(path)
            entries.append((info, None))
        elif path.is_dir():
            info.type = tarfile.DIRTYPE
            info.mode = 0o755
            entries.append((info, None))
        else:
            data = path.read_bytes()
            info.size = len(data)
            info.mode = 0o755 if os.access(path, os.X_OK) else 0o644
            size += len(data)
            entries.append((info, data))
    top = tarfile.TarInfo("./")
    top.type, top.mode = tarfile.DIRTYPE, 0o755
    return [(top, None)] + entries, size


def ar(members) -> bytes:
    out = bytearray(b"!<arch>\n")
    for name, data in members:
        header = f"{name:<16}{0:<12}{0:<6}{0:<6}{'100644':<8}{len(data):<10}`\n".encode()
        out += header + data
        if len(data) % 2:
            out += b"\n"
    return bytes(out)


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--root", type=pathlib.Path, required=True)
    parser.add_argument("--version", required=True)
    parser.add_argument("--out", type=pathlib.Path, required=True)
    args = parser.parse_args()
    mtime = int(os.environ.get("SOURCE_DATE_EPOCH", "0"))
    entries, size = data_entries(args.root)
    control = "\n".join([
        "Package: decibell-native",
        f"Version: {args.version}",
        "Architecture: amd64",
        "Maintainer: sunkhan <gunhanserhat@gmail.com>",
        f"Installed-Size: {(size + 1023) // 1024}",
        f"Depends: {', '.join(DEPENDS)}",
        f"Recommends: {', '.join(RECOMMENDS)}",
        # Electron's package puts its own /usr/bin/decibell link and decibell:// handler in place.
        "Conflicts: decibell-electron-client",
        "Section: net",
        "Priority: optional",
        "Homepage: https://github.com/sunkhan/decibell",
        "Description: Decibell, the native client",
        " Text channels, voice, screen sharing and direct messages with the communities you join.",
        " This is the native (GPUI) client.",
        "",
    ]).encode()
    md5 = []
    import hashlib
    for info, data in entries:
        if data is not None:
            md5.append(f"{hashlib.md5(data).hexdigest()}  {info.name[2:]}")
    md5sums = ("\n".join(md5) + "\n").encode()

    def file(name, data, mode=0o644):
        info = tarfile.TarInfo(name)
        info.size, info.mode = len(data), mode
        return info, data

    top = tarfile.TarInfo("./")
    top.type, top.mode = tarfile.DIRTYPE, 0o755
    control_tar = tar_xz([(top, None), file("./control", control), file("./md5sums", md5sums)], mtime)
    data_tar = tar_xz(entries, mtime)
    args.out.write_bytes(ar([("debian-binary", b"2.0\n"), ("control.tar.xz", control_tar), ("data.tar.xz", data_tar)]))


if __name__ == "__main__":
    main()
