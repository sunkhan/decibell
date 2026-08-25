#!/usr/bin/env python3
"""e2e checks for the 2026-08-21 community-server fix batch.

Talks protobuf-over-TLS to a live community server (8082), the attachment
HTTPS listener (8085), and pokes the SQLite DB directly for seeding checks.
"""
import base64, hashlib, hmac, json, os, socket, sqlite3, ssl, struct, sys, time, subprocess, signal

sys.path.insert(0, os.environ.get("DECIBELL_E2E_PB", os.path.join(os.path.dirname(__file__), "deps", "pb")))
import messages_pb2 as pb  # noqa: E402

SECRET = "testsecret"
HOST = "127.0.0.1"
RUN = os.environ.get("DECIBELL_E2E_RUN", os.path.join(os.path.dirname(__file__), "run"))
DB = os.path.join(RUN, "c.db")
SERVER_BIN = os.environ.get("DECIBELL_E2E_SERVER", os.path.join(os.path.dirname(__file__), "community_server"))
PASS, FAIL = [], []


def check(name, cond, detail=""):
    (PASS if cond else FAIL).append(name)
    print(("  ok   " if cond else "  FAIL ") + name + (f"  ({detail})" if detail and not cond else ""))


def b64(b):
    return base64.urlsafe_b64encode(b).rstrip(b"=").decode()


KEY_PEM = os.path.join(RUN, "jwt_ed25519.pem")
PUB_PEM = KEY_PEM + ".pub"


def ensure_keys():
    """Ed25519 JWT keypair (Theme A): central signs, communities verify."""
    os.makedirs(RUN, exist_ok=True)
    if not os.path.exists(KEY_PEM):
        subprocess.run(["openssl", "genpkey", "-algorithm", "ed25519", "-out", KEY_PEM], check=True, capture_output=True)
        subprocess.run(["openssl", "pkey", "-in", KEY_PEM, "-pubout", "-out", PUB_PEM], check=True, capture_output=True)


_uid_counter = {}


def uid_for(username):
    """Deterministic per-username stable id, like central's users.uid."""
    return _uid_counter.setdefault(username, 1000 + len(_uid_counter))


def jwt(username, nonce="", uid=None):
    ensure_keys()
    h = b64(json.dumps({"alg": "EdDSA", "typ": "JWS"}).encode())
    now = int(time.time())
    p = b64(json.dumps({"iss": "decibell_central_auth", "sub": username, "uid": uid if uid is not None else uid_for(username),
                        "iat": now, "exp": now + 3600, "n": nonce}).encode())
    return f"{h}.{p}.{b64(ed25519_sign(KEY_PEM, f'{h}.{p}'.encode()))}"


def ed25519_sign(key_pem, data):
    """openssl pkeyutl -rawin needs a seekable file (one-shot EdDSA)."""
    import tempfile
    with tempfile.NamedTemporaryFile(delete=False, dir=RUN) as f:
        f.write(data); path = f.name
    try:
        return subprocess.run(["openssl", "pkeyutl", "-sign", "-inkey", key_pem, "-rawin", "-in", path],
                              check=True, capture_output=True).stdout
    finally:
        os.unlink(path)


class Client:
    def __init__(self, username, invite="", nonce="", timeout=3.0, uid=None):
        ctx = ssl.create_default_context()
        ctx.check_hostname = False
        ctx.verify_mode = ssl.CERT_NONE
        raw = socket.create_connection((HOST, 8082), timeout=timeout)
        self.s = ctx.wrap_socket(raw)
        self.username = username
        self.jwt = jwt(username, nonce, uid)
        self.buf = b""
        self.inbox = []
        self.closed = False
        self.send(pb.Packet.COMMUNITY_AUTH_REQ, community_auth_req=pb.CommunityAuthRequest(
            jwt_token=self.jwt, invite_code=invite))

    def send(self, ptype, **payload):
        p = pb.Packet(type=ptype, auth_token=self.jwt, **payload)
        data = p.SerializeToString()
        self.s.sendall(struct.pack(">I", len(data)) + data)

    def _recv_one(self, timeout):
        self.s.settimeout(timeout)
        try:
            while len(self.buf) < 4:
                chunk = self.s.recv(65536)
                if not chunk:
                    self.closed = True
                    return None
                self.buf += chunk
            n = struct.unpack(">I", self.buf[:4])[0]
            while len(self.buf) < 4 + n:
                chunk = self.s.recv(65536)
                if not chunk:
                    self.closed = True
                    return None
                self.buf += chunk
        except (socket.timeout, TimeoutError):
            return None
        except (ssl.SSLError, ConnectionError, OSError):
            self.closed = True
            return None
        body, self.buf = self.buf[4:4 + n], self.buf[4 + n:]
        p = pb.Packet()
        p.ParseFromString(body)   # raises on invalid UTF-8 like prost does
        return p

    def wait(self, ptype, timeout=3.0, pred=lambda p: True):
        """Return the first packet of `ptype` (searching the inbox first)."""
        for i, p in enumerate(self.inbox):
            if p.type == ptype and pred(p):
                return self.inbox.pop(i)
        end = time.time() + timeout
        while time.time() < end:
            p = self._recv_one(max(0.05, end - time.time()))
            if p is None:
                if self.closed:
                    return None
                continue
            if p.type == ptype and pred(p):
                return p
            self.inbox.append(p)
        return None

    def drain(self, timeout=0.5):
        end = time.time() + timeout
        while time.time() < end:
            p = self._recv_one(max(0.05, end - time.time()))
            if p is None:
                if self.closed:
                    return
                continue
            self.inbox.append(p)

    def flush(self, timeout=0.5):
        """Discard everything that arrives within  (and the inbox)."""
        self.drain(timeout)
        self.inbox.clear()

    def is_closed(self, timeout=2.0):
        self.drain(timeout)
        return self.closed

    def close(self):
        try:
            self.s.close()
        except Exception:
            pass


def start_server(fresh=False, extra_env=None):
    os.makedirs(RUN, exist_ok=True)
    if fresh:
        for f in ("c.db", "c.db-wal", "c.db-shm"):
            try: os.remove(os.path.join(RUN, f))
            except FileNotFoundError: pass
    if not os.path.exists(os.path.join(RUN, "server.crt")):
        subprocess.run(["openssl", "req", "-x509", "-newkey", "rsa:2048", "-nodes", "-keyout", "server.key",
                        "-out", "server.crt", "-subj", "/CN=localhost", "-days", "2"], cwd=RUN, check=True,
                       capture_output=True)
    ensure_keys()
    env = dict(os.environ, DECIBELL_JWT_PUBLIC_KEY_FILE=PUB_PEM, DECIBELL_COMMUNITY_SECRET=SECRET,
               DECIBELL_OWNER_USERNAME="alice", DECIBELL_DB_PATH="./c.db",
               DECIBELL_ATTACHMENTS_ROOT="./att", DECIBELL_CENTRAL_HOST="127.0.0.1")
    if extra_env:
        env.update(extra_env)
    log = open(os.path.join(RUN, "server.log"), "ab")
    proc = subprocess.Popen([SERVER_BIN], cwd=RUN, env=env, stdout=log, stderr=subprocess.STDOUT)
    for _ in range(50):
        try:
            socket.create_connection((HOST, 8082), timeout=0.2).close()
            time.sleep(0.2)
            return proc
        except OSError:
            time.sleep(0.1)
    raise RuntimeError("server did not start")


def stop_server(proc):
    proc.send_signal(signal.SIGTERM)
    try: proc.wait(3)
    except subprocess.TimeoutExpired:
        proc.kill(); proc.wait()
    time.sleep(0.3)


def sql(q, *args):
    c = sqlite3.connect(DB)
    try:
        r = c.execute(q, args).fetchall(); c.commit(); return r
    finally:
        c.close()


def auth_ok(c):
    r = c.wait(pb.Packet.COMMUNITY_AUTH_RES)
    return r is not None and r.community_auth_res.success, r


def make_invite(owner):
    owner.send(pb.Packet.INVITE_CREATE_REQ, invite_create_req=pb.InviteCreateRequest(expires_at=0, max_uses=0))
    r = owner.wait(pb.Packet.INVITE_CREATE_RES)
    assert r and r.invite_create_res.success, r
    return r.invite_create_res.invite.code


def join(username, owner, nonce=""):
    code = make_invite(owner)
    c = Client(username, invite=code, nonce=nonce)
    ok, _ = auth_ok(c)
    assert ok, f"{username} could not join"
    return c


# ---------------------------------------------------------------- tests

def test_b1_seed_resurrection():
    print("[B1] deleted seed channel must stay deleted across restart")
    proc = start_server(fresh=True)
    stop_server(proc)
    ids = [r[0] for r in sql("select id from channels order by position")]
    check("fresh DB seeded with 4 defaults", ids == ["general", "announcements", "voice-lounge", "voice-lounge-2"], str(ids))
    sql("delete from channels where id='announcements'")
    proc = start_server()
    stop_server(proc)
    ids = [r[0] for r in sql("select id from channels order by position")]
    check("announcements stays deleted after restart", "announcements" not in ids, str(ids))
    # Pre-stamp (upgraded) DB with an operator-pruned channel list must not be re-seeded either.
    sql("delete from server_meta where key='seed_channels_version'")
    proc = start_server()
    stop_server(proc)
    ids = [r[0] for r in sql("select id from channels order by position")]
    check("legacy DB without stamp is not re-seeded", "announcements" not in ids, str(ids))
    check("stamp written", sql("select value from server_meta where key='seed_channels_version'") == [("1",)])


def test_b2_sync_shutdown():
    print("[B2] silent HTTPS peer after 401 must not freeze the io thread")
    ctx = ssl.create_default_context(); ctx.check_hostname = False; ctx.verify_mode = ssl.CERT_NONE
    raw = socket.create_connection((HOST, 8085), timeout=3)
    s = ctx.wrap_socket(raw)
    s.sendall(b"GET / HTTP/1.1\r\nHost: x\r\n\r\n")
    s.settimeout(3)
    resp = b""
    try:
        while b"\r\n\r\n" not in resp:
            resp += s.recv(4096)
    except Exception:
        pass
    check("got 401 on unauthenticated GET", resp.startswith(b"HTTP/1.1 401"), resp[:40])
    # Stay silent; the server's TLS shutdown must not block the io thread.
    t0 = time.time()
    owner = Client("alice")
    ok, _ = auth_ok(owner)
    dt = time.time() - t0
    check("auth on 8082 still served while 8085 peer is silent", ok and dt < 1.5, f"ok={ok} dt={dt:.2f}s")
    owner.close()
    # Server should have dropped the silent peer within the 2 s deadline.
    time.sleep(2.5)
    try:
        s.settimeout(1.0)
        data = s.recv(10)
        closed = (data == b"")
    except (ssl.SSLError, ConnectionError, OSError):
        closed = True
    except socket.timeout:
        closed = False
    check("silent peer closed by server within deadline", closed)
    s.close()


def test_b3_utf8_truncation():
    print("[B3] mid-UTF-8 truncation must not poison broadcasts")
    owner = Client("alice"); assert auth_ok(owner)[0]
    bob = join("bob", owner)
    owner.flush(0.5); bob.flush(0.5)
    # 30 ASCII bytes + 4-byte emoji = 34 bytes > 32 cap; a byte cut at 32 would split the emoji.
    nick = "a" * 30 + "\U0001F600"
    bob.send(pb.Packet.SET_NICKNAME_REQ, set_nickname_req=pb.SetNicknameRequest(username="bob", nickname=nick))
    r = bob.wait(pb.Packet.MOD_ACTION_RES, pred=lambda p: p.mod_action_res.action == "nickname")
    check("nickname accepted", r is not None and r.mod_action_res.success)
    got = None
    try:
        up = owner.wait(pb.Packet.MEMBER_UPSERT, timeout=3, pred=lambda p: p.member_upsert.member.username == "bob")
        if up:
            got = up.member_upsert.member.nickname
    except Exception as e:  # UnicodeDecodeError / DecodeError == what prost would do
        check("MEMBER_UPSERT decodable", False, repr(e))
        got = None
    check("MEMBER_UPSERT decodable with clamped nickname", got is not None and got == "a" * 30, repr(got))
    # Channel rename: 62 ASCII + 'é' (2 bytes) = 64 ok; 63 + 'é' = 65 → must clamp to 63, not 64 (split).
    owner.send(pb.Packet.CHANNEL_RENAME_REQ, channel_rename_req=pb.ChannelRenameRequest(
        channel_id="general", name="g" * 63 + "é"))
    r = owner.wait(pb.Packet.CHANNEL_ACTION_RES, pred=lambda p: p.channel_action_res.action == "rename")
    check("rename accepted", r is not None and r.channel_action_res.success)
    check("rename clamped on codepoint boundary", r is not None and r.channel_action_res.channel.name == "g" * 63,
          repr(r.channel_action_res.channel.name if r else None))
    # Control chars stripped.
    owner.send(pb.Packet.CHANNEL_RENAME_REQ, channel_rename_req=pb.ChannelRenameRequest(
        channel_id="general", name="gen\r\neral\x01"))
    r = owner.wait(pb.Packet.CHANNEL_ACTION_RES, pred=lambda p: p.channel_action_res.action == "rename")
    check("control chars stripped", r is not None and r.channel_action_res.channel.name == "general")
    # Fresh connection must be able to auth (COMMUNITY_AUTH_RES embeds the channel list).
    c = Client("alice"); ok, _ = auth_ok(c)
    check("fresh auth still decodable", ok)
    c.close(); bob.close(); owner.close()


def test_b4_multi_session_ban():
    print("[B4] ban must close every session of the user")
    owner = Client("alice"); assert auth_ok(owner)[0]
    bob1 = join("bob2", owner, nonce="one")
    bob2 = Client("bob2", nonce="two"); assert auth_ok(bob2)[0]   # second device, different JWT
    owner.flush(0.5); bob1.flush(0.5); bob2.flush(0.5)
    owner.send(pb.Packet.BAN_MEMBER_REQ, ban_member_req=pb.BanMemberRequest(username="bob2", reason="bye"))
    r = owner.wait(pb.Packet.MOD_ACTION_RES, pred=lambda p: p.mod_action_res.action == "ban")
    check("ban succeeded", r is not None and r.mod_action_res.success)
    rev1 = bob1.wait(pb.Packet.MEMBERSHIP_REVOKED, timeout=2)
    rev2 = bob2.wait(pb.Packet.MEMBERSHIP_REVOKED, timeout=2)
    check("session 1 got MEMBERSHIP_REVOKED", rev1 is not None and rev1.membership_revoked.action == "ban")
    check("session 2 got MEMBERSHIP_REVOKED", rev2 is not None and rev2.membership_revoked.action == "ban")
    check("session 1 closed", bob1.is_closed())
    check("session 2 closed", bob2.is_closed())
    # A third, still-open session whose member row was removed behind its back must be dropped on next packet.
    bob1.close(); bob2.close()
    carol = join("carol", owner)
    owner.flush(0.5); carol.flush(0.5)
    sql("delete from members where username='carol'")   # simulate out-of-band removal
    carol.send(pb.Packet.CHANNEL_MSG, channel_msg=pb.ChannelMessage(channel_id="general", content="ghost"))
    got = owner.wait(pb.Packet.CHANNEL_MSG, timeout=1.5)
    check("stale non-member's CHANNEL_MSG not broadcast", got is None)
    check("stale non-member session closed", carol.is_closed())
    carol.close(); owner.close()


def test_b21_jwt_leak():
    print("[JWT leak] forwarded CHANNEL_MSG must not carry the sender's auth_token")
    owner = Client("alice"); assert auth_ok(owner)[0]
    dave = join("dave", owner)
    owner.flush(0.5); dave.flush(0.5)
    dave.send(pb.Packet.CHANNEL_MSG, channel_msg=pb.ChannelMessage(channel_id="general", content="hi"))
    got = owner.wait(pb.Packet.CHANNEL_MSG, timeout=2)
    check("message received", got is not None and got.channel_msg.content == "hi")
    check("auth_token stripped", got is not None and got.auth_token == "", (got.auth_token[:20] + "...") if got else "")
    dave.close(); owner.close()


def test_role_assign_guard():
    print("[role guard] assigning a lower role with bits you lack must be rejected")
    owner = Client("alice"); assert auth_ok(owner)[0]
    erin = join("erin", owner)
    owner.flush(0.5); erin.flush(0.5)
    # Create Admin (ADMINISTRATOR) first → it will end up at position 1 after Mod is created (Mod at 1, shifts Admin to 2)...
    # We need Mod ABOVE Admin: create Admin first (pos 1), then Mod (pos 1, Admin → pos 2), then move Mod to 2.
    owner.send(pb.Packet.ROLE_CREATE_REQ, role_create_req=pb.RoleCreateRequest(name="Admin", color=0, permissions=pb.PERM_ADMINISTRATOR))
    admin = owner.wait(pb.Packet.ROLE_ACTION_RES, pred=lambda p: p.role_action_res.action == "create").role_action_res.role
    owner.send(pb.Packet.ROLE_CREATE_REQ, role_create_req=pb.RoleCreateRequest(name="Mod", color=0, permissions=pb.PERM_MANAGE_ROLES))
    mod = owner.wait(pb.Packet.ROLE_ACTION_RES, pred=lambda p: p.role_action_res.action == "create").role_action_res.role
    owner.send(pb.Packet.ROLE_UPDATE_REQ, role_update_req=pb.RoleUpdateRequest(role_id=mod.id, name="Mod", color=0,
                                                                              permissions=pb.PERM_MANAGE_ROLES, position=2))
    owner.wait(pb.Packet.ROLE_ACTION_RES, pred=lambda p: p.role_action_res.action == "update")
    owner.send(pb.Packet.MEMBER_ROLES_UPDATE_REQ, member_roles_update_req=pb.MemberRolesUpdateRequest(username="erin", role_ids=[mod.id]))
    r = owner.wait(pb.Packet.ROLE_ACTION_RES, pred=lambda p: p.role_action_res.action == "assign")
    check("owner assigned Mod to erin", r is not None and r.role_action_res.success)
    owner.flush(0.5)
    owner.send(pb.Packet.ROLE_LIST_REQ, role_list_req=pb.RoleListRequest())
    rl = owner.wait(pb.Packet.ROLE_LIST_RES)
    pos = {x.name: x.position for x in rl.role_list_res.roles}
    check("hierarchy: Mod above Admin", pos.get("Mod", 0) > pos.get("Admin", 0), str(pos))
    erin.flush(0.5)
    erin.send(pb.Packet.MEMBER_ROLES_UPDATE_REQ, member_roles_update_req=pb.MemberRolesUpdateRequest(username="erin", role_ids=[mod.id, admin.id]))
    r = erin.wait(pb.Packet.ROLE_ACTION_RES, pred=lambda p: p.role_action_res.action == "assign")
    check("erin self-assigning Admin rejected", r is not None and not r.role_action_res.success, r.role_action_res.message if r else None)
    # Sanity: erin can still assign a harmless lower role she holds bits for.
    owner.send(pb.Packet.ROLE_CREATE_REQ, role_create_req=pb.RoleCreateRequest(name="Helper", color=0, permissions=0))
    helper = owner.wait(pb.Packet.ROLE_ACTION_RES, pred=lambda p: p.role_action_res.action == "create").role_action_res.role
    erin.flush(0.5)
    erin.send(pb.Packet.MEMBER_ROLES_UPDATE_REQ, member_roles_update_req=pb.MemberRolesUpdateRequest(username="erin", role_ids=[mod.id, helper.id]))
    r = erin.wait(pb.Packet.ROLE_ACTION_RES, pred=lambda p: p.role_action_res.action == "assign")
    check("erin assigning Helper (no extra bits) allowed", r is not None and r.role_action_res.success, r.role_action_res.message if r else None)
    # Removing a role above... erin removes Helper again (fine).
    erin.send(pb.Packet.MEMBER_ROLES_UPDATE_REQ, member_roles_update_req=pb.MemberRolesUpdateRequest(username="erin", role_ids=[mod.id]))
    r = erin.wait(pb.Packet.ROLE_ACTION_RES, pred=lambda p: p.role_action_res.action == "assign")
    check("erin removing Helper allowed", r is not None and r.role_action_res.success)
    erin.close(); owner.close()


def test_ghost_stream():
    print("[ghost stream] switching voice channel must end the stream in the old channel")
    owner = Client("alice"); assert auth_ok(owner)[0]
    frank = join("frank", owner)
    owner.flush(0.5); frank.flush(0.5)
    frank.send(pb.Packet.JOIN_VOICE_REQ, join_voice_req=pb.JoinVoiceRequest(channel_id="voice-lounge"))
    frank.wait(pb.Packet.VOICE_PRESENCE_UPDATE, pred=lambda p: p.voice_presence_update.channel_id == "voice-lounge")
    frank.send(pb.Packet.START_STREAM_REQ, start_stream_req=pb.StartStreamRequest(channel_id="voice-lounge", target_fps=30))
    sp = owner.wait(pb.Packet.STREAM_PRESENCE_UPDATE, pred=lambda p: p.stream_presence_update.channel_id == "voice-lounge" and len(p.stream_presence_update.active_streams) > 0)
    check("stream visible in voice-lounge", sp is not None)
    owner.flush(0.3)
    frank.send(pb.Packet.JOIN_VOICE_REQ, join_voice_req=pb.JoinVoiceRequest(channel_id="voice-lounge-2"))
    sp = owner.wait(pb.Packet.STREAM_PRESENCE_UPDATE, timeout=2, pred=lambda p: p.stream_presence_update.channel_id == "voice-lounge" and len(p.stream_presence_update.active_streams) == 0)
    check("old channel broadcasts empty stream list after switch", sp is not None)
    # A late joiner must not see a ghost stream in voice-lounge.
    grace = join("grace", owner)
    ghosts = grace.wait(pb.Packet.STREAM_PRESENCE_UPDATE, timeout=1.0, pred=lambda p: p.stream_presence_update.channel_id == "voice-lounge")
    check("late joiner sees no ghost stream in old channel", ghosts is None)
    frank.close(); grace.close(); owner.close()


def test_offline_kick_roster():
    print("[offline kick] kicking/banning an offline member must refresh rosters")
    owner = Client("alice"); assert auth_ok(owner)[0]
    heidi = join("heidi", owner)
    heidi.close()
    time.sleep(0.5)
    owner.flush(0.8)   # swallow the leave-triggered roster broadcast
    owner.send(pb.Packet.KICK_MEMBER_REQ, kick_member_req=pb.KickMemberRequest(username="heidi", reason=""))
    r = owner.wait(pb.Packet.MOD_ACTION_RES, pred=lambda p: p.mod_action_res.action == "kick")
    check("offline kick succeeded", r is not None and r.mod_action_res.success)
    rm = owner.wait(pb.Packet.MEMBER_REMOVE, timeout=2, pred=lambda p: p.member_remove.username == "heidi")
    check("MEMBER_REMOVE after offline kick", rm is not None)
    ivan = join("ivan", owner); ivan.close(); time.sleep(0.5); owner.flush(0.8)
    owner.send(pb.Packet.BAN_MEMBER_REQ, ban_member_req=pb.BanMemberRequest(username="ivan", reason=""))
    r = owner.wait(pb.Packet.MOD_ACTION_RES, pred=lambda p: p.mod_action_res.action == "ban")
    check("offline ban succeeded", r is not None and r.mod_action_res.success)
    rm = owner.wait(pb.Packet.MEMBER_REMOVE, timeout=2, pred=lambda p: p.member_remove.username == "ivan")
    bl = owner.wait(pb.Packet.BAN_LIST_RES, timeout=2, pred=lambda p: any(e.username == "ivan" for e in p.ban_list_res.entries))
    check("MEMBER_REMOVE + BAN_LIST_RES after offline ban", rm is not None and bl is not None)
    owner.close()


def test_no_ghost_after_leave_or_ban():
    print("[ghost session] leave / ban of a user in voice must clear their session state")
    owner = Client("alice"); assert auth_ok(owner)[0]
    judy = join("judy", owner)
    judy.send(pb.Packet.JOIN_VOICE_REQ, join_voice_req=pb.JoinVoiceRequest(channel_id="voice-lounge"))
    vp = owner.wait(pb.Packet.VOICE_PRESENCE_UPDATE, pred=lambda p: p.voice_presence_update.channel_id == "voice-lounge" and "judy" in p.voice_presence_update.active_users)
    check("judy visible in voice", vp is not None)
    owner.flush(0.5); judy.flush(0.5)
    judy.send(pb.Packet.LEAVE_SERVER_REQ, leave_server_req=pb.LeaveServerRequest())
    r = judy.wait(pb.Packet.MOD_ACTION_RES, pred=lambda p: p.mod_action_res.action == "leave")
    check("leave acknowledged", r is not None and r.mod_action_res.success)
    vp = owner.wait(pb.Packet.VOICE_PRESENCE_UPDATE, timeout=3, pred=lambda p: p.voice_presence_update.channel_id == "voice-lounge" and len(p.voice_presence_update.active_users) == 0)
    check("voice presence cleared after leave (session reaped)", vp is not None)
    rm = owner.wait(pb.Packet.MEMBER_REMOVE, timeout=2, pred=lambda p: p.member_remove.username == "judy")
    check("MEMBER_REMOVE after leave", rm is not None)
    check("judy socket closed", judy.is_closed())
    judy.close()
    # Same via ban while in voice (online target, close_after_flush path).
    kim = join("kim", owner)
    kim.send(pb.Packet.JOIN_VOICE_REQ, join_voice_req=pb.JoinVoiceRequest(channel_id="voice-lounge"))
    owner.wait(pb.Packet.VOICE_PRESENCE_UPDATE, pred=lambda p: "kim" in p.voice_presence_update.active_users)
    owner.flush(0.5)
    owner.send(pb.Packet.BAN_MEMBER_REQ, ban_member_req=pb.BanMemberRequest(username="kim", reason=""))
    vp = owner.wait(pb.Packet.VOICE_PRESENCE_UPDATE, timeout=3, pred=lambda p: p.voice_presence_update.channel_id == "voice-lounge" and len(p.voice_presence_update.active_users) == 0)
    check("voice presence cleared after ban (session reaped)", vp is not None)
    check("kim socket closed", kim.is_closed())
    kim.close(); owner.close()


def raw_tls(port=8082):
    ctx = ssl.create_default_context(); ctx.check_hostname = False; ctx.verify_mode = ssl.CERT_NONE
    return ctx.wrap_socket(socket.create_connection((HOST, port), timeout=3))


def socket_closed(sock, wait):
    """True if the peer closes `sock` within `wait` seconds."""
    sock.settimeout(wait)
    try:
        return sock.recv(16) == b""
    except (ssl.SSLError, ConnectionError, OSError):
        return True
    except socket.timeout:
        return False


def test_b9_timeouts():
    print("[B9] auth / idle deadlines + pre-auth frame cap (short-timeout server)")
    proc = start_server(extra_env={"DECIBELL_AUTH_TIMEOUT_SECONDS": "2", "DECIBELL_IDLE_TIMEOUT_SECONDS": "4"})
    try:
        t0 = time.time()
        s1 = raw_tls()
        check("unauthenticated TLS session closed by auth deadline", socket_closed(s1, 4.0), f"{time.time()-t0:.1f}s")
        s1.close()
        # Pre-auth oversized frame → immediate drop.
        s2 = raw_tls()
        s2.sendall(struct.pack(">I", 100 * 1024))
        check("pre-auth 100 KB frame rejected", socket_closed(s2, 2.0))
        s2.close()
        # Authenticated but silent → idle deadline.
        owner = Client("alice"); assert auth_ok(owner)[0]
        t0 = time.time()
        check("idle authenticated session closed after idle deadline", owner.is_closed(6.0), f"{time.time()-t0:.1f}s")
        owner.close()
        # Authenticated + pinging stays alive past the idle deadline.
        owner = Client("alice"); assert auth_ok(owner)[0]
        alive = True
        for _ in range(6):
            time.sleep(1.0)
            owner.send(pb.Packet.CLIENT_PING)
            owner.drain(0.05)
            if owner.closed:
                alive = False; break
        owner.send(pb.Packet.MEMBER_LIST_REQ, member_list_req=pb.MemberListRequest())
        ml = owner.wait(pb.Packet.MEMBER_LIST_RES, timeout=2)
        check("pinging session survives 6 s past a 4 s idle deadline", alive and ml is not None)
        owner.close()
    finally:
        stop_server(proc)


def test_b11_rate_limit():
    print("[B11] message flood is throttled and the sender is told")
    owner = Client("alice"); assert auth_ok(owner)[0]
    lou = join("lou", owner)
    owner.flush(0.5); lou.flush(0.5)
    for i in range(30):
        lou.send(pb.Packet.CHANNEL_MSG, channel_msg=pb.ChannelMessage(channel_id="general", content=f"spam {i}"))
    owner.drain(2.0); lou.drain(1.0)
    delivered = sum(1 for p in owner.inbox if p.type == pb.Packet.CHANNEL_MSG)
    rejected = sum(1 for p in lou.inbox if p.type == pb.Packet.MOD_ACTION_RES and p.mod_action_res.action == "message"
                   and not p.mod_action_res.success)
    check("burst capped (8 burst + ~1.5/s)", 8 <= delivered <= 14, f"delivered={delivered}")
    check("sender notified for each dropped message", rejected == 30 - delivered, f"rejected={rejected} delivered={delivered}")
    lou.close(); owner.close()


def test_b10_caps_cap():
    print("[B10] oversized ClientCapabilities are rejected")
    owner = Client("alice"); assert auth_ok(owner)[0]
    mia = join("mia", owner)
    owner.flush(0.5); mia.flush(0.5)
    caps = pb.ClientCapabilities()
    for i in range(100):
        caps.decode.add(codec=pb.CODEC_H264_HW, max_width=1920, max_height=1080, max_fps=60)
    mia.send(pb.Packet.JOIN_VOICE_REQ, join_voice_req=pb.JoinVoiceRequest(channel_id="voice-lounge", capabilities=caps))
    vp = owner.wait(pb.Packet.VOICE_PRESENCE_UPDATE, timeout=1.5, pred=lambda p: "mia" in p.voice_presence_update.active_users)
    check("join with 100 codec entries dropped", vp is None)
    small = pb.ClientCapabilities(); small.decode.add(codec=pb.CODEC_H264_HW)
    mia.send(pb.Packet.JOIN_VOICE_REQ, join_voice_req=pb.JoinVoiceRequest(channel_id="voice-lounge", capabilities=small))
    vp = owner.wait(pb.Packet.VOICE_PRESENCE_UPDATE, timeout=2, pred=lambda p: "mia" in p.voice_presence_update.active_users)
    check("normal caps accepted", vp is not None)
    mia.close(); owner.close()


def test_b25_invite_params():
    print("[B25] invite parameter validation")
    owner = Client("alice"); assert auth_ok(owner)[0]
    owner.send(pb.Packet.INVITE_CREATE_REQ, invite_create_req=pb.InviteCreateRequest(expires_at=int(time.time()) - 60, max_uses=0))
    r = owner.wait(pb.Packet.INVITE_CREATE_RES)
    check("past expiry rejected", r is not None and not r.invite_create_res.success)
    owner.send(pb.Packet.INVITE_CREATE_REQ, invite_create_req=pb.InviteCreateRequest(expires_at=0, max_uses=-5))
    r = owner.wait(pb.Packet.INVITE_CREATE_RES)
    check("negative max_uses normalised to 0", r is not None and r.invite_create_res.success and r.invite_create_res.invite.max_uses == 0)
    owner.close()


def test_b26_stop_watching_spoof():
    print("[B26] STOP_WATCHING from a non-watcher is inert")
    owner = Client("alice"); assert auth_ok(owner)[0]
    ned = join("ned", owner); oli = join("oli", owner)
    for c in (ned, oli):
        c.send(pb.Packet.JOIN_VOICE_REQ, join_voice_req=pb.JoinVoiceRequest(channel_id="voice-lounge"))
    ned.send(pb.Packet.START_STREAM_REQ, start_stream_req=pb.StartStreamRequest(channel_id="voice-lounge", target_fps=30))
    time.sleep(0.5); ned.flush(0.5); oli.flush(0.5); owner.flush(0.5)
    oli.send(pb.Packet.STOP_WATCHING_REQ, stop_watching_req=pb.StopWatchingRequest(channel_id="voice-lounge", target_username="ned"))
    n = ned.wait(pb.Packet.STREAM_WATCHER_NOTIFY, timeout=1.5)
    check("no LEFT notify for a never-subscribed watcher", n is None)
    oli.send(pb.Packet.WATCH_STREAM_REQ, watch_stream_req=pb.WatchStreamRequest(channel_id="voice-lounge", target_username="ned"))
    n = ned.wait(pb.Packet.STREAM_WATCHER_NOTIFY, timeout=2, pred=lambda p: p.stream_watcher_notify.action == pb.StreamWatcherNotify.JOINED)
    check("real watch → JOINED notify", n is not None)
    oli.send(pb.Packet.STOP_WATCHING_REQ, stop_watching_req=pb.StopWatchingRequest(channel_id="voice-lounge", target_username="ned"))
    n = ned.wait(pb.Packet.STREAM_WATCHER_NOTIFY, timeout=2, pred=lambda p: p.stream_watcher_notify.action == pb.StreamWatcherNotify.LEFT)
    check("real unwatch → LEFT notify", n is not None)
    ned.close(); oli.close(); owner.close()


def test_p2_perm_cache_invalidation():
    print("[P2] permission cache invalidates on role changes")
    owner = Client("alice"); assert auth_ok(owner)[0]
    pat = join("pat", owner)
    owner.flush(0.5); pat.flush(0.5)
    pat.send(pb.Packet.CHANNEL_CREATE_REQ, channel_create_req=pb.ChannelCreateRequest(name="pat-1", type=pb.ChannelInfo.TEXT))
    r = pat.wait(pb.Packet.CHANNEL_ACTION_RES, pred=lambda p: p.channel_action_res.action == "create")
    check("no MANAGE_CHANNELS → create rejected", r is not None and not r.channel_action_res.success)
    owner.send(pb.Packet.ROLE_CREATE_REQ, role_create_req=pb.RoleCreateRequest(name="Builder", color=0, permissions=pb.PERM_MANAGE_CHANNELS))
    role = owner.wait(pb.Packet.ROLE_ACTION_RES, pred=lambda p: p.role_action_res.action == "create").role_action_res.role
    owner.send(pb.Packet.MEMBER_ROLES_UPDATE_REQ, member_roles_update_req=pb.MemberRolesUpdateRequest(username="pat", role_ids=[role.id]))
    owner.wait(pb.Packet.ROLE_ACTION_RES, pred=lambda p: p.role_action_res.action == "assign")
    pat.flush(0.5)
    pat.send(pb.Packet.CHANNEL_CREATE_REQ, channel_create_req=pb.ChannelCreateRequest(name="pat-2", type=pb.ChannelInfo.TEXT))
    r = pat.wait(pb.Packet.CHANNEL_ACTION_RES, pred=lambda p: p.channel_action_res.action == "create")
    check("role assigned → create allowed (cache refreshed)", r is not None and r.channel_action_res.success, r.channel_action_res.message if r else None)
    owner.send(pb.Packet.ROLE_UPDATE_REQ, role_update_req=pb.RoleUpdateRequest(role_id=role.id, name="Builder", color=0, permissions=0, position=role.position))
    owner.wait(pb.Packet.ROLE_ACTION_RES, pred=lambda p: p.role_action_res.action == "update")
    pat.flush(0.5)
    pat.send(pb.Packet.CHANNEL_CREATE_REQ, channel_create_req=pb.ChannelCreateRequest(name="pat-3", type=pb.ChannelInfo.TEXT))
    r = pat.wait(pb.Packet.CHANNEL_ACTION_RES, pred=lambda p: p.channel_action_res.action == "create")
    check("role bits removed → create rejected (cache invalidated)", r is not None and not r.channel_action_res.success)
    owner.send(pb.Packet.ROLE_UPDATE_REQ, role_update_req=pb.RoleUpdateRequest(role_id=role.id, name="Builder", color=0, permissions=pb.PERM_MANAGE_CHANNELS, position=role.position))
    owner.wait(pb.Packet.ROLE_ACTION_RES, pred=lambda p: p.role_action_res.action == "update")
    owner.send(pb.Packet.ROLE_DELETE_REQ, role_delete_req=pb.RoleDeleteRequest(role_id=role.id))
    owner.wait(pb.Packet.ROLE_ACTION_RES, pred=lambda p: p.role_action_res.action == "delete")
    pat.flush(0.5)
    pat.send(pb.Packet.CHANNEL_CREATE_REQ, channel_create_req=pb.ChannelCreateRequest(name="pat-4", type=pb.ChannelInfo.TEXT))
    r = pat.wait(pb.Packet.CHANNEL_ACTION_RES, pred=lambda p: p.channel_action_res.action == "create")
    check("role deleted → create rejected (cache invalidated)", r is not None and not r.channel_action_res.success)
    pat.close(); owner.close()


def test_b20_attachment_url():
    print("[B20] Attachment.url no longer leaks the server filesystem path")
    now = int(time.time())
    mid = sql("insert into messages(channel_id, sender, content, timestamp) values('general','alice','with file',?) returning id", now)[0][0]
    aid = sql("insert into attachments(message_id, kind, filename, mime, size_bytes, storage_path, position, created_at, upload_status, uploader, channel_id) "
              "values(?,0,'x.png','image/png',10,'/srv/secret/att/general/1_x.png',0,?,'ready','alice','general') returning id", mid, now)[0][0]
    owner = Client("alice"); assert auth_ok(owner)[0]
    owner.send(pb.Packet.CHANNEL_HISTORY_REQ, channel_history_req=pb.ChannelHistoryRequest(channel_id="general", before_id=0, limit=50))
    h = owner.wait(pb.Packet.CHANNEL_HISTORY_RES)
    url = None
    for m in h.channel_history_res.messages if h else []:
        for a in m.attachments:
            if a.id == aid: url = a.url
    check("history attachment url is /attachments/<id>", url == f"/attachments/{aid}", repr(url))
    owner.close()


def test_b12_b27_retention_sweep():
    print("[B12/B27] retention sweep: >999 attachments tombstoned once; CHANNEL_PRUNED batched")
    old = int(time.time()) - 10 * 86400
    att_dir = os.path.join(RUN, "att", "general"); os.makedirs(att_dir, exist_ok=True)
    c = sqlite3.connect(DB)
    paths = []
    for i in range(1200):
        mid = c.execute("insert into messages(channel_id, sender, content, timestamp) values('general','alice',?,?)", (f"img {i}", old)).lastrowid
        path = os.path.join(att_dir, f"r{i}.png"); open(path, "wb").write(b"x"); paths.append(path)
        c.execute("insert into attachments(message_id, kind, filename, mime, size_bytes, storage_path, position, created_at, upload_status, uploader, channel_id) "
                  "values(?,0,'r.png','image/png',1,?,0,?,'ready','alice','general')", (mid, path, old))
    # (announcements was deleted by the B1 test — and stays deleted — so use a channel of our own.)
    c.execute("insert or ignore into channels(id, name, type, position) values('archive','archive',0,99)")
    for i in range(2500):
        c.execute("insert into messages(channel_id, sender, content, timestamp) values('archive','alice',?,?)", (f"old {i}", old))
    c.execute("update channels set retention_days_image=1 where id='general'")
    c.execute("update channels set retention_days_text=1 where id='archive'")
    c.commit(); c.close()
    proc = start_server(extra_env={"DECIBELL_RETENTION_INTERVAL_SECONDS": "2"})
    try:
        owner = Client("alice"); assert auth_ok(owner)[0]
        owner.drain(6.0)   # ≥2 sweeps
        pruned = [p.channel_pruned for p in owner.inbox if p.type == pb.Packet.CHANNEL_PRUNED]
        gen = [cp for cp in pruned if cp.channel_id == "general"]
        ann = [cp for cp in pruned if cp.channel_id == "archive"]
        purged = sum(len(cp.purged_attachments) for cp in gen)
        check("1200 attachments tombstoned in exactly one sweep", purged == 1200 and len(gen) == 1, f"purged={purged} packets={len(gen)}")
        deleted = sum(len(cp.deleted_message_ids) for cp in ann)
        check("2500 messages pruned, batched <=2000 per packet", deleted == 2500 and len(ann) == 2 and all(len(cp.deleted_message_ids) <= 2000 for cp in ann),
              f"deleted={deleted} packets={len(ann)}")
        check("blobs unlinked", not any(os.path.exists(p) for p in paths))
        rows = sql("select count(*) from attachments where channel_id='general' and purged_at=0 and created_at<?", old + 1)
        check("no un-tombstoned old rows left (no re-purge loop)", rows == [(0,)], str(rows))
        owner.close()
    finally:
        stop_server(proc)
    sql("update channels set retention_days_image=0, retention_days_text=0")


def test_auth_server_id_field():
    print("[B17 server] CommunityAuthResponse carries server_id (0 without central)")
    c = Client("alice"); ok, r = auth_ok(c)
    check("server_id field present and 0", ok and r.community_auth_res.server_id == 0)
    c.close()


def everyone_role(c):
    c.send(pb.Packet.ROLE_LIST_REQ, role_list_req=pb.RoleListRequest())
    rl = c.wait(pb.Packet.ROLE_LIST_RES)
    return next(r for r in rl.role_list_res.roles if r.is_default)


def set_everyone_perms(owner, perms):
    ev = everyone_role(owner)
    owner.send(pb.Packet.ROLE_UPDATE_REQ, role_update_req=pb.RoleUpdateRequest(role_id=ev.id, name=ev.name, color=0, permissions=perms, position=0))
    r = owner.wait(pb.Packet.ROLE_ACTION_RES, pred=lambda p: p.role_action_res.action == "update")
    assert r and r.role_action_res.success, r
    return ev


def set_overwrite(c, channel_id, target_type, target_id, allow=0, deny=0):
    c.send(pb.Packet.CHANNEL_OVERWRITE_SET_REQ, channel_overwrite_set_req=pb.ChannelOverwriteSetRequest(
        overwrite=pb.ChannelOverwrite(channel_id=channel_id, target_type=target_type, target_id=str(target_id), allow=allow, deny=deny)))
    return c.wait(pb.Packet.CHANNEL_ACTION_RES, pred=lambda p: p.channel_action_res.action == "overwrite")


def channel_list(c, timeout=3):
    u = c.wait(pb.Packet.CHANNEL_LIST_UPDATE, timeout=timeout)
    return {ch.id: ch for ch in u.channel_list_update.channels} if u else None


ALL_MEMBER_BITS = (pb.PERM_SEND_MESSAGES | pb.PERM_CONNECT_VOICE | pb.PERM_STREAM |
                   pb.PERM_VIEW_CHANNEL | pb.PERM_READ_HISTORY | pb.PERM_ATTACH_FILES)


def test_v2_enforced_bits():
    print("[v2] SEND_MESSAGES / CONNECT_VOICE / STREAM are enforced")
    owner = Client("alice"); assert auth_ok(owner)[0]
    quinn = join("quinn", owner)
    owner.flush(0.5); quinn.flush(0.5)
    # everyone loses SEND + CONNECT + STREAM
    set_everyone_perms(owner, ALL_MEMBER_BITS & ~(pb.PERM_SEND_MESSAGES | pb.PERM_CONNECT_VOICE | pb.PERM_STREAM))
    cl = channel_list(quinn)
    check("member got a channel-list push with my_permissions", cl is not None and "general" in cl and not (cl["general"].my_permissions & pb.PERM_SEND_MESSAGES))
    quinn.flush(0.3)
    quinn.send(pb.Packet.CHANNEL_MSG, channel_msg=pb.ChannelMessage(channel_id="general", content="blocked?"))
    r = quinn.wait(pb.Packet.MOD_ACTION_RES, pred=lambda p: p.mod_action_res.action == "message")
    check("message rejected without SEND_MESSAGES", r is not None and not r.mod_action_res.success)
    check("owner didn't receive it", owner.wait(pb.Packet.CHANNEL_MSG, timeout=1.0) is None)
    quinn.send(pb.Packet.JOIN_VOICE_REQ, join_voice_req=pb.JoinVoiceRequest(channel_id="voice-lounge"))
    r = quinn.wait(pb.Packet.MOD_ACTION_RES, pred=lambda p: p.mod_action_res.action == "voice")
    check("voice join rejected without CONNECT_VOICE", r is not None and not r.mod_action_res.success)
    # restore CONNECT but not STREAM
    set_everyone_perms(owner, ALL_MEMBER_BITS & ~pb.PERM_STREAM)
    quinn.flush(0.5)
    quinn.send(pb.Packet.JOIN_VOICE_REQ, join_voice_req=pb.JoinVoiceRequest(channel_id="voice-lounge"))
    vp = owner.wait(pb.Packet.VOICE_PRESENCE_UPDATE, timeout=2, pred=lambda p: "quinn" in p.voice_presence_update.active_users)
    check("voice join allowed again", vp is not None)
    quinn.send(pb.Packet.START_STREAM_REQ, start_stream_req=pb.StartStreamRequest(channel_id="voice-lounge", target_fps=30))
    r = quinn.wait(pb.Packet.MOD_ACTION_RES, pred=lambda p: p.mod_action_res.action == "stream")
    check("stream rejected without STREAM", r is not None and not r.mod_action_res.success)
    set_everyone_perms(owner, ALL_MEMBER_BITS)
    owner.send(pb.Packet.ROLE_LIST_REQ, role_list_req=pb.RoleListRequest()); owner.wait(pb.Packet.ROLE_LIST_RES)
    quinn.close(); owner.close()


def test_v2_private_channel():
    print("[v2] per-channel overwrites: private + read-only channels")
    owner = Client("alice"); assert auth_ok(owner)[0]
    ev = everyone_role(owner)
    rae = join("rae", owner)
    owner.flush(0.5); rae.flush(0.5)
    owner.send(pb.Packet.CHANNEL_CREATE_REQ, channel_create_req=pb.ChannelCreateRequest(name="staff", type=pb.ChannelInfo.TEXT))
    cr = owner.wait(pb.Packet.CHANNEL_ACTION_RES, pred=lambda p: p.channel_action_res.action == "create")
    staff = cr.channel_action_res.channel.id
    cl = channel_list(rae); check("member sees new channel before overwrite", cl is not None and staff in cl)
    rae.flush(0.3); owner.flush(0.3)
    # deny VIEW for everyone → private
    r = set_overwrite(owner, staff, pb.ChannelOverwrite.ROLE, ev.id, deny=pb.PERM_VIEW_CHANNEL)
    check("owner set everyone-deny VIEW", r is not None and r.channel_action_res.success, r.channel_action_res.message if r else None)
    cl = channel_list(rae); check("member's list no longer contains staff", cl is not None and staff not in cl, str(sorted(cl) if cl else None))
    ocl = channel_list(owner); check("owner still sees staff", ocl is not None and staff in ocl)
    rae.flush(0.3); owner.flush(0.3)
    rae.send(pb.Packet.CHANNEL_MSG, channel_msg=pb.ChannelMessage(channel_id=staff, content="sneaky"))
    r = rae.wait(pb.Packet.MOD_ACTION_RES, pred=lambda p: p.mod_action_res.action == "message")
    check("member can't post into hidden channel", r is not None and not r.mod_action_res.success)
    owner.send(pb.Packet.CHANNEL_MSG, channel_msg=pb.ChannelMessage(channel_id=staff, content="secret"))
    check("owner's message not delivered to member", rae.wait(pb.Packet.CHANNEL_MSG, timeout=1.0, pred=lambda p: p.channel_msg.channel_id == staff) is None)
    rae.send(pb.Packet.CHANNEL_HISTORY_REQ, channel_history_req=pb.ChannelHistoryRequest(channel_id=staff, before_id=0, limit=50))
    h = rae.wait(pb.Packet.CHANNEL_HISTORY_RES)
    check("history of hidden channel is empty for member", h is not None and len(h.channel_history_res.messages) == 0)
    # overwrites listing: member denied, owner ok
    rae.send(pb.Packet.CHANNEL_OVERWRITES_REQ, channel_overwrites_req=pb.ChannelOverwritesRequest(channel_id=staff))
    o = rae.wait(pb.Packet.CHANNEL_OVERWRITES_RES)
    check("member can't list overwrites", o is not None and not o.channel_overwrites_res.success)
    owner.send(pb.Packet.CHANNEL_OVERWRITES_REQ, channel_overwrites_req=pb.ChannelOverwritesRequest(channel_id=staff))
    o = owner.wait(pb.Packet.CHANNEL_OVERWRITES_RES, pred=lambda p: p.channel_overwrites_res.success)
    check("owner lists the everyone overwrite", o is not None and len(o.channel_overwrites_res.overwrites) == 1 and o.channel_overwrites_res.overwrites[0].deny == pb.PERM_VIEW_CHANNEL)
    # member overwrite lets rae in
    r = set_overwrite(owner, staff, pb.ChannelOverwrite.MEMBER, "rae", allow=pb.PERM_VIEW_CHANNEL)
    check("member-allow VIEW set", r is not None and r.channel_action_res.success)
    cl = channel_list(rae)
    check("member sees staff again with VIEW+SEND", cl is not None and staff in cl and (cl[staff].my_permissions & (pb.PERM_VIEW_CHANNEL | pb.PERM_SEND_MESSAGES)) == (pb.PERM_VIEW_CHANNEL | pb.PERM_SEND_MESSAGES))
    rae.flush(0.3); owner.flush(0.3)
    owner.send(pb.Packet.CHANNEL_MSG, channel_msg=pb.ChannelMessage(channel_id=staff, content="welcome"))
    check("owner's message now delivered", rae.wait(pb.Packet.CHANNEL_MSG, timeout=2, pred=lambda p: p.channel_msg.channel_id == staff) is not None)
    # read-only: deny SEND for everyone on general
    r = set_overwrite(owner, "general", pb.ChannelOverwrite.ROLE, ev.id, deny=pb.PERM_SEND_MESSAGES)
    cl = channel_list(rae)
    check("read-only: my_permissions lacks SEND on general", cl is not None and not (cl["general"].my_permissions & pb.PERM_SEND_MESSAGES) and (cl["general"].my_permissions & pb.PERM_VIEW_CHANNEL))
    rae.flush(0.3)
    rae.send(pb.Packet.CHANNEL_MSG, channel_msg=pb.ChannelMessage(channel_id="general", content="nope"))
    r = rae.wait(pb.Packet.MOD_ACTION_RES, pred=lambda p: p.mod_action_res.action == "message")
    check("member can't post in read-only channel", r is not None and not r.mod_action_res.success)
    owner.flush(0.3)
    owner.send(pb.Packet.CHANNEL_MSG, channel_msg=pb.ChannelMessage(channel_id="general", content="announcement"))
    check("member still receives owner's post", rae.wait(pb.Packet.CHANNEL_MSG, timeout=2, pred=lambda p: p.channel_msg.content == "announcement") is not None)
    # clear
    set_overwrite(owner, "general", pb.ChannelOverwrite.ROLE, ev.id)
    # attachment GET in private channel: 403 for a member without VIEW
    set_overwrite(owner, staff, pb.ChannelOverwrite.MEMBER, "rae")   # clear member overwrite → hidden again
    now = int(time.time())
    mid = sql("insert into messages(channel_id, sender, content, timestamp) values(?,'alice','f',?) returning id", staff, now)[0][0]
    path = os.path.join(RUN, "att", "secret.bin"); os.makedirs(os.path.dirname(path), exist_ok=True); open(path, "wb").write(b"top secret")
    aid = sql("insert into attachments(message_id, kind, filename, mime, size_bytes, storage_path, position, created_at, upload_status, uploader, channel_id) "
              "values(?,2,'s.bin','application/octet-stream',10,?,0,?,'ready','alice',?) returning id", mid, path, now, staff)[0][0]
    def http_get(user):
        sk = raw_tls(8085)
        sk.sendall(f"GET /attachments/{aid} HTTP/1.1\r\nHost: x\r\nAuthorization: Bearer {jwt(user)}\r\n\r\n".encode())
        sk.settimeout(3); data = b""
        try:
            while True:
                chunk = sk.recv(4096)
                if not chunk: break
                data += chunk
        except Exception: pass
        sk.close(); return data[:12]
    check("attachment GET in hidden channel → 403 for member", http_get("rae").startswith(b"HTTP/1.1 403"))
    check("attachment GET in hidden channel → 200 for owner", http_get("alice").startswith(b"HTTP/1.1 200"))
    rae.close(); owner.close()


def test_v2_overwrite_guards():
    print("[v2] overwrite escalation + lock-out guards, hierarchy rule")
    owner = Client("alice"); assert auth_ok(owner)[0]
    ev = everyone_role(owner)
    sam = join("sam", owner); tia = join("tia", owner)
    owner.flush(0.5); sam.flush(0.5); tia.flush(0.5)
    # sam gets a role with MANAGE_ROLES (but not MANAGE_SERVER / BAN)
    owner.send(pb.Packet.ROLE_CREATE_REQ, role_create_req=pb.RoleCreateRequest(name="Curator", color=0, permissions=pb.PERM_MANAGE_ROLES))
    role = owner.wait(pb.Packet.ROLE_ACTION_RES, pred=lambda p: p.role_action_res.action == "create").role_action_res.role
    owner.send(pb.Packet.MEMBER_ROLES_UPDATE_REQ, member_roles_update_req=pb.MemberRolesUpdateRequest(username="sam", role_ids=[role.id]))
    owner.wait(pb.Packet.ROLE_ACTION_RES, pred=lambda p: p.role_action_res.action == "assign")
    sam.flush(0.5)
    r = set_overwrite(sam, "general", pb.ChannelOverwrite.MEMBER, "sam", allow=pb.PERM_BAN_MEMBERS)
    check("can't grant a bit you don't hold in the channel", r is not None and not r.channel_action_res.success, r.channel_action_res.message if r else None)
    r = set_overwrite(sam, "general", pb.ChannelOverwrite.ROLE, ev.id, deny=pb.PERM_VIEW_CHANNEL)
    check("lock-out guard: can't deny your own VIEW", r is not None and not r.channel_action_res.success, r.channel_action_res.message if r else None)
    r = set_overwrite(sam, "general", pb.ChannelOverwrite.MEMBER, "tia", deny=pb.PERM_SEND_MESSAGES)
    check("can deny a bit you hold (mute tia in #general)", r is not None and r.channel_action_res.success, r.channel_action_res.message if r else None)
    tia.flush(0.5)
    tia.send(pb.Packet.CHANNEL_MSG, channel_msg=pb.ChannelMessage(channel_id="general", content="hi"))
    r = tia.wait(pb.Packet.MOD_ACTION_RES, pred=lambda p: p.mod_action_res.action == "message")
    check("tia muted in #general", r is not None and not r.mod_action_res.success)
    # hierarchy rule: everyone with KICK → two role-less members can't kick each other
    set_everyone_perms(owner, ALL_MEMBER_BITS | pb.PERM_KICK_MEMBERS)
    tia.flush(0.5)
    uma = join("uma", owner); tia.flush(0.5); uma.flush(0.5)
    tia.send(pb.Packet.KICK_MEMBER_REQ, kick_member_req=pb.KickMemberRequest(username="uma", reason=""))
    r = tia.wait(pb.Packet.MOD_ACTION_RES, pred=lambda p: p.mod_action_res.action == "kick")
    check("role-less member can't kick a role-less member (equal level)", r is not None and not r.mod_action_res.success)
    sam.flush(0.5)
    sam.send(pb.Packet.KICK_MEMBER_REQ, kick_member_req=pb.KickMemberRequest(username="uma", reason=""))
    r = sam.wait(pb.Packet.MOD_ACTION_RES, pred=lambda p: p.mod_action_res.action == "kick")
    check("role holder (level 1) can kick a role-less member", r is not None and r.mod_action_res.success, r.mod_action_res.message if r else None)
    set_everyone_perms(owner, ALL_MEMBER_BITS)
    set_overwrite(owner, "general", pb.ChannelOverwrite.MEMBER, "tia")
    # role delete cascades its overwrites
    set_overwrite(owner, "general", pb.ChannelOverwrite.ROLE, role.id, deny=pb.PERM_ATTACH_FILES)
    owner.send(pb.Packet.ROLE_DELETE_REQ, role_delete_req=pb.RoleDeleteRequest(role_id=role.id))
    owner.wait(pb.Packet.ROLE_ACTION_RES, pred=lambda p: p.role_action_res.action == "delete")
    rows = sql("select count(*) from channel_overwrites where target_type=0 and target_id=?", str(role.id))
    check("role delete cascades its overwrites", rows == [(0,)])
    for c in (sam, tia, uma, owner): c.close()


def test_roster_deltas():
    print("[roster] paged snapshot + MEMBER_UPSERT/REMOVE deltas + revision")
    owner = Client("alice"); assert auth_ok(owner)[0]
    owner.flush(0.5)
    # Seed 250 offline members directly (joined long ago) to exercise paging.
    now = int(time.time())
    c = sqlite3.connect(DB)
    for i in range(250):
        c.execute("insert or ignore into members(username, joined_at, nickname) values(?, ?, '')", (f"zz{i:03d}", now))
    c.commit(); c.close()
    owner.send(pb.Packet.MEMBER_LIST_REQ, member_list_req=pb.MemberListRequest(after="", limit=100))
    p1 = owner.wait(pb.Packet.MEMBER_LIST_RES, pred=lambda p: p.member_list_res.first_page)
    r1 = p1.member_list_res
    online = [m.username for m in r1.members if m.is_online]
    offline = [m.username for m in r1.members if not m.is_online]
    check("first page: all online members + 100 offline", "alice" in online and len(offline) == 100 and r1.has_more, f"online={len(online)} offline={len(offline)} has_more={r1.has_more}")
    check("total_members reported", r1.total_members >= 251, str(r1.total_members))
    seen = set(offline)
    after = r1.next_after; pages = 1
    while after:
        owner.send(pb.Packet.MEMBER_LIST_REQ, member_list_req=pb.MemberListRequest(after=after, limit=100))
        pg = owner.wait(pb.Packet.MEMBER_LIST_RES, pred=lambda p: not p.member_list_res.first_page)
        r = pg.member_list_res; pages += 1
        names = [m.username for m in r.members]
        check(f"page {pages}: offline only, no duplicates", all(not m.is_online for m in r.members) and not (seen & set(names)))
        seen |= set(names)
        after = r.next_after if r.has_more else ""
    check("paging covers every offline member exactly once", len(seen) == r1.total_members - len(online), f"{len(seen)} vs {r1.total_members - len(online)}")
    rev = r1.revision
    # presence flip → upsert with is_online
    vic = join("vic", owner)
    up = owner.wait(pb.Packet.MEMBER_UPSERT, timeout=2, pred=lambda p: p.member_upsert.member.username == "vic")
    check("join → MEMBER_UPSERT online", up is not None and up.member_upsert.member.is_online)
    check("revision increased", up is not None and up.member_upsert.revision > rev)
    rev = up.member_upsert.revision
    vic.close()
    up = owner.wait(pb.Packet.MEMBER_UPSERT, timeout=3, pred=lambda p: p.member_upsert.member.username == "vic" and not p.member_upsert.member.is_online)
    check("disconnect → MEMBER_UPSERT offline", up is not None)
    check("revision strictly monotonic", up is not None and up.member_upsert.revision == rev + 1, f"{up.member_upsert.revision if up else None} vs {rev}")
    # two sessions: only the last disconnect flips presence
    w1 = join("wes", owner, nonce="a"); owner.wait(pb.Packet.MEMBER_UPSERT, timeout=2, pred=lambda p: p.member_upsert.member.username == "wes")
    w2 = Client("wes", nonce="b"); assert auth_ok(w2)[0]; owner.flush(0.5)
    w1.close()
    up = owner.wait(pb.Packet.MEMBER_UPSERT, timeout=1.5, pred=lambda p: p.member_upsert.member.username == "wes" and not p.member_upsert.member.is_online)
    check("first of two sessions closing → still online (no offline upsert)", up is None)
    w2.close()
    up = owner.wait(pb.Packet.MEMBER_UPSERT, timeout=3, pred=lambda p: p.member_upsert.member.username == "wes" and not p.member_upsert.member.is_online)
    check("last session closing → offline upsert", up is not None)
    # ban list request gating
    xan = join("xan", owner); xan.flush(0.5)
    xan.send(pb.Packet.BAN_LIST_REQ, ban_list_req=pb.BanListRequest())
    bl = xan.wait(pb.Packet.BAN_LIST_RES)
    check("BAN_LIST_REQ denied without BAN_MEMBERS", bl is not None and not bl.ban_list_res.success)
    owner.send(pb.Packet.BAN_LIST_REQ, ban_list_req=pb.BanListRequest())
    bl = owner.wait(pb.Packet.BAN_LIST_RES, pred=lambda p: p.ban_list_res.success)
    check("owner gets ban list", bl is not None)
    sql("delete from members where username like 'zz%'")
    xan.close(); owner.close()


def test_server_update_and_transfer():
    print("[mgmt] server rename + ownership transfer")
    owner = Client("alice"); assert auth_ok(owner)[0]
    yul = join("yul", owner)
    owner.flush(0.5); yul.flush(0.5)
    yul.send(pb.Packet.SERVER_UPDATE_REQ, server_update_req=pb.ServerUpdateRequest(name="hack", description=""))
    r = yul.wait(pb.Packet.SERVER_UPDATE_RES)
    check("rename denied without MANAGE_SERVER", r is not None and not r.server_update_res.success)
    owner.send(pb.Packet.SERVER_UPDATE_REQ, server_update_req=pb.ServerUpdateRequest(name="Renamed Server", description="new desc"))
    r = owner.wait(pb.Packet.SERVER_UPDATE_RES)
    check("owner renames", r is not None and r.server_update_res.success)
    m = yul.wait(pb.Packet.SERVER_META_UPDATE, timeout=2)
    check("SERVER_META_UPDATE broadcast with new name", m is not None and m.server_meta_update.server_name == "Renamed Server" and m.server_meta_update.server_description == "new desc")
    check("DB is source of truth", sql("select value from server_meta where key='server_name'") == [("Renamed Server",)])
    # fresh auth reflects it
    c = Client("alice"); ok, ar = auth_ok(c); check("auth response carries new name", ok and ar.community_auth_res.server_name == "Renamed Server"); c.close()
    # transfer: non-owner denied; owner → yul
    yul.flush(0.3)
    yul.send(pb.Packet.TRANSFER_OWNERSHIP_REQ, transfer_ownership_req=pb.TransferOwnershipRequest(new_owner="yul"))
    r = yul.wait(pb.Packet.MOD_ACTION_RES, pred=lambda p: p.mod_action_res.action == "transfer")
    check("transfer denied for non-owner", r is not None and not r.mod_action_res.success)
    owner.flush(0.3)
    owner.send(pb.Packet.TRANSFER_OWNERSHIP_REQ, transfer_ownership_req=pb.TransferOwnershipRequest(new_owner="yul"))
    r = owner.wait(pb.Packet.MOD_ACTION_RES, pred=lambda p: p.mod_action_res.action == "transfer")
    check("owner transfers to yul", r is not None and r.mod_action_res.success, r.mod_action_res.message if r else None)
    m = yul.wait(pb.Packet.SERVER_META_UPDATE, timeout=2, pred=lambda p: p.server_meta_update.owner_username == "yul")
    check("SERVER_META_UPDATE announces new owner", m is not None)
    up = yul.wait(pb.Packet.MEMBER_UPSERT, timeout=2, pred=lambda p: p.member_upsert.member.username == "yul" and p.member_upsert.member.is_owner)
    check("MEMBER_UPSERT marks yul as owner", up is not None)
    # old owner can no longer rename; new owner can
    owner.flush(0.3)
    owner.send(pb.Packet.SERVER_UPDATE_REQ, server_update_req=pb.ServerUpdateRequest(name="Alice again", description=""))
    r = owner.wait(pb.Packet.SERVER_UPDATE_RES)
    check("former owner lost MANAGE_SERVER", r is not None and not r.server_update_res.success)
    # transfer back so later tests keep alice as owner
    yul.send(pb.Packet.TRANSFER_OWNERSHIP_REQ, transfer_ownership_req=pb.TransferOwnershipRequest(new_owner="alice"))
    r = yul.wait(pb.Packet.MOD_ACTION_RES, pred=lambda p: p.mod_action_res.action == "transfer")
    check("transfer back", r is not None and r.mod_action_res.success)
    yul.close(); owner.close()


def test_audit_log():
    print("[audit] audit log records mod actions, gated on VIEW_AUDIT_LOG")
    owner = Client("alice"); assert auth_ok(owner)[0]
    zed = join("zed", owner); owner.flush(0.5); zed.flush(0.5)
    zed.send(pb.Packet.AUDIT_LOG_REQ, audit_log_req=pb.AuditLogRequest(before_id=0, limit=10))
    r = zed.wait(pb.Packet.AUDIT_LOG_RES)
    check("denied without VIEW_AUDIT_LOG", r is not None and not r.audit_log_res.success)
    owner.send(pb.Packet.AUDIT_LOG_REQ, audit_log_req=pb.AuditLogRequest(before_id=0, limit=100))
    r = owner.wait(pb.Packet.AUDIT_LOG_RES)
    actions = [e.action for e in r.audit_log_res.entries] if r else []
    check("owner sees entries newest-first incl. ownership_transfer + server_update", r is not None and r.audit_log_res.success
          and "ownership_transfer" in actions and "server_update" in actions and actions.index("ownership_transfer") < actions.index("server_update"), str(actions[:8]))
    first_id = r.audit_log_res.entries[0].id
    owner.send(pb.Packet.AUDIT_LOG_REQ, audit_log_req=pb.AuditLogRequest(before_id=first_id, limit=5))
    r2 = owner.wait(pb.Packet.AUDIT_LOG_RES)
    check("paging with before_id excludes newer", r2 is not None and all(e.id < first_id for e in r2.audit_log_res.entries) and len(r2.audit_log_res.entries) <= 5)
    zed.close(); owner.close()


def test_timeouts():
    print("[timeout] timed-out member can't post / join voice; clear restores")
    owner = Client("alice"); assert auth_ok(owner)[0]
    amy = join("amy", owner); owner.flush(0.5); amy.flush(0.5)
    amy.send(pb.Packet.TIMEOUT_MEMBER_REQ, timeout_member_req=pb.TimeoutMemberRequest(username="alice", until=int(time.time()) + 60))
    r = amy.wait(pb.Packet.MOD_ACTION_RES, pred=lambda p: p.mod_action_res.action == "timeout")
    check("denied without MODERATE_MEMBERS / on owner", r is not None and not r.mod_action_res.success)
    owner.send(pb.Packet.TIMEOUT_MEMBER_REQ, timeout_member_req=pb.TimeoutMemberRequest(username="amy", until=int(time.time()) + 60, reason="cool off"))
    r = owner.wait(pb.Packet.MOD_ACTION_RES, pred=lambda p: p.mod_action_res.action == "timeout")
    check("owner times out amy", r is not None and r.mod_action_res.success)
    up = amy.wait(pb.Packet.MEMBER_UPSERT, timeout=2, pred=lambda p: p.member_upsert.member.username == "amy")
    check("MEMBER_UPSERT carries timed_out_until", up is not None and up.member_upsert.member.timed_out_until > int(time.time()))
    amy.send(pb.Packet.CHANNEL_MSG, channel_msg=pb.ChannelMessage(channel_id="general", content="hi"))
    r = amy.wait(pb.Packet.MOD_ACTION_RES, pred=lambda p: p.mod_action_res.action == "message")
    check("message rejected while timed out", r is not None and not r.mod_action_res.success and "timed out" in r.mod_action_res.message)
    amy.send(pb.Packet.JOIN_VOICE_REQ, join_voice_req=pb.JoinVoiceRequest(channel_id="voice-lounge"))
    r = amy.wait(pb.Packet.MOD_ACTION_RES, pred=lambda p: p.mod_action_res.action == "voice")
    check("voice join rejected while timed out", r is not None and not r.mod_action_res.success)
    owner.send(pb.Packet.TIMEOUT_MEMBER_REQ, timeout_member_req=pb.TimeoutMemberRequest(username="amy", until=0))
    owner.wait(pb.Packet.MOD_ACTION_RES, pred=lambda p: p.mod_action_res.action == "timeout")
    amy.flush(0.5); owner.flush(0.3)
    amy.send(pb.Packet.CHANNEL_MSG, channel_msg=pb.ChannelMessage(channel_id="general", content="back"))
    check("message delivered after clear", owner.wait(pb.Packet.CHANNEL_MSG, timeout=2, pred=lambda p: p.channel_msg.content == "back") is not None)
    amy.close(); owner.close()


def test_ban_expiry_and_purge():
    print("[ban] expiry lifts the ban; purge deletes recent messages")
    owner = Client("alice"); assert auth_ok(owner)[0]
    ben = join("ben", owner); owner.flush(0.5); ben.flush(0.5)
    for i in range(3):
        ben.send(pb.Packet.CHANNEL_MSG, channel_msg=pb.ChannelMessage(channel_id="general", content=f"spam {i}"))
    owner.drain(1.0)
    ids = [p.channel_msg.id for p in owner.inbox if p.type == pb.Packet.CHANNEL_MSG and p.channel_msg.sender == "ben"]
    check("3 messages landed", len(ids) == 3)
    owner.inbox.clear()
    owner.send(pb.Packet.BAN_MEMBER_REQ, ban_member_req=pb.BanMemberRequest(username="ben", reason="spam", expires_at=int(time.time()) + 6, delete_message_seconds=3600))
    r = owner.wait(pb.Packet.MOD_ACTION_RES, pred=lambda p: p.mod_action_res.action == "ban")
    check("temp ban with purge succeeded", r is not None and r.mod_action_res.success)
    rev = ben.wait(pb.Packet.MEMBERSHIP_REVOKED, timeout=2)
    check("target sees reason + expiry", rev is not None and rev.membership_revoked.reason == "spam" and rev.membership_revoked.expires_at > 0)
    owner.drain(1.5)
    deleted = sorted(p.channel_message_deleted.message_id for p in owner.inbox if p.type == pb.Packet.CHANNEL_MESSAGE_DELETED)
    check("CHANNEL_MESSAGE_DELETED for each purged message", deleted == sorted(ids), f"{deleted} vs {sorted(ids)}")
    check("rows gone", sql("select count(*) from messages where sender='ben'") == [(0,)])
    bl = next((p for p in owner.inbox if p.type == pb.Packet.BAN_LIST_RES), None)
    e = next((e for e in bl.ban_list_res.entries if e.username == "ben"), None) if bl else None
    check("ban list entry has reason/by/expiry", e is not None and e.reason == "spam" and e.banned_by == "alice" and e.expires_at > 0)
    ben.close()
    c = Client("ben"); ok, ar = auth_ok(c)
    check("still banned before expiry", not ok and ar.community_auth_res.error_code == "banned"); c.close()
    time.sleep(6.5)
    code = make_invite(owner)
    c = Client("ben", invite=code); ok, ar = auth_ok(c)
    check("ban expired → can rejoin with invite", ok, ar.community_auth_res.error_code if ar else None); c.close()
    owner.close()


def test_slowmode():
    print("[slowmode] one message per N seconds unless MANAGE_MESSAGES")
    owner = Client("alice"); assert auth_ok(owner)[0]
    cal = join("cal", owner); owner.flush(0.5); cal.flush(0.5)
    owner.send(pb.Packet.CHANNEL_UPDATE_REQ, channel_update_req=pb.ChannelUpdateRequest(channel_id="general", slowmode_seconds=3))
    r = owner.wait(pb.Packet.CHANNEL_UPDATE_RES)
    check("slowmode set", r is not None and r.channel_update_res.success and r.channel_update_res.channel.slowmode_seconds == 3)
    cal.flush(0.5)
    cal.send(pb.Packet.CHANNEL_MSG, channel_msg=pb.ChannelMessage(channel_id="general", content="one"))
    cal.send(pb.Packet.CHANNEL_MSG, channel_msg=pb.ChannelMessage(channel_id="general", content="two"))
    r = cal.wait(pb.Packet.MOD_ACTION_RES, timeout=2, pred=lambda p: p.mod_action_res.action == "message")
    check("second message within window rejected", r is not None and "Slowmode" in r.mod_action_res.message)
    owner.flush(0.5)
    owner.send(pb.Packet.CHANNEL_MSG, channel_msg=pb.ChannelMessage(channel_id="general", content="a"))
    owner.send(pb.Packet.CHANNEL_MSG, channel_msg=pb.ChannelMessage(channel_id="general", content="b"))
    owner.drain(1.0)
    got = [p.channel_msg.content for p in owner.inbox if p.type == pb.Packet.CHANNEL_MSG]
    check("owner (MANAGE_MESSAGES) bypasses slowmode", "a" in got and "b" in got)
    owner.send(pb.Packet.CHANNEL_UPDATE_REQ, channel_update_req=pb.ChannelUpdateRequest(channel_id="general", slowmode_seconds=0))
    owner.wait(pb.Packet.CHANNEL_UPDATE_RES)
    cal.close(); owner.close()


def test_voice_moderation():
    print("[voice mod] server mute/deafen, move, disconnect")
    owner = Client("alice"); assert auth_ok(owner)[0]
    dee = join("dee", owner); owner.flush(0.5); dee.flush(0.5)
    dee.send(pb.Packet.JOIN_VOICE_REQ, join_voice_req=pb.JoinVoiceRequest(channel_id="voice-lounge"))
    owner.wait(pb.Packet.VOICE_PRESENCE_UPDATE, pred=lambda p: "dee" in p.voice_presence_update.active_users)
    dee.send(pb.Packet.VOICE_MOD_REQ, voice_mod_req=pb.VoiceModRequest(username="alice", action=pb.VoiceModRequest.SERVER_MUTE))
    r = dee.wait(pb.Packet.MOD_ACTION_RES, pred=lambda p: p.mod_action_res.action == "voice_mod")
    check("denied without VOICE_MODERATE", r is not None and not r.mod_action_res.success)
    owner.flush(0.3)
    owner.send(pb.Packet.VOICE_MOD_REQ, voice_mod_req=pb.VoiceModRequest(username="dee", action=pb.VoiceModRequest.SERVER_MUTE))
    r = owner.wait(pb.Packet.MOD_ACTION_RES, pred=lambda p: p.mod_action_res.action == "voice_mod")
    check("owner server-mutes dee", r is not None and r.mod_action_res.success, r.mod_action_res.message if r else None)
    vp = owner.wait(pb.Packet.VOICE_PRESENCE_UPDATE, timeout=2, pred=lambda p: any(s.username == "dee" and s.is_server_muted for s in p.voice_presence_update.user_states))
    check("presence shows is_server_muted", vp is not None)
    check("persisted on member", sql("select server_muted from members where username='dee'") == [(1,)])
    owner.send(pb.Packet.VOICE_MOD_REQ, voice_mod_req=pb.VoiceModRequest(username="dee", action=pb.VoiceModRequest.SERVER_UNMUTE))
    owner.wait(pb.Packet.MOD_ACTION_RES, pred=lambda p: p.mod_action_res.action == "voice_mod")
    dee.flush(0.5); owner.flush(0.5)
    owner.send(pb.Packet.VOICE_MOD_REQ, voice_mod_req=pb.VoiceModRequest(username="dee", action=pb.VoiceModRequest.MOVE, channel_id="voice-lounge-2"))
    r = owner.wait(pb.Packet.MOD_ACTION_RES, pred=lambda p: p.mod_action_res.action == "voice_mod")
    check("move succeeded", r is not None and r.mod_action_res.success, r.mod_action_res.message if r else None)
    n = dee.wait(pb.Packet.VOICE_FORCE_NOTIFY, timeout=2)
    check("target gets VOICE_FORCE_NOTIFY MOVED", n is not None and n.voice_force_notify.action == pb.VoiceForceNotify.MOVED and n.voice_force_notify.channel_id == "voice-lounge-2")
    vp = owner.wait(pb.Packet.VOICE_PRESENCE_UPDATE, timeout=2, pred=lambda p: p.voice_presence_update.channel_id == "voice-lounge-2" and "dee" in p.voice_presence_update.active_users)
    check("presence in new channel", vp is not None)
    owner.flush(0.3); dee.flush(0.3)
    owner.send(pb.Packet.VOICE_MOD_REQ, voice_mod_req=pb.VoiceModRequest(username="dee", action=pb.VoiceModRequest.DISCONNECT))
    r = owner.wait(pb.Packet.MOD_ACTION_RES, pred=lambda p: p.mod_action_res.action == "voice_mod")
    check("disconnect succeeded", r is not None and r.mod_action_res.success)
    n = dee.wait(pb.Packet.VOICE_FORCE_NOTIFY, timeout=2)
    check("target gets DISCONNECTED", n is not None and n.voice_force_notify.action == pb.VoiceForceNotify.DISCONNECTED)
    vp = owner.wait(pb.Packet.VOICE_PRESENCE_UPDATE, timeout=2, pred=lambda p: p.voice_presence_update.channel_id == "voice-lounge-2" and len(p.voice_presence_update.active_users) == 0)
    check("presence cleared", vp is not None)
    owner.send(pb.Packet.VOICE_MOD_REQ, voice_mod_req=pb.VoiceModRequest(username="dee", action=pb.VoiceModRequest.DISCONNECT))
    r = owner.wait(pb.Packet.MOD_ACTION_RES, pred=lambda p: p.mod_action_res.action == "voice_mod")
    check("disconnect of a non-voice member fails cleanly", r is not None and not r.mod_action_res.success)
    dee.close(); owner.close()


def udp_audio_packet(jwt_token, seq, payload):
    sid = jwt_token[-31:].encode().ljust(32, b"\0")
    return bytes([0]) + sid + struct.pack("<HH", seq, len(payload)) + payload


def test_udp_relay():
    print("[udp] voice relay: ping echo, audio fan-out, server-mute drop, media ping")
    owner = Client("alice"); assert auth_ok(owner)[0]
    ann = join("ann", owner); bob = join("bobby", owner)
    for c in (ann, bob):
        c.send(pb.Packet.JOIN_VOICE_REQ, join_voice_req=pb.JoinVoiceRequest(channel_id="voice-lounge"))
    owner.wait(pb.Packet.VOICE_PRESENCE_UPDATE, pred=lambda p: "bobby" in p.voice_presence_update.active_users)
    ua = socket.socket(socket.AF_INET, socket.SOCK_DGRAM); ua.bind((HOST, 0)); ua.settimeout(2)
    ub = socket.socket(socket.AF_INET, socket.SOCK_DGRAM); ub.bind((HOST, 0)); ub.settimeout(2)
    # PING echo on the voice socket
    ping = bytes([5]) + b"\0" * 32 + struct.pack("<I", 1234)
    ua.sendto(ping, (HOST, 8083))
    check("voice PING echoed", ua.recv(2048) == ping)
    # register endpoints: each sends one AUDIO packet (relayed to the other)
    ub.sendto(udp_audio_packet(bob.jwt, 1, b"b0"), (HOST, 8083))
    ua.sendto(udp_audio_packet(ann.jwt, 1, b"a0"), (HOST, 8083))
    try:
        got = ub.recv(2048)
    except socket.timeout:
        got = b""
    check("audio relayed to the other member with sender rewritten", got[1:33].rstrip(b"\0") == b"ann" and got[-2:] == b"a0")
    # burst: 50 packets, all delivered (drain-per-wakeup path)
    for i in range(50):
        ua.sendto(udp_audio_packet(ann.jwt, 2 + i, b"x" * 100), (HOST, 8083))
    n = 0
    ub.settimeout(0.5)
    try:
        while True:
            ub.recv(2048); n += 1
    except socket.timeout:
        pass
    check("burst of 50 relayed (drained per wakeup)", n == 50, f"got {n}")
    # server-mute ann → her audio is dropped at the relay
    owner.send(pb.Packet.VOICE_MOD_REQ, voice_mod_req=pb.VoiceModRequest(username="ann", action=pb.VoiceModRequest.SERVER_MUTE))
    owner.wait(pb.Packet.MOD_ACTION_RES, pred=lambda p: p.mod_action_res.action == "voice_mod")
    time.sleep(0.3)
    ua.sendto(udp_audio_packet(ann.jwt, 99, b"muted"), (HOST, 8083))
    ub.settimeout(1.0)
    try:
        ub.recv(2048); dropped = False
    except socket.timeout:
        dropped = True
    check("server-muted member's audio dropped by the relay", dropped)
    owner.send(pb.Packet.VOICE_MOD_REQ, voice_mod_req=pb.VoiceModRequest(username="ann", action=pb.VoiceModRequest.SERVER_UNMUTE))
    owner.wait(pb.Packet.MOD_ACTION_RES, pred=lambda p: p.mod_action_res.action == "voice_mod")
    # media socket PING echo (also registers the media endpoint)
    um = socket.socket(socket.AF_INET, socket.SOCK_DGRAM); um.bind((HOST, 0)); um.settimeout(2)
    mping = bytes([5]) + ann.jwt[-31:].encode().ljust(32, b"\0") + struct.pack("<I", 7)
    um.sendto(mping, (HOST, 8084))
    check("media PING echoed", um.recv(2048) == mping)
    for sk in (ua, ub, um): sk.close()
    ann.close(); bob.close(); owner.close()


def test_http_keepalive_and_fts():
    print("[http] keep-alive on the attachment listener; FTS dropped")
    check("messages_fts table gone", sql("select count(*) from sqlite_master where name='messages_fts'") == [(0,)])
    check("FTS triggers gone", sql("select count(*) from sqlite_master where type='trigger' and name like 'messages_a%'") == [(0,)])
    now = int(time.time())
    mid = sql("insert into messages(channel_id, sender, content, timestamp) values('general','alice','ka',?) returning id", now)[0][0]
    path = os.path.join(RUN, "att", "ka.bin"); os.makedirs(os.path.dirname(path), exist_ok=True); open(path, "wb").write(b"0123456789")
    aid = sql("insert into attachments(message_id, kind, filename, mime, size_bytes, storage_path, position, created_at, upload_status, uploader, channel_id) "
              "values(?,2,'ka.bin','application/octet-stream',10,?,0,?,'ready','alice','general') returning id", mid, path, now)[0][0]
    sk = raw_tls(8085)
    def request(extra=""):
        sk.sendall(f"GET /attachments/{aid} HTTP/1.1\r\nHost: x\r\nAuthorization: Bearer {jwt('alice')}\r\n{extra}\r\n".encode())
        sk.settimeout(3); data = b""
        while b"\r\n\r\n" not in data:
            data += sk.recv(4096)
        head, body = data.split(b"\r\n\r\n", 1)
        clen = int([l for l in head.split(b"\r\n") if l.lower().startswith(b"content-length:")][0].split(b":")[1])
        while len(body) < clen:
            body += sk.recv(4096)
        return head, body
    h1, b1 = request()
    check("first GET on connection: 200 + keep-alive", h1.startswith(b"HTTP/1.1 200") and b"keep-alive" in h1.lower() and b1 == b"0123456789")
    h2, b2 = request()
    check("second GET on the SAME connection served", h2.startswith(b"HTTP/1.1 200") and b2 == b"0123456789")
    h3, b3 = request("Connection: close\r\n")
    check("Connection: close honoured", h3.startswith(b"HTTP/1.1 200") and b"connection: close" in h3.lower())
    check("server closed after it", socket_closed(sk, 3.0))
    sk.close()
    # an error response closes too
    sk = raw_tls(8085)
    sk.sendall(b"GET /attachments/999999 HTTP/1.1\r\nHost: x\r\nAuthorization: Bearer " + jwt("alice").encode() + b"\r\n\r\n")
    sk.settimeout(3); data = b""
    try:
        while True:
            chunk = sk.recv(4096)
            if not chunk: break
            data += chunk
    except socket.timeout:
        pass
    check("error response carries Connection: close and closes", data.startswith(b"HTTP/1.1 404") and b"connection: close" in data.lower())
    sk.close()


def test_theme_a_tokens_and_uid():
    print("[theme A] Ed25519 tokens; HS256 / wrong-key tokens rejected; bans follow the uid")
    # A token signed with a different Ed25519 key must be rejected.
    other = os.path.join(RUN, "other_ed25519.pem")
    if not os.path.exists(other):
        subprocess.run(["openssl", "genpkey", "-algorithm", "ed25519", "-out", other], check=True, capture_output=True)
    h = b64(json.dumps({"alg": "EdDSA", "typ": "JWS"}).encode())
    now = int(time.time())
    p = b64(json.dumps({"iss": "decibell_central_auth", "sub": "alice", "uid": 1, "iat": now, "exp": now + 3600}).encode())
    forged = f"{h}.{p}.{b64(ed25519_sign(other, f'{h}.{p}'.encode()))}"
    ctx = ssl.create_default_context(); ctx.check_hostname = False; ctx.verify_mode = ssl.CERT_NONE
    c = Client.__new__(Client)
    c.s = ctx.wrap_socket(socket.create_connection((HOST, 8082), timeout=3)); c.username = "alice"; c.jwt = forged; c.buf = b""; c.inbox = []; c.closed = False
    c.send(pb.Packet.COMMUNITY_AUTH_REQ, community_auth_req=pb.CommunityAuthRequest(jwt_token=forged))
    ok, r = auth_ok(c)
    check("token signed by another key rejected", not ok and r is not None and r.community_auth_res.error_code == "auth"); c.close()
    # An HS256 token (old scheme) with the community secret as key must be rejected too.
    h2 = b64(json.dumps({"alg": "HS256", "typ": "JWT"}).encode())
    sig2 = hmac.new(SECRET.encode(), f"{h2}.{p}".encode(), hashlib.sha256).digest()
    hs = f"{h2}.{p}.{b64(sig2)}"
    c = Client.__new__(Client)
    c.s = ctx.wrap_socket(socket.create_connection((HOST, 8082), timeout=3)); c.username = "alice"; c.jwt = hs; c.buf = b""; c.inbox = []; c.closed = False
    c.send(pb.Packet.COMMUNITY_AUTH_REQ, community_auth_req=pb.CommunityAuthRequest(jwt_token=hs))
    ok, r = auth_ok(c)
    check("HS256 token rejected (no symmetric fallback)", not ok); c.close()
    # uid-keyed bans: ban "gus" (uid 4242); "gus_renamed" with the same uid is still banned.
    owner = Client("alice"); assert auth_ok(owner)[0]
    code = make_invite(owner)
    gus = Client("gus", invite=code, uid=4242); assert auth_ok(gus)[0]
    check("member row stores uid", sql("select uid from members where username='gus'") == [(4242,)])
    owner.flush(0.5)
    owner.send(pb.Packet.BAN_MEMBER_REQ, ban_member_req=pb.BanMemberRequest(username="gus", reason="bye"))
    owner.wait(pb.Packet.MOD_ACTION_RES, pred=lambda p: p.mod_action_res.action == "ban")
    gus.close()
    code = make_invite(owner)
    c = Client("gus_renamed", invite=code, uid=4242); ok, r = auth_ok(c)
    check("renamed account with the banned uid is still banned", not ok and r.community_auth_res.error_code == "banned"); c.close()
    code = make_invite(owner)
    c = Client("gus_renamed", invite=code, uid=4243); ok, r = auth_ok(c)
    check("different uid with the new name is admitted", ok); c.close()
    owner.send(pb.Packet.UNBAN_MEMBER_REQ, unban_member_req=pb.UnbanMemberRequest(username="gus"))
    owner.wait(pb.Packet.MOD_ACTION_RES, pred=lambda p: p.mod_action_res.action == "unban")
    owner.close()


def test_auth_failure_closes():
    print("[regression] failed auth still delivers response then closes")
    c = Client("nobody")
    ok, r = auth_ok(c)
    check("non-member rejected with not_member", r is not None and not ok and r.community_auth_res.error_code == "not_member")
    check("socket closed after rejection", c.is_closed())
    c.close()


def test_c2_username_reuse():
    print("[C2] reused username must not inherit roles; a central rename keeps them")
    owner = Client("alice"); assert auth_ok(owner)[0]
    owner.flush(0.5)
    owner.send(pb.Packet.ROLE_CREATE_REQ,
               role_create_req=pb.RoleCreateRequest(name="C2Marker", color=0, permissions=pb.PERM_MANAGE_CHANNELS))
    role = owner.wait(pb.Packet.ROLE_ACTION_RES, pred=lambda p: p.role_action_res.action == "create").role_action_res.role

    # A member joins and is granted the role.
    victim_uid = uid_for("c2victim")
    victim = join("c2victim", owner)
    owner.send(pb.Packet.MEMBER_ROLES_UPDATE_REQ,
               member_roles_update_req=pb.MemberRolesUpdateRequest(username="c2victim", role_ids=[role.id]))
    owner.wait(pb.Packet.ROLE_ACTION_RES, pred=lambda p: p.role_action_res.action == "assign")
    check("victim granted the role", sql("select role_id from member_roles where username='c2victim'") == [(role.id,)])
    check("victim row stamped with its uid", sql("select uid from members where username='c2victim'") == [(victim_uid,)])
    victim.close()

    # A DIFFERENT central account (new uid) grabs the freed username and joins via invite.
    impostor = Client("c2victim", invite=make_invite(owner), uid=victim_uid + 500000)
    ok, r = auth_ok(impostor)
    check("reused username admitted as a fresh member", ok, r.community_auth_res.error_code if r else None)
    check("reused username inherited NO roles",
          sql("select role_id from member_roles where username='c2victim'") == [])
    check("member row now carries the new identity's uid",
          sql("select uid from members where username='c2victim'") == [(victim_uid + 500000,)])
    impostor.close()

    # Rename: same uid, new display name → membership + roles follow the identity.
    bob_uid = uid_for("c2bob")
    bob = join("c2bob", owner)
    owner.send(pb.Packet.MEMBER_ROLES_UPDATE_REQ,
               member_roles_update_req=pb.MemberRolesUpdateRequest(username="c2bob", role_ids=[role.id]))
    owner.wait(pb.Packet.ROLE_ACTION_RES, pred=lambda p: p.role_action_res.action == "assign")
    check("bob granted the role", sql("select role_id from member_roles where username='c2bob'") == [(role.id,)])
    bob.close()

    # Reconnect under a new name with the SAME uid — no invite needed (member by identity).
    renamed = Client("c2bob_renamed", uid=bob_uid)
    ok, r = auth_ok(renamed)
    check("renamed account admitted without an invite", ok, r.community_auth_res.error_code if r else None)
    check("rename moved the row to the new name",
          sql("select uid from members where username='c2bob_renamed'") == [(bob_uid,)])
    check("old name no longer present", sql("select count(*) from members where username='c2bob'") == [(0,)])
    check("renamed account kept its role",
          sql("select role_id from member_roles where username='c2bob_renamed'") == [(role.id,)])
    renamed.close(); owner.close()


def test_public_join():
    print("[public] public servers accept invite-less joins; private stay invite-only")
    owner = Client("alice"); assert auth_ok(owner)[0]; owner.flush(0.3)

    # Default (private): a non-member without an invite is rejected.
    c = Client("pubj1"); ok, r = auth_ok(c)
    check("private server rejects invite-less join",
          not ok and r.community_auth_res.error_code == "not_member"); c.close()

    # Owner turns public listing on.
    owner.send(pb.Packet.SERVER_UPDATE_REQ,
               server_update_req=pb.ServerUpdateRequest(name="alice server", description="", public_listing=True))
    r = owner.wait(pb.Packet.SERVER_UPDATE_RES)
    check("public listing enabled", r is not None and r.server_update_res.success)

    # A non-member now joins directly, no invite.
    c = Client("pubj2"); ok, r = auth_ok(c)
    check("public server accepts invite-less join", ok, r.community_auth_res.error_code if r else None)
    check("auth response reports public_listing", ok and r.community_auth_res.public_listing is True)
    check("joiner became a member", sql("select count(*) from members where username='pubj2'") == [(1,)])
    c.close()

    # Bans still win over public join.
    owner.send(pb.Packet.BAN_MEMBER_REQ, ban_member_req=pb.BanMemberRequest(username="pubj2", reason=""))
    owner.wait(pb.Packet.MOD_ACTION_RES, pred=lambda p: p.mod_action_res.action == "ban")
    c = Client("pubj2"); ok, r = auth_ok(c)
    check("banned user rejected even on a public server",
          not ok and r.community_auth_res.error_code == "banned"); c.close()

    # Turn it back off → invite-only again.
    owner.send(pb.Packet.SERVER_UPDATE_REQ,
               server_update_req=pb.ServerUpdateRequest(name="alice server", description="", public_listing=False))
    owner.wait(pb.Packet.SERVER_UPDATE_RES)
    c = Client("pubj3"); ok, r = auth_ok(c)
    check("private again rejects invite-less join",
          not ok and r.community_auth_res.error_code == "not_member"); c.close()
    owner.close()


def test_storage():
    print("[storage] MANAGE_SERVER-gated info; editable headroom; 507 when below it")
    owner = Client("alice"); assert auth_ok(owner)[0]; owner.flush(0.5)

    # A non-privileged member is denied.
    stu = join("stu", owner); stu.flush(0.5)
    stu.send(pb.Packet.STORAGE_INFO_REQ, storage_info_req=pb.StorageInfoRequest())
    r = stu.wait(pb.Packet.STORAGE_INFO_RES, timeout=2)
    check("non-admin denied storage info", r is not None and not r.storage_info_res.success)
    stu.close()

    # Owner gets real numbers.
    owner.send(pb.Packet.STORAGE_INFO_REQ, storage_info_req=pb.StorageInfoRequest())
    r = owner.wait(pb.Packet.STORAGE_INFO_RES, timeout=2)
    si = r.storage_info_res if r else None
    check("owner gets storage info", si is not None and si.success)
    check("volume total/available reported", si is not None and si.volume_total_bytes > 0 and si.volume_available_bytes > 0)
    check("database size reported", si is not None and si.database_bytes > 0)
    total = si.volume_total_bytes if si else 0

    # Set the headroom to the whole volume (clamped to capacity) and read it back.
    owner.send(pb.Packet.STORAGE_CONFIG_SET_REQ, storage_config_set_req=pb.StorageConfigSetRequest(min_free_bytes=total))
    r = owner.wait(pb.Packet.STORAGE_INFO_RES, timeout=2)
    check("min-free updated and echoed (clamped to capacity)", r is not None and r.storage_info_res.min_free_bytes == total)
    check("audit records the change", sql("select count(*) from audit_log where action='storage_min_free'")[0][0] >= 1)

    # With headroom == capacity, any upload init is refused before touching disk.
    sk = raw_tls(8085)
    body = json.dumps({"channelId": "general", "filename": "big.bin",
                       "mime": "application/octet-stream", "size": 1024}).encode()
    sk.sendall(b"POST /attachments/init HTTP/1.1\r\nHost: x\r\nAuthorization: Bearer " + jwt("alice").encode() +
               b"\r\nContent-Type: application/json\r\nContent-Length: " + str(len(body)).encode() + b"\r\n\r\n" + body)
    sk.settimeout(3); data = b""
    try:
        while b"\r\n\r\n" not in data:
            chunk = sk.recv(4096)
            if not chunk: break
            data += chunk
    except socket.timeout:
        pass
    check("upload refused with 507 when below headroom", data.startswith(b"HTTP/1.1 507"), data[:40])
    sk.close()

    # Restore a permissive headroom so later tests can upload.
    owner.send(pb.Packet.STORAGE_CONFIG_SET_REQ, storage_config_set_req=pb.StorageConfigSetRequest(min_free_bytes=0))
    owner.wait(pb.Packet.STORAGE_INFO_RES, timeout=2)
    owner.close()


def test_x3_upload_membership_recheck():
    print("[X3] a member kicked/banned mid-upload can't drive PATCH/DELETE")
    owner = Client("alice"); assert auth_ok(owner)[0]; owner.flush(0.3)
    upl = join("upl", owner); upl.flush(0.3)
    tok = jwt("upl")

    def http(method, path, extra_headers="", body=b""):
        sk = raw_tls(8085)
        sk.sendall((f"{method} {path} HTTP/1.1\r\nHost: x\r\n"
                    f"Authorization: Bearer {tok}\r\n{extra_headers}"
                    f"Content-Length: {len(body)}\r\n\r\n").encode() + body)
        sk.settimeout(3); data = b""
        try:
            while b"\r\n\r\n" not in data:
                chunk = sk.recv(4096)
                if not chunk: break
                data += chunk
        except socket.timeout:
            pass
        head, _, rest = data.partition(b"\r\n\r\n")
        clen = 0
        for line in head.split(b"\r\n"):
            if line.lower().startswith(b"content-length:"):
                try: clen = int(line.split(b":", 1)[1])
                except Exception: clen = 0
        while len(rest) < clen:
            try:
                chunk = sk.recv(4096)
                if not chunk: break
                rest += chunk
            except socket.timeout:
                break
        sk.close()
        return head, rest

    init_body = json.dumps({"channelId": "general", "filename": "x.bin",
                            "mime": "application/octet-stream", "size": 4}).encode()
    head, body = http("POST", "/attachments/init", "Content-Type: application/json\r\n", init_body)
    check("init accepted for a member", head.startswith(b"HTTP/1.1 201"), head[:40])
    aid = json.loads(body.decode() or "{}").get("id")

    # Membership revoked out-of-band, exactly as a kick/ban removes the row.
    sql("delete from members where username='upl'")

    head, _ = http("PATCH", f"/attachments/{aid}", "Upload-Offset: 0\r\n", b"ab")
    check("PATCH refused after membership revoked (X3)", head.startswith(b"HTTP/1.1 403"), head[:40])
    head, _ = http("DELETE", f"/attachments/{aid}")
    check("DELETE refused after membership revoked (X3)", head.startswith(b"HTTP/1.1 403"), head[:40])
    upl.close(); owner.close()


def test_m1_m4_timeout():
    print("[M1/M4] timeout ejects from voice and suspends the member's powers")
    owner = Client("alice"); assert auth_ok(owner)[0]
    owner.flush(0.5)
    owner.send(pb.Packet.ROLE_CREATE_REQ,
               role_create_req=pb.RoleCreateRequest(name="Builder2", color=0, permissions=pb.PERM_MANAGE_CHANNELS))
    role = owner.wait(pb.Packet.ROLE_ACTION_RES, pred=lambda p: p.role_action_res.action == "create").role_action_res.role
    mia = join("mia", owner)
    owner.send(pb.Packet.MEMBER_ROLES_UPDATE_REQ,
               member_roles_update_req=pb.MemberRolesUpdateRequest(username="mia", role_ids=[role.id]))
    owner.wait(pb.Packet.ROLE_ACTION_RES, pred=lambda p: p.role_action_res.action == "assign")
    mia.flush(0.5); owner.flush(0.5)

    # mia joins voice; sanity-check she can use her MANAGE_CHANNELS power first.
    mia.send(pb.Packet.JOIN_VOICE_REQ, join_voice_req=pb.JoinVoiceRequest(channel_id="voice-lounge"))
    owner.wait(pb.Packet.VOICE_PRESENCE_UPDATE, pred=lambda p: "mia" in p.voice_presence_update.active_users)
    mia.flush(0.3)
    mia.send(pb.Packet.CHANNEL_CREATE_REQ, channel_create_req=pb.ChannelCreateRequest(name="mia-pre", type=pb.ChannelInfo.TEXT))
    r = mia.wait(pb.Packet.CHANNEL_ACTION_RES, timeout=2, pred=lambda p: p.channel_action_res.action == "create")
    check("privileged member can create a channel before timeout", r is not None and r.channel_action_res.success)

    # Owner times mia out.
    owner.send(pb.Packet.TIMEOUT_MEMBER_REQ,
               timeout_member_req=pb.TimeoutMemberRequest(username="mia", until=int(time.time()) + 60))
    owner.wait(pb.Packet.MOD_ACTION_RES, pred=lambda p: p.mod_action_res.action == "timeout")
    # M1: mia is force-disconnected from voice.
    fn = mia.wait(pb.Packet.VOICE_FORCE_NOTIFY, timeout=2,
                  pred=lambda p: p.voice_force_notify.action == pb.VoiceForceNotify.DISCONNECTED)
    check("timeout ejects the member from voice (M1)", fn is not None)

    # M4: management power is suspended while timed out.
    mia.flush(0.5)
    mia.send(pb.Packet.CHANNEL_CREATE_REQ, channel_create_req=pb.ChannelCreateRequest(name="mia-during", type=pb.ChannelInfo.TEXT))
    r = mia.wait(pb.Packet.CHANNEL_ACTION_RES, timeout=2, pred=lambda p: p.channel_action_res.action == "create")
    check("timed-out member's manage power is suspended (M4)", r is not None and not r.channel_action_res.success)
    check("no channel created during timeout", sql("select count(*) from channels where name='mia-during'") == [(0,)])

    # Clearing the timeout restores the power.
    owner.send(pb.Packet.TIMEOUT_MEMBER_REQ, timeout_member_req=pb.TimeoutMemberRequest(username="mia", until=0))
    owner.wait(pb.Packet.MOD_ACTION_RES, pred=lambda p: p.mod_action_res.action == "timeout")
    mia.flush(0.5)
    mia.send(pb.Packet.CHANNEL_CREATE_REQ, channel_create_req=pb.ChannelCreateRequest(name="mia-post", type=pb.ChannelInfo.TEXT))
    r = mia.wait(pb.Packet.CHANNEL_ACTION_RES, timeout=2, pred=lambda p: p.channel_action_res.action == "create")
    check("manage power restored after clearing the timeout (M4)", r is not None and r.channel_action_res.success)
    mia.close(); owner.close()


def test_m3_slowmode_per_user():
    print("[M3] slowmode is per-user (survives reconnect); only a delivered message consumes the window")
    owner = Client("alice"); assert auth_ok(owner)[0]
    nate = join("nate", owner); owner.flush(0.5); nate.flush(0.5)
    owner.send(pb.Packet.CHANNEL_UPDATE_REQ, channel_update_req=pb.ChannelUpdateRequest(channel_id="general", slowmode_seconds=30))
    assert owner.wait(pb.Packet.CHANNEL_UPDATE_RES).channel_update_res.success
    owner.flush(0.5); nate.flush(0.5)

    # An oversized message is rejected before it can stamp the window...
    nate.send(pb.Packet.CHANNEL_MSG, channel_msg=pb.ChannelMessage(channel_id="general", content="x" * (64 * 1024 + 1)))
    # ...so the next valid message must still go through.
    nate.send(pb.Packet.CHANNEL_MSG, channel_msg=pb.ChannelMessage(channel_id="general", content="valid1"))
    got = owner.wait(pb.Packet.CHANNEL_MSG, timeout=2, pred=lambda p: p.channel_msg.content == "valid1" and p.channel_msg.sender == "nate")
    check("a rejected (oversized) message does not consume the window", got is not None)

    # The delivered message DID consume it.
    nate.send(pb.Packet.CHANNEL_MSG, channel_msg=pb.ChannelMessage(channel_id="general", content="valid2"))
    r = nate.wait(pb.Packet.MOD_ACTION_RES, timeout=2, pred=lambda p: p.mod_action_res.action == "message")
    check("second delivered message within the window is rejected", r is not None and "Slowmode" in r.mod_action_res.message)

    # A reconnect (fresh session, same uid) must not reset the window.
    nate.close()
    nate2 = Client("nate", uid=uid_for("nate")); assert auth_ok(nate2)[0]
    nate2.flush(0.5)
    nate2.send(pb.Packet.CHANNEL_MSG, channel_msg=pb.ChannelMessage(channel_id="general", content="afterreconnect"))
    r = nate2.wait(pb.Packet.MOD_ACTION_RES, timeout=2, pred=lambda p: p.mod_action_res.action == "message")
    check("slowmode survives a reconnect (per-user, not per-session)", r is not None and "Slowmode" in r.mod_action_res.message)

    owner.send(pb.Packet.CHANNEL_UPDATE_REQ, channel_update_req=pb.ChannelUpdateRequest(channel_id="general", slowmode_seconds=0))
    owner.wait(pb.Packet.CHANNEL_UPDATE_RES)
    nate2.close(); owner.close()


def test_m3_session_cap():
    print("[M3] concurrent sessions per user are capped; the oldest is evicted")
    owner = Client("alice"); assert auth_ok(owner)[0]
    first = join("seso", owner)            # 1st session (via invite)
    extra = []
    for _ in range(8):                     # 8 more with the same uid → 9 total, cap is 8
        c = Client("seso", uid=uid_for("seso")); ok, _ = auth_ok(c); assert ok
        extra.append(c)
    check("oldest session evicted once the cap is exceeded", first.is_closed())
    check("newest session stays connected", not extra[-1].is_closed(0.3))
    first.close()
    for c in extra: c.close()
    owner.close()


if __name__ == "__main__":
    test_b1_seed_resurrection()
    proc = start_server()
    try:
        test_auth_failure_closes()
        test_b2_sync_shutdown()
        test_b3_utf8_truncation()
        test_b21_jwt_leak()
        test_b4_multi_session_ban()
        test_role_assign_guard()
        test_c2_username_reuse()
        test_ghost_stream()
        test_offline_kick_roster()
        test_no_ghost_after_leave_or_ban()
        test_b11_rate_limit()
        test_b10_caps_cap()
        test_b25_invite_params()
        test_b26_stop_watching_spoof()
        test_p2_perm_cache_invalidation()
        test_b20_attachment_url()
        test_auth_server_id_field()
        test_v2_enforced_bits()
        test_v2_private_channel()
        test_v2_overwrite_guards()
        test_roster_deltas()
        test_public_join()
        test_server_update_and_transfer()
        test_audit_log()
        test_timeouts()
        test_m1_m4_timeout()
        test_ban_expiry_and_purge()
        test_slowmode()
        test_m3_slowmode_per_user()
        test_m3_session_cap()
        test_voice_moderation()
        test_udp_relay()
        test_http_keepalive_and_fts()
        test_storage()
        test_x3_upload_membership_recheck()
        test_theme_a_tokens_and_uid()
    finally:
        stop_server(proc)
    test_b9_timeouts()
    test_b12_b27_retention_sweep()
    print(f"\n{len(PASS)} passed, {len(FAIL)} failed")
    if FAIL:
        print("FAILED:", FAIL)
        sys.exit(1)
