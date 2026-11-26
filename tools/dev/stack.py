#!/usr/bin/env python3
"""Local test stack: up / down / status / logs / reset / client-env / seed.
Run through tools/dev/local-stack.sh (the harness venv's python, -I)."""
import os
import secrets
import shutil
import signal
import socket
import sqlite3
import subprocess
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import devlib as d  # noqa: E402

CENTRAL_BIN = os.path.join(d.BIN, "central_server")
COMMUNITY_BIN = os.path.join(d.BIN, "community_server")
SHIM = os.path.join(d.BIN, "bindshim.so")
LOGS = os.path.join(d.RUN, "logs")
PIDS = os.path.join(d.RUN, "pids")


def say(msg):
    print(msg, flush=True)


def die(msg):
    print(f"local-stack: {msg}", file=sys.stderr)
    sys.exit(1)


def run(cmd, **kw):
    return subprocess.run(cmd, check=True, capture_output=True, text=True, **kw)


# --- Keys (kept across `reset`: clients pin these certificates) ---------------------------------

def ensure_cert(dirpath, cn):
    os.makedirs(dirpath, exist_ok=True)
    crt, key = os.path.join(dirpath, "server.crt"), os.path.join(dirpath, "server.key")
    if not (os.path.exists(crt) and os.path.exists(key)):
        run(["openssl", "req", "-x509", "-newkey", "rsa:2048", "-nodes", "-days", "3650", "-subj", f"/CN={cn}",
             "-keyout", key, "-out", crt])
        os.chmod(key, 0o600)
    return crt, key


def ensure_keys():
    os.makedirs(d.KEYS, exist_ok=True)
    secret_file = os.path.join(d.KEYS, "community_secret")
    if not os.path.exists(secret_file):
        with open(secret_file, "w") as f:
            f.write(secrets.token_hex(24))
        os.chmod(secret_file, 0o600)
    ensure_cert(os.path.join(d.KEYS, "central"), "decibell-dev-central")
    for c in d.COMMUNITIES:
        ensure_cert(os.path.join(d.KEYS, c["slug"]), f"decibell-dev-{c['slug']}")
    with open(secret_file) as f:
        return f.read().strip()


def link_certs(src_dir, dst_dir):
    os.makedirs(dst_dir, exist_ok=True)
    for name in ("server.crt", "server.key"):
        dst = os.path.join(dst_dir, name)
        if os.path.lexists(dst):
            os.remove(dst)
        os.symlink(os.path.join(src_dir, name), dst)


# --- Processes ----------------------------------------------------------------------------------

def pidfile(name):
    return os.path.join(PIDS, f"{name}.pid")


def running_pid(name, binary):
    """The pid in <name>.pid when that process is alive and runs `binary`."""
    try:
        with open(pidfile(name)) as f:
            pid = int(f.read().strip())
        exe = os.readlink(f"/proc/{pid}/exe")
    except (OSError, ValueError):
        return 0
    return pid if os.path.realpath(exe).replace(" (deleted)", "") == os.path.realpath(binary) else 0


def spawn(name, argv, cwd, env):
    os.makedirs(LOGS, exist_ok=True)
    os.makedirs(PIDS, exist_ok=True)
    log = open(os.path.join(LOGS, f"{name}.log"), "ab")
    log.write(f"\n==== {time.strftime('%Y-%m-%d %H:%M:%S')} start {name} ====\n".encode())
    log.flush()
    # The servers log through std::cout, block-buffered into a file; stdbuf makes `logs` live. (stdbuf
    # preloads its own library next to LD_PRELOAD's bind shim; exec keeps the pid the server's.)
    stdbuf = shutil.which("stdbuf")
    if stdbuf:
        argv = [stdbuf, "-oL", "-eL", *argv]
    p = subprocess.Popen(argv, cwd=cwd, env=env, stdin=subprocess.DEVNULL, stdout=log, stderr=subprocess.STDOUT,
                         start_new_session=True)
    with open(pidfile(name), "w") as f:
        f.write(str(p.pid))
    return p


def stop(name, binary, timeout=10):
    pid = running_pid(name, binary)
    if not pid:
        try:
            os.remove(pidfile(name))
        except FileNotFoundError:
            pass
        return False
    os.kill(pid, signal.SIGTERM)
    end = time.time() + timeout
    while time.time() < end and running_pid(name, binary):
        time.sleep(0.1)
    if running_pid(name, binary):
        os.kill(pid, signal.SIGKILL)
        time.sleep(0.3)
    try:
        os.remove(pidfile(name))
    except FileNotFoundError:
        pass
    return True


def port_free(ip, port, kind=socket.SOCK_STREAM):
    s = socket.socket(socket.AF_INET, kind)
    try:
        s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        s.bind((ip, port))
        return True
    except OSError:
        return False
    finally:
        s.close()


def alive(name):
    """The pid in <name>.pid is a live process (whatever it runs: during startup it may still be stdbuf)."""
    try:
        with open(pidfile(name)) as f:
            pid = int(f.read().strip())
        with open(f"/proc/{pid}/stat") as f:
            return f.read().rsplit(")", 1)[1].split()[0] != "Z"
    except (OSError, ValueError, IndexError):
        return False


def wait_tcp(ip, port, proc_name, binary, timeout=20):
    end = time.time() + timeout
    while time.time() < end:
        try:
            socket.create_connection((ip, port), timeout=0.5).close()
            return
        except OSError:
            if not alive(proc_name):
                die(f"{proc_name} exited during startup; see {os.path.join(LOGS, proc_name + '.log')}")
            time.sleep(0.1)
    die(f"{proc_name} did not start listening on {ip}:{port}")


# --- PostgreSQL ---------------------------------------------------------------------------------

def pg_ctl(*args, check=True):
    return subprocess.run([os.path.join(d.PGBIN, "pg_ctl"), "-D", d.PGDATA, *args], capture_output=True, text=True,
                          check=check)


def pg_running():
    return pg_ctl("status", check=False).returncode == 0


def pg_up():
    if not os.path.exists(os.path.join(d.PGDATA, "PG_VERSION")):
        os.makedirs(os.path.dirname(d.PGDATA), exist_ok=True)
        run([os.path.join(d.PGBIN, "initdb"), "-D", d.PGDATA, "-U", "postgres", "--auth=trust", "-E", "UTF8",
             "--no-instructions"])
    if not pg_running():
        os.makedirs(LOGS, exist_ok=True)
        opts = f"-k {d.PGSOCK} -c listen_addresses='' -p {d.PGPORT} -c max_connections=200 -c fsync=off " \
               f"-c synchronous_commit=off -c full_page_writes=off"
        r = pg_ctl("-l", os.path.join(LOGS, "postgres.log"), "-o", opts, "-w", "-t", "30", "start", check=False)
        if r.returncode != 0:
            die(f"PostgreSQL did not start: {r.stderr.strip() or r.stdout.strip()} (log: {LOGS}/postgres.log)")
    if not d.pg("SELECT 1 FROM pg_database WHERE datname = %s", d.PGDB, db="postgres"):
        d.pg(f"CREATE DATABASE {d.PGDB}", db="postgres")


# --- up / down ----------------------------------------------------------------------------------

def preflight():
    busy = []
    if not running_pid("central", CENTRAL_BIN) and not port_free(d.CENTRAL_HOST, d.CENTRAL_PORT):
        busy.append(f"{d.CENTRAL_HOST}:{d.CENTRAL_PORT}/tcp")
    for c in d.COMMUNITIES:
        if running_pid(c["slug"], COMMUNITY_BIN):
            continue
        for port, kind in ((8082, "tcp"), (8083, "udp"), (8084, "udp"), (8085, "tcp")):
            if not port_free(c["ip"], port, socket.SOCK_DGRAM if kind == "udp" else socket.SOCK_STREAM):
                busy.append(f"{c['ip']}:{port}/{kind}")
    if busy:
        die("in use: " + ", ".join(busy) + ". A community e2e run (it binds 0.0.0.0:8082-8085) or a dev "
            "community server holds the community ports; stop it first.")


def cmd_up():
    for b in (CENTRAL_BIN, COMMUNITY_BIN, SHIM):
        if not os.path.exists(b):
            die(f"{b} missing: run 'local-stack.sh setup' first")
    preflight()
    secret = ensure_keys()
    t0 = time.time()
    pg_up()
    say(f"postgres   up  (unix socket {d.PGSOCK}/.s.PGSQL.{d.PGPORT}, database {d.PGDB})")

    state = d.read_state()
    base = dict(os.environ)
    for k in list(base):
        if k.startswith("DECIBELL_DEV_") or k == "LD_PRELOAD":
            base.pop(k)

    # Central: its port (8080) is hard-coded; the bind shim moves it to 127.0.0.1:CENTRAL_PORT.
    central_dir = os.path.join(d.RUN, "central")
    link_certs(os.path.join(d.KEYS, "central"), central_dir)
    jwt_key = os.path.join(d.KEYS, "central", "jwt_ed25519.pem")
    if not running_pid("central", CENTRAL_BIN):
        env = dict(base, DECIBELL_COMMUNITY_SECRET=secret, DECIBELL_DB_CONN=d.pg_conninfo(),
                   DECIBELL_JWT_KEY_FILE=jwt_key, LD_PRELOAD=SHIM, DECIBELL_DEV_BIND_IP=d.CENTRAL_HOST,
                   DECIBELL_DEV_PORT_MAP=f"8080:{d.CENTRAL_PORT}")
        env.setdefault("DECIBELL_IO_THREADS", "4")
        env.setdefault("DECIBELL_DB_THREADS", "16")
        env.setdefault("DECIBELL_CPU_THREADS", "2")
        spawn("central", [CENTRAL_BIN], central_dir, env)
    wait_tcp(d.CENTRAL_HOST, d.CENTRAL_PORT, "central", CENTRAL_BIN)
    for _ in range(50):
        if os.path.exists(jwt_key + ".pub"):
            break
        time.sleep(0.1)
    central_fp = d.cert_fingerprint_pem(os.path.join(d.KEYS, "central", "server.crt"))
    state["central"] = {"host": d.CENTRAL_HOST, "port": d.CENTRAL_PORT, "fingerprint": central_fp,
                        "pid": running_pid("central", CENTRAL_BIN)}
    d.write_json(d.STATE, state)
    say(f"central    up  {d.CENTRAL_HOST}:{d.CENTRAL_PORT}  cert sha256 {central_fp}")

    # Communities, one after another so a fresh central numbers them 1, 2, 3.
    for c in d.COMMUNITIES:
        start_community(c, secret, central_fp, base)
        st = d.read_state()
        say(f"community  up  {c['name']:<13} {c['ip']}:8082 (udp 8083/8084, https 8085)  "
            f"server_id {st['communities'][c['slug']]['server_id']}")
    say(f"stack up in {time.time() - t0:.1f} s. Client env: export DECIBELL_CENTRAL={d.CENTRAL_HOST}:{d.CENTRAL_PORT}")


def start_community(c, secret=None, central_fp=None, base=None):
    """Start one community (if it isn't running) on its own loopback address with the standard ports,
    wait until it listens and has registered with central through its heartbeat, and record it in
    stack.json."""
    if secret is None:
        secret = ensure_keys()
    if central_fp is None:
        central_fp = d.cert_fingerprint_pem(os.path.join(d.KEYS, "central", "server.crt"))
    if base is None:
        base = {k: v for k, v in os.environ.items() if not k.startswith("DECIBELL_DEV_") and k != "LD_PRELOAD"}
    jwt_key = os.path.join(d.KEYS, "central", "jwt_ed25519.pem")
    cdir = d.community_dir(c["slug"])
    link_certs(os.path.join(d.KEYS, c["slug"]), cdir)
    if not running_pid(c["slug"], COMMUNITY_BIN):
        env = dict(base, DECIBELL_JWT_PUBLIC_KEY_FILE=jwt_key + ".pub", DECIBELL_COMMUNITY_SECRET=secret,
                   DECIBELL_CENTRAL_HOST=d.CENTRAL_HOST, DECIBELL_CENTRAL_PORT=str(d.CENTRAL_PORT),
                   DECIBELL_CENTRAL_CERT_FINGERPRINT=central_fp, DECIBELL_PUBLIC_IP=c["ip"],
                   DECIBELL_SERVER_NAME=c["name"], DECIBELL_SERVER_DESC=c["desc"],
                   DECIBELL_OWNER_USERNAME=c["owner"], DECIBELL_DB_PATH="./community.db",
                   DECIBELL_ATTACHMENTS_ROOT="./attachments", LD_PRELOAD=SHIM, DECIBELL_DEV_BIND_IP=c["ip"])
        env.setdefault("DECIBELL_HEARTBEAT_INTERVAL_SECONDS", "30")
        spawn(c["slug"], [COMMUNITY_BIN], cdir, env)
    wait_tcp(c["ip"], d.COMMUNITY_PORT, c["slug"], COMMUNITY_BIN)
    # Registered with central through the heartbeat: wait for the row and the id handed back.
    end, sid = time.time() + 20, 0
    while time.time() < end:
        rows = d.pg("SELECT id FROM community_servers WHERE host_ip = %s AND port = %s", c["ip"], d.COMMUNITY_PORT)
        if rows and community_meta(c["slug"], "central_server_id") == str(rows[0][0]):
            sid = rows[0][0]
            break
        time.sleep(0.1)
    if not sid:
        die(f"{c['name']} did not register with central (see {LOGS}/{c['slug']}.log)")
    state = d.read_state()
    state.setdefault("communities", {})[c["slug"]] = {
        "name": c["name"], "host": c["ip"], "port": d.COMMUNITY_PORT, "server_id": sid,
        "fingerprint": d.cert_fingerprint_pem(os.path.join(d.KEYS, c["slug"], "server.crt")),
        "pid": running_pid(c["slug"], COMMUNITY_BIN)}
    d.write_json(d.STATE, state)
    return sid


def stop_community(c):
    return stop(c["slug"], COMMUNITY_BIN)


def require_up():
    missing = []
    if not running_pid("central", CENTRAL_BIN):
        missing.append("central")
    missing += [c["slug"] for c in d.COMMUNITIES if not running_pid(c["slug"], COMMUNITY_BIN)]
    if missing:
        die(f"not running: {', '.join(missing)} (run 'local-stack.sh up' first)")


def community_meta(slug, key):
    try:
        c = sqlite3.connect(f"file:{d.community_db(slug)}?mode=ro", uri=True, timeout=2)
        try:
            r = c.execute("SELECT value FROM server_meta WHERE key = ?", (key,)).fetchone()
            return r[0] if r else None
        finally:
            c.close()
    except sqlite3.Error:
        return None


def cmd_down():
    for c in reversed(d.COMMUNITIES):
        if stop(c["slug"], COMMUNITY_BIN):
            say(f"community  stopped  {c['name']}")
    if stop("central", CENTRAL_BIN):
        say("central    stopped")
    if os.path.exists(os.path.join(d.PGDATA, "PG_VERSION")) and pg_running():
        pg_ctl("-m", "fast", "-w", "stop", check=False)
        say("postgres   stopped")
    say("stack down (data kept; 'up' brings it back as it was)")


def cmd_reset():
    cmd_down()
    removed = []
    if os.path.isdir(d.RUN):
        for name in os.listdir(d.RUN):
            if name == "keys":
                continue
            p = os.path.join(d.RUN, name)
            shutil.rmtree(p) if os.path.isdir(p) and not os.path.islink(p) else os.remove(p)
            removed.append(name)
    say(f"reset: wiped {d.RUN} ({', '.join(sorted(removed)) or 'nothing'}); kept keys/ (TLS certs, JWT key, "
        f"community secret) so clients' pins stay valid. Next: local-stack.sh up && local-stack.sh seed")


# --- status / logs / client-env -----------------------------------------------------------------

def client_env_lines():
    st = d.read_state()
    lines = [f"export DECIBELL_CENTRAL={d.CENTRAL_HOST}:{d.CENTRAL_PORT}"]
    fp = st.get("central", {}).get("fingerprint")
    return lines, fp


def cmd_client_env():
    lines, fp = client_env_lines()
    print("\n".join(lines))
    print("# Debug builds honour DECIBELL_CENTRAL; release builds need the cargo feature `local-central`.")
    if fp:
        print(f"# central cert (trusted on first use by the client): sha256 {fp}")
    print(f"# accounts (password {d.PASSWORD}): {d.ACCOUNTS_JSON}")


def cmd_status():
    st = d.read_state()
    pg_ok = os.path.exists(os.path.join(d.PGDATA, "PG_VERSION")) and pg_running()
    say(f"dev dir   {d.DEV}")
    say(f"postgres  {'running' if pg_ok else 'stopped':8} socket {d.PGSOCK}/.s.PGSQL.{d.PGPORT} db {d.PGDB}")
    pid = running_pid("central", CENTRAL_BIN)
    say(f"central   {'running' if pid else 'stopped':8} {d.CENTRAL_HOST}:{d.CENTRAL_PORT}"
        + (f"  pid {pid}" if pid else "") + stale_binary(pid) + f"  log {LOGS}/central.log")
    rows = {}
    if pg_ok:
        try:
            for sid, name, host, port, members, public, age in d.pg(
                    "SELECT id, name, host_ip, port, member_count, is_public, "
                    "EXTRACT(EPOCH FROM NOW() - last_heartbeat)::int FROM community_servers ORDER BY id"):
                rows[host] = (sid, name, members, public, age)
        except Exception as e:  # noqa: BLE001
            say(f"          (central database not readable: {e})")
    for c in d.COMMUNITIES:
        pid = running_pid(c["slug"], COMMUNITY_BIN)
        extra = ""
        if c["ip"] in rows:
            sid, name, members, public, age = rows[c["ip"]]
            extra = f"  id {sid}, {members} members, {'public' if public else 'unlisted'}, heartbeat {age} s ago"
        say(f"{c['slug']:<9} {'running' if pid else 'stopped':8} {c['ip']}:8082 udp 8083/8084 https 8085  "
            f"{c['name']}" + (f"  pid {pid}" if pid else "") + extra + stale_binary(pid))
    if os.path.exists(d.ACCOUNTS_JSON):
        say(f"accounts  {d.ACCOUNTS_JSON} (password {d.PASSWORD})")
    else:
        say("accounts  not seeded yet (local-stack.sh seed)")
    lines, fp = client_env_lines()
    say("client env:")
    for line in lines:
        say("  " + line)


def stale_binary(pid):
    try:
        return "  (binary rebuilt since start: down + up to run it)" if pid and \
            os.readlink(f"/proc/{pid}/exe").endswith(" (deleted)") else ""
    except OSError:
        return ""


def cmd_logs(args):
    names = {"central": "central", "postgres": "postgres", "pg": "postgres"}
    for c in d.COMMUNITIES:
        names[c["slug"]] = c["slug"]
        names[c["name"].lower().replace(" ", "-")] = c["slug"]
    if not args or args[0] not in names:
        die("logs <name>: one of " + ", ".join(sorted(set(names))))
    path = os.path.join(LOGS, names[args[0]] + ".log")
    if not os.path.exists(path):
        die(f"no log yet at {path}")
    follow = ["-F"] if sys.stdout.isatty() and "--no-follow" not in args else []
    os.execvp("tail", ["tail", "-n", "200", *follow, path])


def main():
    signal.signal(signal.SIGPIPE, signal.SIG_DFL)   # `status | head` ends quietly
    cmd, args = (sys.argv[1] if len(sys.argv) > 1 else ""), sys.argv[2:]
    if cmd == "up":
        cmd_up()
    elif cmd == "down":
        cmd_down()
    elif cmd == "reset":
        cmd_reset()
    elif cmd == "status":
        cmd_status()
    elif cmd == "logs":
        cmd_logs(args)
    elif cmd == "client-env":
        cmd_client_env()
    elif cmd == "seed":
        import seed
        seed.main(args)
    else:
        die(f"unknown command {cmd!r}")


if __name__ == "__main__":
    main()
