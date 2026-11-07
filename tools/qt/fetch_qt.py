"""Install a prebuilt Qt from download.qt.io without an account.

aqtinstall 3.3.0 can't read Qt's per-arch repository layout (qt6_6120/qt6_6120_msvc2022_64/,
used since 6.8-ish): it asks for qt6_6120/qt6_6120/Updates.xml and fails. This reads the
per-arch Updates.xml, downloads the chosen packages' .7z archives, checks each against its
published .sha1 and extracts them into --dest/<version>/<arch> (the per-arch archives carry no
<version>/<arch>/ prefix of their own, unlike the older layout).

Run with uv so nothing is installed system-wide (truststore: uv's managed Python ships no CA
bundle, so verify TLS against the OS certificate store):
  uv run --no-project --python 3.12 --with py7zr --with truststore python -I tools/qt/fetch_qt.py \
      --version 6.12.0 --arch msvc2022_64 --dest C:\\Qt --addons qtshadertools qtimageformats
"""
import argparse
import hashlib
import pathlib
import sys
import urllib.request
import xml.etree.ElementTree as ET

import py7zr

try:
    import truststore

    truststore.inject_into_ssl()
except ImportError:
    pass

BASE = "https://download.qt.io/online/qtsdkrepository"


def get(url: str) -> bytes:
    req = urllib.request.Request(url, headers={"User-Agent": "decibell-fetch-qt"})
    with urllib.request.urlopen(req, timeout=120) as r:
        return r.read()


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--version", required=True)            # e.g. 6.12.0
    ap.add_argument("--arch", default="msvc2022_64")       # repo folder suffix
    ap.add_argument("--host", default="windows_x86")
    ap.add_argument("--dest", required=True)
    ap.add_argument("--addons", nargs="*", default=[])     # e.g. qtshadertools qtimageformats
    ap.add_argument("--cache", default=None)
    a = ap.parse_args()

    v = a.version.replace(".", "")
    target_arch = "win64_" + a.arch if a.host.startswith("windows") else a.arch
    folder = f"{BASE}/{a.host}/desktop/qt6_{v}/qt6_{v}_{a.arch}"
    updates = ET.fromstring(get(f"{folder}/Updates.xml"))

    wanted = {f"qt.qt6.{v}.{target_arch}"}
    wanted |= {f"qt.qt6.{v}.addons.{m}.{target_arch}" for m in a.addons}
    found = {}
    for pu in updates.iter("PackageUpdate"):
        name = pu.findtext("Name")
        if name in wanted:
            found[name] = pu
    missing = wanted - found.keys()
    if missing:
        print("not in Updates.xml:", ", ".join(sorted(missing)), file=sys.stderr)
        return 1

    cache = pathlib.Path(a.cache) if a.cache else pathlib.Path(a.dest) / "_downloads"
    dest = pathlib.Path(a.dest) / a.version / a.arch
    dest.mkdir(parents=True, exist_ok=True)
    cache.mkdir(parents=True, exist_ok=True)
    for name, pu in sorted(found.items()):
        ver = pu.findtext("Version")
        archives = [x.strip() for x in (pu.findtext("DownloadableArchives") or "").split(",") if x.strip()]
        for arc in archives:
            url = f"{folder}/{name}/{ver}{arc}"
            local = cache / f"{ver}{arc}"
            if not local.exists():
                print("download", arc, flush=True)
                data = get(url)
                want = get(url + ".sha1").decode().split()[0].strip().lower()
                got = hashlib.sha1(data).hexdigest()
                if got != want:
                    print(f"sha1 mismatch for {arc}: {got} != {want}", file=sys.stderr)
                    return 1
                local.write_bytes(data)
            print("extract", arc, flush=True)
            with py7zr.SevenZipFile(local, "r") as z:
                z.extractall(dest)
    print("installed", a.version, a.arch, "to", dest)
    return 0


if __name__ == "__main__":
    sys.exit(main())
