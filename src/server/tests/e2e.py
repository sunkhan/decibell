#!/usr/bin/env python3
"""e2e checks for the central server.

Starts a bundled PostgreSQL (pgserver) and a live central server (8080),
then talks protobuf-over-TLS to it as clients and as a community server
(shared secret). Run with the deps venv's python — see README.md.
"""
import base64, json, os, socket, ssl, struct, subprocess, sys, time

sys.path.insert(0, os.environ.get("DECIBELL_E2E_PB", os.path.join(os.path.dirname(__file__), "deps", "pb")))
import messages_pb2 as pb  # noqa: E402
import pgserver  # noqa: E402
import psycopg  # noqa: E402

SECRET = "testsecret"
HOST, PORT = "127.0.0.1", 8080
RUN = os.environ.get("DECIBELL_E2E_RUN", os.path.join(os.path.dirname(__file__), "run"))
SERVER_BIN = os.environ.get("DECIBELL_E2E_SERVER", os.path.join(os.path.dirname(__file__), "central_server"))
# Optional: the central binary from before the uid migration (main at the
# branch point). When set, the migration check seeds a database with it.
OLD_SERVER_BIN = os.environ.get("DECIBELL_E2E_SERVER_OLD", "")
DB_NAME = "central_e2e"
PASS, FAIL = [], []
PG = None


def check(name, cond, detail=""):
    (PASS if cond else FAIL).append(name)
    print(("  ok   " if cond else "  FAIL ") + name + (f"  ({detail})" if detail and not cond else ""))


# --- PostgreSQL --------------------------------------------------------------

def pg_uri(db="postgres"):
    return PG.get_uri(db)


def fresh_db():
    with psycopg.connect(pg_uri(), autocommit=True) as c:
        c.execute(f"DROP DATABASE IF EXISTS {DB_NAME} WITH (FORCE)")
        c.execute(f"CREATE DATABASE {DB_NAME}")


def sql(q, *args):
    with psycopg.connect(pg_uri(DB_NAME), autocommit=True) as c:
        cur = c.execute(q, args)
        return cur.fetchall() if cur.description else []


def libpq_conninfo():
    # pgserver hands out a socket-dir URI; libpq takes the same form.
    return pg_uri(DB_NAME)


# --- Central process ---------------------------------------------------------

def ensure_tls():
    os.makedirs(RUN, exist_ok=True)
    if not os.path.exists(os.path.join(RUN, "server.crt")):
        subprocess.run(["openssl", "req", "-x509", "-newkey", "rsa:2048", "-nodes", "-days", "30",
                        "-subj", "/CN=localhost", "-keyout", os.path.join(RUN, "server.key"),
                        "-out", os.path.join(RUN, "server.crt")], check=True, capture_output=True)


def start_server(binary=None, log_name="central.log", extra_env=None, nofile=None, cpus=None):
    ensure_tls()
    env = dict(os.environ, DECIBELL_COMMUNITY_SECRET=SECRET, DECIBELL_DB_CONN=libpq_conninfo(),
               DECIBELL_JWT_KEY_FILE=os.path.join(RUN, "jwt_ed25519.pem"), **(extra_env or {}))
    log = open(os.path.join(RUN, log_name), "ab")

    def limit_files():
        if nofile:
            import resource
            resource.setrlimit(resource.RLIMIT_NOFILE, (nofile, nofile))
        if cpus:
            os.sched_setaffinity(0, cpus)  # every thread central starts inherits it

    proc = subprocess.Popen([binary or SERVER_BIN], cwd=RUN, env=env, stdout=log, stderr=subprocess.STDOUT,
                            preexec_fn=limit_files)
    end = time.time() + 15
    while time.time() < end:
        try:
            socket.create_connection((HOST, PORT), timeout=0.5).close()
            return proc
        except OSError:
            if proc.poll() is not None:
                raise RuntimeError(f"central exited ({proc.returncode}); see {log_name}")
            time.sleep(0.1)
    raise RuntimeError("central did not start listening")


def stop_server(proc):
    proc.terminate()
    try:
        proc.wait(5)
    except subprocess.TimeoutExpired:
        proc.kill()
        proc.wait()


# --- Wire client ---------------------------------------------------------------

_next_src = [0]


def next_source():
    """A fresh loopback source address per connection: central limits
    connections, sign-ins and sign-ups per address, and the suite makes far
    more of them than one person would."""
    _next_src[0] += 1
    n = _next_src[0]
    return f"127.0.{1 + (n // 250) % 190}.{1 + n % 250}"


class Conn:
    """One TLS connection to central. `token` goes into every packet."""

    def __init__(self, timeout=3.0, src=None):
        ctx = ssl.create_default_context()
        ctx.check_hostname = False
        ctx.verify_mode = ssl.CERT_NONE
        raw = socket.create_connection((HOST, PORT), timeout=timeout, source_address=(src or next_source(), 0))
        self.s = ctx.wrap_socket(raw)
        self.buf = b""
        self.inbox = []
        self.closed = False
        self.token = ""
        self.sizes = {}  # packet type -> body size of the last such frame

    def send(self, ptype, token=None, **payload):
        p = pb.Packet(type=ptype, auth_token=self.token if token is None else token, **payload)
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
        p.ParseFromString(body)
        self.sizes[p.type] = n
        return p

    def wait(self, ptype, timeout=3.0, pred=lambda p: True):
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

    def drain(self, timeout=0.3):
        end = time.time() + timeout
        while time.time() < end:
            p = self._recv_one(max(0.05, end - time.time()))
            if p is None and self.closed:
                return
            if p is not None:
                self.inbox.append(p)

    def clear(self):
        self.drain()
        self.inbox.clear()

    def close(self):
        try:
            self.s.close()
        except OSError:
            pass


def register(username, password="password123", email=None, src=None):
    c = Conn(src=src)
    c.send(pb.Packet.REGISTER_REQ, register_req=pb.RegisterRequest(
        username=username, password=password, email=email or f"{username}@example.com"))
    r = c.wait(pb.Packet.REGISTER_RES)
    c.close()
    return r.register_res if r else None


class User(Conn):
    """A logged-in client session."""

    def __init__(self, username, password="password123", resume="", src=None):
        super().__init__(src=src)
        self.username = username
        req = pb.LoginRequest(resume_token=resume) if resume else pb.LoginRequest(username=username, password=password)
        self.send(pb.Packet.LOGIN_REQ, login_req=req)
        r = self.wait(pb.Packet.LOGIN_RES, timeout=15)
        self.login = r.login_res if r else None
        self.ok = bool(self.login and self.login.success)
        if self.ok:
            self.token = self.login.jwt_token

    def watch(self, *uids):
        """Replace the watch set (PRESENCE_SUBSCRIBE); returns the answer
        for the newly watched (None when nothing was new)."""
        self.send(pb.Packet.PRESENCE_SUBSCRIBE, presence_subscribe=pb.PresenceSubscribe(uids=list(uids)))
        r = self.wait(pb.Packet.PRESENCE_UPDATE, timeout=1.5)
        return r.presence_update if r else None

    def claims(self):
        part = self.token.split(".")[1]
        return json.loads(base64.urlsafe_b64decode(part + "=" * (-len(part) % 4)))

    def friend(self, action, target="", target_uid=0):
        self.send(pb.Packet.FRIEND_ACTION_REQ, friend_action_req=pb.FriendActionReq(
            action=action, target_username=target, target_uid=target_uid))
        r = self.wait(pb.Packet.FRIEND_ACTION_RES)
        return r.friend_action_res if r else None

    def friends(self):
        # A FRIEND_LIST_RES may also arrive pushed by someone else's action.
        self.clear()
        self.send(pb.Packet.FRIEND_LIST_REQ, friend_list_req=pb.FriendListReq())
        r = self.wait(pb.Packet.FRIEND_LIST_RES)
        return {f.username: f for f in r.friend_list_res.friends} if r else None

    def dm(self, recipient="", content="hi", nonce="", recipient_uid=0, reply_to=0, envelope=b""):
        self.send(pb.Packet.DIRECT_MSG, direct_msg=pb.DirectMessage(
            recipient=recipient, recipient_uid=recipient_uid, content=content, nonce=nonce,
            reply_to=reply_to, envelope=envelope))
        r = self.wait(pb.Packet.DIRECT_MSG, pred=lambda p: p.direct_msg.nonce == nonce if nonce else True)
        return r.direct_msg if r else None

    def history(self, peer="", peer_uid=0, **kw):
        self.send(pb.Packet.DM_HISTORY_REQ, dm_history_req=pb.DmHistoryReq(peer=peer, peer_uid=peer_uid, **kw))
        r = self.wait(pb.Packet.DM_HISTORY_RES)
        return r.dm_history_res if r else None

    def conversations(self):
        self.send(pb.Packet.DM_CONVERSATIONS_REQ, dm_conversations_req=pb.DmConversationsReq())
        r = self.wait(pb.Packet.DM_CONVERSATIONS_RES)
        return {c.peer: c for c in r.dm_conversations_res.conversations} if r else None


def jwt_claims(token):
    part = token.split(".")[1]
    return json.loads(base64.urlsafe_b64decode(part + "=" * (-len(part) % 4)))


def b64url(data):
    return base64.urlsafe_b64encode(data).rstrip(b"=").decode()


def mint_jwt(claims):
    """A token signed with central's own key (the harness can read it), for
    shapes central never issues: expired, another audience."""
    head = b64url(json.dumps({"alg": "EdDSA", "typ": "JWS"}).encode())
    body = b64url(json.dumps(claims).encode())
    # Ed25519 signs in one shot, which openssl only does from a file.
    path = os.path.join(RUN, "jwt_signing_input")
    with open(path, "wb") as f:
        f.write(f"{head}.{body}".encode())
    sig = subprocess.run(["openssl", "pkeyutl", "-sign", "-rawin", "-inkey", os.path.join(RUN, "jwt_ed25519.pem"),
                          "-in", path], capture_output=True, check=True).stdout
    return f"{head}.{body}.{b64url(sig)}"


def community_send(ptype, **payload):
    c = Conn()
    c.send(ptype, token=SECRET, **payload)
    return c


def heartbeat(name="Test Community", port=9000):
    c = community_send(pb.Packet.SERVER_HEARTBEAT, server_heartbeat=pb.ServerHeartbeat(
        name=name, description="d", host_ip="10.0.0.1", port=port, member_count=1))
    r = c.wait(pb.Packet.SERVER_HEARTBEAT_RES)
    c.close()
    return r.server_heartbeat_res.server_id if r else 0


# --- Tests -----------------------------------------------------------------------

def test_register_login():
    print("register / login")
    r = register("alice")
    check("register alice", r and r.success, r and r.message)
    check("register bob", register("bob").success)
    check("register carol", register("carol").success)
    check("duplicate name refused", not register("alice", email="other@example.com").success)
    check("uppercase name refused", not register("Dave").success)
    a = User("alice")
    check("login alice", a.ok)
    check("jwt sub is the name", a.claims().get("sub") == "alice")
    check("jwt uid is positive", int(a.claims().get("uid", 0)) > 0)
    bad = User("alice", password="wrongpassword")
    check("wrong password refused", not bad.ok)
    ghost = User("nobody")
    check("unknown user refused", not ghost.ok)
    # A database failure isn't a wrong password: clients forget a saved
    # password only on "Invalid username or password.", so an outage must
    # never answer with it (review S10).
    sql("ALTER TABLE users RENAME TO users_offline")
    try:
        down = User("alice")
        msg = down.login.message if down.login else ""
        check("database error isn't 'wrong password'", not down.ok and "couldn't check" in msg, msg)
        down.close()
    finally:
        sql("ALTER TABLE users_offline RENAME TO users")
    for c in (a, bad, ghost):
        c.close()


def test_friends():
    print("friends")
    a, b = User("alice"), User("bob")
    r = a.friend(pb.ADD, "bob")
    check("add bob", r and r.success, r and r.message)
    check("bob sees incoming", b.friends().get("alice") and b.friends()["alice"].status == pb.FriendInfo.PENDING_INCOMING)
    check("alice sees outgoing", a.friends()["bob"].status == pb.FriendInfo.PENDING_OUTGOING)
    r = b.friend(pb.ACCEPT, "alice", uid_of("alice"))
    check("bob accepts", r and r.success, r and r.message)
    fa = a.friends()
    check("friends, online", fa.get("bob") and fa["bob"].status == pb.FriendInfo.ONLINE)
    r = a.friend(pb.ADD, "nosuchuser")
    check("add unknown refused", r and not r.success and r.message == "User not found.", r and r.message)
    r = a.friend(pb.ADD, "alice")
    check("add self refused", r and not r.success)
    a.close()
    time.sleep(0.2)
    check("friend offline after logout", b.friends()["alice"].status == pb.FriendInfo.OFFLINE)
    b.close()


def test_dms():
    print("direct messages")
    a, b = User("alice"), User("bob")
    m = a.dm("bob", "hello bob", nonce="n1")
    check("echo carries id", m and m.id > 0 and m.sender == "alice")
    got = b.wait(pb.Packet.DIRECT_MSG, pred=lambda p: p.direct_msg.content == "hello bob")
    check("bob receives live", got is not None and got.direct_msg.id == (m.id if m else -1))
    # S1 (review 2026-10-17): a relayed DM is built fresh — never a copy of
    # the sender's packet, whose auth_token is their session token.
    check("relayed DM carries no token", got is not None and got.auth_token == "")
    m2 = b.dm("alice", "reply", nonce="n2", reply_to=m.id if m else 0)
    check("reply embeds parent", m2 and m2.reply_to == m.id and m2.reply_to_sender == "alice"
          and m2.reply_to_content == "hello bob")
    h = a.history("bob", uid_of("bob"))
    check("history newest first", h and [x.content for x in h.messages][:2] == ["reply", "hello bob"])
    older = a.history("bob", uid_of("bob"), before_id=m2.id if m2 else 0)
    check("history echoes its before_id (0 = the newest page)", h and h.before_id == 0 and older
          and older.before_id == (m2.id if m2 else -1) and [x.content for x in older.messages][:1] == ["hello bob"])
    convs = a.conversations()
    check("conversation listed", convs and "bob" in convs and convs["bob"].last_message_content == "reply")
    check("one unread for alice", convs and convs["bob"].unread_count == 1, convs and convs["bob"].unread_count)
    a.send(pb.Packet.DM_MARK_READ_REQ, dm_mark_read_req=pb.DmMarkReadReq(peer="bob", peer_uid=uid_of("bob"), up_to_id=m2.id))
    time.sleep(0.2)
    check("read clears unread", a.conversations()["bob"].unread_count == 0)
    # Edit + delete are sender-only.
    a.send(pb.Packet.DM_EDIT_REQ, dm_edit_req=pb.DmEditReq(peer="bob", peer_uid=uid_of("bob"), message_id=m.id, content="hello again"))
    r = a.wait(pb.Packet.DM_EDIT_RES)
    check("edit own", r and r.dm_edit_res.success)
    e = b.wait(pb.Packet.DM_MESSAGE_EDITED)
    check("peer sees edit", e and e.dm_message_edited.content == "hello again" and e.dm_message_edited.peer == "alice")
    b.send(pb.Packet.DM_EDIT_REQ, dm_edit_req=pb.DmEditReq(peer="alice", peer_uid=uid_of("alice"), message_id=m.id, content="forged"))
    r = b.wait(pb.Packet.DM_EDIT_RES)
    check("edit other's refused", r and not r.dm_edit_res.success)
    b.send(pb.Packet.DM_DELETE_REQ, dm_delete_req=pb.DmDeleteReq(peer="alice", peer_uid=uid_of("alice"), message_id=m.id))
    r = b.wait(pb.Packet.DM_DELETE_RES)
    check("delete other's refused", r and not r.dm_delete_res.success)
    a.send(pb.Packet.DM_DELETE_REQ, dm_delete_req=pb.DmDeleteReq(peer="bob", peer_uid=uid_of("bob"), message_id=m.id))
    r = a.wait(pb.Packet.DM_DELETE_RES)
    check("delete own", r and r.dm_delete_res.success)
    d = b.wait(pb.Packet.DM_MESSAGE_DELETED)
    check("peer sees delete", d and d.dm_message_deleted.message_id == m.id and d.dm_message_deleted.peer == "alice")
    # Unknown recipients are refused, with the nonce (2081fb0).
    r = a.dm("ghostname", "anyone?", nonce="n3")
    check("dm to unknown name refused", r and r.id == 0 and r.content == "This user doesn't exist.", r and r.content)
    check("nothing stored for unknown name", sql("SELECT count(*) FROM dm_messages WHERE recipient = 'ghostname'")[0][0] == 0)
    a.send(pb.Packet.DM_MARK_READ_REQ, dm_mark_read_req=pb.DmMarkReadReq(peer="ghostname", up_to_id=99))
    time.sleep(0.2)
    a.send(pb.Packet.DIRECT_MSG, direct_msg=pb.DirectMessage(
        recipient_uid=uid_of("bob"), content="no parent", nonce="n1b", reply_to=0,
        reply_to_sender="mallory", reply_to_content="forged quote", edited_at=12345))
    got = b.wait(pb.Packet.DIRECT_MSG, pred=lambda p: p.direct_msg.content == "no parent")
    check("client-set reply preview / edited_at dropped", got and got.direct_msg.reply_to_sender == ""
          and got.direct_msg.reply_to_content == "" and got.direct_msg.edited_at == 0)
    r = a.dm("bob", "x" * (64 * 1024 + 1), nonce="n1c")
    check("oversized DM refused with the nonce", r and r.id == 0 and r.content == "Message too long.", r and r.content)
    r = a.dm("alice", "me", nonce="n1d")
    check("self-DM refused with the nonce", r and r.id == 0 and r.nonce == "n1d")
    a.close(); b.close()


def test_dm_conversation_rows():
    """Review B11: the conversation list comes from dm_conversations (one row
    per owner and peer, kept by triggers on dm_messages) instead of grouping
    every DM the user ever sent or received. A send moves the pair up, deleting
    its newest message falls back to the one before, deleting the last ends
    the conversation, and rows written with plain SQL keep it current too."""
    print("dm conversations from their own table (review B11)")
    for n in ("dora", "ezra", "finn"):
        register(n)
    a, b, c = User("dora"), User("ezra"), User("finn")
    ua, ub, uc = uid_of("dora"), uid_of("ezra"), uid_of("finn")

    def listing(u):
        u.send(pb.Packet.DM_CONVERSATIONS_REQ, dm_conversations_req=pb.DmConversationsReq())
        r = u.wait(pb.Packet.DM_CONVERSATIONS_RES)
        return [(x.peer, x.last_message_id) for x in r.dm_conversations_res.conversations] if r else None

    def rows(x, y):
        return sql("SELECT lo_uid, hi_uid, last_id FROM dm_conversations WHERE lo_uid = %s AND hi_uid = %s",
                   min(x, y), max(x, y))

    def delete(u, peer_uid, mid):
        u.send(pb.Packet.DM_DELETE_REQ, dm_delete_req=pb.DmDeleteReq(peer_uid=peer_uid, message_id=mid))
        r = u.wait(pb.Packet.DM_DELETE_RES)
        return r is not None and r.dm_delete_res.success

    d1 = a.dm("ezra", "one", nonce="cv1")
    d2 = b.dm("dora", "two", nonce="cv2")
    d3 = a.dm("finn", "three", nonce="cv3")
    ok = all(m and m.id > 0 for m in (d1, d2, d3))
    check("conversations listed newest first", ok and listing(a) == [("finn", d3.id), ("ezra", d2.id)]
          and listing(b) == [("dora", d2.id)], listing(a))
    check("one row per conversation, at the pair's newest message", ok
          and rows(ua, ub) == [(min(ua, ub), max(ua, ub), d2.id)])
    convs = c.conversations()
    check("unread counted per conversation", convs and convs["dora"].unread_count == 1)
    check("deleting the newest falls back to the one before", ok and delete(b, ua, d2.id)
          and listing(a) == [("finn", d3.id), ("ezra", d1.id)]
          and a.conversations()["ezra"].last_message_content == "one")
    check("deleting the last ends the conversation", ok and delete(a, ub, d1.id)
          and listing(a) == [("finn", d3.id)] and listing(b) == [] and rows(ua, ub) == [])
    sql("INSERT INTO dm_messages (sender_uid, sender, recipient_uid, recipient, content, sent_at) "
        "VALUES (%s, 'ezra', %s, 'finn', 'seeded', 1)", ub, uc)
    seeded = sql("SELECT max(id) FROM dm_messages WHERE content = 'seeded'")[0][0]
    check("a row written with SQL lists too", listing(c) == [("ezra", seeded), ("dora", d3.id)] if ok else False,
          listing(c))
    check("the unread index exists", sql("SELECT count(*) FROM pg_indexes WHERE indexname = 'dm_messages_unread_idx'")[0][0] == 1)
    a.close(); b.close(); c.close()


def test_dm_gates():
    print("dm gates")
    a, c = User("alice"), User("carol")
    c.send(pb.Packet.DM_PRIVACY, dm_privacy=pb.DmPrivacySetting(friends_only=True))
    time.sleep(0.2)
    r = a.dm("carol", "hi carol", nonce="g1")
    check("friends-only refuses a stranger", r and r.id == 0 and "friends list" in r.content, r and r.content)
    c.send(pb.Packet.DM_PRIVACY, dm_privacy=pb.DmPrivacySetting(friends_only=False))
    time.sleep(0.2)
    r = a.dm("carol", "hi carol", nonce="g2")
    check("stranger allowed when open", r and r.id > 0)
    r = c.friend(pb.BLOCK, "alice", uid_of("alice"))
    check("carol blocks alice", r and r.success)
    r = a.dm("carol", "blocked?", nonce="g3")
    check("blocked sender refused", r and r.id == 0)
    r = c.dm("alice", "blocked?", nonce="g4")
    check("blocker can't dm the blocked either", r and r.id == 0)
    fc = c.friends()
    check("blocker lists the block", fc.get("alice") and fc["alice"].status == pb.FriendInfo.BLOCKED)
    check("blocked user doesn't see it", "carol" not in a.friends())
    c.friend(pb.REMOVE, "alice", uid_of("alice"))
    a.close(); c.close()


def test_presence_and_avatars():
    print("presence / avatars (by subscription)")
    ua, ub = uid_of("alice"), uid_of("bob")
    a = User("alice")
    a.clear()
    check("no snapshot at login", not any(p.type == pb.Packet.PRESENCE_UPDATE for p in a.inbox))
    ans = a.watch(ub)
    check("watching an offline account: covered, no entry",
          ans is not None and list(ans.covered_uids) == [ub] and len(ans.users) == 0, ans)
    b = User("bob")
    delta = a.wait(pb.Packet.PRESENCE_DELTA, pred=lambda p: p.presence_delta.user.uid == ub)
    check("a watcher hears the login", delta and delta.presence_delta.online and delta.presence_delta.user.username == "bob")
    b.clear()
    ans = b.watch(ua)
    check("watching an online account: its presence", ans is not None and list(ans.covered_uids) == [ua]
          and [u.uid for u in ans.users] == [ua] and ans.users[0].username == "alice")
    check("re-sending the same set answers nothing", b.watch(ua) is None)
    jpeg = b"\xff\xd8" + b"x" * 100
    b.send(pb.Packet.UPDATE_AVATAR_REQ, update_avatar_req=pb.UpdateAvatarReq(data=jpeg))
    r = b.wait(pb.Packet.UPDATE_AVATAR_RES)
    check("avatar stored", r and r.update_avatar_res.success and r.update_avatar_res.version)
    ch = a.wait(pb.Packet.AVATAR_CHANGED)
    check("avatar change reaches the watcher", ch and ch.avatar_changed.uid == ub and ch.avatar_changed.username == "bob")
    a.send(pb.Packet.FETCH_AVATAR_REQ, fetch_avatar_req=pb.FetchAvatarReq(username="bob", uid=ub))
    f = a.wait(pb.Packet.FETCH_AVATAR_RES)
    check("avatar fetched", f and f.fetch_avatar_res.data == jpeg)
    # Someone not watching hears none of it.
    c = User("carol")
    c.clear()
    b.send(pb.Packet.SET_STATUS_REQ, set_status_req=pb.SetStatusRequest(status=pb.PRESENCE_DND))
    d = a.wait(pb.Packet.PRESENCE_DELTA, pred=lambda p: p.presence_delta.user.uid == ub)
    check("a status change reaches the watcher", d and d.presence_delta.user.status == pb.PRESENCE_DND)
    c.drain(0.5)
    check("... and nobody else", not any(p.type in (pb.Packet.PRESENCE_DELTA, pb.Packet.AVATAR_CHANGED)
                                         for p in c.inbox))
    # A replaced set stops the deltas.
    check("dropping bob from the set", a.watch() is None)
    a.clear()
    b.send(pb.Packet.SET_STATUS_REQ, set_status_req=pb.SetStatusRequest(status=pb.PRESENCE_ONLINE))
    a.drain(0.5)
    check("... stops his deltas", not any(p.type == pb.Packet.PRESENCE_DELTA for p in a.inbox))
    ans = a.watch(ub)
    check("watching again answers his current state", ans is not None and [u.uid for u in ans.users] == [ub]
          and ans.users[0].status == pb.PRESENCE_ONLINE and ans.users[0].avatar_version == r.update_avatar_res.version)
    b.close()
    off = a.wait(pb.Packet.PRESENCE_DELTA, pred=lambda p: p.presence_delta.user.uid == ub)
    check("the watcher hears bob offline", off and not off.presence_delta.online)
    # Pictures of accounts that aren't visible ride the answer too.
    version = r.update_avatar_res.version
    a.watch()
    offline = a.watch(ub)
    check("watching an offline account: its picture", offline is not None and list(offline.covered_uids) == [ub]
          and len(offline.users) == 0 and [(v.uid, v.version) for v in offline.avatars] == [(ub, version)], offline)
    b = User("bob")
    a.wait(pb.Packet.PRESENCE_DELTA, pred=lambda p: p.presence_delta.user.uid == ub)
    b.send(pb.Packet.SET_STATUS_REQ, set_status_req=pb.SetStatusRequest(status=pb.PRESENCE_INVISIBLE))
    a.wait(pb.Packet.PRESENCE_DELTA, pred=lambda p: p.presence_delta.user.uid == ub and not p.presence_delta.online)
    a.watch()
    hidden = a.watch(ub)
    check("an invisible account answers exactly like an offline one",
          hidden is not None and hidden.SerializeToString() == offline.SerializeToString(), hidden)
    b.send(pb.Packet.SET_STATUS_REQ, set_status_req=pb.SetStatusRequest(status=pb.PRESENCE_ONLINE))
    a.wait(pb.Packet.PRESENCE_DELTA, pred=lambda p: p.presence_delta.user.uid == ub and p.presence_delta.online)
    a.watch()
    shown = a.watch(ub)
    check("a visible account's picture is in its entry only", shown is not None and len(shown.avatars) == 0
          and [u.avatar_version for u in shown.users] == [version])
    b.close()
    # The cap: the first 5000 accounts of a longer set.
    big = list(range(10_000_000, 10_006_000))
    ans = c.watch(*big)
    check("a watch set is cut at 5000", ans is not None and len(ans.covered_uids) == 5000
          and set(ans.covered_uids) == set(big[:5000]))
    c.close()
    time.sleep(0.3)
    a.watch()
    bare = a.watch(uid_of("carol"))
    check("no picture: covered, nothing else", bare is not None and list(bare.covered_uids) == [uid_of("carol")]
          and len(bare.users) == 0 and len(bare.avatars) == 0, bare)
    a.close()


def test_watch_cleanup():
    print("presence: a watcher's disconnect and re-login")
    ua, ub = uid_of("alice"), uid_of("bob")
    b = User("bob")
    a = User("alice")
    a.watch(ub)
    a.close()  # disconnects while watching
    time.sleep(0.3)
    b.send(pb.Packet.SET_STATUS_REQ, set_status_req=pb.SetStatusRequest(status=pb.PRESENCE_AWAY))
    time.sleep(0.3)
    a2 = User("alice")
    a2.clear()
    b.send(pb.Packet.SET_STATUS_REQ, set_status_req=pb.SetStatusRequest(status=pb.PRESENCE_ONLINE))
    a2.drain(0.5)
    check("a new login starts with an empty watch set", not any(p.type == pb.Packet.PRESENCE_DELTA for p in a2.inbox))
    # A second login replaces the first connection; the old one's set goes.
    a2.watch(ub)
    a3 = User("alice")
    a3.clear()
    time.sleep(0.3)
    b.send(pb.Packet.SET_STATUS_REQ, set_status_req=pb.SetStatusRequest(status=pb.PRESENCE_DND))
    a3.drain(0.5)
    check("the replacing login doesn't inherit the old set", not any(p.type == pb.Packet.PRESENCE_DELTA for p in a3.inbox))
    a2.drain(0.5)
    check("the replaced connection was closed", a2.closed)
    b.send(pb.Packet.SET_STATUS_REQ, set_status_req=pb.SetStatusRequest(status=pb.PRESENCE_ONLINE))
    a3.close(); b.close()


def test_login_storm():
    """bcrypt runs on the CPU pool: a burst of logins must not stall
    everyone else (it used to block the only io thread)."""
    print("login storm")
    import threading
    for i in range(12):
        register(f"storm{i}")
    a = User("alice")
    b = User("bob")
    done = []

    def login(i):
        u = User(f"storm{i}")
        done.append(u.ok)
        u.close()

    threads = [threading.Thread(target=login, args=(i,)) for i in range(12)]
    t0 = time.time()
    for t in threads:
        t.start()
    time.sleep(0.05)
    t1 = time.time()
    m = a.dm("bob", "during the storm", nonce="storm1")
    got = b.wait(pb.Packet.DIRECT_MSG, pred=lambda p: p.direct_msg.content == "during the storm")
    dm_latency = time.time() - t1
    for t in threads:
        t.join()
    storm = time.time() - t0
    check("every storm login succeeded", len(done) == 12 and all(done), done)
    check(f"a DM crosses during the storm ({dm_latency:.2f} s; the storm took {storm:.2f} s)",
          m is not None and got is not None and dm_latency < max(1.0, storm / 2))
    a.close(); b.close()


def test_calls():
    print("call signals")
    a = User("alice")
    a.send(pb.Packet.CALL_SIGNAL, call_signal=pb.CallSignal(kind=pb.CallSignal.INVITE, call_id="c1", to="bob"))
    r = a.wait(pb.Packet.CALL_SIGNAL)
    check("offline peer answers PEER_OFFLINE", r and r.call_signal.kind == pb.CallSignal.PEER_OFFLINE
          and getattr(r.call_signal, "from") == "bob")
    b = User("bob")
    a.send(pb.Packet.CALL_SIGNAL, call_signal=pb.CallSignal(kind=pb.CallSignal.INVITE, call_id="c2", to="bob"))
    r = b.wait(pb.Packet.CALL_SIGNAL)
    check("invite delivered, from stamped", r and getattr(r.call_signal, "from") == "alice" and r.call_signal.call_id == "c2")
    # S1: the callee's client answers an INVITE on its own (RINGING), so a
    # relayed signal carrying the sender's token would leak it zero-click.
    check("relayed invite carries no token", r and r.auth_token == "")
    b.send(pb.Packet.CALL_SIGNAL, call_signal=pb.CallSignal(kind=pb.CallSignal.RINGING, call_id="c2", to_uid=uid_of("alice")))
    r = a.wait(pb.Packet.CALL_SIGNAL, pred=lambda p: p.call_signal.kind == pb.CallSignal.RINGING)
    check("relayed ringing carries no token", r and r.auth_token == "" and r.call_signal.from_uid == uid_of("bob"))
    a.send(pb.Packet.CALL_SIGNAL, call_signal=pb.CallSignal(
        kind=pb.CallSignal.INVITE, call_id="c2b", to_uid=uid_of("bob"),
        candidates=[pb.CallCandidate(ip="1" * 46, port=1)]))
    check("oversized candidate address dropped", b.wait(pb.Packet.CALL_SIGNAL, timeout=0.8) is None)
    a.send(pb.Packet.CALL_SIGNAL, call_signal=pb.CallSignal(kind=pb.CallSignal.INVITE, call_id="c3", to="nosuchuser"))
    r = a.wait(pb.Packet.CALL_SIGNAL)
    check("unknown peer answers PEER_OFFLINE", r and r.call_signal.kind == pb.CallSignal.PEER_OFFLINE)
    # S13: only an INVITE is answered. A made-up CANCEL to someone offline
    # gets the same silence as one to someone invisible, so it can't probe.
    uc = uid_of("carol")
    a.send(pb.Packet.CALL_SIGNAL, call_signal=pb.CallSignal(kind=pb.CallSignal.CANCEL, call_id="probe", to_uid=uc))
    check("non-invite to an offline user gets no answer", a.wait(pb.Packet.CALL_SIGNAL, timeout=0.8) is None)
    a.send(pb.Packet.CALL_SIGNAL, call_signal=pb.CallSignal(kind=pb.CallSignal.PEER_OFFLINE, call_id="c2", to_uid=uid_of("bob")))
    check("clients can't send central's own kinds", b.wait(pb.Packet.CALL_SIGNAL, timeout=0.8) is None)
    a.close(); b.close()


def test_blocks():
    """S12 (review 2026-10-17): a block belongs to whoever placed it."""
    print("blocks")
    ua, uc = uid_of("alice"), uid_of("carol")
    a, c = User("alice"), User("carol")
    check("carol blocks alice", c.friend(pb.BLOCK, target_uid=ua).success)
    r = a.friend(pb.REMOVE, target_uid=uc)
    check("the blocked side's REMOVE is a quiet no-op", r and r.success)
    check("still blocked after it", a.dm(recipient_uid=uc, content="unblocked?", nonce="b1").id == 0)
    a.friend(pb.REJECT, target_uid=uc)
    check("REJECT can't lift it either", a.dm(recipient_uid=uc, content="unblocked?", nonce="b2").id == 0)
    c.clear()
    a.send(pb.Packet.CALL_SIGNAL, call_signal=pb.CallSignal(kind=pb.CallSignal.HANGUP, call_id="x", to_uid=uc))
    check("a blocked user's non-invite signals are dropped", c.wait(pb.Packet.CALL_SIGNAL, timeout=0.8) is None)
    check("blocking back succeeds", a.friend(pb.BLOCK, target_uid=uc).success)
    check("blocking back keeps carol's block listed", c.friends()["alice"].status == pb.FriendInfo.BLOCKED)
    check("and lists alice's own", a.friends()["carol"].status == pb.FriendInfo.BLOCKED)
    c.friend(pb.REMOVE, target_uid=ua)
    check("carol lifting hers leaves alice's", c.dm(recipient_uid=ua, content="hi?", nonce="b3").id == 0)
    check("carol no longer lists one", "alice" not in c.friends())
    a.friend(pb.REMOVE, target_uid=uc)
    check("both lifted: DMs flow again", a.dm(recipient_uid=uc, content="hello again", nonce="b4").id > 0)
    a.close(); c.close()


def bundle(seed):
    return pb.E2eeKeyBundle(dh_pub=bytes([seed]) * 32, sign_pub=bytes([seed + 1]) * 32, signature=bytes([seed + 2]) * 64)


def test_e2ee_keys():
    print("e2ee keys")
    a, b = User("alice"), User("bob")
    b.watch(uid_of("alice"))
    a.send(pb.Packet.E2EE_PUBLISH_KEYS_REQ, e2ee_publish_keys_req=pb.E2eePublishKeysReq(bundle=bundle(1), backup=b"wrapped"))
    r = a.wait(pb.Packet.E2EE_PUBLISH_KEYS_RES)
    check("publish", r and r.e2ee_publish_keys_res.success and r.e2ee_publish_keys_res.key_id == 1)
    ch = b.wait(pb.Packet.E2EE_KEYS_CHANGED)
    check("keys-changed reaches the watcher", ch and ch.e2ee_keys_changed.username == "alice" and ch.e2ee_keys_changed.key_id == 1)
    b.send(pb.Packet.E2EE_FETCH_KEYS_REQ, e2ee_fetch_keys_req=pb.E2eeFetchKeysReq(username="alice"))
    f = b.wait(pb.Packet.E2EE_FETCH_KEYS_RES)
    check("fetch current", f and f.e2ee_fetch_keys_res.found and f.e2ee_fetch_keys_res.bundle.dh_pub == bytes([1]) * 32)
    a.send(pb.Packet.E2EE_PUBLISH_KEYS_REQ, e2ee_publish_keys_req=pb.E2eePublishKeysReq(bundle=bundle(5)))
    r = a.wait(pb.Packet.E2EE_PUBLISH_KEYS_RES)
    check("second bundle is key 2", r and r.e2ee_publish_keys_res.key_id == 2)
    b.send(pb.Packet.E2EE_FETCH_KEYS_REQ, e2ee_fetch_keys_req=pb.E2eeFetchKeysReq(username="alice", key_id=1))
    f = b.wait(pb.Packet.E2EE_FETCH_KEYS_RES)
    check("old bundle still served", f and f.e2ee_fetch_keys_res.found and f.e2ee_fetch_keys_res.bundle.key_id == 1)
    a.send(pb.Packet.E2EE_FETCH_BACKUP_REQ, e2ee_fetch_backup_req=pb.E2eeFetchBackupReq())
    f = a.wait(pb.Packet.E2EE_FETCH_BACKUP_RES)
    check("own backup", f and f.e2ee_fetch_backup_res.found and f.e2ee_fetch_backup_res.backup == b"wrapped")
    b.send(pb.Packet.E2EE_FETCH_KEYS_REQ, e2ee_fetch_keys_req=pb.E2eeFetchKeysReq(username="nosuchuser"))
    f = b.wait(pb.Packet.E2EE_FETCH_KEYS_RES)
    check("unknown user: not found", f and not f.e2ee_fetch_keys_res.found)
    a.close(); b.close()


def test_e2ee_binding():
    print("e2ee uid binding (stage 4)")
    register("erin")
    register("frank")
    e, f = User("erin"), User("frank")
    f.watch(e.login.uid)
    check("login advertises the uid binding", e.login.e2ee_uid_binding)

    def publish(req):
        e.send(pb.Packet.E2EE_PUBLISH_KEYS_REQ, e2ee_publish_keys_req=req)
        r = e.wait(pb.Packet.E2EE_PUBLISH_KEYS_RES)
        return r.e2ee_publish_keys_res if r else None

    def fetch(key_id=0):
        f.send(pb.Packet.E2EE_FETCH_KEYS_REQ, e2ee_fetch_keys_req=pb.E2eeFetchKeysReq(uid=e.login.uid, key_id=key_id))
        r = f.wait(pb.Packet.E2EE_FETCH_KEYS_RES)
        return r.e2ee_fetch_keys_res if r else None

    r = publish(pb.E2eePublishKeysReq(bundle=bundle(11), backup=b"erin-wrapped"))
    check("v1 publish", r and r.success and r.key_id == 1)
    x = fetch()
    check("unset binding is stored as the name", x and x.found and x.bundle.binding == 1 and x.bundle.signed_name == "erin")
    f.clear()

    def rebind(seed, key_id, sig=b"\x55" * 64, **kw):
        b = bundle(seed)
        b.key_id = key_id
        b.signature = sig
        b.binding = 2
        return publish(pb.E2eePublishKeysReq(bundle=b, rebind=True, **kw))

    r = rebind(11, 1)
    check("rebind accepted, same key id", r and r.success and r.key_id == 1, r)
    ch = f.wait(pb.Packet.E2EE_KEYS_CHANGED)
    check("rebind broadcasts keys-changed with the same id", ch and ch.e2ee_keys_changed.uid == e.login.uid
          and ch.e2ee_keys_changed.key_id == 1)
    x = fetch()
    check("rebound in place", x and x.bundle.key_id == 1 and x.bundle.binding == 2
          and x.bundle.signature == b"\x55" * 64 and x.bundle.dh_pub == bytes([11]) * 32)
    check("still one row", sql("SELECT count(*) FROM e2ee_keys WHERE uid = %s", e.login.uid)[0][0] == 1)
    r = rebind(12, 1)
    check("rebind with other keys refused", r and not r.success and r.key_id == 1)
    r = rebind(11, 0)
    check("rebind without a key id refused", r and not r.success)
    r = rebind(11, 1, backup=b"x")
    check("rebind with a backup refused", r and not r.success)
    r = publish(pb.E2eePublishKeysReq(bundle=pb.E2eeKeyBundle(
        dh_pub=bytes([13]) * 32, sign_pub=bytes([14]) * 32, signature=bytes([15]) * 64, binding=3)))
    check("unknown binding refused", r and not r.success)
    r = publish(pb.E2eePublishKeysReq(bundle=pb.E2eeKeyBundle(
        dh_pub=bytes([13]) * 32, sign_pub=bytes([14]) * 32, signature=bytes([15]) * 64, binding=2)))
    check("v2 publish", r and r.success and r.key_id == 2)
    x = fetch()
    check("v2 bundle served with its binding", x and x.bundle.key_id == 2 and x.bundle.binding == 2)
    r = rebind(11, 1)
    check("rebinding an old generation refused", r and not r.success and r.key_id == 2)
    x = fetch(1)
    check("old generation untouched", x and x.bundle.binding == 2 and x.bundle.dh_pub == bytes([11]) * 32)
    e.send(pb.Packet.E2EE_FETCH_BACKUP_REQ, e2ee_fetch_backup_req=pb.E2eeFetchBackupReq())
    b = e.wait(pb.Packet.E2EE_FETCH_BACKUP_RES)
    check("backup untouched by rebinds", b and b.e2ee_fetch_backup_res.backup == b"erin-wrapped")
    e.close(); f.close()


def test_memberships():
    print("community memberships")
    sid = heartbeat()
    check("heartbeat assigns an id", sid > 0)
    ua = uid_of("alice")
    c = community_send(pb.Packet.MEMBERSHIP_REGISTER_REQ,
                       membership_register_req=pb.MembershipRegisterReq(username="alice", uid=ua, server_id=sid))
    time.sleep(0.3); c.close()
    a = User("alice")
    check("login lists the membership", [m.id for m in a.login.memberships] == [sid])
    a.close()
    # A name alone addresses nobody (users by uid).
    c = community_send(pb.Packet.MEMBERSHIP_REVOKE_REQ,
                       membership_revoke_req=pb.MembershipRevokeReq(username="alice", server_id=sid))
    time.sleep(0.3); c.close()
    a = User("alice")
    check("a name-only revoke is ignored", [m.id for m in a.login.memberships] == [sid])
    a.close()
    c = community_send(pb.Packet.MEMBERSHIP_REVOKE_REQ,
                       membership_revoke_req=pb.MembershipRevokeReq(uid=ua, server_id=sid))
    time.sleep(0.3); c.close()
    a = User("alice")
    check("revoke removes it", len(a.login.memberships) == 0)
    a.close()


def uid_of(name):
    rows = sql("SELECT uid FROM users WHERE username = %s", name)
    return rows[0][0] if rows else 0


def test_uid_wire():
    print("uids on the wire")
    a, b = User("alice"), User("bob")
    ua, ub = uid_of("alice"), uid_of("bob")
    check("login returns the uid", a.login.uid == ua and a.login.username == "alice")
    check("jwt uid matches", int(a.claims()["uid"]) == ua)
    fb = b.friends()
    check("friend entry carries the uid", fb["alice"].uid == ua)
    a.clear(); b.clear()
    # A DM addressed only by uid.
    m = a.dm(recipient_uid=ub, content="by uid", nonce="u1")
    check("dm by uid delivered", m and m.id > 0 and m.recipient == "bob" and m.recipient_uid == ub
          and m.sender_uid == ua, m)
    got = b.wait(pb.Packet.DIRECT_MSG, pred=lambda p: p.direct_msg.content == "by uid")
    check("recipient sees sender uid", got and got.direct_msg.sender_uid == ua)
    sealed = a.dm(recipient_uid=ub, content="[encrypted]", nonce="u2", envelope=b"\x01sealed")
    r = b.dm(recipient_uid=ua, content="re", nonce="u3", reply_to=sealed.id)
    check("reply parent by uid, sealed names", r and r.reply_to_sender_uid == ua
          and r.reply_to_sealed_sender == "alice" and r.reply_to_sealed_recipient == "bob")
    h = b.history(peer_uid=ua)
    check("history by uid", h and h.peer == "alice" and h.peer_uid == ua and len(h.messages) >= 3)
    row = next((x for x in h.messages if x.id == sealed.id), None) if h else None
    check("envelope row carries sealed names", row and row.sender_uid == ua and row.sealed_sender == "alice"
          and row.sealed_recipient == "bob")
    plain = next((x for x in h.messages if x.content == "by uid"), None) if h else None
    check("plaintext row has no sealed names", plain and plain.sealed_sender == "" and plain.sender_uid == ua)
    convs = b.conversations()
    c = convs and convs.get("alice")
    check("conversation carries uids", c and c.peer_uid == ua and c.last_message_sender_uid == ub)
    b.send(pb.Packet.DM_MARK_READ_REQ, dm_mark_read_req=pb.DmMarkReadReq(peer_uid=ua, up_to_id=r.id))
    time.sleep(0.2)
    check("mark read by uid", b.conversations()["alice"].unread_count == 0)
    b.send(pb.Packet.DM_EDIT_REQ, dm_edit_req=pb.DmEditReq(peer_uid=ua, message_id=r.id, content="re2"))
    e = b.wait(pb.Packet.DM_EDIT_RES)
    check("edit by uid", e and e.dm_edit_res.success and e.dm_edit_res.peer_uid == ua)
    ed = a.wait(pb.Packet.DM_MESSAGE_EDITED)
    check("edit broadcast carries uids", ed and ed.dm_message_edited.peer_uid == ub and ed.dm_message_edited.sender_uid == ub)
    b.send(pb.Packet.DM_DELETE_REQ, dm_delete_req=pb.DmDeleteReq(peer_uid=ua, message_id=r.id))
    d = b.wait(pb.Packet.DM_DELETE_RES)
    check("delete by uid", d and d.dm_delete_res.success)
    dd = a.wait(pb.Packet.DM_MESSAGE_DELETED)
    check("delete broadcast carries uid", dd and dd.dm_message_deleted.peer_uid == ub)
    r = a.dm(recipient_uid=987654, content="forged", nonce="u4")
    check("dm to a forged uid refused", r and r.id == 0 and r.content == "This user doesn't exist.")
    # Calls by uid.
    a.clear(); b.clear()
    a.send(pb.Packet.CALL_SIGNAL, call_signal=pb.CallSignal(kind=pb.CallSignal.INVITE, call_id="u5", to_uid=ub))
    sig = b.wait(pb.Packet.CALL_SIGNAL)
    check("call by uid, from_uid stamped", sig and sig.call_signal.from_uid == ua
          and getattr(sig.call_signal, "from") == "alice" and sig.call_signal.to_uid == ub)
    # Friends by uid.
    r = a.friend(pb.REMOVE, target_uid=ub)
    check("friend action by uid", r and r.success)
    check("removed", "bob" not in a.friends())
    r = a.friend(pb.ADD, target_uid=987654)
    check("friend action on a forged uid", r and not r.success and r.message == "User not found.")
    # Only ADD takes a typed name: anything else names its target by uid.
    r = a.friend(pb.BLOCK, "bob")
    check("a name-only BLOCK addresses nobody", r and not r.success and r.message == "User not found.")
    a.friend(pb.ADD, target_uid=ub); b.friend(pb.ACCEPT, target_uid=ua)
    # Keys and avatars by uid.
    b.send(pb.Packet.E2EE_FETCH_KEYS_REQ, e2ee_fetch_keys_req=pb.E2eeFetchKeysReq(uid=ua))
    f = b.wait(pb.Packet.E2EE_FETCH_KEYS_RES)
    check("keys by uid", f and f.e2ee_fetch_keys_res.found and f.e2ee_fetch_keys_res.uid == ua
          and f.e2ee_fetch_keys_res.bundle.uid == ua and f.e2ee_fetch_keys_res.bundle.signed_name == "alice")
    a.send(pb.Packet.FETCH_AVATAR_REQ, fetch_avatar_req=pb.FetchAvatarReq(uid=ub))
    f = a.wait(pb.Packet.FETCH_AVATAR_RES)
    check("avatar by uid", f and f.fetch_avatar_res.uid == ub and f.fetch_avatar_res.username == "bob"
          and f.fetch_avatar_res.data)
    # Presence carries uids.
    a.watch(uid_of("carol"))
    a.clear()
    c = User("carol")
    delta = a.wait(pb.Packet.PRESENCE_DELTA, pred=lambda p: p.presence_delta.user.username == "carol")
    check("presence delta carries the uid", delta and delta.presence_delta.user.uid == uid_of("carol"))
    snap = c.watch(ua)
    check("subscription answers carry uids", snap and all(u.uid > 0 for u in snap.users) and ua in snap.covered_uids)
    a.close(); b.close(); c.close()


def test_memberships_by_uid():
    print("memberships by uid")
    sid = heartbeat(name="Second", port=9001)
    ub = uid_of("bob")
    c = community_send(pb.Packet.MEMBERSHIP_REGISTER_REQ,
                       membership_register_req=pb.MembershipRegisterReq(uid=ub, server_id=sid))
    time.sleep(0.3); c.close()
    c = community_send(pb.Packet.MEMBERSHIP_REGISTER_REQ,
                       membership_register_req=pb.MembershipRegisterReq(uid=987654, server_id=sid))
    time.sleep(0.3); c.close()
    b = User("bob")
    check("registered by uid", sid in [m.id for m in b.login.memberships])
    b.close()
    check("forged uid stored nothing", sql("SELECT count(*) FROM community_memberships WHERE uid = 987654")[0][0] == 0)
    c = community_send(pb.Packet.MEMBERSHIP_REVOKE_REQ,
                       membership_revoke_req=pb.MembershipRevokeReq(uid=ub, server_id=sid))
    time.sleep(0.3); c.close()
    b = User("bob")
    check("revoked by uid", sid not in [m.id for m in b.login.memberships])
    b.close()


def test_friends_in_community():
    """Invite friends: central says which of my friends are already in a
    community I'm in, from its membership table (the community never sees the
    friend list), and nothing about one I'm not in."""
    print("friends in a community (invite friends)")
    for n in ("gail", "hank", "iris", "jude"):
        register(n)
    sid = heartbeat(name="Friends", port=9005)
    other = heartbeat(name="Elsewhere", port=9006)
    ug, uh, ui, uj = (uid_of(n) for n in ("gail", "hank", "iris", "jude"))
    for u, s in ((ug, sid), (uh, sid), (uj, sid), (ui, other)):
        c = community_send(pb.Packet.MEMBERSHIP_REGISTER_REQ,
                           membership_register_req=pb.MembershipRegisterReq(uid=u, server_id=s))
        time.sleep(0.2); c.close()
    g, h, i, j = User("gail"), User("hank"), User("iris"), User("jude")
    for peer in (h, i):   # gail's friends: hank (in), iris (elsewhere); jude is in but no friend
        g.friend(pb.ADD, target_uid=peer.login.uid)
        peer.friend(pb.ACCEPT, target_uid=ug)
    time.sleep(0.3)

    def ask(u, server_id):
        u.send(pb.Packet.FRIENDS_IN_COMMUNITY_REQ, friends_in_community_req=pb.FriendsInCommunityRequest(server_id=server_id))
        r = u.wait(pb.Packet.FRIENDS_IN_COMMUNITY_RES, pred=lambda p: p.friends_in_community_res.server_id == server_id)
        return r.friends_in_community_res if r else None

    r = ask(g, sid)
    check("friends who are members, and only they", r is not None and r.success and list(r.member_uids) == [uh],
          r and list(r.member_uids))
    r = ask(g, other)
    check("nothing about a community the caller isn't in", r is not None and not r.success and not r.member_uids)
    r = ask(i, sid)
    check("a friend who isn't a member learns nothing about it", r is not None and not r.success)
    r = ask(h, sid)
    check("the other side of the friendship sees the caller", r is not None and r.success and list(r.member_uids) == [ug])
    for x in (g, h, i, j):
        x.close()


def test_rename_keeps_everything():
    """There is no rename feature yet (stage 5); renaming the row directly
    shows every table follows the uid, and a newcomer under the old name
    inherits nothing."""
    print("rename (simulated)")
    sid = heartbeat(name="Third", port=9002)
    ua, ub = uid_of("alice"), uid_of("bob")
    c = community_send(pb.Packet.MEMBERSHIP_REGISTER_REQ,
                       membership_register_req=pb.MembershipRegisterReq(uid=ua, server_id=sid))
    time.sleep(0.3); c.close()
    sql("UPDATE users SET username = 'alice2' WHERE uid = %s", ua)
    a = User("alice2")
    check("renamed user logs in, same uid", a.ok and a.login.uid == ua)
    check("memberships follow", sid in [m.id for m in a.login.memberships])
    b = User("bob")
    fb = b.friends()
    check("friendship follows, current name", "alice2" in fb and fb["alice2"].uid == ua and "alice" not in fb)
    h = b.history(peer_uid=ua)
    check("history shows the current name", h and h.peer == "alice2"
          and all(x.sender in ("alice2", "bob") for x in h.messages))
    sealed = [x for x in h.messages if x.envelope] if h else []
    check("sealed rows keep the names they were sealed under", sealed and all(
        x.sealed_sender in ("alice", "bob") and x.sealed_recipient in ("alice", "bob") for x in sealed))
    convs = b.conversations()
    check("conversation under the current name", convs and "alice2" in convs and convs["alice2"].peer_uid == ua)
    b.send(pb.Packet.E2EE_FETCH_KEYS_REQ, e2ee_fetch_keys_req=pb.E2eeFetchKeysReq(username="alice2"))
    f = b.wait(pb.Packet.E2EE_FETCH_KEYS_RES)
    check("keys follow, signed name kept", f and f.e2ee_fetch_keys_res.found
          and f.e2ee_fetch_keys_res.bundle.username == "alice2" and f.e2ee_fetch_keys_res.bundle.signed_name == "alice")
    a.send(pb.Packet.E2EE_FETCH_BACKUP_REQ, e2ee_fetch_backup_req=pb.E2eeFetchBackupReq())
    f = a.wait(pb.Packet.E2EE_FETCH_BACKUP_RES)
    check("backup follows", f and f.e2ee_fetch_backup_res.found)
    # A newcomer takes the old name.
    check("newcomer registers the old name", register("alice", email="new-alice@example.com").success)
    q = User("alice")
    check("newcomer gets a new uid", q.ok and q.login.uid not in (0, ua))
    check("newcomer: no memberships", len(q.login.memberships) == 0)
    check("newcomer: no friends", q.friends() == {})
    check("newcomer: no conversations", q.conversations() == {})
    h = q.history("bob", uid_of("bob"))
    check("newcomer: no history with bob", h and len(h.messages) == 0)
    q.send(pb.Packet.E2EE_FETCH_BACKUP_REQ, e2ee_fetch_backup_req=pb.E2eeFetchBackupReq())
    f = q.wait(pb.Packet.E2EE_FETCH_BACKUP_RES)
    check("newcomer: no key backup", f and not f.e2ee_fetch_backup_res.found)
    b.send(pb.Packet.E2EE_FETCH_KEYS_REQ, e2ee_fetch_keys_req=pb.E2eeFetchKeysReq(username="alice"))
    f = b.wait(pb.Packet.E2EE_FETCH_KEYS_RES)
    check("newcomer: no keys under the name", f and not f.e2ee_fetch_keys_res.found)
    # Old alice's sent DM can't be edited by the newcomer, even by name.
    old = next((x for x in b.history(peer_uid=ua).messages if x.sender_uid == ua), None)
    q.send(pb.Packet.DM_EDIT_REQ, dm_edit_req=pb.DmEditReq(peer="bob", peer_uid=uid_of("bob"), message_id=old.id, content="hijack"))
    r = q.wait(pb.Packet.DM_EDIT_RES)
    check("newcomer can't edit old alice's DM", r and not r.dm_edit_res.success)
    m = q.dm("bob", "hi, I'm new", nonce="q1")
    got = b.wait(pb.Packet.DIRECT_MSG, pred=lambda p: p.direct_msg.content == "hi, I'm new")
    check("newcomer's DM is a separate conversation", got and got.direct_msg.sender_uid == q.login.uid
          and set(b.conversations()) >= {"alice", "alice2"})
    q.close(); a.close(); b.close()


def test_rename():
    print("rename (stage 5)")
    for n in ("gina", "hank", "ivan"):
        register(n)
    g, h = User("gina"), User("hank")
    ug = g.login.uid
    check("login advertises renames", g.login.renames)

    def rename(u, name, pw="password123"):
        u.send(pb.Packet.RENAME_REQ, rename_req=pb.RenameRequest(new_username=name, password=pw))
        r = u.wait(pb.Packet.RENAME_RES)
        return r.rename_res if r else None

    r = rename(g, "gina2", pw="wrong-password")
    check("wrong password refused", r and not r.success and r.username == "gina" and r.message == "Wrong password.", r)
    r = rename(g, "Gina2")
    check("registration rules apply", r and not r.success and "lowercase" in r.message)
    r = rename(g, "hank")
    check("a taken name refused", r and not r.success and r.message == "That username is taken.")
    r = rename(g, "gina")
    check("the same name refused", r and not r.success)
    h.watch(ug, uid_of("ivan"))
    h.clear()
    lurker = Conn()   # connected, never signed in
    r = rename(g, "gina2")
    check("renamed", r and r.success and r.username == "gina2" and r.jwt_token, r)
    g.token = r.jwt_token if r else g.token
    claims = g.claims()
    check("fresh token names the new username, same uid", claims.get("sub") == "gina2" and claims.get("uid") == ug)
    rn = h.wait(pb.Packet.USER_RENAMED)
    check("others are told the new name by uid", rn and rn.user_renamed.uid == ug
          and rn.user_renamed.username == "gina2")
    # Unknown fields survive a parse, so an old name on the wire would show up here.
    check("... and nothing else (no old name)", rn and rn.user_renamed.SerializeToString()
          == pb.UserRenamed(uid=ug, username="gina2").SerializeToString())
    check("a connection that never signed in hears nothing", lurker.wait(pb.Packet.USER_RENAMED, timeout=0.5) is None)
    lurker.close()
    check("audit row", sql("SELECT old_username, new_username FROM name_changes WHERE uid = %s", ug) == [("gina", "gina2")])
    m = g.dm("hank", "under my new name", nonce="rn1")
    check("the session acts under the new name", m and m.sender == "gina2" and m.sender_uid == ug)
    check("the old name is reserved for registration", not register("gina", email="other-gina@example.com").success)
    i = User("ivan")
    r = rename(i, "gina")
    check("... and for other renames", r and not r.success and r.message == "That username is taken.")
    r = rename(g, "gina")
    check("renaming back to your own reserved name", r and r.success and r.username == "gina")
    g.token = r.jwt_token if r else g.token
    r = rename(g, "gina3")
    check("two renames a day", r and not r.success and "twice a day" in r.message, r)
    # Invisible: announced all the same (owner: profile pushes may reveal activity).
    i.send(pb.Packet.SET_STATUS_REQ, set_status_req=pb.SetStatusRequest(status=pb.PRESENCE_INVISIBLE))
    time.sleep(0.2)
    h.clear()
    r = rename(i, "ivan2")
    check("invisible user renamed", r and r.success)
    rn = h.wait(pb.Packet.USER_RENAMED)
    check("... and announced like anyone's", rn and rn.user_renamed.uid == uid_of("ivan2")
          and rn.user_renamed.username == "ivan2")
    i.close()
    # Signing in with a reserved old name reaches the account, under its current name.
    old = User("ivan")
    check("old name signs in during the reservation", old.ok and old.login.uid == uid_of("ivan2")
          and old.login.username == "ivan2" and old.claims().get("sub") == "ivan2")
    old.close()
    # After the reservation the old name is free.
    sql("UPDATE name_changes SET changed_at = changed_at - %s WHERE uid IN (%s, %s)",
        15 * 24 * 3600, ug, uid_of("ivan2"))
    check("old name no longer signs in", not User("ivan").ok)
    check("old name free to register after 14 days", register("gina2", email="new-gina2@example.com").success)
    # Renames past the reservation are forgotten (hourly, and with every rename).
    register("jill")
    j = User("jill")
    r = rename(j, "jill2")
    check("expired renames forgotten",
          r and r.success and sql("SELECT count(*) FROM name_changes WHERE uid IN (%s, %s)", ug, uid_of("ivan2")) == [(0,)]
          and sql("SELECT old_username FROM name_changes WHERE uid = %s", j.login.uid) == [("jill",)])
    j.close()
    g.close(); h.close()


def test_token_refresh():
    print("token refresh")
    a = User("hank")
    old = a.claims()
    time.sleep(1.1)
    a.send(pb.Packet.TOKEN_REFRESH_REQ, token_refresh_req=pb.TokenRefreshRequest())
    r = a.wait(pb.Packet.TOKEN_REFRESH_RES)
    check("fresh token issued", r and r.token_refresh_res.success and r.token_refresh_res.username == "hank"
          and r.token_refresh_res.jwt_token and r.token_refresh_res.jwt_token != a.token)
    if r:
        a.token = r.token_refresh_res.jwt_token
    new = a.claims()
    check("same account, later issue time", new.get("uid") == old.get("uid") and new.get("sub") == "hank"
          and new.get("iat", 0) > old.get("iat", 0))
    convs = a.conversations()
    check("the session keeps working on the new token", convs is not None)
    a.close()
    c = Conn()
    c.send(pb.Packet.TOKEN_REFRESH_REQ, token_refresh_req=pb.TokenRefreshRequest())
    check("refused without a session", c.wait(pb.Packet.TOKEN_REFRESH_RES, timeout=1.0) is None)
    c.close()


def test_resume():
    print("resume after a dropped connection")
    register("rhea")
    a = User("rhea")
    rt = a.login.resume_token if a.ok else ""
    claims = jwt_claims(rt) if rt else {}
    check("a password sign-in hands out a resume token", rt and rt != a.token
          and claims.get("aud") == "decibell_central_resume" and claims.get("exp", 0) - claims.get("iat", 0) == 30 * 86400,
          claims)
    check("the session token has no audience", "aud" not in a.claims())
    b = User("", resume=rt)
    check("resume signs the account in", b.ok and b.login.uid == a.login.uid and b.login.username == "rhea"
          and jwt_claims(b.token).get("uid") == a.login.uid, b.login)
    check("resume echoes its token (no extension)", b.ok and b.login.resume_token == rt)
    check("the resumed session works", b.conversations() is not None)
    a.drain(0.5)
    check("it replaced the earlier connection", a.closed)
    a.close()
    check("a session token isn't a resume token", not User("", resume=b.token).ok)
    check("nor is junk", not User("", resume="x.y.z").ok)
    now = int(time.time())
    expired = mint_jwt({"iss": "decibell_central_auth", "aud": "decibell_central_resume", "uid": b.login.uid,
                        "iat": now - 31 * 86400, "exp": now - 86400})
    check("an expired resume token is refused", not User("", resume=expired).ok)
    other = mint_jwt({"iss": "decibell_central_auth", "aud": "somewhere_else", "uid": b.login.uid,
                      "iat": now, "exp": now + 3600})
    check("another audience is refused", not User("", resume=other).ok)
    c = Conn()
    c.token = rt
    c.send(pb.Packet.DM_CONVERSATIONS_REQ, dm_conversations_req=pb.DmConversationsReq())
    c.drain(0.5)
    check("a resume token isn't a session token", c.closed or not any(
        p.type == pb.Packet.DM_CONVERSATIONS_RES for p in c.inbox))
    c.close()
    # A resume follows a rename (the token names the uid only).
    b.send(pb.Packet.RENAME_REQ, rename_req=pb.RenameRequest(new_username="rhea2", password="password123"))
    r = b.wait(pb.Packet.RENAME_RES)
    check("renamed", r and r.rename_res.success, r and r.rename_res.message)
    b.close()
    d = User("", resume=rt)
    check("a resume signs in under the current name", d.ok and d.login.username == "rhea2"
          and jwt_claims(d.token).get("sub") == "rhea2")
    d.close()
    # Cancelling: a cutoff after the token's password sign-in refuses it;
    # a password sign-in after the cutoff hands out one that works.
    time.sleep(1.1)
    sql("UPDATE users SET sessions_valid_after = %s WHERE uid = %s", int(time.time()), d.login.uid)
    check("a resume token from before the cutoff is refused", not User("", resume=rt).ok)
    e = User("rhea2")
    rt2 = e.login.resume_token if e.ok else ""
    e.close()
    check("one from a later password sign-in works", rt2 and User("", resume=rt2).ok)


def test_dm_pacing():
    print("DM pacing")
    a, b = User("alice"), User("bob")
    ub = uid_of("bob")
    t0 = time.time()
    for i in range(20):
        a.send(pb.Packet.DIRECT_MSG, direct_msg=pb.DirectMessage(recipient_uid=ub, content=f"burst {i}", nonce=f"burst{i}"))
    got = []
    end = time.time() + 10
    while len(got) < 20 and time.time() < end:
        p = b.wait(pb.Packet.DIRECT_MSG, timeout=max(0.1, end - time.time()))
        if p and p.direct_msg.content.startswith("burst "):
            got.append(p.direct_msg.content)
    elapsed = time.time() - t0
    check("a burst of 20 DMs all arrives, in order", got == [f"burst {i}" for i in range(20)], got[-3:])
    check(f"... paced to 10 at once, then 3 a second ({elapsed:.1f} s)", 2.5 <= elapsed <= 6)
    a.close(); b.close()


# --- Response sizes (review A1) -----------------------------------------------
# The native client drops its connection over any frame past 2 MiB and asks
# again after reconnecting, so a response that big locks a user out. Big or
# old rows are seeded straight into the database: the wire caps them now.

CLIENT_FRAME = 2 * 1024 * 1024  # native net/framing.rs MAX_FRAME_SIZE


def seed_users(prefix, n):
    """`n` accounts named prefix1..prefixN, straight into the database (no
    bcrypt; they can't sign in by password). Returns their uids in order."""
    sql("INSERT INTO users (username, email, password_hash) "
        "SELECT %s::text || g, %s::text || g || '@example.com', 'x' FROM generate_series(1, %s) g",
        prefix, prefix, n)
    return [r[0] for r in sql("SELECT uid FROM users WHERE username LIKE %s ORDER BY uid", prefix + "%")]


def drop_users(uids):
    """Seeded accounts and everything that names them."""
    u = list(uids)
    sql("DELETE FROM friendships WHERE uid1 = ANY(%s) OR uid2 = ANY(%s)", u, u)
    sql("DELETE FROM dm_read_cursors WHERE reader_uid = ANY(%s) OR peer_uid = ANY(%s)", u, u)
    sql("DELETE FROM dm_messages WHERE sender_uid = ANY(%s) OR recipient_uid = ANY(%s)", u, u)
    sql("DELETE FROM community_memberships WHERE uid = ANY(%s)", u)
    sql("DELETE FROM users WHERE uid = ANY(%s)", u)


def seed_dms(sender_uid, sender, recipient_uid, recipient, content_sql, n=1, envelope=None):
    """`n` DMs from sender to recipient with the body `content_sql` (an SQL
    expression in g, 1..n); returns their ids, oldest first."""
    rows = sql("INSERT INTO dm_messages (sender_uid, sender, recipient_uid, recipient, content, sent_at, envelope) "
               f"SELECT %s::bigint, %s::text, %s::bigint, %s::text, {content_sql}, 1700000000 + g, %s::bytea "
               "FROM generate_series(1, %s) g RETURNING id",
               sender_uid, sender, recipient_uid, recipient, envelope, n)
    return sorted(r[0] for r in rows)


def utf8_len(s):
    return len(s.encode())


def test_dm_page_budget():
    print("DM history pages within the frame cap (review A1)")
    [up] = seed_users("pagepeer", 1)
    ua = uid_of("alice")
    # 40 messages of 60 KiB: 2.4 MB, all inside the default page's 50 rows.
    ids = seed_dms(up, "pagepeer1", ua, "alice", "repeat(chr(96 + g), 61440)", n=40)
    a = User("alice")
    h = a.history(peer_uid=up, limit=50)
    size = a.sizes.get(pb.Packet.DM_HISTORY_RES, 0)
    check("a history page stays under the client's frame cap", h is not None and size <= CLIENT_FRAME, size)
    check("... cut by bytes, and says older rows follow", h is not None and 0 < len(h.messages) < 40 and h.has_more,
          h and (len(h.messages), h.has_more))
    got, pages = [m.id for m in h.messages] if h else [], 1
    while h is not None and h.has_more and pages < 10:
        h = a.history(peer_uid=up, before_id=min(got), limit=50)
        pages += 1
        if h is not None:
            got += [m.id for m in h.messages]
    check(f"paging on from the oldest row collects every row once ({pages} pages)",
          sorted(got) == ids and len(set(got)) == len(got))
    # A window around a message keeps the target and marks both edges.
    mid = ids[20]
    w = a.history(peer_uid=up, around_id=mid, limit=100)
    wids = [m.id for m in w.messages] if w else []
    check("a window around a message: under the cap, target kept, both edges marked",
          w is not None and a.sizes.get(pb.Packet.DM_HISTORY_RES, 0) <= CLIENT_FRAME and mid in wids
          and w.has_more and w.has_more_after and len(wids) < 40
          and wids == ids[ids.index(wids[0]):ids.index(wids[0]) + len(wids)], wids)
    n = a.history(peer_uid=up, after_id=ids[0], limit=100)
    nids = [m.id for m in n.messages] if n else []
    check("a newer page: under the cap, oldest first, newer rows marked",
          n is not None and a.sizes.get(pb.Packet.DM_HISTORY_RES, 0) <= CLIENT_FRAME and n.has_more_after
          and 0 < len(nids) < 39 and nids == ids[1:1 + len(nids)], nids)
    # A message stored before the 64 KiB cap (2.1 MB of 3-byte characters)
    # comes cut on a character boundary, and so does a reply's quote of it.
    [huge] = seed_dms(up, "pagepeer1", ua, "alice", "repeat('€', 700000)")
    h = a.history(peer_uid=up, limit=1)
    m = h.messages[0] if h and h.messages else None
    check("a pre-cap 2.1 MB message comes cut to 64 KiB, marked",
          m is not None and m.id == huge and utf8_len(m.content) <= 64 * 1024 and m.content.endswith("…")
          and m.content.startswith("€€€") and a.sizes.get(pb.Packet.DM_HISTORY_RES, 0) <= CLIENT_FRAME,
          m and utf8_len(m.content))
    r = a.dm(recipient_uid=up, content="about that", nonce="a1-reply", reply_to=huge)
    check("a reply quotes at most 512 bytes of it", r is not None and r.reply_to == huge
          and 0 < utf8_len(r.reply_to_content) <= 512 and r.reply_to_content.endswith("…"),
          r and utf8_len(r.reply_to_content))
    h = a.history(peer_uid=up, limit=1)
    m = h.messages[0] if h and h.messages else None
    check("... in history too", m is not None and m.reply_to == huge and utf8_len(m.reply_to_content) <= 512
          and m.reply_to_content.endswith("…"))
    a.close()
    drop_users([up])


def test_dm_conversation_pages():
    print("DM conversation pages (review A1)")
    peers = seed_users("convpeer", 450)
    names = dict(sql("SELECT uid, username FROM users WHERE uid = ANY(%s)", peers))
    register("convme")
    me = uid_of("convme")
    sql("INSERT INTO dm_messages (sender_uid, sender, recipient_uid, recipient, content, sent_at) "
        "SELECT uid, username, %s::bigint, 'convme', 'hello from ' || username, 1700000000 FROM users "
        "WHERE uid = ANY(%s) ORDER BY uid", me, peers)
    # The newest 40 conversations end in a 60 KiB envelope (it can't be cut),
    # the newest two in a 1.2 MB plaintext message stored before the cap.
    for u in peers[2:42]:
        seed_dms(u, names[u], me, "convme", "'[encrypted]'", envelope=b"\x03" + b"e" * 61440)
    for u in peers[:2]:
        seed_dms(u, names[u], me, "convme", "repeat('x', 1258291)")
    c = User("convme")
    seen, sizes, pages, before, last = {}, [], [], 0, None
    while len(pages) < 10:
        c.send(pb.Packet.DM_CONVERSATIONS_REQ, dm_conversations_req=pb.DmConversationsReq(before_id=before))
        r = c.wait(pb.Packet.DM_CONVERSATIONS_RES, timeout=5)
        if r is None:
            break
        last = r.dm_conversations_res
        sizes.append(c.sizes.get(pb.Packet.DM_CONVERSATIONS_RES, 0))
        pages.append((len(last.conversations), last.has_more, last.before_id == before))
        for conv in last.conversations:
            seen.setdefault(conv.peer_uid, []).append(conv)
        if not last.has_more or not last.conversations:
            break
        before = min(conv.last_message_id for conv in last.conversations)
    check("every conversation page stays under the client's frame cap", sizes and max(sizes) <= CLIENT_FRAME, sizes)
    check("pages hold at most 200 and echo their cursor", pages and all(n <= 200 and echo for n, _, echo in pages), pages)
    check("the envelope-heavy page is cut by bytes and says more follow", pages and pages[0][0] < 200 and pages[0][1],
          pages)
    check("paging on lists every conversation once, then stops", set(seen) == set(peers)
          and all(len(v) == 1 for v in seen.values()) and last is not None and not last.has_more, (len(seen), pages))
    big = seen.get(peers[0], [None])[0]
    check("a pre-cap 1.2 MB last message previews as 512 bytes, marked",
          big is not None and utf8_len(big.last_message_content) <= 512 and big.last_message_content.endswith("…")
          and big.unread_count == 2, big and (utf8_len(big.last_message_content), big.unread_count))
    sealed = seen.get(peers[2], [None])[0]
    check("an encrypted last message keeps its whole envelope",
          sealed is not None and len(sealed.last_message_envelope) == 61441)
    plain = seen.get(peers[-1], [None])[0]
    check("an untouched conversation: its preview and one unread",
          plain is not None and plain.last_message_content == f"hello from {names[peers[-1]]}" and plain.unread_count == 1)
    c.close()
    drop_users(peers)


def test_friend_list_bounds():
    print("friend list bounds (review A1)")
    peers = seed_users("frpeer", 5200)
    register("frme")
    me, ua = uid_of("frme"), uid_of("alice")
    # 5200 strangers' requests waiting (more than one list holds), and alice
    # as an accepted friend.
    sql("INSERT INTO friendships (uid1, uid2, status, action_uid) "
        "SELECT LEAST(uid, %s::bigint), GREATEST(uid, %s::bigint), 'PENDING', uid FROM users WHERE uid = ANY(%s)",
        me, me, peers)
    sql("INSERT INTO friendships (uid1, uid2, status, action_uid) VALUES (%s, %s, 'ACCEPTED', %s)",
        min(me, ua), max(me, ua), ua)
    v = User("frme")
    fl = v.friends()
    size = v.sizes.get(pb.Packet.FRIEND_LIST_RES, 0)
    check("a friend list holds at most 5000 rows", fl is not None and len(fl) <= 5000 and size <= CLIENT_FRAME,
          fl and (len(fl), size))
    check("... accepted friends first", fl is not None and "alice" in fl
          and fl["alice"].status in (pb.FriendInfo.ONLINE, pb.FriendInfo.OFFLINE))
    c = User("carol")
    r = c.friend(pb.ADD, "frme")
    check("a request past 1000 waiting is refused", r is not None and not r.success
          and "can't take more friend requests" in r.message, r and r.message)
    check("... and stores nothing", not sql("SELECT 1 FROM friendships WHERE uid1 = %s AND uid2 = %s",
                                             min(me, uid_of("carol")), max(me, uid_of("carol"))))
    v.close(); c.close()
    drop_users(peers)
    sql("DELETE FROM friendships WHERE uid1 = %s OR uid2 = %s", me, me)


def test_community_strings():
    print("community names, descriptions, memberships and pictures (review A1)")
    # The heartbeat clamps what a community reports, as the community does.
    c = community_send(pb.Packet.SERVER_HEARTBEAT, server_heartbeat=pb.ServerHeartbeat(
        name="N" * 100, description="é" * 100000, host_ip="10.0.9.1", port=9100, member_count=1,
        public_listing=True))
    r = c.wait(pb.Packet.SERVER_HEARTBEAT_RES)
    c.close()
    sid = r.server_heartbeat_res.server_id if r else 0
    row = sql("SELECT octet_length(name), octet_length(description) FROM community_servers WHERE id = %s", sid)
    check("a heartbeat's long name and description are clamped, not refused",
          sid > 0 and row and row[0][0] == 64 and 0 < row[0][1] <= 512, row)
    # A description stored before the clamp reaches nobody whole.
    sql("UPDATE community_servers SET description = repeat('d', 1000000) WHERE id = %s", sid)
    ua = uid_of("alice")
    sql("INSERT INTO community_memberships (uid, server_id, joined_at) VALUES (%s, %s, 1)", ua, sid)
    a = User("alice")
    mine = [m for m in a.login.memberships if m.id == sid] if a.ok else []
    check("... not in a login's memberships", len(mine) == 1 and len(mine[0].description) <= 512
          and a.sizes.get(pb.Packet.LOGIN_RES, 0) <= CLIENT_FRAME)
    a.send(pb.Packet.SERVER_LIST_REQ, server_list_req=pb.ServerListRequest())
    r = a.wait(pb.Packet.SERVER_LIST_RES)
    listed = [s for s in r.server_list_res.servers if s.id == sid] if r else []
    check("... nor in the directory", len(listed) == 1 and len(listed[0].description) == 512)
    c = community_send(pb.Packet.INVITE_REGISTER_REQ, invite_register_req=pb.InviteRegisterRequest(
        code="bigdesc1", host="10.0.9.1", port=9100))
    time.sleep(0.3); c.close()
    a.send(pb.Packet.INVITE_RESOLVE_REQ, invite_resolve_req=pb.InviteResolveRequest(code="bigdesc1"))
    r = a.wait(pb.Packet.INVITE_RESOLVE_RES)
    check("... nor in an invite preview", r is not None and r.invite_resolve_res.success
          and r.invite_resolve_res.server_id == sid and len(r.invite_resolve_res.server_description) == 512)
    a.close()
    # A login lists at most 200 memberships, the oldest.
    sql("INSERT INTO community_servers (name, description, host_ip, port, member_count) "
        "SELECT 'mcap' || g, 'd', '10.1.0.' || g, 9000 + g, 1 FROM generate_series(1, 250) g")
    sql("INSERT INTO community_memberships (uid, server_id, joined_at) "
        "SELECT %s::bigint, id, 2000000000 + id FROM community_servers WHERE name LIKE 'mcap%%'", ua)
    want = [r[0] for r in sql("SELECT server_id FROM community_memberships cm JOIN community_servers cs "
                              "ON cs.id = cm.server_id WHERE cm.uid = %s ORDER BY cm.joined_at, cm.server_id LIMIT 200",
                              ua)]
    a = User("alice")
    got = [m.id for m in a.login.memberships] if a.ok else []
    check("a login lists at most 200 memberships, the oldest", got == want and len(got) == 200, len(got))
    a.close()
    sql("DELETE FROM community_memberships WHERE server_id IN (SELECT id FROM community_servers WHERE name LIKE 'mcap%%') "
        "OR server_id = %s", sid)
    sql("DELETE FROM community_servers WHERE name LIKE 'mcap%%'")
    # Server pictures: over the community's 1 MiB cap is dropped, and one
    # stored before central checked is served as none.
    small = b"\xff\xd8" + b"p" * 1000
    c = community_send(pb.Packet.SYNC_SERVER_PICTURE_REQ, sync_server_picture_req=pb.SyncServerPictureReq(
        host="10.0.9.1", port=9100, data=small, version="a" * 64, server_id=sid))
    time.sleep(0.3); c.close()
    c = community_send(pb.Packet.SYNC_SERVER_PICTURE_REQ, sync_server_picture_req=pb.SyncServerPictureReq(
        host="10.0.9.1", port=9100, data=b"\xff\xd8" + b"q" * (1200 * 1024), version="b" * 64, server_id=sid))
    time.sleep(0.3); c.close()
    a = User("alice")
    a.send(pb.Packet.FETCH_SERVER_PICTURE_REQ, fetch_server_picture_req=pb.FetchServerPictureReq(server_id=sid))
    r = a.wait(pb.Packet.FETCH_SERVER_PICTURE_RES)
    check("a 1.2 MB server picture is refused; the last one stays",
          r is not None and r.fetch_server_picture_res.data == small and r.fetch_server_picture_res.version == "a" * 64)
    sql("UPDATE community_servers SET picture = %s, picture_version = %s WHERE id = %s",
        b"\xff\xd8" + b"r" * (1536 * 1024), "c" * 64, sid)
    a.send(pb.Packet.FETCH_SERVER_PICTURE_REQ, fetch_server_picture_req=pb.FetchServerPictureReq(server_id=sid))
    r = a.wait(pb.Packet.FETCH_SERVER_PICTURE_RES)
    check("... and a stored 1.5 MB one is served as none",
          r is not None and r.fetch_server_picture_res.data == b"" and r.fetch_server_picture_res.version == "")
    a.close()
    sql("DELETE FROM community_invites WHERE code = 'bigdesc1'")
    sql("DELETE FROM community_servers WHERE id = %s", sid)


def test_presence_parts():
    print("presence answers in parts (review A1)")
    import resource
    from concurrent.futures import ThreadPoolExecutor
    from cryptography.hazmat.primitives import serialization
    soft, hard = resource.getrlimit(resource.RLIMIT_NOFILE)
    if soft != resource.RLIM_INFINITY and soft < 8192:
        resource.setrlimit(resource.RLIMIT_NOFILE, (min(8192, hard) if hard != resource.RLIM_INFINITY else 8192, hard))
    n = 3000
    # Presence entries as large as they get: a 32-character name and a
    # 128-character status of 4-byte characters, a 32-byte emoji, an avatar.
    # 3000 of them come to ~2.2 MB.
    sql("INSERT INTO users (username, email, password_hash, status_text, status_emoji, avatar_version) "
        "SELECT %s::text || lpad(g::text, 6, '0'), 'presence-part-' || g || '@example.com', 'x', "
        "  %s::text, %s::text, %s::text "
        "FROM generate_series(1, %s) g",
        "𝐀" * 26, "𝐀" * 128, "😀" * 8, "f" * 64, n)
    uids = [r[0] for r in sql("SELECT uid FROM users WHERE email LIKE 'presence-part-%%' ORDER BY uid")]
    with open(os.path.join(RUN, "jwt_ed25519.pem"), "rb") as f:
        key = serialization.load_pem_private_key(f.read(), password=None)
    head = b64url(json.dumps({"alg": "EdDSA", "typ": "JWS"}).encode())

    def resume(uid):
        now = int(time.time())
        body = b64url(json.dumps({"iss": "decibell_central_auth", "aud": "decibell_central_resume",
                                  "uid": uid, "iat": now, "exp": now + 3600}).encode())
        signing_input = f"{head}.{body}"
        return f"{signing_input}.{b64url(key.sign(signing_input.encode()))}"

    jobs = [(u, resume(u), next_source()) for u in uids]
    with ThreadPoolExecutor(32) as ex:
        sessions = list(ex.map(lambda j: User("", resume=j[1], src=j[2]), jobs))
    signed_in = sum(s.ok for s in sessions)
    check(f"{n} accounts signed in", signed_in == n, signed_in)
    w = User("alice")
    w.clear()
    w.send(pb.Packet.PRESENCE_SUBSCRIBE, presence_subscribe=pb.PresenceSubscribe(uids=uids))
    parts, covered, users, consistent = [], set(), 0, True
    end = time.time() + 60
    while len(covered) < n and time.time() < end:
        p = w.wait(pb.Packet.PRESENCE_UPDATE, timeout=max(0.1, end - time.time()))
        if p is None:
            break
        parts.append(w.sizes.get(pb.Packet.PRESENCE_UPDATE, 0))
        u = p.presence_update
        part = set(u.covered_uids)
        covered |= part
        users += len(u.users)
        consistent = consistent and all(x.uid in part for x in u.users)
    check(f"{n} big presence entries come in parts ({len(parts)}), each under the client's frame cap",
          len(parts) >= 2 and max(parts) <= CLIENT_FRAME, parts)
    check("... which together cover every account, each part whole", covered == set(uids) and users == n
          and consistent, (len(covered), users, consistent))
    w.close()
    for s in sessions:
        s.close()
    time.sleep(0.5)
    drop_users(uids)


def test_abuse_limits():
    print("abuse limits")
    # Failed sign-ins per account and address: 10 from one address hold
    # that address only (owner decision: a griefer locks out only
    # themselves), even with the right password.
    register("lockme")
    ok = User("lockme")
    rt = ok.login.resume_token
    ok.close()
    griefer = "127.0.250.4"
    for _ in range(10):
        User("lockme", password="wrong-password", src=griefer).close()
    t0 = time.time()
    blocked = User("lockme", src=griefer)
    took = time.time() - t0
    check("10 failed sign-ins hold that address for the account", not blocked.ok and blocked.login
          and "failed sign-ins" in blocked.login.message, blocked.login and blocked.login.message)
    check("... without a password check", took < 0.2, f"{took:.2f}s")
    blocked.close()
    owner = User("lockme")
    check("... while the owner signs in from elsewhere", owner.ok)
    owner.close()
    again = User("lockme", src=griefer)
    check("... which doesn't release the griefer's address", not again.ok)
    again.close()
    # The account-wide cap: 100 failed sign-ins from many addresses hold
    # the account everywhere; its resume token still signs in.
    import threading
    register("capme")
    ok = User("capme")
    cap_rt = ok.login.resume_token
    ok.close()

    def guess(i):
        User("capme", password=f"guess{i}").close()

    threads = [threading.Thread(target=guess, args=(i,)) for i in range(110)]
    for t in threads:
        t.start()
    for t in threads:
        t.join()
    held = User("capme")
    check("100 failed sign-ins from many addresses hold the account", not held.ok and held.login
          and "failed sign-ins" in held.login.message, held.login and held.login.message)
    held.close()
    r = User("", resume=cap_rt)
    check("... but its resume token still signs in", r.ok)
    r.close()
    # Password sign-ins per address: 30, then refused without hashing. All
    # at once, so the allowance can't refill meanwhile (it would under a
    # sanitizer, where each check is slow).
    src = "127.0.250.1"
    conns = [Conn(src=src) for _ in range(40)]
    results = []

    def attempt(i):
        t0 = time.time()
        conns[i].send(pb.Packet.LOGIN_REQ, login_req=pb.LoginRequest(username=f"nosuch{i}", password="x"))
        r = conns[i].wait(pb.Packet.LOGIN_RES, timeout=60)
        results.append((r.login_res.message if r else "", time.time() - t0))

    threads = [threading.Thread(target=attempt, args=(i,)) for i in range(40)]
    for t in threads:
        t.start()
    for t in threads:
        t.join()
    checked = [d for m, d in results if m == "Invalid username or password."]
    refused = [d for m, d in results if "from your network" in m]
    check("password sign-ins from one address stop at 30", 30 <= len(checked) <= 31 and len(checked) + len(refused) == 40,
          f"{len(checked)} checked, {len(refused)} refused")
    check("... refused without a password check", refused and min(refused) < min(checked),
          f"refused in {min(refused or [0]):.2f}s, checked in {min(checked or [0]):.2f}s+")
    for c in conns:
        c.close()
    check("another address still signs in", User("alice").ok)
    # Sign-ups per address: 10, then refused.
    src = "127.0.250.2"
    outcomes = []

    def signup(i):
        res = register(f"signup{i}", src=src)
        outcomes.append(bool(res and res.success))

    threads = [threading.Thread(target=signup, args=(i,)) for i in range(10)]
    for t in threads:
        t.start()
    for t in threads:
        t.join()
    check("10 sign-ups from one address", outcomes.count(True) == 10, outcomes)
    r = register("signup10", src=src)
    check("the 11th is refused", r and not r.success and "sign-ups" in r.message, r and r.message)
    # New connections per address: 60 at once, then 10 a second.
    socks = [socket.create_connection((HOST, PORT), timeout=2, source_address=("127.0.250.3", 0))
             for _ in range(75)]
    time.sleep(0.5)
    refused = len(socks) - open_count(socks)
    check("connecting too fast from one address is cut off", 10 <= refused <= 16, f"{refused} of 75 refused")
    for s in socks:
        s.close()


def raw_connections(n, per_source=10, base=200):
    return [socket.create_connection((HOST, PORT), timeout=2,
                                     source_address=(f"127.0.{base + i // per_source}.1", 0))
            for i in range(n)]


def open_count(socks):
    alive = 0
    for s in socks:
        s.settimeout(0.05)
        try:
            if s.recv(1) != b"":
                alive += 1
        except (socket.timeout, TimeoutError):
            alive += 1
        except OSError:
            pass
    return alive


def cpu_seconds(pid):
    with open(f"/proc/{pid}/stat") as f:
        fields = f.read().rsplit(")", 1)[1].split()
    return (int(fields[11]) + int(fields[12])) / os.sysconf("SC_CLK_TCK")


def test_connection_limits():
    """Own server instances: the connection cap, and running out of file
    descriptors (accept used to retry in a busy loop)."""
    print("connection limits")
    proc = start_server(log_name="central_cap.log", extra_env={"DECIBELL_MAX_CONNECTIONS": "40"})
    try:
        socks = raw_connections(40)
        time.sleep(0.3)
        extra = raw_connections(5, base=230)
        time.sleep(0.5)
        check("the server-wide cap holds", open_count(socks) == 40 and open_count(extra) == 0,
              f"{open_count(socks)} + {open_count(extra)}")
        for s in socks[:10] + extra:
            s.close()
        time.sleep(0.3)
        check("a freed slot takes a new connection", User("alice").ok)
        for s in socks[10:]:
            s.close()
    finally:
        stop_server(proc)
    proc = start_server(log_name="central_fd.log", nofile=128,
                        extra_env={"DECIBELL_MAX_CONNECTIONS": "1000", "DECIBELL_IO_THREADS": "2",
                                   "DECIBELL_CPU_THREADS": "1"})
    try:
        socks = raw_connections(160)
        time.sleep(0.5)
        before = cpu_seconds(proc.pid)
        time.sleep(2.0)
        used = cpu_seconds(proc.pid) - before
        check("out of descriptors, accept doesn't spin", used < 0.3, f"{used:.2f}s of CPU in 2 s")
        for s in socks:
            s.close()
        time.sleep(0.5)
        check("... and it recovers once some close", User("alice").ok)
        with open(os.path.join(RUN, "central_fd.log")) as f:
            check("... and says so in the log", "accept failed" in f.read())
    finally:
        stop_server(proc)


def seed_with_old_binary():
    """Data shapes as the pre-uid central writes them."""
    for n in ("alice", "bob", "carol", "dave"):
        register(n)
    a, b, c = User("alice"), User("bob"), User("carol")
    a.friend(pb.ADD, "bob"); b.friend(pb.ACCEPT, "alice", uid_of("alice"))
    c.friend(pb.BLOCK, "alice", uid_of("alice"))
    b.friend(pb.ADD, "carol")
    m1 = a.dm("bob", "plain from alice", nonce="s1")
    m2 = b.dm("alice", "[encrypted]", nonce="s2", envelope=b"\x01sealed-by-bob", reply_to=m1.id)
    a.dm("bob", "unread one", nonce="s3")
    b.send(pb.Packet.DM_MARK_READ_REQ, dm_mark_read_req=pb.DmMarkReadReq(peer="alice", peer_uid=uid_of("alice"), up_to_id=m1.id))
    a.send(pb.Packet.E2EE_PUBLISH_KEYS_REQ, e2ee_publish_keys_req=pb.E2eePublishKeysReq(bundle=bundle(1), backup=b"alice-wrapped"))
    a.wait(pb.Packet.E2EE_PUBLISH_KEYS_RES)
    a.send(pb.Packet.E2EE_PUBLISH_KEYS_REQ, e2ee_publish_keys_req=pb.E2eePublishKeysReq(bundle=bundle(4)))
    a.wait(pb.Packet.E2EE_PUBLISH_KEYS_RES)
    b.send(pb.Packet.E2EE_PUBLISH_KEYS_REQ, e2ee_publish_keys_req=pb.E2eePublishKeysReq(bundle=bundle(7), backup=b"bob-wrapped"))
    b.wait(pb.Packet.E2EE_PUBLISH_KEYS_RES)
    time.sleep(0.3)
    for x in (a, b, c):
        x.close()
    sid = heartbeat(name="Migrated", port=9100)
    for n in ("alice", "bob"):
        k = community_send(pb.Packet.MEMBERSHIP_REGISTER_REQ,
                           membership_register_req=pb.MembershipRegisterReq(username=n, server_id=sid))
        time.sleep(0.2); k.close()
    # A message left for a name nobody holds, as pre-2081fb0 central stored them.
    sql("INSERT INTO dm_messages (sender, recipient, content, sent_at) VALUES ('alice', 'ghost', 'waiting', 1)")
    return sid, m1.id, m2.id


def test_migration():
    print("migration from the pre-uid schema")
    old = start_server(OLD_SERVER_BIN, log_name="central-old.log")
    try:
        sid, m1, m2 = seed_with_old_binary()
    finally:
        stop_server(old)
    log_path = os.path.join(RUN, "central.log")
    before = os.path.getsize(log_path) if os.path.exists(log_path) else 0
    new = start_server()
    try:
        time.sleep(0.3)
        with open(log_path, "rb") as f:
            f.seek(before)
            log = f.read().decode(errors="replace")
        line = next((l for l in log.splitlines() if "Migration 1" in l), "")
        check("migration ran and logged its counts", "friendships 3/3" in line and "dm_messages 3/4" in line
              and "(1 for unknown names removed)" in line and "e2ee keys 3/3" in line
              and "e2ee backups 2/2" in line and "memberships 2/2" in line and "read cursors 1/1" in line, line)
        check("schema version 5", sql("SELECT version FROM central_schema")[0][0] == 5)
        line5 = next((l for l in log.splitlines() if "Migration 5" in l), "")
        check("migration 5 filled the conversation table", "dm_conversations filled: 1" in line5, line5)
        check("no account starts with a session cutoff",
              sql("SELECT DISTINCT sessions_valid_after FROM users") == [(0,)])
        check("migrated bundles bind the name", sql("SELECT DISTINCT binding FROM e2ee_keys") == [(1,)])
        check("users keyed by uid", sql(
            "SELECT a.attname FROM pg_index i JOIN pg_attribute a ON a.attrelid = i.indrelid "
            "AND a.attnum = ANY(i.indkey) WHERE i.indrelid = 'users'::regclass AND i.indisprimary") == [("uid",)])
        check("legacy tables kept", len(sql("SELECT 1 FROM pg_tables WHERE tablename LIKE 'legacy_%%'")) == 5)
        check("ghost message removed", sql("SELECT count(*) FROM dm_messages WHERE content = 'waiting'")[0][0] == 0)
        a, b, c = User("alice"), User("bob"), User("carol")
        check("memberships survive", [m.id for m in a.login.memberships] == [sid]
              and [m.id for m in b.login.memberships] == [sid])
        fa, fb, fc = a.friends(), b.friends(), c.friends()
        check("friendship survives", fa.get("bob") and fa["bob"].status == pb.FriendInfo.ONLINE)
        check("block survives", fc.get("alice") and fc["alice"].status == pb.FriendInfo.BLOCKED and "carol" not in fa)
        check("pending request survives", fb.get("carol") and fb["carol"].status == pb.FriendInfo.PENDING_OUTGOING
              and fc.get("bob") and fc["bob"].status == pb.FriendInfo.PENDING_INCOMING)
        h = a.history("bob", uid_of("bob"))
        ids = [x.id for x in h.messages] if h else []
        check("history survives with its ids", h and len(h.messages) == 3 and m1 in ids and m2 in ids)
        row = next((x for x in h.messages if x.id == m2), None) if h else None
        check("envelope + reply survive", row and row.envelope == b"\x01sealed-by-bob" and row.reply_to == m1
              and row.reply_to_sender == "alice" and row.sealed_sender == "bob" and row.sealed_recipient == "alice")
        convs = b.conversations()
        check("read cursor survives", convs and convs["alice"].unread_count == 1, convs and convs["alice"].unread_count)
        r = c.dm("alice", "still blocked?", nonce="m1")
        check("block still applies to DMs", r and r.id == 0)
        b.send(pb.Packet.E2EE_FETCH_KEYS_REQ, e2ee_fetch_keys_req=pb.E2eeFetchKeysReq(username="alice"))
        f = b.wait(pb.Packet.E2EE_FETCH_KEYS_RES)
        check("current key survives", f and f.e2ee_fetch_keys_res.bundle.key_id == 2
              and f.e2ee_fetch_keys_res.bundle.dh_pub == bytes([4]) * 32 and f.e2ee_fetch_keys_res.bundle.signed_name == "alice")
        a.send(pb.Packet.E2EE_PUBLISH_KEYS_REQ, e2ee_publish_keys_req=pb.E2eePublishKeysReq(bundle=bundle(9)))
        r = a.wait(pb.Packet.E2EE_PUBLISH_KEYS_RES)
        check("key ids continue", r and r.e2ee_publish_keys_res.key_id == 3)
        a.send(pb.Packet.E2EE_FETCH_BACKUP_REQ, e2ee_fetch_backup_req=pb.E2eeFetchBackupReq())
        f = a.wait(pb.Packet.E2EE_FETCH_BACKUP_RES)
        check("backup survives", f and f.e2ee_fetch_backup_res.backup == b"alice-wrapped")
        m = a.dm("bob", "after the migration", nonce="m2")
        check("new DMs work", m and m.id > max(ids))
        for x in (a, b, c):
            x.close()
    finally:
        stop_server(new)
    # A second start must not migrate again or recreate the old tables.
    new = start_server()
    try:
        a = User("alice")
        check("restart: still works", a.ok and a.friends().get("bob") is not None)
        a.close()
        check("restart: no old tables recreated", sql("SELECT count(*) FROM pg_tables WHERE tablename IN "
              "('friends', 'dm_read_state', 'user_e2ee_keys', 'user_e2ee_backup', 'user_communities')")[0][0] == 0)
    finally:
        stop_server(new)


def main():
    global PG
    os.makedirs(RUN, exist_ok=True)
    PG = pgserver.get_server(os.path.join(RUN, "pg"), cleanup_mode="stop")
    if OLD_SERVER_BIN:
        fresh_db()
        test_migration()
    else:
        print("(DECIBELL_E2E_SERVER_OLD unset: migration check skipped)")
    fresh_db()
    proc = start_server()
    try:
        test_register_login()
        test_friends()
        test_dms()
        test_dm_conversation_rows()
        test_dm_gates()
        test_blocks()
        test_presence_and_avatars()
        test_watch_cleanup()
        test_login_storm()
        test_calls()
        test_e2ee_keys()
        test_e2ee_binding()
        test_memberships()
        test_uid_wire()
        test_memberships_by_uid()
        test_friends_in_community()
        test_rename_keeps_everything()
        test_rename()
        test_token_refresh()
        test_resume()
        test_dm_pacing()
        test_dm_page_budget()
        test_dm_conversation_pages()
        test_friend_list_bounds()
        test_community_strings()
        test_presence_parts()
        test_abuse_limits()
    finally:
        stop_server(proc)
    test_connection_limits()
    print(f"\n{len(PASS)} passed, {len(FAIL)} failed")
    if FAIL:
        print("failed:", ", ".join(FAIL))
    sys.exit(1 if FAIL else 0)


if __name__ == "__main__":
    main()
