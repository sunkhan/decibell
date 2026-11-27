#!/usr/bin/env python3
"""Finds every DLL `decibell.exe` needs that Windows doesn't provide, by walking the PE import
tables (normal and delay-load) from the executable through each DLL found, and copies them beside
it. The list therefore comes from the install tree vcpkg built (FFmpeg's, dav1d's, SVT-AV1's DLLs
and whatever they import), never from memory.

    python -I packaging/windows/dlls.py --exe stage/decibell.exe --search "$VCPKG_ROOT/installed/x64-windows/bin" \
        [--redist "<VS>\\VC\\Redist\\MSVC\\<v>\\x64\\Microsoft.VC143.CRT"] [--system C:\\Windows\\System32] \
        [--copy-to stage] [--list dlls.txt]

A name is resolved as: an API set (`api-ms-win-*`, `ext-ms-*`) or a DLL in the system directory →
Windows' own, skipped; a DLL in `--search` → copied, and its imports walked; the Visual C++ runtime
(`vcruntime140*.dll`, `msvcp140*.dll`, `concrt140.dll`) → copied app-local from `--redist`
(Microsoft's redistributable files, which may be shipped beside an application). Anything else
fails the step: a DLL the package would miss on a clean machine."""
import argparse
import pathlib
import shutil
import struct
import sys

VC_RUNTIME = ("vcruntime140", "msvcp140", "concrt140", "vccorlib140")


def imports(path: pathlib.Path) -> list:
    data = path.read_bytes()
    if data[:2] != b"MZ":
        raise ValueError(f"{path}: not a PE file")
    pe = struct.unpack_from("<I", data, 0x3C)[0]
    if data[pe:pe + 4] != b"PE\0\0":
        raise ValueError(f"{path}: no PE header")
    sections_count = struct.unpack_from("<H", data, pe + 6)[0]
    optional_size = struct.unpack_from("<H", data, pe + 20)[0]
    optional = pe + 24
    magic = struct.unpack_from("<H", data, optional)[0]
    directories = optional + (112 if magic == 0x20B else 96)
    sections = []
    table = optional + optional_size
    for i in range(sections_count):
        s = table + 40 * i
        vsize, vaddr, rsize, raw = struct.unpack_from("<IIII", data, s + 8)
        sections.append((vaddr, max(vsize, rsize), raw))

    def offset(rva: int) -> int:
        for vaddr, size, raw in sections:
            if vaddr <= rva < vaddr + size:
                return raw + rva - vaddr
        raise ValueError(f"{path}: RVA {rva:#x} in no section")

    def cstring(rva: int) -> str:
        at = offset(rva)
        return data[at:data.index(b"\0", at)].decode("ascii", "replace")

    names = []
    # Directory 1: imports (20-byte descriptors, Name at +12); 13: delay-load (32 bytes, Name at +4).
    for index, size, name_at in ((1, 20, 12), (13, 32, 4)):
        rva, length = struct.unpack_from("<II", data, directories + 8 * index)
        if not rva or not length:
            continue
        at = offset(rva)
        while True:
            entry = data[at:at + size]
            if len(entry) < size or entry == b"\0" * size:
                break
            name_rva = struct.unpack_from("<I", entry, name_at)[0]
            if name_rva:
                names.append(cstring(name_rva))
            at += size
    return names


def find(name: str, directories: list):
    for d in directories:
        if d is None:
            continue
        for candidate in d.iterdir():
            if candidate.name.lower() == name.lower():
                return candidate
    return None


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--exe", type=pathlib.Path, required=True)
    parser.add_argument("--search", type=pathlib.Path, action="append", default=[])
    parser.add_argument("--redist", type=pathlib.Path)
    parser.add_argument("--system", type=pathlib.Path, default=pathlib.Path("C:/Windows/System32"))
    parser.add_argument("--copy-to", type=pathlib.Path)
    parser.add_argument("--list", type=pathlib.Path)
    args = parser.parse_args()

    shipped, missing, system = {}, [], set()
    queue = [args.exe]
    seen = set()
    while queue:
        module = queue.pop()
        for name in imports(module):
            key = name.lower()
            if key in seen:
                continue
            seen.add(key)
            if key.startswith(("api-ms-win-", "ext-ms-")):
                continue
            found = find(name, args.search)
            if found:
                shipped[key] = found
                queue.append(found)
            elif key.startswith(VC_RUNTIME):
                redist = find(name, [args.redist]) if args.redist and args.redist.is_dir() else None
                if redist:
                    shipped[key] = redist
                elif args.system.is_dir() and find(name, [args.system]):
                    print(f"dlls: {name} only in {args.system}: not redistributed (pass --redist)", file=sys.stderr)
                    missing.append(name)
                else:
                    missing.append(name)
            elif args.system.is_dir() and find(name, [args.system]):
                system.add(name)
            else:
                missing.append(name)
    for key, path in sorted(shipped.items()):
        print(f"dlls: ship {path.name}  ({path})")
        if args.copy_to:
            shutil.copy2(path, args.copy_to / path.name)
    print(f"dlls: Windows provides {', '.join(sorted(system)) or '(not checked)'}")
    if args.list:
        args.list.write_text("".join(f"{p.name}\n" for _, p in sorted(shipped.items())))
    if missing:
        print("dlls: not found anywhere: " + ", ".join(missing), file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
