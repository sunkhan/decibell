"""Shared pieces of the local test stack (tools/dev/): where things live, the
fixed addresses, and small wire clients for central, a community and the
attachment HTTPS API. Imported by stack.py, seed.py and smoke.py, which run
with the harness venv's python in isolated mode (-I).
"""
import hashlib
import http.client
import json
import os
import socket
import ssl
import struct
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.environ.get("DECIBELL_DEV_REPO") or os.path.dirname(os.path.dirname(HERE))
DEV = os.environ.get("DECIBELL_DEV_DIR") or os.path.join(os.path.expanduser("~"), ".cache", "decibell-dev")
DEPS_CENTRAL = os.path.join(DEV, "deps", "central")
DEPS_COMMUNITY = os.path.join(DEV, "deps", "community")
BIN = os.path.join(DEV, "bin")
RUN = os.path.join(DEV, "run")
KEYS = os.path.join(RUN, "keys")          # survives `reset` (clients pin these certs)
PGBIN = os.path.join(DEPS_CENTRAL, "pginstall", "bin")
PGDATA = os.path.join(RUN, "pg", "data")
PGSOCK = os.path.join(RUN, "pg")          # unix socket only, no TCP port
PGPORT = 54329                            # names the socket file; nothing listens on TCP
PGDB = "decibell"
STATE = os.path.join(RUN, "stack.json")
ACCOUNTS_JSON = os.path.join(RUN, "accounts.json")

sys.path.insert(0, os.path.join(DEPS_CENTRAL, "pb"))
import messages_pb2 as pb  # noqa: E402

CENTRAL_HOST = "127.0.0.1"
CENTRAL_PORT = int(os.environ.get("DECIBELL_DEV_CENTRAL_PORT", "18080"))
# Community ports are hard-coded in src/community/main.cpp (8082 TCP, 8083 voice UDP, 8084 media UDP,
# 8085 attachment HTTPS) and advertised as such, so each community gets its own loopback address instead.
COMMUNITY_PORT = 8082
COMMUNITIES = [
    {"slug": "gaming", "name": "Daily Gaming", "owner": "alice", "ip": "127.0.0.11",
     "desc": "Games, clips and late-night voice. The busy one (local test stack)."},
    {"slug": "rust", "name": "Rustaceans", "owner": "bob", "ip": "127.0.0.12",
     "desc": "Borrow checker support group (local test stack)."},
    {"slug": "design", "name": "Design Lab", "owner": "carol", "ip": "127.0.0.13",
     "desc": "Critique, type nerdery and colour arguments (local test stack)."},
]
BY_SLUG = {c["slug"]: c for c in COMMUNITIES}
BY_NAME = {c["name"]: c for c in COMMUNITIES}

PASSWORD = "decibell-test-1"
ACCOUNTS = ["alice", "bob", "carol", "dave", "erin", "perf1", "perf2", "perf3", "perf4"]
# Accounts added after the stack was first seeded (`local-stack.sh top-up`, and the end of `seed`): each
# (name, the community it joins, its role there, whether it gets E2EE keys). perf5: a second streaming
# account beside perf4 for measurements with two streams (tools/perf/scenarios.sh … watch-2).
EXTRA_ACCOUNTS = [("perf5", "gaming", "Member", True)]
# The passphrase of every stack account that has E2EE keys (perf3, perf4, perf5): what the GPUI client's
# voice self-tests and tools/perf/streambot.sh use.
E2EE_PASSPHRASE = "decibell-m3-test-passphrase"


def community_dir(slug):
    return os.path.join(RUN, "communities", slug)


def community_db(slug):
    return os.path.join(community_dir(slug), "community.db")


def read_state():
    try:
        with open(STATE) as f:
            return json.load(f)
    except (OSError, ValueError):
        return {}


def write_json(path, obj):
    tmp = path + ".tmp"
    with open(tmp, "w") as f:
        json.dump(obj, f, indent=2)
        f.write("\n")
    os.replace(tmp, path)


def cert_fingerprint_pem(path):
    """sha256-hex of the certificate's DER, as the servers report it."""
    with open(path) as f:
        return hashlib.sha256(ssl.PEM_cert_to_DER_cert(f.read())).hexdigest()


def pg_conninfo(db=PGDB):
    return f"host={PGSOCK} port={PGPORT} dbname={db} user=postgres"


def pg(q, *args, db=PGDB):
    import psycopg
    with psycopg.connect(pg_conninfo(db), autocommit=True) as c:
        cur = c.execute(q, args)
        return cur.fetchall() if cur.description else []


# --- Source addresses ------------------------------------------------------------------------
# Central limits sign-ins / sign-ups / connections per address. Tools bind a loopback source
# from 127.0.200.0/23 (the e2e suites use 127.0.1-190.x and 127.0.250.x).

_src = [int.from_bytes(os.urandom(2), "big") % 400]


def next_source():
    _src[0] += 1
    n = _src[0] % 500
    return f"127.0.{200 + n // 250}.{1 + n % 250}"


# --- Wire ---------------------------------------------------------------------------------------

class PinMismatch(Exception):
    pass


def tls_connect(host, port, pin=None, timeout=5.0, src=None):
    ctx = ssl.create_default_context()
    ctx.check_hostname = False
    ctx.verify_mode = ssl.CERT_NONE  # self-signed: pinned by fingerprint below, as the clients do
    raw = socket.create_connection((host, port), timeout=timeout, source_address=(src or next_source(), 0))
    s = ctx.wrap_socket(raw)
    seen = hashlib.sha256(s.getpeercert(binary_form=True)).hexdigest()
    if pin and seen != pin:
        s.close()
        raise PinMismatch(f"{host}:{port} presented {seen}, pinned {pin}")
    return s, seen


class Conn:
    """One framed-protobuf TLS connection ([u32 BE length][Packet])."""

    def __init__(self, host, port, pin=None, timeout=5.0):
        self.s, self.fingerprint = tls_connect(host, port, pin, timeout)
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

    def wait(self, ptype, timeout=5.0, pred=lambda p: True):
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

    def flush(self, timeout=0.3):
        self.drain(timeout)
        self.inbox.clear()

    def close(self):
        try:
            self.s.close()
        except OSError:
            pass


def central_pin():
    st = read_state()
    return st.get("central", {}).get("fingerprint")


def register(username, password=PASSWORD, email=None):
    c = Conn(CENTRAL_HOST, CENTRAL_PORT, central_pin())
    try:
        c.send(pb.Packet.REGISTER_REQ, register_req=pb.RegisterRequest(
            username=username, password=password, email=email or f"{username}@example.test"))
        r = c.wait(pb.Packet.REGISTER_RES, timeout=15)
        return r.register_res if r else None
    finally:
        c.close()


class Central(Conn):
    """A signed-in central session (a password sign-in from a fixed tool device id per account)."""

    def __init__(self, username, password=PASSWORD, device=None, app="local-stack"):
        super().__init__(CENTRAL_HOST, CENTRAL_PORT, central_pin())
        self.username = username
        # One fixed tool device per account (seed and smoke share it), so device lists stay tidy.
        self.device = device or ("devstack-" + hashlib.sha256(f"tools:{username}".encode()).hexdigest()[:24])
        self.send(pb.Packet.LOGIN_REQ, login_req=pb.LoginRequest(
            username=username, password=password, device_id=self.device, device_name="Decibell dev stack",
            device_platform="linux", app_version=app))
        r = self.wait(pb.Packet.LOGIN_RES, timeout=20)
        self.login = r.login_res if r else None
        self.ok = bool(self.login and self.login.success)
        if self.ok:
            self.token = self.login.jwt_token
            self.uid = self.login.uid

    def friend(self, action, target_uid=0, target=""):
        self.send(pb.Packet.FRIEND_ACTION_REQ, friend_action_req=pb.FriendActionReq(
            action=action, target_uid=target_uid, target_username=target))
        r = self.wait(pb.Packet.FRIEND_ACTION_RES)
        return r.friend_action_res if r else None

    def friends(self):
        self.flush(0.2)
        self.send(pb.Packet.FRIEND_LIST_REQ, friend_list_req=pb.FriendListReq())
        r = self.wait(pb.Packet.FRIEND_LIST_RES)
        return {f.uid: f for f in r.friend_list_res.friends} if r else {}

    def dm(self, recipient_uid, content, nonce="", reply_to=0):
        nonce = nonce or os.urandom(8).hex()
        self.send(pb.Packet.DIRECT_MSG, direct_msg=pb.DirectMessage(
            recipient_uid=recipient_uid, content=content, nonce=nonce, reply_to=reply_to))
        r = self.wait(pb.Packet.DIRECT_MSG, pred=lambda p: p.direct_msg.nonce == nonce)
        return r.direct_msg if r else None

    def server_list(self):
        self.send(pb.Packet.SERVER_LIST_REQ, server_list_req=pb.ServerListRequest())
        r = self.wait(pb.Packet.SERVER_LIST_RES)
        return list(r.server_list_res.servers) if r else None


class Community(Conn):
    """A community session: TLS to host:8082 pinned to the fingerprint central reports, then
    COMMUNITY_AUTH_REQ with the session token (optionally an invite code to join)."""

    def __init__(self, host, token, pin=None, invite="", port=COMMUNITY_PORT):
        super().__init__(host, port, pin)
        self.host = host
        self.pin = pin
        self.token = token   # for the attachment API's Bearer header
        self.send(pb.Packet.COMMUNITY_AUTH_REQ, community_auth_req=pb.CommunityAuthRequest(
            jwt_token=token, invite_code=invite))
        r = self.wait(pb.Packet.COMMUNITY_AUTH_RES, timeout=10)
        self.auth = r.community_auth_res if r else None
        self.ok = bool(self.auth and self.auth.success)
        self.channels = list(self.auth.channels) if self.ok else []

    def send(self, ptype, token=None, **payload):
        # Authenticated once by COMMUNITY_AUTH_REQ; like the client, no per-packet token here.
        super().send(ptype, token="" if token is None else token, **payload)

    def refresh_channels(self, timeout=1.0):
        """The newest CHANNEL_LIST_UPDATE pushed so far (lists are pushed whole after a change)."""
        self.drain(timeout)
        ups = [p for p in self.inbox if p.type == pb.Packet.CHANNEL_LIST_UPDATE]
        if ups:
            self.channels = list(ups[-1].channel_list_update.channels)
            self.inbox = [p for p in self.inbox if p.type != pb.Packet.CHANNEL_LIST_UPDATE]
        return self.channels

    def channel(self, name, ctype=None):
        for ch in self.channels:
            if ch.name == name and (ctype is None or ch.type == ctype):
                return ch
        return None

    def history(self, channel_id, limit=50, **kw):
        self.send(pb.Packet.CHANNEL_HISTORY_REQ, channel_history_req=pb.ChannelHistoryRequest(
            channel_id=channel_id, limit=limit, **kw))
        r = self.wait(pb.Packet.CHANNEL_HISTORY_RES, timeout=10,
                      pred=lambda p: p.channel_history_res.channel_id == channel_id)
        return r.channel_history_res if r else None

    def attachment_http(self):
        port = self.auth.attachment_port or (COMMUNITY_PORT + 3)
        return AttachmentHttp(self.host, port, self.token, self.pin)


class AttachmentHttp:
    """The community's attachment HTTPS API (HTTP/1.1 keep-alive, Bearer session token), pinned to the
    community's certificate fingerprint like the chat port."""

    def __init__(self, host, port, token, pin=None):
        self.host, self.port, self.token, self.pin = host, port, token, pin
        self.conn = None

    def _connect(self):
        ctx = ssl.create_default_context()
        ctx.check_hostname = False
        ctx.verify_mode = ssl.CERT_NONE
        conn = http.client.HTTPSConnection(self.host, self.port, context=ctx, timeout=15,
                                           source_address=(next_source(), 0))
        conn.connect()
        seen = hashlib.sha256(conn.sock.getpeercert(binary_form=True)).hexdigest()
        if self.pin and seen != self.pin:
            conn.close()
            raise PinMismatch(f"attachments {self.host}:{self.port} presented {seen}, pinned {self.pin}")
        self.conn = conn

    def request(self, method, path, body=b"", headers=None):
        for attempt in range(2):
            if self.conn is None:
                self._connect()
            h = {"Authorization": f"Bearer {self.token}", "Content-Length": str(len(body))}
            h.update(headers or {})
            try:
                self.conn.request(method, path, body=body, headers=h)
                r = self.conn.getresponse()
                data = r.read()
                if (r.getheader("Connection") or "").lower() == "close":
                    self.conn.close()
                    self.conn = None
                return r.status, dict((k.lower(), v) for k, v in r.getheaders()), data
            except (http.client.HTTPException, OSError):
                if self.conn:
                    self.conn.close()
                self.conn = None
                if attempt:
                    raise
        raise RuntimeError("unreachable")

    def upload(self, channel_id, filename, mime, data, width=0, height=0, placeholder="", thumbs=None):
        """init -> PATCH -> complete -> thumbnails. Returns the attachment id."""
        meta = {"channelId": channel_id, "filename": filename, "mime": mime, "size": len(data)}
        if width and height:
            meta.update(width=width, height=height)
        if placeholder:
            meta["placeholder"] = placeholder
        st, _, body = self.request("POST", "/attachments/init", json.dumps(meta).encode(),
                                   {"Content-Type": "application/json"})
        if st != 201:
            raise RuntimeError(f"init {filename}: HTTP {st} {body[:80]!r}")
        aid = json.loads(body)["id"]
        off = 0
        while off < len(data):
            chunk = data[off:off + (1 << 20)]
            st, hd, body = self.request("PATCH", f"/attachments/{aid}", chunk,
                                        {"Upload-Offset": str(off), "Content-Type": "application/offset+octet-stream"})
            if st != 204:
                raise RuntimeError(f"patch {filename}@{off}: HTTP {st} {body[:80]!r}")
            off = int(hd.get("upload-offset", off + len(chunk)))
        st, _, body = self.request("POST", f"/attachments/{aid}/complete")
        if st not in (200, 204):
            raise RuntimeError(f"complete {filename}: HTTP {st} {body[:80]!r}")
        for size, jpeg in (thumbs or {}).items():
            st, _, body = self.request("POST", f"/attachments/{aid}/thumbnail?size={size}", jpeg,
                                       {"Content-Type": "image/jpeg"})
            if st != 204:
                raise RuntimeError(f"thumbnail {size} {filename}: HTTP {st} {body[:80]!r}")
        return aid

    def close(self):
        if self.conn:
            self.conn.close()
            self.conn = None
