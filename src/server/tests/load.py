#!/usr/bin/env python3
"""Central load test (docs/superpowers/specs/2026-10-15-central-hardening-design.md).

Runs central and its PostgreSQL pinned to a CPU set sized like the production
box, seeds accounts straight into the database, and drives them from worker
processes over real TLS connections, each from its own loopback address range
(central limits connections per address):

  1. password sign-ins: bcrypt throughput;
  2. a ramp of connected clients (signed in with resume tokens, subscribed to
     50 others, pinging): at each step, central's memory and CPU, and DM and
     presence latency;
  3. traffic at full population: DMs, auto-away flips and re-subscriptions at
     rising rates, with delivery latency for every DM;
  4. a central restart with everyone connected: how long until all are back.

Clients sign in by resume token, minted here with central's key: the path a
real client takes after a restart. Usage, from src/server/tests after
setup_deps.sh and build.sh:

  DECIBELL_E2E_PB=$D/pb DECIBELL_E2E_SERVER=$D/central_server \\
  DECIBELL_E2E_RUN=$D/load "$D/pgvenv/bin/python" -I load.py \\
      [--steps 5000,10000,20000,40000] [--rates 250,500,1000,2000,4000] \\
      [--server-cpus 0,2,4,6,8,10] [--worker-cpus 12-19] [--workers 8]

The report goes to $DECIBELL_E2E_RUN/load-report.md (and .json).
"""
import argparse
import asyncio
import json
import multiprocessing as mp
import os
import random
import signal
import socket
import ssl
import statistics
import struct
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import e2e  # noqa: E402  (paths, PostgreSQL and central helpers)
from e2e import pb  # noqa: E402

import psutil  # noqa: E402
import pgserver  # noqa: E402
from cryptography.hazmat.primitives import serialization  # noqa: E402

HOST, PORT = e2e.HOST, e2e.PORT
WATCH = 50
BACKOFF = [1, 2, 4, 8, 16, 30]  # the native client's steps, jittered 50-150%
PASSWORD = "password123"


def cpu_list(spec):
    out = []
    for part in spec.split(","):
        if "-" in part:
            a, b = part.split("-")
            out.extend(range(int(a), int(b) + 1))
        elif part:
            out.append(int(part))
    return out


def pct(samples, q):
    if not samples:
        return None
    s = sorted(samples)
    return s[min(len(s) - 1, int(q * len(s)))]


def frame(ptype, token="", **payload):
    data = pb.Packet(type=ptype, auth_token=token, **payload).SerializeToString()
    return struct.pack(">I", len(data)) + data


def tls_context():
    ctx = ssl.create_default_context()
    ctx.check_hostname = False
    ctx.verify_mode = ssl.CERT_NONE
    return ctx


# --- Tokens ---------------------------------------------------------------------

def b64url(data):
    import base64
    return base64.urlsafe_b64encode(data).rstrip(b"=").decode()


class Minter:
    """Resume tokens signed with central's key, as central mints them."""

    def __init__(self, key_path):
        with open(key_path, "rb") as f:
            self.key = serialization.load_pem_private_key(f.read(), password=None)
        self.head = b64url(json.dumps({"alg": "EdDSA", "typ": "JWS"}).encode())

    def resume(self, uid):
        now = int(time.time())
        body = b64url(json.dumps({"iss": "decibell_central_auth", "aud": "decibell_central_resume",
                                  "uid": uid, "iat": now, "exp": now + 30 * 86400}).encode())
        signing_input = f"{self.head}.{body}"
        return f"{signing_input}.{b64url(self.key.sign(signing_input.encode()))}"


# --- Worker: a few thousand clients on one asyncio loop ----------------------

class Client:
    __slots__ = ("w", "uid", "src", "resume", "token", "writer", "up", "attempt", "watch", "task", "idle",
                 "relogin_at")

    def __init__(self, w, uid, src, resume):
        self.w, self.uid, self.src, self.resume = w, uid, src, resume
        self.token, self.writer, self.up, self.attempt, self.watch, self.task = "", None, False, 0, [], None
        self.idle = False
        self.relogin_at = None

    def send(self, data):
        if self.up and self.writer is not None and not self.writer.is_closing():
            self.writer.write(data)
            return True
        return False

    async def run(self, first_done):
        """Connect, resume, subscribe; on a drop, back off (jittered) and
        come back, like the native client. `first_done(ok)` reports the
        first attempt."""
        signalled = False

        def report(ok):
            nonlocal signalled
            if not signalled:
                signalled = True
                first_done(ok)

        while not self.w.stopping:
            try:
                reader, writer = await asyncio.open_connection(
                    HOST, PORT, ssl=self.w.ctx, local_addr=(self.src, 0))
            except (OSError, ssl.SSLError):
                self.w.connect_errors += 1
                report(False)
                await self.backoff()
                continue
            self.writer = writer
            writer.write(frame(pb.Packet.LOGIN_REQ, login_req=pb.LoginRequest(resume_token=self.resume)))
            try:
                ok = await self.read_until_login(reader)
                if not ok:
                    self.w.login_failures += 1
                    writer.close()
                    report(False)
                    await self.backoff()
                    continue
                self.up = True
                self.attempt = 0
                self.w.connected += 1
                now = time.time()
                self.w.last_up = now
                if self.w.restart_mark and self.relogin_at is None and now > self.w.restart_mark:
                    self.relogin_at = now
                report(True)
                writer.write(frame(pb.Packet.PRESENCE_SUBSCRIBE, self.token,
                                   presence_subscribe=pb.PresenceSubscribe(uids=self.watch)))
                await self.read_loop(reader)
            except (OSError, asyncio.IncompleteReadError, asyncio.TimeoutError, ssl.SSLError, ConnectionError):
                pass
            finally:
                if self.up:
                    self.up = False
                    self.w.connected -= 1
                    self.w.drops += 1
                    if self.w.first_down is None:
                        self.w.first_down = time.time()
                try:
                    writer.close()
                except Exception:
                    pass
            report(False)
            await self.backoff()

    async def backoff(self):
        step = BACKOFF[min(self.attempt, len(BACKOFF) - 1)]
        self.attempt += 1
        await asyncio.sleep(step * random.uniform(0.5, 1.5))

    async def read_packet(self, reader):
        head = await reader.readexactly(4)
        body = await reader.readexactly(struct.unpack(">I", head)[0])
        p = pb.Packet()
        p.ParseFromString(body)
        return p

    async def read_until_login(self, reader):
        while True:
            p = await asyncio.wait_for(self.read_packet(reader), 60)
            if p.type == pb.Packet.LOGIN_RES:
                if p.login_res.success:
                    self.token = p.login_res.jwt_token
                    return True
                self.w.login_errors[p.login_res.message] = self.w.login_errors.get(p.login_res.message, 0) + 1
                return False

    async def read_loop(self, reader):
        w = self.w
        while True:
            p = await self.read_packet(reader)
            t = p.type
            if t == pb.Packet.DIRECT_MSG:
                d = p.direct_msg
                if d.recipient_uid == self.uid and d.content.startswith("lt:"):
                    w.dm_lat.append((time.monotonic_ns() - int(d.content[3:])) / 1e6)
                    w.dms_recv += 1
            elif t == pb.Packet.PRESENCE_DELTA:
                w.deltas += 1
            elif t == pb.Packet.PRESENCE_UPDATE:
                w.answers += 1


class Worker:
    def __init__(self, wid, uids, key_path, addr_base):
        self.wid = wid
        self.ctx = tls_context()
        self.minter = Minter(key_path)
        self.all_uids = uids  # every load account, in index order
        self.stopping = False
        self.clients = []
        self.connected = self.drops = self.login_failures = self.connect_errors = 0
        self.restart_mark = None
        self.login_errors = {}
        self.dms_sent = self.dms_recv = self.deltas = self.answers = 0
        self.dm_lat = []
        self.first_down = None
        self.last_up = 0.0
        self.online = 0  # accounts [0, online) are connected (across workers)
        self.rates = {"dm": 0.0, "idle": 0.0, "resub": 0.0}
        self.addr_base = addr_base

    def source(self, n):
        # 40 clients per loopback address: under the per-address caps.
        k = self.addr_base + n // 40
        return f"127.{10 + k // 62500}.{(k // 250) % 250 + 1}.{k % 250 + 1}"

    async def connect(self, mine, online):
        """Bring up `mine` (indexes into all_uids) with up to 200 handshakes
        in flight; returns once each has signed in or failed once."""
        self.online = online
        t0 = time.time()
        sem = asyncio.Semaphore(200)
        ok = [0]
        loop = asyncio.get_running_loop()

        async def one(i):
            async with sem:
                uid = self.all_uids[i]
                c = Client(self, uid, self.source(len(self.clients)), self.minter.resume(uid))
                c.watch = random.sample(self.all_uids[:max(online, WATCH + 1)], WATCH)
                self.clients.append(c)
                done = loop.create_future()
                c.task = asyncio.create_task(c.run(lambda r: done.done() or done.set_result(r)))
                if await done:
                    ok[0] += 1

        await asyncio.gather(*(one(i) for i in mine))
        return {"ok": ok[0], "failed": len(mine) - ok[0], "secs": time.time() - t0}

    async def traffic(self):
        """Background traffic at self.rates (per second, this worker's share)."""
        carry = {"dm": 0.0, "idle": 0.0, "resub": 0.0}
        tick = 0.05
        ping_cursor = 0
        last_ping = time.monotonic()
        while not self.stopping:
            await asyncio.sleep(tick)
            up = [c for c in random.sample(self.clients, min(len(self.clients), 512)) if c.up] if self.clients else []
            for kind in carry:
                carry[kind] += self.rates[kind] * tick
            pool = self.all_uids[:max(self.online, 2)]
            while carry["dm"] >= 1 and up:
                carry["dm"] -= 1
                c = random.choice(up)
                to = random.choice(pool)
                if to == c.uid:
                    continue
                if c.send(frame(pb.Packet.DIRECT_MSG, c.token, direct_msg=pb.DirectMessage(
                        recipient_uid=to, content=f"lt:{time.monotonic_ns()}"))):
                    self.dms_sent += 1
            while carry["idle"] >= 1 and up:
                carry["idle"] -= 1
                c = random.choice(up)
                c.idle = not c.idle
                c.send(frame(pb.Packet.SET_IDLE_REQ, c.token, set_idle_req=pb.SetIdleRequest(idle=c.idle)))
            while carry["resub"] >= 1 and up:
                carry["resub"] -= 1
                c = random.choice(up)
                c.watch = random.sample(pool, min(WATCH, len(pool)))
                c.send(frame(pb.Packet.PRESENCE_SUBSCRIBE, c.token,
                             presence_subscribe=pb.PresenceSubscribe(uids=c.watch)))
            # Keepalive: every client pings about every 30 s, spread out.
            now = time.monotonic()
            if self.clients and now - last_ping >= 1.0:
                last_ping = now
                n = max(1, len(self.clients) // 30)
                for _ in range(n):
                    c = self.clients[ping_cursor % len(self.clients)]
                    ping_cursor += 1
                    c.send(frame(pb.Packet.CLIENT_PING, handshake=pb.Handshake()))

    def stats(self, reset_latency=True):
        lat = self.dm_lat
        if reset_latency:
            self.dm_lat = []
        relogins = [c.relogin_at for c in self.clients if c.relogin_at] if self.restart_mark else []
        return {"connected": self.connected, "clients": len(self.clients), "drops": self.drops,
                "login_failures": self.login_failures, "login_errors": dict(self.login_errors),
                "connect_errors": self.connect_errors, "relogins": relogins,
                "dms_sent": self.dms_sent, "dms_recv": self.dms_recv, "deltas": self.deltas,
                "answers": self.answers, "lat": lat, "first_down": self.first_down, "last_up": self.last_up}


def worker_main(conn, wid, uids, key_path, addr_base, cpus):
    if cpus:
        os.sched_setaffinity(0, cpus)
    import resource
    hard = resource.getrlimit(resource.RLIMIT_NOFILE)[1]
    resource.setrlimit(resource.RLIMIT_NOFILE, (hard, hard))
    signal.signal(signal.SIGINT, signal.SIG_IGN)
    w = Worker(wid, uids, key_path, addr_base)

    async def main():
        loop = asyncio.get_running_loop()
        traffic = asyncio.create_task(w.traffic())
        while True:
            msg = await loop.run_in_executor(None, conn.recv)
            cmd = msg["cmd"]
            if cmd == "connect":
                conn.send(await w.connect(msg["mine"], msg["online"]))
            elif cmd == "rates":
                w.rates = msg["rates"]
                w.online = msg.get("online", w.online)
                conn.send({"ok": True})
            elif cmd == "stats":
                conn.send(w.stats())
            elif cmd == "mark_restart":
                w.first_down = None
                w.drops = 0
                w.restart_mark = time.time()
                for c in w.clients:
                    c.relogin_at = None
                conn.send({"ok": True})
            elif cmd == "stop":
                w.stopping = True
                traffic.cancel()
                for c in w.clients:
                    if c.writer is not None:
                        c.writer.close()
                conn.send({"ok": True})
                return

    asyncio.run(main())


# --- Orchestrator -------------------------------------------------------------

class Pool:
    def __init__(self, n, uids, key_path, cpus):
        ctx = mp.get_context("spawn")
        self.conns, self.procs = [], []
        for wid in range(n):
            a, b = ctx.Pipe()
            # Each worker's loopback range: room for 25,000 addresses.
            p = ctx.Process(target=worker_main, args=(b, wid, uids, key_path, wid * 25000, cpus), daemon=True)
            p.start()
            self.conns.append(a)
            self.procs.append(p)

    def all(self, msgs):
        for c, m in zip(self.conns, msgs):
            c.send(m)
        return [c.recv() for c in self.conns]

    def broadcast(self, msg):
        return self.all([msg] * len(self.conns))

    def stats(self):
        parts = self.broadcast({"cmd": "stats"})
        total = {k: sum(p[k] for p in parts) for k in
                 ("connected", "clients", "drops", "login_failures", "connect_errors", "dms_sent", "dms_recv",
                  "deltas", "answers")}
        total["lat"] = [x for p in parts for x in p["lat"]]
        total["relogins"] = [x for p in parts for x in p["relogins"]]
        errors = {}
        for p in parts:
            for k, v in p["login_errors"].items():
                errors[k] = errors.get(k, 0) + v
        total["login_errors"] = errors
        downs = [p["first_down"] for p in parts if p["first_down"]]
        total["first_down"] = min(downs) if downs else None
        total["last_up"] = max(p["last_up"] for p in parts)
        return total

    def stop(self):
        try:
            self.broadcast({"cmd": "stop"})
        except Exception:
            pass
        for p in self.procs:
            p.join(5)
            if p.is_alive():
                p.kill()


class Meter:
    """CPU (in cores) and memory of central and PostgreSQL over a window,
    and the client workers' CPU (to show they weren't the bottleneck)."""

    def __init__(self, central_pid, pg_dir, worker_pids=()):
        self.central = psutil.Process(central_pid)
        self.pg_dir = pg_dir
        self.workers = [psutil.Process(p) for p in worker_pids]

    def pg_procs(self):
        out = []
        for p in psutil.process_iter(["name", "cmdline"]):
            try:
                if p.info["name"] and p.info["name"].startswith("postgres"):
                    out.append(p)
            except psutil.Error:
                pass
        return out

    def cpu(self, procs):
        total = 0.0
        for p in procs:
            try:
                t = p.cpu_times()
                total += t.user + t.system
            except psutil.Error:
                pass
        return total

    def window(self, secs):
        pg = self.pg_procs()
        c0, p0, w0, t0 = self.cpu([self.central]), self.cpu(pg), self.cpu(self.workers), time.time()
        time.sleep(secs)
        c1, p1, w1, t1 = self.cpu([self.central]), self.cpu(pg), self.cpu(self.workers), time.time()
        mem = self.central.memory_info().rss
        return {"central_cores": (c1 - c0) / (t1 - t0), "pg_cores": (p1 - p0) / (t1 - t0),
                "worker_cores": (w1 - w0) / (t1 - t0),
                "central_rss_mb": mem / 2**20, "central_fds": self.central.num_fds(),
                "central_threads": self.central.num_threads()}


def pin_postgres(cpus):
    for p in psutil.process_iter(["name"]):
        try:
            if p.info["name"] and p.info["name"].startswith("postgres"):
                p.cpu_affinity(cpus)
        except psutil.Error:
            pass


def seed(n):
    """`n` load accounts and two probes, sharing one bcrypt hash."""
    r = e2e.register("loadseed", PASSWORD)
    assert r and r.success, r
    h = e2e.sql("SELECT password_hash FROM users WHERE username = 'loadseed'")[0][0]
    e2e.sql("INSERT INTO users (username, email, password_hash) "
            "SELECT 'load' || g, 'load' || g || '@load.test', %s FROM generate_series(0, %s) g", h, n - 1)
    e2e.sql("INSERT INTO users (username, email, password_hash) VALUES "
            "('probea', 'probea@load.test', %s), ('probeb', 'probeb@load.test', %s)", h, h)
    rows = e2e.sql("SELECT uid FROM users WHERE username ~ '^load[0-9]+$' ORDER BY uid")
    return [r[0] for r in rows]


async def password_logins(n, uid_names):
    """`n` password sign-ins at once, 10 per address: bcrypt throughput."""
    ctx = tls_context()
    t0 = time.time()

    async def one(i):
        reader, writer = await asyncio.open_connection(HOST, PORT, ssl=ctx, local_addr=(f"127.9.{i // 10}.1", 0))
        writer.write(frame(pb.Packet.LOGIN_REQ, login_req=pb.LoginRequest(username=uid_names[i], password=PASSWORD)))
        ok = False
        try:
            while True:
                head = await asyncio.wait_for(reader.readexactly(4), 120)
                p = pb.Packet()
                p.ParseFromString(await reader.readexactly(struct.unpack(">I", head)[0]))
                if p.type == pb.Packet.LOGIN_RES:
                    ok = p.login_res.success
                    break
        finally:
            writer.close()
        return ok

    results = await asyncio.gather(*(one(i) for i in range(n)), return_exceptions=True)
    secs = time.time() - t0
    return {"n": n, "ok": sum(1 for r in results if r is True), "secs": secs, "per_sec": n / secs}


class Probe:
    """Two probe accounts on fresh connections (a ramp step can outlast
    central's 60 s idle sweep): B's auto-away flips as A, watching B, sees
    them, and A → B DMs."""

    def __enter__(self):
        self.a = e2e.User("probea", src="127.8.0.1")
        self.b = e2e.User("probeb", src="127.8.0.2")
        self.ub = self.b.login.uid
        self.a.watch(self.ub)
        self.idle = False
        return self

    def __exit__(self, *exc):
        self.a.close()
        self.b.close()

    def presence(self, samples=4):
        out = []
        for _ in range(samples):
            self.a.clear()
            self.idle = not self.idle
            t0 = time.monotonic()
            self.b.send(pb.Packet.SET_IDLE_REQ, set_idle_req=pb.SetIdleRequest(idle=self.idle))
            d = self.a.wait(pb.Packet.PRESENCE_DELTA, pred=lambda p: p.presence_delta.user.uid == self.ub, timeout=5)
            if d:
                out.append((time.monotonic() - t0) * 1000)
            time.sleep(0.3)
        return out

    def dm(self, samples=5):
        out = []
        for i in range(samples):
            t0 = time.monotonic()
            self.a.send(pb.Packet.DIRECT_MSG, direct_msg=pb.DirectMessage(recipient_uid=self.ub, content=f"probe {i}"))
            d = self.b.wait(pb.Packet.DIRECT_MSG, pred=lambda p: p.direct_msg.content == f"probe {i}", timeout=5)
            if d:
                out.append((time.monotonic() - t0) * 1000)
            time.sleep(0.4)
        return out


def ms(x):
    return "–" if x is None else f"{x:.1f}"


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--steps", default="5000,10000,20000,30000,40000")
    ap.add_argument("--rates", default="250,500,1000,2000,4000", help="DMs per second, total")
    ap.add_argument("--server-cpus", default="0,2,4,6,8,10")
    ap.add_argument("--worker-cpus", default="12-19")
    ap.add_argument("--workers", type=int, default=8)
    ap.add_argument("--password-logins", type=int, default=120)
    ap.add_argument("--hold", type=float, default=20.0, help="seconds per traffic step")
    args = ap.parse_args()
    steps = [int(x) for x in args.steps.split(",")]
    rates = [int(x) for x in args.rates.split(",")]
    server_cpus, worker_cpus = cpu_list(args.server_cpus), cpu_list(args.worker_cpus)
    n_users = steps[-1]
    run = e2e.RUN
    os.makedirs(run, exist_ok=True)
    os.sched_setaffinity(0, worker_cpus)
    report = {"steps": [], "traffic": [], "config": vars(args)}

    e2e.PG = pgserver.get_server(os.path.join(run, "pg"), cleanup_mode="stop")
    e2e.fresh_db()
    pin_postgres(server_cpus)
    env = {"DECIBELL_IO_THREADS": str(len(server_cpus)), "DECIBELL_CPU_THREADS": str(max(1, len(server_cpus) // 2))}
    proc = e2e.start_server(log_name="central-load.log", extra_env=env, cpus=server_cpus)
    pool = None
    try:
        print(f"seeding {n_users} accounts")
        uids = seed(n_users)
        names = [f"load{i}" for i in range(n_users)]
        meter = Meter(proc.pid, os.path.join(run, "pg"))

        print(f"password sign-ins ({args.password_logins})")
        report["password"] = asyncio.run(password_logins(args.password_logins, names))
        print(f"  {report['password']}")

        pool = Pool(args.workers, uids, os.path.join(run, "jwt_ed25519.pem"), worker_cpus)
        meter = Meter(proc.pid, os.path.join(run, "pg"), [p.pid for p in pool.procs])
        online = 0
        for target in steps:
            mine = [[i for i in range(online, target) if i % args.workers == w] for w in range(args.workers)]
            t0 = time.time()
            res = pool.all([{"cmd": "connect", "mine": m, "online": target} for m in mine])
            secs = time.time() - t0
            online = target
            pool.broadcast({"cmd": "rates", "rates": {"dm": 0, "idle": 0, "resub": 0}, "online": online})
            time.sleep(2)
            m = meter.window(5)
            st = pool.stats()
            with Probe() as probe:
                presence_ms, dm_ms = probe.presence(), probe.dm()
            row = {"clients": target, "connected": st["connected"],
                   "failed": sum(r["failed"] for r in res), "connect_secs": secs,
                   "connect_per_sec": (target - (report["steps"][-1]["clients"] if report["steps"] else 0)) / secs,
                   "presence_ms": presence_ms, "dm_ms": dm_ms, "login_errors": st["login_errors"],
                   "connect_errors": st["connect_errors"], **m}
            report["steps"].append(row)
            print(f"  {target:>6} clients: {st['connected']} up, {row['failed']} failed, "
                  f"{row['connect_per_sec']:.0f}/s, central {m['central_rss_mb']:.0f} MB "
                  f"{m['central_cores']:.2f} cores idle, presence p50 {ms(pct(row['presence_ms'], .5))} ms, "
                  f"dm p50 {ms(pct(row['dm_ms'], .5))} ms {st['login_errors'] or ''}")

        print("traffic")
        idle_rate = 0.02 * online / 60  # 2% of users flip away/back per minute ≈ status churn
        resub_rate = online / 120       # everyone switches view every ~2 minutes
        for rate in rates:
            per = {"dm": rate / args.workers, "idle": idle_rate / args.workers, "resub": resub_rate / args.workers}
            pool.broadcast({"cmd": "rates", "rates": per, "online": online})
            time.sleep(3)
            before = pool.stats()
            m = meter.window(args.hold)
            after = pool.stats()
            lat = after["lat"]
            sent = after["dms_sent"] - before["dms_sent"]
            recv = after["dms_recv"] - before["dms_recv"]
            row = {"dm_rate": rate, "sent_per_sec": sent / args.hold, "recv_per_sec": recv / args.hold,
                   "deltas_per_sec": (after["deltas"] - before["deltas"]) / args.hold,
                   "answers_per_sec": (after["answers"] - before["answers"]) / args.hold,
                   "lat_p50": pct(lat, .5), "lat_p99": pct(lat, .99), "lat_max": max(lat) if lat else None,
                   "connected": after["connected"], "drops": after["drops"], **m}
            report["traffic"].append(row)
            print(f"  {rate:>5} DM/s target: sent {row['sent_per_sec']:.0f}/s recv {row['recv_per_sec']:.0f}/s, "
                  f"latency p50 {ms(row['lat_p50'])} p99 {ms(row['lat_p99'])} max {ms(row['lat_max'])} ms, "
                  f"central {m['central_cores']:.2f} cores, pg {m['pg_cores']:.2f} cores, "
                  f"clients {m['worker_cores']:.2f} cores, {after['connected']} up, {after['drops']} drops")
            if row["lat_p99"] is not None and row["lat_p99"] > 1000:
                print("  p99 above 1 s: stopping the ramp")
                break
        pool.broadcast({"cmd": "rates", "rates": {"dm": 0, "idle": 0, "resub": 0}, "online": online})

        print("restart with everyone connected")
        pool.broadcast({"cmd": "mark_restart"})
        e2e.stop_server(proc)
        t_down = time.time()
        proc = e2e.start_server(log_name="central-load.log", extra_env=env, cpus=server_cpus)
        t_up = time.time()
        meter = Meter(proc.pid, os.path.join(run, "pg"), [p.pid for p in pool.procs])
        w0 = meter.cpu(meter.workers)
        deadline = t_up + 300
        while time.time() < deadline:
            st = pool.stats()
            if len(st["relogins"]) >= online:
                break
            time.sleep(0.5)
        cpu_used = meter.cpu([meter.central])
        worker_cpu = meter.cpu(meter.workers) - w0
        back = sorted(t - t_up for t in st["relogins"])
        marks = {str(q): back[min(len(back) - 1, int(q * online) - 1)]
                 for q in (0.5, 0.9, 0.99, 1.0) if len(back) >= int(q * online)}
        report["restart"] = {"down_secs": t_up - t_down, "back": marks, "connected": st["connected"],
                             "resumed": len(back), "of": online, "central_cpu_secs": cpu_used,
                             "client_cpu_secs": worker_cpu,
                             "login_errors": st["login_errors"], "connect_errors": st["connect_errors"],
                             "password_estimate_secs": online / report["password"]["per_sec"]}
        print(f"  {report['restart']}")
    finally:
        if pool:
            pool.stop()
        e2e.stop_server(proc)

    write_report(report, run)


def write_report(r, run):
    with open(os.path.join(run, "load-report.json"), "w") as f:
        json.dump(r, f, indent=1)
    cfg = r["config"]
    lines = [f"# Central load test", "",
             f"Central + PostgreSQL on CPUs {cfg['server_cpus']} ({len(cpu_list(cfg['server_cpus']))} threads: "
             f"{len(cpu_list(cfg['server_cpus']))} io, {max(1, len(cpu_list(cfg['server_cpus'])) // 2)} CPU); "
             f"{cfg['workers']} client workers on CPUs {cfg['worker_cpus']}.", ""]
    p = r.get("password")
    if p:
        lines += [f"**Password sign-ins:** {p['ok']}/{p['n']} in {p['secs']:.1f} s = {p['per_sec']:.1f}/s.", ""]
    lines += ["## Connected clients (idle: subscribed to 50 others, pinging)", "",
              "| clients | up | connect /s | central RSS | per client | central CPU | presence p50 | DM p50 |",
              "|---:|---:|---:|---:|---:|---:|---:|---:|"]
    for s in r["steps"]:
        lines.append(f"| {s['clients']} | {s['connected']} | {s['connect_per_sec']:.0f} | {s['central_rss_mb']:.0f} MB "
                     f"| {s['central_rss_mb'] * 1024 / max(1, s['connected']):.0f} KB | {s['central_cores']:.2f} cores "
                     f"| {ms(pct(s['presence_ms'], .5))} ms | {ms(pct(s['dm_ms'], .5))} ms |")
    lines += ["", "## Traffic at full population", "",
              "| DM/s target | sent /s | delivered /s | presence deltas /s | answers /s | p50 | p99 | max | central CPU | PostgreSQL CPU | client CPU |",
              "|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|"]
    for t in r["traffic"]:
        lines.append(f"| {t['dm_rate']} | {t['sent_per_sec']:.0f} | {t['recv_per_sec']:.0f} | {t['deltas_per_sec']:.0f} "
                     f"| {t['answers_per_sec']:.0f} | {ms(t['lat_p50'])} ms | {ms(t['lat_p99'])} ms | {ms(t['lat_max'])} ms "
                     f"| {t['central_cores']:.2f} | {t['pg_cores']:.2f} | {t.get('worker_cores', 0):.2f} |")
    rs = r.get("restart")
    if rs:
        back = rs["back"]
        lines += ["", "## Restart with everyone connected", "",
                  f"Down {rs['down_secs']:.1f} s. Signed back in after central was up again: 50% in "
                  f"{ms(back.get('0.5'))} s, 90% {ms(back.get('0.9'))} s, 99% {ms(back.get('0.99'))} s, all "
                  f"{ms(back.get('1.0'))} s ({rs['resumed']}/{rs['of']}); central used {rs['central_cpu_secs']:.1f} "
                  f"CPU-s since its start, the test clients {rs.get('client_cpu_secs', 0):.1f}. By password instead: about {rs['password_estimate_secs']:.0f} s of bcrypt."]
    with open(os.path.join(run, "load-report.md"), "w") as f:
        f.write("\n".join(lines) + "\n")
    print("\n".join(lines))


if __name__ == "__main__":
    main()
