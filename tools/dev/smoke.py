#!/usr/bin/env python3
"""Smoke test of the running local stack, done the way a client does it:

  1. TLS to central (DECIBELL_CENTRAL, default 127.0.0.1:18080), trust on first use, then a password
     sign-in as alice; the login reply must carry her memberships (Daily Gaming, Rustaceans), and the
     public directory must list Daily Gaming and Design Lab (which she is not in).
  2. TLS to Daily Gaming at the host:port central reported, pinned to the cert fingerprint central
     reported, COMMUNITY_AUTH_REQ with the session token.
  3. The newest page of #general must hold messages.
  4. One attachment thumbnail over the attachment HTTPS API (the port from COMMUNITY_AUTH_RES, the same
     pin, Bearer token) must be a JPEG no bigger than the size asked for.

Run: tools/dev/local-stack.sh smoke   (or the venv python with -I). Exit 0 when every check passes.
"""
import hashlib
import io
import os
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import devlib as d  # noqa: E402

pb = d.pb
FAILED = []


def check(name, ok, detail=""):
    print(("  ok    " if ok else "  FAIL  ") + name + (f"  [{detail}]" if detail else ""), flush=True)
    if not ok:
        FAILED.append(name)
    return ok


def main():
    central = os.environ.get("DECIBELL_CENTRAL", f"{d.CENTRAL_HOST}:{d.CENTRAL_PORT}")
    host, port = central.rsplit(":", 1)
    if not (host.startswith("127.") or host == "localhost"):
        check(f"central {central} is a loopback address (this test signs in with the stack's test accounts)", False)
        return
    d.CENTRAL_HOST, d.CENTRAL_PORT = host, int(port)
    known = d.central_pin()
    t0 = time.time()
    print(f"smoke: central {central}")

    # 1. Central: trust on first use (compared with what `up` recorded), password sign-in.
    try:
        a = d.Central("alice", app="local-stack-smoke")
    except OSError as e:
        check("connect to central", False, str(e))
        return
    check("central TLS (trust on first use)", True, f"sha256 {a.fingerprint[:16]}..."
          + ("" if not known else " = the stack's cert" if a.fingerprint == known else " != stack.json!"))
    if known and a.fingerprint != known:
        check("central cert is the stack's", False)
    if not check("password sign-in as alice", a.ok, a.login.message if a.login and not a.ok else
                 f"uid {a.login.uid}, token {len(a.token)} chars" if a.ok else "no LOGIN_RES"):
        return
    mem = {m.name: m for m in a.login.memberships}
    check("login reply lists alice's memberships", set(mem) == {"Daily Gaming", "Rustaceans"}, ", ".join(sorted(mem)))
    for m in mem.values():
        check(f"  {m.name}: host, port, cert fingerprint", bool(m.host_ip and m.port and len(m.cert_fingerprint) == 64),
              f"{m.host_ip}:{m.port} id {m.id}, {m.member_count} members")
    listing = {s.name: s for s in (a.server_list() or [])}
    check("public directory lists Daily Gaming + Design Lab", {"Daily Gaming", "Design Lab"} <= set(listing),
          ", ".join(sorted(listing)))
    check("Design Lab is listed, alice is not a member", "Design Lab" in listing and "Design Lab" not in mem)
    a.send(pb.Packet.DM_CONVERSATIONS_REQ, dm_conversations_req=pb.DmConversationsReq())
    r = a.wait(pb.Packet.DM_CONVERSATIONS_RES)
    convs = {c.peer: c for c in r.dm_conversations_res.conversations} if r else {}
    check("DM conversations (bob, carol)", {"bob", "carol"} <= set(convs),
          ", ".join(f"{p}: {c.unread_count} unread" for p, c in sorted(convs.items())))

    # 2. Daily Gaming, the way the client connects: central's host:port, pinned to central's fingerprint.
    dg = mem.get("Daily Gaming")
    if not dg:
        return
    try:
        s = d.Community(dg.host_ip, a.token, pin=dg.cert_fingerprint, port=dg.port)
    except d.PinMismatch as e:
        check("community cert matches the fingerprint central reported", False, str(e))
        return
    check("TLS to Daily Gaming, pinned to central's fingerprint", True, f"{dg.host_ip}:{dg.port}")
    if not check("COMMUNITY_AUTH_REQ with the session token", s.ok,
                 (s.auth.error_code or s.auth.message) if s.auth and not s.ok else
                 f"{len(s.channels)} channels, owner uid {s.auth.owner_uid}, attachments on :{s.auth.attachment_port}"
                 if s.ok else "no answer"):
        return
    general = s.channel("general", pb.ChannelInfo.TEXT)
    check("#general is in the channel list", general is not None)

    # 3. Newest page of #general.
    h = s.history("general", limit=50)
    msgs = list(h.messages) if h else []
    check("newest page of #general has messages", len(msgs) > 0,
          f"{len(msgs)} messages, has_more={h.has_more if h else None}, ids {msgs[0].id}..{msgs[-1].id}" if msgs else "")
    check("page is the newest (before_id echo 0) and older pages exist", bool(h and h.before_id == 0 and h.has_more))

    # 4. One attachment thumbnail over HTTPS.
    att = next((x for m in reversed(msgs) for x in m.attachments if x.thumbnail_size_bytes > 0), None)
    if att is None:   # page back until an image turns up
        before = msgs[0].id if msgs else 0
        for _ in range(20):
            h = s.history("general", limit=50, before_id=before)
            if not h or not h.messages:
                break
            att = next((x for m in reversed(h.messages) for x in m.attachments if x.thumbnail_size_bytes > 0), None)
            if att:
                break
            before = h.messages[0].id
    if not check("an image attachment with thumbnails in #general", att is not None,
                 f"#{att.id} {att.filename} {att.width}x{att.height}, mask {att.thumbnail_sizes_mask}" if att else ""):
        return
    http = s.attachment_http()
    try:
        st, hd, body = http.request("GET", f"/attachments/{att.id}?variant=thumb&size=320")
    except (OSError, d.PinMismatch) as e:
        check("GET thumbnail", False, str(e))
        return
    ok = st == 200 and hd.get("content-type") == "image/jpeg" and body[:3] == b"\xff\xd8\xff"
    dims = ""
    if ok:
        try:
            from PIL import Image
            w, hh = Image.open(io.BytesIO(body)).size
            dims = f"{w}x{hh}"
            ok = max(w, hh) <= 320
        except ImportError:
            pass
    check("GET /attachments/<id>?variant=thumb&size=320 (pinned HTTPS, Bearer token)", ok,
          f"HTTP {st}, {hd.get('content-type')}, {len(body)} bytes {dims}, sha256 {hashlib.sha256(body).hexdigest()[:12]}")
    st, hd, body = http.request("GET", f"/attachments/{att.id}", headers={"Range": "bytes=0-1023"})
    check("GET /attachments/<id> with a Range", st in (200, 206) and len(body) > 0 and body[:2] == b"\xff\xd8",
          f"HTTP {st}, {len(body)} bytes")
    http.close()
    s.close()
    a.close()
    print(f"smoke: {'PASS' if not FAILED else 'FAIL'} in {time.time() - t0:.2f} s")


if __name__ == "__main__":
    try:
        main()
    except Exception as e:  # noqa: BLE001
        check("unexpected error", False, repr(e))
    sys.exit(1 if FAILED else 0)
