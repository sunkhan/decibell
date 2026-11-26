#!/usr/bin/env python3
"""Wire helper for tests/stack.rs (DECIBELL_STACK=1): the parts of a client this crate doesn't have
(sign-in, community auth, sending a message), done with tools/dev/devlib.py against the local stack.

Run with the stack's venv python in isolated mode:
    ~/.cache/decibell-dev/deps/central/pgvenv/bin/python -I tests/stack_helper.py <repo>
It signs in as alice (a password sign-in from the tools' fixed device, like smoke.py), joins
Daily Gaming's session pinned to the fingerprint central reported, prints one JSON line with the
attachment target, then answers JSON-line commands on stdin:
    {"cmd": "send", "channel": "<id>", "content": "...", "attachments": [ids]}
        -> the echoed message: {"id", "attachments": [{id, filename, mime, size_bytes, width, height,
           thumbnail_sizes_mask, thumbnail_size_bytes, placeholder, kind}]}
    {"cmd": "delete", "channel": "<id>", "id": <message id>}   (our own test message)
        -> {"success": bool, "message": "..."}
    {"cmd": "quit"}
Loopback only: refuses a central that isn't 127.x.
"""
import json
import os
import sys

repo = sys.argv[1]
sys.path.insert(0, os.path.join(repo, "tools", "dev"))
import devlib as d  # noqa: E402

pb = d.pb


def out(obj):
    sys.stdout.write(json.dumps(obj) + "\n")
    sys.stdout.flush()


def main():
    if not d.CENTRAL_HOST.startswith("127."):
        out({"error": "central is not loopback"})
        return
    a = d.Central("alice", app="decibell-attachments-test")
    if not a.ok:
        out({"error": "alice could not sign in"})
        return
    mem = {m.name: m for m in a.login.memberships}
    dg = mem.get("Daily Gaming")
    if not dg or not dg.host_ip.startswith("127."):
        out({"error": "no loopback Daily Gaming membership"})
        return
    s = d.Community(dg.host_ip, a.token, pin=dg.cert_fingerprint, port=dg.port)
    if not s.ok:
        out({"error": "community auth failed"})
        return
    chans = {c.name: c for c in s.channels if c.type == pb.ChannelInfo.TEXT}
    out({
        "host": dg.host_ip,
        "port": s.auth.attachment_port,
        "jwt": a.token,
        "fingerprint": dg.cert_fingerprint,
        "max_bytes": s.auth.max_attachment_bytes,
        "general": chans["general"].id,
        "clips": chans["clips"].id if "clips" in chans else chans["general"].id,
        "vault": chans["vault"].id if "vault" in chans else "",
        "vault_encrypted": bool(chans["vault"].encrypted) if "vault" in chans else False,
    })
    for line in sys.stdin:
        req = json.loads(line)
        if req.get("cmd") == "quit":
            break
        if req.get("cmd") == "delete":
            s.send(pb.Packet.MESSAGE_DELETE_REQ, message_delete_req=pb.MessageDeleteReq(
                channel_id=req["channel"], message_id=req["id"]))
            r = s.wait(pb.Packet.MESSAGE_DELETE_RES, timeout=10,
                       pred=lambda p: p.message_delete_res.message_id == req["id"])
            out({"success": bool(r and r.message_delete_res.success),
                 "message": r.message_delete_res.message if r else "no answer"})
            continue
        if req.get("cmd") == "send":
            nonce = os.urandom(8).hex()
            s.send(pb.Packet.CHANNEL_MSG, channel_msg=pb.ChannelMessage(
                channel_id=req["channel"], content=req["content"], nonce=nonce,
                attachments=[pb.Attachment(id=i) for i in req["attachments"]]))
            r = s.wait(pb.Packet.CHANNEL_MSG, timeout=10, pred=lambda p: p.channel_msg.nonce == nonce)
            if not r:
                rej = [p for p in s.inbox if p.type == pb.Packet.CHANNEL_MSG_REJECTED]
                out({"error": rej[0].channel_msg_rejected.reason if rej else "no echo"})
                continue
            m = r.channel_msg
            out({"id": m.id, "attachments": [{
                "id": x.id, "filename": x.filename, "mime": x.mime, "size_bytes": x.size_bytes,
                "width": x.width, "height": x.height, "thumbnail_sizes_mask": x.thumbnail_sizes_mask,
                "thumbnail_size_bytes": x.thumbnail_size_bytes, "placeholder": x.placeholder, "kind": x.kind,
            } for x in m.attachments]})
    s.close()
    a.close()


if __name__ == "__main__":
    main()
