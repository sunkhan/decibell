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
# Optional: the community binary from before a schema migration (built from
# that commit with build.sh). When set, the v15 (users by uid) migration
# check seeds a database with it.
OLD_SERVER_BIN = os.environ.get("DECIBELL_E2E_SERVER_OLD", "")
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


def jwt(username, nonce="", uid=None, iat=None):
    ensure_keys()
    h = b64(json.dumps({"alg": "EdDSA", "typ": "JWS"}).encode())
    now = int(time.time())
    p = b64(json.dumps({"iss": "decibell_central_auth", "sub": username, "uid": uid if uid is not None else uid_for(username),
                        "iat": now if iat is None else iat, "exp": now + 3600, "n": nonce}).encode())
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


_next_src = [0]


def next_source():
    """A fresh loopback source address per connection: the server caps
    connections per address (and new connections per second), which one
    test run from 127.0.0.1 would otherwise run into. 127.0.250.x is left
    for the tests of those caps."""
    _next_src[0] += 1
    n = _next_src[0]
    return f"127.0.{1 + (n // 250) % 190}.{1 + n % 250}"


def connect(port, timeout=3.0, src=None):
    return socket.create_connection((HOST, port), timeout=timeout, source_address=(src or next_source(), 0))


class Client:
    max_frame = 0   # largest frame received (the client drops any over 2 MiB)

    def __init__(self, username, invite="", nonce="", timeout=3.0, uid=None, hidden=False, iat=None, src=None):
        ctx = ssl.create_default_context()
        ctx.check_hostname = False
        ctx.verify_mode = ssl.CERT_NONE
        raw = connect(8082, timeout, src)
        self.s = ctx.wrap_socket(raw)
        self.username = username
        self.jwt = jwt(username, nonce, uid, iat)
        self.buf = b""
        self.inbox = []
        self.closed = False
        self.send(pb.Packet.COMMUNITY_AUTH_REQ, community_auth_req=pb.CommunityAuthRequest(
            jwt_token=self.jwt, invite_code=invite, presence_hidden=hidden))

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
            self.max_frame = max(self.max_frame, n)
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


def start_server(fresh=False, extra_env=None, binary=None):
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
    proc = subprocess.Popen([binary or SERVER_BIN], cwd=RUN, env=env, stdout=log, stderr=subprocess.STDOUT)
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


def uid_of(username):
    """The account a request targets (users by uid: requests never name
    their target): the member row's uid, else the uid its token carries."""
    rows = sql("select uid from members where username=?", username)
    return rows[0][0] if rows else uid_for(username)


def auth_ok(c):
    r = c.wait(pb.Packet.COMMUNITY_AUTH_RES)
    return r is not None and r.community_auth_res.success, r


def make_invite(owner):
    owner.send(pb.Packet.INVITE_CREATE_REQ, invite_create_req=pb.InviteCreateRequest(expires_at=0, max_uses=0))
    r = owner.wait(pb.Packet.INVITE_CREATE_RES)
    assert r and r.invite_create_res.success, r
    return r.invite_create_res.invite.code


def join(username, owner, nonce="", hidden=False):
    code = make_invite(owner)
    c = Client(username, invite=code, nonce=nonce, hidden=hidden)
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
    raw = connect(8085)
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
    bob.send(pb.Packet.SET_NICKNAME_REQ, set_nickname_req=pb.SetNicknameRequest(username="bob", uid=uid_of("bob"), nickname=nick))
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
    owner.send(pb.Packet.BAN_MEMBER_REQ, ban_member_req=pb.BanMemberRequest(username="bob2", uid=uid_of("bob2"), reason="bye"))
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
    owner.send(pb.Packet.MEMBER_ROLES_UPDATE_REQ, member_roles_update_req=pb.MemberRolesUpdateRequest(username="erin", uid=uid_of("erin"), role_ids=[mod.id]))
    r = owner.wait(pb.Packet.ROLE_ACTION_RES, pred=lambda p: p.role_action_res.action == "assign")
    check("owner assigned Mod to erin", r is not None and r.role_action_res.success)
    owner.flush(0.5)
    owner.send(pb.Packet.ROLE_LIST_REQ, role_list_req=pb.RoleListRequest())
    rl = owner.wait(pb.Packet.ROLE_LIST_RES)
    pos = {x.name: x.position for x in rl.role_list_res.roles}
    check("hierarchy: Mod above Admin", pos.get("Mod", 0) > pos.get("Admin", 0), str(pos))
    erin.flush(0.5)
    erin.send(pb.Packet.MEMBER_ROLES_UPDATE_REQ, member_roles_update_req=pb.MemberRolesUpdateRequest(username="erin", uid=uid_of("erin"), role_ids=[mod.id, admin.id]))
    r = erin.wait(pb.Packet.ROLE_ACTION_RES, pred=lambda p: p.role_action_res.action == "assign")
    check("erin self-assigning Admin rejected", r is not None and not r.role_action_res.success, r.role_action_res.message if r else None)
    # Sanity: erin can still assign a harmless lower role she holds bits for.
    owner.send(pb.Packet.ROLE_CREATE_REQ, role_create_req=pb.RoleCreateRequest(name="Helper", color=0, permissions=0))
    helper = owner.wait(pb.Packet.ROLE_ACTION_RES, pred=lambda p: p.role_action_res.action == "create").role_action_res.role
    erin.flush(0.5)
    erin.send(pb.Packet.MEMBER_ROLES_UPDATE_REQ, member_roles_update_req=pb.MemberRolesUpdateRequest(username="erin", uid=uid_of("erin"), role_ids=[mod.id, helper.id]))
    r = erin.wait(pb.Packet.ROLE_ACTION_RES, pred=lambda p: p.role_action_res.action == "assign")
    check("erin assigning Helper (no extra bits) allowed", r is not None and r.role_action_res.success, r.role_action_res.message if r else None)
    # Removing a role above... erin removes Helper again (fine).
    erin.send(pb.Packet.MEMBER_ROLES_UPDATE_REQ, member_roles_update_req=pb.MemberRolesUpdateRequest(username="erin", uid=uid_of("erin"), role_ids=[mod.id]))
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
    owner.send(pb.Packet.KICK_MEMBER_REQ, kick_member_req=pb.KickMemberRequest(username="heidi", uid=uid_of("heidi"), reason=""))
    r = owner.wait(pb.Packet.MOD_ACTION_RES, pred=lambda p: p.mod_action_res.action == "kick")
    check("offline kick succeeded", r is not None and r.mod_action_res.success)
    rm = owner.wait(pb.Packet.MEMBER_REMOVE, timeout=2, pred=lambda p: p.member_remove.username == "heidi")
    check("MEMBER_REMOVE after offline kick", rm is not None)
    ivan = join("ivan", owner); ivan.close(); time.sleep(0.5); owner.flush(0.8)
    owner.send(pb.Packet.BAN_MEMBER_REQ, ban_member_req=pb.BanMemberRequest(username="ivan", uid=uid_of("ivan"), reason=""))
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
    owner.send(pb.Packet.BAN_MEMBER_REQ, ban_member_req=pb.BanMemberRequest(username="kim", uid=uid_of("kim"), reason=""))
    vp = owner.wait(pb.Packet.VOICE_PRESENCE_UPDATE, timeout=3, pred=lambda p: p.voice_presence_update.channel_id == "voice-lounge" and len(p.voice_presence_update.active_users) == 0)
    check("voice presence cleared after ban (session reaped)", vp is not None)
    check("kim socket closed", kim.is_closed())
    kim.close(); owner.close()


def raw_tls(port=8082, src=None):
    ctx = ssl.create_default_context(); ctx.check_hostname = False; ctx.verify_mode = ssl.CERT_NONE
    return ctx.wrap_socket(connect(port, 3, src))


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
        lou.send(pb.Packet.CHANNEL_MSG, channel_msg=pb.ChannelMessage(channel_id="general", content=f"spam {i}", nonce=f"n{i}"))
    owner.drain(2.0); lou.drain(1.0)
    delivered_nonces = [p.channel_msg.nonce for p in owner.inbox if p.type == pb.Packet.CHANNEL_MSG]
    delivered = len(delivered_nonces)
    rejected = sum(1 for p in lou.inbox if p.type == pb.Packet.MOD_ACTION_RES and p.mod_action_res.action == "message"
                   and not p.mod_action_res.success)
    rejections = [p.channel_msg_rejected for p in lou.inbox if p.type == pb.Packet.CHANNEL_MSG_REJECTED]
    check("burst capped (10 burst + ~3/s)", 10 <= delivered <= 16, f"delivered={delivered}")
    check("sender notified for each dropped message", rejected == 30 - delivered, f"rejected={rejected} delivered={delivered}")
    # The typed rejection names the dropped request's nonce — the client
    # withdraws exactly that optimistic bubble (no more ghost rows).
    expected_dropped = sorted(set(f"n{i}" for i in range(30)) - set(delivered_nonces))
    check("CHANNEL_MSG_REJECTED names each dropped nonce",
          sorted(r.nonce for r in rejections) == expected_dropped,
          f"rejected_nonces={sorted(r.nonce for r in rejections)[:5]}... expected={expected_dropped[:5]}...")
    check("rejection carries channel + reason",
          all(r.channel_id == "general" and "too fast" in r.reason for r in rejections))
    check("a rate-limit refusal carries no slowmode wait",
          bool(rejections) and all(r.retry_after_seconds == 0 for r in rejections))
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
    oli.send(pb.Packet.STOP_WATCHING_REQ, stop_watching_req=pb.StopWatchingRequest(channel_id="voice-lounge", target_username="ned", target_uid=uid_of("ned")))
    n = ned.wait(pb.Packet.STREAM_WATCHER_NOTIFY, timeout=1.5)
    check("no LEFT notify for a never-subscribed watcher", n is None)
    oli.send(pb.Packet.WATCH_STREAM_REQ, watch_stream_req=pb.WatchStreamRequest(channel_id="voice-lounge", target_username="ned", target_uid=uid_of("ned")))
    n = ned.wait(pb.Packet.STREAM_WATCHER_NOTIFY, timeout=2, pred=lambda p: p.stream_watcher_notify.action == pb.StreamWatcherNotify.JOINED)
    check("real watch → JOINED notify", n is not None)
    oli.send(pb.Packet.STOP_WATCHING_REQ, stop_watching_req=pb.StopWatchingRequest(channel_id="voice-lounge", target_username="ned", target_uid=uid_of("ned")))
    n = ned.wait(pb.Packet.STREAM_WATCHER_NOTIFY, timeout=2, pred=lambda p: p.stream_watcher_notify.action == pb.StreamWatcherNotify.LEFT)
    check("real unwatch → LEFT notify", n is not None)
    ned.close(); oli.close(); owner.close()


def test_stream_thumbnails():
    print("[thumbs] 256 KB cap, 3 burst / 1 per 5 s, last one cached for fetch")
    owner = Client("alice"); assert auth_ok(owner)[0]
    sam = join("sam", owner); tia = join("tia", owner)
    for c in (sam, tia):
        c.send(pb.Packet.JOIN_VOICE_REQ, join_voice_req=pb.JoinVoiceRequest(channel_id="voice-lounge"))
    sam.send(pb.Packet.START_STREAM_REQ, start_stream_req=pb.StartStreamRequest(channel_id="voice-lounge", target_fps=30))
    time.sleep(0.5); sam.flush(0.5); tia.flush(0.5); owner.flush(0.5)

    def push(size, tag):
        sam.send(pb.Packet.STREAM_THUMBNAIL_UPDATE, stream_thumbnail_update=pb.StreamThumbnailUpdate(
            channel_id="voice-lounge", thumbnail_data=bytes([tag]) * size))

    def relayed(timeout):
        r = tia.wait(pb.Packet.STREAM_THUMBNAIL_UPDATE, timeout=timeout)
        return r.stream_thumbnail_update if r is not None else None

    push(200 * 1024, 1)   # over the old 128 KB cap
    t = relayed(3)
    check("200 KB thumbnail relayed, owner stamped", t is not None and len(t.thumbnail_data) == 200 * 1024
          and t.owner_username == "sam", None if t is None else len(t.thumbnail_data))
    push(256 * 1024 + 1, 2)
    check("256 KB + 1 dropped", relayed(1.0) is None)
    push(256 * 1024, 3)   # third token of the burst
    t = relayed(3)
    check("exactly 256 KB relayed", t is not None and len(t.thumbnail_data) == 256 * 1024)
    push(1024, 4)         # burst spent (the oversized one took a token too)
    check("4th in a burst refused", relayed(1.0) is None)

    tia.send(pb.Packet.FETCH_STREAM_THUMBNAIL_REQ,
             fetch_stream_thumbnail_req=pb.FetchStreamThumbnailReq(owner_uid=uid_of("sam")))
    r = tia.wait(pb.Packet.FETCH_STREAM_THUMBNAIL_RES, timeout=3)
    d = r.fetch_stream_thumbnail_res.thumbnail_data if r is not None else b""
    check("fetch serves the last accepted thumbnail", len(d) == 256 * 1024 and d[:1] == b"\x03", len(d))

    time.sleep(5.2)       # 1 token per 5 s
    push(1024, 5)
    t = relayed(3)
    check("bucket refills (1 per 5 s)", t is not None and t.thumbnail_data[:1] == b"\x05")
    sam.close(); tia.close(); owner.close()


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
    owner.send(pb.Packet.MEMBER_ROLES_UPDATE_REQ, member_roles_update_req=pb.MemberRolesUpdateRequest(username="pat", uid=uid_of("pat"), role_ids=[role.id]))
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
    print("[B12/B27] retention sweep: >999 attachments tombstoned once; CHANNEL_PRUNED batched (100,000 ids)")
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
    # Past one packet's 100,000 ids (review A3: 2,000 a packet was hundreds
    # of frames per viewer for a big first sweep).
    c.executemany("insert into messages(channel_id, sender, content, timestamp) values('archive','alice',?,?)",
                  ((f"old {i}", old) for i in range(100500)))
    c.execute("update channels set retention_days_image=1 where id='general'")
    c.execute("update channels set retention_days_text=1 where id='archive'")
    c.commit(); c.close()
    proc = start_server(extra_env={"DECIBELL_RETENTION_INTERVAL_SECONDS": "2"})
    try:
        owner = Client("alice"); assert auth_ok(owner)[0]
        owner.drain(8.0)   # ≥2 sweeps
        pruned = [p.channel_pruned for p in owner.inbox if p.type == pb.Packet.CHANNEL_PRUNED]
        gen = [cp for cp in pruned if cp.channel_id == "general"]
        ann = [cp for cp in pruned if cp.channel_id == "archive"]
        purged = sum(len(cp.purged_attachments) for cp in gen)
        check("1200 attachments tombstoned in exactly one sweep", purged == 1200 and len(gen) == 1, f"purged={purged} packets={len(gen)}")
        deleted = sum(len(cp.deleted_message_ids) for cp in ann)
        check("100,500 messages pruned in two packets of <= 100,000", deleted == 100500 and len(ann) == 2
              and all(len(cp.deleted_message_ids) <= 100000 for cp in ann), f"deleted={deleted} packets={len(ann)}")
        check("each CHANNEL_PRUNED fits the client's frame cap", owner.max_frame <= 2 * 1024 * 1024, owner.max_frame)
        check("blobs unlinked", not any(os.path.exists(p) for p in paths))
        rows = sql("select count(*) from attachments where channel_id='general' and purged_at=0 and created_at<?", old + 1)
        check("no un-tombstoned old rows left (no re-purge loop)", rows == [(0,)], str(rows))
        owner.close()
    finally:
        stop_server(proc)
    sql("update channels set retention_days_image=0, retention_days_text=0")


def test_upload_sweep_goes_by_idle_time():
    print("[transfers] abandoned-upload sweep: idle uploads go, slow/paused ones that still PATCH stay")
    proc = start_server(extra_env={"DECIBELL_RETENTION_INTERVAL_SECONDS": "2"})
    try:
        def init(name):
            head, body = attachment_http("alice", "POST", "/attachments/init", "Content-Type: application/json\r\n",
                                         json.dumps({"channelId": "general", "filename": name, "mime": "application/octet-stream", "size": 4}).encode())
            return json.loads(body.decode() or "{}").get("id") if head.startswith(b"HTTP/1.1 201") else None
        idle, active, stale = init("idle.bin"), init("active.bin"), init("stale.bin")
        check("three uploads started", None not in (idle, active, stale))
        head, _ = attachment_http("alice", "PATCH", f"/attachments/{active}", "Upload-Offset: 0\r\n", b"ab")
        check("PATCH accepted", head.startswith(b"HTTP/1.1 204"), head[:40])
        now = int(time.time())
        stamped = sql("select last_activity_at from attachments where id=?", active)
        check("PATCH stamps last_activity_at", stamped and abs(stamped[0][0] - now) <= 5, str(stamped))
        # All three began two hours ago; only `active` has progressed lately.
        old = now - 7200
        sql("update attachments set created_at=? where id in (?,?,?)", old, idle, active, stale)
        sql("update attachments set last_activity_at=? where id=?", old, stale)
        time.sleep(5)   # >= 2 sweeps
        left = {r[0] for r in sql("select id from attachments where id in (?,?,?)", idle, active, stale)}
        check("never-patched upload swept after an hour", idle not in left)
        check("upload idle for two hours swept", stale not in left)
        check("old but recently active upload kept", active in left)
        head, _ = attachment_http("alice", "PATCH", f"/attachments/{active}", "Upload-Offset: 2\r\n", b"cd")
        check("kept upload resumes", head.startswith(b"HTTP/1.1 204"), head[:40])
    finally:
        stop_server(proc)


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
    """A member target is given by name here and sent as its uid (the
    name stays in target_id for the pre-uid build in migration checks)."""
    target_uid = uid_of(target_id) if target_type == pb.ChannelOverwrite.MEMBER else 0
    c.send(pb.Packet.CHANNEL_OVERWRITE_SET_REQ, channel_overwrite_set_req=pb.ChannelOverwriteSetRequest(
        overwrite=pb.ChannelOverwrite(channel_id=channel_id, target_type=target_type, target_id=str(target_id),
                                      target_uid=target_uid, allow=allow, deny=deny)))
    return c.wait(pb.Packet.CHANNEL_ACTION_RES, pred=lambda p: p.channel_action_res.action == "overwrite")


def channel_list(c, timeout=3):
    u = c.wait(pb.Packet.CHANNEL_LIST_UPDATE, timeout=timeout)
    return {ch.id: ch for ch in u.channel_list_update.channels} if u else None


# The seeded `everyone` default (perms::kDefaultEveryone); tests restore to it.
V3_DEFAULT_BITS = (pb.PERM_CREATE_INVITE | pb.PERM_CHANGE_NICKNAME | pb.PERM_EMBED_LINKS | pb.PERM_SPEAK)
ALL_MEMBER_BITS = (pb.PERM_SEND_MESSAGES | pb.PERM_CONNECT_VOICE | pb.PERM_STREAM |
                   pb.PERM_VIEW_CHANNEL | pb.PERM_READ_HISTORY | pb.PERM_ATTACH_FILES | V3_DEFAULT_BITS)


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
    owner.send(pb.Packet.MEMBER_ROLES_UPDATE_REQ, member_roles_update_req=pb.MemberRolesUpdateRequest(username="sam", uid=uid_of("sam"), role_ids=[role.id]))
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
    tia.send(pb.Packet.KICK_MEMBER_REQ, kick_member_req=pb.KickMemberRequest(username="uma", uid=uid_of("uma"), reason=""))
    r = tia.wait(pb.Packet.MOD_ACTION_RES, pred=lambda p: p.mod_action_res.action == "kick")
    check("role-less member can't kick a role-less member (equal level)", r is not None and not r.mod_action_res.success)
    sam.flush(0.5)
    sam.send(pb.Packet.KICK_MEMBER_REQ, kick_member_req=pb.KickMemberRequest(username="uma", uid=uid_of("uma"), reason=""))
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


def _latin_fold_table():
    """kLatinFold from search.cpp (U+00C0..U+017F), the server's fold."""
    import re
    src = open(os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "search.cpp"), encoding="utf-8").read()
    body = src[src.index("kLatinFold[0x180 - 0xC0] = {"):]
    body = body[body.index("{") + 1:body.index("};")]
    return [int(v, 16) for line in body.splitlines() for v in re.findall(r"0x([0-9A-Fa-f]{4})", line.split("//")[0])]


_LATIN = None


def search_fold(s):
    """search.cpp search_fold, code point by code point."""
    global _LATIN
    if _LATIN is None:
        _LATIN = _latin_fold_table()
        assert len(_LATIN) == 0xC0, len(_LATIN)
    out = []
    for ch in s:
        c = ord(ch)
        if 0x300 <= c <= 0x36F:
            continue
        if c < 0x80:
            c = c + 32 if 0x41 <= c <= 0x5A else c
        elif 0xC0 <= c < 0x180:
            c = _LATIN[c - 0xC0]
        elif 0x391 <= c <= 0x3A9 and c != 0x3A2:
            c += 0x20
        elif 0x410 <= c <= 0x42F:
            c += 0x20
        elif 0x400 <= c <= 0x40F:
            c += 0x50
        out.append(chr(c))
    return "".join(out)


def roster_order_key(m):
    """messages.proto "Roster order": fold(display), display, uid."""
    name = m.nickname or m.username
    return (search_fold(name).encode(), name.encode(), m.uid)


def member_page(c, after="", limit=100):
    """One roster page; a refusal (the query bucket is shared with every
    list fetch) is retried."""
    first = after == ""
    for _ in range(20):
        c.send(pb.Packet.MEMBER_LIST_REQ, member_list_req=pb.MemberListRequest(after=after, limit=limit))
        pg = c.wait(pb.Packet.MEMBER_LIST_RES, timeout=5,
                    pred=lambda p: p.member_list_res.first_page == first and not p.member_list_res.lookup_uids)
        if pg is None or pg.member_list_res.success:
            return pg.member_list_res if pg else None
        time.sleep(0.6)
    return None


def roster_pages(c, limit=1000, max_pages=10000):
    """Every page of the roster sequence: (pages, members in order)."""
    pages, members, after = [], [], ""
    while len(pages) < max_pages:
        r = member_page(c, after, limit)
        if r is None:
            break
        pages.append(r)
        members.extend(r.members)
        if not r.has_more:
            break
        after = r.next_after
    return pages, members


def roster_all(c):
    return roster_pages(c)[1]


def test_roster_deltas():
    print("[roster] paged snapshot + MEMBER_UPSERT/REMOVE deltas + revision + counts")
    owner = Client("alice"); assert auth_ok(owner)[0]
    owner.flush(0.5)
    # Seed 250 offline members directly (joined long ago) to exercise paging.
    now = int(time.time())
    c = sqlite3.connect(DB)
    for i in range(250):
        c.execute("insert or ignore into members(uid, username, joined_at, nickname) values(?, ?, ?, '')",
                  (uid_for(f"zz{i:03d}"), f"zz{i:03d}", now))
    c.commit(); c.close()
    total = sql("select count(*) from members")[0][0]
    pages, seq = roster_pages(owner, limit=100)
    r1 = pages[0]
    online = [m for m in seq if m.is_online]
    check("first page: first_page, 100 members, more to come", r1.first_page and len(r1.members) == 100 and r1.has_more,
          f"first={r1.first_page} n={len(r1.members)} more={r1.has_more}")
    check("total_members = the members table", r1.total_members == total, f"{r1.total_members} vs {total}")
    check("online_count = the online members listed", all(p.online_count == len(online) for p in pages) and "alice" in [m.username for m in online],
          f"{[p.online_count for p in pages]} vs {len(online)}")
    check("later pages: not first_page", all(not p.first_page for p in pages[1:]) and len(pages) >= 3)
    flips = [i for i in range(1, len(seq)) if seq[i - 1].is_online != seq[i].is_online]
    check("sequence: every online member, then every offline member", len(flips) <= 1 and seq[0].is_online, f"flips at {flips}")
    check("each part in roster order",
          [roster_order_key(m) for m in online] == sorted(roster_order_key(m) for m in online)
          and [roster_order_key(m) for m in seq if not m.is_online] == sorted(roster_order_key(m) for m in seq if not m.is_online))
    uids = [m.uid for m in seq]
    check("paging covers every member exactly once", len(uids) == len(set(uids)) == total, f"{len(uids)} / {len(set(uids))} / {total}")
    rev = r1.revision
    # presence flip → upsert with is_online and the counts after it
    vic = join("vic", owner)
    up = owner.wait(pb.Packet.MEMBER_UPSERT, timeout=2, pred=lambda p: p.member_upsert.member.username == "vic")
    check("join → MEMBER_UPSERT online", up is not None and up.member_upsert.member.is_online)
    check("join → counts: one more member, one more online", up is not None and up.member_upsert.total_members == total + 1
          and up.member_upsert.online_count == len(online) + 1, f"{up.member_upsert.total_members if up else None}/{up.member_upsert.online_count if up else None}")
    check("revision increased", up is not None and up.member_upsert.revision > rev)
    rev = up.member_upsert.revision
    vic.close()
    up = owner.wait(pb.Packet.MEMBER_UPSERT, timeout=3, pred=lambda p: p.member_upsert.member.username == "vic" and not p.member_upsert.member.is_online)
    check("disconnect → MEMBER_UPSERT offline", up is not None)
    check("disconnect → online count back down", up is not None and up.member_upsert.online_count == len(online)
          and up.member_upsert.total_members == total + 1)
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
    # A kicked member leaves the counts at once, though their session is
    # still closing; their leave() then says nothing more.
    xen = join("xen", owner)
    up = owner.wait(pb.Packet.MEMBER_UPSERT, timeout=2, pred=lambda p: p.member_upsert.member.username == "xen")
    before = (up.member_upsert.online_count, up.member_upsert.total_members) if up else (0, 0)
    owner.send(pb.Packet.KICK_MEMBER_REQ, kick_member_req=pb.KickMemberRequest(username="xen", uid=uid_of("xen")))
    rm = owner.wait(pb.Packet.MEMBER_REMOVE, timeout=2, pred=lambda p: p.member_remove.username == "xen")
    check("kick → MEMBER_REMOVE with the counts after it", rm is not None
          and (rm.member_remove.online_count, rm.member_remove.total_members) == (before[0] - 1, before[1] - 1),
          f"{(rm.member_remove.online_count, rm.member_remove.total_members) if rm else None} vs {before}")
    owner.drain(1.0)
    check("kick → no upsert when the kicked session closes",
          not any(p.type == pb.Packet.MEMBER_UPSERT and p.member_upsert.member.username == "xen" for p in owner.inbox))
    xen.close()
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


def test_roster_online_paging():
    print("[roster] online members page too: order, cursor across online → offline, counts, deltas, lookups")
    owner = Client("alice"); assert auth_ok(owner)[0]
    # Names that only sort right folded (case, accents, Turkish ı / İ) and
    # one nickname that sorts its member first. All join online.
    names = ["opÇağrı", "opcem", "opÖmer", "opoya", "opZed", "opábel", "opAbby", "opıpek", "opİlker", "opilknur",
             "opbo", "opBo", "opdan", "opemre", "opfaye", "opgus", "ophal", "opivy", "opjo", "opkai"]
    now = int(time.time())
    c = sqlite3.connect(DB)
    c.executemany("insert or ignore into members(uid, username, joined_at, nickname) values(?,?,?,'')",
                  ((uid_for(n), n, now) for n in names))
    # Offline neighbours on both sides of the online names.
    c.executemany("insert or ignore into members(uid, username, joined_at, nickname) values(?,?,?,'')",
                  ((uid_for(f"opoff{i:02d}"), f"opoff{i:02d}", now) for i in range(12)))
    c.commit(); c.close()
    clients = []
    for n in names:
        cl = Client(n); assert auth_ok(cl)[0], n
        clients.append(cl)
    owner.flush(1.0)
    total = sql("select count(*) from members")[0][0]
    # A page size that leaves the online part ending mid-page, after at
    # least two pages of online members only.
    n_online = member_page(owner, limit=1).online_count
    limit = next(k for k in (7, 8, 9, 6, 5) if n_online % k and n_online > 2 * k)
    pages, seq = roster_pages(owner, limit=limit)
    online = [m for m in seq if m.is_online]
    offline = [m for m in seq if not m.is_online]
    check("online paging: every member exactly once", len({m.uid for m in seq}) == len(seq) == total, f"{len(seq)} vs {total}")
    check("online paging: online part in roster order", [roster_order_key(m) for m in online] == sorted(roster_order_key(m) for m in online))
    check("online paging: offline part in roster order", [roster_order_key(m) for m in offline] == sorted(roster_order_key(m) for m in offline))
    check("online paging: online part first", seq[:len(online)] == online)
    check("online paging: the first page holds only online members and says more follow",
          pages[0].first_page and len(pages[0].members) == limit and all(m.is_online for m in pages[0].members)
          and not pages[0].online_complete and pages[0].has_more)
    done = next((i for i, p in enumerate(pages) if p.online_complete), None)
    check("online paging: online_complete from the page that ends the online part on",
          done is not None and all(p.online_complete for p in pages[done:]) and not any(p.online_complete for p in pages[:done])
          and all(all(m.is_online for m in p.members) for p in pages[:done]))
    crossing = pages[done] if done is not None else None
    check("online paging: one page crosses from online into offline",
          crossing is not None and any(m.is_online for m in crossing.members) and any(not m.is_online for m in crossing.members),
          str([m.is_online for m in crossing.members]) if crossing else "")
    check("online paging: every page carries the counts",
          all(p.online_count == len(online) and p.total_members == total for p in pages))
    folded = [m.username for m in online if m.username.startswith("op")]
    check("online paging: folded order (Abby, ábel, Bo, bo, Çağrı, cem … İlker, ilknur, ıpek …)",
          folded[:6] == ["opAbby", "opábel", "opBo", "opbo", "opÇağrı", "opcem"]
          and folded.index("opİlker") < folded.index("opilknur") < folded.index("opıpek") < folded.index("opivy"), str(folded))
    # The requester's own row rides the first page when the page doesn't
    # reach it.
    late = clients[names.index("opZed")]
    late.flush(0.3)
    late.send(pb.Packet.MEMBER_LIST_REQ, member_list_req=pb.MemberListRequest(limit=2))
    p1 = late.wait(pb.Packet.MEMBER_LIST_RES, pred=lambda p: p.member_list_res.first_page)
    check("first page: the requester's own row in `extra`", p1 is not None and len(p1.member_list_res.extra) == 1
          and p1.member_list_res.extra[0].uid == uid_of("opZed") and p1.member_list_res.extra[0].is_online
          and uid_of("opZed") not in [m.uid for m in p1.member_list_res.members])
    early = clients[names.index("opAbby")]
    early.flush(0.3)
    early.send(pb.Packet.MEMBER_LIST_REQ, member_list_req=pb.MemberListRequest(limit=50))
    p1 = early.wait(pb.Packet.MEMBER_LIST_RES, pred=lambda p: p.member_list_res.first_page)
    check("first page: no `extra` when the page holds the requester", p1 is not None and len(p1.member_list_res.extra) == 0)
    # Deltas while partly loaded: the owner holds page 1 (limit 3); a member
    # who sorts past it goes offline, another comes online, a nickname moves
    # one to the front. The pages that follow from page 1's cursor stay
    # consistent with what the deltas said.
    owner.flush(0.3)
    p1 = member_page(owner, limit=3)
    rev = p1.revision
    gone = clients.pop(names.index("opkai")); names.remove("opkai")
    gone.close()
    up = owner.wait(pb.Packet.MEMBER_UPSERT, timeout=3, pred=lambda p: p.member_upsert.member.username == "opkai")
    check("partial: an unloaded member going offline → upsert with the online count down",
          up is not None and not up.member_upsert.member.is_online and up.member_upsert.online_count == p1.online_count - 1
          and up.member_upsert.revision == rev + 1)
    back = Client("opoff11"); assert auth_ok(back)[0]
    up = owner.wait(pb.Packet.MEMBER_UPSERT, timeout=3, pred=lambda p: p.member_upsert.member.username == "opoff11")
    check("partial: an unloaded member coming online → upsert with the count back up",
          up is not None and up.member_upsert.member.is_online and up.member_upsert.online_count == p1.online_count
          and up.member_upsert.revision == rev + 2)
    r = set_nick(owner, "opjo", "Aaaa first")
    up = owner.wait(pb.Packet.MEMBER_UPSERT, timeout=3, pred=lambda p: p.member_upsert.member.username == "opjo")
    check("partial: a nickname change → upsert, counts unchanged", mod_ok(r) and up is not None
          and up.member_upsert.member.nickname == "Aaaa first" and up.member_upsert.online_count == p1.online_count)
    rest, after = [], p1.next_after
    while after:
        pg = member_page(owner, after, limit=10)
        rest.extend(pg.members)
        after = pg.next_after if pg.has_more else ""
    later_online = [m.username for m in rest if m.is_online]
    later_offline = [m.username for m in rest if not m.is_online]
    check("partial: the member who went offline is in the offline part", "opkai" in later_offline and "opkai" not in later_online)
    check("partial: the member who came online is in the online part, once",
          later_online.count("opoff11") == 1 and "opoff11" not in later_offline)
    check("partial: a member renamed before the loaded edge isn't sent again",
          "opjo" not in later_online + later_offline and roster_order_key(next(m for m in seq if m.username == "opjo")) > roster_order_key(p1.members[-1]))
    loaded = {m.uid for m in p1.members}
    check("partial: page 1 + the rest = every member but the one moved behind the edge, once each",
          len(loaded | {m.uid for m in rest}) == len(p1.members) + len(rest) == total - 1)
    fresh = [m.username for m in roster_all(owner) if m.is_online]
    check("after the rename: the nickname sorts its member before alice and every op member",
          "opjo" in fresh and fresh.index("opjo") < fresh.index("alice")
          and all(fresh.index("opjo") <= i for i, n in enumerate(fresh) if n.startswith("op")), str(fresh[:5]))
    # Lookups: members wherever they sort; a uid that isn't a member is
    # echoed but not returned.
    owner.flush(0.3)
    want = [uid_of("opoff05"), uid_of("opZed"), 987654321]
    owner.send(pb.Packet.MEMBER_LIST_REQ, member_list_req=pb.MemberListRequest(uids=want))
    lk = owner.wait(pb.Packet.MEMBER_LIST_RES, pred=lambda p: list(p.member_list_res.lookup_uids) == want)
    lr = lk.member_list_res if lk else None
    check("lookup: the members among the uids, the rest echoed only",
          lr is not None and lr.success and sorted(m.uid for m in lr.extra) == sorted(want[:2]) and not lr.members
          and not lr.first_page and not lr.has_more and lr.online_count > 0 and lr.total_members == total,
          str(lr)[:300] if lr else "")
    # Rate limited: still answered, so the client's "loading" never sticks.
    owner.flush(0.3)
    for i in range(30):
        owner.send(pb.Packet.MEMBER_LIST_REQ, member_list_req=pb.MemberListRequest(uids=[uid_of("opZed"), 5000 + i]))
    got = []
    for _ in range(30):
        p = owner.wait(pb.Packet.MEMBER_LIST_RES, timeout=2)
        if p is None:
            break
        got.append(p.member_list_res)
    refused = [r for r in got if not r.success]
    check("rate limited lookups: every request answered, the refused ones echo their uids",
          len(got) == 30 and refused and all(len(r.lookup_uids) == 2 for r in refused), f"{len(got)} answers, {len(refused)} refused")
    for cl in clients + [back]:
        cl.close()
    sql("delete from members where username like 'op%'")
    owner.close()


def test_roster_page_budget():
    print("[roster] a page stops at the frame budget and at 1,000 members")
    owner = Client("alice"); assert auth_ok(owner)[0]
    now = int(time.time())
    c = sqlite3.connect(DB)
    # Nicknames far past the 32-byte cap (written behind the server's back):
    # 400 rows of ~4 KB would make a 1.6 MB page.
    c.executemany("insert into members(uid, username, joined_at, nickname) values(?,?,?,?)",
                  ((900000 + i, f"bg{i:04d}", now, f"bg{i:04d}" + "n" * 4000) for i in range(400)))
    c.executemany("insert into members(uid, username, joined_at, nickname) values(?,?,?,'')",
                  ((901000 + i, f"bh{i:04d}", now) for i in range(1100)))
    c.commit(); c.close()
    owner.flush(0.3)
    total = sql("select count(*) from members")[0][0]
    pages, seq = roster_pages(owner, limit=5000)
    check("budget: no page past 1,000 members", pages and all(len(p.members) <= 1000 for p in pages), str([len(p.members) for p in pages]))
    check("budget: a page of 4 KB names stops short of the frame cap", owner.max_frame < 1536 * 1024
          and any(len(p.members) < 1000 and p.has_more for p in pages), f"max frame {owner.max_frame}, {[len(p.members) for p in pages]}")
    check("budget: the pages still cover every member once", len(seq) == len({m.uid for m in seq}) == total, f"{len(seq)} vs {total}")
    sql("delete from members where uid between 900000 and 902099")
    owner.close()


def test_presence_hidden():
    print("[presence] invisible members stay offline in rosters (presence_hidden / PRESENCE_VISIBILITY)")
    owner = Client("alice"); assert auth_ok(owner)[0]
    owner.flush(0.5)
    is_ivy = lambda online: (lambda p: p.member_upsert.member.username == "ivy"
                             and p.member_upsert.member.is_online == online)
    any_ivy = lambda p: p.member_upsert.member.username == "ivy"
    # A brand-new member joining invisible still reaches every roster — offline.
    ivy = join("ivy", owner, hidden=True)
    up = owner.wait(pb.Packet.MEMBER_UPSERT, timeout=2, pred=any_ivy)
    check("new member joining invisible → upsert, offline", up is not None and not up.member_upsert.member.is_online)
    entry = [m for m in roster_all(owner) if m.username == "ivy"]
    check("the roster lists the invisible member offline", len(entry) == 1 and not entry[0].is_online)
    # Disconnecting and reconnecting invisible: no roster traffic at all —
    # an upsert at either moment would give the connection away.
    ivy.close()
    check("invisible disconnect → no upsert", owner.wait(pb.Packet.MEMBER_UPSERT, timeout=1.5, pred=any_ivy) is None)
    ivy = Client("ivy", hidden=True); assert auth_ok(ivy)[0]
    check("invisible reconnect → no upsert", owner.wait(pb.Packet.MEMBER_UPSERT, timeout=1.5, pred=any_ivy) is None)
    # Toggling flips the roster both ways; a repeat is a no-op.
    ivy.send(pb.Packet.PRESENCE_VISIBILITY, presence_visibility=pb.PresenceVisibility(hidden=False))
    check("PRESENCE_VISIBILITY hidden=false → upsert online", owner.wait(pb.Packet.MEMBER_UPSERT, timeout=2, pred=is_ivy(True)) is not None)
    ivy.send(pb.Packet.PRESENCE_VISIBILITY, presence_visibility=pb.PresenceVisibility(hidden=False))
    check("repeat hidden=false → no upsert", owner.wait(pb.Packet.MEMBER_UPSERT, timeout=1.0, pred=any_ivy) is None)
    ivy.send(pb.Packet.PRESENCE_VISIBILITY, presence_visibility=pb.PresenceVisibility(hidden=True))
    check("PRESENCE_VISIBILITY hidden=true → upsert offline", owner.wait(pb.Packet.MEMBER_UPSERT, timeout=2, pred=is_ivy(False)) is not None)
    # A second, visible session shows them online; the hidden one leaving
    # changes nothing; the visible one leaving flips them offline.
    ivy2 = Client("ivy", nonce="b"); assert auth_ok(ivy2)[0]
    check("visible second session → upsert online", owner.wait(pb.Packet.MEMBER_UPSERT, timeout=2, pred=is_ivy(True)) is not None)
    ivy.close()
    check("hidden session leaving while a visible one stays → no upsert", owner.wait(pb.Packet.MEMBER_UPSERT, timeout=1.5, pred=any_ivy) is None)
    ivy2.close()
    check("last visible session leaving → upsert offline", owner.wait(pb.Packet.MEMBER_UPSERT, timeout=3, pred=is_ivy(False)) is not None)
    owner.close()


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
    yul.send(pb.Packet.TRANSFER_OWNERSHIP_REQ, transfer_ownership_req=pb.TransferOwnershipRequest(new_owner="yul", new_owner_uid=uid_of("yul")))
    r = yul.wait(pb.Packet.MOD_ACTION_RES, pred=lambda p: p.mod_action_res.action == "transfer")
    check("transfer denied for non-owner", r is not None and not r.mod_action_res.success)
    owner.flush(0.3)
    owner.send(pb.Packet.TRANSFER_OWNERSHIP_REQ, transfer_ownership_req=pb.TransferOwnershipRequest(new_owner="yul", new_owner_uid=uid_of("yul")))
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
    yul.send(pb.Packet.TRANSFER_OWNERSHIP_REQ, transfer_ownership_req=pb.TransferOwnershipRequest(new_owner="alice", new_owner_uid=uid_of("alice")))
    r = yul.wait(pb.Packet.MOD_ACTION_RES, pred=lambda p: p.mod_action_res.action == "transfer")
    check("transfer back", r is not None and r.mod_action_res.success)
    yul.close(); owner.close()


def test_member_search_and_role_counts():
    print("[search] MEMBER_SEARCH by name / role across the whole membership; role member_count")
    owner = Client("alice"); assert auth_ok(owner)[0]
    owner.flush(0.5)
    now = int(time.time())
    for i in range(150):
        sql("insert or ignore into members(uid, username, joined_at, nickname) values(?, ?, ?, '')",
            uid_for(f"zq{i:03d}"), f"zq{i:03d}", now)
    sql("insert or ignore into members(uid, username, joined_at, nickname) values(?, 'quill', ?, 'Zqueen')",
        uid_for("quill"), now)
    owner.send(pb.Packet.ROLE_CREATE_REQ, role_create_req=pb.RoleCreateRequest(name="Searchers", color=0, permissions=0))
    role = owner.wait(pb.Packet.ROLE_ACTION_RES, pred=lambda p: p.role_action_res.action == "create").role_action_res.role
    for u in ("zq001", "zq002", "quill"):
        sql("insert into member_roles(uid, role_id) values(?, ?)", uid_for(u), role.id)

    def search(**kw):
        owner.send(pb.Packet.MEMBER_SEARCH_REQ, member_search_req=pb.MemberSearchRequest(**kw))
        r = owner.wait(pb.Packet.MEMBER_SEARCH_RES, pred=lambda p: p.member_search_res.seq == kw.get("seq", 0))
        return r.member_search_res if r else None

    r = search(query="ZQ", limit=100, seq=7)
    names = [m.username for m in r.members] if r else []
    check("search: case-insensitive on username + nickname, 151 matches", r is not None and r.success and r.total_matches == 151, str(r.total_matches if r else None))
    check("search: first page = 100 by username, has_more, seq echoed", len(names) == 100 and names == sorted(names) and r.has_more and r.seq == 7 and r.first_page)
    check("search: offline members included", r is not None and all(not m.is_online for m in r.members))
    r2 = search(query="zq", after=r.next_after, limit=100, seq=8)
    names2 = [m.username for m in r2.members] if r2 else []
    check("search: page 2 has the rest, no overlap", len(names2) == 51 and not set(names) & set(names2) and not r2.has_more and not r2.first_page, f"{len(names2)}")
    check("search: nickname match ('quill' / Zqueen)", "quill" in names + names2)
    r = search(role_id=role.id, seq=9)
    check("search: role filter", r is not None and sorted(m.username for m in r.members) == ["quill", "zq001", "zq002"], str([m.username for m in r.members] if r else None))
    check("search: role filter carries role_ids", r is not None and all(role.id in m.role_ids for m in r.members))
    r = search(query="zq00", role_id=-1, seq=10)
    check("search: role_id -1 = no roles", r is not None and sorted(m.username for m in r.members) == [f"zq00{i}" for i in (0, 3, 4, 5, 6, 7, 8, 9)], str([m.username for m in r.members] if r else None))
    r = search(query="100%", seq=11)
    check("search: no match", r is not None and r.success and r.total_matches == 0 and not r.members)

    owner.flush(0.3)
    owner.send(pb.Packet.ROLE_LIST_REQ, role_list_req=pb.RoleListRequest())
    rl = owner.wait(pb.Packet.ROLE_LIST_RES)
    counts = {x.name: x.member_count for x in rl.role_list_res.roles} if rl else {}
    everyone = next((x.member_count for x in rl.role_list_res.roles if x.is_default), None) if rl else None
    total = sql("select count(*) from members")[0][0]
    check("role list: member_count per role", counts.get("Searchers") == 3, str(counts))
    check("role list: everyone counts every member", everyone == total, f"{everyone} vs {total}")
    owner.flush(0.3)
    owner.send(pb.Packet.MEMBER_ROLES_UPDATE_REQ, member_roles_update_req=pb.MemberRolesUpdateRequest(username="zq010", uid=uid_of("zq010"), role_ids=[role.id]))
    pushed = owner.wait(pb.Packet.ROLE_LIST_RES, timeout=2,
                        pred=lambda p: any(x.name == "Searchers" and x.member_count == 4 for x in p.role_list_res.roles))
    check("assigning a role pushes the new count", pushed is not None)
    owner.send(pb.Packet.ROLE_DELETE_REQ, role_delete_req=pb.RoleDeleteRequest(role_id=role.id))
    owner.wait(pb.Packet.ROLE_ACTION_RES, pred=lambda p: p.role_action_res.action == "delete")
    sql("delete from members where username like 'zq%' or username = 'quill'")
    sql("delete from member_roles where uid in (select uid from members where username like 'zq%' or username = 'quill')")
    owner.close()


def test_audit_search():
    print("[audit search] text query over names / nicknames / channels / roles + action filter")
    owner = Client("alice"); assert auth_ok(owner)[0]
    owner.flush(0.5)
    now = int(time.time())
    owner.send(pb.Packet.ROLE_CREATE_REQ, role_create_req=pb.RoleCreateRequest(name="Lanterns", color=0, permissions=0))
    lantern = owner.wait(pb.Packet.ROLE_ACTION_RES, pred=lambda p: p.role_action_res.action == "create").role_action_res.role
    ch = sql("select id, name from channels where type = 0 order by position limit 1")[0]
    sql("insert or ignore into members(uid, username, joined_at, nickname) values(?, 'owl', ?, 'Night Owl')",
        uid_for("owl"), now)
    rows = [
        ("auditor1", "kick", "victim1", "", "reason: 100%_off spam"),
        ("auditor1", "overwrite_set", f"role:{lantern.id}", ch[0], "allow 8192 deny 0"),
        ("owl", "ban", "victim2", "", ""),
        ("auditor1", "channel_update", "", ch[0], "slowmode 5s"),
    ]
    for actor, action, target, cid, details in rows:
        sql("insert into audit_log(ts, actor_uid, actor, action, target, channel_id, details) values(?, ?, ?, ?, ?, ?, ?)",
            now, uid_for(actor), actor, action, target, cid, details)

    def search(seq, **kw):
        owner.send(pb.Packet.AUDIT_LOG_REQ, audit_log_req=pb.AuditLogRequest(limit=100, seq=seq, **kw))
        r = owner.wait(pb.Packet.AUDIT_LOG_RES, pred=lambda p: p.audit_log_res.seq == seq)
        return r.audit_log_res if r else None

    r = search(21, query="100%_")
    check("audit search: % and _ match literally", r is not None and [e.target for e in r.entries] == ["victim1"], str([e.target for e in r.entries] if r else None))
    check("audit entries carry the actor's uid", r is not None and [e.actor_uid for e in r.entries] == [uid_for("auditor1")])
    r = search(22, query="NIGHT OWL")
    check("audit search: actor nickname, case-insensitive", r is not None and [e.target for e in r.entries] == ["victim2"])
    r = search(23, query="lanterns")
    tgts = [e.action for e in r.entries] if r else []
    check("audit search: role name behind role:<id> + role_create", r is not None and "overwrite_set" in tgts and "role_create" in tgts, str(tgts))
    r = search(24, query=ch[1], actions=["channel_update"])
    check("audit search: channel name + action filter", r is not None and len(r.entries) >= 1 and all(e.action == "channel_update" for e in r.entries), str([e.action for e in r.entries] if r else None))
    r = search(25, query="victim", actions=["ban", "unban"])
    check("audit search: action filter narrows a query", r is not None and [e.target for e in r.entries] == ["victim2"])
    r = search(26, actions=["kick"])
    check("audit search: actions alone", r is not None and len(r.entries) >= 1 and all(e.action == "kick" for e in r.entries))
    owner.send(pb.Packet.AUDIT_LOG_REQ, audit_log_req=pb.AuditLogRequest(limit=1, seq=28, query="auditor1"))
    p1 = owner.wait(pb.Packet.AUDIT_LOG_RES, pred=lambda p: p.audit_log_res.seq == 28)
    e1 = p1.audit_log_res.entries[0] if p1 and p1.audit_log_res.entries else None
    owner.send(pb.Packet.AUDIT_LOG_REQ, audit_log_req=pb.AuditLogRequest(limit=10, seq=29, query="auditor1", before_id=e1.id if e1 else 0))
    p2 = owner.wait(pb.Packet.AUDIT_LOG_RES, pred=lambda p: p.audit_log_res.seq == 29)
    check("audit search: paging keeps the filter", p1 is not None and p1.audit_log_res.has_more and p2 is not None
          and len(p2.audit_log_res.entries) == 2 and all(e.actor == "auditor1" and e.id < e1.id for e in p2.audit_log_res.entries))
    owner.send(pb.Packet.ROLE_DELETE_REQ, role_delete_req=pb.RoleDeleteRequest(role_id=lantern.id))
    owner.wait(pb.Packet.ROLE_ACTION_RES, pred=lambda p: p.role_action_res.action == "delete")
    sql("delete from audit_log where actor in ('auditor1', 'owl')")
    sql("delete from members where uid = ?", uid_for("owl"))
    owner.close()


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
    amy.send(pb.Packet.TIMEOUT_MEMBER_REQ, timeout_member_req=pb.TimeoutMemberRequest(username="alice", uid=uid_of("alice"), until=int(time.time()) + 60))
    r = amy.wait(pb.Packet.MOD_ACTION_RES, pred=lambda p: p.mod_action_res.action == "timeout")
    check("denied without MODERATE_MEMBERS / on owner", r is not None and not r.mod_action_res.success)
    owner.send(pb.Packet.TIMEOUT_MEMBER_REQ, timeout_member_req=pb.TimeoutMemberRequest(username="amy", uid=uid_of("amy"), until=int(time.time()) + 60, reason="cool off"))
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
    owner.send(pb.Packet.TIMEOUT_MEMBER_REQ, timeout_member_req=pb.TimeoutMemberRequest(username="amy", uid=uid_of("amy"), until=0))
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
    owner.send(pb.Packet.BAN_MEMBER_REQ, ban_member_req=pb.BanMemberRequest(username="ben", uid=uid_of("ben"), reason="spam", expires_at=int(time.time()) + 6, delete_message_seconds=3600))
    r = owner.wait(pb.Packet.MOD_ACTION_RES, pred=lambda p: p.mod_action_res.action == "ban")
    check("temp ban with purge succeeded", r is not None and r.mod_action_res.success)
    rev = ben.wait(pb.Packet.MEMBERSHIP_REVOKED, timeout=2)
    check("target sees reason + expiry", rev is not None and rev.membership_revoked.reason == "spam" and rev.membership_revoked.expires_at > 0)
    owner.drain(1.5)
    pruned = [p.channel_pruned for p in owner.inbox if p.type == pb.Packet.CHANNEL_PRUNED and p.channel_pruned.channel_id == "general"]
    deleted = sorted(i for cp in pruned for i in cp.deleted_message_ids)
    check("one CHANNEL_PRUNED names every purged message", len(pruned) == 1 and deleted == sorted(ids), f"{deleted} vs {sorted(ids)}")
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


def search_req(c, seq, **kw):
    """MESSAGE_SEARCH round trip; waits out the per-session search bucket."""
    for _ in range(12):
        c.send(pb.Packet.MESSAGE_SEARCH_REQ, message_search_req=pb.MessageSearchRequest(seq=seq, **kw))
        r = c.wait(pb.Packet.MESSAGE_SEARCH_RES, timeout=5, pred=lambda p: p.message_search_res.seq == seq)
        if r is None:
            return None
        res = r.message_search_res
        if not res.success and res.retry_after_ms > 0:
            time.sleep(res.retry_after_ms / 1000.0)
            continue
        return res
    return None


def test_message_search():
    """Message search (spec 2026-10-14): words / prefixes / accents / phrases /
    exclusions / file names over the FTS index, sender by uid (rename-proof),
    channel scope + access, has-filters, dates, sorts + paging, encrypted
    channels (filters only), index upkeep on edit / delete / wipe, the
    rate-limit answer, and the uid fixes to edit / delete / ban purge."""
    print("[search] MESSAGE_SEARCH: FTS + filters + access + paging + upkeep")
    H = pb.MessageSearchRequest
    owner = Client("alice"); assert auth_ok(owner)[0]
    bob = join("sbob", owner); carol = join("scarol", owner)
    ev = everyone_role(owner)
    for c in (owner, bob, carol): c.flush(0.4)

    def create(name, encrypted=False):
        owner.send(pb.Packet.CHANNEL_CREATE_REQ, channel_create_req=pb.ChannelCreateRequest(
            name=name, type=pb.ChannelInfo.TEXT, encrypted=encrypted))
        r = owner.wait(pb.Packet.CHANNEL_ACTION_RES, pred=lambda p: p.channel_action_res.action == "create")
        return r.channel_action_res.channel.id
    ch_a, ch_b, sealed, private = create("srch-a"), create("srch-b"), create("srch-sealed", True), create("srch-private")
    r = set_overwrite(owner, private, pb.ChannelOverwrite.ROLE, ev.id, deny=pb.PERM_VIEW_CHANNEL)
    check("private search channel set up", r is not None and r.channel_action_res.success)
    for c in (owner, bob, carol): c.flush(0.4)

    # Timestamps after everything already stored, an hour apart (server
    # timestamps grow with ids; the date bounds rely on it).
    t0 = max(int(time.time()), sql("select coalesce(max(timestamp), 0) from messages")[0][0]) + 10
    H1 = 3600
    def put(channel, sender, content, hour, uid=None, envelope=None):
        return sql("insert into messages(channel_id, sender, content, timestamp, envelope, sender_uid) "
                   "values(?,?,?,?,?,?) returning id", channel, sender, content, t0 + hour * H1, envelope,
                   uid if uid is not None else uid_for(sender))[0][0]
    def attach(mid, channel, kind, filename, encrypted=0):
        aid = sql("insert into attachments(message_id, kind, filename, mime, size_bytes, position, created_at, "
                  "upload_status, uploader, channel_id, encrypted) values(0,?,?,'application/octet-stream',10,0,?,"
                  "'ready','sbob',?,?) returning id", kind, filename, t0, channel, encrypted)[0][0]
        sql("update attachments set message_id=? where id=?", mid, aid)   # bind → re-index with the name
        return aid
    m1 = put(ch_a, "sbob", "The deployment finished on staging", 0)
    m2 = put(ch_b, "scarol", "deploy again tomorrow", 1)
    m3 = put(ch_a, "sbob", "Café crème brûlée recipe", 2)
    m4 = put(ch_a, "alice", "deploy deploy deploy, see https://example.com/runbook", 3)
    m5 = put(ch_b, "sbob_old", "notes from before my rename: deploy checklist", 4, uid=uid_for("sbob"))
    m6 = put(ch_a, "ghost", "a departed member said deploy once", 5, uid=0)
    m7 = put(ch_b, "scarol", "here is the file", 6); attach(m7, ch_b, 2, "Q3-runbook.pdf")
    m8 = put(ch_a, "sbob", "screenshot", 7); attach(m8, ch_a, 0, "screen.png")
    m9 = put(sealed, "sbob", "PLACEHOLDER deploy", 8, envelope=b"\x02SEALED")
    attach(m9, sealed, 0, "encrypted", encrypted=1)
    m10 = put(private, "alice", "deploy secret plans", 9)
    # Plaintext left in an encrypted channel from before encryption became
    # fixed at creation: its text is not searched either.
    m11 = put(sealed, "sbob", "legacy plaintext deploy zanzibar", 9)
    deploy_visible = [m6, m5, m4, m2, m1]   # newest first, what bob can see

    seq = iter(range(100, 10000))
    def ids(res):
        return [m.id for m in res.messages] if res else None

    r = search_req(bob, next(seq), query="deploy")
    check("words: prefix match, newest first, bob's view", r is not None and r.success and ids(r) == deploy_visible, str(ids(r)) + (r.message if r else ""))
    check("words: first page counted", r is not None and r.first_page and r.total == 5 and not r.total_capped and not r.has_more, f"{r.total if r else None}")
    check("words: encrypted channel reported as skipped", r is not None and sealed in r.text_skipped_channel_ids, str(list(r.text_skipped_channel_ids)) if r else None)
    check("words: the sealed row's placeholder never matches", r is not None and m9 not in ids(r))
    r = search_req(owner, next(seq), query="zanzibar")
    check("words: an encrypted channel's leftover plaintext isn't indexed", r is not None and r.success and ids(r) == [], str(ids(r)))
    r = search_req(owner, next(seq), query="deploy")
    check("owner also sees the private channel's match", r is not None and m10 in ids(r))
    r = search_req(bob, next(seq), query="cafe")
    check("accents fold (cafe → Café)", r is not None and ids(r) == [m3], str(ids(r)))
    r = search_req(bob, next(seq), query='"deploy again"')
    check("exact phrase", r is not None and ids(r) == [m2], str(ids(r)))
    r = search_req(bob, next(seq), query="deploy -tomorrow -checklist")
    check("excluded words", r is not None and ids(r) == [m6, m4, m1], str(ids(r)))
    r = search_req(bob, next(seq), query="-deploy")
    check("excluded words alone are refused", r is not None and not r.success and "leave out" in r.message, r.message if r else None)
    r = search_req(bob, next(seq), query="!!! ???")
    check("punctuation only, no filters: refused", r is not None and not r.success, r.message if r else None)
    r = search_req(bob, next(seq), query="")
    check("an empty search is refused", r is not None and not r.success)
    r = search_req(bob, next(seq), query="runbook")
    check("file names match (bound attachment re-indexed)", r is not None and m7 in ids(r), str(ids(r)))
    r = search_req(bob, next(seq), query='deploy" OR "x NEAR(')
    check("FTS syntax in the input is inert", r is not None and r.success, r.message if r else None)

    # Senders by account: every name they posted under.
    mine = [ch_a, ch_b, sealed, private]
    r = search_req(carol, next(seq), author_uids=[uid_of("sbob")], channel_ids=mine)
    got = ids(r) or []
    check("from: member matches every row of their account, incl. pre-rename + encrypted", r is not None and set(got) == {m1, m3, m5, m8, m9, m11}, str(got))
    sealed_row = next((m for m in r.messages if m.id == m9), None) if r else None
    check("from: encrypted rows come with their envelope", sealed_row is not None and sealed_row.envelope == b"\x02SEALED")
    r = search_req(carol, next(seq), query="deploy", author_uids=[uid_of("sbob"), uid_of("scarol")])
    check("from: several people + words", r is not None and ids(r) == [m5, m2, m1], str(ids(r)))

    # Channel scope and access.
    r = search_req(bob, next(seq), query="deploy", channel_ids=[ch_b])
    check("in: one channel", r is not None and ids(r) == [m5, m2], str(ids(r)))
    r = search_req(bob, next(seq), query="deploy", channel_ids=[private])
    check("in: an unreadable channel is dropped silently", r is not None and r.success and ids(r) == [], str(ids(r)))
    r = search_req(bob, next(seq), channel_ids=[ch_a])
    check("in: channel alone lists it", r is not None and ids(r)[:3] == [m8, m6, m4], str(ids(r)))

    # Has-filters.
    r = search_req(bob, next(seq), has=H.HAS_IMAGE, channel_ids=mine)
    check("has: image (encrypted kinds count)", r is not None and set(ids(r)) == {m8, m9}, str(ids(r)))
    r = search_req(bob, next(seq), has=H.HAS_DOCUMENT, channel_ids=mine)
    check("has: document", r is not None and ids(r) == [m7], str(ids(r)))
    r = search_req(bob, next(seq), has=H.HAS_FILE, channel_ids=[ch_a, ch_b])
    check("has: any file", r is not None and ids(r) == [m8, m7], str(ids(r)))
    r = search_req(bob, next(seq), has=H.HAS_LINK, channel_ids=mine)
    check("has: link (plaintext only, sealed skipped)", r is not None and ids(r) == [m4] and list(r.text_skipped_channel_ids) == [sealed], str(ids(r)))
    r = search_req(bob, next(seq), has=H.HAS_IMAGE | H.HAS_DOCUMENT, channel_ids=mine)
    check("has: every bit must hold", r is not None and ids(r) == [], str(ids(r)))

    # Dates (after inclusive, before exclusive).
    r = search_req(bob, next(seq), query="deploy", after=t0 + 1 * H1, before=t0 + 4 * H1)
    check("dates: window", r is not None and ids(r) == [m4, m2], str(ids(r)))
    r = search_req(bob, next(seq), query="deploy", after=t0 + 100 * H1)
    check("dates: nothing that recent", r is not None and r.success and ids(r) == [] and r.total == 0)
    r = search_req(bob, next(seq), query="deploy", before=t0)
    check("dates: before everything", r is not None and r.success and ids(r) == [], str(ids(r)))
    r = search_req(bob, next(seq), after=t0 + 5, before=t0 + 2)
    check("dates: an empty range is refused", r is not None and not r.success)

    # Sorts + paging.
    r = search_req(bob, next(seq), query="deploy", sort=H.OLDEST)
    check("sort: oldest", r is not None and ids(r) == list(reversed(deploy_visible)), str(ids(r)))
    p1 = search_req(bob, next(seq), query="deploy", limit=2)
    p2 = search_req(bob, next(seq), query="deploy", limit=2, cursor=p1.next_cursor if p1 else 0)
    p3 = search_req(bob, next(seq), query="deploy", limit=2, cursor=p2.next_cursor if p2 else 0)
    check("paging: newest pages of 2 with cursors", p1 and p2 and p3 and ids(p1) + ids(p2) + ids(p3) == deploy_visible
          and p1.has_more and p2.has_more and not p3.has_more, f"{ids(p1)} {ids(p2)} {ids(p3)}")
    check("paging: only the first page counts", p1 is not None and p1.total == 5 and p2 is not None and p2.total == -1 and not p2.first_page)
    q1 = search_req(bob, next(seq), query="deploy", sort=H.OLDEST, limit=3)
    q2 = search_req(bob, next(seq), query="deploy", sort=H.OLDEST, limit=3, cursor=q1.next_cursor if q1 else 0)
    check("paging: oldest", q1 and q2 and ids(q1) + ids(q2) == list(reversed(deploy_visible)) and not q2.has_more, f"{ids(q1)} {ids(q2)}")
    r = search_req(bob, next(seq), query="deploy", sort=H.RELEVANCE)
    check("sort: relevance puts the 3× match first", r is not None and ids(r)[0] == m4 and sorted(ids(r)) == sorted(deploy_visible), str(ids(r)))
    a1 = search_req(bob, next(seq), query="deploy", sort=H.RELEVANCE, limit=3)
    a2 = search_req(bob, next(seq), query="deploy", sort=H.RELEVANCE, limit=3, offset=a1.next_offset if a1 else 0)
    check("paging: relevance by offset", a1 and a2 and a1.next_offset == 3 and ids(a1) + ids(a2) == ids(r) and not a2.has_more, f"{ids(a1)} {ids(a2)}")

    # The index follows edits, deletes and wipes.
    bob.flush(0.3)
    bob.send(pb.Packet.MESSAGE_EDIT_REQ, message_edit_req=pb.MessageEditReq(channel_id=ch_b, message_id=m5, content="renamed note about zeppelins"))
    e = bob.wait(pb.Packet.MESSAGE_EDIT_RES, timeout=2)
    check("edit a pre-rename message (ownership by uid)", e is not None and e.message_edit_res.success, e.message_edit_res.message if e else None)
    r = search_req(bob, next(seq), query="zeppelin")
    check("edit re-indexes the new text", r is not None and ids(r) == [m5], str(ids(r)))
    r = search_req(bob, next(seq), query="checklist")
    check("... and drops the old", r is not None and ids(r) == [])
    carol.flush(0.3)
    carol.send(pb.Packet.MESSAGE_EDIT_REQ, message_edit_req=pb.MessageEditReq(channel_id=ch_b, message_id=m7, content="here is the updated file"))
    carol.wait(pb.Packet.MESSAGE_EDIT_RES, timeout=2)
    r = search_req(bob, next(seq), query="runbook")
    check("an edit keeps the file names indexed", r is not None and ids(r) == [m7, m4], str(ids(r)))
    bob.flush(0.3)
    bob.send(pb.Packet.MESSAGE_EDIT_REQ, message_edit_req=pb.MessageEditReq(channel_id=ch_b, message_id=m2, content="hijack"))
    e = bob.wait(pb.Packet.MESSAGE_EDIT_RES, timeout=2)
    check("someone else's message still can't be edited", e is not None and not e.message_edit_res.success)
    bob.send(pb.Packet.MESSAGE_DELETE_REQ, message_delete_req=pb.MessageDeleteReq(channel_id=ch_b, message_id=m5))
    d = bob.wait(pb.Packet.MESSAGE_DELETE_RES, timeout=2)
    check("delete a pre-rename message (ownership by uid)", d is not None and d.message_delete_res.success, d.message_delete_res.message if d else None)
    check("no audit entry for deleting your own", sql("select count(*) from audit_log where action='message_delete' and details=?", f"message {m5}") == [(0,)])
    r = search_req(bob, next(seq), query="zeppelin")
    check("delete drops it from the index", r is not None and ids(r) == [])
    owner.flush(0.3)
    owner.send(pb.Packet.CHANNEL_WIPE_REQ, channel_wipe_req=pb.ChannelWipeRequest(channel_id=ch_b))
    owner.wait(pb.Packet.CHANNEL_WIPE_RES, timeout=3)
    r = search_req(bob, next(seq), query="tomorrow")
    check("wipe drops the channel from the index", r is not None and ids(r) == [])
    check("FTS integrity-check passes", sql("insert into messages_fts(messages_fts) values('integrity-check')") == [])

    # A refused search is answered, never dropped.
    for c in (bob,): c.flush(0.3)
    base = next(seq)
    for i in range(10):
        bob.send(pb.Packet.MESSAGE_SEARCH_REQ, message_search_req=pb.MessageSearchRequest(seq=base * 100 + i, query="deploy"))
    bob.drain(3.0)
    answers = [p.message_search_res for p in bob.inbox if p.type == pb.Packet.MESSAGE_SEARCH_RES]
    limited = [a for a in answers if not a.success and a.retry_after_ms > 0]
    check("a burst: refusals carry retry_after_ms", len(limited) >= 3, f"{len(answers)} answers, {len(limited)} limited")
    bob.inbox.clear()
    time.sleep(6)

    # A member from before uids reached the server sits under a placeholder
    # (legacy_claims); their first token claims it — row, messages, roles.
    sql("insert into members(uid, username, joined_at, nickname) values(-500, 'lazy', ?, 'Lazy')", int(time.time()))
    sql("insert into legacy_claims(username, uid) values('lazy', -500)")
    sql("insert into member_roles(uid, role_id) select -500, id from roles where is_default = 0 limit 1")
    had_role = sql("select count(*) from member_roles where uid = -500")[0][0]
    lm = put(ch_a, "lazy", "lazy message", 10, uid=-500)
    lazy = Client("lazy"); ok, _ = auth_ok(lazy)
    check("legacy member: claimed on first auth (member row by uid, nickname kept)",
          ok and sql("select uid, nickname from members where username='lazy'") == [(uid_for("lazy"), "Lazy")])
    check("legacy member: messages and roles follow", sql("select sender_uid from messages where id=?", lm) == [(uid_for("lazy"),)]
          and sql("select count(*) from member_roles where uid=?", uid_for("lazy"))[0][0] == had_role
          and sql("select count(*) from legacy_claims where username='lazy'") == [(0,)])
    rm = owner.wait(pb.Packet.MEMBER_REMOVE, timeout=2, pred=lambda p: p.member_remove.uid == -500)
    up = owner.wait(pb.Packet.MEMBER_UPSERT, timeout=2, pred=lambda p: p.member_upsert.member.uid == uid_for("lazy"))
    check("legacy member: rosters swap the placeholder's row for the account's", rm is not None and up is not None
          and up.member_upsert.member.nickname == "Lazy" and up.member_upsert.total_members == rm.member_remove.total_members + 1)
    lazy.close()

    # Bans go by uid: a made-up placeholder can't be banned (it would only
    # have matched a name, outside the hierarchy), and a placeholder ban row
    # never matches a member by name.
    owner.flush(0.3)
    owner.send(pb.Packet.BAN_MEMBER_REQ, ban_member_req=pb.BanMemberRequest(username="bob", uid=-999, reason="x"))
    r = owner.wait(pb.Packet.MOD_ACTION_RES, pred=lambda p: p.mod_action_res.action == "ban")
    check("a ban on a made-up placeholder is refused",
          r is not None and not r.mod_action_res.success and sql("select count(*) from bans where uid=-999") == [(0,)])
    sql("insert into bans(uid, username, banned_at, banned_by_uid, banned_by, reason, expires_at) "
        "values(-777, 'bob', ?, 0, '', '', 0)", int(time.time()))
    bob2 = Client("bob"); ok, _ = auth_ok(bob2)
    check("a placeholder ban never matches a member by name", ok)
    bob2.close()
    sql("delete from bans where uid=-777")

    # Past the claim deadline a legacy name claims nothing: by then the name
    # may belong to someone else.
    sql("insert into members(uid, username, joined_at, nickname) values(-501, 'late', ?, 'Late')", int(time.time()))
    sql("insert into legacy_claims(username, uid) values('late', -501)")
    sql("insert or replace into server_meta(key, value) values('legacy_claims_until', ?)", str(int(time.time()) - 1))
    late = Client("late"); auth_ok(late)
    check("legacy claims expire (a later holder of the name inherits nothing)",
          sql("select uid, nickname from members where uid = -501") == [(-501, "Late")]
          and sql("select nickname from members where uid = ?", uid_for("late")) in ([], [("",)])
          and sql("select count(*) from legacy_claims") == [(0,)])
    late.close()
    sql("delete from server_meta where key='legacy_claims_until'")
    sql("delete from members where uid = -501")

    # Ban purge by uid: a rename inside the window doesn't shield messages.
    cm = put(ch_a, "scarol_old", "spam under an older name", 11, uid=uid_for("scarol"))
    cm2 = put(ch_a, "scarol", "spam", 11)
    sql("update messages set timestamp=? where id in (?, ?)", int(time.time()) - 60, cm, cm2)
    owner.flush(0.3)
    owner.send(pb.Packet.BAN_MEMBER_REQ, ban_member_req=pb.BanMemberRequest(username="scarol", uid=uid_of("scarol"), reason="t", delete_message_seconds=3600))
    owner.wait(pb.Packet.MOD_ACTION_RES, pred=lambda p: p.mod_action_res.action == "ban")
    check("ban purge goes by uid (pre-rename row purged too)", sql("select count(*) from messages where id in (?, ?)", cm, cm2) == [(0,)])
    sql("delete from bans where username='scarol'")
    for c in (owner, bob, carol): c.close()


def test_message_search_turkish_and_within():
    """Dotless ı matches i (word search, via the index), and inside-word
    matching (MessageSearchRequest.match_within_words): fragments anywhere
    in words, folded like the index, punctuation kept, file names,
    exclusions, encrypted channels skipped, filters, newest-first order."""
    print("[search] ı ↔ i, and matching inside words")
    owner = Client("alice"); assert auth_ok(owner)[0]
    tess = join("tess", owner)
    for c in (owner, tess): c.flush(0.4)
    def create(name, encrypted=False):
        owner.send(pb.Packet.CHANNEL_CREATE_REQ, channel_create_req=pb.ChannelCreateRequest(
            name=name, type=pb.ChannelInfo.TEXT, encrypted=encrypted))
        r = owner.wait(pb.Packet.CHANNEL_ACTION_RES, pred=lambda p: p.channel_action_res.action == "create")
        return r.channel_action_res.channel.id
    ch, sealed = create("within-a"), create("within-sealed", True)
    for c in (owner, tess): c.flush(0.4)
    t0 = max(int(time.time()), sql("select coalesce(max(timestamp), 0) from messages")[0][0]) + 10
    def put(sender, content, k, envelope=None, channel=None):
        return sql("insert into messages(channel_id, sender, content, timestamp, envelope, sender_uid) values(?,?,?,?,?,?) returning id",
                   channel or ch, sender, content, t0 + k * 60, envelope, uid_for(sender))[0][0]
    k1 = put("tess", "kısa yol tarifi", 0)
    k2 = put("alice", "kisa cevap ver", 1)
    k3 = put("tess", "Çağrı merkezini aradım", 2)
    x1 = put("tess", "the xabcy token", 3)
    x2 = put("alice", "redeployment went fine", 4)
    x3 = put("tess", "Déploiement terminé", 5)
    x4 = put("alice", "please deploy again soon", 6)
    x5 = put("tess", "I write c++ daily", 7)
    x6 = put("alice", "notes attached", 8)
    aid = sql("insert into attachments(message_id, kind, filename, mime, size_bytes, position, created_at, upload_status, uploader, channel_id) "
              "values(0,2,'Quarterly-ROADMAP.pdf','application/pdf',10,0,?,'ready','alice',?) returning id", t0, ch)[0][0]
    sql("update attachments set message_id=? where id=?", x6, aid)
    s1 = put("tess", "PLACEHOLDER xabcy", 9, envelope=b"\x02S", channel=sealed)
    seq = iter(range(5000, 9000))
    ids = lambda r: [m.id for m in r.messages] if r else None
    W = dict(channel_ids=[ch])

    r = search_req(owner, next(seq), query="kisa", **W)
    check("ı ↔ i: 'kisa' finds kısa and kisa", r is not None and sorted(ids(r)) == sorted([k1, k2]), str(ids(r)))
    r = search_req(owner, next(seq), query="kısa", **W)
    check("ı ↔ i: 'kısa' finds both too", r is not None and sorted(ids(r)) == sorted([k1, k2]), str(ids(r)))
    r = search_req(owner, next(seq), query="CAGRI", **W)
    check("ı ↔ i: 'CAGRI' finds Çağrı", r is not None and ids(r) == [k3], str(ids(r)))

    r = search_req(owner, next(seq), query="abc", **W)
    check("words: 'abc' doesn't find xabcy", r is not None and r.success and ids(r) == [], str(ids(r)))
    r = search_req(owner, next(seq), query="abc", match_within_words=True)
    check("inside words: 'abc' finds xabcy", r is not None and ids(r) == [x1], str(ids(r)))
    check("inside words: the encrypted channel is skipped and named", r is not None and sealed in r.text_skipped_channel_ids and s1 not in ids(r))
    r = search_req(owner, next(seq), query="DÉPLO", match_within_words=True, **W)
    check("inside words: case + accents fold (redeployment, Déploiement, deploy)", r is not None and ids(r) == [x4, x3, x2], str(ids(r)))
    r = search_req(owner, next(seq), query="kisa", match_within_words=True, **W)
    check("inside words: ı ↔ i", r is not None and ids(r) == [k2, k1], str(ids(r)))
    r = search_req(owner, next(seq), query='"oy ag"', match_within_words=True, **W)
    check("inside words: a phrase across a word boundary", r is not None and ids(r) == [x4], str(ids(r)))
    r = search_req(owner, next(seq), query="c++", match_within_words=True, **W)
    check("inside words: punctuation kept ('c++')", r is not None and ids(r) == [x5], str(ids(r)))
    r = search_req(owner, next(seq), query="deplo -again", match_within_words=True, **W)
    check("inside words: exclusions", r is not None and ids(r) == [x3, x2], str(ids(r)))
    r = search_req(owner, next(seq), query="roadm", match_within_words=True, **W)
    check("inside words: file names", r is not None and ids(r) == [x6], str(ids(r)))
    r = search_req(owner, next(seq), query="eploy", match_within_words=True, author_uids=[uid_of("alice")], **W)
    check("inside words + a sender", r is not None and ids(r) == [x4, x2], str(ids(r)))
    r = search_req(owner, next(seq), query="eploy", match_within_words=True, sort=pb.MessageSearchRequest.RELEVANCE, **W)
    check("inside words: relevance sorts newest first", r is not None and ids(r) == [x4, x2], str(ids(r)))
    r = search_req(owner, next(seq), query="-abc", match_within_words=True, **W)
    check("inside words: exclusions alone are refused", r is not None and not r.success)
    r = search_req(owner, next(seq), query="eploy", match_within_words=True, limit=1, **W)
    r2 = search_req(owner, next(seq), query="eploy", match_within_words=True, limit=1, cursor=r.next_cursor if r else 0, **W)
    check("inside words: counted and paged", r is not None and r.total == 2 and ids(r) == [x4] and r.has_more and r2 is not None and ids(r2) == [x2], f"{r.total if r else None} {ids(r2)}")
    for c in (owner, tess): c.close()


def test_v14_search_migration():
    print("[v14] search index + sender uids on an upgraded DB")
    proc = start_server(); stop_server(proc)
    # Back to the pre-v14 shape: no index, no triggers, no sender_uid.
    for t in ("messages_fts_insert", "messages_fts_delete", "messages_fts_edit", "attachments_fts_bind"):
        sql(f"drop trigger if exists {t}")
    sql("drop table if exists messages_fts")
    sql("drop index if exists idx_messages_sender_uid")
    sql("alter table messages drop column sender_uid")
    sql("delete from server_meta where key in ('search_index_version', 'search_fts_v14', 'sender_uid_v14')")
    # Whatever alice's row holds now (her uid, or the owner placeholder if
    # she hasn't connected yet), restored at the end.
    alice_uid = sql("select uid from members where username='alice'")[0][0]
    sql("update members set uid=4242 where uid=?", alice_uid)
    now = int(time.time())
    plain = sql("insert into messages(channel_id, sender, content, timestamp) values('general','alice','zebra crossing',?) returning id", now)[0][0]
    sealed = sql("insert into messages(channel_id, sender, content, timestamp, envelope) values('general','alice','zebra placeholder',?, x'02') returning id", now)[0][0]
    stranger = sql("insert into messages(channel_id, sender, content, timestamp) values('general','nobody','hi',?) returning id", now)[0][0]
    sql("insert into channels(id, name, type, position, encrypted) values('v14-sealed', 'v14-sealed', 0, 300, 1)")
    legacy = sql("insert into messages(channel_id, sender, content, timestamp) values('v14-sealed','alice','quagga',?) returning id", now)[0][0]
    att = sql("insert into attachments(message_id, kind, filename, mime, size_bytes, position, created_at, upload_status, uploader, channel_id) "
              "values(?,2,'okapi-notes.txt','text/plain',1,0,?,'ready','alice','general') returning id", plain, now)[0][0]
    proc = start_server(); stop_server(proc)
    check("index rebuilt with the plaintext rows", sql("select rowid from messages_fts where messages_fts match '\"zebra\"'") == [(plain,)])
    check("... but not plaintext left in an encrypted channel", sql("select rowid from messages_fts where messages_fts match '\"quagga\"'") == [])
    check("... including bound file names", sql("select rowid from messages_fts where messages_fts match '\"okapi\"'") == [(plain,)])
    check("sender_uid back-filled from members by name", sql("select sender_uid from messages where id=?", plain) == [(4242,)])
    check("a name no member holds stays 0", sql("select sender_uid from messages where id=?", stranger) == [(0,)])
    check("schema stamped 17", sql("select value from server_meta where key='schema_version'") == [("17",)])
    check("index at version 2", sql("select value from server_meta where key='search_index_version'") == [("2",)])
    # Version 1 (ı kept as ı) is rebuilt once: a row added while the
    # triggers are gone shows up folded afterwards.
    stop_server(start_server())
    for t in ("messages_fts_insert", "messages_fts_delete", "messages_fts_edit", "attachments_fts_bind"):
        sql(f"drop trigger if exists {t}")
    turkish = sql("insert into messages(channel_id, sender, content, timestamp) values('general','alice','kısa yol',?) returning id", now)[0][0]
    sql("update server_meta set value='1' where key='search_index_version'")
    sql("insert or replace into server_meta(key, value) values('search_fts_v14', '1')")
    proc = start_server(); stop_server(proc)
    check("version 1 index rebuilt as 2, ı stored as i", sql("select rowid from messages_fts where messages_fts match '\"kisa\"'") == [(turkish,)]
          and sql("select value from server_meta where key='search_index_version'") == [("2",)]
          and sql("select count(*) from server_meta where key='search_fts_v14'") == [(0,)])
    check("triggers back after the rebuild", sql("select count(*) from sqlite_master where type='trigger' and name like '%fts%'") == [(4,)])
    sql("delete from messages where id=?", turkish)
    sql("delete from attachments where id=?", att)
    sql("delete from messages where id in (?, ?, ?, ?)", plain, sealed, stranger, legacy)
    sql("delete from channels where id='v14-sealed'")
    sql("update members set uid=? where uid=4242", alice_uid)
    sql("update messages set sender_uid=? where sender_uid=4242", alice_uid)


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
    rj = cal.wait(pb.Packet.CHANNEL_MSG_REJECTED, timeout=2, pred=lambda p: "Slowmode" in p.channel_msg_rejected.reason)
    check("slowmode refusal says how long is left (retry_after_seconds 1..3)",
          rj is not None and 1 <= rj.channel_msg_rejected.retry_after_seconds <= 3)
    owner.flush(0.5)
    owner.send(pb.Packet.CHANNEL_MSG, channel_msg=pb.ChannelMessage(channel_id="general", content="a"))
    owner.send(pb.Packet.CHANNEL_MSG, channel_msg=pb.ChannelMessage(channel_id="general", content="b"))
    owner.drain(1.0)
    got = [p.channel_msg.content for p in owner.inbox if p.type == pb.Packet.CHANNEL_MSG]
    check("owner (MANAGE_MESSAGES) bypasses slowmode", "a" in got and "b" in got)
    owner.send(pb.Packet.CHANNEL_UPDATE_REQ, channel_update_req=pb.ChannelUpdateRequest(channel_id="general", slowmode_seconds=0))
    owner.wait(pb.Packet.CHANNEL_UPDATE_RES)
    cal.close(); owner.close()


def test_channel_topic():
    print("[topic] MANAGE_CHANNELS sets a text channel's topic; everyone who can see it gets it")
    owner = Client("alice"); assert auth_ok(owner)[0]
    tia = join("tia", owner); owner.flush(0.5); tia.flush(0.5)

    def update(c, channel_id, **kw):
        c.send(pb.Packet.CHANNEL_UPDATE_REQ, channel_update_req=pb.ChannelUpdateRequest(channel_id=channel_id, **kw))
        return c.wait(pb.Packet.CHANNEL_UPDATE_RES)

    r = update(owner, "general", topic="   Talk about anything   ")
    check("topic set (trimmed) and echoed", r is not None and r.channel_update_res.success and
          r.channel_update_res.channel.topic == "Talk about anything", str(r.channel_update_res.channel.topic) if r else None)
    r = tia.wait(pb.Packet.CHANNEL_UPDATE_RES)
    check("members get the new topic", r is not None and r.channel_update_res.channel.topic == "Talk about anything")
    r = update(owner, "general", slowmode_seconds=0)
    check("an update without a topic leaves it alone", r is not None and r.channel_update_res.channel.topic == "Talk about anything")
    tia.flush(0.5)
    r = update(tia, "general", topic="hijacked")
    check("topic edit denied without MANAGE_CHANNELS", r is not None and not r.channel_update_res.success)
    check("...and the topic is unchanged", sql("select topic from channels where id='general'") == [("Talk about anything",)])
    r = update(owner, "general", topic="é" * 700)
    check("topic clamped to 1024 bytes on a character boundary", r is not None and
          r.channel_update_res.channel.topic == "é" * 512)
    r = update(owner, "voice-lounge", topic="not here")
    check("voice channels have no topic", r is not None and r.channel_update_res.channel.topic == "")
    late = Client("tia")
    ok, r = auth_ok(late)
    lst = {ch.id: ch for ch in r.community_auth_res.channels} if ok else {}
    check("the channel list at auth carries the topic", lst.get("general") is not None and lst["general"].topic == "é" * 512)
    late.close()
    owner.flush(0.5)
    r = update(owner, "general", topic="")
    check("an empty topic clears it", r is not None and r.channel_update_res.channel.topic == "")
    check("audited as a topic change",
          sql("select details from audit_log where action='channel_update' and channel_id='general' order by id desc limit 1") == [("topic",)])
    tia.close(); owner.close()


def test_personal_invites():
    print("[invites] personal invites for Invite friends: single-use, bound, reused, request ids")
    owner = Client("alice"); assert auth_ok(owner)[0]
    owner.flush(0.5)

    def create(c, request_id, for_user="", expires_at=0, max_uses=0):
        c.send(pb.Packet.INVITE_CREATE_REQ, invite_create_req=pb.InviteCreateRequest(
            expires_at=expires_at, max_uses=max_uses, for_user=for_user, for_uid=uid_of(for_user) if for_user else 0,
            request_id=request_id))
        return c.wait(pb.Packet.INVITE_CREATE_RES, pred=lambda p: p.invite_create_res.request_id == request_id)

    r = create(owner, 41, for_user="pia", max_uses=0)
    inv = r.invite_create_res.invite if r else None
    check("personal invite created, request id echoed", r is not None and r.invite_create_res.success)
    check("...single-use, never expires, bound to the invitee",
          inv is not None and inv.max_uses == 1 and inv.expires_at == 0 and inv.for_user == "pia", str(inv))
    r = create(owner, 42, for_user="pia")
    check("asking again returns the same live invite",
          r is not None and r.invite_create_res.success and r.invite_create_res.invite.code == inv.code)
    check("...without minting another", sql("select count(*) from invites where for_user='pia'") == [(1,)])
    owner.send(pb.Packet.INVITE_LIST_REQ, invite_list_req=pb.InviteListRequest())
    r = owner.wait(pb.Packet.INVITE_LIST_RES)
    check("the invite list names the invitee",
          r is not None and any(i.code == inv.code and i.for_user == "pia" for i in r.invite_list_res.invites))

    other = Client("quin", invite=inv.code)
    ok, r = auth_ok(other)
    check("someone else can't redeem it", not ok and r is not None and "someone else" in r.community_auth_res.message,
          r.community_auth_res.message if r else None)
    other.close()
    check("...and doesn't spend it", sql("select uses from invites where code=?", inv.code) == [(0,)])
    pia = Client("pia", invite=inv.code)
    check("the invitee joins with it", auth_ok(pia)[0])
    check("...which uses it up", sql("select count(*) from invites where code=?", inv.code) == [(0,)])
    owner.flush(0.5); pia.flush(0.5)

    r = create(owner, 43, for_user="pia")
    check("no personal invite for an existing member, flagged as such",
          r is not None and not r.invite_create_res.success and r.invite_create_res.already_member)
    r = create(owner, 44, for_user="alice")
    check("no personal invite for yourself", r is not None and not r.invite_create_res.success and
          not r.invite_create_res.already_member)
    r = create(owner, 45, max_uses=3)
    check("ordinary invites are unchanged", r is not None and r.invite_create_res.success and
          r.invite_create_res.invite.max_uses == 3 and r.invite_create_res.invite.for_user == "")

    set_everyone_perms(owner, ALL_MEMBER_BITS & ~pb.PERM_CREATE_INVITE)
    pia.flush(0.5)
    r = create(pia, 46, for_user="zed")
    check("a refusal still echoes the request id", r is not None and not r.invite_create_res.success)
    set_everyone_perms(owner, ALL_MEMBER_BITS)
    pia.close(); owner.close()


def test_voice_moderation():
    print("[voice mod] server mute/deafen, move, disconnect")
    owner = Client("alice"); assert auth_ok(owner)[0]
    dee = join("dee", owner); owner.flush(0.5); dee.flush(0.5)
    dee.send(pb.Packet.JOIN_VOICE_REQ, join_voice_req=pb.JoinVoiceRequest(channel_id="voice-lounge"))
    owner.wait(pb.Packet.VOICE_PRESENCE_UPDATE, pred=lambda p: "dee" in p.voice_presence_update.active_users)
    dee.send(pb.Packet.VOICE_MOD_REQ, voice_mod_req=pb.VoiceModRequest(username="alice", uid=uid_of("alice"), action=pb.VoiceModRequest.SERVER_MUTE))
    r = dee.wait(pb.Packet.MOD_ACTION_RES, pred=lambda p: p.mod_action_res.action == "voice_mod")
    check("denied without MUTE_MEMBERS", r is not None and not r.mod_action_res.success)
    owner.flush(0.3)
    owner.send(pb.Packet.VOICE_MOD_REQ, voice_mod_req=pb.VoiceModRequest(username="dee", uid=uid_of("dee"), action=pb.VoiceModRequest.SERVER_MUTE))
    r = owner.wait(pb.Packet.MOD_ACTION_RES, pred=lambda p: p.mod_action_res.action == "voice_mod")
    check("owner server-mutes dee", r is not None and r.mod_action_res.success, r.mod_action_res.message if r else None)
    vp = owner.wait(pb.Packet.VOICE_PRESENCE_UPDATE, timeout=2, pred=lambda p: any(s.username == "dee" and s.is_server_muted for s in p.voice_presence_update.user_states))
    check("presence shows is_server_muted", vp is not None)
    check("persisted on member", sql("select server_muted from members where username='dee'") == [(1,)])
    owner.send(pb.Packet.VOICE_MOD_REQ, voice_mod_req=pb.VoiceModRequest(username="dee", uid=uid_of("dee"), action=pb.VoiceModRequest.SERVER_UNMUTE))
    owner.wait(pb.Packet.MOD_ACTION_RES, pred=lambda p: p.mod_action_res.action == "voice_mod")
    dee.flush(0.5); owner.flush(0.5)
    owner.send(pb.Packet.VOICE_MOD_REQ, voice_mod_req=pb.VoiceModRequest(username="dee", uid=uid_of("dee"), action=pb.VoiceModRequest.MOVE, channel_id="voice-lounge-2"))
    r = owner.wait(pb.Packet.MOD_ACTION_RES, pred=lambda p: p.mod_action_res.action == "voice_mod")
    check("move succeeded", r is not None and r.mod_action_res.success, r.mod_action_res.message if r else None)
    n = dee.wait(pb.Packet.VOICE_FORCE_NOTIFY, timeout=2)
    check("target gets VOICE_FORCE_NOTIFY MOVED", n is not None and n.voice_force_notify.action == pb.VoiceForceNotify.MOVED and n.voice_force_notify.channel_id == "voice-lounge-2")
    vp = owner.wait(pb.Packet.VOICE_PRESENCE_UPDATE, timeout=2, pred=lambda p: p.voice_presence_update.channel_id == "voice-lounge-2" and "dee" in p.voice_presence_update.active_users)
    check("presence in new channel", vp is not None)
    owner.flush(0.3); dee.flush(0.3)
    owner.send(pb.Packet.VOICE_MOD_REQ, voice_mod_req=pb.VoiceModRequest(username="dee", uid=uid_of("dee"), action=pb.VoiceModRequest.DISCONNECT))
    r = owner.wait(pb.Packet.MOD_ACTION_RES, pred=lambda p: p.mod_action_res.action == "voice_mod")
    check("disconnect succeeded", r is not None and r.mod_action_res.success)
    n = dee.wait(pb.Packet.VOICE_FORCE_NOTIFY, timeout=2)
    check("target gets DISCONNECTED", n is not None and n.voice_force_notify.action == pb.VoiceForceNotify.DISCONNECTED)
    vp = owner.wait(pb.Packet.VOICE_PRESENCE_UPDATE, timeout=2, pred=lambda p: p.voice_presence_update.channel_id == "voice-lounge-2" and len(p.voice_presence_update.active_users) == 0)
    check("presence cleared", vp is not None)
    owner.send(pb.Packet.VOICE_MOD_REQ, voice_mod_req=pb.VoiceModRequest(username="dee", uid=uid_of("dee"), action=pb.VoiceModRequest.DISCONNECT))
    r = owner.wait(pb.Packet.MOD_ACTION_RES, pred=lambda p: p.mod_action_res.action == "voice_mod")
    check("disconnect of a non-voice member fails cleanly", r is not None and not r.mod_action_res.success)
    dee.close(); owner.close()


def udp_key(jwt_token):
    """A session's UDP key as datagrams carry it: the token's last 31 chars, NUL-padded."""
    return jwt_token[-31:].encode().ljust(32, b"\0")


def udp_audio_packet(jwt_token, seq, payload):
    return bytes([0]) + udp_key(jwt_token) + struct.pack("<HH", seq, len(payload)) + payload


def udp_ping(jwt_token, ts=1):
    """A PING as native sends it (45 bytes: audio header + 8-byte timestamp)."""
    return bytes([5]) + udp_key(jwt_token) + struct.pack("<HHQ", 0, 8, ts)


BIND_CHALLENGE, BIND_ECHO = 10, 11


def bind_echo(jwt_token, challenge):
    """The client's answer (review S11): the challenge, type flipped, key in the id field."""
    return bytes([BIND_ECHO]) + udp_key(jwt_token) + challenge[33:41]


def udp_bind(sock, jwt_token, port):
    """Bind `sock` as the session's endpoint on `port` the way native does
    (review S11): PING -> BIND_CHALLENGE -> BIND_ECHO. True once a PING is
    then echoed (the relay echoes only to a bound endpoint)."""
    sock.sendto(udp_ping(jwt_token, 1), (HOST, port))
    try:
        ch = sock.recv(2048)
    except socket.timeout:
        return False
    if len(ch) != 41 or ch[0] != BIND_CHALLENGE:
        return False
    sock.sendto(bind_echo(jwt_token, ch), (HOST, port))
    ping = udp_ping(jwt_token, 2)
    sock.sendto(ping, (HOST, port))
    try:
        return sock.recv(2048) == ping
    except socket.timeout:
        return False


def udp_quiet(sock, timeout=0.6):
    """True when nothing arrives on `sock` within `timeout`."""
    sock.settimeout(timeout)
    try:
        sock.recv(2048)
        return False
    except socket.timeout:
        return True
    finally:
        sock.settimeout(2)


def test_udp_relay():
    print("[udp] voice relay: ping echo, audio fan-out, server-mute drop, media ping")
    owner = Client("alice"); assert auth_ok(owner)[0]
    ann = join("ann", owner); bob = join("bobby", owner)
    for c in (ann, bob):
        c.send(pb.Packet.JOIN_VOICE_REQ, join_voice_req=pb.JoinVoiceRequest(channel_id="voice-lounge"))
    owner.wait(pb.Packet.VOICE_PRESENCE_UPDATE, pred=lambda p: "bobby" in p.voice_presence_update.active_users)
    ua = socket.socket(socket.AF_INET, socket.SOCK_DGRAM); ua.bind((HOST, 0)); ua.settimeout(2)
    ub = socket.socket(socket.AF_INET, socket.SOCK_DGRAM); ub.bind((HOST, 0)); ub.settimeout(2)
    # A PING without a live key is not echoed: the relay is no reflector.
    ua.sendto(bytes([5]) + b"\0" * 32 + struct.pack("<HHQ", 0, 8, 1234), (HOST, 8083))
    check("unkeyed voice PING is not echoed", udp_quiet(ua))
    # register endpoints the way the client does: PING, challenge, echo
    check("voice endpoints bind through the challenge, then PINGs echo",
          udp_bind(ub, bob.jwt, 8083) and udp_bind(ua, ann.jwt, 8083))
    ua.sendto(udp_audio_packet(ann.jwt, 1, b"a0"), (HOST, 8083))
    try:
        got = ub.recv(2048)
    except socket.timeout:
        got = b""
    ann_mid = str(uid_for("ann")).encode()
    check("audio relayed to the other member with sender rewritten to their uid",
          got[1:33].rstrip(b"\0") == ann_mid and got[-2:] == b"a0")
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
    owner.send(pb.Packet.VOICE_MOD_REQ, voice_mod_req=pb.VoiceModRequest(username="ann", uid=uid_of("ann"), action=pb.VoiceModRequest.SERVER_MUTE))
    owner.wait(pb.Packet.MOD_ACTION_RES, pred=lambda p: p.mod_action_res.action == "voice_mod")
    time.sleep(0.3)
    ua.sendto(udp_audio_packet(ann.jwt, 99, b"muted"), (HOST, 8083))
    ub.settimeout(1.0)
    try:
        ub.recv(2048); dropped = False
    except socket.timeout:
        dropped = True
    check("server-muted member's audio dropped by the relay", dropped)
    owner.send(pb.Packet.VOICE_MOD_REQ, voice_mod_req=pb.VoiceModRequest(username="ann", uid=uid_of("ann"), action=pb.VoiceModRequest.SERVER_UNMUTE))
    owner.wait(pb.Packet.MOD_ACTION_RES, pred=lambda p: p.mod_action_res.action == "voice_mod")
    # media socket: the same handshake binds the media endpoint, then PINGs echo
    um = socket.socket(socket.AF_INET, socket.SOCK_DGRAM); um.bind((HOST, 0)); um.settimeout(2)
    check("media endpoint binds through the challenge, then PINGs echo", udp_bind(um, ann.jwt, 8084))
    # Keyframe requests and NACKs name the streamer by uid. A relayed NACK
    # never carries the requester's UDP key (a session secret).
    ub_media = socket.socket(socket.AF_INET, socket.SOCK_DGRAM); ub_media.bind((HOST, 0)); ub_media.settimeout(2)
    udp_bind(ub_media, bob.jwt, 8084)
    bob_key = udp_key(bob.jwt)
    ub_media.sendto(bytes([2]) + bob_key + ann_mid.ljust(32, b"\0"), (HOST, 8084))
    try:
        kf = um.recv(2048)
    except socket.timeout:
        kf = b""
    check("keyframe request reaches the streamer named by uid", kf[:1] == b"\x02" and kf[1:65] == b"\0" * 64)
    nack = bytes([3]) + bob_key + ann_mid.ljust(32, b"\0") + struct.pack("<IH", 77, 1) + struct.pack("<H", 4) + b"\0" * 126
    ub_media.sendto(nack, (HOST, 8084))
    try:
        got_nack = um.recv(2048)
    except socket.timeout:
        got_nack = b""
    check("NACK relayed by uid, requester's key stripped", got_nack[:1] == b"\x03"
          and got_nack[1:33] == b"\0" * 32 and got_nack[33:65].rstrip(b"\0") == ann_mid
          and struct.unpack("<I", got_nack[65:69])[0] == 77)
    ub_media.sendto(bytes([2]) + bob_key + b"ann".ljust(32, b"\0"), (HOST, 8084))
    um.settimeout(0.6)
    try:
        um.recv(2048); by_name = True
    except socket.timeout:
        by_name = False
    check("a name is not a media target any more", not by_name)
    um.settimeout(2)
    # A live rename mid-call: the media identity is the uid, so audio keeps
    # its sender and the same UDP key keeps working.
    time.sleep(1.1)
    ann.send(pb.Packet.SESSION_TOKEN_UPDATE, session_token_update=pb.SessionTokenUpdate(
        jwt_token=jwt("ann-renamed", uid=uid_for("ann"))))
    vp = owner.wait(pb.Packet.VOICE_PRESENCE_UPDATE, pred=lambda p: "ann-renamed" in p.voice_presence_update.active_users)
    check("renamed mid-call: voice presence shows the new name", vp is not None)
    ua.sendto(udp_audio_packet(ann.jwt, 500, b"still"), (HOST, 8083))
    ub.settimeout(2)
    try:
        got = ub.recv(2048)
    except socket.timeout:
        got = b""
    check("audio flows on through a rename, same sender uid", got[1:33].rstrip(b"\0") == ann_mid and got[-5:] == b"still")
    time.sleep(1.1)
    back = Client("ann", uid=uid_for("ann")); auth_ok(back); back.close()   # the name back, for later tests
    for sk in (ua, ub, um, ub_media): sk.close()
    ann.close(); bob.close(); owner.close()


def test_udp_return_routability():
    """Review S11: a keyed datagram never binds or moves a session's UDP
    endpoint; only an echo of the relay's challenge from that address does."""
    print("[udp] return-routability: endpoints bind only through an echoed challenge (S11)")
    owner = Client("alice"); assert auth_ok(owner)[0]
    rip = join("ripley", owner); ty = join("tycho", owner)
    for c in (rip, ty):
        c.send(pb.Packet.JOIN_VOICE_REQ, join_voice_req=pb.JoinVoiceRequest(channel_id="voice-lounge"))
    owner.wait(pb.Packet.VOICE_PRESENCE_UPDATE, pred=lambda p: "ripley" in p.voice_presence_update.active_users
               and "tycho" in p.voice_presence_update.active_users)

    def sock():
        s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM); s.bind((HOST, 0)); s.settimeout(2); return s

    def recv(s):
        try:
            return s.recv(2048)
        except socket.timeout:
            return b""

    ty_mid = str(uid_for("tycho")).encode()
    ut = sock(); ur = sock(); ux = sock()
    check("normal join: PING, challenge, echo binds; PINGs then echo", udp_bind(ut, ty.jwt, 8083))

    # ripley's first keyed datagram comes from ur, which isn't bound yet
    ur.sendto(udp_audio_packet(rip.jwt, 1, b"hello from ripley"), (HOST, 8083))
    ch = recv(ur)
    check("keyed AUDIO from an unbound endpoint earns a 41-byte challenge carrying no key",
          len(ch) == 41 and ch[0] == BIND_CHALLENGE and ch[1:33] == b"\0" * 32, ch[:1])
    got = recv(ut)
    check("...while the audio itself still relays by key", got[-17:] == b"hello from ripley")
    ut.sendto(udp_audio_packet(ty.jwt, 1, b"t1"), (HOST, 8083))
    check("nothing is relayed to an endpoint that hasn't echoed", udp_quiet(ur))
    ur.sendto(bytes([BIND_ECHO]) + udp_key(rip.jwt) + bytes(8), (HOST, 8083))
    ut.sendto(udp_audio_packet(ty.jwt, 2, b"t2"), (HOST, 8083))
    check("an echo with the wrong cookie binds nothing", udp_quiet(ur))
    ux.sendto(bind_echo(rip.jwt, ch), (HOST, 8083))
    ut.sendto(udp_audio_packet(ty.jwt, 3, b"t3"), (HOST, 8083))
    check("the right cookie from another address binds nothing", udp_quiet(ur, 0.4) and udp_quiet(ux, 0.4))
    ur.sendto(bind_echo(rip.jwt, ch), (HOST, 8083))
    time.sleep(0.05)
    ut.sendto(udp_audio_packet(ty.jwt, 4, b"t4"), (HOST, 8083))
    got = recv(ur)
    check("the echo from the challenged address binds it: relay flows",
          got[1:33].rstrip(b"\0") == ty_mid and got[-2:] == b"t4")
    ur.sendto(bind_echo(rip.jwt, ch), (HOST, 8083))   # a replayed echo is harmless

    # Spoofed-source rebind: socket B (ux) sends with A's (ripley's) key.
    time.sleep(0.3)   # past the per-session challenge interval
    ux.sendto(udp_ping(rip.jwt, 5), (HOST, 8083))
    ch2 = recv(ux)
    check("a keyed PING from another address gets a challenge, not an echo",
          len(ch2) == 41 and ch2[0] == BIND_CHALLENGE and ch2[33:41] != ch[33:41])
    ux.sendto(udp_audio_packet(rip.jwt, 6, b"spoofed source"), (HOST, 8083))
    recv(ut)   # relayed by key, as before
    ut.sendto(udp_audio_packet(ty.jwt, 5, b"t5"), (HOST, 8083))
    got = recv(ur)
    check("keyed datagrams from another address don't move the endpoint",
          got[-2:] == b"t5" and udp_quiet(ux, 0.4))
    # ...a stale cookie (the first challenge's) doesn't move it either
    ux.sendto(bind_echo(rip.jwt, ch), (HOST, 8083))
    ut.sendto(udp_audio_packet(ty.jwt, 6, b"t6"), (HOST, 8083))
    check("a stale cookie doesn't move it", recv(ur)[-2:] == b"t6" and udp_quiet(ux, 0.4))

    # Roam / NAT rebinding: the new address echoes its challenge -> moved.
    ux.sendto(bind_echo(rip.jwt, ch2), (HOST, 8083))
    time.sleep(0.05)
    ut.sendto(udp_audio_packet(ty.jwt, 7, b"t7"), (HOST, 8083))
    check("echoing from the new address moves the endpoint (roam)",
          recv(ux)[-2:] == b"t7" and udp_quiet(ur, 0.4))

    # Challenges are paced per session per socket, and never outgrow their trigger.
    time.sleep(0.3)
    uy = sock()
    for i in range(20):
        uy.sendto(udp_ping(rip.jwt, 100 + i), (HOST, 8083))
    n = 0
    uy.settimeout(0.2)
    try:
        while True:
            uy.recv(2048); n += 1
    except socket.timeout:
        pass
    check("a burst from a new address earns one challenge (250 ms pacing)", n == 1, f"got {n}")
    time.sleep(0.3)
    uy.sendto(udp_audio_packet(rip.jwt, 7, b"abc"), (HOST, 8083))   # 37 + 3 = 40 bytes < 41
    check("a datagram smaller than a challenge earns none", udp_quiet(uy, 0.4))
    uy.sendto(udp_ping(jwt("nobody-here"), 1), (HOST, 8083))
    check("a PING with an unknown key gets nothing back", udp_quiet(uy, 0.4))

    # Media socket: same rule. tycho's media endpoint isn't bound, so a
    # keyframe request for him goes nowhere until he echoes his challenge.
    umt = sock(); umr = sock()
    check("ripley's media endpoint binds through the challenge", udp_bind(umr, rip.jwt, 8084))
    umt.sendto(udp_ping(ty.jwt, 1), (HOST, 8084))
    mch = recv(umt)
    check("media PING from an unbound endpoint gets a challenge", len(mch) == 41 and mch[0] == BIND_CHALLENGE)
    kf = bytes([2]) + udp_key(rip.jwt) + ty_mid.ljust(32, b"\0")
    umr.sendto(kf, (HOST, 8084))
    check("nothing is relayed to an unbound media endpoint", udp_quiet(umt))
    umt.sendto(udp_ping(ty.jwt, 2), (HOST, 8084))
    mch2 = recv(umt)
    check("the same address is re-sent the same cookie (a slow path's echo stays good)",
          len(mch2) == 41 and mch2[33:41] == mch[33:41])
    umt.sendto(bind_echo(ty.jwt, mch), (HOST, 8084))
    time.sleep(0.3)   # keyframe relays are paced per streamer
    umr.sendto(kf, (HOST, 8084))
    check("after the echo the keyframe request reaches tycho", recv(umt)[:1] == b"\x02")
    for s in (ut, ur, ux, uy, umt, umr): s.close()
    rip.close(); ty.close(); owner.close()


def test_mls_delivery_service():
    """MLS DS: group create / info / epoch rule / broadcast excludes committer /
    participant gating / group dropped when the channel empties; sealed UDP types relay."""
    print("[mls] delivery service + sealed relay types")
    owner = Client("alice"); assert auth_ok(owner)[0]
    ann = join("ann", owner); bob = join("bobby", owner); cid = join("cid", owner)
    for c in (ann, bob):
        c.send(pb.Packet.JOIN_VOICE_REQ, join_voice_req=pb.JoinVoiceRequest(channel_id="voice-lounge"))
    owner.wait(pb.Packet.VOICE_PRESENCE_UPDATE, pred=lambda p: "bobby" in p.voice_presence_update.active_users)
    for c in (ann, bob, cid):
        c.flush(0.3)
    # no group yet
    ann.send(pb.Packet.MLS_GROUP_INFO_REQ, mls_group_info_req=pb.MlsGroupInfoReq(channel_id="voice-lounge"))
    r = ann.wait(pb.Packet.MLS_GROUP_INFO_RES)
    check("group info: none before create", r is not None and not r.mls_group_info_res.exists)
    # a non-participant gets nothing
    cid.send(pb.Packet.MLS_GROUP_INFO_REQ, mls_group_info_req=pb.MlsGroupInfoReq(channel_id="voice-lounge"))
    check("non-participant gets no group info", cid.wait(pb.Packet.MLS_GROUP_INFO_RES, timeout=1.0) is None)
    # create (epoch 0)
    ann.send(pb.Packet.MLS_GROUP_CREATE_REQ, mls_group_create_req=pb.MlsGroupCreateReq(channel_id="voice-lounge", group_info=b"GI0"))
    r = ann.wait(pb.Packet.MLS_COMMIT_RES)
    check("create accepted at epoch 0", r is not None and r.mls_commit_res.success and r.mls_commit_res.epoch == 0)
    bob.send(pb.Packet.MLS_GROUP_CREATE_REQ, mls_group_create_req=pb.MlsGroupCreateReq(channel_id="voice-lounge", group_info=b"GI0b"))
    r = bob.wait(pb.Packet.MLS_COMMIT_RES)
    check("second create refused with 'exists'", r is not None and not r.mls_commit_res.success and r.mls_commit_res.message == "exists")
    bob.send(pb.Packet.MLS_GROUP_INFO_REQ, mls_group_info_req=pb.MlsGroupInfoReq(channel_id="voice-lounge"))
    r = bob.wait(pb.Packet.MLS_GROUP_INFO_RES)
    check("group info after create", r is not None and r.mls_group_info_res.exists and r.mls_group_info_res.epoch == 0 and r.mls_group_info_res.group_info == b"GI0")
    # commit epoch rule
    bob.send(pb.Packet.MLS_COMMIT_REQ, mls_commit_req=pb.MlsCommitReq(channel_id="voice-lounge", epoch=2, commit=b"C", group_info=b"GI2"))
    r = bob.wait(pb.Packet.MLS_COMMIT_RES)
    check("stale/skipped epoch refused", r is not None and not r.mls_commit_res.success and r.mls_commit_res.message == "stale_epoch" and r.mls_commit_res.epoch == 0)
    bob.send(pb.Packet.MLS_COMMIT_REQ, mls_commit_req=pb.MlsCommitReq(channel_id="voice-lounge", epoch=1, commit=b"C1", group_info=b"GI1"))
    r = bob.wait(pb.Packet.MLS_COMMIT_RES)
    check("epoch 1 commit accepted", r is not None and r.mls_commit_res.success and r.mls_commit_res.epoch == 1)
    b = ann.wait(pb.Packet.MLS_COMMIT_BROADCAST)
    check("broadcast reaches the other participant with sender + epoch", b is not None and b.mls_commit_broadcast.sender == "bobby" and b.mls_commit_broadcast.epoch == 1 and b.mls_commit_broadcast.commit == b"C1")
    check("committer does not receive its own broadcast", bob.wait(pb.Packet.MLS_COMMIT_BROADCAST, timeout=0.8) is None)
    check("non-participant gets no broadcast", cid.wait(pb.Packet.MLS_COMMIT_BROADCAST, timeout=0.5) is None)
    ann.send(pb.Packet.MLS_GROUP_INFO_REQ, mls_group_info_req=pb.MlsGroupInfoReq(channel_id="voice-lounge"))
    r = ann.wait(pb.Packet.MLS_GROUP_INFO_RES)
    check("group info follows the commit", r is not None and r.mls_group_info_res.epoch == 1 and r.mls_group_info_res.group_info == b"GI1")
    # a commit without group_info keeps the previous GroupInfo
    ann.send(pb.Packet.MLS_COMMIT_REQ, mls_commit_req=pb.MlsCommitReq(channel_id="voice-lounge", epoch=2, commit=b"C2"))
    r = ann.wait(pb.Packet.MLS_COMMIT_RES)
    check("epoch 2 accepted without group info", r is not None and r.mls_commit_res.success and r.mls_commit_res.epoch == 2)
    bob.wait(pb.Packet.MLS_COMMIT_BROADCAST)
    bob.send(pb.Packet.MLS_GROUP_INFO_REQ, mls_group_info_req=pb.MlsGroupInfoReq(channel_id="voice-lounge"))
    r = bob.wait(pb.Packet.MLS_GROUP_INFO_RES)
    check("previous group info retained", r is not None and r.mls_group_info_res.epoch == 2 and r.mls_group_info_res.group_info == b"GI1")
    # sealed UDP types relay like their plain twins
    ua = socket.socket(socket.AF_INET, socket.SOCK_DGRAM); ua.bind((HOST, 0)); ua.settimeout(2)
    ub = socket.socket(socket.AF_INET, socket.SOCK_DGRAM); ub.bind((HOST, 0)); ub.settimeout(2)
    def sealed_audio(jwt_token, seq, payload, ptype=7):
        return bytes([ptype]) + udp_key(jwt_token) + struct.pack("<HH", seq, len(payload)) + payload
    udp_bind(ub, bob.jwt, 8083); udp_bind(ua, ann.jwt, 8083)
    ua.sendto(sealed_audio(ann.jwt, 1, b"sealed-a"), (HOST, 8083))
    try:
        got = ub.recv(2048)
    except socket.timeout:
        got = b""
    check("AUDIO_SEALED relayed with sender rewritten", got[:1] == b"\x07"
          and got[1:33].rstrip(b"\0") == str(uid_for("ann")).encode() and got[-8:] == b"sealed-a")
    # group dropped when the channel empties
    for c in (ann, bob):
        c.send(pb.Packet.LEAVE_VOICE_REQ, leave_voice_req=pb.LeaveVoiceRequest())
    owner.wait(pb.Packet.VOICE_PRESENCE_UPDATE, pred=lambda p: len(p.voice_presence_update.active_users) == 0)
    cid.send(pb.Packet.JOIN_VOICE_REQ, join_voice_req=pb.JoinVoiceRequest(channel_id="voice-lounge"))
    owner.wait(pb.Packet.VOICE_PRESENCE_UPDATE, pred=lambda p: "cid" in p.voice_presence_update.active_users)
    cid.flush(0.3)
    cid.send(pb.Packet.MLS_GROUP_INFO_REQ, mls_group_info_req=pb.MlsGroupInfoReq(channel_id="voice-lounge"))
    r = cid.wait(pb.Packet.MLS_GROUP_INFO_RES)
    check("group dropped once the channel emptied", r is not None and not r.mls_group_info_res.exists)
    for sk in (ua, ub): sk.close()
    for c in (ann, bob, cid, owner): c.close()


def attachment_http(user, method, path, extra_headers="", body=b""):
    """One request against the attachment HTTP endpoint as `user`."""
    sk = raw_tls(8085)
    sk.sendall((f"{method} {path} HTTP/1.1\r\nHost: x\r\n"
                f"Authorization: Bearer {jwt(user)}\r\n{extra_headers}"
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


def test_encrypted_channels():
    """Encrypted text channels: chosen at creation (+ audit), fixed for life,
    wire-format enforcement both ways, envelope through broadcast / history /
    edit / reply preview, key escrow (create / fetch / needs / gap fill / stale
    epoch / viewer gating / CHANGED on join and removal), and the legacy guard:
    a sealed message in a channel switched off before the switch was removed
    can't be edited to plaintext."""
    print("[e2ee] encrypted text channels: creation-time choice, enforcement, envelopes, key escrow")
    owner = Client("alice"); assert auth_ok(owner)[0]
    ed = join("edith", owner); fay = join("fay", owner)
    for c in (owner, ed, fay): c.flush(0.4)
    owner.send(pb.Packet.CHANNEL_CREATE_REQ, channel_create_req=pb.ChannelCreateRequest(
        name="sealed-room", type=pb.ChannelInfo.TEXT, encrypted=True))
    cr = owner.wait(pb.Packet.CHANNEL_ACTION_RES, timeout=2, pred=lambda p: p.channel_action_res.action == "create")
    check("created encrypted", cr is not None and cr.channel_action_res.success and cr.channel_action_res.channel.encrypted)
    enc = cr.channel_action_res.channel.id if cr and cr.channel_action_res.success else "sealed-room"
    check("channels row flagged", sql("select encrypted from channels where id=?", enc) == [(1,)])
    check("audit row", sql("select details from audit_log where action='channel_create' and channel_id=?", enc) == [("encrypted",)])
    lst = channel_list(ed)
    check("members see it encrypted in the channel list", lst is not None and enc in lst and lst[enc].encrypted)
    owner.send(pb.Packet.CHANNEL_CREATE_REQ, channel_create_req=pb.ChannelCreateRequest(
        name="sealed-voice", type=pb.ChannelInfo.VOICE, encrypted=True))
    cr = owner.wait(pb.Packet.CHANNEL_ACTION_RES, timeout=2, pred=lambda p: p.channel_action_res.action == "create")
    check("encrypted is ignored for voice channels", cr is not None and cr.channel_action_res.success and not cr.channel_action_res.channel.encrypted)
    owner.send(pb.Packet.CHANNEL_UPDATE_REQ, channel_update_req=pb.ChannelUpdateRequest(channel_id=enc, slowmode_seconds=0))
    r = owner.wait(pb.Packet.CHANNEL_UPDATE_RES, timeout=2)
    check("a settings update leaves it encrypted", r is not None and r.channel_update_res.success and r.channel_update_res.channel.encrypted)
    for c in (owner, ed, fay): c.flush(0.4)
    # enforcement: plaintext refused in an encrypted channel
    ed.send(pb.Packet.CHANNEL_MSG, channel_msg=pb.ChannelMessage(channel_id=enc, content="plain", nonce="n1"))
    rj = ed.wait(pb.Packet.CHANNEL_MSG_REJECTED, timeout=2, pred=lambda p: p.channel_msg_rejected.nonce == "n1")
    check("plaintext refused in encrypted channel", rj is not None)
    check("plaintext not persisted", sql("select count(*) from messages where content='plain'") == [(0,)])
    # envelope accepted; placeholder content; broadcast + history carry it
    ed.send(pb.Packet.CHANNEL_MSG, channel_msg=pb.ChannelMessage(channel_id=enc, content="PLACEHOLDER", envelope=b"\x02ENV1", nonce="n2"))
    bc = owner.wait(pb.Packet.CHANNEL_MSG, timeout=2, pred=lambda p: p.channel_msg.nonce == "n2")
    check("sealed message broadcast with envelope", bc is not None and bc.channel_msg.envelope == b"\x02ENV1" and bc.channel_msg.content == "PLACEHOLDER")
    mid = bc.channel_msg.id if bc else 0
    check("envelope persisted as blob", sql("select envelope from messages where id=?", mid) == [(b"\x02ENV1",)])
    ed.flush(0.3)
    # reply preview embeds the parent's envelope
    ed.send(pb.Packet.CHANNEL_MSG, channel_msg=pb.ChannelMessage(channel_id=enc, content="PLACEHOLDER", envelope=b"\x02ENV2", nonce="n3", reply_to=mid))
    bc2 = owner.wait(pb.Packet.CHANNEL_MSG, timeout=2, pred=lambda p: p.channel_msg.nonce == "n3")
    check("reply carries the parent's envelope", bc2 is not None and bc2.channel_msg.reply_to == mid and bc2.channel_msg.reply_to_envelope == b"\x02ENV1")
    # history
    fay.flush(0.3)
    fay.send(pb.Packet.CHANNEL_HISTORY_REQ, channel_history_req=pb.ChannelHistoryRequest(channel_id=enc, limit=50))
    h = fay.wait(pb.Packet.CHANNEL_HISTORY_RES, timeout=2)
    row = next((m for m in h.channel_history_res.messages if m.id == mid), None) if h else None
    check("history returns the envelope", row is not None and row.envelope == b"\x02ENV1")
    # edit: plaintext edit refused, sealed edit accepted + broadcast
    ed.send(pb.Packet.MESSAGE_EDIT_REQ, message_edit_req=pb.MessageEditReq(channel_id=enc, message_id=mid, content="plain-edit"))
    r = ed.wait(pb.Packet.MESSAGE_EDIT_RES, timeout=2)
    check("plaintext edit refused in encrypted channel", r is not None and not r.message_edit_res.success)
    ed.send(pb.Packet.MESSAGE_EDIT_REQ, message_edit_req=pb.MessageEditReq(channel_id=enc, message_id=mid, content="PLACEHOLDER", envelope=b"\x02ENV1b"))
    r = ed.wait(pb.Packet.MESSAGE_EDIT_RES, timeout=2)
    check("sealed edit accepted", r is not None and r.message_edit_res.success, r.message_edit_res.message if r else None)
    e = owner.wait(pb.Packet.CHANNEL_MESSAGE_EDITED, timeout=2, pred=lambda p: p.channel_message_edited.message_id == mid)
    check("edit broadcast carries the envelope", e is not None and e.channel_message_edited.envelope == b"\x02ENV1b")
    # --- attachments: sealed uploads only, in an encrypted channel ---
    def upload(user, channel, init):
        head, body = attachment_http(user, "POST", "/attachments/init", "Content-Type: application/json\r\n",
                                     json.dumps(dict(init, channelId=channel)).encode())
        if not head.startswith(b"HTTP/1.1 201"):
            return head, None
        aid = json.loads(body.decode() or "{}").get("id")
        attachment_http(user, "PATCH", f"/attachments/{aid}", "Upload-Offset: 0\r\n", b"ab")
        attachment_http(user, "POST", f"/attachments/{aid}/complete")
        return head, aid
    plain = {"filename": "p.bin", "mime": "application/octet-stream", "size": 2}
    sealed = {"filename": "encrypted", "mime": "image/png", "size": 2, "encrypted": True, "kind": 0,
              "width": 640, "height": 480, "durationMs": 7, "placeholder": "zzz"}
    _, plain_aid = upload("edith", enc, plain)
    head, enc_aid = upload("edith", enc, sealed)
    check("sealed upload init accepted", enc_aid is not None, head[:40])
    check("sealed row keeps the declared kind and no metadata",
          sql("select encrypted, kind, width, height, duration_ms, coalesce(placeholder,''), mime from attachments where id=?", enc_aid)
          == [(1, 0, 0, 0, 0, "", "application/octet-stream")])
    head, _ = attachment_http("edith", "POST", "/attachments/init", "Content-Type: application/json\r\n",
                              json.dumps(dict(sealed, channelId=enc, kind=9)).encode())
    check("sealed upload needs a valid kind", head.startswith(b"HTTP/1.1 400"), head[:40])
    ed.send(pb.Packet.CHANNEL_MSG, channel_msg=pb.ChannelMessage(channel_id=enc, content="PLACEHOLDER", envelope=b"\x02ENV3", nonce="n4",
                                                                 attachments=[pb.Attachment(id=plain_aid), pb.Attachment(id=enc_aid)]))
    bc3 = ed.wait(pb.Packet.CHANNEL_MSG, timeout=2, pred=lambda p: p.channel_msg.nonce == "n4")
    check("only the sealed upload binds in an encrypted channel", bc3 is not None and [a.id for a in bc3.channel_msg.attachments] == [enc_aid])
    check("bound attachment flagged encrypted on the wire", bc3 is not None and len(bc3.channel_msg.attachments) == 1
          and bc3.channel_msg.attachments[0].encrypted and bc3.channel_msg.attachments[0].kind == 0)
    check("plain upload left unbound", sql("select message_id from attachments where id=?", plain_aid) == [(0,)])
    mid4 = bc3.channel_msg.id if bc3 else 0
    ed.flush(0.3)
    ed.send(pb.Packet.CHANNEL_HISTORY_REQ, channel_history_req=pb.ChannelHistoryRequest(channel_id=enc, limit=50))
    h = ed.wait(pb.Packet.CHANNEL_HISTORY_RES, timeout=2)
    hrow = next((m for m in h.channel_history_res.messages if m.id == mid4), None) if h else None
    check("history carries the encrypted flag", hrow is not None and len(hrow.attachments) == 1 and hrow.attachments[0].encrypted)
    # envelope refused in a plaintext channel (a fresh one — earlier tests
    # reshape the seeded channels)
    owner.send(pb.Packet.CHANNEL_CREATE_REQ, channel_create_req=pb.ChannelCreateRequest(name="plain-e2ee", type=pb.ChannelInfo.TEXT))
    cr = owner.wait(pb.Packet.CHANNEL_ACTION_RES, timeout=2, pred=lambda p: p.channel_action_res.action == "create")
    plain_id = cr.channel_action_res.channel.id if cr and cr.channel_action_res.success else "plain-e2ee"
    ed.flush(0.3)
    ed.send(pb.Packet.CHANNEL_MSG, channel_msg=pb.ChannelMessage(channel_id=plain_id, content="PLACEHOLDER", envelope=b"\x02ENVX", nonce="n5"))
    rj = ed.wait(pb.Packet.CHANNEL_MSG_REJECTED, timeout=2, pred=lambda p: p.channel_msg_rejected.nonce == "n5")
    check("envelope refused in plaintext channel", rj is not None)
    # ...and a sealed upload doesn't bind there either
    _, enc2 = upload("edith", plain_id, dict(sealed, kind=2))
    ed.send(pb.Packet.CHANNEL_MSG, channel_msg=pb.ChannelMessage(channel_id=plain_id, content="with file", nonce="n6", attachments=[pb.Attachment(id=enc2)]))
    bc4 = ed.wait(pb.Packet.CHANNEL_MSG, timeout=2, pred=lambda p: p.channel_msg.nonce == "n6")
    check("sealed upload doesn't bind in a plaintext channel", bc4 is not None and len(bc4.channel_msg.attachments) == 0)
    for c in (owner, ed, fay): c.flush(0.3)

    # --- key escrow ---
    ed.send(pb.Packet.CHANNEL_KEYS_REQ, channel_keys_req=pb.ChannelKeysReq(channel_id=enc))
    k = ed.wait(pb.Packet.CHANNEL_KEYS_RES, timeout=2)
    check("keys: no epoch yet, members listed", k is not None and k.channel_keys_res.encrypted and k.channel_keys_res.current_epoch == 0 and set(k.channel_keys_res.members) >= {"alice", "edith", "fay"})
    # gap-fill before any epoch is a stale epoch
    ed.send(pb.Packet.CHANNEL_KEYS_PUBLISH_REQ, channel_keys_publish_req=pb.ChannelKeysPublishReq(channel_id=enc, epoch=2, blobs=[pb.ChannelKeyBlob(recipient="fay", recipient_uid=uid_of("fay"), blob=b"B")]))
    r = ed.wait(pb.Packet.CHANNEL_KEYS_PUBLISH_RES, timeout=2)
    check("skipping an epoch is refused", r is not None and not r.channel_keys_publish_res.success and r.channel_keys_publish_res.message == "stale_epoch")
    # edith creates epoch 1 for herself + alice (not fay), plus a non-viewer that must be dropped
    KC = b"K" * 32   # a key check (opaque to the server: 32 bytes)
    ed.send(pb.Packet.CHANNEL_KEYS_PUBLISH_REQ, channel_keys_publish_req=pb.ChannelKeysPublishReq(channel_id=enc, epoch=1, key_check=KC, blobs=[
        pb.ChannelKeyBlob(recipient="edith", recipient_uid=uid_of("edith"), blob=b"E1-edith"), pb.ChannelKeyBlob(recipient="alice", recipient_uid=uid_of("alice"), blob=b"E1-alice"), pb.ChannelKeyBlob(recipient="nobody", recipient_uid=uid_of("nobody"), blob=b"E1-x")]))
    r = ed.wait(pb.Packet.CHANNEL_KEYS_PUBLISH_RES, timeout=2)
    check("epoch 1 created", r is not None and r.channel_keys_publish_res.success and r.channel_keys_publish_res.current_epoch == 1)
    ch = owner.wait(pb.Packet.CHANNEL_KEYS_CHANGED, timeout=2, pred=lambda p: p.channel_keys_changed.channel_id == enc)
    check("CHANNEL_KEYS_CHANGED names an online holder as filler", ch is not None and ch.channel_keys_changed.current_epoch == 1 and ch.channel_keys_changed.filler in ("alice", "edith") and not ch.channel_keys_changed.rotate)
    check("blob for a non-viewer dropped", sql("select count(*) from channel_key_blobs where recipient='nobody'") == [(0,)])
    owner.send(pb.Packet.CHANNEL_KEYS_REQ, channel_keys_req=pb.ChannelKeysReq(channel_id=enc))
    k = owner.wait(pb.Packet.CHANNEL_KEYS_RES, timeout=2)
    mine = [(b.epoch, b.sender, b.blob) for b in k.channel_keys_res.blobs] if k else []
    needs = {n.username: list(n.epochs) for n in k.channel_keys_res.needs} if k else {}
    check("owner gets her blob with the sealer stamped", mine == [(1, "edith", b"E1-alice")])
    check("needs lists fay for epoch 1", needs.get("fay") == [1] and "alice" not in needs)
    def publish(c, channel, epoch, blobs, key_check=KC):
        c.send(pb.Packet.CHANNEL_KEYS_PUBLISH_REQ, channel_keys_publish_req=pb.ChannelKeysPublishReq(channel_id=channel, epoch=epoch, key_check=key_check, blobs=[
            pb.ChannelKeyBlob(recipient=u, recipient_uid=uid_of(u), blob=b) for u, b in blobs]))
        return c.wait(pb.Packet.CHANNEL_KEYS_PUBLISH_RES, timeout=2)
    def refused(r, message):
        return r is not None and not r.channel_keys_publish_res.success and r.channel_keys_publish_res.message == message
    # gap fill: alice (a holder) seals epoch 1 for fay; a second fill for fay doesn't replace it
    r = publish(owner, enc, 1, [("fay", b"E1-fay")])
    check("gap fill accepted at the current epoch", r is not None and r.channel_keys_publish_res.success and r.channel_keys_publish_res.current_epoch == 1)
    r = publish(owner, enc, 1, [("fay", b"OVERWRITE")])
    check("existing blob never overwritten", r is not None and r.channel_keys_publish_res.success
          and sql("select blob from channel_key_blobs where recipient='fay' and epoch=1") == [(b"E1-fay",)])
    fay.flush(0.3)
    fay.send(pb.Packet.CHANNEL_KEYS_REQ, channel_keys_req=pb.ChannelKeysReq(channel_id=enc))
    k = fay.wait(pb.Packet.CHANNEL_KEYS_RES, timeout=2)
    # (the shared test server carries members from earlier tests who lack
    # blobs, so `needs` isn't empty — only fay's entry must be gone)
    check("fay now holds epoch 1 and is no longer missing", k is not None and [(b.epoch, b.blob) for b in k.channel_keys_res.blobs] == [(1, b"E1-fay")] and all(n.username != "fay" for n in k.channel_keys_res.needs))
    # a new member joining triggers CHANGED for a filler
    for c in (owner, ed, fay): c.flush(0.3)
    gus = join("gus", owner)
    ch = owner.wait(pb.Packet.CHANNEL_KEYS_CHANGED, timeout=3, pred=lambda p: p.channel_keys_changed.channel_id == enc)
    check("new member → CHANGED with a filler, no rotation", ch is not None and not ch.channel_keys_changed.rotate and ch.channel_keys_changed.filler in ("alice", "edith", "fay"))
    # review S14: fills come only from holders of the epoch...
    gus.flush(0.3)
    r = publish(gus, enc, 1, [("edith", b"GARBAGE")])
    check("keys: a fill from a non-holder is refused", refused(r, "not_a_holder"))
    # ...and never seal to their publisher: that's a creation that lost the race (a different key)
    for c in (owner, ed, fay): c.flush(0.3)
    r = publish(ed, enc, 1, [("edith", b"LOSER"), ("gus", b"LOSER-gus")])
    check("keys: a lost creation race is refused, not stored as a fill", refused(r, "stale_epoch")
          and sql("select count(*) from channel_key_blobs where recipient_uid=?", uid_of("gus")) == [(0,)])
    r = publish(ed, enc, 1, [("gus", b"E1-gus")])
    check("keys: a holder fills the newcomer", r is not None and r.channel_keys_publish_res.success)
    # a viewer can't mint epochs at will: a new one needs a rotation to be due
    r = publish(ed, enc, 2, [("edith", b"E2-early")])
    check("keys: no new epoch without a due rotation", refused(r, "no_rotation_due")
          and sql("select max(epoch) from channel_key_epochs where channel_id=?", enc) == [(1,)])
    # escrow only in encrypted channels
    r = publish(ed, plain_id, 1, [("edith", b"P1")])
    check("keys: refused in a plaintext channel", r is not None and not r.channel_keys_publish_res.success
          and sql("select count(*) from channel_key_epochs where channel_id=?", plain_id) == [(0,)])
    # removal (leave) → blobs deleted + rotation requested
    for c in (owner, ed, fay, gus): c.flush(0.3)
    fay.send(pb.Packet.LEAVE_SERVER_REQ, leave_server_req=pb.LeaveServerRequest())
    ch = owner.wait(pb.Packet.CHANNEL_KEYS_CHANGED, timeout=3, pred=lambda p: p.channel_keys_changed.channel_id == enc and p.channel_keys_changed.rotate)
    check("removal → CHANGED with rotate", ch is not None and ch.channel_keys_changed.filler in ("alice", "edith", "gus"))
    check("removed member's blobs deleted", sql("select count(*) from channel_key_blobs where recipient='fay'") == [(0,)])
    # the due rotation: only a holder of the current epoch may mint it, once, in one transaction
    hal = join("hal", owner); hal.flush(0.3)
    r = publish(hal, enc, 2, [("hal", b"E2-hal")])
    check("keys: a non-holder can't rotate", refused(r, "not_a_holder"))
    r = publish(ed, enc, 2, [("edith", b"E2-edith"), ("alice", b"E2-alice"), ("gus", b"E2-gus")])
    check("keys: the due rotation by a holder is accepted", r is not None and r.channel_keys_publish_res.success
          and r.channel_keys_publish_res.current_epoch == 2
          and sql("select count(*) from channel_key_blobs where channel_id=? and epoch=2", enc) == [(3,)])
    r = publish(ed, enc, 3, [("edith", b"E3-edith")])
    check("keys: a rotation is spent once done", refused(r, "no_rotation_due"))
    hal.close()

    # --- review S14: key checks, members dropping blobs they can't use,
    # refused senders, orphaned epochs, paged replies ---
    def keys_res(c, channel, **cursors):
        c.send(pb.Packet.CHANNEL_KEYS_REQ, channel_keys_req=pb.ChannelKeysReq(channel_id=channel, **cursors))
        r = c.wait(pb.Packet.CHANNEL_KEYS_RES, timeout=2, pred=lambda p: p.channel_keys_res.channel_id == channel
                   and p.channel_keys_res.after_epoch == cursors.get("after_epoch", 0)
                   and p.channel_keys_res.needs_after_uid == cursors.get("needs_after_uid", 0))
        return r.channel_keys_res if r else None
    def all_needs(c, channel):
        # Walks the needs pages: {uid: epochs}, or None when a page is missing.
        out, after = {}, 0
        for _ in range(50):
            r = keys_res(c, channel, **({"needs_after_uid": after} if after else {}))
            if r is None:
                return None
            out.update({n.uid: list(n.epochs) for n in r.needs})
            if not r.more_needs or not r.needs:
                return out
            after = r.needs[-1].uid
        return None
    def reject(c, channel, epochs, bad=False):
        c.send(pb.Packet.CHANNEL_KEYS_REJECT, channel_keys_reject=pb.ChannelKeysReject(channel_id=channel, epochs=epochs, bad=bad))
    def changed(channel, timeout=3):
        p = owner.wait(pb.Packet.CHANNEL_KEYS_CHANGED, timeout=timeout, pred=lambda p: p.channel_keys_changed.channel_id == channel)
        return p.channel_keys_changed if p else None
    def gus_blobs(epoch):
        return sql("select blob from channel_key_blobs where channel_id=? and epoch=? and recipient_uid=?", enc, epoch, uid_of("gus"))
    for c in (owner, ed, gus): c.flush(0.3)
    k = keys_res(owner, enc)
    check("keys: blobs carry their epoch's key check, cursors echoed", k is not None
          and [(b.epoch, b.key_check) for b in k.blobs] == [(1, KC), (2, KC)]
          and not k.more_blobs and not k.current_orphaned and set(k.members) >= {"alice", "edith", "gus"})
    # a blob sealed to a key the member no longer has: dropped, no refusal
    reject(gus, enc, [2])
    ch = changed(enc)
    check("keys: a dropped blob asks a filler, never the member who dropped it",
          ch is not None and not ch.rotate and ch.filler_uid in (uid_of("alice"), uid_of("edith")))
    check("...only that blob goes, and a stale one refuses nobody", gus_blobs(2) == [] and gus_blobs(1) == [(b"E1-gus",)]
          and sql("select count(*) from channel_key_refusals where channel_id=?", enc) == [(0,)])
    # a bad blob: its sender may not seal to that member again
    r = publish(owner, enc, 2, [("gus", b"E2-GARBAGE")])
    check("keys: a holder seals the dropped epoch again", r is not None and r.channel_keys_publish_res.success and gus_blobs(2) == [(b"E2-GARBAGE",)])
    for c in (owner, ed, gus): c.flush(0.3)
    reject(gus, enc, [2], bad=True)
    ch = changed(enc)
    check("keys: a bad blob's sender is refused and not named filler for them",
          ch is not None and ch.filler_uid == uid_of("edith")
          and sql("select sender_uid from channel_key_refusals where channel_id=? and recipient_uid=?", enc, uid_of("gus")) == [(uid_of("alice"),)])
    r = publish(owner, enc, 2, [("gus", b"E2-AGAIN")])
    check("...their blobs to that member are dropped", r is not None and r.channel_keys_publish_res.success and gus_blobs(2) == [])
    n_owner = all_needs(owner, enc); n_ed = all_needs(ed, enc)
    check("...and that member isn't one of their needs, but is another holder's",
          n_owner is not None and uid_of("gus") not in n_owner and n_ed is not None and n_ed.get(uid_of("gus")) == [2])
    check("needs never list the caller", n_owner is not None and uid_of("alice") not in n_owner)
    r = publish(ed, enc, 2, [("gus", b"E2-gus")])
    check("keys: another holder fills them", r is not None and r.channel_keys_publish_res.success and gus_blobs(2) == [(b"E2-gus",)])
    for c in (owner, ed, gus): c.flush(0.3)
    reject(ed, enc, [77])
    check("keys: dropping a blob you don't have changes nothing", changed(enc, timeout=1) is None)

    # orphaned epochs: when nobody holds the current one, any viewer mints the next
    owner.send(pb.Packet.CHANNEL_CREATE_REQ, channel_create_req=pb.ChannelCreateRequest(name="sealed-orphan", type=pb.ChannelInfo.TEXT, encrypted=True))
    cr = owner.wait(pb.Packet.CHANNEL_ACTION_RES, timeout=2, pred=lambda p: p.channel_action_res.action == "create")
    orph = cr.channel_action_res.channel.id if cr and cr.channel_action_res.success else "sealed-orphan"
    for c in (owner, ed, gus): c.flush(0.4)
    r = publish(ed, orph, 1, [("edith", b"O1-edith")], key_check=b"")
    check("keys: a new epoch needs its key check", refused(r, "A new epoch needs its key check."))
    r = publish(ed, orph, 1, [("edith", b"O1-edith")], key_check=b"short")
    check("...of 32 bytes", refused(r, "A new epoch needs its key check."))
    r = publish(ed, orph, 1, [("edith", b"O1-edith")])
    check("keys: the check is stored with the epoch", r is not None and r.channel_keys_publish_res.success
          and sql("select key_check from channel_key_epochs where channel_id=? and epoch=1", orph) == [(KC,)])
    r = publish(gus, orph, 2, [("gus", b"O2-gus")])
    check("keys: a held epoch isn't replaced", refused(r, "no_rotation_due"))
    reject(ed, orph, [1], bad=True)   # her own blob: refuses nobody
    k = keys_res(ed, orph)
    check("keys: the last holder dropping it orphans the epoch", k is not None and k.current_epoch == 1 and k.current_orphaned
          and len(k.blobs) == 0 and sql("select count(*) from channel_key_refusals where channel_id=?", orph) == [(0,)])
    r = publish(gus, orph, 2, [("gus", b"O2-gus"), ("edith", b"O2-edith")])
    check("keys: then any viewer may mint the next one", r is not None and r.channel_keys_publish_res.success
          and r.channel_keys_publish_res.current_epoch == 2)
    r = publish(gus, orph, 3, [("gus", b"O3-gus")])
    check("...once", refused(r, "no_rotation_due"))

    # paging: 5000 epochs, alice holds every one
    owner.send(pb.Packet.CHANNEL_CREATE_REQ, channel_create_req=pb.ChannelCreateRequest(name="sealed-pages", type=pb.ChannelInfo.TEXT, encrypted=True))
    cr = owner.wait(pb.Packet.CHANNEL_ACTION_RES, timeout=2, pred=lambda p: p.channel_action_res.action == "create")
    pages = cr.channel_action_res.channel.id if cr and cr.channel_action_res.success else "sealed-pages"
    N = 5000
    sql("with recursive n(i) as (select 1 union all select i + 1 from n where i < ?) "
        "insert into channel_key_epochs(channel_id, epoch, created_by_uid, created_by, created_at, key_check) "
        "select ?, i, ?, 'edith', 0, ? from n", N, pages, uid_of("edith"), KC)
    sql("with recursive n(i) as (select 1 union all select i + 1 from n where i < ?) "
        "insert into channel_key_blobs(channel_id, epoch, recipient_uid, recipient, sender_uid, sender, blob, created_at) "
        "select ?, i, ?, 'alice', ?, 'edith', x'01', 0 from n", N, pages, uid_of("alice"), uid_of("edith"))
    for c in (owner, ed, gus): c.flush(0.4)
    p1 = keys_res(owner, pages)
    check("keys: a first page holds 256 blobs, the viewers and one need capped at 4096 epochs",
          p1 is not None and p1.current_epoch == N and [b.epoch for b in p1.blobs] == list(range(1, 257)) and p1.more_blobs
          and len(p1.members) > 0 and len(p1.needs) == 1 and len(p1.needs[0].epochs) == 4096 and p1.more_needs,
          (len(p1.blobs), len(p1.needs), p1.more_needs) if p1 else None)
    p2 = keys_res(owner, pages, after_epoch=256)
    check("keys: a blob page holds the next 256 and nothing else", p2 is not None and [b.epoch for b in p2.blobs] == list(range(257, 513))
          and p2.more_blobs and not p2.members and not p2.needs)
    p3 = keys_res(owner, pages, after_epoch=N - 10)
    check("keys: the last blob page says so", p3 is not None and [b.epoch for b in p3.blobs] == list(range(N - 9, N + 1)) and not p3.more_blobs)
    first = p1.needs[0].uid if p1 and p1.needs else 0
    p4 = keys_res(owner, pages, needs_after_uid=first)
    check("keys: a needs page holds the next viewer by uid and nothing else", p4 is not None and len(p4.needs) == 1
          and p4.needs[0].uid > first and not p4.blobs and not p4.members)
    check("keys: pages stay far below the 2 MiB frame cap",
          all(p is not None and p.ByteSize() < 512 * 1024 for p in (p1, p2, p4)), [p.ByteSize() for p in (p1, p2, p4) if p])
    # escrow goes with a deleted channel, refusals included
    sql("insert into channel_key_refusals(channel_id, recipient_uid, sender_uid, created_at) values(?, ?, ?, 0)", pages, uid_of("gus"), uid_of("alice"))
    for ch_id in (pages, orph):
        owner.send(pb.Packet.CHANNEL_DELETE_REQ, channel_delete_req=pb.ChannelDeleteRequest(channel_id=ch_id))
        owner.wait(pb.Packet.CHANNEL_ACTION_RES, timeout=3, pred=lambda p: p.channel_action_res.action == "delete")
    check("keys: deleting a channel drops its epochs, blobs and refusals",
          all(sql(f"select count(*) from {t} where channel_id in (?, ?)", pages, orph) == [(0,)]
              for t in ("channel_key_epochs", "channel_key_blobs", "channel_key_refusals")))

    # a rotation owed while no holder is online is asked for when one signs in
    owner.send(pb.Packet.CHANNEL_CREATE_REQ, channel_create_req=pb.ChannelCreateRequest(name="sealed-rot", type=pb.ChannelInfo.TEXT, encrypted=True))
    cr = owner.wait(pb.Packet.CHANNEL_ACTION_RES, timeout=2, pred=lambda p: p.channel_action_res.action == "create")
    rot = cr.channel_action_res.channel.id if cr and cr.channel_action_res.success else "sealed-rot"
    rhea = join("rhea", owner); rhea.flush(0.4)
    r = publish(rhea, rot, 1, [("rhea", b"R1-rhea")])
    check("keys: rhea alone holds the new channel's epoch", r is not None and r.channel_keys_publish_res.success)
    rhea.close()
    for c in (owner, ed, gus): c.flush(0.4)
    owner.send(pb.Packet.KICK_MEMBER_REQ, kick_member_req=pb.KickMemberRequest(username="hal", uid=uid_of("hal"), reason=""))
    owner.wait(pb.Packet.MOD_ACTION_RES, timeout=3, pred=lambda p: p.mod_action_res.action == "kick")
    ch = owner.wait(pb.Packet.CHANNEL_KEYS_CHANGED, timeout=1.5, pred=lambda p: p.channel_keys_changed.channel_id == rot)
    check("keys: a removal with no holder online asks nobody, but the rotation is kept",
          ch is None and sql("select count(*) from channel_key_rotations where channel_id=?", rot) == [(1,)])
    rhea = Client("rhea"); check("rhea back", auth_ok(rhea)[0])
    ch = owner.wait(pb.Packet.CHANNEL_KEYS_CHANGED, timeout=3, pred=lambda p: p.channel_keys_changed.channel_id == rot and p.channel_keys_changed.rotate)
    check("keys: the owed rotation goes to the holder who signs in", ch is not None and ch.channel_keys_changed.filler_uid == uid_of("rhea"))
    rhea.flush(0.3)
    r = publish(rhea, rot, 2, [("rhea", b"R2-rhea")])
    check("keys: they rotate, and nothing is owed after", r is not None and r.channel_keys_publish_res.success
          and sql("select count(*) from channel_key_rotations where channel_id=?", rot) == [(0,)])
    rhea.close()
    owner.send(pb.Packet.CHANNEL_DELETE_REQ, channel_delete_req=pb.ChannelDeleteRequest(channel_id=rot))
    owner.wait(pb.Packet.CHANNEL_ACTION_RES, timeout=3, pred=lambda p: p.channel_action_res.action == "delete")

    # a refusal steers, it never strands: a refused sender who is the only
    # holder may still seal to that member (an app bug flagging honest blobs
    # must not lock them out of a two-member channel for good)
    owner.send(pb.Packet.CHANNEL_CREATE_REQ, channel_create_req=pb.ChannelCreateRequest(name="sealed-solo", type=pb.ChannelInfo.TEXT, encrypted=True))
    cr = owner.wait(pb.Packet.CHANNEL_ACTION_RES, timeout=2, pred=lambda p: p.channel_action_res.action == "create")
    solo = cr.channel_action_res.channel.id if cr and cr.channel_action_res.success else "sealed-solo"
    # the refused sender has the lower uid, so preferring the other holder shows
    (f, fc), (o, oc) = sorted([("alice", owner), ("edith", ed)], key=lambda t: uid_of(t[0]))
    def solo_gus():
        return sql("select blob from channel_key_blobs where channel_id=? and epoch=1 and recipient_uid=?", solo, uid_of("gus"))
    for c in (owner, ed, gus): c.flush(0.4)
    r = publish(fc, solo, 1, [(f, b"S1-" + f.encode()), ("gus", b"S1-GARBAGE")])
    check("keys: one member holds the epoch, another has a bad blob", r is not None and r.channel_keys_publish_res.success)
    for c in (owner, ed, gus): c.flush(0.3)
    reject(gus, solo, [1], bad=True)
    ch = changed(solo)
    check("keys: the refused sender is still named filler when nobody else holds it",
          ch is not None and ch.filler_uid == uid_of(f)
          and sql("select sender_uid from channel_key_refusals where channel_id=? and recipient_uid=?", solo, uid_of("gus")) == [(uid_of(f),)])
    nf = all_needs(fc, solo)
    check("...still sees them as a need", nf is not None and nf.get(uid_of("gus")) == [1])
    r = publish(fc, solo, 1, [("gus", b"S1-gus")])
    check("...and may seal to them", r is not None and r.channel_keys_publish_res.success and solo_gus() == [(b"S1-gus",)])
    r = publish(fc, solo, 1, [(o, b"S1-" + o.encode())])
    check("keys: a second holder", r is not None and r.channel_keys_publish_res.success)
    for c in (owner, ed, gus): c.flush(0.3)
    reject(gus, solo, [1], bad=True)
    ch = changed(solo)
    check("keys: with another holder the refusal applies again: that one is named",
          ch is not None and ch.filler_uid == uid_of(o))
    nf = all_needs(fc, solo)
    r = publish(fc, solo, 1, [("gus", b"S1-again")])
    check("...the refused sender's blobs to them drop and they're not its need",
          r is not None and r.channel_keys_publish_res.success and solo_gus() == []
          and nf is not None and uid_of("gus") not in nf)
    r = publish(oc, solo, 1, [("gus", b"S1-gus-2")])
    check("...and the other holder fills them", r is not None and r.channel_keys_publish_res.success and solo_gus() == [(b"S1-gus-2",)])
    owner.send(pb.Packet.CHANNEL_DELETE_REQ, channel_delete_req=pb.ChannelDeleteRequest(channel_id=solo))
    owner.wait(pb.Packet.CHANNEL_ACTION_RES, timeout=3, pred=lambda p: p.channel_action_res.action == "delete")
    # A sender renamed since sending: a v1 edit (0x02 binds the name it was
    # sealed under) is refused; a v2 edit (0x04 binds the uid) is accepted.
    ed2 = Client("edith2", uid=uid_for("edith")); check("renamed sender admitted", auth_ok(ed2)[0])
    ed2.flush(0.3)
    ed2.send(pb.Packet.MESSAGE_EDIT_REQ, message_edit_req=pb.MessageEditReq(channel_id=enc, message_id=mid, content="PLACEHOLDER", envelope=b"\x02ENV1c"))
    r = ed2.wait(pb.Packet.MESSAGE_EDIT_RES, timeout=2)
    check("renamed: a v1 edit is refused", r is not None and not r.message_edit_res.success
          and "previous username" in r.message_edit_res.message, r.message_edit_res.message if r else None)
    ed2.send(pb.Packet.MESSAGE_EDIT_REQ, message_edit_req=pb.MessageEditReq(channel_id=enc, message_id=mid, content="PLACEHOLDER", envelope=b"\x04ENV1d"))
    r = ed2.wait(pb.Packet.MESSAGE_EDIT_RES, timeout=2)
    check("renamed: a v2 edit is accepted", r is not None and r.message_edit_res.success, r.message_edit_res.message if r else None)
    ed2.close()
    back = Client("edith"); auth_ok(back); back.close()   # the name back, for the checks below
    # Legacy: a channel switched off back when the switch existed. Its sealed
    # rows can't be edited to plaintext (that would publish the text and drop
    # the attachment keys in the envelope); new plaintext still flows.
    sql("update channels set encrypted=0 where id=?", enc)
    owner.flush(0.3); ed.flush(0.3)
    ed.send(pb.Packet.MESSAGE_EDIT_REQ, message_edit_req=pb.MessageEditReq(channel_id=enc, message_id=mid, content="now public"))
    r = ed.wait(pb.Packet.MESSAGE_EDIT_RES, timeout=2)
    check("legacy: plaintext edit of a sealed message refused", r is not None and not r.message_edit_res.success
          and "encrypted" in r.message_edit_res.message, r.message_edit_res.message if r else None)
    check("legacy: the sealed row is untouched", sql("select envelope, content from messages where id=?", mid) == [(b"\x04ENV1d", "PLACEHOLDER")])
    ed.send(pb.Packet.CHANNEL_MSG, channel_msg=pb.ChannelMessage(channel_id=enc, content="plain now", nonce="n7"))
    bc = owner.wait(pb.Packet.CHANNEL_MSG, timeout=2, pred=lambda p: p.channel_msg.nonce == "n7")
    pmid = bc.channel_msg.id if bc else 0
    ed.flush(0.3)
    ed.send(pb.Packet.MESSAGE_EDIT_REQ, message_edit_req=pb.MessageEditReq(channel_id=enc, message_id=pmid, content="plain edited"))
    r = ed.wait(pb.Packet.MESSAGE_EDIT_RES, timeout=2)
    check("legacy: plaintext messages there still edit", r is not None and r.message_edit_res.success)
    for c in (owner, ed, gus): c.close()
    fay.close()


def test_http_keepalive_and_fts():
    print("[http] keep-alive on the attachment listener; the old FTS triggers stay gone")
    check("old per-row FTS triggers gone", sql("select count(*) from sqlite_master where type='trigger' and name like 'messages_a%'") == [(0,)])
    check("search index present (v14)", sql("select count(*) from sqlite_master where name='messages_fts'") == [(1,)])
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
    c.s = ctx.wrap_socket(connect(8082)); c.username = "alice"; c.jwt = forged; c.buf = b""; c.inbox = []; c.closed = False
    c.send(pb.Packet.COMMUNITY_AUTH_REQ, community_auth_req=pb.CommunityAuthRequest(jwt_token=forged))
    ok, r = auth_ok(c)
    check("token signed by another key rejected", not ok and r is not None and r.community_auth_res.error_code == "auth"); c.close()
    # An HS256 token (old scheme) with the community secret as key must be rejected too.
    h2 = b64(json.dumps({"alg": "HS256", "typ": "JWT"}).encode())
    sig2 = hmac.new(SECRET.encode(), f"{h2}.{p}".encode(), hashlib.sha256).digest()
    hs = f"{h2}.{p}.{b64(sig2)}"
    c = Client.__new__(Client)
    c.s = ctx.wrap_socket(connect(8082)); c.username = "alice"; c.jwt = hs; c.buf = b""; c.inbox = []; c.closed = False
    c.send(pb.Packet.COMMUNITY_AUTH_REQ, community_auth_req=pb.CommunityAuthRequest(jwt_token=hs))
    ok, r = auth_ok(c)
    check("HS256 token rejected (no symmetric fallback)", not ok); c.close()
    # uid-keyed bans: ban "tgus" (uid 4242) by uid; "tgus_renamed" with the same uid is still banned.
    owner = Client("alice"); assert auth_ok(owner)[0]
    code = make_invite(owner)
    gus = Client("tgus", invite=code, uid=4242); assert auth_ok(gus)[0]
    check("member row keyed by uid", sql("select uid from members where username='tgus'") == [(4242,)])
    owner.flush(0.5)
    owner.send(pb.Packet.BAN_MEMBER_REQ, ban_member_req=pb.BanMemberRequest(uid=4242, reason="bye"))
    r = owner.wait(pb.Packet.MOD_ACTION_RES, pred=lambda p: p.mod_action_res.action == "ban")
    check("ban by uid", r is not None and r.mod_action_res.success and r.mod_action_res.uid == 4242
          and r.mod_action_res.username == "tgus")
    gus.close()
    code = make_invite(owner)
    c = Client("tgus_renamed", invite=code, uid=4242); ok, r = auth_ok(c)
    check("renamed account with the banned uid is still banned", not ok and r.community_auth_res.error_code == "banned"); c.close()
    code = make_invite(owner)
    c = Client("tgus_renamed", invite=code, uid=4243); ok, r = auth_ok(c)
    check("different uid with the new name is admitted", ok); c.close()
    owner.send(pb.Packet.UNBAN_MEMBER_REQ, unban_member_req=pb.UnbanMemberRequest(uid=4242))
    r = owner.wait(pb.Packet.MOD_ACTION_RES, pred=lambda p: p.mod_action_res.action == "unban")
    check("unban by uid", r is not None and r.mod_action_res.success)
    owner.close()



def test_v15_uid_migration():
    """Users by uid (docs/superpowers/specs/2026-10-15-uid-identity-design.md):
    a v14 database the previous build filled — plus rows from before uids
    reached the server — migrates to uid keys; legacy names get
    placeholders that their first token claims."""
    print("[v15] users by uid: migrating a v14 database")
    old = start_server(fresh=True, binary=OLD_SERVER_BIN)
    try:
        owner = Client("alice"); assert auth_ok(owner)[0]
        bob = join("bob", owner); carol = join("carol", owner); frank = join("frank", owner)
        for c in (owner, bob, carol, frank): c.flush(0.4)
        owner.send(pb.Packet.ROLE_CREATE_REQ, role_create_req=pb.RoleCreateRequest(name="Mods", color=0, permissions=0))
        mods = owner.wait(pb.Packet.ROLE_ACTION_RES, pred=lambda p: p.role_action_res.action == "create").role_action_res.role
        owner.send(pb.Packet.MEMBER_ROLES_UPDATE_REQ, member_roles_update_req=pb.MemberRolesUpdateRequest(username="bob", uid=uid_of("bob"), role_ids=[mods.id]))
        owner.wait(pb.Packet.ROLE_ACTION_RES, pred=lambda p: p.role_action_res.action == "assign")
        set_overwrite(owner, "general", pb.ChannelOverwrite.MEMBER, "carol", deny=pb.PERM_SEND_MESSAGES)
        owner.send(pb.Packet.INVITE_CREATE_REQ, invite_create_req=pb.InviteCreateRequest(for_user="dana", request_id=7))
        dana_code = owner.wait(pb.Packet.INVITE_CREATE_RES).invite_create_res.invite.code
        owner.send(pb.Packet.BAN_MEMBER_REQ, ban_member_req=pb.BanMemberRequest(username="eve", uid=uid_of("eve"), reason="never seen"))
        owner.wait(pb.Packet.MOD_ACTION_RES, pred=lambda p: p.mod_action_res.action == "ban")
        owner.send(pb.Packet.BAN_MEMBER_REQ, ban_member_req=pb.BanMemberRequest(username="frank", uid=uid_of("frank"), reason="seen"))
        owner.wait(pb.Packet.MOD_ACTION_RES, pred=lambda p: p.mod_action_res.action == "ban")
        bob.send(pb.Packet.CHANNEL_MSG, channel_msg=pb.ChannelMessage(channel_id="general", content="from v14", nonce="m1"))
        bob_msg = bob.wait(pb.Packet.CHANNEL_MSG, pred=lambda p: p.channel_msg.content == "from v14").channel_msg.id
        for c in (owner, bob, carol, frank): c.close()
        # Rows from before uids reached this server: a member who never
        # reconnected since, and everything they left behind.
        now = int(time.time())
        sql("insert into members(username, joined_at, nickname, uid) values('lazy2', ?, 'Lazybones', 0)", now)
        sql("insert into member_roles(username, role_id) values('lazy2', ?)", mods.id)
        sql("insert into channel_overwrites(channel_id, target_type, target_id, allow, deny) values('general', 1, 'lazy2', 0, ?)",
            pb.PERM_ATTACH_FILES)
        lazy_msg = sql("insert into messages(channel_id, sender, content, timestamp) values('general', 'lazy2', 'old words', ?) returning id", now)[0][0]
        sql("insert into invites(code, created_by, created_at, expires_at, max_uses, uses) values('LAZYINV001', 'lazy2', ?, 0, 0, 0)", now)
        sql("insert into attachments(message_id, kind, filename, mime, size_bytes, position, created_at, upload_status, uploader, channel_id) "
            "values(0, 2, 'half.bin', 'application/octet-stream', 0, 0, ?, 'uploading', 'lazy2', 'general')", now)
        sql("insert into channel_key_epochs(channel_id, epoch, created_by, created_at) values('general', 1, 'lazy2', ?)", now)
        sql("insert into channel_key_blobs(channel_id, epoch, recipient, sender, blob, created_at) values('general', 1, 'bob', 'lazy2', x'01', ?)", now)
        check("seeded as v14", sql("select value from server_meta where key='schema_version'") == [("14",)])
    finally:
        stop_server(old)

    new = start_server()
    stop_server(new)
    U = uid_for
    ph = dict(sql("select username, uid from legacy_claims"))
    check("v15 to v17: schema stamped at the latest", sql("select value from server_meta where key='schema_version'") == [("17",)])
    check("v17: epochs gain a key check (none for old ones), refusal and rotation tables",
          sql("select key_check from channel_key_epochs where channel_id='general' and epoch=1") == [(None,)]
          and sql("select count(*) from sqlite_master where name in ('channel_key_refusals', 'channel_key_rotations')") == [(2,)])
    check("v15: placeholders for names with no uid", set(ph) == {"lazy2", "eve", "dana"} and all(v < 0 for v in ph.values()), str(ph))
    check("v15: owner by uid", sql("select value from server_meta where key='owner_uid'") == [(str(U("alice")),)]
          and sql("select count(*) from server_meta where key='owner'") == [(0,)])
    check("v15: members keyed by uid", dict(sql("select username, uid from members"))
          == {"alice": U("alice"), "bob": U("bob"), "carol": U("carol"), "lazy2": ph["lazy2"]})
    check("v15: legacy member keeps their nickname", sql("select nickname from members where username='lazy2'") == [("Lazybones",)])
    check("v15: roles by uid", sorted(sql("select uid from member_roles where role_id=?", mods.id)) == sorted([(U("bob"),), (ph["lazy2"],)]))
    check("v15: member overwrites name the uid", sorted(sql("select target_id from channel_overwrites where target_type=1"))
          == sorted([(str(U("carol")),), (str(ph["lazy2"]),)]))
    check("v15: bans by uid; a name-only ban keeps its placeholder", dict(sql("select username, uid from bans"))
          == {"eve": ph["eve"], "frank": U("frank")})
    check("v15: personal invite for a never-seen name", sql("select created_by_uid, for_uid from invites where code=?", dana_code)
          == [(U("alice"), ph["dana"])])
    check("v15: invite and upload by the legacy member", sql("select created_by_uid from invites where code='LAZYINV001'") == [(ph["lazy2"],)]
          and sql("select uploader_uid from attachments where filename='half.bin'") == [(ph["lazy2"],)])
    check("v15: key escrow by uid, names kept", sql("select recipient_uid, recipient, sender_uid, sender from channel_key_blobs")
          == [(U("bob"), "bob", ph["lazy2"], "lazy2")]
          and sql("select created_by_uid from channel_key_epochs where channel_id='general'") == [(ph["lazy2"],)])
    check("v15: senders by uid", sql("select sender_uid from messages where id=?", bob_msg) == [(U("bob"),)]
          and sql("select sender_uid from messages where id=?", lazy_msg) == [(ph["lazy2"],)])
    check("v15: audit actors by uid", sql("select count(*) from audit_log where actor='alice' and actor_uid=?", U("alice"))[0][0] > 0)

    proc = start_server()
    try:
        owner = Client("alice"); ok, r = auth_ok(owner)
        check("migrated: owner reported by uid", ok and r.community_auth_res.owner_uid == U("alice")
              and r.community_auth_res.owner_username == "alice")
        bob = Client("bob"); assert auth_ok(bob)[0]
        carol = Client("carol"); assert auth_ok(carol)[0]
        for c in (owner, bob, carol): c.flush(0.4)
        carol.send(pb.Packet.CHANNEL_MSG, channel_msg=pb.ChannelMessage(channel_id="general", content="hi", nonce="c1"))
        rej = carol.wait(pb.Packet.CHANNEL_MSG_REJECTED, timeout=2)
        check("migrated: carol's member overwrite still applies", rej is not None)
        owner.send(pb.Packet.MEMBER_LIST_REQ, member_list_req=pb.MemberListRequest(after="", limit=100))
        ml = owner.wait(pb.Packet.MEMBER_LIST_RES, pred=lambda p: p.member_list_res.first_page)
        bm = next((m for m in ml.member_list_res.members if m.username == "bob"), None) if ml else None
        check("migrated: bob keeps his role, roster carries uids", bm is not None and bm.uid == U("bob") and list(bm.role_ids) == [mods.id])
        e = Client("eve", invite=make_invite(owner)); ok, r = auth_ok(e)
        check("migrated: the never-seen banned name is still banned", not ok and r.community_auth_res.error_code == "banned")
        e.close()
        check("... and the ban moved to her uid", dict(sql("select username, uid from bans")).get("eve") == U("eve"))
        f = Client("frank", invite=make_invite(owner)); ok, r = auth_ok(f)
        check("migrated: frank still banned", not ok and r.community_auth_res.error_code == "banned"); f.close()
        d = Client("dana", invite=dana_code); ok, r = auth_ok(d)
        check("migrated: the personal invite admits dana", ok, r.community_auth_res.error_code if r else None); d.close()
        lz = Client("lazy2"); ok, r = auth_ok(lz)
        check("migrated: the legacy member claims their placeholder", ok
              and sql("select uid, nickname from members where username='lazy2'") == [(U("lazy2"), "Lazybones")])
        check("... roles, overwrite, message, invite, upload and key blob follow",
              sql("select count(*) from member_roles where uid=?", U("lazy2")) == [(1,)]
              and sql("select count(*) from channel_overwrites where target_type=1 and target_id=?", str(U("lazy2"))) == [(1,)]
              and sql("select sender_uid from messages where id=?", lazy_msg) == [(U("lazy2"),)]
              and sql("select created_by_uid from invites where code='LAZYINV001'") == [(U("lazy2"),)]
              and sql("select uploader_uid from attachments where filename='half.bin'") == [(U("lazy2"),)]
              and sql("select sender_uid from channel_key_blobs") == [(U("lazy2"),)]
              and sql("select count(*) from legacy_claims where username='lazy2'") == [(0,)])
        lz.flush(0.3)
        lz.send(pb.Packet.MESSAGE_EDIT_REQ, message_edit_req=pb.MessageEditReq(channel_id="general", message_id=lazy_msg, content="new words"))
        er = lz.wait(pb.Packet.MESSAGE_EDIT_RES)
        check("... and they can edit their old message", er is not None and er.message_edit_res.success)
        for c in (owner, bob, carol, lz): c.close()
    finally:
        stop_server(proc)
    proc = start_server(); stop_server(proc)
    check("v15 to v17 don't run twice", sql("select value from server_meta where key='schema_version'") == [("17",)]
          and sql("select count(*) from members") == [(5,)])


def test_uid_wire():
    """Users by uid on the wire: payloads carry uids, requests target by uid."""
    print("[uid] uids on the wire, requests by uid")
    U = uid_for
    owner = Client("alice"); ok, r = auth_ok(owner)
    check("auth: owner by uid", ok and r.community_auth_res.owner_uid == U("alice"))
    w1 = join("uidw1", owner); w2 = join("uidw2", owner)
    for c in (owner, w1, w2): c.flush(0.4)
    # Token without a uid claim: refused.
    h = b64(json.dumps({"alg": "EdDSA", "typ": "JWS"}).encode())
    now = int(time.time())
    p = b64(json.dumps({"iss": "decibell_central_auth", "sub": "uidw1", "iat": now, "exp": now + 3600}).encode())
    nouid = f"{h}.{p}.{b64(ed25519_sign(KEY_PEM, f'{h}.{p}'.encode()))}"
    ctx = ssl.create_default_context(); ctx.check_hostname = False; ctx.verify_mode = ssl.CERT_NONE
    c = Client.__new__(Client)
    c.s = ctx.wrap_socket(connect(8082)); c.username = "uidw1"; c.jwt = nouid; c.buf = b""; c.inbox = []; c.closed = False
    c.send(pb.Packet.COMMUNITY_AUTH_REQ, community_auth_req=pb.CommunityAuthRequest(jwt_token=nouid))
    ok, r = auth_ok(c)
    check("a token without a uid is refused", not ok and r is not None and r.community_auth_res.error_code == "auth"); c.close()
    # Messages carry the sender's uid; replies the parent's.
    w1.send(pb.Packet.CHANNEL_MSG, channel_msg=pb.ChannelMessage(channel_id="general", content="uid one", nonce="u1"))
    m1 = w2.wait(pb.Packet.CHANNEL_MSG, pred=lambda p: p.channel_msg.content == "uid one")
    check("message carries sender_uid", m1 is not None and m1.channel_msg.sender_uid == U("uidw1") and m1.channel_msg.sender == "uidw1")
    w2.send(pb.Packet.CHANNEL_MSG, channel_msg=pb.ChannelMessage(channel_id="general", content="uid two", nonce="u2",
                                                                 reply_to=m1.channel_msg.id))
    m2 = w1.wait(pb.Packet.CHANNEL_MSG, pred=lambda p: p.channel_msg.content == "uid two")
    check("reply carries the parent's uid", m2 is not None and m2.channel_msg.reply_to_sender_uid == U("uidw1"))
    w1.send(pb.Packet.CHANNEL_HISTORY_REQ, channel_history_req=pb.ChannelHistoryRequest(channel_id="general", limit=5))
    h1 = w1.wait(pb.Packet.CHANNEL_HISTORY_RES)
    row = next((m for m in h1.channel_history_res.messages if m.id == m1.channel_msg.id), None) if h1 else None
    check("history carries sender_uid", row is not None and row.sender_uid == U("uidw1") and row.sealed_sender == "")
    # A renamed sender shows under the current name; the stored row keeps the old one.
    sql("update members set username='uidw1x' where uid=?", U("uidw1"))
    w1.send(pb.Packet.CHANNEL_HISTORY_REQ, channel_history_req=pb.ChannelHistoryRequest(channel_id="general", limit=5))
    h1 = w1.wait(pb.Packet.CHANNEL_HISTORY_RES)
    row = next((m for m in h1.channel_history_res.messages if m.id == m1.channel_msg.id), None) if h1 else None
    check("history shows the sender's current name", row is not None and row.sender == "uidw1x"
          and sql("select sender from messages where id=?", m1.channel_msg.id) == [("uidw1",)])
    sql("update members set username='uidw1' where uid=?", U("uidw1"))
    # Moderation by uid: nickname, timeout, kick.
    owner.send(pb.Packet.SET_NICKNAME_REQ, set_nickname_req=pb.SetNicknameRequest(uid=U("uidw2"), nickname="Two"))
    r = owner.wait(pb.Packet.MOD_ACTION_RES, pred=lambda p: p.mod_action_res.action == "nickname")
    check("nickname by uid", r is not None and r.mod_action_res.success and r.mod_action_res.uid == U("uidw2")
          and sql("select nickname from members where uid=?", U("uidw2")) == [("Two",)])
    up = owner.wait(pb.Packet.MEMBER_UPSERT, pred=lambda p: p.member_upsert.member.uid == U("uidw2"))
    check("member upsert carries the uid", up is not None and up.member_upsert.member.nickname == "Two")
    owner.send(pb.Packet.TIMEOUT_MEMBER_REQ, timeout_member_req=pb.TimeoutMemberRequest(uid=U("uidw2"), until=int(time.time()) + 600))
    r = owner.wait(pb.Packet.MOD_ACTION_RES, pred=lambda p: p.mod_action_res.action == "timeout")
    check("timeout by uid", r is not None and r.mod_action_res.success)
    owner.send(pb.Packet.TIMEOUT_MEMBER_REQ, timeout_member_req=pb.TimeoutMemberRequest(uid=U("uidw2"), until=0))
    owner.wait(pb.Packet.MOD_ACTION_RES, pred=lambda p: p.mod_action_res.action == "timeout")
    owner.send(pb.Packet.KICK_MEMBER_REQ, kick_member_req=pb.KickMemberRequest(uid=987654321))
    r = owner.wait(pb.Packet.MOD_ACTION_RES, pred=lambda p: p.mod_action_res.action == "kick")
    check("kick of an unknown uid", r is not None and not r.mod_action_res.success)
    owner.send(pb.Packet.KICK_MEMBER_REQ, kick_member_req=pb.KickMemberRequest(uid=U("uidw2")))
    r = owner.wait(pb.Packet.MOD_ACTION_RES, pred=lambda p: p.mod_action_res.action == "kick")
    rm = owner.wait(pb.Packet.MEMBER_REMOVE, pred=lambda p: p.member_remove.uid == U("uidw2"))
    check("kick by uid", r is not None and r.mod_action_res.success and rm is not None and rm.member_remove.username == "uidw2")
    # Personal invite by uid.
    owner.send(pb.Packet.INVITE_CREATE_REQ, invite_create_req=pb.InviteCreateRequest(for_uid=U("uidw3"), for_user="uidw3", request_id=9))
    inv = owner.wait(pb.Packet.INVITE_CREATE_RES).invite_create_res.invite
    check("personal invite by uid", inv.for_uid == U("uidw3") and inv.created_by_uid == U("alice"))
    other = Client("uidw3", invite=inv.code, uid=U("uidw3") + 7); ok, _ = auth_ok(other)
    check("... refuses another account with the same name", not ok); other.close()
    w3 = Client("uidw3", invite=inv.code); ok, _ = auth_ok(w3)
    check("... admits the account it was made for", ok); w3.close()
    # Search by author uid.
    sr = search_req(w1, 4100, author_uids=[U("uidw1")])
    check("search by author uid", sr is not None and sr.success
          and m1.channel_msg.id in [m.id for m in sr.messages]
          and all(m.sender_uid == U("uidw1") for m in sr.messages))
    for c in (owner, w1, w2): c.close()
    # Renames follow the newest token: one issued before the token that
    # last named a member (a device that kept it across a rename) can't
    # rename them back, and its session goes by the newer name.
    c = Client("uidw1y", uid=U("uidw1")); ok, _ = auth_ok(c); c.close()
    check("a newer token renames", ok and sql("select username from members where uid=?", U("uidw1")) == [("uidw1y",)])
    stale = Client("uidw1", uid=U("uidw1"), iat=int(time.time()) - 600); ok, _ = auth_ok(stale)
    check("an older token keeps the newer name", ok
          and sql("select username from members where uid=?", U("uidw1")) == [("uidw1y",)])
    stale.send(pb.Packet.CHANNEL_MSG, channel_msg=pb.ChannelMessage(channel_id="general", content="stale token", nonce="st1"))
    echo = stale.wait(pb.Packet.CHANNEL_MSG, pred=lambda p: p.channel_msg.nonce == "st1")
    check("... and its session acts under it", echo is not None and echo.channel_msg.sender == "uidw1y")
    stale.close()
    back = Client("uidw1", uid=U("uidw1")); auth_ok(back); back.close()


def test_live_rename():
    """A newer token for a connected member renames them in place: no
    reconnect, and the voice and stream views follow."""
    print("[uid] live rename via SESSION_TOKEN_UPDATE")
    U = uid_for
    owner = Client("alice"); assert auth_ok(owner)[0]
    lr = join("liveren", owner)
    for c in (owner, lr): c.flush(0.4)
    lr.send(pb.Packet.JOIN_VOICE_REQ, join_voice_req=pb.JoinVoiceRequest(channel_id="voice-lounge"))
    lr.wait(pb.Packet.VOICE_PRESENCE_UPDATE, pred=lambda p: p.voice_presence_update.channel_id == "voice-lounge")
    lr.send(pb.Packet.START_STREAM_REQ, start_stream_req=pb.StartStreamRequest(channel_id="voice-lounge", target_fps=30))
    owner.wait(pb.Packet.STREAM_PRESENCE_UPDATE, pred=lambda p: any(
        s.owner_uid == U("liveren") for s in p.stream_presence_update.active_streams))
    owner.flush(0.3)
    update = lambda token: lr.send(pb.Packet.SESSION_TOKEN_UPDATE,
                                   session_token_update=pb.SessionTokenUpdate(jwt_token=token))
    update(jwt("liveold", uid=U("liveren"), iat=int(time.time()) - 600))
    update(jwt("intruder", uid=U("liveren") + 1))
    update("not-a-token")
    time.sleep(0.4)
    check("older, foreign or bad tokens change nothing",
          sql("select username from members where uid=?", U("liveren")) == [("liveren",)])
    time.sleep(1.1)   # a later token (central's verifier refuses one issued in the future)
    update(jwt("liveren2", uid=U("liveren")))
    up = owner.wait(pb.Packet.MEMBER_UPSERT, pred=lambda p: p.member_upsert.member.uid == U("liveren"))
    check("roster relabels the row in place (an upsert, no remove)",
          up is not None and up.member_upsert.member.username == "liveren2"
          and not any(p.type == pb.Packet.MEMBER_REMOVE and p.member_remove.uid == U("liveren") for p in owner.inbox))
    vp = owner.wait(pb.Packet.VOICE_PRESENCE_UPDATE, pred=lambda p: p.voice_presence_update.channel_id == "voice-lounge"
                    and "liveren2" in p.voice_presence_update.active_users)
    check("voice presence shows the new name", vp is not None)
    sp = owner.wait(pb.Packet.STREAM_PRESENCE_UPDATE, pred=lambda p: any(
        s.owner_username == "liveren2" for s in p.stream_presence_update.active_streams))
    check("the stream carries on under the new name", sp is not None and any(
        s.owner_uid == U("liveren") for s in sp.stream_presence_update.active_streams))
    check("member row renamed", sql("select username from members where uid=?", U("liveren")) == [("liveren2",)])
    lr.send(pb.Packet.CHANNEL_MSG, channel_msg=pb.ChannelMessage(channel_id="general", content="after a live rename", nonce="lr1"))
    echo = lr.wait(pb.Packet.CHANNEL_MSG, pred=lambda p: p.channel_msg.nonce == "lr1")
    check("the same connection acts under the new name", echo is not None and echo.channel_msg.sender == "liveren2"
          and not lr.closed)
    # Removed meanwhile (row gone, socket still up): a token renames nobody.
    sql("delete from members where uid=?", U("liveren"))
    owner.flush(0.3)
    time.sleep(1.1)
    update(jwt("liveren3", uid=U("liveren")))
    stray = owner.wait(pb.Packet.MEMBER_UPSERT, timeout=0.8,
                       pred=lambda p: p.member_upsert.member.username == "liveren3")
    check("a removed member's token renames nobody", stray is None
          and sql("select count(*) from members where uid=?", U("liveren")) == [(0,)])
    lr.close(); owner.close()


def test_auth_failure_closes():
    print("[regression] failed auth still delivers response then closes")
    c = Client("nobody")
    ok, r = auth_ok(c)
    check("non-member rejected with not_member", r is not None and not ok and r.community_auth_res.error_code == "not_member")
    check("socket closed after rejection", c.is_closed())
    c.close()


def test_c2_username_reuse():
    print("[C2] users by uid: a reused username inherits nothing; a central rename keeps everything")
    owner = Client("alice"); assert auth_ok(owner)[0]
    owner.flush(0.5)
    owner.send(pb.Packet.ROLE_CREATE_REQ,
               role_create_req=pb.RoleCreateRequest(name="C2Marker", color=0, permissions=pb.PERM_MANAGE_CHANNELS))
    role = owner.wait(pb.Packet.ROLE_ACTION_RES, pred=lambda p: p.role_action_res.action == "create").role_action_res.role

    # A member joins and is granted the role (addressed by uid).
    victim_uid = uid_for("c2victim")
    victim = join("c2victim", owner)
    owner.send(pb.Packet.MEMBER_ROLES_UPDATE_REQ,
               member_roles_update_req=pb.MemberRolesUpdateRequest(uid=victim_uid, role_ids=[role.id]))
    owner.wait(pb.Packet.ROLE_ACTION_RES, pred=lambda p: p.role_action_res.action == "assign")
    check("victim granted the role", sql("select role_id from member_roles where uid=?", victim_uid) == [(role.id,)])
    check("victim row keyed by its uid", sql("select uid from members where username='c2victim'") == [(victim_uid,)])
    victim.close()

    # A DIFFERENT central account (new uid) holds the name now and joins via invite.
    impostor_uid = victim_uid + 500000
    impostor = Client("c2victim", invite=make_invite(owner), uid=impostor_uid)
    ok, r = auth_ok(impostor)
    check("reused username admitted as a fresh member", ok, r.community_auth_res.error_code if r else None)
    check("reused username inherited NO roles", sql("select role_id from member_roles where uid=?", impostor_uid) == [])
    check("the original member keeps their row and role",
          sql("select role_id from member_roles where uid=?", victim_uid) == [(role.id,)]
          and sql("select count(*) from members where uid in (?, ?)", victim_uid, impostor_uid) == [(2,)])
    impostor.close()
    owner.send(pb.Packet.KICK_MEMBER_REQ, kick_member_req=pb.KickMemberRequest(uid=impostor_uid))
    owner.wait(pb.Packet.MOD_ACTION_RES, pred=lambda p: p.mod_action_res.action == "kick")
    check("kick by uid removes only that account", sql("select uid from members where username='c2victim'") == [(victim_uid,)])

    # Rename: same uid, new name → same row, roles untouched; the roster
    # retires the old name and upserts the new one.
    bob_uid = uid_for("c2bob")
    bob = join("c2bob", owner)
    owner.send(pb.Packet.MEMBER_ROLES_UPDATE_REQ,
               member_roles_update_req=pb.MemberRolesUpdateRequest(username="c2bob", uid=uid_of("c2bob"), role_ids=[role.id]))
    owner.wait(pb.Packet.ROLE_ACTION_RES, pred=lambda p: p.role_action_res.action == "assign")
    check("bob granted the role", sql("select role_id from member_roles where uid=?", bob_uid) == [(role.id,)])
    bob.close()
    owner.flush(0.5)

    # Reconnect under a new name with the SAME uid — no invite needed (member by identity).
    renamed = Client("c2bob_renamed", uid=bob_uid)
    ok, r = auth_ok(renamed)
    check("renamed account admitted without an invite", ok, r.community_auth_res.error_code if r else None)
    check("rename updated the name on the same row",
          sql("select uid from members where username='c2bob_renamed'") == [(bob_uid,)]
          and sql("select count(*) from members where username='c2bob'") == [(0,)])
    check("renamed account kept its role", sql("select role_id from member_roles where uid=?", bob_uid) == [(role.id,)])
    up = owner.wait(pb.Packet.MEMBER_UPSERT, pred=lambda p: p.member_upsert.member.username == "c2bob_renamed")
    check("roster: the same uid's row relabelled, nothing removed",
          up is not None and up.member_upsert.member.uid == bob_uid
          and not any(p.type == pb.Packet.MEMBER_REMOVE and p.member_remove.uid == bob_uid for p in owner.inbox))
    # Requests target accounts only: a name alone addresses nobody.
    owner.send(pb.Packet.KICK_MEMBER_REQ, kick_member_req=pb.KickMemberRequest(username="c2bob_renamed", reason=""))
    r = owner.wait(pb.Packet.MOD_ACTION_RES, pred=lambda p: p.mod_action_res.action == "kick")
    check("a kick naming its target only by name is refused",
          r is not None and not r.mod_action_res.success
          and sql("select count(*) from members where uid=?", bob_uid) == [(1,)])
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
    owner.send(pb.Packet.BAN_MEMBER_REQ, ban_member_req=pb.BanMemberRequest(username="pubj2", uid=uid_of("pubj2"), reason=""))
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


def test_history_around_and_after():
    print("[jump] history around_id returns a centered window; after_id pages newer")
    owner = Client("alice"); assert auth_ok(owner)[0]; owner.flush(0.5)
    # Seed 40 messages directly (history reads from the DB; avoids the
    # per-session send rate limit). Capture their ids in insertion order.
    now = int(time.time())
    ids = []
    for i in range(40):
        rid = sql("insert into messages(channel_id, sender, content, timestamp) "
                  "values('general','alice',?,?) returning id", f"jm{i}", now + i)[0][0]
        ids.append(rid)
    target = ids[20]  # a message in the middle

    # Context window around the target: window includes it, both has_more flags set.
    owner.send(pb.Packet.CHANNEL_HISTORY_REQ,
               channel_history_req=pb.ChannelHistoryRequest(channel_id="general", around_id=target, limit=10))
    r = owner.wait(pb.Packet.CHANNEL_HISTORY_RES, timeout=2, pred=lambda p: p.channel_history_res.around_id == target)
    msgs = r.channel_history_res.messages
    got_ids = [m.id for m in msgs]
    check("window contains the target", target in got_ids)
    check("window is oldest→newest", got_ids == sorted(got_ids))
    check("window has messages before the target", any(m.id < target for m in msgs))
    check("window has messages after the target", any(m.id > target for m in msgs))
    check("more-before + more-after both set for a middle target",
          r.channel_history_res.has_more and r.channel_history_res.has_more_after)

    # after_id pages newer (oldest-first), from near the top.
    owner.send(pb.Packet.CHANNEL_HISTORY_REQ,
               channel_history_req=pb.ChannelHistoryRequest(channel_id="general", after_id=ids[0], limit=10))
    r = owner.wait(pb.Packet.CHANNEL_HISTORY_RES, timeout=2, pred=lambda p: p.channel_history_res.after_id == ids[0])
    after = [m.id for m in r.channel_history_res.messages]
    check("after page is the 10 ids right after ids[0]", after == ids[1:11])
    check("after page reports more newer", r.channel_history_res.has_more_after)

    # after_id near the end → no more newer.
    owner.send(pb.Packet.CHANNEL_HISTORY_REQ,
               channel_history_req=pb.ChannelHistoryRequest(channel_id="general", after_id=ids[38], limit=10))
    r = owner.wait(pb.Packet.CHANNEL_HISTORY_RES, timeout=2, pred=lambda p: p.channel_history_res.after_id == ids[38])
    check("after page near end returns the tail, no more newer",
          [m.id for m in r.channel_history_res.messages] == [ids[39]] and not r.channel_history_res.has_more_after)
    owner.close()


def test_b4_history_nonce():
    print("[B4] history hands a sender back the nonce of a recent send, nobody else")
    owner = Client("alice"); assert auth_ok(owner)[0]; owner.flush(0.5)
    nia = join("nia", owner, nonce="d1"); nia.flush(0.5); owner.flush(0.5)
    nia.send(pb.Packet.CHANNEL_MSG, channel_msg=pb.ChannelMessage(channel_id="general", content="b4 lost echo", nonce="b4-n1"))
    echo = nia.wait(pb.Packet.CHANNEL_MSG, timeout=2, pred=lambda p: p.channel_msg.nonce == "b4-n1")
    check("the send is echoed with its nonce", echo is not None and echo.channel_msg.id > 0)
    mid = echo.channel_msg.id if echo else 0
    owner.send(pb.Packet.CHANNEL_MSG, channel_msg=pb.ChannelMessage(channel_id="general", content="b4 other", nonce="b4-owner"))
    oecho = owner.wait(pb.Packet.CHANNEL_MSG, timeout=2, pred=lambda p: p.channel_msg.nonce == "b4-owner")
    oid = oecho.channel_msg.id if oecho else 0

    def row(c, after, want):
        c.send(pb.Packet.CHANNEL_HISTORY_REQ, channel_history_req=pb.ChannelHistoryRequest(channel_id="general", after_id=after, limit=50))
        r = c.wait(pb.Packet.CHANNEL_HISTORY_RES, timeout=2, pred=lambda p: p.channel_history_res.after_id == after)
        return next((m for m in r.channel_history_res.messages if m.id == want), None) if r else None

    mine = row(nia, mid - 1, mid)
    check("the sender's history row carries its nonce", mine is not None and mine.nonce == "b4-n1",
          f"nonce={mine.nonce if mine else None!r}")
    theirs = row(owner, mid - 1, mid)
    check("another member's history row has no nonce", theirs is not None and theirs.nonce == "")
    others = row(nia, mid - 1, oid)
    check("someone else's row has no nonce for the sender either", others is not None and others.nonce == "")
    # The latest page (before_id 0) carries it too: that is the re-sync page.
    nia.send(pb.Packet.CHANNEL_HISTORY_REQ, channel_history_req=pb.ChannelHistoryRequest(channel_id="general", limit=50))
    r = nia.wait(pb.Packet.CHANNEL_HISTORY_RES, timeout=2,
                 pred=lambda p: p.channel_history_res.after_id == 0 and p.channel_history_res.around_id == 0)
    latest = next((m for m in r.channel_history_res.messages if m.id == mid), None) if r else None
    check("the latest page carries the nonce", latest is not None and latest.nonce == "b4-n1")
    # The sender's other device (same account, another session) reconciles too.
    nia2 = Client("nia", nonce="d2"); assert auth_ok(nia2)[0]; nia2.flush(0.5)
    other_device = row(nia2, mid - 1, mid)
    check("the sender's other session gets it as well", other_device is not None and other_device.nonce == "b4-n1")
    nia2.close(); nia.close(); owner.close()


def test_message_reply():
    print("[reply] reply_to persists + echoes on broadcast/history; invalid dropped")
    owner = Client("alice"); assert auth_ok(owner)[0]
    rp = join("rpr", owner); owner.flush(0.5); rp.flush(0.5)

    # Post a parent message; capture its id.
    owner.send(pb.Packet.CHANNEL_MSG, channel_msg=pb.ChannelMessage(channel_id="general", content="parent"))
    parent = rp.wait(pb.Packet.CHANNEL_MSG, timeout=2, pred=lambda p: p.channel_msg.content == "parent")
    pid = parent.channel_msg.id
    check("parent posted", pid > 0)
    check("parent has reply_to=0", parent.channel_msg.reply_to == 0)
    # Give the parent an attachment so the reply's embedded preview carries its
    # kind (VIDEO=1) — lets the client label an attachment-only parent.
    sql("insert into attachments(message_id, kind, filename, created_at) values(?, 1, 'clip.mp4', ?)",
        pid, int(time.time()))
    owner.flush(0.3); rp.flush(0.3)

    # Reply to it.
    rp.send(pb.Packet.CHANNEL_MSG, channel_msg=pb.ChannelMessage(channel_id="general", content="a reply", reply_to=pid))
    bc = owner.wait(pb.Packet.CHANNEL_MSG, timeout=2, pred=lambda p: p.channel_msg.content == "a reply")
    check("reply broadcast carries reply_to", bc is not None and bc.channel_msg.reply_to == pid)
    check("reply_to persisted", sql("select reply_to from messages where id=?", bc.channel_msg.id) == [(pid,)])
    check("broadcast embeds parent preview",
          bc.channel_msg.reply_to_sender == "alice" and bc.channel_msg.reply_to_content == "parent")
    check("broadcast embeds parent attachment kinds",
          list(bc.channel_msg.reply_to_attachment_kinds) == [1])

    # A reply_to pointing at a nonexistent message is dropped to 0.
    rp.send(pb.Packet.CHANNEL_MSG, channel_msg=pb.ChannelMessage(channel_id="general", content="bad ref", reply_to=999999))
    bc = owner.wait(pb.Packet.CHANNEL_MSG, timeout=2, pred=lambda p: p.channel_msg.content == "bad ref")
    check("invalid reply_to dropped on broadcast", bc is not None and bc.channel_msg.reply_to == 0)
    check("invalid reply_to not persisted", sql("select reply_to from messages where id=?", bc.channel_msg.id) == [(0,)])

    # History reflects reply_to.
    rp.flush(0.3)
    rp.send(pb.Packet.CHANNEL_HISTORY_REQ, channel_history_req=pb.ChannelHistoryRequest(channel_id="general", limit=50))
    h = rp.wait(pb.Packet.CHANNEL_HISTORY_RES, timeout=2)
    replies = [m for m in h.channel_history_res.messages if m.content == "a reply"]
    check("history shows reply_to", len(replies) == 1 and replies[0].reply_to == pid)
    check("history embeds parent preview",
          len(replies) == 1 and replies[0].reply_to_sender == "alice"
          and replies[0].reply_to_content == "parent")
    check("history embeds parent attachment kinds",
          len(replies) == 1 and list(replies[0].reply_to_attachment_kinds) == [1])

    # Deleted parent → history still carries reply_to but the embedded sender
    # comes back empty (the client renders an unclickable tombstone).
    sql("delete from messages where id=?", pid)
    rp.flush(0.3)
    rp.send(pb.Packet.CHANNEL_HISTORY_REQ, channel_history_req=pb.ChannelHistoryRequest(channel_id="general", limit=50))
    h = rp.wait(pb.Packet.CHANNEL_HISTORY_RES, timeout=2)
    replies = [m for m in h.channel_history_res.messages if m.content == "a reply"]
    check("deleted parent -> empty embedded sender",
          len(replies) == 1 and replies[0].reply_to == pid
          and replies[0].reply_to_sender == "" and replies[0].reply_to_content == "")
    rp.close(); owner.close()


def test_message_edit():
    print("[edit] own channel message edits; others rejected; (edited) via edited_at")
    owner = Client("alice"); assert auth_ok(owner)[0]
    ed = join("edr", owner); owner.flush(0.5); ed.flush(0.5)

    # A member posts a message; capture its id from the broadcast.
    ed.send(pb.Packet.CHANNEL_MSG, channel_msg=pb.ChannelMessage(channel_id="general", content="orig"))
    bc = owner.wait(pb.Packet.CHANNEL_MSG, timeout=2, pred=lambda p: p.channel_msg.content == "orig" and p.channel_msg.sender == "edr")
    mid = bc.channel_msg.id
    check("message posted", mid > 0)
    check("fresh message has edited_at=0", bc.channel_msg.edited_at == 0)
    owner.flush(0.3); ed.flush(0.3)

    # Owner (not the author) can't edit someone else's message.
    owner.send(pb.Packet.MESSAGE_EDIT_REQ, message_edit_req=pb.MessageEditReq(channel_id="general", message_id=mid, content="hijacked"))
    r = owner.wait(pb.Packet.MESSAGE_EDIT_RES, timeout=2)
    check("editing another's message rejected", r is not None and not r.message_edit_res.success)

    # Author edits their own message.
    ed.send(pb.Packet.MESSAGE_EDIT_REQ, message_edit_req=pb.MessageEditReq(channel_id="general", message_id=mid, content="edited!"))
    r = ed.wait(pb.Packet.MESSAGE_EDIT_RES, timeout=2)
    check("author edit accepted", r is not None and r.message_edit_res.success, r.message_edit_res.message if r else None)
    e = owner.wait(pb.Packet.CHANNEL_MESSAGE_EDITED, timeout=2, pred=lambda p: p.channel_message_edited.message_id == mid)
    check("CHANNEL_MESSAGE_EDITED broadcast with new content", e is not None and e.channel_message_edited.content == "edited!" and e.channel_message_edited.edited_at > 0)
    check("row persisted", sql("select content, edited_at>0 from messages where id=?", mid) == [("edited!", 1)])

    # Empty edit rejected.
    ed.send(pb.Packet.MESSAGE_EDIT_REQ, message_edit_req=pb.MessageEditReq(channel_id="general", message_id=mid, content=""))
    r = ed.wait(pb.Packet.MESSAGE_EDIT_RES, timeout=2)
    check("empty edit rejected", r is not None and not r.message_edit_res.success)

    # History reflects the edit + edited_at.
    ed.flush(0.3)
    ed.send(pb.Packet.CHANNEL_HISTORY_REQ, channel_history_req=pb.ChannelHistoryRequest(channel_id="general", limit=50))
    h = ed.wait(pb.Packet.CHANNEL_HISTORY_RES, timeout=2)
    row = next((m for m in h.channel_history_res.messages if m.id == mid), None)
    check("history shows edited content + edited_at", row is not None and row.content == "edited!" and row.edited_at > 0)
    ed.close(); owner.close()


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
               member_roles_update_req=pb.MemberRolesUpdateRequest(username="mia", uid=uid_of("mia"), role_ids=[role.id]))
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
               timeout_member_req=pb.TimeoutMemberRequest(username="mia", uid=uid_of("mia"), until=int(time.time()) + 60))
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
    owner.send(pb.Packet.TIMEOUT_MEMBER_REQ, timeout_member_req=pb.TimeoutMemberRequest(username="mia", uid=uid_of("mia"), until=0))
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
    r = nate.wait(pb.Packet.MOD_ACTION_RES, timeout=2, pred=lambda p: p.mod_action_res.action == "message")
    check("an oversized message is refused, not dropped silently", r is not None and "too long" in r.mod_action_res.message)
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


# ---------------------------------------------------------------- permissions v3
# docs/superpowers/specs/2026-10-09-permissions-v3-design.md

def create_role(owner, name, perms, manage_each_other=False):
    owner.send(pb.Packet.ROLE_CREATE_REQ, role_create_req=pb.RoleCreateRequest(
        name=name, color=0, permissions=perms, manage_each_other=manage_each_other))
    r = owner.wait(pb.Packet.ROLE_ACTION_RES, pred=lambda p: p.role_action_res.action == "create")
    assert r and r.role_action_res.success, r
    return r.role_action_res.role


def delete_role(owner, role_id):
    owner.send(pb.Packet.ROLE_DELETE_REQ, role_delete_req=pb.RoleDeleteRequest(role_id=role_id))
    owner.wait(pb.Packet.ROLE_ACTION_RES, pred=lambda p: p.role_action_res.action == "delete")


def assign_roles(actor, username, role_ids):
    actor.send(pb.Packet.MEMBER_ROLES_UPDATE_REQ, member_roles_update_req=pb.MemberRolesUpdateRequest(
        username=username, uid=uid_of(username), role_ids=role_ids))
    return actor.wait(pb.Packet.ROLE_ACTION_RES, pred=lambda p: p.role_action_res.action == "assign")


def mod_res(c, action, timeout=3.0):
    return c.wait(pb.Packet.MOD_ACTION_RES, timeout=timeout, pred=lambda p: p.mod_action_res.action == action)


def mod_ok(r):
    return r is not None and r.mod_action_res.success


def voice_mod(c, username, action, channel_id=""):
    c.send(pb.Packet.VOICE_MOD_REQ, voice_mod_req=pb.VoiceModRequest(username=username, uid=uid_of(username), action=action, channel_id=channel_id))
    return mod_res(c, "voice_mod")


def set_nick(c, username, nickname):
    c.send(pb.Packet.SET_NICKNAME_REQ, set_nickname_req=pb.SetNicknameRequest(username=username, uid=uid_of(username), nickname=nickname))
    return mod_res(c, "nickname")


def create_channel(owner, name, ctype, category_id=""):
    owner.send(pb.Packet.CHANNEL_CREATE_REQ, channel_create_req=pb.ChannelCreateRequest(
        name=name, type=ctype, category_id=category_id))
    r = owner.wait(pb.Packet.CHANNEL_ACTION_RES, pred=lambda p: p.channel_action_res.action == "create")
    assert r and r.channel_action_res.success, r
    return r.channel_action_res.channel


def list_overwrites(c, channel_id):
    # Managers also get CHANNEL_OVERWRITES_RES pushed after every change;
    # drop queued ones so the answer reflects the state as of now.
    c.drain(0.3)
    c.inbox = [p for p in c.inbox if not (p.type == pb.Packet.CHANNEL_OVERWRITES_RES
                                          and p.channel_overwrites_res.channel_id == channel_id)]
    c.send(pb.Packet.CHANNEL_OVERWRITES_REQ, channel_overwrites_req=pb.ChannelOverwritesRequest(channel_id=channel_id))
    r = c.wait(pb.Packet.CHANNEL_OVERWRITES_RES, pred=lambda p: p.channel_overwrites_res.channel_id == channel_id)
    return {(o.target_type, o.target_id): (o.allow, o.deny) for o in r.channel_overwrites_res.overwrites} if r else None


def sync_perms(c, channel_id, category_id=""):
    c.send(pb.Packet.CHANNEL_PERMISSIONS_SYNC_REQ, channel_permissions_sync_req=pb.ChannelPermissionsSyncRequest(
        channel_id=channel_id, category_id=category_id))
    return c.wait(pb.Packet.CHANNEL_ACTION_RES, pred=lambda p: p.channel_action_res.action == "sync")


def set_category_sync(c, category_id, on):
    c.send(pb.Packet.CATEGORY_SYNC_SET_REQ, category_sync_set_req=pb.CategorySyncSetRequest(
        channel_id=category_id, enabled=on))
    return c.wait(pb.Packet.CHANNEL_ACTION_RES, pred=lambda p: p.channel_action_res.action == "category_sync")


def reorder(c, ids):
    c.send(pb.Packet.CHANNEL_REORDER_REQ, channel_reorder_req=pb.ChannelReorderRequest(channel_ids=ids))
    return c.wait(pb.Packet.CHANNEL_ACTION_RES, pred=lambda p: p.channel_action_res.action == "reorder")


def channel_order():
    return [r[0] for r in sql("select id from channels order by position, id")]


def perm_synced(channel_id):
    r = sql("select perm_synced from channels where id=?", channel_id)
    return r[0][0] if r else None


def fresh_list(c, timeout=3):
    """Next CHANNEL_LIST_UPDATE that arrives from now on (drops queued ones)."""
    c.inbox = [p for p in c.inbox if p.type != pb.Packet.CHANNEL_LIST_UPDATE]
    return channel_list(c, timeout)


def test_v11_migration():
    print("[v11] permissions v3 migration on an upgraded DB")
    proc = start_server(); stop_server(proc)
    old_everyone = (pb.PERM_SEND_MESSAGES | pb.PERM_CONNECT_VOICE | pb.PERM_STREAM |
                    pb.PERM_VIEW_CHANNEL | pb.PERM_READ_HISTORY | pb.PERM_ATTACH_FILES)
    sql("update roles set permissions=? where is_default=1", old_everyone)
    rid = sql("insert into roles(name, color, position, permissions, is_default) "
              "values('OldVoiceMod', 0, 1, ?, 0) returning id", pb.PERM_MUTE_MEMBERS)[0][0]
    sql("insert into channel_overwrites(channel_id, target_type, target_id, allow, deny) values('voice-lounge', 0, ?, ?, 0)",
        str(rid), pb.PERM_MUTE_MEMBERS)
    sql("insert into channel_overwrites(channel_id, target_type, target_id, allow, deny) values('voice-lounge-2', 0, ?, 0, ?)",
        str(rid), pb.PERM_MUTE_MEMBERS)
    sql("delete from server_meta where key='perm_bits_v11'")
    sql("alter table channels drop column perm_synced")   # pre-v11 shape
    proc = start_server(); stop_server(proc)
    ev = sql("select permissions from roles where is_default=1")[0][0]
    check("everyone gained the v3 default bits", ev & V3_DEFAULT_BITS == V3_DEFAULT_BITS and ev & old_everyone == old_everyone, hex(ev))
    split = pb.PERM_MUTE_MEMBERS | pb.PERM_DEAFEN_MEMBERS | pb.PERM_MOVE_MEMBERS
    check("VOICE_MODERATE role gained DEAFEN + MOVE", sql("select permissions from roles where id=?", rid) == [(split,)])
    check("... in overwrite allow", sql("select allow from channel_overwrites where channel_id='voice-lounge' and target_id=?", str(rid)) == [(split,)])
    check("... and in overwrite deny", sql("select deny from channel_overwrites where channel_id='voice-lounge-2' and target_id=?", str(rid)) == [(split,)])
    synced = dict(sql("select id, perm_synced from channels"))
    check("no channel outside a syncing category is synced (rows of its own or not)",
          synced.get("voice-lounge") == 0 and synced.get("voice-lounge-2") == 0 and synced.get("general") == 0, str(synced))
    check("v11 stamp written", sql("select value from server_meta where key='perm_bits_v11'") == [("1",)])
    sql("delete from channel_overwrites where target_id=?", str(rid))
    sql("delete from roles where id=?", rid)

    # v12: an upgrade from before v3 keeps every category a plain group, even
    # one carrying (formerly inert) overwrites.
    ev_id = str(sql("select id from roles where is_default=1")[0][0])
    sql("insert into channels(id, name, type, position) values('old-cat', 'Old', 2, 100), ('old-child', 'old-child', 0, 101)")
    sql("insert into channel_overwrites(channel_id, target_type, target_id, allow, deny) values('old-cat', 0, ?, 0, ?)",
        ev_id, pb.PERM_VIEW_CHANNEL)
    sql("alter table channels drop column category_sync")
    sql("update server_meta set value='10' where key='schema_version'")
    sql("delete from server_meta where key in ('category_sync_v12', 'perm_bits_v11')")
    sql("alter table channels drop column perm_synced")
    proc = start_server(); stop_server(proc)
    check("v10 → v12: a category with rows stays a plain group", sql("select category_sync from channels where id='old-cat'") == [(0,)])
    check("...and its channel follows nothing", perm_synced("old-child") == 0)
    check("schema at the latest (v17)", sql("select value from server_meta where key='schema_version'") == [("17",)])
    # A DB that ran v11 live keeps the categories whose rows were applying.
    sql("update channels set perm_synced=1 where id='old-child'")
    sql("update server_meta set value='11' where key='schema_version'")
    sql("delete from server_meta where key='category_sync_v12'")
    proc = start_server(); stop_server(proc)
    check("v11 → v12: a category whose rows applied keeps syncing", sql("select category_sync from channels where id='old-cat'") == [(1,)])
    check("...and its synced channel keeps following it", perm_synced("old-child") == 1)
    sql("delete from channel_overwrites where channel_id='old-cat'")
    sql("delete from channels where id in ('old-cat', 'old-child')")

    # v13: channel topics + personal invites on an upgraded DB.
    sql("alter table channels drop column topic")
    sql("alter table invites drop column for_user")
    sql("update server_meta set value='12' where key='schema_version'")
    proc = start_server(); stop_server(proc)
    check("v12 → v13: channels.topic + invites.for_user added",
          sql("select count(*) from pragma_table_info('channels') where name='topic'") == [(1,)] and
          sql("select count(*) from pragma_table_info('invites') where name='for_user'") == [(1,)])


def test_v3_invites_and_nicknames():
    print("[v3] Create Invite / Manage Invites / Change Nickname")
    owner = Client("alice"); assert auth_ok(owner)[0]
    ivy = join("ivy", owner); jon = join("jon", owner)
    owner.flush(0.5); ivy.flush(0.5); jon.flush(0.5)

    def create(c):
        c.send(pb.Packet.INVITE_CREATE_REQ, invite_create_req=pb.InviteCreateRequest(expires_at=0, max_uses=0))
        return c.wait(pb.Packet.INVITE_CREATE_RES)

    def codes(c):
        c.send(pb.Packet.INVITE_LIST_REQ, invite_list_req=pb.InviteListRequest())
        r = c.wait(pb.Packet.INVITE_LIST_RES)
        return (r.invite_list_res.success, {i.code for i in r.invite_list_res.invites}) if r else (False, set())

    r = create(ivy)
    check("member creates an invite (CREATE_INVITE on by default)", r is not None and r.invite_create_res.success,
          r.invite_create_res.message if r else None)
    ivy_code = r.invite_create_res.invite.code
    jon_code = create(jon).invite_create_res.invite.code
    ok, mine = codes(ivy)
    check("member lists only their own invites", ok and mine == {ivy_code}, str(mine))
    ok, every = codes(owner)
    check("MANAGE_INVITES lists everyone's", ok and {ivy_code, jon_code} <= every)
    ivy.send(pb.Packet.INVITE_REVOKE_REQ, invite_revoke_req=pb.InviteRevokeRequest(code=jon_code))
    r = ivy.wait(pb.Packet.INVITE_REVOKE_RES)
    check("member can't revoke someone else's invite", r is not None and not r.invite_revoke_res.success)
    ivy.send(pb.Packet.INVITE_REVOKE_REQ, invite_revoke_req=pb.InviteRevokeRequest(code=ivy_code))
    r = ivy.wait(pb.Packet.INVITE_REVOKE_RES)
    check("member revokes their own invite", r is not None and r.invite_revoke_res.success)
    check("member sets their own nickname (CHANGE_NICKNAME on by default)", mod_ok(set_nick(ivy, "ivy", "Ivy V")))

    set_everyone_perms(owner, ALL_MEMBER_BITS & ~(pb.PERM_CHANGE_NICKNAME | pb.PERM_CREATE_INVITE))
    ivy.flush(0.5)
    check("own nickname denied without CHANGE_NICKNAME", not mod_ok(set_nick(ivy, "ivy", "Ivy W")))
    r = create(ivy)
    check("invite create denied without CREATE_INVITE", r is not None and not r.invite_create_res.success)
    ok, _ = codes(ivy)
    check("invite list denied without CREATE_INVITE", not ok)
    r = create(owner)
    check("MANAGE_INVITES alone still creates", r is not None and r.invite_create_res.success)
    set_everyone_perms(owner, ALL_MEMBER_BITS)
    ivy.close(); jon.close(); owner.close()


def test_v3_peer_management():
    print("[v3] 'members with this role can manage each other'")
    owner = Client("alice"); assert auth_ok(owner)[0]
    kip = join("kip", owner); lev = join("lev", owner)
    bits = (pb.PERM_MANAGE_NICKNAMES | pb.PERM_MUTE_MEMBERS | pb.PERM_KICK_MEMBERS |
            pb.PERM_BAN_MEMBERS | pb.PERM_MODERATE_MEMBERS | pb.PERM_MANAGE_ROLES)
    mods = create_role(owner, "PeerMods", bits)
    assign_roles(owner, "kip", [mods.id]); assign_roles(owner, "lev", [mods.id])
    owner.flush(0.5); kip.flush(0.5); lev.flush(0.5)
    check("peer nickname denied while the flag is off", not mod_ok(set_nick(kip, "lev", "Levvy")))
    owner.send(pb.Packet.ROLE_UPDATE_REQ, role_update_req=pb.RoleUpdateRequest(
        role_id=mods.id, name="PeerMods", color=0, permissions=bits, position=mods.position, manage_each_other=True))
    r = owner.wait(pb.Packet.ROLE_ACTION_RES, pred=lambda p: p.role_action_res.action == "update")
    check("flag saved and echoed", r is not None and r.role_action_res.success and r.role_action_res.role.manage_each_other)
    kip.flush(0.5)
    check("peer nickname allowed with the flag", mod_ok(set_nick(kip, "lev", "Levvy")))
    check("peer server-mute allowed", mod_ok(voice_mod(kip, "lev", pb.VoiceModRequest.SERVER_MUTE)))
    voice_mod(kip, "lev", pb.VoiceModRequest.SERVER_UNMUTE)
    kip.send(pb.Packet.KICK_MEMBER_REQ, kick_member_req=pb.KickMemberRequest(username="lev", uid=uid_of("lev"), reason=""))
    check("peer kick still denied", not mod_ok(mod_res(kip, "kick")))
    kip.send(pb.Packet.BAN_MEMBER_REQ, ban_member_req=pb.BanMemberRequest(username="lev", uid=uid_of("lev"), reason=""))
    check("peer ban still denied", not mod_ok(mod_res(kip, "ban")))
    kip.send(pb.Packet.TIMEOUT_MEMBER_REQ, timeout_member_req=pb.TimeoutMemberRequest(username="lev", uid=uid_of("lev"), until=int(time.time()) + 60))
    check("peer timeout still denied", not mod_ok(mod_res(kip, "timeout")))
    r = assign_roles(kip, "lev", [])
    check("removing the shared role from a peer denied", r is not None and not r.role_action_res.success)
    check("the owner is never a peer", not mod_ok(set_nick(kip, "alice", "boss")))
    check("lev still in the server", not lev.is_closed(0.3))
    delete_role(owner, mods.id)
    kip.close(); lev.close(); owner.close()


def test_v3_voice_split_and_passes():
    print("[v3] voice moderation split (channel-scoped) + moves grant a voice pass")
    owner = Client("alice"); assert auth_ok(owner)[0]
    ev = everyone_role(owner)
    mox = join("max", owner); nox = join("nox", owner)
    muter = create_role(owner, "Muter", pb.PERM_MUTE_MEMBERS)
    assign_roles(owner, "max", [muter.id])
    nox.send(pb.Packet.JOIN_VOICE_REQ, join_voice_req=pb.JoinVoiceRequest(channel_id="voice-lounge"))
    owner.wait(pb.Packet.VOICE_PRESENCE_UPDATE, pred=lambda p: "nox" in p.voice_presence_update.active_users)
    mox.flush(0.5)
    check("MUTE_MEMBERS: server mute allowed", mod_ok(voice_mod(mox, "nox", pb.VoiceModRequest.SERVER_MUTE)))
    check("no DEAFEN_MEMBERS: deafen denied", not mod_ok(voice_mod(mox, "nox", pb.VoiceModRequest.SERVER_DEAFEN)))
    check("no MOVE_MEMBERS: disconnect denied", not mod_ok(voice_mod(mox, "nox", pb.VoiceModRequest.DISCONNECT)))
    voice_mod(mox, "nox", pb.VoiceModRequest.SERVER_UNMUTE)
    set_overwrite(owner, "voice-lounge", pb.ChannelOverwrite.ROLE, muter.id, allow=pb.PERM_MOVE_MEMBERS)
    mox.flush(0.5); nox.flush(0.5)
    check("MOVE_MEMBERS via an overwrite in the target's channel: move allowed",
          mod_ok(voice_mod(mox, "nox", pb.VoiceModRequest.MOVE, "voice-lounge-2")))
    nox.wait(pb.Packet.VOICE_FORCE_NOTIFY, timeout=2)
    check("...but not out of a channel where the mover lacks it",
          not mod_ok(voice_mod(mox, "nox", pb.VoiceModRequest.MOVE, "voice-lounge")))
    set_overwrite(owner, "voice-lounge", pb.ChannelOverwrite.ROLE, muter.id)

    vault = create_channel(owner, "vault", pb.ChannelInfo.VOICE).id
    set_overwrite(owner, vault, pb.ChannelOverwrite.ROLE, ev.id, deny=pb.PERM_VIEW_CHANNEL | pb.PERM_CONNECT_VOICE)
    set_overwrite(owner, "voice-lounge-2", pb.ChannelOverwrite.ROLE, muter.id, allow=pb.PERM_MOVE_MEMBERS)
    nox.flush(0.5); mox.flush(0.5)
    nox.send(pb.Packet.JOIN_VOICE_REQ, join_voice_req=pb.JoinVoiceRequest(channel_id=vault))
    check("nox can't join the vault himself", not mod_ok(mod_res(nox, "voice")))
    r = voice_mod(mox, "nox", pb.VoiceModRequest.MOVE, vault)
    check("a mover who can't see the destination is refused", not mod_ok(r) and r is not None
          and "Unknown voice channel" in r.mod_action_res.message, r.mod_action_res.message if r else None)
    nox.flush(0.5)
    check("owner moves nox into a channel he can neither view nor join",
          mod_ok(voice_mod(owner, "nox", pb.VoiceModRequest.MOVE, vault)))
    cl = channel_list(nox)
    vc = pb.PERM_VIEW_CHANNEL | pb.PERM_CONNECT_VOICE
    check("pass: the vault lands in nox's list with VIEW + CONNECT",
          cl is not None and vault in cl and cl[vault].my_permissions & vc == vc)
    n = nox.wait(pb.Packet.VOICE_FORCE_NOTIFY, timeout=2)
    check("...then the MOVED notify names it", n is not None and n.voice_force_notify.channel_id == vault)
    vp = nox.wait(pb.Packet.VOICE_PRESENCE_UPDATE, timeout=2,
                  pred=lambda p: p.voice_presence_update.channel_id == vault and "nox" in p.voice_presence_update.active_users)
    check("nox receives the vault's voice presence", vp is not None)
    check("SPEAK follows the normal chain (not suppressed)",
          vp is not None and not any(s.is_suppressed for s in vp.voice_presence_update.user_states if s.username == "nox"))
    nox.flush(0.3)
    nox.send(pb.Packet.LEAVE_VOICE_REQ, leave_voice_req=pb.LeaveVoiceRequest())
    cl = channel_list(nox)
    check("leaving ends the pass: the vault leaves nox's list", cl is not None and vault not in cl)
    nox.send(pb.Packet.JOIN_VOICE_REQ, join_voice_req=pb.JoinVoiceRequest(channel_id=vault))
    check("...and he can't rejoin it", not mod_ok(mod_res(nox, "voice")))
    set_overwrite(owner, "voice-lounge-2", pb.ChannelOverwrite.ROLE, muter.id)
    owner.send(pb.Packet.CHANNEL_DELETE_REQ, channel_delete_req=pb.ChannelDeleteRequest(channel_id=vault))
    owner.wait(pb.Packet.CHANNEL_ACTION_RES, pred=lambda p: p.channel_action_res.action == "delete")
    delete_role(owner, muter.id)
    mox.close(); nox.close(); owner.close()


def test_v3_speak():
    print("[v3] SPEAK: suppressed in presence, audio dropped at the relay")
    owner = Client("alice"); assert auth_ok(owner)[0]
    ev = everyone_role(owner)
    orr = join("orr", owner); pia = join("pia", owner)
    for c in (orr, pia):
        c.send(pb.Packet.JOIN_VOICE_REQ, join_voice_req=pb.JoinVoiceRequest(channel_id="voice-lounge"))
    owner.wait(pb.Packet.VOICE_PRESENCE_UPDATE, pred=lambda p: "pia" in p.voice_presence_update.active_users and "orr" in p.voice_presence_update.active_users)
    uo = socket.socket(socket.AF_INET, socket.SOCK_DGRAM); uo.bind((HOST, 0)); uo.settimeout(2)
    up = socket.socket(socket.AF_INET, socket.SOCK_DGRAM); up.bind((HOST, 0)); up.settimeout(2)
    udp_bind(up, pia.jwt, 8083); udp_bind(uo, orr.jwt, 8083)
    uo.sendto(udp_audio_packet(orr.jwt, 1, b"o0"), (HOST, 8083))

    def heard(timeout=1.0):
        up.settimeout(timeout)
        try:
            return up.recv(2048)
        except socket.timeout:
            return None

    got = heard(2.0)
    check("baseline: orr's audio relayed", got is not None and got[-2:] == b"o0")
    owner.flush(0.3)
    set_overwrite(owner, "voice-lounge", pb.ChannelOverwrite.ROLE, ev.id, deny=pb.PERM_SPEAK)
    vp = owner.wait(pb.Packet.VOICE_PRESENCE_UPDATE, timeout=2, pred=lambda p: p.voice_presence_update.channel_id == "voice-lounge"
                    and any(s.username == "orr" and s.is_suppressed for s in p.voice_presence_update.user_states))
    check("presence marks orr suppressed once SPEAK is denied", vp is not None)
    time.sleep(0.2)
    uo.sendto(udp_audio_packet(orr.jwt, 2, b"hush"), (HOST, 8083))
    check("audio from a member without SPEAK dropped by the relay", heard() is None)
    owner.flush(0.3)
    set_overwrite(owner, "voice-lounge", pb.ChannelOverwrite.ROLE, ev.id)
    vp = owner.wait(pb.Packet.VOICE_PRESENCE_UPDATE, timeout=2, pred=lambda p: p.voice_presence_update.channel_id == "voice-lounge"
                    and any(s.username == "orr" and not s.is_suppressed for s in p.voice_presence_update.user_states))
    check("cleared: no longer suppressed", vp is not None)
    time.sleep(0.2)
    uo.sendto(udp_audio_packet(orr.jwt, 3, b"ok"), (HOST, 8083))
    got = heard(2.0)
    check("...and relayed again", got is not None and got[-2:] == b"ok")
    uo.close(); up.close()
    orr.close(); pia.close(); owner.close()


def test_v3_embed_links():
    print("[v3] EMBED_LINKS: server marks the message, history + edits carry it")
    owner = Client("alice"); assert auth_ok(owner)[0]
    ev = everyone_role(owner)
    qin = join("qin", owner)
    set_overwrite(owner, "general", pb.ChannelOverwrite.ROLE, ev.id, deny=pb.PERM_EMBED_LINKS)
    owner.flush(0.5); qin.flush(0.5)
    qin.send(pb.Packet.CHANNEL_MSG, channel_msg=pb.ChannelMessage(channel_id="general", content="https://example.com/a", nonce="emb-1"))
    m = owner.wait(pb.Packet.CHANNEL_MSG, pred=lambda p: p.channel_msg.nonce == "emb-1")
    check("message from a sender without EMBED_LINKS is marked", m is not None and m.channel_msg.suppress_embeds)
    owner.send(pb.Packet.CHANNEL_MSG, channel_msg=pb.ChannelMessage(channel_id="general", content="https://example.com/b", nonce="emb-2"))
    m2 = qin.wait(pb.Packet.CHANNEL_MSG, pred=lambda p: p.channel_msg.nonce == "emb-2")
    check("the owner's message isn't", m2 is not None and not m2.channel_msg.suppress_embeds)
    qin.send(pb.Packet.CHANNEL_MSG, channel_msg=pb.ChannelMessage(channel_id="general", content="x", nonce="emb-3", suppress_embeds=False))
    m3 = owner.wait(pb.Packet.CHANNEL_MSG, pred=lambda p: p.channel_msg.nonce == "emb-3")
    check("a client can't clear the mark itself", m3 is not None and m3.channel_msg.suppress_embeds)
    mid = m.channel_msg.id if m else 0
    owner.send(pb.Packet.CHANNEL_HISTORY_REQ, channel_history_req=pb.ChannelHistoryRequest(channel_id="general", before_id=0, limit=20))
    h = owner.wait(pb.Packet.CHANNEL_HISTORY_RES)
    row = next((x for x in h.channel_history_res.messages if x.id == mid), None) if h else None
    check("history carries the mark", row is not None and row.suppress_embeds)
    set_overwrite(owner, "general", pb.ChannelOverwrite.ROLE, ev.id)
    qin.flush(0.5); owner.flush(0.5)
    qin.send(pb.Packet.MESSAGE_EDIT_REQ, message_edit_req=pb.MessageEditReq(channel_id="general", message_id=mid, content="https://example.com/c"))
    e = owner.wait(pb.Packet.CHANNEL_MESSAGE_EDITED, pred=lambda p: p.channel_message_edited.message_id == mid)
    check("an edit re-evaluates (EMBED_LINKS back → unmarked)", e is not None and not e.channel_message_edited.suppress_embeds)
    check("...and persists", sql("select suppress_embeds from messages where id=?", mid) == [(0,)])
    qin.close(); owner.close()


def test_v3_thumbnail_scope():
    print("[v3] stream thumbnails don't leak out of hidden channels")
    owner = Client("alice"); assert auth_ok(owner)[0]
    ev = everyone_role(owner)
    rex = join("rex", owner); sid = join("sid", owner)
    back = create_channel(owner, "backroom", pb.ChannelInfo.VOICE).id
    set_overwrite(owner, back, pb.ChannelOverwrite.ROLE, ev.id, deny=pb.PERM_VIEW_CHANNEL)
    set_overwrite(owner, back, pb.ChannelOverwrite.MEMBER, "rex", allow=pb.PERM_VIEW_CHANNEL)
    rex.send(pb.Packet.JOIN_VOICE_REQ, join_voice_req=pb.JoinVoiceRequest(channel_id=back))
    rex.send(pb.Packet.START_STREAM_REQ, start_stream_req=pb.StartStreamRequest(channel_id=back, target_fps=30))
    time.sleep(0.5)
    rex.send(pb.Packet.STREAM_THUMBNAIL_UPDATE, stream_thumbnail_update=pb.StreamThumbnailUpdate(
        channel_id=back, thumbnail_data=b"\x07" * 2048))
    time.sleep(0.5)

    def fetch(c):
        c.send(pb.Packet.FETCH_STREAM_THUMBNAIL_REQ, fetch_stream_thumbnail_req=pb.FetchStreamThumbnailReq(owner_uid=uid_of("rex")))
        r = c.wait(pb.Packet.FETCH_STREAM_THUMBNAIL_RES, timeout=3)
        return r.fetch_stream_thumbnail_res.thumbnail_data if r is not None else None

    d = fetch(sid)
    check("a member who can't see the channel gets no thumbnail", d is not None and len(d) == 0, None if d is None else len(d))
    d = fetch(owner)
    check("a member who can see it does", d is not None and len(d) == 2048)
    rex.close()
    time.sleep(0.5)
    d = fetch(owner)
    check("no live stream, no thumbnail", d is not None and len(d) == 0)
    owner.send(pb.Packet.CHANNEL_DELETE_REQ, channel_delete_req=pb.ChannelDeleteRequest(channel_id=back))
    owner.wait(pb.Packet.CHANNEL_ACTION_RES, pred=lambda p: p.channel_action_res.action == "delete")
    sid.close(); owner.close()


def test_v3_category_sync():
    print("[v3] category permission sync: opt-in per category, never silent")
    owner = Client("alice"); assert auth_ok(owner)[0]
    ev = everyone_role(owner)
    evk = (0, str(ev.id))
    tom = join("tom", owner); val = join("val", owner)
    cat = create_channel(owner, "Staff Area", pb.ChannelInfo.CATEGORY)
    check("a new category is a plain group", not cat.category_sync)
    cat = cat.id
    child = create_channel(owner, "staff-talk", pb.ChannelInfo.TEXT, category_id=cat)
    check("a channel created in a plain group doesn't sync", not child.permissions_synced)
    child = child.id
    tom.flush(0.5)
    set_overwrite(owner, cat, pb.ChannelOverwrite.ROLE, ev.id, deny=pb.PERM_VIEW_CHANNEL)
    cl = fresh_list(tom)
    check("a plain group's rows don't touch its channels (header shown for its visible channel)",
          cl is not None and child in cl and cat in cl and cl[cat].my_permissions == 0)
    r = set_category_sync(owner, cat, True)
    check("category sync switched on", r is not None and r.channel_action_res.success and r.channel_action_res.channel.category_sync,
          r.channel_action_res.message if r else None)
    cl = fresh_list(tom)
    check("switching it on changes no channel", cl is not None and child in cl and perm_synced(child) == 0)
    later = create_channel(owner, "staff-later", pb.ChannelInfo.TEXT, category_id=cat)
    check("a channel created in a syncing category follows it", later.permissions_synced)
    later = later.id
    cl = fresh_list(tom)
    check("...so it's hidden with the category", cl is not None and later not in cl)

    r = sync_perms(owner, child, category_id="general")
    check("sync refused when the channel isn't in the category the user agreed to", r is not None and not r.channel_action_res.success)
    tom.flush(0.3)
    r = sync_perms(owner, child, category_id=cat)
    check("explicit sync", r is not None and r.channel_action_res.success and r.channel_action_res.channel.permissions_synced,
          r.channel_action_res.message if r else None)
    cl = fresh_list(tom)
    check("after the sync tom loses the channel and the header", cl is not None and child not in cl and cat not in cl)
    check("a synced channel reports its category's rows", list_overwrites(owner, child) == {evk: (0, pb.PERM_VIEW_CHANNEL)})

    tom.flush(0.3)
    r = set_overwrite(owner, child, pb.ChannelOverwrite.MEMBER, "tom", allow=pb.PERM_VIEW_CHANNEL)
    check("editing a synced channel un-syncs it", r is not None and r.channel_action_res.success
          and not r.channel_action_res.channel.permissions_synced)
    rows = list_overwrites(owner, child)
    check("...after copying the category's rows onto it",
          rows == {evk: (0, pb.PERM_VIEW_CHANNEL), (1, "tom"): (pb.PERM_VIEW_CHANNEL, 0)}, str(rows))
    cl = fresh_list(tom)
    check("tom sees the channel, and its category header with it", cl is not None and child in cl and cat in cl and later not in cl)

    # escalation + lock-out guards: val may manage the child's overwrites but lacks MANAGE_MESSAGES
    keeper = create_role(owner, "Keeper", 0)
    set_overwrite(owner, child, pb.ChannelOverwrite.MEMBER, "val", allow=pb.PERM_VIEW_CHANNEL | pb.PERM_MANAGE_ROLES)
    set_overwrite(owner, child, pb.ChannelOverwrite.ROLE, keeper.id, allow=pb.PERM_MANAGE_MESSAGES)
    val.flush(0.5)
    r = sync_perms(val, child)
    check("sync refused when it would change bits the actor lacks", r is not None and not r.channel_action_res.success,
          r.channel_action_res.message if r else None)
    set_overwrite(owner, child, pb.ChannelOverwrite.ROLE, keeper.id)
    r = sync_perms(val, child)
    check("sync refused when it would lock the actor out", r is not None and not r.channel_action_res.success
          and "access" in r.channel_action_res.message, r.channel_action_res.message if r else None)
    rows = list_overwrites(owner, child)
    check("...and the channel's own rows were restored", rows is not None and (1, "val") in rows and (1, "tom") in rows, str(rows))
    set_overwrite(owner, child, pb.ChannelOverwrite.MEMBER, "val")
    set_overwrite(owner, child, pb.ChannelOverwrite.MEMBER, "tom")
    r = sync_perms(owner, child, category_id=cat)
    check("owner re-syncs", r is not None and r.channel_action_res.success and perm_synced(child) == 1)

    # Moves never change what a channel allows. (Fresh session: the
    # management bucket is 20 burst / 2 per s.)
    owner.close()
    owner = Client("alice"); assert auth_ok(owner)[0]
    ids = channel_order(); ids.remove(child); ids.insert(ids.index("general") + 1, child)
    tom.flush(0.3)
    r = reorder(owner, ids)
    check("synced channel dragged out of the category", r is not None and r.channel_action_res.success,
          r.channel_action_res.message if r else None)
    check("...keeps the category's rows as its own, unsynced",
          perm_synced(child) == 0 and list_overwrites(owner, child) == {evk: (0, pb.PERM_VIEW_CHANNEL)})
    cl = fresh_list(tom)
    check("...so it stays hidden", cl is not None and child not in cl)
    ids = channel_order(); ids.remove(child); ids.insert(ids.index(later), child)
    r = reorder(owner, ids)
    check("dragged back in, it doesn't sync by itself", r is not None and r.channel_action_res.success and perm_synced(child) == 0)

    # Turning sync off keeps every follower's permissions.
    tom.flush(0.3)
    r = set_category_sync(owner, cat, False)
    check("category sync switched off", r is not None and r.channel_action_res.success and not r.channel_action_res.channel.category_sync)
    check("...its follower keeps the rows as its own",
          perm_synced(later) == 0 and list_overwrites(owner, later) == {evk: (0, pb.PERM_VIEW_CHANNEL)})
    cl = fresh_list(tom)
    check("...and stays hidden", cl is not None and later not in cl)
    r = sync_perms(owner, later)
    check("a plain group can't be synced with", r is not None and not r.channel_action_res.success)

    # Sync all.
    set_category_sync(owner, cat, True)
    r = sync_perms(owner, cat)
    check("syncing the category syncs every channel in it", r is not None and r.channel_action_res.success
          and perm_synced(child) == 1 and perm_synced(later) == 1, r.channel_action_res.message if r else None)
    r = sync_perms(owner, "general")
    check("an uncategorized channel can't sync", r is not None and not r.channel_action_res.success)

    owner.send(pb.Packet.CHANNEL_DELETE_REQ, channel_delete_req=pb.ChannelDeleteRequest(channel_id=cat))
    r = owner.wait(pb.Packet.CHANNEL_ACTION_RES, pred=lambda p: p.channel_action_res.action == "delete")
    check("category deleted", r is not None and r.channel_action_res.success)
    cl = fresh_list(tom)
    check("its synced channels stay hidden (rows copied, not inherited from the next category)",
          cl is not None and child not in cl and later not in cl, str(sorted(cl)) if cl else None)
    check("...as their own rows", list_overwrites(owner, child) == {evk: (0, pb.PERM_VIEW_CHANNEL)})
    for ch in (child, later):
        owner.send(pb.Packet.CHANNEL_DELETE_REQ, channel_delete_req=pb.ChannelDeleteRequest(channel_id=ch))
        owner.wait(pb.Packet.CHANNEL_ACTION_RES, pred=lambda p: p.channel_action_res.action == "delete")
    delete_role(owner, keeper.id)
    tom.close(); val.close(); owner.close()


def test_presence_follows_visibility():
    print("[presence] a voice channel that becomes visible arrives with its roster")
    owner = Client("alice"); assert auth_ok(owner)[0]
    ev = everyone_role(owner)
    ola = join("ola", owner)
    hideout = create_channel(owner, "hideout", pb.ChannelInfo.VOICE).id
    set_overwrite(owner, hideout, pb.ChannelOverwrite.ROLE, ev.id, deny=pb.PERM_VIEW_CHANNEL)
    owner.flush(0.3); ola.flush(0.3)
    owner.send(pb.Packet.JOIN_VOICE_REQ, join_voice_req=pb.JoinVoiceRequest(channel_id=hideout))
    owner.wait(pb.Packet.VOICE_PRESENCE_UPDATE, pred=lambda p: p.voice_presence_update.channel_id == hideout)
    check("hidden: no presence for it reaches the member",
          ola.wait(pb.Packet.VOICE_PRESENCE_UPDATE, timeout=1.0, pred=lambda p: p.voice_presence_update.channel_id == hideout) is None)
    ola.flush(0.3)
    set_overwrite(owner, hideout, pb.ChannelOverwrite.MEMBER, "ola", allow=pb.PERM_VIEW_CHANNEL)
    cl = channel_list(ola)
    check("granted VIEW: the channel lands in the member's list", cl is not None and hideout in cl)
    vp = ola.wait(pb.Packet.VOICE_PRESENCE_UPDATE, timeout=2, pred=lambda p: p.voice_presence_update.channel_id == hideout)
    check("...followed by its current roster", vp is not None and "alice" in vp.voice_presence_update.active_users)
    owner.send(pb.Packet.LEAVE_VOICE_REQ, leave_voice_req=pb.LeaveVoiceRequest())
    owner.send(pb.Packet.CHANNEL_DELETE_REQ, channel_delete_req=pb.ChannelDeleteRequest(channel_id=hideout))
    owner.wait(pb.Packet.CHANNEL_ACTION_RES, pred=lambda p: p.channel_action_res.action == "delete")
    ola.close(); owner.close()


# ---------------------------------------------------------------- pre-1.0 review (2026-10-17)

def delete_channel(owner, channel_id):
    owner.send(pb.Packet.CHANNEL_DELETE_REQ, channel_delete_req=pb.ChannelDeleteRequest(channel_id=channel_id))
    owner.wait(pb.Packet.CHANNEL_ACTION_RES, pred=lambda p: p.channel_action_res.action == "delete")


def test_s2_channel_msg_whitelist():
    print("[S2] the CHANNEL_MSG broadcast is built from a whitelist; refusals answer the nonce")
    owner = Client("alice"); assert auth_ok(owner)[0]
    wl = join("wlist", owner)
    room = create_channel(owner, "wl-room", pb.ChannelInfo.TEXT).id
    cat = create_channel(owner, "wl-cat", pb.ChannelInfo.CATEGORY).id
    owner.flush(0.5); wl.flush(0.5)
    owner.send(pb.Packet.CHANNEL_MSG, channel_msg=pb.ChannelMessage(channel_id=room, content="wl-parent", nonce="wl-p"))
    parent = wl.wait(pb.Packet.CHANNEL_MSG, timeout=2, pred=lambda p: p.channel_msg.nonce == "wl-p")
    pid = parent.channel_msg.id if parent else 0
    sql("insert into attachments(message_id, kind, filename, created_at) values(?, 1, 'clip.mp4', ?)", pid, int(time.time()))
    owner.flush(0.3); wl.flush(0.3)
    # Every server-owned field forged, a stub attachment that isn't an
    # upload, and a field number this server has never heard of.
    cm = pb.ChannelMessage(channel_id=room, content="wl-forged", nonce="wl-1", reply_to=pid,
                           sender="mallory", sender_uid=1, id=777, timestamp=1, edited_at=12345,
                           reply_to_sender="mallory", reply_to_sender_uid=1, reply_to_content="forged quote",
                           reply_to_attachment_kinds=[3, 3], reply_to_envelope=b"x", sealed_sender="mallory",
                           reply_to_sealed_sender="m",
                           attachments=[pb.Attachment(id=999999, filename="evil.exe", url="https://evil")])
    cm.MergeFromString(b"\xc2\x3e\x08FORGEDXX")   # field 1000, length-delimited
    check("whitelist: the forged request carries the unknown field",
          b"FORGEDXX" in pb.Packet(type=pb.Packet.CHANNEL_MSG, channel_msg=cm).SerializeToString())
    wl.send(pb.Packet.CHANNEL_MSG, channel_msg=cm)
    bc = owner.wait(pb.Packet.CHANNEL_MSG, timeout=2, pred=lambda p: p.channel_msg.content == "wl-forged")
    m = bc.channel_msg if bc else None
    check("whitelist: sender, id and time are the server's", m is not None and m.sender == "wlist"
          and m.sender_uid == uid_of("wlist") and m.id > 0 and m.id != 777 and m.timestamp > 1)
    check("whitelist: client edited_at / sealed names / reply envelope dropped", m is not None and m.edited_at == 0
          and m.sealed_sender == "" and m.reply_to_sealed_sender == "" and m.reply_to_envelope == b"")
    check("whitelist: the quote is the parent's, kinds not appended", m is not None and m.reply_to == pid
          and m.reply_to_sender == "alice" and m.reply_to_sender_uid == uid_of("alice")
          and m.reply_to_content == "wl-parent" and list(m.reply_to_attachment_kinds) == [1])
    check("whitelist: a stub that isn't an upload doesn't ride along", m is not None and len(m.attachments) == 0)
    check("whitelist: no unknown field and no token relayed", bc is not None
          and b"FORGEDXX" not in bc.SerializeToString() and bc.auth_token == "")
    check("whitelist: the nonce is echoed", m is not None and m.nonce == "wl-1")
    # Refusals name the nonce so exactly that optimistic bubble goes.
    def rejected(nonce, **kw):
        wl.send(pb.Packet.CHANNEL_MSG, channel_msg=pb.ChannelMessage(nonce=nonce, **kw))
        return wl.wait(pb.Packet.CHANNEL_MSG_REJECTED, timeout=2, pred=lambda p: p.channel_msg_rejected.nonce == nonce)
    wl.flush(0.3)
    wl.send(pb.Packet.CHANNEL_MSG, channel_msg=pb.ChannelMessage(channel_id=room, content="wl-long-nonce", nonce="n" * 65))
    rj = wl.wait(pb.Packet.CHANNEL_MSG_REJECTED, timeout=2, pred=lambda p: p.channel_msg_rejected.channel_id == room)
    check("a nonce over 64 bytes is refused, not echoed", rj is not None and rj.channel_msg_rejected.nonce == ""
          and sql("select count(*) from messages where content='wl-long-nonce'") == [(0,)])
    wl.send(pb.Packet.CHANNEL_MSG, channel_msg=pb.ChannelMessage(channel_id=room, content="wl-64", nonce="n" * 64))
    bc = owner.wait(pb.Packet.CHANNEL_MSG, timeout=2, pred=lambda p: p.channel_msg.content == "wl-64")
    check("a 64-byte nonce still goes through", bc is not None and bc.channel_msg.nonce == "n" * 64)
    check("oversized text is refused with its nonce", rejected("wl-big", channel_id=room, content="x" * (64 * 1024 + 1)) is not None)
    check("an unknown channel is refused with its nonce", rejected("wl-nochan", channel_id="no-such-channel", content="hi") is not None)
    check("a category is refused with its nonce", rejected("wl-cat", channel_id=cat, content="hi") is not None)
    check("an empty message is refused with its nonce", rejected("wl-empty", channel_id=room) is not None)
    delete_channel(owner, room); delete_channel(owner, cat)
    wl.close(); owner.close()


def test_s15_channel_id_cap():
    print("[S15] over-long channel ids are dropped at the edge; the longest real ids still work")
    owner = Client("alice"); assert auth_ok(owner)[0]
    owner.flush(0.5)
    for cid in ("x" * 37, "x" * (512 * 1024)):
        owner.send(pb.Packet.CHANNEL_HISTORY_REQ, channel_history_req=pb.ChannelHistoryRequest(channel_id=cid, limit=5))
    r = owner.wait(pb.Packet.CHANNEL_HISTORY_RES, timeout=1)
    check("history for an over-long channel id gets no answer", r is None)
    owner.send(pb.Packet.CHANNEL_MSG, channel_msg=pb.ChannelMessage(channel_id="y" * 100, content="s15-long", nonce="s15"))
    r = owner.wait(pb.Packet.CHANNEL_MSG_REJECTED, timeout=1, pred=lambda p: p.channel_msg_rejected.nonce == "s15")
    check("a message to an over-long channel id is dropped", r is None
          and sql("select count(*) from messages where content='s15-long'") == [(0,)])
    # The longest ids the server makes: a 32-byte slug, its "-2" twin,
    # and (seeded) a "-999" one at exactly kMaxChannelIdBytes = 36.
    c1 = create_channel(owner, "a" * 40, pb.ChannelInfo.TEXT).id
    c2 = create_channel(owner, "a" * 40, pb.ChannelInfo.TEXT).id
    check("slugs are capped at 32 bytes; suffixed ids extend them", len(c1) == 32 and c2 == c1 + "-2", f"{c1} {c2}")
    c36 = "b" * 32 + "-999"
    sql("insert into channels(id, name, type, position) values(?, 's15-edge', 0, 500)", c36)
    c3 = create_channel(owner, "s15-refresh", pb.ChannelInfo.TEXT).id   # the server learns of the seeded row
    owner.flush(0.3)
    for cid in (c2, c36):
        owner.send(pb.Packet.CHANNEL_HISTORY_REQ, channel_history_req=pb.ChannelHistoryRequest(channel_id=cid, limit=5))
        r = owner.wait(pb.Packet.CHANNEL_HISTORY_RES, timeout=2, pred=lambda p: p.channel_history_res.channel_id == cid)
        check(f"a {len(cid)}-byte id is served", r is not None)
    check("the session survives the dropped requests", not owner.is_closed(0.2))
    for cid in (c1, c2, c36, c3):
        delete_channel(owner, cid)
    owner.close()


def test_s17_write_backlog_bytes():
    print("[S17] a session that stops reading is dropped once ~16 MB is queued, not 1024 frames")
    owner = Client("alice"); assert auth_ok(owner)[0]
    slow = join("slowr", owner)
    room = create_channel(owner, "s17-room", pb.ChannelInfo.TEXT).id
    now = int(time.time())
    c = sqlite3.connect(DB)
    for i in range(30):   # 100 KB rows (seeded past the 64 KB send cap): a ~3 MB history page
        c.execute("insert into messages(channel_id, sender, content, timestamp) values(?, 'alice', ?, ?)",
                  (room, f"{i:03d}" + "z" * 100000, now))
    c.commit(); c.close()
    slow.flush(0.5)
    for _ in range(20):   # ~60 MB of answers; the client reads none of it
        slow.send(pb.Packet.CHANNEL_HISTORY_REQ, channel_history_req=pb.ChannelHistoryRequest(channel_id=room, limit=50))
    time.sleep(2.0)
    check("the non-reading session is closed", slow.is_closed(10.0))
    owner.flush(0.3)
    owner.send(pb.Packet.MEMBER_LIST_REQ, member_list_req=pb.MemberListRequest())
    check("everyone else is unaffected", owner.wait(pb.Packet.MEMBER_LIST_RES, timeout=2) is not None)
    delete_channel(owner, room)
    slow.close(); owner.close()


def peer_closed(sock, wait):
    """True if the peer closes `sock` within `wait` seconds, False if it is
    still open then (socket_closed() can't tell: on Python 3.10+ its
    OSError clause also catches the timeout)."""
    sock.settimeout(wait)
    try:
        return sock.recv(16) == b""
    except (socket.timeout, TimeoutError):
        return False
    except OSError:
        return True


def fd_count(proc):
    return len(os.listdir(f"/proc/{proc.pid}/fd"))


def settle_fds(proc, want, wait=2.0):
    """The server's descriptor count once it drops to `want` (or after `wait`)."""
    end = time.time() + wait
    n = fd_count(proc)
    while n > want and time.time() < end:
        time.sleep(0.1)
        n = fd_count(proc)
    return n


def test_s16_connection_caps():
    print("[S16] connection caps (server-wide, per address, per second); every error path closes at once")
    proc = start_server(extra_env={"DECIBELL_MAX_CONNECTIONS": "40", "DECIBELL_AUTH_TIMEOUT_SECONDS": "30"})
    try:
        time.sleep(0.5)
        base = fd_count(proc)
        # Server-wide: 40 chat connections (each from its own address), the 41st refused.
        held = [connect(8082) for _ in range(40)]
        extra = connect(8082)
        check("chat: the connection over the server-wide cap is refused", peer_closed(extra, 1.0))
        check("chat: the admitted ones stay open", not any(peer_closed(s, 0.05) for s in held[:5]))
        extra.close()
        # A peer that hangs up mid-handshake frees its slot (and descriptor) at
        # once, not at the 30 s auth deadline.
        held.pop().close()
        time.sleep(0.3)
        again = connect(8082)
        check("chat: a closed connection's slot is free again at once", not peer_closed(again, 0.5))
        held.append(again)
        for s in held: s.close()
        check("chat: hung-up pre-auth connections release their descriptors at once", settle_fds(proc, base) <= base,
              f"{fd_count(proc)} > {base}")
        # Signed-in sessions whose peer goes away: the socket closes at once
        # (it used to sit in CLOSE_WAIT until the 90 s idle deadline).
        cs = [Client("alice") for _ in range(3)]
        check("chat: sessions sign in", all(auth_ok(c)[0] for c in cs))
        for c in cs: c.close()
        check("chat: dropped sessions release their descriptors at once", settle_fds(proc, base) <= base,
              f"{fd_count(proc)} > {base}")
        # Attachments: 20 connections (half the chat cap).
        att = [connect(8085) for _ in range(20)]
        extra = connect(8085)
        check("attachments: the connection over the cap is refused", peer_closed(extra, 1.0))
        extra.close()
        for s in att: s.close()
        settle_fds(proc, base)
        # A connection that hangs up idle after the handshake, and one that
        # hangs up mid-head: both close at once (not at the 30 s deadline).
        k = raw_tls(8085)
        h = raw_tls(8085)
        h.sendall(b"GET /attachments/1 HTTP/1.1\r\nHost: x\r\n")
        time.sleep(0.2)
        k.close(); h.close()
        check("attachments: hung-up connections release their descriptors at once", settle_fds(proc, base) <= base,
              f"{fd_count(proc)} > {base}")
    finally:
        stop_server(proc)
    # Per address (default server-wide cap).
    proc = start_server(extra_env={"DECIBELL_AUTH_TIMEOUT_SECONDS": "30"})
    try:
        # New connections: 60 at once, then 10 a second.
        burst = [connect(8082, src="127.0.250.1") for _ in range(100)]
        time.sleep(0.5)
        refused = sum(1 for s in burst if peer_closed(s, 0.02))
        check("chat: a burst from one address is cut after ~60", refused >= 20 and len(burst) - refused >= 60,
              f"refused={refused}")
        for s in burst: s.close()
        # Per address, open: 256 at a time (paced under the rate).
        src = "127.0.250.2"
        opened = [connect(8082, src=src) for _ in range(60)]
        while len(opened) < 256:
            time.sleep(0.11)
            opened.append(connect(8082, src=src))
        time.sleep(0.2)
        over = connect(8082, src=src)
        check("chat: the 257th open connection from one address is refused", peer_closed(over, 1.0)
              and not peer_closed(opened[-1], 0.05))
        over.close()
        for s in opened: s.close()
    finally:
        stop_server(proc)


# ---------------------------------------------------------------- pre-1.0 review: availability (A1-A3)

FRAME_CAP = 2 * 1024 * 1024   # the client drops the connection on a bigger frame


def history_page(c, channel_id, **kw):
    c.send(pb.Packet.CHANNEL_HISTORY_REQ, channel_history_req=pb.ChannelHistoryRequest(channel_id=channel_id, **kw))
    r = c.wait(pb.Packet.CHANNEL_HISTORY_RES, timeout=5, pred=lambda p: p.channel_history_res.channel_id == channel_id
               and p.channel_history_res.around_id == kw.get("around_id", 0)
               and p.channel_history_res.after_id == kw.get("after_id", 0))
    return r.channel_history_res if r else None


def seed_rows(channel_id, rows, sender="alice"):
    """(content, reply_to, envelope) rows straight into the DB, past the
    send rate limit and the 64 KB check; their ids, in order."""
    c = sqlite3.connect(DB)
    now = int(time.time())
    ids = [c.execute("insert into messages(channel_id, sender, sender_uid, content, timestamp, reply_to, envelope) "
                     "values(?,?,?,?,?,?,?)", (channel_id, sender, uid_of(sender), content, now, reply_to, envelope)).lastrowid
           for content, reply_to, envelope in rows]
    c.commit(); c.close()
    return ids


def unknown_field(payload):
    """Field 1000, length-delimited: a field this build has never heard of."""
    n, out = len(payload), b"\xc2\x3e"
    while True:
        b, n = n & 0x7f, n >> 7
        out += bytes([b | (0x80 if n else 0)])
        if not n:
            return out + payload


def send_raw(c, packet):
    packet.auth_token = c.jwt
    data = packet.SerializeToString()
    c.s.sendall(struct.pack(">I", len(data)) + data)


def test_a1_history_budget():
    print("[A1] history pages fit the client's frame cap and page on without gaps; reply quotes are one line")
    owner = Client("alice"); assert auth_ok(owner)[0]
    room = create_channel(owner, "a1-history", pb.ChannelInfo.TEXT).id
    big = "€" * 21800   # 65,400 bytes, under the 64 KB send cap
    parent = seed_rows(room, [(big, 0, None)])[0]
    replies = seed_rows(room, [(f"{i:03d}" + "r" * 65000, parent, None) for i in range(30)])
    everything = [parent] + replies
    owner.flush(0.3)
    # Each reply used to carry its own 64 KB and its parent's: ~3.9 MB a page.
    owner.max_frame = 0
    page = history_page(owner, room, limit=50)
    got = [m.id for m in page.messages] if page else []
    check("history: the latest page fits the frame cap", page is not None and owner.max_frame <= FRAME_CAP, owner.max_frame)
    check("history: it keeps the newest rows and says older ones exist", bool(got) and got == everything[-len(got):]
          and len(got) < len(everything) and page.has_more and not page.has_more_after, f"{len(got)} rows")
    q = page.messages[-1] if got else None
    check("history: a reply quotes 512 bytes of its parent, cut on a character", q is not None and q.reply_to == parent
          and q.reply_to_content == "€" * 170, len(q.reply_to_content.encode()) if q else None)
    seen = list(got)
    for _ in range(10):
        if not page or not page.has_more:
            break
        page = history_page(owner, room, before_id=seen[0], limit=50)
        seen = ([m.id for m in page.messages] if page else []) + seen
    check("history: older pages walk the whole channel without a gap", seen == everything and owner.max_frame <= FRAME_CAP,
          f"{len(seen)} of {len(everything)}")
    page = history_page(owner, room, after_id=parent, limit=50)
    seen = [m.id for m in page.messages] if page else []
    check("history: a newer page keeps the oldest rows and says newer ones exist", bool(seen)
          and seen == replies[:len(seen)] and len(seen) < len(replies) and page.has_more_after)
    for _ in range(10):
        if not page or not page.has_more_after:
            break
        page = history_page(owner, room, after_id=seen[-1], limit=50)
        seen += [m.id for m in page.messages] if page else []
    check("history: newer pages walk to the end without a gap", seen == replies and owner.max_frame <= FRAME_CAP,
          f"{len(seen)} of {len(replies)}")
    target = replies[15]
    page = history_page(owner, room, around_id=target, limit=100)
    ids = [m.id for m in page.messages] if page else []
    start = everything.index(ids[0]) if ids else 0
    check("history: a jump keeps the target and the rows around it, both ends flagged", target in ids
          and ids == everything[start:start + len(ids)] and len(ids) < len(everything)
          and page.has_more and page.has_more_after and owner.max_frame <= FRAME_CAP, f"{len(ids)} rows")
    owner.send(pb.Packet.CHANNEL_MSG, channel_msg=pb.ChannelMessage(channel_id=room, content="a1-live", reply_to=parent, nonce="a1-live"))
    bc = owner.wait(pb.Packet.CHANNEL_MSG, timeout=2, pred=lambda p: p.channel_msg.nonce == "a1-live")
    check("history: a live reply's quote is cut the same way", bc is not None and bc.channel_msg.reply_to_content == "€" * 170)
    # A sealed parent's quote is its whole envelope (a prefix wouldn't
    # open): kept, and counted by the budget.
    owner.send(pb.Packet.CHANNEL_CREATE_REQ, channel_create_req=pb.ChannelCreateRequest(
        name="a1-sealed", type=pb.ChannelInfo.TEXT, encrypted=True))
    cr = owner.wait(pb.Packet.CHANNEL_ACTION_RES, timeout=2, pred=lambda p: p.channel_action_res.action == "create")
    sealed = cr.channel_action_res.channel.id
    env = b"\x04" + b"E" * 65000
    sp = seed_rows(sealed, [("", 0, env)])[0]
    sr = seed_rows(sealed, [("", sp, b"\x04" + bytes([65 + i]) * 65000) for i in range(20)])
    owner.max_frame = 0
    page = history_page(owner, sealed, limit=50)
    ids = [m.id for m in page.messages] if page else []
    check("history: sealed replies fit the cap, newest kept, older flagged", page is not None and owner.max_frame <= FRAME_CAP
          and bool(ids) and ids == sr[-len(ids):] and len(ids) < len(sr) and page.has_more, f"{len(ids)} rows, {owner.max_frame} B")
    check("history: sealed replies carry the parent's whole envelope", bool(ids)
          and all(m.reply_to_envelope == env for m in page.messages))
    delete_channel(owner, room); delete_channel(owner, sealed)
    owner.close()


def test_a1_ban_and_invite_lists():
    print("[A1] reasons and labels clamped; ban / invite lists fit the frame cap and flag truncation; ban pushes coalesce")
    owner = Client("alice"); assert auth_ok(owner)[0]
    viewer = Client("alice"); assert auth_ok(viewer)[0]   # another ban viewer
    kicked = join("a1kick", owner)
    owner.flush(0.5); viewer.flush(1.2)
    now = int(time.time())
    owner.send(pb.Packet.KICK_MEMBER_REQ, kick_member_req=pb.KickMemberRequest(
        username="a1kick", uid=uid_of("a1kick"), reason="k" * 1100000))
    rev = kicked.wait(pb.Packet.MEMBERSHIP_REVOKED, timeout=3)
    check("kick: the target sees the reason cut to 512 bytes", rev is not None and rev.membership_revoked.reason == "k" * 512)
    mod_res(owner, "kick")
    # A 1.1 MB reason and a 1,000-byte label for someone who isn't a member.
    owner.send(pb.Packet.BAN_MEMBER_REQ, ban_member_req=pb.BanMemberRequest(username="L" * 1000, uid=990001, reason="é" * 550000))
    r = mod_res(owner, "ban")
    row = sql("select length(username), length(cast(reason as blob)) from bans where uid=990001")
    check("ban: a 1.1 MB reason is stored as 512 bytes, the label as 64", mod_ok(r) and row == [(64, 512)], str(row))
    bl = viewer.wait(pb.Packet.BAN_LIST_RES, timeout=3, pred=lambda p: any(e.uid == 990001 for e in p.ban_list_res.entries))
    e = next((e for e in bl.ban_list_res.entries if e.uid == 990001), None) if bl else None
    check("ban: the pushed list carries them clamped", e is not None and e.reason == "é" * 256 and e.username == "L" * 64)
    # Rows stored before the caps: two 1.1 MB reasons cut every ban viewer off.
    for uid in (990002, 990003):
        sql("insert into bans(uid, username, banned_at, banned_by_uid, banned_by, reason) values(?, 'old', ?, 0, 'alice', ?)",
            uid, now, "x" * 1100000)
    owner.flush(1.2)   # pushed lists (and the coalescing timer's)
    owner.max_frame = 0
    owner.send(pb.Packet.BAN_LIST_REQ, ban_list_req=pb.BanListRequest())
    bl = owner.wait(pb.Packet.BAN_LIST_RES, timeout=3, pred=lambda p: p.ban_list_res.success)
    olds = [e for e in bl.ban_list_res.entries if e.uid in (990002, 990003)] if bl else []
    check("ban list: stored 1.1 MB reasons go out as 512 bytes", len(olds) == 2 and owner.max_frame <= FRAME_CAP
          and all(e.reason == "x" * 512 for e in olds), owner.max_frame)
    c = sqlite3.connect(DB)
    c.executemany("insert into bans(uid, username, banned_at, banned_by_uid, banned_by, reason) values(?,?,?,0,'alice',?)",
                  ((991000 + i, f"raider{i}", now - 100 - i, "r" * 300) for i in range(12000)))
    c.commit(); c.close()
    owner.flush(0.3)
    owner.max_frame = 0
    owner.send(pb.Packet.BAN_LIST_REQ, ban_list_req=pb.BanListRequest())
    bl = owner.wait(pb.Packet.BAN_LIST_RES, timeout=5, pred=lambda p: p.ban_list_res.success)
    ents = list(bl.ban_list_res.entries) if bl else []
    check("ban list: 12,000 bans stop at the byte budget, newest first, flagged truncated", bl is not None
          and bl.ban_list_res.truncated and 1000 < len(ents) < 12000 and owner.max_frame <= FRAME_CAP
          and all(a.banned_at >= b.banned_at for a, b in zip(ents, ents[1:])), f"{len(ents)} entries, {owner.max_frame} B")
    sql("delete from bans where uid >= 990000")
    # A burst of bans: one list at once, the rest together a second later.
    viewer.flush(1.5)
    for i in range(15):
        owner.send(pb.Packet.BAN_MEMBER_REQ, ban_member_req=pb.BanMemberRequest(username=f"raid{i}", uid=996000 + i, reason="raid"))
    for i in range(15):
        mod_res(owner, "ban")
    viewer.drain(2.5)
    lists = [p.ban_list_res for p in viewer.inbox if p.type == pb.Packet.BAN_LIST_RES]
    last = {e.uid for e in lists[-1].entries} if lists else set()
    check("ban list: 15 bans in a burst push at most 3 lists to a viewer, the last one complete",
          1 <= len(lists) <= 3 and {996000 + i for i in range(15)} <= last, f"{len(lists)} lists")
    sql("delete from bans where uid >= 990000")
    c = sqlite3.connect(DB)
    c.executemany("insert into invites(code, created_by, created_at, created_by_uid) values(?, 'alice', ?, ?)",
                  ((f"A1INV{i:05d}", now - 1000 - i, uid_of("alice")) for i in range(10050)))
    c.commit(); c.close()
    owner.flush(0.3)
    owner.max_frame = 0
    owner.send(pb.Packet.INVITE_LIST_REQ, invite_list_req=pb.InviteListRequest())
    r = owner.wait(pb.Packet.INVITE_LIST_RES, timeout=5)
    inv = r.invite_list_res if r else None
    check("invite list: at most 10,000 rows, newest first, flagged truncated", inv is not None and inv.success
          and inv.truncated and len(inv.invites) == 10000 and owner.max_frame <= FRAME_CAP
          and all(a.created_at >= b.created_at for a, b in zip(inv.invites, inv.invites[1:])),
          f"{len(inv.invites) if inv else None} invites")
    sql("delete from invites where code like 'A1INV%'")
    owner.send(pb.Packet.INVITE_LIST_REQ, invite_list_req=pb.InviteListRequest())
    r = owner.wait(pb.Packet.INVITE_LIST_RES, timeout=3)
    check("invite list: a short list isn't flagged", r is not None and r.invite_list_res.success and not r.invite_list_res.truncated)
    kicked.close(); viewer.close(); owner.close()


def test_a1_attachment_metadata():
    print("[A1] upload names capped at 255 bytes and types normalised; stored oversized ones are cut on the way out")
    owner = Client("alice"); assert auth_ok(owner)[0]
    room = create_channel(owner, "a1-files", pb.ChannelInfo.TEXT).id
    init = {"channelId": room, "filename": "ü" * 400 + ".png", "mime": "image/png\r\nX-Evil: 1", "size": 2}
    head, body = attachment_http("alice", "POST", "/attachments/init", "Content-Type: application/json\r\n",
                                 json.dumps(init).encode())
    aid = json.loads(body.decode() or "{}").get("id") if head.startswith(b"HTTP/1.1 201") else None
    row = sql("select filename, mime from attachments where id=?", aid) if aid else []
    check("upload: an 804-byte name is stored as 254 bytes, cut on a character", row != [] and row[0][0] == "ü" * 127,
          len(row[0][0].encode()) if row else head[:40])
    check("upload: a malformed type is stored as application/octet-stream", row != [] and row[0][1] == "application/octet-stream")
    mid = seed_rows(room, [("with a file", 0, None)])[0]
    sql("update attachments set message_id=?, upload_status='ready', filename=?, mime=? where id=?",
        mid, "f" * 100000, "text/html\r\nSet-Cookie: x", aid)
    owner.flush(0.3)
    page = history_page(owner, room, limit=10)
    m = next((m for m in page.messages if m.id == mid), None) if page else None
    a = m.attachments[0] if m and m.attachments else None
    check("history: a stored 100 KB name goes out as 255 bytes, the bad type as octet-stream",
          a is not None and a.filename == "f" * 255 and a.mime == "application/octet-stream")
    delete_channel(owner, room)
    owner.close()


def test_a1_strings_and_forwarded_packets():
    print("[A1] role overwrite ids parsed whole; caps / thumbnails / codec notices carry no unknown fields; key blob names bounded")
    owner = Client("alice"); assert auth_ok(owner)[0]
    role = create_role(owner, "a1-target", 0)
    room = create_channel(owner, "a1-ow", pb.ChannelInfo.TEXT).id
    owner.flush(0.3)
    r = set_overwrite(owner, room, pb.ChannelOverwrite.ROLE, str(role.id) + "j" * 100000, deny=pb.PERM_ATTACH_FILES)
    check("overwrite: a role id with trailing junk is refused, nothing stored", r is not None and not r.channel_action_res.success
          and sql("select count(*) from channel_overwrites where channel_id=?", room) == [(0,)])
    r = set_overwrite(owner, room, pb.ChannelOverwrite.ROLE, "00" + str(role.id), deny=pb.PERM_ATTACH_FILES)
    check("overwrite: a padded role id is stored as the id", r is not None and r.channel_action_res.success
          and sql("select target_id from channel_overwrites where channel_id=?", room) == [(str(role.id),)])
    delete_channel(owner, room)
    delete_role(owner, role.id)
    # Voice: capabilities, thumbnails and codec notices used to keep unknown
    # fields (up to a whole frame) and pass them to everyone in the channel.
    st = join("a1strm", owner); vw = join("a1view", owner)
    junk = unknown_field(b"JUNK" * 25000)
    cap = pb.CodecCapability(codec=1, max_width=1920, max_height=1080, max_fps=60)
    cap.MergeFromString(junk)
    vw.send(pb.Packet.JOIN_VOICE_REQ, join_voice_req=pb.JoinVoiceRequest(channel_id="voice-lounge"))
    st.send(pb.Packet.JOIN_VOICE_REQ, join_voice_req=pb.JoinVoiceRequest(
        channel_id="voice-lounge", capabilities=pb.ClientCapabilities(encode=[cap], decode=[cap])))
    vp = owner.wait(pb.Packet.VOICE_PRESENCE_UPDATE, timeout=3, pred=lambda p: "a1strm" in p.voice_presence_update.active_users)
    check("voice presence: capabilities without unknown fields", vp is not None and b"JUNK" not in vp.SerializeToString()
          and any(c.codec == 1 for caps in vp.voice_presence_update.user_capabilities for c in caps.encode))
    st.send(pb.Packet.START_STREAM_REQ, start_stream_req=pb.StartStreamRequest(channel_id="voice-lounge", target_fps=30))
    time.sleep(0.5); vw.flush(0.5)
    upd = pb.StreamThumbnailUpdate(channel_id="voice-lounge", thumbnail_data=b"T" * 1000)
    upd.MergeFromString(junk)
    p = pb.Packet(type=pb.Packet.STREAM_THUMBNAIL_UPDATE, stream_thumbnail_update=upd)
    p.MergeFromString(unknown_field(b"PKTJUNK" * 1000))
    send_raw(st, p)
    t = vw.wait(pb.Packet.STREAM_THUMBNAIL_UPDATE, timeout=3)
    check("thumbnail: relayed rebuilt (image and owner only, no token)", t is not None
          and t.stream_thumbnail_update.thumbnail_data == b"T" * 1000 and t.stream_thumbnail_update.owner_uid == uid_of("a1strm")
          and t.auth_token == "" and b"JUNK" not in t.SerializeToString())
    note = pb.StreamCodecChangedNotify(channel_id="voice-lounge", new_codec=1, new_width=1280, new_height=720,
                                       new_fps=30, reason=pb.StreamCodecChangedNotify.STREAMER_INITIATED,
                                       streamer_uid=uid_of("a1strm"))
    note.MergeFromString(junk)
    send_raw(st, pb.Packet(type=pb.Packet.STREAM_CODEC_CHANGED_NOTIFY, stream_codec_changed_notify=note))
    n = vw.wait(pb.Packet.STREAM_CODEC_CHANGED_NOTIFY, timeout=3)
    check("codec notice: passed on rebuilt", n is not None and n.stream_codec_changed_notify.new_width == 1280
          and n.stream_codec_changed_notify.reason == pb.StreamCodecChangedNotify.STREAMER_INITIATED
          and n.stream_codec_changed_notify.streamer_username == "a1strm" and n.auth_token == ""
          and b"JUNK" not in n.SerializeToString())
    st.send(pb.Packet.LEAVE_VOICE_REQ, leave_voice_req=pb.LeaveVoiceRequest())
    vw.send(pb.Packet.LEAVE_VOICE_REQ, leave_voice_req=pb.LeaveVoiceRequest())
    # Key escrow: a recipient name longer than any name is refused with its blob.
    owner.send(pb.Packet.CHANNEL_CREATE_REQ, channel_create_req=pb.ChannelCreateRequest(
        name="a1-keys", type=pb.ChannelInfo.TEXT, encrypted=True))
    cr = owner.wait(pb.Packet.CHANNEL_ACTION_RES, timeout=2, pred=lambda p: p.channel_action_res.action == "create")
    enc = cr.channel_action_res.channel.id
    owner.send(pb.Packet.CHANNEL_KEYS_PUBLISH_REQ, channel_keys_publish_req=pb.ChannelKeysPublishReq(
        channel_id=enc, epoch=1, key_check=b"K" * 32, blobs=[
            pb.ChannelKeyBlob(recipient="alice", recipient_uid=uid_of("alice"), blob=b"E1-alice"),
            pb.ChannelKeyBlob(recipient="N" * 1000, recipient_uid=uid_of("a1view"), blob=b"E1-view")]))
    r = owner.wait(pb.Packet.CHANNEL_KEYS_PUBLISH_RES, timeout=2)
    check("keys: a blob with a 1,000-byte recipient name is dropped, the rest stored", r is not None
          and r.channel_keys_publish_res.success
          and sql("select count(*) from channel_key_blobs where channel_id=? and recipient_uid=?", enc, uid_of("a1view")) == [(0,)]
          and sql("select count(*) from channel_key_blobs where channel_id=? and recipient_uid=?", enc, uid_of("alice")) == [(1,)])
    delete_channel(owner, enc)
    st.close(); vw.close(); owner.close()


def test_a1_role_and_channel_caps():
    print("[A1] at most 250 roles and 500 channels (both lists go out whole)")
    owner = Client("alice"); assert auth_ok(owner)[0]
    have = sql("select count(*), coalesce(max(position), 0) from roles")[0]
    c = sqlite3.connect(DB)
    c.executemany("insert into roles(name, position) values(?, ?)",
                  ((f"a1r{i}", have[1] + 1 + i) for i in range(249 - have[0])))
    c.commit(); c.close()
    owner.flush(0.3)
    last = create_role(owner, "a1-250th", 0)
    owner.send(pb.Packet.ROLE_CREATE_REQ, role_create_req=pb.RoleCreateRequest(name="a1-251st", color=0, permissions=0))
    r = owner.wait(pb.Packet.ROLE_ACTION_RES, pred=lambda p: p.role_action_res.action == "create")
    check("roles: the 251st is refused", r is not None and not r.role_action_res.success
          and sql("select count(*) from roles") == [(250,)], r.role_action_res.message if r else None)
    delete_role(owner, last.id)
    sql("delete from roles where name like 'a1r%'")
    have = sql("select count(*), coalesce(max(position), 0) from channels")[0]
    c = sqlite3.connect(DB)
    c.executemany("insert into channels(id, name, type, position) values(?, ?, 0, ?)",
                  ((f"a1c{i}", f"a1c{i}", have[1] + 1 + i) for i in range(499 - have[0])))
    c.commit(); c.close()
    owner.flush(0.3)
    lastc = create_channel(owner, "a1-500th", pb.ChannelInfo.TEXT).id
    owner.send(pb.Packet.CHANNEL_CREATE_REQ, channel_create_req=pb.ChannelCreateRequest(name="a1-501st", type=pb.ChannelInfo.TEXT))
    r = owner.wait(pb.Packet.CHANNEL_ACTION_RES, pred=lambda p: p.channel_action_res.action == "create")
    check("channels: the 501st is refused", r is not None and not r.channel_action_res.success
          and sql("select count(*) from channels") == [(500,)], r.channel_action_res.message if r else None)
    sql("delete from channels where id like 'a1c%'")
    delete_channel(owner, lastc)
    owner.close()


def test_a2_relay_drops_oversized_datagrams():
    print("[A2] the relay drops audio bigger than the audio struct (FEC / NACK likewise) instead of passing it on")
    owner = Client("alice"); assert auth_ok(owner)[0]
    sa = join("a2send", owner); sb = join("a2recv", owner)
    for c in (sa, sb):
        c.send(pb.Packet.JOIN_VOICE_REQ, join_voice_req=pb.JoinVoiceRequest(channel_id="voice-lounge"))
    owner.wait(pb.Packet.VOICE_PRESENCE_UPDATE, pred=lambda p: {"a2send", "a2recv"} <= set(p.voice_presence_update.active_users))

    def sock():
        s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM); s.bind((HOST, 0)); s.settimeout(2); return s

    def recv(s):
        try:
            return s.recv(4096)
        except socket.timeout:
            return b""

    ua, ub = sock(), sock()
    check("relay: both voice endpoints bind", udp_bind(ua, sa.jwt, 8083) and udp_bind(ub, sb.jwt, 8083))
    ua.sendto(udp_audio_packet(sa.jwt, 1, b"q" * 1200), (HOST, 8083))   # 1,237 bytes: the struct exactly
    check("relay: audio of exactly the struct's size is relayed", len(recv(ub)) == 1237)
    ua.sendto(udp_audio_packet(sa.jwt, 2, b"q" * 1201), (HOST, 8083))   # 1,238 bytes
    check("relay: audio one byte bigger is dropped", udp_quiet(ub))
    ua.sendto(bytes([7]) + udp_audio_packet(sa.jwt, 3, b"s" * 1201)[1:], (HOST, 8083))
    check("relay: sealed audio one byte bigger is dropped too", udp_quiet(ub))
    # Media: a2recv watches a2send's stream.
    sa.send(pb.Packet.START_STREAM_REQ, start_stream_req=pb.StartStreamRequest(channel_id="voice-lounge", target_fps=30))
    time.sleep(0.3)
    sb.send(pb.Packet.WATCH_STREAM_REQ, watch_stream_req=pb.WatchStreamRequest(channel_id="voice-lounge", target_uid=uid_of("a2send")))
    ma, mb = sock(), sock()
    check("relay: both media endpoints bind", udp_bind(ma, sa.jwt, 8084) and udp_bind(mb, sb.jwt, 8084))
    udp_quiet(ma, 0.3)   # swallow the watch's keyframe request, if it came
    fec = lambda n: bytes([4]) + udp_key(sa.jwt) + struct.pack("<IHHH", 1, 0, 5, n) + b"f" * n
    ma.sendto(fec(1200), (HOST, 8084))   # 1,243 bytes: the FEC struct exactly
    check("relay: FEC of exactly the struct's size is relayed", len(recv(mb)) == 1243)
    ma.sendto(fec(1201), (HOST, 8084))
    check("relay: FEC one byte bigger is dropped", udp_quiet(mb))
    nack = (bytes([3]) + udp_key(sb.jwt) + str(uid_of("a2send")).encode().ljust(32, b"\0")
            + struct.pack("<IH", 9, 1) + b"\0" * 128)
    mb.sendto(nack, (HOST, 8084))        # 199 bytes: the NACK struct exactly
    check("relay: a NACK of the struct's size reaches the streamer", recv(ma)[:1] == b"\x03")
    mb.sendto(nack + b"\0", (HOST, 8084))
    check("relay: a bigger NACK is dropped", udp_quiet(ma))
    for c in (sa, sb):
        c.send(pb.Packet.LEAVE_VOICE_REQ, leave_voice_req=pb.LeaveVoiceRequest())
    for s in (ua, ub, ma, mb): s.close()
    sa.close(); sb.close(); owner.close()


def test_a3_bulk_fanout():
    print("[A3] deleting a role held by 1,100 members, or purging 1,100 messages, is O(1) frames per viewer")
    owner = Client("alice"); assert auth_ok(owner)[0]
    viewer = join("a3view", owner)
    role = create_role(owner, "a3-crowd", 0)
    now = int(time.time())
    c = sqlite3.connect(DB)
    c.executemany("insert into members(uid, username, joined_at) values(?,?,?)",
                  ((800000 + i, f"a3m{i:04d}", now) for i in range(1100)))
    c.executemany("insert into member_roles(uid, role_id) values(?,?)", ((800000 + i, role.id) for i in range(1100)))
    c.commit(); c.close()
    owner.flush(0.5); viewer.flush(0.5)
    delete_role(owner, role.id)
    rl = viewer.wait(pb.Packet.ROLE_LIST_RES, timeout=3, pred=lambda p: all(r.id != role.id for r in p.role_list_res.roles))
    viewer.drain(1.0)
    upserts = sum(1 for p in viewer.inbox if p.type == pb.Packet.MEMBER_UPSERT)
    check("role delete: the role list is the whole delta, no upsert per holder", rl is not None and upserts == 0, f"{upserts} upserts")
    check("role delete: the viewer stays connected", not viewer.is_closed(0.3))
    check("role delete: the holders' rows are gone", sql("select count(*) from member_roles where role_id=?", role.id) == [(0,)])
    sql("delete from members where uid between 800000 and 801099")
    # Ban with purge: one CHANNEL_PRUNED per channel, not a delete per message.
    # (Fresh sessions: before the fix the role delete cut the owner's off too.)
    owner.close()
    owner = Client("alice"); assert auth_ok(owner)[0]
    other = Client("alice"); assert auth_ok(other)[0]
    spam = join("a3spam", owner)
    room = create_channel(owner, "a3-purge", pb.ChannelInfo.TEXT).id
    c = sqlite3.connect(DB)
    c.executemany("insert into messages(channel_id, sender, sender_uid, content, timestamp) values(?,?,?,?,?)",
                  ((room, "a3spam", uid_of("a3spam"), f"spam {i}", now) for i in range(1100)))
    c.commit(); c.close()
    owner.flush(0.5); other.flush(0.5)
    owner.send(pb.Packet.BAN_MEMBER_REQ, ban_member_req=pb.BanMemberRequest(
        username="a3spam", uid=uid_of("a3spam"), reason="spam", delete_message_seconds=3600))
    r = mod_res(owner, "ban")
    other.drain(1.5)
    pruned = [p.channel_pruned for p in other.inbox if p.type == pb.Packet.CHANNEL_PRUNED and p.channel_pruned.channel_id == room]
    deletes = sum(1 for p in other.inbox if p.type == pb.Packet.CHANNEL_MESSAGE_DELETED)
    check("ban purge: one CHANNEL_PRUNED names all 1,100 messages", mod_ok(r) and len(pruned) == 1
          and len(pruned[0].deleted_message_ids) == 1100 and deletes == 0, f"{len(pruned)} pruned, {deletes} deletes")
    check("ban purge: the viewer stays connected", not other.is_closed(0.3))
    sql("delete from bans where uid=?", uid_of("a3spam"))
    spam.close(); other.close(); viewer.close(); owner.close()
    owner = Client("alice"); assert auth_ok(owner)[0]
    delete_channel(owner, room)
    owner.close()


if __name__ == "__main__":
    if OLD_SERVER_BIN:
        test_v15_uid_migration()
    else:
        print("(DECIBELL_E2E_SERVER_OLD unset: v15 migration check skipped)")
    test_b1_seed_resurrection()
    test_v11_migration()
    test_v14_search_migration()
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
        test_stream_thumbnails()
        test_p2_perm_cache_invalidation()
        test_b20_attachment_url()
        test_auth_server_id_field()
        test_v2_enforced_bits()
        test_v2_private_channel()
        test_v2_overwrite_guards()
        test_roster_deltas()
        test_roster_online_paging()
        test_roster_page_budget()
        test_member_search_and_role_counts()
        test_audit_search()
        test_message_search()
        test_message_search_turkish_and_within()
        test_presence_hidden()
        test_public_join()
        test_server_update_and_transfer()
        test_audit_log()
        test_timeouts()
        test_m1_m4_timeout()
        test_ban_expiry_and_purge()
        test_slowmode()
        test_channel_topic()
        test_personal_invites()
        test_m3_slowmode_per_user()
        test_m3_session_cap()
        test_voice_moderation()
        test_presence_follows_visibility()
        test_udp_relay()
        test_udp_return_routability()
        test_mls_delivery_service()
        test_v3_invites_and_nicknames()
        test_v3_peer_management()
        test_v3_voice_split_and_passes()
        test_v3_speak()
        test_v3_embed_links()
        test_v3_thumbnail_scope()
        test_v3_category_sync()
        test_encrypted_channels()
        test_http_keepalive_and_fts()
        test_message_edit()
        test_message_reply()
        test_history_around_and_after()
        test_b4_history_nonce()
        test_storage()
        test_x3_upload_membership_recheck()
        test_theme_a_tokens_and_uid()
        test_uid_wire()
        test_live_rename()
        test_s2_channel_msg_whitelist()
        test_s15_channel_id_cap()
        test_s17_write_backlog_bytes()
        test_a1_history_budget()
        test_a1_ban_and_invite_lists()
        test_a1_attachment_metadata()
        test_a1_strings_and_forwarded_packets()
        test_a1_role_and_channel_caps()
        test_a2_relay_drops_oversized_datagrams()
        test_a3_bulk_fanout()
    finally:
        stop_server(proc)
    test_b9_timeouts()
    test_b12_b27_retention_sweep()
    test_upload_sweep_goes_by_idle_time()
    test_s16_connection_caps()
    print(f"\n{len(PASS)} passed, {len(FAIL)} failed")
    if FAIL:
        print("FAILED:", FAIL)
        sys.exit(1)
