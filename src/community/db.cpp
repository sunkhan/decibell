#include "db.hpp"

#include <sqlite3.h>

#include <chrono>
#include <cstdint>
#include <iostream>
#include <random>
#include <string>
#include <vector>

namespace chatproj {

namespace {

int64_t now_seconds() {
    return std::chrono::duration_cast<std::chrono::seconds>(
               std::chrono::system_clock::now().time_since_epoch())
        .count();
}

// Crockford base32 alphabet (no I, L, O, U) for human-friendly invite codes.
constexpr char kBase32[] = "0123456789ABCDEFGHJKMNPQRSTVWXYZ";

std::string random_invite_code() {
    std::random_device rd;
    std::uniform_int_distribution<int> dist(0, 31);
    std::string code;
    code.reserve(10);
    for (int i = 0; i < 10; ++i) {
        code.push_back(kBase32[dist(rd)]);
    }
    return code;
}

// Scoped sqlite3_stmt — prepared on construction, finalized on destruction.
struct Stmt {
    sqlite3_stmt* s = nullptr;
    sqlite3* db = nullptr;

    Stmt(sqlite3* d, const char* sql) : db(d) {
        if (sqlite3_prepare_v2(db, sql, -1, &s, nullptr) != SQLITE_OK) {
            std::cerr << "[DB] prepare failed for \"" << sql << "\": "
                      << sqlite3_errmsg(db) << "\n";
            s = nullptr;
        }
    }
    ~Stmt() {
        if (s) sqlite3_finalize(s);
    }
    Stmt(const Stmt&) = delete;
    Stmt& operator=(const Stmt&) = delete;

    void bind_text(int i, const std::string& v) {
        sqlite3_bind_text(s, i, v.c_str(), -1, SQLITE_TRANSIENT);
    }
    void bind_int(int i, int v) { sqlite3_bind_int(s, i, v); }
    void bind_int64(int i, int64_t v) { sqlite3_bind_int64(s, i, v); }

    int step() { return sqlite3_step(s); }

    std::string col_text(int i) const {
        const unsigned char* t = sqlite3_column_text(s, i);
        return t ? std::string(reinterpret_cast<const char*>(t)) : std::string();
    }
    int64_t col_int64(int i) const { return sqlite3_column_int64(s, i); }
    int col_int(int i) const { return sqlite3_column_int(s, i); }
};

bool exec_sql(sqlite3* db, const char* sql) {
    char* err = nullptr;
    if (sqlite3_exec(db, sql, nullptr, nullptr, &err) != SQLITE_OK) {
        std::cerr << "[DB] exec failed: \"" << sql << "\": "
                  << (err ? err : "?") << "\n";
        if (err) sqlite3_free(err);
        return false;
    }
    return true;
}

} // namespace

CommunityDb::CommunityDb() = default;

CommunityDb::~CommunityDb() {
    if (db_) {
        sqlite3_close(db_);
        db_ = nullptr;
    }
}

bool CommunityDb::open(const std::string& path,
                       const std::string& owner_username,
                       const std::string& server_name,
                       const std::string& server_description) {
    std::lock_guard<std::mutex> lock(mutex_);

    if (sqlite3_open(path.c_str(), &db_) != SQLITE_OK) {
        std::cerr << "[DB] Failed to open " << path << ": "
                  << (db_ ? sqlite3_errmsg(db_) : "?") << "\n";
        return false;
    }

    // Reasonable defaults for a small embedded DB.
    exec_sql(db_, "PRAGMA journal_mode=WAL;");
    exec_sql(db_, "PRAGMA foreign_keys=ON;");
    exec_sql(db_, "PRAGMA synchronous=NORMAL;");

    init_schema_();
    seed_if_empty_(owner_username, server_name, server_description);
    return true;
}

void CommunityDb::init_schema_() {
    exec_sql(db_,
        "CREATE TABLE IF NOT EXISTS server_meta ("
        "  key TEXT PRIMARY KEY,"
        "  value TEXT NOT NULL"
        ");");

    exec_sql(db_,
        "CREATE TABLE IF NOT EXISTS members ("
        "  username TEXT PRIMARY KEY,"
        "  joined_at INTEGER NOT NULL,"
        "  nickname TEXT NOT NULL DEFAULT ''"
        ");");

    exec_sql(db_,
        "CREATE TABLE IF NOT EXISTS invites ("
        "  code TEXT PRIMARY KEY,"
        "  created_by TEXT NOT NULL,"
        "  created_at INTEGER NOT NULL,"
        "  expires_at INTEGER NOT NULL DEFAULT 0,"
        "  max_uses INTEGER NOT NULL DEFAULT 0,"
        "  uses INTEGER NOT NULL DEFAULT 0"
        ");");

    exec_sql(db_,
        "CREATE TABLE IF NOT EXISTS channels ("
        "  id TEXT PRIMARY KEY,"
        "  name TEXT NOT NULL,"
        "  type INTEGER NOT NULL DEFAULT 0,"
        "  position INTEGER NOT NULL DEFAULT 0,"
        "  voice_bitrate_kbps INTEGER NOT NULL DEFAULT 0"
        ");");

    exec_sql(db_,
        "CREATE TABLE IF NOT EXISTS bans ("
        "  username TEXT PRIMARY KEY,"
        "  banned_at INTEGER NOT NULL,"
        "  banned_by TEXT NOT NULL DEFAULT '',"
        "  reason TEXT NOT NULL DEFAULT ''"
        ");");

    // --- v2 schema additions: persistent messages + per-channel retention ---
    migrate_to_v2_();
}

namespace {
// Returns true if the table has a column with the given name. Used to make
// column-add migrations idempotent without a full ALTER-TABLE error dance.
bool column_exists(sqlite3* db, const char* table, const char* column) {
    std::string sql = std::string("PRAGMA table_info(") + table + ");";
    Stmt q(db, sql.c_str());
    if (!q.s) return false;
    while (q.step() == SQLITE_ROW) {
        // PRAGMA table_info columns: cid(0) name(1) type(2) notnull(3) dflt(4) pk(5)
        if (q.col_text(1) == column) return true;
    }
    return false;
}
} // namespace

void CommunityDb::migrate_to_v2_() {
    // Per-channel retention columns on `channels`. 0 = keep forever.
    // Text retention governs the message row itself; attachment retentions
    // soft-delete the blob while leaving a metadata tombstone.
    struct ChannelCol { const char* name; } cols[] = {
        { "retention_days_text"     },
        { "retention_days_image"    },
        { "retention_days_video"    },
        { "retention_days_document" },
        { "retention_days_audio"    },
    };
    for (const auto& c : cols) {
        if (!column_exists(db_, "channels", c.name)) {
            std::string sql = std::string("ALTER TABLE channels ADD COLUMN ")
                              + c.name + " INTEGER NOT NULL DEFAULT 0;";
            exec_sql(db_, sql.c_str());
        }
    }

    // Persistent messages.
    exec_sql(db_,
        "CREATE TABLE IF NOT EXISTS messages ("
        "  id INTEGER PRIMARY KEY AUTOINCREMENT,"
        "  channel_id TEXT NOT NULL,"
        "  sender TEXT NOT NULL,"
        "  content TEXT NOT NULL,"
        "  timestamp INTEGER NOT NULL"
        ");");
    exec_sql(db_,
        "CREATE INDEX IF NOT EXISTS idx_messages_channel_id "
        "ON messages(channel_id, id DESC);");
    exec_sql(db_,
        "CREATE INDEX IF NOT EXISTS idx_messages_channel_ts "
        "ON messages(channel_id, timestamp);");

    // Attachments — tombstone on purge (storage_path = NULL, purged_at != 0)
    // rather than DELETE so the UI can render "file X cleaned up after N days".
    //
    // message_id=0 means "pending" — the attachment is mid-upload or the
    // uploader hasn't yet referenced it in a CHANNEL_MSG. Abandoned pending
    // rows older than 1 hour are swept by the retention loop.
    //
    // upload_status:
    //   'uploading' — bytes still arriving; do NOT serve or bind
    //   'ready'     — file is final on disk at storage_path
    // Defaults to 'ready' for backwards compatibility on fresh v3 installs
    // (so any manually-inserted rows don't get treated as pending).
    exec_sql(db_,
        "CREATE TABLE IF NOT EXISTS attachments ("
        "  id INTEGER PRIMARY KEY AUTOINCREMENT,"
        "  message_id INTEGER NOT NULL,"
        "  kind INTEGER NOT NULL,"
        "  filename TEXT NOT NULL,"
        "  mime TEXT NOT NULL DEFAULT '',"
        "  size_bytes INTEGER NOT NULL DEFAULT 0,"
        "  storage_path TEXT,"
        "  position INTEGER NOT NULL DEFAULT 0,"
        "  created_at INTEGER NOT NULL,"
        "  purged_at INTEGER NOT NULL DEFAULT 0,"
        "  upload_status TEXT NOT NULL DEFAULT 'ready',"
        "  expected_size INTEGER NOT NULL DEFAULT 0,"
        "  uploader TEXT NOT NULL DEFAULT '',"
        "  channel_id TEXT NOT NULL DEFAULT '',"
        "  FOREIGN KEY (message_id) REFERENCES messages(id) ON DELETE CASCADE"
        ");");
    exec_sql(db_,
        "CREATE INDEX IF NOT EXISTS idx_attachments_message "
        "ON attachments(message_id);");
    exec_sql(db_,
        "CREATE INDEX IF NOT EXISTS idx_attachments_kind_created "
        "ON attachments(kind, created_at) WHERE purged_at = 0;");
    // Additive migration for pre-v3 DBs that have the attachments table but
    // not the upload-lifecycle columns. CREATE TABLE above is a no-op if the
    // table exists, so we ALTER each new column in idempotently.
    struct AttachCol { const char* name; const char* ddl; };
    const AttachCol attach_cols[] = {
        { "upload_status", "upload_status TEXT NOT NULL DEFAULT 'ready'" },
        { "expected_size", "expected_size INTEGER NOT NULL DEFAULT 0" },
        { "uploader",      "uploader TEXT NOT NULL DEFAULT ''" },
        { "channel_id",    "channel_id TEXT NOT NULL DEFAULT ''" },
    };
    for (const auto& c : attach_cols) {
        if (!column_exists(db_, "attachments", c.name)) {
            std::string sql = std::string("ALTER TABLE attachments ADD COLUMN ") + c.ddl + ";";
            exec_sql(db_, sql.c_str());
        }
    }
    // For sweeping abandoned pending uploads cheaply.
    exec_sql(db_,
        "CREATE INDEX IF NOT EXISTS idx_attachments_pending "
        "ON attachments(created_at) WHERE message_id = 0;");

    // FTS5 virtual table shadowing messages.content. Populated/kept in sync
    // via triggers so search is ready the moment we ship a search UI.
    // `content='messages'` keeps FTS5 content-less (stored in the source
    // table) to halve overhead vs. a fully-materialized FTS copy.
    exec_sql(db_,
        "CREATE VIRTUAL TABLE IF NOT EXISTS messages_fts USING fts5("
        "  content, sender UNINDEXED, channel_id UNINDEXED,"
        "  content='messages', content_rowid='id'"
        ");");
    exec_sql(db_,
        "CREATE TRIGGER IF NOT EXISTS messages_ai AFTER INSERT ON messages BEGIN "
        "  INSERT INTO messages_fts(rowid, content, sender, channel_id) "
        "  VALUES (new.id, new.content, new.sender, new.channel_id);"
        "END;");
    exec_sql(db_,
        "CREATE TRIGGER IF NOT EXISTS messages_ad AFTER DELETE ON messages BEGIN "
        "  INSERT INTO messages_fts(messages_fts, rowid, content, sender, channel_id) "
        "  VALUES('delete', old.id, old.content, old.sender, old.channel_id);"
        "END;");
    exec_sql(db_,
        "CREATE TRIGGER IF NOT EXISTS messages_au AFTER UPDATE ON messages BEGIN "
        "  INSERT INTO messages_fts(messages_fts, rowid, content, sender, channel_id) "
        "  VALUES('delete', old.id, old.content, old.sender, old.channel_id);"
        "  INSERT INTO messages_fts(rowid, content, sender, channel_id) "
        "  VALUES (new.id, new.content, new.sender, new.channel_id);"
        "END;");

    set_meta_("schema_version", "3");
}

void CommunityDb::seed_if_empty_(const std::string& owner,
                                 const std::string& name,
                                 const std::string& desc) {
    // Only seed if server_meta is empty — i.e. fresh DB file.
    Stmt check(db_, "SELECT COUNT(*) FROM server_meta;");
    if (check.s && check.step() == SQLITE_ROW && check.col_int(0) > 0) {
        // Existing DB — still refresh the current name/description to match
        // whatever the operator configured via env vars on this launch,
        // since that is authoritative. Owner is NOT overwritten; ownership
        // transfers are a deliberate manual operation.
        if (!name.empty()) set_meta_("server_name", name);
        set_meta_("server_description", desc);
        return;
    }

    set_meta_("schema_version", "1");
    set_meta_("owner", owner);
    set_meta_("server_name", name);
    set_meta_("server_description", desc);

    // Owner is automatically a member.
    if (!owner.empty()) {
        Stmt ins(db_,
            "INSERT OR IGNORE INTO members(username, joined_at, nickname) "
            "VALUES(?, ?, '');");
        if (ins.s) {
            ins.bind_text(1, owner);
            ins.bind_int64(2, now_seconds());
            ins.step();
        }
    }

    // Seed the three default channels matching the prior hardcoded layout.
    struct Seed {
        const char* id;
        const char* name;
        int type;     // 0 text, 1 voice
        int position;
        int bitrate;
    };
    const Seed seeds[] = {
        { "general",       "general",       0, 0, 0  },
        { "announcements", "announcements", 0, 1, 0  },
        { "voice-lounge",  "Voice Lounge",  1, 2, 64 },
    };
    for (const auto& seed : seeds) {
        Stmt ins(db_,
            "INSERT OR IGNORE INTO channels(id, name, type, position, voice_bitrate_kbps) "
            "VALUES(?, ?, ?, ?, ?);");
        if (!ins.s) continue;
        ins.bind_text(1, seed.id);
        ins.bind_text(2, seed.name);
        ins.bind_int(3, seed.type);
        ins.bind_int(4, seed.position);
        ins.bind_int(5, seed.bitrate);
        ins.step();
    }

    std::cout << "[DB] Seeded community DB. Owner: " << owner << "\n";
}

std::string CommunityDb::get_meta_(const std::string& key) const {
    Stmt q(db_, "SELECT value FROM server_meta WHERE key=?;");
    if (!q.s) return {};
    q.bind_text(1, key);
    if (q.step() == SQLITE_ROW) return q.col_text(0);
    return {};
}

void CommunityDb::set_meta_(const std::string& key, const std::string& value) {
    Stmt q(db_,
        "INSERT INTO server_meta(key, value) VALUES(?, ?) "
        "ON CONFLICT(key) DO UPDATE SET value=excluded.value;");
    if (!q.s) return;
    q.bind_text(1, key);
    q.bind_text(2, value);
    q.step();
}

std::string CommunityDb::owner() const {
    std::lock_guard<std::mutex> lock(mutex_);
    return get_meta_("owner");
}

std::string CommunityDb::server_name() const {
    std::lock_guard<std::mutex> lock(mutex_);
    return get_meta_("server_name");
}

std::string CommunityDb::server_description() const {
    std::lock_guard<std::mutex> lock(mutex_);
    return get_meta_("server_description");
}

bool CommunityDb::is_member(const std::string& username) const {
    std::lock_guard<std::mutex> lock(mutex_);
    Stmt q(db_, "SELECT 1 FROM members WHERE username=?;");
    if (!q.s) return false;
    q.bind_text(1, username);
    return q.step() == SQLITE_ROW;
}

bool CommunityDb::add_member(const std::string& username) {
    std::lock_guard<std::mutex> lock(mutex_);
    Stmt q(db_,
        "INSERT OR IGNORE INTO members(username, joined_at, nickname) "
        "VALUES(?, ?, '');");
    if (!q.s) return false;
    q.bind_text(1, username);
    q.bind_int64(2, now_seconds());
    return q.step() == SQLITE_DONE;
}

bool CommunityDb::remove_member(const std::string& username) {
    std::lock_guard<std::mutex> lock(mutex_);
    Stmt q(db_, "DELETE FROM members WHERE username=?;");
    if (!q.s) return false;
    q.bind_text(1, username);
    return q.step() == SQLITE_DONE;
}

std::vector<DbMember> CommunityDb::list_members() const {
    std::lock_guard<std::mutex> lock(mutex_);
    std::vector<DbMember> out;
    Stmt q(db_,
        "SELECT username, joined_at, nickname FROM members "
        "ORDER BY joined_at ASC;");
    if (!q.s) return out;
    while (q.step() == SQLITE_ROW) {
        DbMember m;
        m.username = q.col_text(0);
        m.joined_at = q.col_int64(1);
        m.nickname = q.col_text(2);
        out.push_back(std::move(m));
    }
    return out;
}

bool CommunityDb::is_banned(const std::string& username) const {
    std::lock_guard<std::mutex> lock(mutex_);
    Stmt q(db_, "SELECT 1 FROM bans WHERE username=?;");
    if (!q.s) return false;
    q.bind_text(1, username);
    return q.step() == SQLITE_ROW;
}

bool CommunityDb::add_ban(const std::string& username,
                          const std::string& banned_by,
                          const std::string& reason) {
    std::lock_guard<std::mutex> lock(mutex_);
    // Remove membership and insert ban atomically.
    exec_sql(db_, "BEGIN IMMEDIATE;");
    {
        Stmt del(db_, "DELETE FROM members WHERE username=?;");
        if (del.s) {
            del.bind_text(1, username);
            del.step();
        }
    }
    bool ok = false;
    {
        Stmt ins(db_,
            "INSERT INTO bans(username, banned_at, banned_by, reason) "
            "VALUES(?, ?, ?, ?) "
            "ON CONFLICT(username) DO UPDATE SET "
            "  banned_at=excluded.banned_at, "
            "  banned_by=excluded.banned_by, "
            "  reason=excluded.reason;");
        if (ins.s) {
            ins.bind_text(1, username);
            ins.bind_int64(2, now_seconds());
            ins.bind_text(3, banned_by);
            ins.bind_text(4, reason);
            ok = (ins.step() == SQLITE_DONE);
        }
    }
    exec_sql(db_, ok ? "COMMIT;" : "ROLLBACK;");
    return ok;
}

bool CommunityDb::remove_ban(const std::string& username) {
    std::lock_guard<std::mutex> lock(mutex_);
    Stmt q(db_, "DELETE FROM bans WHERE username=?;");
    if (!q.s) return false;
    q.bind_text(1, username);
    return q.step() == SQLITE_DONE;
}

std::vector<std::string> CommunityDb::list_bans() const {
    std::lock_guard<std::mutex> lock(mutex_);
    std::vector<std::string> out;
    Stmt q(db_, "SELECT username FROM bans ORDER BY banned_at DESC;");
    if (!q.s) return out;
    while (q.step() == SQLITE_ROW) {
        out.push_back(q.col_text(0));
    }
    return out;
}

std::optional<DbInvite> CommunityDb::create_invite(const std::string& created_by,
                                                   int64_t expires_at,
                                                   int32_t max_uses) {
    std::lock_guard<std::mutex> lock(mutex_);

    // Retry a handful of times on the astronomically unlikely event of a code
    // collision (10 chars of Crockford base32 ~= 2^50 entropy).
    for (int attempt = 0; attempt < 8; ++attempt) {
        DbInvite inv;
        inv.code = random_invite_code();
        inv.created_by = created_by;
        inv.created_at = now_seconds();
        inv.expires_at = expires_at;
        inv.max_uses = max_uses;
        inv.uses = 0;

        Stmt ins(db_,
            "INSERT INTO invites(code, created_by, created_at, expires_at, max_uses, uses) "
            "VALUES(?, ?, ?, ?, ?, 0);");
        if (!ins.s) return std::nullopt;
        ins.bind_text(1, inv.code);
        ins.bind_text(2, inv.created_by);
        ins.bind_int64(3, inv.created_at);
        ins.bind_int64(4, inv.expires_at);
        ins.bind_int(5, inv.max_uses);
        int rc = ins.step();
        if (rc == SQLITE_DONE) return inv;
        if (rc != SQLITE_CONSTRAINT) {
            std::cerr << "[DB] create_invite failed: "
                      << sqlite3_errmsg(db_) << "\n";
            return std::nullopt;
        }
        // On constraint violation (duplicate code), fall through to retry.
    }
    return std::nullopt;
}

std::vector<DbInvite> CommunityDb::list_invites() const {
    std::lock_guard<std::mutex> lock(mutex_);
    std::vector<DbInvite> out;
    Stmt q(db_,
        "SELECT code, created_by, created_at, expires_at, max_uses, uses "
        "FROM invites ORDER BY created_at DESC;");
    if (!q.s) return out;
    while (q.step() == SQLITE_ROW) {
        DbInvite inv;
        inv.code = q.col_text(0);
        inv.created_by = q.col_text(1);
        inv.created_at = q.col_int64(2);
        inv.expires_at = q.col_int64(3);
        inv.max_uses = q.col_int(4);
        inv.uses = q.col_int(5);
        out.push_back(std::move(inv));
    }
    return out;
}

bool CommunityDb::revoke_invite(const std::string& code) {
    std::lock_guard<std::mutex> lock(mutex_);
    Stmt q(db_, "DELETE FROM invites WHERE code=?;");
    if (!q.s) return false;
    q.bind_text(1, code);
    if (q.step() != SQLITE_DONE) return false;
    return sqlite3_changes(db_) > 0;
}

InviteResult CommunityDb::redeem_invite(const std::string& code,
                                        const std::string& redeeming_user,
                                        DbInvite* out_invite) {
    std::lock_guard<std::mutex> lock(mutex_);

    // Ban check first — banned users can never redeem, regardless of invite.
    {
        Stmt q(db_, "SELECT 1 FROM bans WHERE username=?;");
        if (q.s) {
            q.bind_text(1, redeeming_user);
            if (q.step() == SQLITE_ROW) return InviteResult::Banned;
        }
    }

    // If already a member, the invite code is moot — treat as success and skip
    // the uses increment so invites aren't wasted on double-joins.
    {
        Stmt q(db_, "SELECT 1 FROM members WHERE username=?;");
        if (q.s) {
            q.bind_text(1, redeeming_user);
            if (q.step() == SQLITE_ROW) return InviteResult::AlreadyMember;
        }
    }

    exec_sql(db_, "BEGIN IMMEDIATE;");

    DbInvite inv;
    {
        Stmt q(db_,
            "SELECT code, created_by, created_at, expires_at, max_uses, uses "
            "FROM invites WHERE code=?;");
        if (!q.s) { exec_sql(db_, "ROLLBACK;"); return InviteResult::Unknown; }
        q.bind_text(1, code);
        if (q.step() != SQLITE_ROW) { exec_sql(db_, "ROLLBACK;"); return InviteResult::Unknown; }
        inv.code = q.col_text(0);
        inv.created_by = q.col_text(1);
        inv.created_at = q.col_int64(2);
        inv.expires_at = q.col_int64(3);
        inv.max_uses = q.col_int(4);
        inv.uses = q.col_int(5);
    }

    int64_t now = now_seconds();
    if (inv.expires_at > 0 && inv.expires_at <= now) {
        // Lazy-delete expired invites so list_invites stays clean.
        Stmt del(db_, "DELETE FROM invites WHERE code=?;");
        if (del.s) { del.bind_text(1, code); del.step(); }
        exec_sql(db_, "COMMIT;");
        return InviteResult::Expired;
    }

    if (inv.max_uses > 0 && inv.uses >= inv.max_uses) {
        Stmt del(db_, "DELETE FROM invites WHERE code=?;");
        if (del.s) { del.bind_text(1, code); del.step(); }
        exec_sql(db_, "COMMIT;");
        return InviteResult::Exhausted;
    }

    // Increment uses; delete if this redemption exhausts it.
    inv.uses += 1;
    if (inv.max_uses > 0 && inv.uses >= inv.max_uses) {
        Stmt del(db_, "DELETE FROM invites WHERE code=?;");
        if (del.s) { del.bind_text(1, code); del.step(); }
    } else {
        Stmt upd(db_, "UPDATE invites SET uses=? WHERE code=?;");
        if (upd.s) {
            upd.bind_int(1, inv.uses);
            upd.bind_text(2, code);
            upd.step();
        }
    }

    exec_sql(db_, "COMMIT;");
    if (out_invite) *out_invite = inv;
    return InviteResult::Ok;
}

std::vector<DbChannel> CommunityDb::list_channels() const {
    std::lock_guard<std::mutex> lock(mutex_);
    std::vector<DbChannel> out;
    Stmt q(db_,
        "SELECT id, name, type, position, voice_bitrate_kbps, "
        "  retention_days_text, retention_days_image, retention_days_video, "
        "  retention_days_document, retention_days_audio "
        "FROM channels ORDER BY position ASC, id ASC;");
    if (!q.s) return out;
    while (q.step() == SQLITE_ROW) {
        DbChannel c;
        c.id = q.col_text(0);
        c.name = q.col_text(1);
        c.type = q.col_int(2);
        c.position = q.col_int(3);
        c.voice_bitrate_kbps = q.col_int(4);
        c.retention_days_text     = q.col_int(5);
        c.retention_days_image    = q.col_int(6);
        c.retention_days_video    = q.col_int(7);
        c.retention_days_document = q.col_int(8);
        c.retention_days_audio    = q.col_int(9);
        out.push_back(std::move(c));
    }
    return out;
}

std::optional<DbChannel> CommunityDb::get_channel(const std::string& channel_id) const {
    std::lock_guard<std::mutex> lock(mutex_);
    Stmt q(db_,
        "SELECT id, name, type, position, voice_bitrate_kbps, "
        "  retention_days_text, retention_days_image, retention_days_video, "
        "  retention_days_document, retention_days_audio "
        "FROM channels WHERE id=?;");
    if (!q.s) return std::nullopt;
    q.bind_text(1, channel_id);
    if (q.step() != SQLITE_ROW) return std::nullopt;
    DbChannel c;
    c.id = q.col_text(0);
    c.name = q.col_text(1);
    c.type = q.col_int(2);
    c.position = q.col_int(3);
    c.voice_bitrate_kbps = q.col_int(4);
    c.retention_days_text     = q.col_int(5);
    c.retention_days_image    = q.col_int(6);
    c.retention_days_video    = q.col_int(7);
    c.retention_days_document = q.col_int(8);
    c.retention_days_audio    = q.col_int(9);
    return c;
}

bool CommunityDb::update_channel_retention(const std::string& channel_id,
                                           int32_t text_days,
                                           int32_t image_days,
                                           int32_t video_days,
                                           int32_t document_days,
                                           int32_t audio_days) {
    auto clamp = [](int32_t v) { return v < 0 ? 0 : v; };
    std::lock_guard<std::mutex> lock(mutex_);
    Stmt q(db_,
        "UPDATE channels SET "
        "  retention_days_text=?, retention_days_image=?, retention_days_video=?, "
        "  retention_days_document=?, retention_days_audio=? "
        "WHERE id=?;");
    if (!q.s) return false;
    q.bind_int(1, clamp(text_days));
    q.bind_int(2, clamp(image_days));
    q.bind_int(3, clamp(video_days));
    q.bind_int(4, clamp(document_days));
    q.bind_int(5, clamp(audio_days));
    q.bind_text(6, channel_id);
    if (q.step() != SQLITE_DONE) return false;
    return sqlite3_changes(db_) > 0;
}

int64_t CommunityDb::insert_message(const std::string& channel_id,
                                    const std::string& sender,
                                    const std::string& content,
                                    int64_t timestamp) {
    std::lock_guard<std::mutex> lock(mutex_);
    Stmt q(db_,
        "INSERT INTO messages(channel_id, sender, content, timestamp) "
        "VALUES(?, ?, ?, ?);");
    if (!q.s) return 0;
    q.bind_text(1, channel_id);
    q.bind_text(2, sender);
    q.bind_text(3, content);
    q.bind_int64(4, timestamp);
    if (q.step() != SQLITE_DONE) return 0;
    return sqlite3_last_insert_rowid(db_);
}

std::vector<DbMessage> CommunityDb::fetch_messages(const std::string& channel_id,
                                                   int64_t before_id,
                                                   int32_t limit,
                                                   bool* has_more) const {
    if (has_more) *has_more = false;
    // Cap limit server-side regardless of client input.
    if (limit <= 0) limit = 50;
    if (limit > 200) limit = 200;
    // Fetch one extra row so we can tell the caller whether more exist older
    // than the returned page without a second query.
    const int32_t fetch = limit + 1;

    std::lock_guard<std::mutex> lock(mutex_);
    std::vector<DbMessage> out;
    Stmt q(db_,
        before_id > 0
            ? "SELECT id, channel_id, sender, content, timestamp FROM messages "
              "WHERE channel_id=? AND id<? ORDER BY id DESC LIMIT ?;"
            : "SELECT id, channel_id, sender, content, timestamp FROM messages "
              "WHERE channel_id=? ORDER BY id DESC LIMIT ?;");
    if (!q.s) return out;
    q.bind_text(1, channel_id);
    int next_idx = 2;
    if (before_id > 0) {
        q.bind_int64(next_idx++, before_id);
    }
    q.bind_int(next_idx, fetch);

    while (q.step() == SQLITE_ROW) {
        DbMessage m;
        m.id = q.col_int64(0);
        m.channel_id = q.col_text(1);
        m.sender = q.col_text(2);
        m.content = q.col_text(3);
        m.timestamp = q.col_int64(4);
        out.push_back(std::move(m));
    }

    if (static_cast<int32_t>(out.size()) > limit) {
        out.pop_back();
        if (has_more) *has_more = true;
    }
    return out;
}

std::vector<DbAttachment> CommunityDb::fetch_attachments_for_messages(
    const std::vector<int64_t>& message_ids) const {
    std::vector<DbAttachment> out;
    if (message_ids.empty()) return out;

    // Build `?,?,?` placeholder list sized to input. Bounded by fetch_messages
    // cap so this never explodes.
    std::string placeholders;
    placeholders.reserve(message_ids.size() * 2);
    for (size_t i = 0; i < message_ids.size(); ++i) {
        if (i > 0) placeholders.push_back(',');
        placeholders.push_back('?');
    }
    const std::string sql =
        "SELECT id, message_id, kind, filename, mime, size_bytes, "
        "  COALESCE(storage_path, ''), position, created_at, purged_at, "
        "  upload_status, expected_size, uploader "
        "FROM attachments WHERE message_id IN (" + placeholders + ") "
        "  AND upload_status = 'ready' "
        "ORDER BY message_id ASC, position ASC;";

    std::lock_guard<std::mutex> lock(mutex_);
    Stmt q(db_, sql.c_str());
    if (!q.s) return out;
    for (size_t i = 0; i < message_ids.size(); ++i) {
        q.bind_int64(static_cast<int>(i + 1), message_ids[i]);
    }
    while (q.step() == SQLITE_ROW) {
        DbAttachment a;
        a.id = q.col_int64(0);
        a.message_id = q.col_int64(1);
        a.kind = q.col_int(2);
        a.filename = q.col_text(3);
        a.mime = q.col_text(4);
        a.size_bytes = q.col_int64(5);
        a.storage_path = q.col_text(6);
        a.position = q.col_int(7);
        a.created_at = q.col_int64(8);
        a.purged_at = q.col_int64(9);
        a.upload_status = q.col_text(10);
        a.expected_size = q.col_int64(11);
        a.uploader = q.col_text(12);
        out.push_back(std::move(a));
    }
    return out;
}

int64_t CommunityDb::insert_pending_attachment(const std::string& channel_id,
                                               int32_t kind,
                                               const std::string& filename,
                                               const std::string& mime,
                                               int64_t expected_size,
                                               const std::string& storage_path,
                                               const std::string& uploader,
                                               int32_t position) {
    std::lock_guard<std::mutex> lock(mutex_);
    Stmt q(db_,
        "INSERT INTO attachments("
        "  message_id, kind, filename, mime, size_bytes, storage_path, "
        "  position, created_at, purged_at, upload_status, expected_size, "
        "  uploader, channel_id"
        ") VALUES(0, ?, ?, ?, 0, ?, ?, ?, 0, 'uploading', ?, ?, ?);");
    if (!q.s) return 0;
    q.bind_int(1, kind);
    q.bind_text(2, filename);
    q.bind_text(3, mime);
    q.bind_text(4, storage_path);
    q.bind_int(5, position);
    q.bind_int64(6, now_seconds());
    q.bind_int64(7, expected_size);
    q.bind_text(8, uploader);
    q.bind_text(9, channel_id);
    if (q.step() != SQLITE_DONE) return 0;
    return sqlite3_last_insert_rowid(db_);
}

std::optional<DbAttachment> CommunityDb::get_attachment(int64_t attachment_id) const {
    std::lock_guard<std::mutex> lock(mutex_);
    Stmt q(db_,
        "SELECT id, message_id, kind, filename, mime, size_bytes, "
        "  COALESCE(storage_path, ''), position, created_at, purged_at, "
        "  upload_status, expected_size, uploader, channel_id "
        "FROM attachments WHERE id=?;");
    if (!q.s) return std::nullopt;
    q.bind_int64(1, attachment_id);
    if (q.step() != SQLITE_ROW) return std::nullopt;
    DbAttachment a;
    a.id = q.col_int64(0);
    a.message_id = q.col_int64(1);
    a.kind = q.col_int(2);
    a.filename = q.col_text(3);
    a.mime = q.col_text(4);
    a.size_bytes = q.col_int64(5);
    a.storage_path = q.col_text(6);
    a.position = q.col_int(7);
    a.created_at = q.col_int64(8);
    a.purged_at = q.col_int64(9);
    a.upload_status = q.col_text(10);
    a.expected_size = q.col_int64(11);
    a.uploader = q.col_text(12);
    return a;
}

bool CommunityDb::update_attachment_storage_path(int64_t attachment_id,
                                                  const std::string& path) {
    std::lock_guard<std::mutex> lock(mutex_);
    Stmt q(db_, "UPDATE attachments SET storage_path=? WHERE id=?;");
    if (!q.s) return false;
    q.bind_text(1, path);
    q.bind_int64(2, attachment_id);
    return q.step() == SQLITE_DONE && sqlite3_changes(db_) > 0;
}

bool CommunityDb::complete_attachment(int64_t attachment_id, int64_t final_size) {
    std::lock_guard<std::mutex> lock(mutex_);
    Stmt q(db_,
        "UPDATE attachments SET upload_status='ready', size_bytes=? "
        "WHERE id=? AND upload_status='uploading';");
    if (!q.s) return false;
    q.bind_int64(1, final_size);
    q.bind_int64(2, attachment_id);
    if (q.step() != SQLITE_DONE) return false;
    return sqlite3_changes(db_) > 0;
}

std::optional<std::string> CommunityDb::abort_pending_attachment(int64_t attachment_id) {
    std::lock_guard<std::mutex> lock(mutex_);
    std::string path;
    {
        Stmt q(db_,
            "SELECT COALESCE(storage_path, '') FROM attachments "
            "WHERE id=? AND upload_status='uploading';");
        if (!q.s) return std::nullopt;
        q.bind_int64(1, attachment_id);
        if (q.step() != SQLITE_ROW) return std::nullopt;
        path = q.col_text(0);
    }
    Stmt del(db_, "DELETE FROM attachments WHERE id=? AND upload_status='uploading';");
    if (!del.s) return std::nullopt;
    del.bind_int64(1, attachment_id);
    if (del.step() != SQLITE_DONE) return std::nullopt;
    if (sqlite3_changes(db_) == 0) return std::nullopt;
    return path;
}

std::vector<int64_t> CommunityDb::bind_attachments(const std::vector<int64_t>& attachment_ids,
                                                    int64_t message_id,
                                                    const std::string& channel_id,
                                                    const std::string& uploader) {
    std::vector<int64_t> bound;
    if (attachment_ids.empty()) return bound;

    std::string placeholders;
    for (size_t i = 0; i < attachment_ids.size(); ++i) {
        if (i > 0) placeholders.push_back(',');
        placeholders.push_back('?');
    }

    std::lock_guard<std::mutex> lock(mutex_);
    exec_sql(db_, "BEGIN IMMEDIATE;");
    // Collect the ids that are actually eligible for binding. A UPDATE ...
    // RETURNING would do this in one shot but we stay portable to older SQLite.
    {
        const std::string select_sql =
            "SELECT id FROM attachments "
            "WHERE id IN (" + placeholders + ") "
            "  AND upload_status='ready' "
            "  AND message_id=0 "
            "  AND uploader=? "
            "  AND channel_id=?;";
        Stmt q(db_, select_sql.c_str());
        if (!q.s) { exec_sql(db_, "ROLLBACK;"); return bound; }
        for (size_t i = 0; i < attachment_ids.size(); ++i) {
            q.bind_int64(static_cast<int>(i + 1), attachment_ids[i]);
        }
        q.bind_text(static_cast<int>(attachment_ids.size() + 1), uploader);
        q.bind_text(static_cast<int>(attachment_ids.size() + 2), channel_id);
        while (q.step() == SQLITE_ROW) {
            bound.push_back(q.col_int64(0));
        }
    }
    if (bound.empty()) { exec_sql(db_, "ROLLBACK;"); return bound; }

    // Bind them with a single UPDATE — guaranteed to match the eligibility
    // check since we're inside the transaction.
    std::string bind_placeholders;
    for (size_t i = 0; i < bound.size(); ++i) {
        if (i > 0) bind_placeholders.push_back(',');
        bind_placeholders.push_back('?');
    }
    const std::string update_sql =
        "UPDATE attachments SET message_id=? "
        "WHERE id IN (" + bind_placeholders + ");";
    Stmt upd(db_, update_sql.c_str());
    if (!upd.s) { exec_sql(db_, "ROLLBACK;"); bound.clear(); return bound; }
    upd.bind_int64(1, message_id);
    for (size_t i = 0; i < bound.size(); ++i) {
        upd.bind_int64(static_cast<int>(i + 2), bound[i]);
    }
    if (upd.step() != SQLITE_DONE) { exec_sql(db_, "ROLLBACK;"); bound.clear(); return bound; }

    // Re-number positions in binding order so the client sees them in the
    // order the uploader committed them, regardless of upload completion order.
    for (size_t i = 0; i < bound.size(); ++i) {
        Stmt pos(db_, "UPDATE attachments SET position=? WHERE id=?;");
        if (!pos.s) continue;
        pos.bind_int(1, static_cast<int>(i));
        pos.bind_int64(2, bound[i]);
        pos.step();
    }

    exec_sql(db_, "COMMIT;");
    return bound;
}

std::vector<DbAttachment> CommunityDb::list_stale_pending_attachments(int64_t cutoff_ts) const {
    std::vector<DbAttachment> out;
    std::lock_guard<std::mutex> lock(mutex_);
    Stmt q(db_,
        "SELECT id, message_id, kind, filename, mime, size_bytes, "
        "  COALESCE(storage_path, ''), position, created_at, purged_at, "
        "  upload_status, expected_size, uploader, channel_id "
        "FROM attachments WHERE message_id=0 AND created_at<?;");
    if (!q.s) return out;
    q.bind_int64(1, cutoff_ts);
    while (q.step() == SQLITE_ROW) {
        DbAttachment a;
        a.id = q.col_int64(0);
        a.message_id = q.col_int64(1);
        a.kind = q.col_int(2);
        a.filename = q.col_text(3);
        a.mime = q.col_text(4);
        a.size_bytes = q.col_int64(5);
        a.storage_path = q.col_text(6);
        a.position = q.col_int(7);
        a.created_at = q.col_int64(8);
        a.purged_at = q.col_int64(9);
        a.upload_status = q.col_text(10);
        a.expected_size = q.col_int64(11);
        a.uploader = q.col_text(12);
        out.push_back(std::move(a));
    }
    return out;
}

bool CommunityDb::delete_attachment_row(int64_t attachment_id) {
    std::lock_guard<std::mutex> lock(mutex_);
    Stmt q(db_, "DELETE FROM attachments WHERE id=?;");
    if (!q.s) return false;
    q.bind_int64(1, attachment_id);
    return q.step() == SQLITE_DONE && sqlite3_changes(db_) > 0;
}

CommunityDb::PrunedTextResult CommunityDb::prune_text_messages(
    const std::string& channel_id, int64_t cutoff_ts) {
    PrunedTextResult out;
    std::lock_guard<std::mutex> lock(mutex_);

    // Collect doomed message ids + any still-present attachment blobs that
    // need to be unlinked from disk (CASCADE drops the DB rows but not files).
    std::vector<int64_t> doomed;
    {
        Stmt q(db_,
            "SELECT id FROM messages WHERE channel_id=? AND timestamp<?;");
        if (!q.s) return out;
        q.bind_text(1, channel_id);
        q.bind_int64(2, cutoff_ts);
        while (q.step() == SQLITE_ROW) {
            doomed.push_back(q.col_int64(0));
        }
    }
    if (doomed.empty()) return out;

    {
        Stmt q(db_,
            "SELECT storage_path FROM attachments "
            "WHERE message_id IN (SELECT id FROM messages "
            "                     WHERE channel_id=? AND timestamp<?) "
            "  AND storage_path IS NOT NULL;");
        if (q.s) {
            q.bind_text(1, channel_id);
            q.bind_int64(2, cutoff_ts);
            while (q.step() == SQLITE_ROW) {
                out.unlink_paths.push_back(q.col_text(0));
            }
        }
    }

    {
        Stmt q(db_, "DELETE FROM messages WHERE channel_id=? AND timestamp<?;");
        if (q.s) {
            q.bind_text(1, channel_id);
            q.bind_int64(2, cutoff_ts);
            q.step();
        }
    }

    out.deleted_ids = std::move(doomed);
    return out;
}

std::vector<PurgedAttachmentInfo> CommunityDb::prune_attachments(
    const std::string& channel_id, int32_t kind, int64_t cutoff_ts) {
    std::vector<PurgedAttachmentInfo> out;
    std::lock_guard<std::mutex> lock(mutex_);

    // Find attachments to tombstone. Scoped by channel via the messages JOIN.
    {
        Stmt q(db_,
            "SELECT a.id, a.message_id, COALESCE(a.storage_path, '') "
            "FROM attachments a "
            "JOIN messages m ON m.id = a.message_id "
            "WHERE m.channel_id=? AND a.kind=? AND a.created_at<? "
            "  AND a.purged_at=0;");
        if (!q.s) return out;
        q.bind_text(1, channel_id);
        q.bind_int(2, kind);
        q.bind_int64(3, cutoff_ts);
        while (q.step() == SQLITE_ROW) {
            PurgedAttachmentInfo p;
            p.attachment_id = q.col_int64(0);
            p.message_id = q.col_int64(1);
            p.storage_path = q.col_text(2);
            out.push_back(std::move(p));
        }
    }
    if (out.empty()) return out;

    // Soft-delete: storage_path→NULL, purged_at→now. Single UPDATE rather
    // than a loop — attachments.id list is bounded by this channel+kind page.
    const int64_t now = now_seconds();
    std::string placeholders;
    placeholders.reserve(out.size() * 2);
    for (size_t i = 0; i < out.size(); ++i) {
        if (i > 0) placeholders.push_back(',');
        placeholders.push_back('?');
    }
    const std::string sql =
        "UPDATE attachments SET storage_path=NULL, purged_at=? "
        "WHERE id IN (" + placeholders + ");";
    Stmt q(db_, sql.c_str());
    if (q.s) {
        q.bind_int64(1, now);
        for (size_t i = 0; i < out.size(); ++i) {
            q.bind_int64(static_cast<int>(i + 2), out[i].attachment_id);
        }
        q.step();
    }
    for (auto& p : out) p.purged_at = now;
    return out;
}

} // namespace chatproj
