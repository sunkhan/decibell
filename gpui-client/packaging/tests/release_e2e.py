#!/usr/bin/env python3
"""The updater and the crash reporter end to end, through a real `decibell` binary of a development
build (the `dev` feature: decibell-release's test hooks), against loopback fixtures: a fake GitHub
(the native channel's feed and its signature, the release download with ranges and cut connections,
Electron's `releases/latest` and `latest*.yml` beside them) and a fake Sentry envelope endpoint.
Never GitHub, never Sentry, never the owner's profile or keyring: every run has its own temporary
profile, HOME-independent, on a private D-Bus session.

    python3 -I packaging/tests/release_e2e.py --binary target/release/decibell [--only updater,apply,crash]

The updater cases open the app's window for a few seconds (the first check follows the first frame);
most crash cases panic before any window (the recovered ones open it). A release (optimised) build
is needed for the crash cases: its profile unwinds as a shipped build's does, and the panic hook ends
the process on a panic nothing catches.

Signing uses the owner's documented commands (`openssl genpkey` / `pkeyutl -sign -rawin`), so the
test also proves them (gpui-client/PACKAGING.md, "Signing key")."""

import argparse
import base64
import hashlib
import http.server
import json
import os
import pathlib
import re
import shutil
import signal
import subprocess
import sys
import tempfile
import threading
import time

ROOT = pathlib.Path(__file__).resolve().parents[3]
VERSION = json.loads((ROOT / "electron-client/package.json").read_text())["version"]
REPO = "sunkhan/decibell"
RESULTS = []


def check(name, ok, detail=""):
    RESULTS.append((name, ok, detail))
    print(f"{'PASS' if ok else 'FAIL'} {name}{': ' + detail if detail else ''}", flush=True)


# ── Fixtures ──────────────────────────────────────────────────────────────────────────────────

class Server:
    """A loopback HTTP server whose handler is a function of (method, path, headers, body)."""

    def __init__(self, handle):
        outer = self
        self.requests = []

        class Handler(http.server.BaseHTTPRequestHandler):
            protocol_version = "HTTP/1.1"

            def log_message(self, *_):
                pass

            def respond(self, method):
                length = int(self.headers.get("Content-Length") or 0)
                body = self.rfile.read(length) if length else b""
                outer.requests.append((method, self.path, dict(self.headers), body))
                status, headers, payload, cut = handle(method, self.path, self.headers, body)
                self.send_response(status)
                for k, v in headers.items():
                    self.send_header(k, v)
                self.send_header("Content-Length", str(len(payload)))
                self.send_header("Connection", "close")
                self.end_headers()
                self.wfile.write(payload[:cut] if cut is not None else payload)
                self.close_connection = True

            def do_GET(self):
                self.respond("GET")

            def do_POST(self):
                self.respond("POST")

        self.httpd = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.origin = f"http://127.0.0.1:{self.httpd.server_address[1]}"
        threading.Thread(target=self.httpd.serve_forever, daemon=True).start()

    def close(self):
        self.httpd.shutdown()


def openssl_key(dir):
    key = dir / "update.pem"
    subprocess.run(["openssl", "genpkey", "-algorithm", "ed25519", "-out", str(key)], check=True)
    der = subprocess.run(["openssl", "pkey", "-in", str(key), "-pubout", "-outform", "DER"], check=True, capture_output=True).stdout
    return key, base64.b64encode(der[-32:]).decode()


def openssl_sign(key, data):
    # Ed25519 signs in one shot: OpenSSL wants the message as a file (`-in`), as the owner's command has it.
    message = key.parent / "message"
    message.write_bytes(data)
    sig = subprocess.run(["openssl", "pkeyutl", "-sign", "-inkey", str(key), "-rawin", "-in", str(message)], check=True, capture_output=True).stdout
    return base64.b64encode(sig)


class FakeGitHub:
    """The native channel's feed for `version`, its download (cut after `cut_at` bytes on the first
    `cuts` requests; `serve` may differ from what the feed hashes), and Electron's releases."""

    def __init__(self, key, version, payload, serve=None, cuts=0, cut_at=0, bad_sig_key=None):
        self.downloads = 0
        name = f"Decibell-{version}-x86_64.AppImage"
        served = serve if serve is not None else payload
        self.server = Server(self.handle)
        url = f"{self.server.origin}/{REPO}/releases/download/native-v{version}/{name}"
        feed = {"schema": 1, "version": version, "release_page": f"https://github.com/{REPO}/releases/tag/native-v{version}",
                "files": {"linux-x86_64-appimage": {"url": url, "size": len(payload), "sha512": base64.b64encode(hashlib.sha512(payload).digest()).decode()}}}
        self.feed = json.dumps(feed, separators=(",", ":")).encode()
        self.sig = openssl_sign(bad_sig_key or key, self.feed)
        self.served, self.name, self.cuts, self.cut_at = served, name, cuts, cut_at

    def handle(self, method, path, headers, body):
        if path.endswith("/releases/latest"):
            return 302, {"Location": f"/{REPO}/releases/tag/ev0.9.9"}, b"", None
        if "/releases/download/ev" in path or path.endswith(".yml"):
            return 200, {}, b"version: 0.9.9\n", None
        if path == f"/{REPO}/releases/download/native-latest/decibell-native.json":
            return 200, {}, self.feed, None
        if path == f"/{REPO}/releases/download/native-latest/decibell-native.json.sig":
            return 200, {}, self.sig, None
        if path.endswith(self.name):
            n = self.downloads
            self.downloads += 1
            start = 0
            m = re.match(r"bytes=(\d+)-", headers.get("Range") or "")
            if m:
                start = int(m.group(1))
            body = self.served[start:]
            status, extra = (206, {"Content-Range": f"bytes {start}-{len(self.served) - 1}/{len(self.served)}"}) if start else (200, {})
            return status, extra, body, (self.cut_at if n < self.cuts else None)
        return 404, {}, b"", None


# ── Running the app ───────────────────────────────────────────────────────────────────────────

def run_app(binary, profile, env_extra, until, timeout=60, cwd=None, bus=True, args=(), alive=None):
    """Starts the app on a private D-Bus session; returns (lines, returncode) once `until(lines)` or
    the timeout; the whole process group is then terminated. `alive`, a list, gets whether the
    process was still running when `until` held."""
    env = {k: v for k, v in os.environ.items() if not k.startswith(("DECIBELL_", "APPIMAGE"))}
    env.update({"DECIBELL_USER_DATA_DIR": str(profile), "DECIBELL_LOG": "info", "DECIBELL_TEST_UPDATE_DELAY_MS": "200"})
    env.update(env_extra)
    # A private session bus for runs that reach the UI; the crash cases end before GPUI starts.
    cmd = ["dbus-run-session", "--", str(binary)] if bus and shutil.which("dbus-run-session") else [str(binary)]
    cmd += [str(a) for a in args]
    proc = subprocess.Popen(cmd, env=env, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True, start_new_session=True, cwd=cwd)
    lines = []
    deadline = time.monotonic() + timeout

    def reader():
        for line in proc.stdout:
            lines.append(line.rstrip("\n"))

    t = threading.Thread(target=reader, daemon=True)
    t.start()
    while time.monotonic() < deadline:
        if until(lines):
            if alive is not None:
                alive.append(proc.poll() is None)
            break
        if proc.poll() is not None:
            break
        time.sleep(0.05)
    code = proc.poll()
    if code is None:
        os.killpg(proc.pid, signal.SIGTERM)
        try:
            proc.wait(5)
        except subprocess.TimeoutExpired:
            os.killpg(proc.pid, signal.SIGKILL)
            proc.wait()
    else:
        # The app may have left children (a trial start): end the whole group.
        try:
            os.killpg(proc.pid, signal.SIGTERM)
        except ProcessLookupError:
            pass
    t.join(2)
    return lines, code


def states(lines):
    return [l[len("decibell: update "):] for l in lines if l.startswith("decibell: update ")]


def done(lines):
    return any(re.match(r"decibell: update (NotAvailable|Downloaded|Error)", l) for l in lines)


# ── Cases ─────────────────────────────────────────────────────────────────────────────────────

def updater_cases(binary, work, key, public):
    payload = os.urandom(3 * 1024 * 1024 + 123)

    def profile_env(name, kind="appimage"):
        d = work / name
        (d / "apps").mkdir(parents=True)
        appimage = d / "apps/Decibell.AppImage"
        appimage.write_bytes(b"stand-in")
        return d / "profile", {"DECIBELL_TEST_INSTALL_KIND": kind, "APPIMAGE": str(appimage), "DECIBELL_TEST_UPDATE_PUBLIC_KEY": public}

    # 1. No update: the channel's version is this one; then About's Check now checks again.
    gh = FakeGitHub(key, VERSION, payload)
    profile, env = profile_env("no-update")
    env["DECIBELL_TEST_UPDATE_ORIGIN"] = gh.server.origin
    control = work / "no-update" / "control"
    os.mkfifo(control)
    env["DECIBELL_DEV_CONTROL"] = str(control)
    asked = []

    def check_again(ls):
        s = states(ls)
        if not asked and s and s[-1].startswith("NotAvailable"):
            with open(control, "w") as fifo:
                fifo.write("update-check\n")
            asked.append(len(s))
        return asked and len(states(ls)) >= asked[0] + 2 and states(ls)[-1].startswith("NotAvailable")

    lines, _ = run_app(binary, profile, env, check_again)
    s = states(lines)
    check("updater: no update", s[:2] == ["Idle", "Checking"] and asked and s[asked[0] - 1].startswith("NotAvailable"), " → ".join(s))
    feeds = [p for (_, p, _, _) in gh.server.requests if p.endswith("decibell-native.json")]
    check("updater: Check now checks again", asked and s[asked[0]:] [:1] == ["Checking"] and s[-1].startswith("NotAvailable") and len(feeds) == 2,
          f"{' → '.join(s[asked[0]:]) if asked else 'never asked'}; feed fetched {len(feeds)} times")
    gh.server.close()

    # 2. Available → downloaded → verified → staged; Electron's releases never asked for.
    gh = FakeGitHub(key, "9.9.9", payload)
    profile, env = profile_env("download")
    env["DECIBELL_TEST_UPDATE_ORIGIN"] = gh.server.origin
    lines, _ = run_app(binary, profile, env, done)
    s = states(lines)
    staged = json.loads((profile / "update/staged.json").read_text()) if (profile / "update/staged.json").is_file() else {}
    landed = profile / "update" / staged.get("file", "-")
    ok = (s[1:3] == ["Checking", 'Available { version: "9.9.9" }'] and any(x.startswith("Downloading") for x in s)
          and s[-1] == 'Downloaded { version: "9.9.9" }' and landed.is_file() and landed.read_bytes() == payload)
    check("updater: available → downloading → downloaded, staged and verified", ok, f"{len(s)} states, last {s[-1] if s else None}")
    asked = [p for (_, p, _, _) in gh.server.requests]
    check("updater: never asks for releases/latest or Electron's files", not any(p.endswith("/releases/latest") or "/ev" in p or p.endswith(".yml") for p in asked), ", ".join(sorted(set(asked))))
    gh.server.close()

    # 3. A tampered download (the bytes served aren't the ones the feed hashes).
    tampered = bytearray(payload)
    tampered[1000] ^= 0xFF
    gh = FakeGitHub(key, "9.9.9", payload, serve=bytes(tampered))
    profile, env = profile_env("tampered")
    env["DECIBELL_TEST_UPDATE_ORIGIN"] = gh.server.origin
    lines, _ = run_app(binary, profile, env, done)
    s = states(lines)
    check("updater: a tampered file is refused, nothing staged",
          s[-1] == 'Error { message: "the download doesn\'t match its checksum" }' and not (profile / "update/staged.json").exists(), s[-1] if s else "")
    gh.server.close()

    # 4. A feed signed by another key: refused before anything is offered or downloaded.
    (work / "other-key").mkdir(parents=True)
    other, _ = openssl_key(work / "other-key")
    gh = FakeGitHub(key, "9.9.9", payload, bad_sig_key=other)
    profile, env = profile_env("bad-signature")
    env["DECIBELL_TEST_UPDATE_ORIGIN"] = gh.server.origin
    lines, _ = run_app(binary, profile, env, done)
    s = states(lines)
    check("updater: a bad signature is refused (test key baked)",
          s[-1] == 'Error { message: "the update feed\'s signature doesn\'t match" }' and not any("Available" in x for x in s) and gh.downloads == 0, s[-1] if s else "")
    gh.server.close()

    # 5. A cut connection: the same check resumes where it stopped (Range), twice.
    gh = FakeGitHub(key, "9.9.9", payload, cuts=2, cut_at=1_000_000)
    profile, env = profile_env("resume")
    env["DECIBELL_TEST_UPDATE_ORIGIN"] = gh.server.origin
    lines, _ = run_app(binary, profile, env, done)
    s = states(lines)
    ranges = [h.get("Range") for (_, p, h, _) in gh.server.requests if p.endswith(gh.name)]
    landed = profile / "update" / gh.name
    check("updater: resumes after a cut connection", s[-1] == 'Downloaded { version: "9.9.9" }' and ranges == [None, "bytes=1000000-", "bytes=2000000-"]
          and landed.read_bytes() == payload, f"ranges {ranges}")
    gh.server.close()

    # 6. A package manager's install: notify only, nothing downloaded.
    gh = FakeGitHub(key, "9.9.9", payload)
    profile, env = profile_env("notify-only", kind="tarball")
    env["DECIBELL_TEST_UPDATE_ORIGIN"] = gh.server.origin
    lines, _ = run_app(binary, profile, env, lambda ls: any("Available" in l for l in ls))
    s = states(lines)
    check("updater: a tarball / package-manager install only says it is available", s[-1:] == ['Available { version: "9.9.9" }'] and gh.downloads == 0, " → ".join(s))
    gh.server.close()

    # 7. Restart (the update chip, through the control pipe) on a start that was opened with an
    #    invite link: the app quits, and this version starts again with --apply-update alone (an
    #    old link must not open again; electron-updater restarts with no arguments).
    gh = FakeGitHub(key, "9.9.9", payload)
    profile, env = profile_env("restart")
    env["DECIBELL_TEST_UPDATE_ORIGIN"] = gh.server.origin
    control = work / "restart" / "control"
    os.mkfifo(control)
    env["DECIBELL_DEV_CONTROL"] = str(control)
    sent = []

    def restart_when_ready(ls):
        if not sent and any(l == 'decibell: update Downloaded { version: "9.9.9" }' for l in ls):
            with open(control, "w") as fifo:
                fifo.write("restart-to-update\n")
            sent.append(True)
        return any(l.startswith("decibell: update relaunch") for l in ls)

    lines, code = run_app(binary, profile, env, restart_when_ready, args=["decibell://invite/ABCD234567"])
    relaunch = [l for l in lines if l.startswith("decibell: update relaunch")]
    check("updater: Restart quits and starts this version again with --apply-update alone (no old link)",
          sent and relaunch == ['decibell: update relaunch ["--apply-update"]'], relaunch[0] if relaunch else f"no relaunch (exit {code})")
    gh.server.close()


def apply_cases(binary, work):
    def setup(name, new_bytes):
        d = work / name
        (d / "apps").mkdir(parents=True)
        appimage = d / "apps/Decibell.AppImage"
        shutil.copy2(binary, appimage)
        update = d / "profile/update"
        update.mkdir(parents=True)
        (update / "Decibell-9.9.9-x86_64.AppImage").write_bytes(new_bytes)
        (update / "staged.json").write_text(json.dumps({"version": "9.9.9", "file": "Decibell-9.9.9-x86_64.AppImage",
                                                        "sha512": base64.b64encode(hashlib.sha512(new_bytes).digest()).decode()}))
        env = {"DECIBELL_TEST_INSTALL_KIND": "appimage", "APPIMAGE": str(appimage), "DECIBELL_TEST_UPDATE_ORIGIN": "http://127.0.0.1:9"}
        return d, appimage, env

    # The "new version": this binary with a marker appended (an ELF runs with trailing bytes).
    new = pathlib.Path(binary).read_bytes() + b"\0DECIBELL-TEST-NEW-VERSION"
    d, appimage, env = setup("apply-ok", new)
    lines, _ = run_app(binary, d / "profile", env, lambda ls: any("told the previous version" in l for l in ls) or any("the new version started" in l for l in ls), timeout=60)
    time.sleep(0.5)
    check("apply: the new version starts on trial, says so, and replaces the old file",
          appimage.read_bytes().endswith(b"DECIBELL-TEST-NEW-VERSION") and not (appimage.parent / ".Decibell.AppImage.previous").exists()
          and not (d / "profile/update/staged.json").exists(), "; ".join(l for l in lines if "[update]" in l)[-300:])

    # A "new version" that exits at once: rolled back, the old one carries on and says why.
    d, appimage, env = setup("apply-rollback", b"#!/bin/sh\nexit 3\n")
    old = appimage.read_bytes()
    control = d / "control"
    os.mkfifo(control)
    env["DECIBELL_DEV_CONTROL"] = str(control)
    failed_path = d / "profile/update/failed.json"
    seen_before = []

    def notice_then_seen(ls):
        # The first check (against a closed port) ends in an error: the notice must outlive it, and
        # stay unread until About shows it (here: the control pipe's stand-in for opening About).
        if not seen_before and any(l.startswith("decibell: update Error") for l in ls):
            seen_before.append(json.loads(failed_path.read_text()).get("shown") if failed_path.exists() else None)
            with open(control, "w") as fifo:
                fifo.write("update-notice-seen\n")
        return any(l == "decibell: update notice seen" for l in ls)

    lines, _ = run_app(binary, d / "profile", env, notice_then_seen, timeout=60)
    notices = [l for l in lines if l.startswith("decibell: update notice \"")]
    failed = json.loads(failed_path.read_text()) if failed_path.exists() else {}
    check("apply: a version that doesn't start is rolled back and remembered",
          appimage.read_bytes() == old and failed.get("version") == "9.9.9" and len(notices) == 1
          and notices[0].startswith('decibell: update notice "Version 9.9.9 didn\'t start (it exited before showing a window'),
          notices[0] if notices else "no notice")
    check("apply: the notice outlives the first check and is marked read only once About showed it",
          seen_before == [False] and failed.get("shown") is True, f"shown after the check: {seen_before}, after About: {failed.get('shown')}")


def crash_cases(binary, work):
    binary = pathlib.Path(binary)
    received = []

    def sentry(method, path, headers, body):
        received.append((path, headers, body))
        return 200, {}, b'{"id":"x"}', None

    server = Server(sentry)
    dsn = f"http://0123456789abcdef@127.0.0.1:{server.httpd.server_address[1]}/42"
    home = os.environ.get("HOME", "/home/x")
    token = "eyJhbGciOiJFZERTQSJ9.eyJ1aWQiOjF9.c2lnbmF0dXJlLXNpZ25hdHVyZQ"
    message = f'cannot open {home}/.config/decibell/secret.txt for perf1 with token {token} at 10.1.2.3:8080: "hello, private message"'

    def run(name, env, config=None, panic=message):
        profile = work / name
        profile.mkdir(parents=True)
        if config is not None:
            (profile / "config.json").write_text(json.dumps(config))
        e = {"DECIBELL_TEST_INSTALL_KIND": "tarball", **env}
        if panic is not None:
            e["DECIBELL_TEST_PANIC"] = panic
        before = len(received)
        lines, code = run_app(binary, profile, e, lambda ls: False, timeout=20, bus=False)
        time.sleep(0.3)
        return profile, lines, code, received[before:]

    # The account's and communities' names are known to the scrubber once signed in: registered here.
    profile, lines, code, got = run("panic", {"DECIBELL_TEST_CRASH_DSN": dsn, "DECIBELL_TEST_CRASH_NAMES": "perf1,Daily Gaming"})
    ok = len(got) == 1
    detail = f"{len(got)} envelope(s), exit {code}"
    if ok:
        path, headers, body = got[0]
        parts = body.split(b"\n")
        event = json.loads(parts[2])
        value = event["exception"]["values"][0]["value"]
        install_id = json.loads((profile / "config.json").read_text())["settings"]["crash_reporting_install_id"]
        leaks = [s for s in [home, "perf1", token, "10.1.2.3", "hello, private message", "secret.txt"] if s in body.decode(errors="replace")]
        ok = (path == "/api/42/envelope/" and "sentry_key=0123456789abcdef" in headers.get("X-Sentry-Auth", "")
              and event["release"] == f"decibell@{VERSION}" and event["level"] == "fatal" and event["environment"] == "production"
              and event["user"] == {"id": install_id} and event["tags"]["is_packaged"] == "true" and event["tags"]["thread"] == "decibell-test-panic"
              and "server_name" not in event and "breadcrumbs" not in event and not leaks
              and event["debug_meta"]["images"][0]["code_file"] == binary.name
              and len(event["debug_meta"]["images"][0].get("debug_id", "")) == 36
              and event["exception"]["values"][0]["stacktrace"]["frames"])
        detail = f'value "{value}", tags {event["tags"]}, leaks {leaks}'
    check("crash: a panic sends exactly one envelope, scrubbed, with Electron's tags", ok, detail)
    check("crash: a panic nothing catches still ends the process (SIGABRT)", code in (-6, 134), f"exit {code}")

    # The main thread: the same.
    _, _, code, got = run("main", {"DECIBELL_TEST_CRASH_DSN": dsn}, panic="main:on the main thread")
    events = [json.loads(b.split(b"\n")[2]) for (_, _, b) in got]
    check("crash: a panic on the main thread ends the process with one fatal report",
          code in (-6, 134) and len(events) == 1 and events[0]["level"] == "fatal" and events[0]["tags"]["thread"] == "main",
          f"exit {code}, {[(e['level'], e['tags']['thread']) for e in events]}")

    def recovered(name, panic, marker, thread, catcher):
        profile = work / name
        profile.mkdir(parents=True)
        e = {"DECIBELL_TEST_INSTALL_KIND": "tarball", "DECIBELL_TEST_CRASH_DSN": dsn, "DECIBELL_TEST_PANIC": panic, "DECIBELL_PERF": "1"}
        before = len(received)
        alive = []
        lines, code = run_app(binary, profile, e, lambda ls: any(marker in l for l in ls), timeout=40, alive=alive)
        time.sleep(0.5)
        events = [json.loads(b.split(b"\n")[2]) for (_, _, b) in received[before:]]
        ok = (alive == [True] and len(events) == 1 and events[0]["level"] == "error"
              and events[0]["tags"]["thread"] == thread and events[0]["tags"].get("recovered") == catcher
              and events[0]["exception"]["values"][0]["mechanism"]["handled"] is True)
        detail = f"alive {alive}, {[(e['level'], e['tags'].get('thread'), e['tags'].get('recovered')) for e in events]}"
        return ok, detail

    # Caught: the app carries on (its window opens), one report at level error, handled.
    ok, detail = recovered("guarded", "guarded:read failed in a guarded section", "first-frame", "decibell-test-panic", "guarded")
    check("crash: a panic in a section of ours that catches it is reported as recovered, the app carries on", ok, detail)
    ok, detail = recovered("runtime", "runtime:a task of the core panicked", "crash-test alive after the runtime panic", "decibell-rt", "runtime")
    check("crash: a panic in a task on the core's runtime (a core worker) is caught there, reported as recovered, the app carries on", ok, detail)

    # A caught panic resumed on the UI thread (what rt::run does with an awaited task's): the
    # process ends, with a fatal report naming where the panic began.
    profile = work / "resumed"
    profile.mkdir(parents=True)
    before = len(received)
    lines, code = run_app(binary, profile, {"DECIBELL_TEST_INSTALL_KIND": "tarball", "DECIBELL_TEST_CRASH_DSN": dsn,
                                            "DECIBELL_TEST_PANIC": "resumed:a task the UI thread waited for panicked"}, lambda ls: False, timeout=40)
    time.sleep(0.5)
    events = [json.loads(b.split(b"\n")[2]) for (_, _, b) in received[before:]]
    fatal = [e for e in events if e["level"] == "fatal"]
    check("crash: a caught panic resumed on the UI thread ends the process with a fatal report",
          code in (-6, 134) and len(fatal) == 1 and fatal[0]["tags"].get("resumed_from") == "decibell-rt" and fatal[0]["tags"]["thread"] == "main",
          f"exit {code}, {[(e['level'], e['tags'].get('thread'), e['tags'].get('resumed_from')) for e in events]}")

    _, _, _, got = run("toggle-off", {"DECIBELL_TEST_CRASH_DSN": dsn}, config={"settings": {"crash_reporting_enabled": False}})
    check("crash: Privacy toggle off → nothing sent, no connection", len(got) == 0 and not any(p for p, _, _ in got))
    connections_before = len(server.requests)
    profile, _, _, got = run("no-dsn", {})
    check("crash: no DSN baked → nothing sent, no connection", len(got) == 0 and len(server.requests) == connections_before and not (profile / "crash-reports").exists())

    profile, lines, code, got = run("native", {"DECIBELL_TEST_CRASH_DSN": dsn}, panic="segv")
    records = list((profile / "crash-reports").glob("*.native"))
    check("crash: a segfault leaves a record and dies as SIGSEGV", code in (-11, 139) and len(records) == 1 and len(got) == 0, f"exit {code}, {len(records)} record(s)")
    profile2 = profile
    before = len(received)
    lines, _ = run_app(binary, profile2, {"DECIBELL_TEST_INSTALL_KIND": "tarball", "DECIBELL_TEST_CRASH_DSN": dsn, "DECIBELL_TEST_PANIC": "wait-pending"}, lambda ls: False, timeout=20, bus=False)
    got = received[before:]
    ok = len(got) == 1
    if ok:
        event = json.loads(got[0][2].split(b"\n")[2])
        ok = event["exception"]["values"][0]["type"] == "SIGSEGV" and event["tags"].get("native_crash") == "true" and not list((profile2 / "crash-reports").glob("*"))
        detail = event["exception"]["values"][0]["value"]
    else:
        detail = f"{len(got)} envelope(s)"
    check("crash: the next start sends the native crash, once", ok, detail)
    server.close()


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", required=True, type=pathlib.Path)
    parser.add_argument("--only", default="updater,apply,crash")
    args = parser.parse_args()
    binary = args.binary.resolve()
    with tempfile.TemporaryDirectory(prefix="decibell-release-e2e-", dir=os.environ.get("DECIBELL_E2E_TMP")) as tmp:
        work = pathlib.Path(tmp)
        (work / "key").mkdir()
        key, public = openssl_key(work / "key")
        only = args.only.split(",")
        if "updater" in only:
            updater_cases(binary, work / "updater", key, public)
        if "apply" in only:
            apply_cases(binary, work / "apply")
        if "crash" in only:
            crash_cases(binary, work / "crash")
    failed = [r for r in RESULTS if not r[1]]
    print(f"\n{len(RESULTS) - len(failed)} passed, {len(failed)} failed")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
