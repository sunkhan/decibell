"""`local-stack.sh seed`: fill the running stack with the test fixtures. Idempotent: every step looks at
the databases first and does only what is missing, so a second run changes nothing.

Through the real protocol: accounts, friends, a few DMs, joins by invite, channels, categories, roles,
overwrites, slowmode, topics, invites, public listing, messages, edits, attachment uploads over HTTPS
(with thumbnails and ThumbHash placeholders), moderation (ban, kick, timeout, nicknames, a deletion).
Through SQL, as CLAUDE.md asks for bulk rows: the DM histories, placeholder members, and the channel
histories (the 10k fixture of qt-client/tools/gen-fixture.mjs in #general). Community SQL runs with that
community stopped, so its caches start from what is on disk.
"""
import collections
import json
import os
import sqlite3
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import devlib as d  # noqa: E402
import fixture  # noqa: E402
import stack  # noqa: E402

pb = d.pb
P = pb  # permission bits live at module level (pb.PERM_*)

NOW = int(time.time())
FIXTURE_CACHE = os.path.join(d.DEV, "cache", "fixture-v1")   # survives `reset`

EVERYONE_DEFAULT = (P.PERM_SEND_MESSAGES | P.PERM_CONNECT_VOICE | P.PERM_STREAM | P.PERM_VIEW_CHANNEL
                    | P.PERM_READ_HISTORY | P.PERM_ATTACH_FILES | P.PERM_CREATE_INVITE | P.PERM_CHANGE_NICKNAME
                    | P.PERM_EMBED_LINKS | P.PERM_SPEAK)
MOD_BITS = (P.PERM_KICK_MEMBERS | P.PERM_BAN_MEMBERS | P.PERM_MANAGE_MESSAGES | P.PERM_MANAGE_NICKNAMES
            | P.PERM_VIEW_AUDIT_LOG | P.PERM_MODERATE_MEMBERS | P.PERM_MUTE_MEMBERS | P.PERM_DEAFEN_MEMBERS
            | P.PERM_MOVE_MEMBERS | P.PERM_MANAGE_INVITES)
TEXT, VOICE, CATEGORY = pb.ChannelInfo.TEXT, pb.ChannelInfo.VOICE, pb.ChannelInfo.CATEGORY
ROLE, MEMBER = pb.ChannelOverwrite.ROLE, pb.ChannelOverwrite.MEMBER


def log(msg):
    print(f"[seed] {msg}", flush=True)


class SeedError(Exception):
    pass


# --- Placeholder people ---------------------------------------------------------------------------------
# Community-only members (positive uids no central account has: 100001+), inserted by SQL. The first
# names are gen-fixture.mjs's (lower-cased into valid usernames; the owner's own account name left out).

FIXTURE_NAMES = ['johnny_bravo', 'battalocelot', 'berkayacan', 'dolpompx', 'efil', 'emreaydn', 'fiary',
                 'maghteriton', 'n.rosess', 'asdasd', 'kekrem', 'qalite', 'deniz', 'zeynep_k',
                 'mert.the.long.name.example', 'griefer_9000', 'loud_larry', 'drive_by_dan']
ADJ = ['quiet', 'pixel', 'lazy', 'turbo', 'cosmic', 'salty', 'mellow', 'rapid', 'sleepy', 'neon', 'crispy', 'lucky',
       'grumpy', 'stealthy', 'fuzzy', 'brave', 'tiny', 'mega', 'frosty', 'sunny', 'retro', 'wild', 'shy', 'noisy']
NOUN = ['koala', 'falcon', 'otter', 'panda', 'lynx', 'badger', 'gecko', 'heron', 'yak', 'moose', 'crab', 'raven',
        'tapir', 'walrus', 'mantis', 'bison', 'quokka', 'ferret', 'puffin', 'wombat', 'llama', 'newt', 'orca']
PLACEHOLDER_BASE = 100001


def placeholder_pool(n=360):
    rnd = fixture.Rng(1234)
    names, seen = [], set()
    for name in FIXTURE_NAMES:
        names.append(name)
        seen.add(name)
    while len(names) < n:
        name = rnd.pick(ADJ) + rnd.pick(['', '_', '.']) + rnd.pick(NOUN)
        if rnd() < 0.5:
            name += str(rnd.between(1, 999))
        if name not in seen and len(name) <= 32:
            seen.add(name)
            names.append(name)
    return [(PLACEHOLDER_BASE + i, name) for i, name in enumerate(names)]


POOL = placeholder_pool()
POOL_BY_NAME = {name: uid for uid, name in POOL}
# Which placeholders each community has (overlapping a little, as real people do).
POOL_SLICE = {"gaming": POOL[0:291], "rust": POOL[280:305], "design": POOL[300:340]}


# --- Helpers ---------------------------------------------------------------------------------------------

class Ctx:
    def __init__(self):
        self.uids = {}
        self.tokens = {}
        self.notes = []
        self.invites = {}
        self.sent = 0       # channel messages sent through the protocol this run
        self.mod = 0        # moderation actions this run

    # central -----------------------------------------------------------------------------------------------
    def central(self, name):
        s = d.Central(name, app="local-stack-seed")
        if not s.ok:
            raise SeedError(f"sign-in as {name} failed: {s.login.message if s.login else 'no answer'}")
        self.uids[name] = s.uid
        self.tokens[name] = s.token
        return s

    def token(self, name):
        if name not in self.tokens:
            self.central(name).close()
        return self.tokens[name]

    # community ---------------------------------------------------------------------------------------------
    def session(self, slug, name, invite=""):
        c = d.BY_SLUG[slug]
        fp = d.read_state()["communities"][slug]["fingerprint"]
        s = d.Community(c["ip"], self.token(name), fp, invite)
        if not s.ok:
            raise SeedError(f"{name} could not connect to {c['name']}: "
                            f"{s.auth.error_code if s.auth else 'no answer'} {s.auth.message if s.auth else ''}")
        s.who = name
        s.slug = slug
        s.last_send = 0.0
        return s


def cdb(slug, ro=False):
    path = d.community_db(slug)
    con = sqlite3.connect(f"file:{path}?mode=ro" if ro else path, uri=ro, timeout=10)
    con.execute("PRAGMA busy_timeout = 10000")
    return con


def q(slug, sql, *args):
    con = cdb(slug, ro=True)
    try:
        return con.execute(sql, args).fetchall()
    finally:
        con.close()


def q1(slug, sql, *args):
    rows = q(slug, sql, *args)
    return rows[0][0] if rows else None


def is_member(slug, uid):
    return bool(q(slug, "SELECT 1 FROM members WHERE uid = ?", uid))


def expect(r, what):
    if r is None:
        raise SeedError(f"{what}: no answer")
    return r


def channel_by_name(s, name, ctype=None):
    s.refresh_channels(0.2)
    return s.channel(name, ctype)


def ensure_channel(s, name, ctype, category_id="", encrypted=False):
    ch = channel_by_name(s, name, ctype)
    if ch:
        return ch
    s.send(pb.Packet.CHANNEL_CREATE_REQ, channel_create_req=pb.ChannelCreateRequest(
        name=name, type=ctype, category_id=category_id, encrypted=encrypted))
    r = expect(s.wait(pb.Packet.CHANNEL_ACTION_RES, pred=lambda p: p.channel_action_res.action == "create"),
               f"create #{name}").channel_action_res
    if not r.success:
        raise SeedError(f"create #{name}: {r.message}")
    s.refresh_channels(0.3)
    return r.channel


def rename_channel(s, channel_id, name):
    cur = next((c for c in s.refresh_channels(0.2) if c.id == channel_id), None)
    if cur is None or cur.name == name:
        return
    s.send(pb.Packet.CHANNEL_RENAME_REQ, channel_rename_req=pb.ChannelRenameRequest(channel_id=channel_id, name=name))
    r = expect(s.wait(pb.Packet.CHANNEL_ACTION_RES, pred=lambda p: p.channel_action_res.action == "rename"),
               f"rename {channel_id}").channel_action_res
    if not r.success:
        raise SeedError(f"rename {channel_id}: {r.message}")


def reorder(s, ids):
    cur = [c.id for c in s.refresh_channels(0.3)]
    if cur == ids:
        return
    if sorted(cur) != sorted(ids):
        raise SeedError(f"reorder: channel set differs: have {cur}, want {ids}")
    s.send(pb.Packet.CHANNEL_REORDER_REQ, channel_reorder_req=pb.ChannelReorderRequest(channel_ids=ids))
    r = expect(s.wait(pb.Packet.CHANNEL_ACTION_RES, pred=lambda p: p.channel_action_res.action == "reorder"),
               "reorder").channel_action_res
    if not r.success:
        raise SeedError(f"reorder: {r.message}")
    s.refresh_channels(0.3)


def update_channel(s, ch, slowmode=None, topic=None):
    cur = next(c for c in s.refresh_channels(0.2) if c.id == ch.id)
    if (slowmode is None or cur.slowmode_seconds == slowmode) and (topic is None or cur.topic == topic):
        return
    req = pb.ChannelUpdateRequest(
        channel_id=cur.id, retention_days_text=cur.retention_days_text, retention_days_image=cur.retention_days_image,
        retention_days_video=cur.retention_days_video, retention_days_document=cur.retention_days_document,
        retention_days_audio=cur.retention_days_audio)
    if slowmode is not None:
        req.slowmode_seconds = slowmode
    if topic is not None:
        req.topic = topic
    s.send(pb.Packet.CHANNEL_UPDATE_REQ, channel_update_req=req)
    r = expect(s.wait(pb.Packet.CHANNEL_UPDATE_RES), f"update #{cur.name}").channel_update_res
    if not r.success:
        raise SeedError(f"update #{cur.name}: {r.message}")


def roles(s):
    s.inbox = [p for p in s.inbox if p.type != pb.Packet.ROLE_LIST_RES]
    s.send(pb.Packet.ROLE_LIST_REQ, role_list_req=pb.RoleListRequest())
    r = expect(s.wait(pb.Packet.ROLE_LIST_RES), "role list").role_list_res
    return {role.name: role for role in r.roles}


def ensure_role(s, name, color, perms):
    have = roles(s)
    if name in have:
        return have[name]
    s.send(pb.Packet.ROLE_CREATE_REQ, role_create_req=pb.RoleCreateRequest(name=name, color=color, permissions=perms))
    r = expect(s.wait(pb.Packet.ROLE_ACTION_RES, pred=lambda p: p.role_action_res.action == "create"),
               f"create role {name}").role_action_res
    if not r.success:
        raise SeedError(f"create role {name}: {r.message}")
    return r.role


def set_everyone(s, perms):
    everyone = next(r for r in roles(s).values() if r.is_default)
    if everyone.permissions == perms:
        return
    s.send(pb.Packet.ROLE_UPDATE_REQ, role_update_req=pb.RoleUpdateRequest(
        role_id=everyone.id, name=everyone.name, color=everyone.color, permissions=perms, position=0,
        manage_each_other=everyone.manage_each_other))
    r = expect(s.wait(pb.Packet.ROLE_ACTION_RES, pred=lambda p: p.role_action_res.action == "update"),
               "update @everyone").role_action_res
    if not r.success:
        raise SeedError(f"update @everyone: {r.message}")


def assign(s, uid, role_ids):
    have = sorted(r[0] for r in q(s.slug, "SELECT role_id FROM member_roles WHERE uid = ?", uid))
    if have == sorted(role_ids):
        return
    s.send(pb.Packet.MEMBER_ROLES_UPDATE_REQ, member_roles_update_req=pb.MemberRolesUpdateRequest(
        uid=uid, role_ids=role_ids))
    r = expect(s.wait(pb.Packet.ROLE_ACTION_RES, pred=lambda p: p.role_action_res.action == "assign"),
               f"assign roles to {uid}").role_action_res
    if not r.success:
        raise SeedError(f"assign roles to {uid}: {r.message}")


def overwrite(s, channel_id, target_type, target, allow=0, deny=0):
    """target: a role id (ROLE) or a member uid (MEMBER)."""
    tid = str(target)
    row = q(s.slug, "SELECT allow, deny FROM channel_overwrites WHERE channel_id=? AND target_type=? AND target_id=?",
            channel_id, int(target_type), tid)
    if row and tuple(row[0]) == (allow, deny):
        return
    ow = pb.ChannelOverwrite(channel_id=channel_id, target_type=target_type, allow=allow, deny=deny)
    if target_type == MEMBER:
        ow.target_uid = int(target)
    else:
        ow.target_id = tid
    s.send(pb.Packet.CHANNEL_OVERWRITE_SET_REQ, channel_overwrite_set_req=pb.ChannelOverwriteSetRequest(overwrite=ow))
    r = expect(s.wait(pb.Packet.CHANNEL_ACTION_RES, pred=lambda p: p.channel_action_res.action == "overwrite"),
               f"overwrite on {channel_id}").channel_action_res
    if not r.success:
        raise SeedError(f"overwrite on {channel_id}: {r.message}")


def category_sync(s, category_id):
    cur = next(c for c in s.refresh_channels(0.2) if c.id == category_id)
    if not cur.category_sync:
        s.send(pb.Packet.CATEGORY_SYNC_SET_REQ, category_sync_set_req=pb.CategorySyncSetRequest(
            channel_id=category_id, enabled=True))
        r = expect(s.wait(pb.Packet.CHANNEL_ACTION_RES, pred=lambda p: p.channel_action_res.action == "category_sync"),
                   "category sync").channel_action_res
        if not r.success:
            raise SeedError(f"category sync: {r.message}")


def sync_channels(s, category_id, child_ids):
    s.refresh_channels(0.2)
    if all(c.permissions_synced for c in s.channels if c.id in child_ids):
        return
    s.send(pb.Packet.CHANNEL_PERMISSIONS_SYNC_REQ, channel_permissions_sync_req=pb.ChannelPermissionsSyncRequest(
        channel_id=category_id))
    r = expect(s.wait(pb.Packet.CHANNEL_ACTION_RES, pred=lambda p: p.channel_action_res.action == "sync"),
               "permissions sync").channel_action_res
    if not r.success:
        raise SeedError(f"permissions sync: {r.message}")


def server_update(s, name, desc, public):
    a = s.auth
    if a.server_name == name and a.server_description == desc and a.public_listing == public:
        return
    s.send(pb.Packet.SERVER_UPDATE_REQ, server_update_req=pb.ServerUpdateRequest(
        name=name, description=desc, public_listing=public))
    r = expect(s.wait(pb.Packet.SERVER_UPDATE_RES), "server update").server_update_res
    if not r.success:
        raise SeedError(f"server update: {r.message}")


def ensure_invite(s, max_uses=0, expires_at=0, for_uid=0):
    """An invite of this shape by this creator, created if there is none (expiring ones match on 'has
    an expiry')."""
    for code, mu, exp, fu in q(s.slug, "SELECT code, max_uses, expires_at, for_uid FROM invites WHERE created_by_uid=?",
                               s_uid(s)):
        if mu == max_uses and fu == for_uid and (exp == 0) == (expires_at == 0) and (exp == 0 or exp > NOW):
            return code
    s.send(pb.Packet.INVITE_CREATE_REQ, invite_create_req=pb.InviteCreateRequest(
        expires_at=expires_at, max_uses=max_uses, for_uid=for_uid, request_id=7))
    r = expect(s.wait(pb.Packet.INVITE_CREATE_RES), "create invite").invite_create_res
    if not r.success:
        raise SeedError(f"create invite: {r.message}")
    return r.invite.code


def s_uid(s):
    return s._uid


def pace(s, gap=0.4):
    """The community's message bucket is 10 burst / 3 per s; stay under it like the client does."""
    wait = s.last_send + gap - time.time()
    if wait > 0:
        time.sleep(wait)
    s.last_send = time.time()


def has_msg(slug, channel_id, uid, content):
    return bool(q(slug, "SELECT 1 FROM messages WHERE channel_id=? AND sender_uid=? AND content=?",
                  channel_id, uid, content))


SENT = [0]


def send_msg(s, channel_id, content, attachments=(), reply_to=0):
    SENT[0] += 1
    nonce = os.urandom(8).hex()
    pace(s)
    s.send(pb.Packet.CHANNEL_MSG, channel_msg=pb.ChannelMessage(
        channel_id=channel_id, content=content, nonce=nonce, reply_to=reply_to,
        attachments=[pb.Attachment(id=a) for a in attachments]))
    end = time.time() + 10
    while time.time() < end:
        r = s.wait(pb.Packet.CHANNEL_MSG, timeout=0.3, pred=lambda p: p.channel_msg.nonce == nonce)
        if r:
            return r.channel_msg
        rej = [p for p in s.inbox if p.type == pb.Packet.CHANNEL_MSG_REJECTED and p.channel_msg_rejected.nonce == nonce]
        if rej:
            raise SeedError(f"message refused in {channel_id}: {rej[0].channel_msg_rejected.reason}")
    raise SeedError(f"no echo for a message in {channel_id}")


def say_once(s, channel_id, content, **kw):
    """Send unless this sender already said exactly this there. Returns the message id."""
    row = q(s.slug, "SELECT id FROM messages WHERE channel_id=? AND sender_uid=? AND content=? ORDER BY id DESC LIMIT 1",
            channel_id, s._uid, content)
    if row:
        return row[0][0]
    return send_msg(s, channel_id, content, **kw).id


MODS = [0]


def mod_res(s, action):
    MODS[0] += 1
    r = expect(s.wait(pb.Packet.MOD_ACTION_RES, pred=lambda p: p.mod_action_res.action == action), action)
    if not r.mod_action_res.success:
        raise SeedError(f"{action}: {r.mod_action_res.message}")


# --- Attachments ---------------------------------------------------------------------------------------------

def picture(key, w, h, label):
    """A drawn picture with its JPEG / PNG bytes, thumbnails and ThumbHash, cached on disk by key + size."""
    import media
    os.makedirs(FIXTURE_CACHE, exist_ok=True)
    stem = os.path.join(FIXTURE_CACHE, f"pic-{key}-{w}x{h}")
    meta_path = stem + ".json"
    if os.path.exists(meta_path):
        with open(meta_path) as f:
            meta = json.load(f)
    else:
        img = media.draw_picture(key, w, h, label)
        with open(stem + ".jpg", "wb") as f:
            f.write(media.jpeg(img, 88))
        thumbs = media.thumbnails(img)
        for size, data in thumbs.items():
            with open(f"{stem}.thumb-{size}px.jpg", "wb") as f:
                f.write(data)
        meta = {"placeholder": media.thumbhash(img), "thumb_bytes": sum(len(v) for v in thumbs.values()),
                "size": os.path.getsize(stem + ".jpg")}
        d.write_json(meta_path, meta)
    meta["path"] = stem + ".jpg"
    meta["thumbs"] = {size: f"{stem}.thumb-{size}px.jpg" for size in (320, 640, 1280)}
    return meta


def upload_picture(http, channel_id, filename, key, w, h, as_png=False):
    import media
    pic = picture(key, w, h, filename)
    if as_png:
        from PIL import Image
        data = media.png(Image.open(pic["path"]))
        mime = "image/png"
    else:
        with open(pic["path"], "rb") as f:
            data = f.read()
        mime = "image/jpeg"
    thumbs = {}
    for size, path in pic["thumbs"].items():
        with open(path, "rb") as f:
            thumbs[size] = f.read()
    return http.upload(channel_id, filename, mime, data, w, h, pic["placeholder"], thumbs)


def bound_upload(slug, channel_id, filename):
    """The id of a message in this channel that already carries an attachment of this name."""
    return q1(slug, "SELECT message_id FROM attachments WHERE channel_id=? AND filename=? AND message_id > 0",
              channel_id, filename)


# --- Central ---------------------------------------------------------------------------------------------------

def seed_accounts(ctx):
    created = 0
    for name in d.ACCOUNTS:
        rows = d.pg("SELECT uid FROM users WHERE username = %s", name)
        if not rows:
            r = d.register(name)
            if not r or not r.success:
                raise SeedError(f"register {name}: {r.message if r else 'no answer'}")
            rows = d.pg("SELECT uid FROM users WHERE username = %s", name)
            created += 1
        ctx.uids[name] = rows[0][0]
    log(f"accounts: {len(d.ACCOUNTS)} ({created} registered now), password {d.PASSWORD!r}")
    write_accounts(ctx)


def seed_friends(ctx):
    U = ctx.uids
    a = ctx.central("alice")
    fl = a.friends()
    did = []
    for peer in ("bob", "carol"):
        f = fl.get(U[peer])
        if f is None:
            r = a.friend(pb.ADD, U[peer], peer)
            if not (r and r.success):
                raise SeedError(f"alice adds {peer}: {r.message if r else 'no answer'}")
            f = pb.FriendInfo(status=pb.FriendInfo.PENDING_OUTGOING)
        if f.status in (pb.FriendInfo.PENDING_OUTGOING, pb.FriendInfo.PENDING_INCOMING):
            p = ctx.central(peer)
            r = p.friend(pb.ACCEPT, U["alice"], "alice")
            p.close()
            if not (r and r.success):
                raise SeedError(f"{peer} accepts alice: {r.message if r else 'no answer'}")
            did.append(f"alice<->{peer}")
    if U["dave"] not in fl:
        p = ctx.central("dave")
        r = p.friend(pb.ADD, U["alice"], "alice")
        p.close()
        if not (r and r.success):
            raise SeedError(f"dave adds alice: {r.message if r else 'no answer'}")
        did.append("dave->alice pending")
    if fl.get(U["erin"]) is None or fl[U["erin"]].status != pb.FriendInfo.BLOCKED:
        r = a.friend(pb.BLOCK, U["erin"], "erin")
        if not (r and r.success):
            raise SeedError(f"alice blocks erin: {r.message if r else 'no answer'}")
        did.append("alice blocked erin")
    a.close()
    log("friends: " + (", ".join(did) if did else "already in place"))


def dm_history(a, b, n, seed, end_ts, span_days):
    """(sender, recipient, content, sent_at, reply_index, edited_at) oldest first, in conversation bursts."""
    rnd = fixture.Rng(seed)
    gen = fixture.ContentGen(rnd)
    t = 0
    who = 0
    rows = []
    for i in range(n):
        if rnd() < 0.1:
            t += rnd.between(2, 30) * 3600
        elif rnd() < 0.6:
            t += rnd.between(5, 120)
        else:
            t += rnd.between(2, 20) * 60
        if rnd() < 0.35:
            who ^= 1
        content = gen.content()
        reply = i - rnd.between(1, 4) if i > 5 and rnd() < 0.05 else None
        edited = rnd.between(20, 600) if rnd() < 0.03 else 0
        rows.append([(a, b)[who], (b, a)[who], content, t, reply, edited])
    scale = min(1.0, span_days * 86400 / max(1, rows[-1][3]))
    shift = end_ts - int(rows[-1][3] * scale)
    for r in rows:
        r[3] = shift + int(r[3] * scale)
        r[5] = r[3] + r[5] if r[5] else 0
    return rows


def seed_dms(ctx):
    import psycopg
    U = ctx.uids
    plan = [("alice", "bob", 594, 41, 45), ("alice", "carol", 29, 42, 20)]
    for a, b, n, seed, days in plan:
        lo, hi = sorted((U[a], U[b]))
        have = d.pg("SELECT count(*) FROM dm_messages WHERE LEAST(sender_uid, recipient_uid) = %s "
                    "AND GREATEST(sender_uid, recipient_uid) = %s", lo, hi)[0][0]
        if have:
            log(f"DMs {a}<->{b}: {have} already there")
            continue
        rows = dm_history(a, b, n, seed, NOW - 3 * 3600, days)
        ids = []
        with psycopg.connect(d.pg_conninfo()) as con:
            with con.cursor() as cur:
                for snd, rcp, content, at, reply, edited in rows:
                    cur.execute(
                        "INSERT INTO dm_messages (sender, recipient, content, sent_at, edited_at, reply_to, sender_uid, "
                        "recipient_uid) VALUES (%s, %s, %s, %s, %s, %s, %s, %s) RETURNING id",
                        (snd, rcp, content, at, edited, ids[reply] if reply is not None else 0, U[snd], U[rcp]))
                    ids.append(cur.fetchone()[0])
                # The peer has read everything so far (alice's cursor is moved by a real DM_MARK_READ below).
                cur.execute("INSERT INTO dm_read_cursors (reader_uid, peer_uid, last_read_id) VALUES (%s, %s, %s) "
                            "ON CONFLICT (reader_uid, peer_uid) DO UPDATE SET last_read_id = "
                            "GREATEST(dm_read_cursors.last_read_id, EXCLUDED.last_read_id)", (U[b], U[a], ids[-1]))
            con.commit()
        log(f"DMs {a}<->{b}: {n} rows by SQL ({days} days, ending 3 h ago)")


def seed_live_dms(ctx):
    U = ctx.uids
    script = [("bob", "alice", "you around? the stack is up 🎉"),
              ("bob", "alice", "check #clips, I posted the screenshots"),
              ("alice", "bob", "on it, give me 5 min"),
              ("bob", "alice", "also: patch notes draft is in your inbox"),
              ("bob", "alice", "ping me when you've read it 👀"),
              ("carol", "alice", "mockups for the welcome banner are in Design Lab #critique")]
    sessions = {}
    sent = 0
    for snd, rcp, text in script:
        if d.pg("SELECT 1 FROM dm_messages WHERE sender_uid = %s AND recipient_uid = %s AND content = %s",
                U[snd], U[rcp], text):
            continue
        s = sessions.get(snd) or sessions.setdefault(snd, ctx.central(snd))
        m = s.dm(U[rcp], text)
        if not m or not m.id:
            raise SeedError(f"DM {snd}->{rcp} got no echo")
        sent += 1
        time.sleep(0.15)
    # alice has read the bob thread up to her own reply: bob's last two stay unread.
    mine = d.pg("SELECT max(id) FROM dm_messages WHERE sender_uid = %s AND recipient_uid = %s", U["alice"], U["bob"])
    if mine and mine[0][0]:
        a = sessions.get("alice") or sessions.setdefault("alice", ctx.central("alice"))
        a.send(pb.Packet.DM_MARK_READ_REQ, dm_mark_read_req=pb.DmMarkReadReq(peer_uid=U["bob"], up_to_id=mine[0][0]))
        a.drain(0.4)
    for s in sessions.values():
        s.close()
    log(f"DMs live: {sent} sent through central (DIRECT_MSG), alice's read cursor on bob moved by DM_MARK_READ")


# --- Communities: structure (owner sessions) -----------------------------------------------------------------

def join_all(ctx, slug, names, code):
    """Each account joins the way a client does: COMMUNITY_AUTH_REQ carrying the invite code."""
    joined = []
    for name in names:
        if is_member(slug, ctx.uids[name]):
            continue
        s = ctx.session(slug, name, invite=code)
        s.close()
        joined.append(name)
    return joined


def owner_session(ctx, slug):
    c = d.BY_SLUG[slug]
    s = ctx.session(slug, c["owner"])
    s._uid = ctx.uids[c["owner"]]
    if s.auth.owner_uid != s._uid:
        raise SeedError(f"{c['owner']} is not the owner of {c['name']} (owner uid {s.auth.owner_uid})")
    return s


def structure_gaming(ctx):
    U = ctx.uids
    s = owner_session(ctx, "gaming")
    server_update(s, "Daily Gaming", d.BY_SLUG["gaming"]["desc"], True)
    rename_channel(s, "voice-lounge", "Lounge")
    rename_channel(s, "voice-lounge-2", "Squad")
    cat_info = ensure_channel(s, "Information", CATEGORY)
    cat_text = ensure_channel(s, "Text Channels", CATEGORY)
    cat_staff = ensure_channel(s, "Staff", CATEGORY)
    cat_voice = ensure_channel(s, "Voice Channels", CATEGORY)
    welcome = ensure_channel(s, "welcome", TEXT, cat_info.id)
    clips = ensure_channel(s, "clips", TEXT, cat_text.id)
    offtopic = ensure_channel(s, "off-topic", TEXT, cat_text.id)
    slow = ensure_channel(s, "slow-lane", TEXT, cat_text.id)
    # End-to-end encrypted, chosen at creation; left empty: clients set up identities and epoch keys.
    vault = ensure_channel(s, "vault", TEXT, cat_text.id, encrypted=True)
    modchat = ensure_channel(s, "mod-chat", TEXT, cat_staff.id)
    modlog = ensure_channel(s, "mod-log", TEXT, cat_staff.id)
    afk = ensure_channel(s, "AFK", VOICE, cat_voice.id)
    reorder(s, [cat_info.id, welcome.id, "announcements",
                cat_text.id, "general", clips.id, offtopic.id, slow.id, vault.id,
                cat_staff.id, modchat.id, modlog.id,
                cat_voice.id, "voice-lounge", "voice-lounge-2", afk.id])
    update_channel(s, offtopic, topic="Anything that isn't games. Be nice, no spoilers without ||tags||.")
    update_channel(s, s.channel("announcements"), topic="Server news. Only moderators post here.")
    update_channel(s, s.channel("general"), topic="Games, builds, patch notes. The busy one.")
    update_channel(s, slow, slowmode=30, topic="Slowmode is on: one message every 30 s.")
    update_channel(s, vault, topic="End-to-end encrypted. Topics stay readable, messages don't.")

    # Roles: each new role lands at position 1, so create the most senior first.
    admin = ensure_role(s, "Admin", 0xE74C3C, P.PERM_ADMINISTRATOR)
    mod = ensure_role(s, "Moderator", 0x3498DB, EVERYONE_DEFAULT | MOD_BITS)
    member = ensure_role(s, "Member", 0x2ECC71, EVERYONE_DEFAULT)
    everyone_id = next(r.id for r in roles(s).values() if r.is_default)

    # Overwrites: read-only announcements, a private Staff category its channels follow, a member one.
    overwrite(s, "announcements", ROLE, everyone_id, deny=P.PERM_SEND_MESSAGES)
    overwrite(s, "announcements", ROLE, mod.id, allow=P.PERM_SEND_MESSAGES)
    category_sync(s, cat_staff.id)
    overwrite(s, cat_staff.id, ROLE, everyone_id, deny=P.PERM_VIEW_CHANNEL)
    overwrite(s, cat_staff.id, ROLE, mod.id, allow=P.PERM_VIEW_CHANNEL | P.PERM_SEND_MESSAGES)
    sync_channels(s, cat_staff.id, {modchat.id, modlog.id})

    # Invites: an open one (the joins below use it), a 5-use one, one that expires in a week.
    code = ensure_invite(s)
    ensure_invite(s, max_uses=5)
    ensure_invite(s, expires_at=NOW + 7 * 86400)
    ctx.invites["gaming"] = code
    joined = join_all(ctx, "gaming", ["bob", "carol", "dave", "erin", "perf1", "perf2", "perf3", "perf4"], code)
    assign(s, U["bob"], [admin.id])
    assign(s, U["carol"], [mod.id])
    for name in ("dave", "erin", "perf1", "perf2", "perf3", "perf4"):
        assign(s, U[name], [member.id])
    overwrite(s, offtopic.id, MEMBER, U["dave"], allow=P.PERM_MANAGE_MESSAGES)
    s.close()
    log(f"Daily Gaming: structure in place; joined by invite now: {', '.join(joined) or 'nobody new'}")


def structure_rust(ctx):
    U = ctx.uids
    s = owner_session(ctx, "rust")
    server_update(s, "Rustaceans", d.BY_SLUG["rust"]["desc"], False)
    rename_channel(s, "voice-lounge", "Pairing")
    rename_channel(s, "voice-lounge-2", "Office Hours")
    help_ = ensure_channel(s, "help", TEXT)
    showcase = ensure_channel(s, "showcase", TEXT)
    core = ensure_channel(s, "core-team", TEXT)
    reorder(s, ["general", "announcements", help_.id, showcase.id, core.id, "voice-lounge", "voice-lounge-2"])
    update_channel(s, s.channel("general"), topic="Rust talk. Paste compiler errors in full.")
    update_channel(s, help_, topic="Ask anything. Minimal reproductions get answers fastest.")
    # @everyone without invites, nicknames or streaming: alice (a plain member) sees the gated UI.
    limited = EVERYONE_DEFAULT & ~(P.PERM_CREATE_INVITE | P.PERM_CHANGE_NICKNAME | P.PERM_STREAM)
    set_everyone(s, limited)
    core_role = ensure_role(s, "Core", 0xDEA584, limited | P.PERM_MANAGE_MESSAGES | P.PERM_KICK_MEMBERS
                            | P.PERM_CREATE_INVITE | P.PERM_MANAGE_INVITES | P.PERM_VIEW_AUDIT_LOG
                            | P.PERM_MODERATE_MEMBERS | P.PERM_CHANGE_NICKNAME)
    rustacean = ensure_role(s, "Rustacean", 0xB7410E, limited)
    everyone_id = next(r.id for r in roles(s).values() if r.is_default)
    overwrite(s, "announcements", ROLE, everyone_id, deny=P.PERM_SEND_MESSAGES)
    overwrite(s, showcase.id, ROLE, everyone_id, deny=P.PERM_ATTACH_FILES)
    overwrite(s, core.id, ROLE, everyone_id, deny=P.PERM_VIEW_CHANNEL)
    overwrite(s, core.id, ROLE, core_role.id, allow=P.PERM_VIEW_CHANNEL)
    code = ensure_invite(s)
    ctx.invites["rust"] = code
    joined = join_all(ctx, "rust", ["alice", "carol", "dave"], code)
    ensure_invite(s, for_uid=U["erin"])   # personal, single use, for someone not in yet
    assign(s, U["carol"], [core_role.id])
    assign(s, U["alice"], [rustacean.id])
    assign(s, U["dave"], [rustacean.id])
    s.close()
    log(f"Rustaceans: structure in place; joined by invite now: {', '.join(joined) or 'nobody new'}")


def structure_design(ctx):
    U = ctx.uids
    s = owner_session(ctx, "design")
    server_update(s, "Design Lab", d.BY_SLUG["design"]["desc"], True)
    rename_channel(s, "voice-lounge", "Studio")
    rename_channel(s, "voice-lounge-2", "Crit Room")
    critique = ensure_channel(s, "critique", TEXT)
    insp = ensure_channel(s, "inspiration", TEXT)
    typen = ensure_channel(s, "type-nerds", TEXT)
    reorder(s, ["general", "announcements", critique.id, insp.id, typen.id, "voice-lounge", "voice-lounge-2"])
    update_channel(s, critique, topic="Post work, get feedback. Be kind, be specific.")
    curator = ensure_role(s, "Curator", 0x9B59B6, EVERYONE_DEFAULT | P.PERM_MANAGE_MESSAGES | P.PERM_MANAGE_INVITES)
    code = ensure_invite(s)
    ctx.invites["design"] = code
    ensure_invite(s, max_uses=1)
    joined = join_all(ctx, "design", ["bob", "dave", "erin"], code)
    assign(s, U["bob"], [curator.id])
    s.close()
    log(f"Design Lab: structure in place (public); joined by invite now: {', '.join(joined) or 'nobody new'}")


# --- Communities: bulk (SQL, community stopped) ---------------------------------------------------------------

def bulk(ctx, slug, fill):
    if q1(slug, "SELECT value FROM server_meta WHERE key = 'devstack_bulk'") == "1":
        log(f"{d.BY_SLUG[slug]['name']}: bulk rows already there")
        return
    c = d.BY_SLUG[slug]
    t0 = time.time()
    stack.stop_community(c)
    con = cdb(slug)
    try:
        con.execute("BEGIN IMMEDIATE")
        counts = fill(con)
        con.execute("INSERT OR REPLACE INTO server_meta(key, value) VALUES ('devstack_bulk', '1')")
        con.commit()
    except Exception:
        con.rollback()
        raise
    finally:
        con.close()
    stack.start_community(c)
    log(f"{c['name']}: bulk by SQL in {time.time() - t0:.1f} s (community restarted): {counts}")


def insert_members(con, people, joined_rnd):
    n = 0
    for uid, name in people:
        cur = con.execute("INSERT OR IGNORE INTO members(uid, username, joined_at, nickname) VALUES (?, ?, ?, '')",
                          (uid, name, NOW - joined_rnd.between(100, 700) * 86400))
        n += cur.rowcount
    return n


def senders_for(ctx, real, people, n):
    """`n` senders: the real accounts first (heavy / regular talkers), then placeholders."""
    out = [(ctx.uids[r], r) for r in real]
    out += [p for p in people if p[1] not in real][:n - len(out)]
    return out


def channel_stream(channel_id, senders, count, seed, end_ms, span_days, **kw):
    msgs = fixture.generate(count, seed, senders=senders, span_days=span_days, end_ms=end_ms, **kw)
    for m in msgs:
        m["channel"] = channel_id
        m["sender"] = senders[m["sender"]]
        if m["reply_to"] is not None:
            m["reply_to"] = msgs[m["reply_to"]]
    return msgs


def insert_messages(con, slug, streams):
    """Insert every stream's messages in time order (ids rise with time across channels, as live ones do),
    with their image attachments: rows bound the way /complete + CHANNEL_MSG bind them, files hard-linked
    from the picture cache (thumbnails at 320 / 640 / 1280 and a ThumbHash each)."""
    allm = sorted((m for st in streams for m in st), key=lambda m: m["ts_ms"])
    att_dir = os.path.join(d.community_dir(slug), "attachments")
    pics = {}
    n_att = 0
    for m in allm:
        uid, name = m["sender"]
        ts = m["ts_ms"] // 1000
        reply = m["reply_to"]["id"] if m["reply_to"] else 0
        edited = min(NOW, ts + m["edited_after_s"]) if m["edited_after_s"] else 0
        cur = con.execute("INSERT INTO messages(channel_id, sender, sender_uid, content, timestamp, edited_at, reply_to) "
                          "VALUES (?, ?, ?, ?, ?, ?, ?)", (m["channel"], name, uid, m["content"], ts, edited, reply))
        m["id"] = cur.lastrowid
        for pos, (pid, w, h) in enumerate(m["images"]):
            if (pid, w, h) not in pics:
                pics[(pid, w, h)] = picture(pid, w, h, f"photo {pid}  {w}x{h}")
            pic = pics[(pid, w, h)]
            fname = f"photo-{pid}-{w}x{h}.jpg"
            cur = con.execute(
                "INSERT INTO attachments(message_id, kind, filename, mime, size_bytes, storage_path, position, created_at, "
                "upload_status, expected_size, uploader, channel_id, width, height, thumbnail_size_bytes, "
                "thumbnail_sizes_mask, placeholder, encrypted, last_activity_at, uploader_uid) "
                "VALUES (0, 0, ?, 'image/jpeg', ?, '', ?, ?, 'ready', ?, ?, ?, ?, ?, ?, 7, ?, 0, ?, ?)",
                (fname, pic["size"], pos, ts, pic["size"], name, m["channel"], w, h, pic["thumb_bytes"],
                 pic["placeholder"], ts, uid))
            aid = cur.lastrowid
            rel = f"./attachments/{m['channel']}/{aid}_{fname}"
            os.makedirs(os.path.join(att_dir, m["channel"]), exist_ok=True)
            dst = os.path.join(d.community_dir(slug), rel[2:])
            link(pic["path"], dst)
            for size, path in pic["thumbs"].items():
                link(path, f"{dst}.thumb-{size}px.jpg")
            # Binding by UPDATE fires the trigger that indexes the file name for search, as a send does.
            con.execute("UPDATE attachments SET storage_path = ?, message_id = ? WHERE id = ?", (rel, m["id"], aid))
            n_att += 1
    return len(allm), n_att


def link(src, dst):
    if os.path.exists(dst):
        os.remove(dst)
    try:
        os.link(src, dst)
    except OSError:
        import shutil
        shutil.copyfile(src, dst)


def fill_gaming(ctx):
    def fill(con):
        people = POOL_SLICE["gaming"]
        added = insert_members(con, people, fixture.Rng(501))
        role = {name: rid for rid, name in con.execute("SELECT id, name FROM roles")}
        for uid, name in people[:200]:
            con.execute("INSERT OR IGNORE INTO member_roles(uid, role_id) VALUES (?, ?)", (uid, role["Member"]))
        for name in ("deniz", "zeynep_k", "efil"):
            con.execute("INSERT OR IGNORE INTO member_roles(uid, role_id) VALUES (?, ?)",
                        (POOL_BY_NAME[name], role["Moderator"]))
        # 40 senders for #general: alice, bob, carol, dave talk most; erin + 3 regulars; 32 occasional.
        talkers = senders_for(ctx, ["alice", "bob", "carol", "dave", "erin"], people, 40)
        end_ms = NOW * 1000
        general = channel_stream("general", talkers, 10000, 7, end_ms, 90)
        offtopic = channel_stream(q1("gaming", "SELECT id FROM channels WHERE name='off-topic'"), talkers, 300, 31,
                                  end_ms - 3 * 3600 * 1000, 60)
        n_msg, n_att = insert_messages(con, "gaming", [general, offtopic])
        return f"{added} placeholder members, {n_msg} messages (#general 10000 = the gen-fixture.mjs seed-7 " \
               f"fixture, #off-topic 300), {n_att} image attachments"
    bulk(ctx, "gaming", fill)


def fill_rust(ctx):
    def fill(con):
        people = POOL_SLICE["rust"]
        added = insert_members(con, people, fixture.Rng(502))
        talkers = senders_for(ctx, ["bob", "alice", "carol", "dave"], people, 16)
        end_ms = (NOW - 2 * 3600) * 1000
        general = channel_stream("general", talkers, 400, 11, end_ms, 30, edit_rate=0.03)
        help_ = channel_stream(q1("rust", "SELECT id FROM channels WHERE name='help'"), talkers, 100, 12,
                               end_ms - 5 * 3600 * 1000, 30)
        n_msg, n_att = insert_messages(con, "rust", [general, help_])
        return f"{added} placeholder members, {n_msg} messages (#general 400, #help 100), {n_att} image attachments"
    bulk(ctx, "rust", fill)


def fill_design(ctx):
    def fill(con):
        people = POOL_SLICE["design"]
        added = insert_members(con, people, fixture.Rng(503))
        talkers = senders_for(ctx, ["carol", "bob", "erin", "dave"], people, 14)
        end_ms = (NOW - 5 * 3600) * 1000
        general = channel_stream("general", talkers, 70, 22, end_ms, 20)
        critique = channel_stream(q1("design", "SELECT id FROM channels WHERE name='critique'"), talkers, 80, 21,
                                  end_ms - 3600 * 1000, 20)
        n_msg, n_att = insert_messages(con, "design", [general, critique])
        return f"{added} placeholder members, {n_msg} messages (#general 70, #critique 80), {n_att} image attachments"
    bulk(ctx, "design", fill)


# --- Communities: live traffic + moderation (after the bulk, so ids keep rising with time) -------------------

def live_gaming(ctx):
    U = ctx.uids
    ses = {}

    def S(name):
        if name not in ses:
            ses[name] = ctx.session("gaming", name)
            ses[name]._uid = U[name]
        return ses[name]

    ch = {c.name: c.id for c in S("alice").channels}
    uploads = 0
    sent0, mods0 = SENT[0], MODS[0]

    # #general: the present end of the busy channel.
    b1 = say_once(S("bob"), "general", "anyone up for ranked tonight? 🎮")
    say_once(S("carol"), "general", "after 9, need to finish a mockup first", reply_to=b1)
    edited = "Patch notes are up: https://example.com/patch-notes (voice got **a lot** better)"
    if not has_msg("gaming", "general", U["alice"], edited):
        a1 = say_once(S("alice"), "general", "Patch notes are up: https://example.com/patch-notes voice got a lot better")
        s = S("alice")
        pace(s)
        s.send(pb.Packet.MESSAGE_EDIT_REQ, message_edit_req=pb.MessageEditReq(channel_id="general", message_id=a1,
                                                                             content=edited))
        r = expect(s.wait(pb.Packet.MESSAGE_EDIT_RES), "edit").message_edit_res
        if not r.success:
            raise SeedError(f"edit: {r.message}")
    if not bound_upload("gaming", "general", "ranked-win.jpg"):
        h = S("dave").attachment_http()
        aid = upload_picture(h, "general", "ranked-win.jpg", 101, 1920, 1080)
        h.close()
        send_msg(S("dave"), "general", "gg 😎 finally out of silver", attachments=[aid])
        uploads += 1
    say_once(S("erin"), "general", "`fps=144` finally stable on wayland, the new driver fixed it")

    # #announcements (read-only for @everyone; alice owns the place).
    say_once(S("alice"), "announcements",
             "**Server rules**\n1. Be nice.\n2. No spoilers outside ||spoiler tags||.\n3. Clips go in #clips.\n"
             "4. Ask a `Moderator` if something is off.")
    say_once(S("alice"), "announcements", "Movie night Friday 21:00 in Lounge 🍿 bring snacks")

    # #clips: real uploads over the HTTPS API (init, PATCH, complete, 320/640/1280 thumbnails, ThumbHash).
    if not bound_upload("gaming", ch["clips"], "clip-night-1.jpg"):
        h = S("alice").attachment_http()
        ids = [upload_picture(h, ch["clips"], f"clip-night-{i + 1}.jpg", 110 + i, w, hh)
               for i, (w, hh) in enumerate([(1600, 900), (1280, 960), (900, 1200)])]
        h.close()
        send_msg(S("alice"), ch["clips"], "new clips from last night's session", attachments=ids)
        uploads += 3
    if not bound_upload("gaming", ch["clips"], "phone-screenshot.jpg"):
        h = S("bob").attachment_http()
        aid = upload_picture(h, ch["clips"], "phone-screenshot.jpg", 120, 1080, 1920)
        h.close()
        send_msg(S("bob"), ch["clips"], "phone screenshot of the scoreboard", attachments=[aid])
        uploads += 1
    if not bound_upload("gaming", ch["clips"], "patch-notes.txt"):
        import media
        h = S("carol").attachment_http()
        notes = ("Patch 0.9.4\n- voice: smoother jitter buffer\n- streams: 5 fps slides mode\n"
                 "- chat: link previews for clips\n").encode()
        a1_ = h.upload(ch["clips"], "patch-notes.txt", "text/plain", notes)
        a2_ = h.upload(ch["clips"], "settings-export.zip", "application/zip",
                       media.zip_of({"settings.json": b'{"theme": "nocturne", "ptt": "F13"}\n',
                                     "README.txt": b"Decibell settings export (dev stack fixture)\n"}))
        h.close()
        send_msg(S("carol"), ch["clips"], "patch notes + my settings export, for anyone asking", attachments=[a1_, a2_])
        uploads += 2
    if not bound_upload("gaming", ch["clips"], "voice-memo.wav"):
        import media
        h = S("dave").attachment_http()
        aid = h.upload(ch["clips"], "voice-memo.wav", "audio/wav", media.wav_tone(3.0))
        h.close()
        send_msg(S("dave"), ch["clips"], "mic test, does this sound clipped?", attachments=[aid])
        uploads += 1
    if not bound_upload("gaming", ch["clips"], "ultrawide.png"):
        h = S("erin").attachment_http()
        aid = upload_picture(h, ch["clips"], "ultrawide.png", 130, 2560, 1080, as_png=True)
        h.close()
        send_msg(S("erin"), ch["clips"], "ultrawide finally works in this game", attachments=[aid])
        uploads += 1

    # #slow-lane (slowmode 30 s: one message each), staff channels.
    say_once(S("dave"), ch["slow-lane"], "testing slowmode 🐢")
    say_once(S("erin"), ch["slow-lane"], "me too, see you in 30 seconds")
    say_once(S("carol"), ch["mod-chat"], "heads up: griefer_9000 posted spam links in #general again")
    say_once(S("alice"), ch["mod-chat"], "on it, banning")
    say_once(S("alice"), ch["mod-log"], "2026 moderation log: ban griefer_9000 (spam), kick drive_by_dan (bot), "
                                        "timeout loud_larry (caps)")

    # Moderation by the owner and a moderator: audited for real.
    a = S("alice")
    victim = POOL_BY_NAME["griefer_9000"]
    spam = q1("gaming", "SELECT id FROM messages WHERE channel_id='general' AND sender_uid=? ORDER BY id DESC LIMIT 1",
              victim)
    if spam and not q("gaming", "SELECT 1 FROM audit_log WHERE action='message_delete'"):
        c = S("carol")
        c.send(pb.Packet.MESSAGE_DELETE_REQ, message_delete_req=pb.MessageDeleteReq(channel_id="general",
                                                                                     message_id=spam))
        r = expect(c.wait(pb.Packet.MESSAGE_DELETE_RES), "delete").message_delete_res
        MODS[0] += 1
        if not r.success:
            raise SeedError(f"moderator delete: {r.message}")
    if not q("gaming", "SELECT 1 FROM bans WHERE uid = ?", victim):
        a.send(pb.Packet.BAN_MEMBER_REQ, ban_member_req=pb.BanMemberRequest(uid=victim, username="griefer_9000",
                                                                            reason="Spam links in #general"))
        mod_res(a, "ban")
    kicked = POOL_BY_NAME["drive_by_dan"]
    if is_member("gaming", kicked) and not q("gaming", "SELECT 1 FROM audit_log WHERE action='kick'"):
        a.send(pb.Packet.KICK_MEMBER_REQ, kick_member_req=pb.KickMemberRequest(uid=kicked, username="drive_by_dan",
                                                                               reason="Inactive bot account"))
        mod_res(a, "kick")
    loud = POOL_BY_NAME["loud_larry"]
    if (q1("gaming", "SELECT timed_out_until FROM members WHERE uid = ?", loud) or 0) <= NOW:
        a.send(pb.Packet.TIMEOUT_MEMBER_REQ, timeout_member_req=pb.TimeoutMemberRequest(
            uid=loud, username="loud_larry", until=NOW + 86400, reason="CAPS LOCK in voice and chat"))
        mod_res(a, "timeout")
    nicks = {U["bob"]: "Bobby 🛠", POOL_BY_NAME["zeynep_k"]: "Zeynep ✨", POOL_BY_NAME["deniz"]: "Deniz | mod",
             POOL_BY_NAME["mert.the.long.name.example"]: "Mert (AFK)", POOL_BY_NAME["battalocelot"]: "Ocelot"}
    for uid, nick in nicks.items():
        if q1("gaming", "SELECT nickname FROM members WHERE uid = ?", uid) != nick:
            a.send(pb.Packet.SET_NICKNAME_REQ, set_nickname_req=pb.SetNicknameRequest(uid=uid, nickname=nick))
            mod_res(a, "nickname")
    for s in ses.values():
        s.close()
    log(f"Daily Gaming: {SENT[0] - sent0} messages sent, {uploads} files uploaded over HTTPS, "
        f"{MODS[0] - mods0} moderation actions (ban, kick, timeout, nicknames, a moderator's delete) this run")


def live_rust(ctx):
    U = ctx.uids
    ses = {}

    def S(name):
        if name not in ses:
            ses[name] = ctx.session("rust", name)
            ses[name]._uid = U[name]
        return ses[name]

    ch = {c.name: c.id for c in S("bob").channels}
    sent0 = SENT[0]
    say_once(S("bob"), "general", "Rust 1.92 is out 🦀 time for `cargo update`")
    say_once(S("alice"), "general", "the new borrow checker errors are so much nicer")
    q1_ = say_once(S("alice"), ch["help"], "why does this not compile?\n```rust\nlet s = String::from(\"hi\");\n"
                                           "let t = s;\nprintln!(\"{s}\");\n```")
    say_once(S("bob"), ch["help"], "`s` was moved into `t`. Clone it, or borrow: `let t = &s;`", reply_to=q1_)
    say_once(S("carol"), ch["core-team"], "core sync moved to Thursday")
    say_once(S("bob"), "announcements", "Meetup next week. Details soon, bring your side projects.")
    for s in ses.values():
        s.close()
    log(f"Rustaceans: {SENT[0] - sent0} messages sent this run")


def live_design(ctx):
    U = ctx.uids
    ses = {}

    def S(name):
        if name not in ses:
            ses[name] = ctx.session("design", name)
            ses[name]._uid = U[name]
        return ses[name]

    ch = {c.name: c.id for c in S("carol").channels}
    uploads = 0
    sent0 = SENT[0]
    say_once(S("carol"), "general", "Welcome to Design Lab ✏️ post work in #critique, references in #inspiration.")
    if not bound_upload("design", ch["inspiration"], "palette-dusk.jpg"):
        h = S("carol").attachment_http()
        ids = [upload_picture(h, ch["inspiration"], "palette-dusk.jpg", 140, 1500, 1000),
               upload_picture(h, ch["inspiration"], "poster-grid.jpg", 141, 1000, 1500)]
        h.close()
        send_msg(S("carol"), ch["inspiration"], "two references for the rebrand moodboard", attachments=ids)
        uploads += 2
    say_once(S("bob"), ch["inspiration"], "love the palette on the first one")
    if not bound_upload("design", ch["critique"], "landing-v3.jpg"):
        h = S("erin").attachment_http()
        aid = upload_picture(h, ch["critique"], "landing-v3.jpg", 142, 1440, 900)
        h.close()
        send_msg(S("erin"), ch["critique"], "feedback on this landing page? v3", attachments=[aid])
        uploads += 1
    say_once(S("dave"), ch["critique"], "the hero text needs more contrast, otherwise 🔥")
    for s in ses.values():
        s.close()
    log(f"Design Lab: {SENT[0] - sent0} messages sent, {uploads} files uploaded over HTTPS this run")


# --- Output --------------------------------------------------------------------------------------------------

def write_accounts(ctx):
    st = d.read_state()
    member_of = collections.defaultdict(list)
    for c in d.COMMUNITIES:
        try:
            for (uid,) in q(c["slug"], "SELECT uid FROM members WHERE uid > 0 AND uid < ?", PLACEHOLDER_BASE):
                member_of[uid].append(c["name"])
        except sqlite3.Error:
            pass
    out = {
        "central": f"{d.CENTRAL_HOST}:{d.CENTRAL_PORT}",
        "central_cert_sha256": st.get("central", {}).get("fingerprint", ""),
        "password": d.PASSWORD,
        "accounts": [{"name": n, "uid": ctx.uids.get(n), "password": d.PASSWORD, "email": f"{n}@example.test",
                      "communities": member_of.get(ctx.uids.get(n), [])}
                     for n in d.ACCOUNTS + [e[0] for e in d.EXTRA_ACCOUNTS] if ctx.uids.get(n)],
        "communities": [{"name": c["name"], "slug": c["slug"], "owner": c["owner"], "host": c["ip"],
                         "port": d.COMMUNITY_PORT, "attachment_port": d.COMMUNITY_PORT + 3,
                         "server_id": st.get("communities", {}).get(c["slug"], {}).get("server_id"),
                         "cert_sha256": st.get("communities", {}).get(c["slug"], {}).get("fingerprint"),
                         "invite": ctx.invites.get(c["slug"], "")} for c in d.COMMUNITIES],
    }
    d.write_json(d.ACCOUNTS_JSON, out)


def summary(ctx):
    U = ctx.uids
    lines = []
    users = d.pg("SELECT count(*) FROM users")[0][0]
    fr = dict(d.pg("SELECT status, count(*) FROM friendships GROUP BY status"))
    lines.append(f"central: {users} accounts; friendships {fr}")
    for a, b in (("alice", "bob"), ("alice", "carol")):
        lo, hi = sorted((U[a], U[b]))
        n = d.pg("SELECT count(*), count(*) FILTER (WHERE edited_at > 0), count(*) FILTER (WHERE reply_to > 0) "
                 "FROM dm_messages WHERE LEAST(sender_uid, recipient_uid) = %s AND GREATEST(sender_uid, recipient_uid) = %s",
                 lo, hi)[0]
        lines.append(f"DMs {a}<->{b}: {n[0]} ({n[1]} edited, {n[2]} replies)")
    mem = d.pg("SELECT s.name, count(m.uid) FROM community_servers s LEFT JOIN community_memberships m "
               "ON m.server_id = s.id GROUP BY s.name ORDER BY s.name")
    lines.append("central memberships (real accounts): " + ", ".join(f"{n} {c}" for n, c in mem))
    lines.append("directory (public): " + ", ".join(r[0] for r in d.pg(
        "SELECT name FROM community_servers WHERE is_public ORDER BY name")))
    for c in d.COMMUNITIES:
        slug = c["slug"]
        members = q1(slug, "SELECT count(*) FROM members")
        real = q1(slug, "SELECT count(*) FROM members WHERE uid > 0 AND uid < ?", PLACEHOLDER_BASE)
        nick = q1(slug, "SELECT count(*) FROM members WHERE nickname <> ''")
        chans = q(slug, "SELECT type, count(*) FROM channels GROUP BY type")
        tmap = {0: "text", 1: "voice", 2: "categories"}
        per = q(slug, "SELECT c.name, count(m.id) FROM channels c LEFT JOIN messages m ON m.channel_id = c.id "
                      "WHERE c.type = 0 GROUP BY c.id ORDER BY c.position")
        att = q(slug, "SELECT count(*), sum(thumbnail_sizes_mask = 7), sum(placeholder <> '') FROM attachments "
                      "WHERE message_id > 0")[0]
        kinds = dict(q(slug, "SELECT kind, count(*) FROM attachments WHERE message_id > 0 GROUP BY kind"))
        kname = {0: "image", 1: "video", 2: "document", 3: "audio"}
        rl = q(slug, "SELECT name, (SELECT count(*) FROM member_roles mr WHERE mr.role_id = r.id) FROM roles r "
                     "ORDER BY position DESC")
        lines.append(f"{c['name']} (owner {c['owner']}): {members} members ({real} real accounts, "
                     f"{members - real} placeholders, {nick} nicknames); channels "
                     + ", ".join(f"{n} {tmap[t]}" for t, n in chans))
        lines.append("  messages: " + ", ".join(f"#{n} {k}" for n, k in per)
                     + f" = {sum(k for _, k in per)}; edited {q1(slug, 'SELECT count(*) FROM messages WHERE edited_at > 0')},"
                     f" replies {q1(slug, 'SELECT count(*) FROM messages WHERE reply_to > 0')}")
        lines.append(f"  attachments: {att[0]} bound ("
                     + ", ".join(f"{kname[k]} {n}" for k, n in sorted(kinds.items()))
                     + f"), {att[1] or 0} with 320/640/1280 thumbnails, {att[2] or 0} with ThumbHash")
        lines.append("  roles: " + ", ".join(f"{n} ({k})" if n != "everyone" else "@everyone" for n, k in rl)
                     + f"; overwrites {q1(slug, 'SELECT count(*) FROM channel_overwrites')}"
                     + f"; invites {q1(slug, 'SELECT count(*) FROM invites')}"
                     + f"; bans {q1(slug, 'SELECT count(*) FROM bans')}"
                     + f"; audit rows {q1(slug, 'SELECT count(*) FROM audit_log')}")
    return lines


def main(args):
    stack.require_up()
    t0 = time.time()
    ctx = Ctx()
    try:
        seed_accounts(ctx)
        seed_friends(ctx)
        seed_dms(ctx)
        structure_gaming(ctx)
        structure_rust(ctx)
        structure_design(ctx)
        fill_gaming(ctx)
        fill_rust(ctx)
        fill_design(ctx)
        live_gaming(ctx)
        live_rust(ctx)
        live_design(ctx)
        seed_live_dms(ctx)
    except (SeedError, d.PinMismatch) as e:
        print(f"seed: {e}", file=sys.stderr)
        sys.exit(1)
    try:
        top_up(ctx)
    except (SeedError, d.PinMismatch) as e:
        print(f"seed: {e}", file=sys.stderr)
        sys.exit(1)
    time.sleep(1.0)   # memberships reach central through the communities' sync worker
    write_accounts(ctx)
    for line in top_up_e2ee(ctx):
        log(line)
    for note in ctx.notes:
        log(f"note: {note}")
    log(f"done in {time.time() - t0:.1f} s. Summary:")
    for line in summary(ctx):
        print("  " + line)
    print(f"  accounts file: {d.ACCOUNTS_JSON}")


# --- Top-up ------------------------------------------------------------------------------------------------
# What was added to the fixtures after stacks were seeded. Purely additive and idempotent: it looks first
# and adds only what is missing, through the protocol, with the stack running and other clients signed
# in; nothing that exists is changed, no server is stopped, no row is rewritten. `seed` ends with it, so
# a stack made from scratch has the same.

def headless_binary():
    """The GPUI client's `decibell-headless` (it sets E2EE keys up as a client does), if it is built."""
    repo = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
    for path in (os.environ.get("DECIBELL_HEADLESS", ""),
                 os.path.join(repo, "gpui-client", "target", "release", "decibell-headless"),
                 os.path.join(repo, "gpui-client", "target", "debug", "decibell-headless")):
        if path and os.access(path, os.X_OK):
            return path
    return None


def has_e2ee_keys(uid):
    return bool(d.pg("SELECT 1 FROM e2ee_keys WHERE uid = %s LIMIT 1", uid))


def set_up_e2ee(name):
    """Identity keys for `name` with the stack's passphrase, made by a client (the keys never exist
    outside one): `decibell-headless login <name> e2ee-ready <passphrase> logout` in a throwaway profile.
    Returns None when done, else why not."""
    import shutil
    import subprocess
    import tempfile
    exe = headless_binary()
    if not exe:
        return ("decibell-headless is not built (cd gpui-client && cargo build --release -p decibell-model "
                "--features local-central --bin decibell-headless), or name it with DECIBELL_HEADLESS")
    profile = tempfile.mkdtemp(prefix="decibell-topup-")
    try:
        env = dict(os.environ, DECIBELL_CENTRAL=f"{d.CENTRAL_HOST}:{d.CENTRAL_PORT}", DECIBELL_DEV_PASSWORD=d.PASSWORD)
        r = subprocess.run([exe, "--user-data-dir", profile, "--timeout", "120", "login", name,
                            "e2ee-ready", d.E2EE_PASSPHRASE, "logout", "wait", "logged-out"],
                           env=env, capture_output=True, text=True, timeout=180)
        if r.returncode != 0 or "FAILED" in r.stdout:
            tail = " | ".join((r.stdout + r.stderr).strip().splitlines()[-3:])
            return f"decibell-headless could not set the keys up: {tail}"
        return None
    except (OSError, subprocess.SubprocessError) as e:
        return f"decibell-headless: {e}"
    finally:
        shutil.rmtree(profile, ignore_errors=True)


def top_up(ctx):
    """Adds each of `devlib.EXTRA_ACCOUNTS` that is missing: the account, its membership (joined by the
    community's open invite, as a client joins), its role, its E2EE keys, its entry in accounts.json.
    Returns what it did, for the log."""
    did = []
    for name, slug, role_name, e2ee in d.EXTRA_ACCOUNTS:
        rows = d.pg("SELECT uid FROM users WHERE username = %s", name)
        if not rows:
            r = d.register(name)
            if not r or not r.success:
                raise SeedError(f"register {name}: {r.message if r else 'no answer'}")
            rows = d.pg("SELECT uid FROM users WHERE username = %s", name)
            did.append(f"registered {name} (uid {rows[0][0]})")
        uid = ctx.uids[name] = rows[0][0]
        community = d.BY_SLUG[slug]["name"]
        member = is_member(slug, uid)
        role_id = q1(slug, "SELECT id FROM roles WHERE name = ?", role_name)
        has_role = role_id is not None and bool(q(slug, "SELECT 1 FROM member_roles WHERE uid = ? AND role_id = ?", uid, role_id))
        if not member or not has_role:
            s = owner_session(ctx, slug)
            try:
                if not member:
                    code = ensure_invite(s)          # the open invite `seed` made; made only if it is gone
                    ctx.invites.setdefault(slug, code)
                    ctx.session(slug, name, invite=code).close()
                    did.append(f"{name} joined {community}")
                if not has_role:
                    if role_id is None:
                        raise SeedError(f"{community} has no role {role_name!r} (seed the stack first)")
                    assign(s, uid, [role_id])
                    did.append(f"{name} got the role {role_name}")
            finally:
                s.close()
    return did


def top_up_e2ee(ctx):
    """E2EE keys for the extra accounts that should have them and have none. After accounts.json lists
    them: the client that makes the keys signs in from it."""
    did = []
    for name, _, _, e2ee in d.EXTRA_ACCOUNTS:
        uid = ctx.uids.get(name)
        if not e2ee or not uid or has_e2ee_keys(uid):
            continue
        why = set_up_e2ee(name)
        if why is None and has_e2ee_keys(uid):
            did.append(f"{name} has E2EE keys (passphrase {d.E2EE_PASSPHRASE!r})")
        else:
            ctx.notes.append(f"{name} has no E2EE keys yet: {why or 'central has none after the setup'}. "
                             f"Its first client sets them up (tools/perf/streambot.sh does, with the "
                             f"stack's passphrase), or run top-up again once the binary is built.")
    return did


def top_up_accounts_file(ctx):
    """accounts.json gains the entries of the extra accounts; every other byte of it stays."""
    try:
        with open(d.ACCOUNTS_JSON) as f:
            out = json.load(f)
    except (OSError, ValueError):
        raise SeedError(f"{d.ACCOUNTS_JSON} is missing: this stack was never seeded (local-stack.sh seed)")
    changed = False
    for name, slug, _, _ in d.EXTRA_ACCOUNTS:
        uid = ctx.uids.get(name)
        communities = [c["name"] for c in d.COMMUNITIES if is_member(c["slug"], uid)]
        entry = {"name": name, "uid": uid, "password": d.PASSWORD, "email": f"{name}@example.test",
                 "communities": communities}
        have = next((a for a in out["accounts"] if a.get("name") == name), None)
        if have != entry:
            if have is None:
                out["accounts"].append(entry)
            else:
                have.clear()
                have.update(entry)
            changed = True
    if changed:
        d.write_json(d.ACCOUNTS_JSON, out)
    return changed


def top_up_main(args):
    stack.require_up()
    ctx = Ctx()
    try:
        did = top_up(ctx)
        if did:
            time.sleep(1.0)   # the membership reaches central through the community's sync worker
        if top_up_accounts_file(ctx):
            did.append("accounts.json lists " + ", ".join(e[0] for e in d.EXTRA_ACCOUNTS))
        did += top_up_e2ee(ctx)
    except (SeedError, d.PinMismatch) as e:
        print(f"top-up: {e}", file=sys.stderr)
        sys.exit(1)
    for line in did:
        print(f"[top-up] {line}", flush=True)
    for note in ctx.notes:
        print(f"[top-up] note: {note}", flush=True)
    if not did:
        print("[top-up] nothing to add: " + ", ".join(
            f"{name} (uid {ctx.uids[name]}) is in {d.BY_SLUG[slug]['name']} as {role}"
            + (", with E2EE keys" if e2ee and has_e2ee_keys(ctx.uids[name]) else "")
            for name, slug, role, e2ee in d.EXTRA_ACCOUNTS), flush=True)
