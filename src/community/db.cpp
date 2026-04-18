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
        "SELECT id, name, type, position, voice_bitrate_kbps "
        "FROM channels ORDER BY position ASC, id ASC;");
    if (!q.s) return out;
    while (q.step() == SQLITE_ROW) {
        DbChannel c;
        c.id = q.col_text(0);
        c.name = q.col_text(1);
        c.type = q.col_int(2);
        c.position = q.col_int(3);
        c.voice_bitrate_kbps = q.col_int(4);
        out.push_back(std::move(c));
    }
    return out;
}

} // namespace chatproj
