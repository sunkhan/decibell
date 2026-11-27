#!/usr/bin/env python3
"""Writes the native updater's feed (decibell-release's `feed.rs` reads it) for a release: the
version, the release page, and for each self-updating package its download URL, size and SHA-512
(base64, as electron-updater's latest*.yml carries it).

    feed.py --version 0.9.5 --repo sunkhan/decibell --tag native-v0.9.5 --dir <packages> --out decibell-native.json

The bytes written are what gets signed (`openssl pkeyutl -sign -rawin`), so the file is written
once, compactly, and never re-serialised afterwards."""
import argparse
import base64
import hashlib
import json
import pathlib
import sys

# The feed's keys (`feed::artifact_key`) → the file each names.
ARTIFACTS = {
    "linux-x86_64-appimage": "Decibell-{v}-x86_64.AppImage",
    "windows-x86_64-nsis": "Decibell-{v}-x64-setup.exe",
}


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--version", required=True)
    parser.add_argument("--repo", required=True)
    parser.add_argument("--tag", required=True)
    parser.add_argument("--dir", type=pathlib.Path, required=True)
    parser.add_argument("--out", type=pathlib.Path, required=True)
    args = parser.parse_args()
    base = f"https://github.com/{args.repo}/releases/download/{args.tag}"
    files = {}
    for key, pattern in ARTIFACTS.items():
        path = args.dir / pattern.format(v=args.version)
        if not path.is_file():
            print(f"feed: {path} is missing", file=sys.stderr)
            return 1
        data = path.read_bytes()
        files[key] = {
            "url": f"{base}/{path.name}",
            "size": len(data),
            "sha512": base64.b64encode(hashlib.sha512(data).digest()).decode(),
        }
    feed = {
        "schema": 1,
        "version": args.version,
        "release_page": f"https://github.com/{args.repo}/releases/tag/{args.tag}",
        "files": files,
    }
    args.out.write_bytes(json.dumps(feed, separators=(",", ":"), sort_keys=True).encode())
    return 0


if __name__ == "__main__":
    sys.exit(main())
