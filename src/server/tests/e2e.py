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


def start_server(binary=None, log_name="central.log"):
    ensure_tls()
    env = dict(os.environ, DECIBELL_COMMUNITY_SECRET=SECRET, DECIBELL_DB_CONN=libpq_conninfo(),
               DECIBELL_JWT_KEY_FILE=os.path.join(RUN, "jwt_ed25519.pem"))
    log = open(os.path.join(RUN, log_name), "ab")
    proc = subprocess.Popen([binary or SERVER_BIN], cwd=RUN, env=env, stdout=log, stderr=subprocess.STDOUT)
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

class Conn:
    """One TLS connection to central. `token` goes into every packet."""

    def __init__(self, timeout=3.0):
        ctx = ssl.create_default_context()
        ctx.check_hostname = False
        ctx.verify_mode = ssl.CERT_NONE
        self.s = ctx.wrap_socket(socket.create_connection((HOST, PORT), timeout=timeout))
        self.buf = b""
        self.inbox = []
        self.closed = False
        self.token = ""

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


def register(username, password="password123", email=None):
    c = Conn()
    c.send(pb.Packet.REGISTER_REQ, register_req=pb.RegisterRequest(
        username=username, password=password, email=email or f"{username}@example.com"))
    r = c.wait(pb.Packet.REGISTER_RES)
    c.close()
    return r.register_res if r else None


class User(Conn):
    """A logged-in client session."""

    def __init__(self, username, password="password123"):
        super().__init__()
        self.username = username
        self.send(pb.Packet.LOGIN_REQ, login_req=pb.LoginRequest(username=username, password=password))
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
    m2 = b.dm("alice", "reply", nonce="n2", reply_to=m.id if m else 0)
    check("reply embeds parent", m2 and m2.reply_to == m.id and m2.reply_to_sender == "alice"
          and m2.reply_to_content == "hello bob")
    h = a.history("bob", uid_of("bob"))
    check("history newest first", h and [x.content for x in h.messages][:2] == ["reply", "hello bob"])
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
    a.close(); b.close()


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
    # The cap: the first 5000 accounts of a longer set.
    big = list(range(10_000_000, 10_006_000))
    ans = c.watch(*big)
    check("a watch set is cut at 5000", ans is not None and len(ans.covered_uids) == 5000
          and set(ans.covered_uids) == set(big[:5000]))
    c.close(); a.close()


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
    a.send(pb.Packet.CALL_SIGNAL, call_signal=pb.CallSignal(kind=pb.CallSignal.INVITE, call_id="c3", to="nosuchuser"))
    r = a.wait(pb.Packet.CALL_SIGNAL)
    check("unknown peer answers PEER_OFFLINE", r and r.call_signal.kind == pb.CallSignal.PEER_OFFLINE)
    a.close(); b.close()


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
        check("schema version 3", sql("SELECT version FROM central_schema")[0][0] == 3)
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
        test_dm_gates()
        test_presence_and_avatars()
        test_watch_cleanup()
        test_login_storm()
        test_calls()
        test_e2ee_keys()
        test_e2ee_binding()
        test_memberships()
        test_uid_wire()
        test_memberships_by_uid()
        test_rename_keeps_everything()
        test_rename()
        test_token_refresh()
    finally:
        stop_server(proc)
    print(f"\n{len(PASS)} passed, {len(FAIL)} failed")
    if FAIL:
        print("failed:", ", ".join(FAIL))
    sys.exit(1 if FAIL else 0)


if __name__ == "__main__":
    main()
