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


def jwt(username, nonce=""):
    h = b64(json.dumps({"alg": "HS256", "typ": "JWT"}).encode())
    now = int(time.time())
    p = b64(json.dumps({"iss": "decibell_central_auth", "sub": username,
                        "iat": now, "exp": now + 3600, "n": nonce}).encode())
    sig = hmac.new(SECRET.encode(), f"{h}.{p}".encode(), hashlib.sha256).digest()
    return f"{h}.{p}.{b64(sig)}"


class Client:
    def __init__(self, username, invite="", nonce="", timeout=3.0):
        ctx = ssl.create_default_context()
        ctx.check_hostname = False
        ctx.verify_mode = ssl.CERT_NONE
        raw = socket.create_connection((HOST, 8082), timeout=timeout)
        self.s = ctx.wrap_socket(raw)
        self.username = username
        self.jwt = jwt(username, nonce)
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


def start_server(fresh=False):
    os.makedirs(RUN, exist_ok=True)
    if fresh:
        for f in ("c.db", "c.db-wal", "c.db-shm"):
            try: os.remove(os.path.join(RUN, f))
            except FileNotFoundError: pass
    if not os.path.exists(os.path.join(RUN, "server.crt")):
        subprocess.run(["openssl", "req", "-x509", "-newkey", "rsa:2048", "-nodes", "-keyout", "server.key",
                        "-out", "server.crt", "-subj", "/CN=localhost", "-days", "2"], cwd=RUN, check=True,
                       capture_output=True)
    env = dict(os.environ, DECIBELL_JWT_SECRET=SECRET, DECIBELL_OWNER_USERNAME="alice", DECIBELL_DB_PATH="./c.db",
               DECIBELL_ATTACHMENTS_ROOT="./att", DECIBELL_CENTRAL_HOST="127.0.0.1")
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
        ml = owner.wait(pb.Packet.MEMBER_LIST_RES, timeout=3)
        if ml:
            got = next((m.nickname for m in ml.member_list_res.members if m.username == "bob"), None)
    except Exception as e:  # UnicodeDecodeError / DecodeError == what prost would do
        check("MEMBER_LIST_RES decodable", False, repr(e))
        got = None
    check("MEMBER_LIST_RES decodable with clamped nickname", got is not None and got == "a" * 30, repr(got))
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
    ml = owner.wait(pb.Packet.MEMBER_LIST_RES, timeout=2)
    names = [m.username for m in ml.member_list_res.members] if ml else None
    check("roster broadcast after offline kick, heidi gone", ml is not None and "heidi" not in names, str(names))
    ivan = join("ivan", owner); ivan.close(); time.sleep(0.5); owner.flush(0.8)
    owner.send(pb.Packet.BAN_MEMBER_REQ, ban_member_req=pb.BanMemberRequest(username="ivan", reason=""))
    r = owner.wait(pb.Packet.MOD_ACTION_RES, pred=lambda p: p.mod_action_res.action == "ban")
    check("offline ban succeeded", r is not None and r.mod_action_res.success)
    ml = owner.wait(pb.Packet.MEMBER_LIST_RES, timeout=2)
    check("roster broadcast after offline ban, ivan in ban list", ml is not None and "ivan" in list(ml.member_list_res.bans)
          and "ivan" not in [m.username for m in ml.member_list_res.members])
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
    ml = owner.wait(pb.Packet.MEMBER_LIST_RES, timeout=2)
    check("roster without judy after leave", ml is not None and "judy" not in [m.username for m in ml.member_list_res.members])
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


def test_auth_failure_closes():
    print("[regression] failed auth still delivers response then closes")
    c = Client("nobody")
    ok, r = auth_ok(c)
    check("non-member rejected with not_member", r is not None and not ok and r.community_auth_res.error_code == "not_member")
    check("socket closed after rejection", c.is_closed())
    c.close()


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
        test_ghost_stream()
        test_offline_kick_roster()
        test_no_ghost_after_leave_or_ban()
    finally:
        stop_server(proc)
    print(f"\n{len(PASS)} passed, {len(FAIL)} failed")
    if FAIL:
        print("FAILED:", FAIL)
        sys.exit(1)
