#include "db.hpp"

#include <sqlite3.h>

#include <algorithm>
#include <chrono>
#include <cstdint>
#include <cstdlib>
#include <iostream>
#include <random>
#include <set>
#include <string>
#include <unordered_map>
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

// Prepared-statement cache. Every query used to be sqlite3_prepare_v2'd on
// each call (a full SQL parse + planner pass), which made has_permission()
// — two statements, called once per online user per roster broadcast and
// soon once per CHANNEL_MSG — the dominant DB cost. Statements are cached
// by SQL text and handed out to Stmt; a statement that is already checked
// out (the same SQL used in a nested scope) falls back to a one-off
// prepare, so reuse can never alias a live cursor. All access happens
// under CommunityDb::mutex_ (one DB per process), and the cache is
// finalized before sqlite3_close in ~CommunityDb.
struct StmtCache {
    struct Entry { sqlite3_stmt* stmt; bool in_use; };
    std::unordered_map<std::string, Entry> by_sql;
    static constexpr size_t kMaxEntries = 512;

    void finalize_all() {
        for (auto& [sql, e] : by_sql) sqlite3_finalize(e.stmt);
        by_sql.clear();
    }
};
StmtCache& stmt_cache() {
    static StmtCache c;
    return c;
}

// Scoped sqlite3_stmt — checked out of the cache (or prepared) on
// construction; reset + released (or finalized) on destruction.
struct Stmt {
    sqlite3_stmt* s = nullptr;
    sqlite3* db = nullptr;
    StmtCache::Entry* cached = nullptr;

    Stmt(sqlite3* d, const char* sql) : db(d) {
        auto& cache = stmt_cache();
        auto it = cache.by_sql.find(sql);
        if (it != cache.by_sql.end() && !it->second.in_use) {
            cached = &it->second;
            cached->in_use = true;
            s = cached->stmt;
            return;
        }
        if (sqlite3_prepare_v2(db, sql, -1, &s, nullptr) != SQLITE_OK) {
            std::cerr << "[DB] prepare failed for \"" << sql << "\": "
                      << sqlite3_errmsg(db) << "\n";
            s = nullptr;
            return;
        }
        if (it == cache.by_sql.end() && cache.by_sql.size() < StmtCache::kMaxEntries) {
            auto ins = cache.by_sql.emplace(sql, StmtCache::Entry{s, true});
            cached = &ins.first->second;
        }
    }
    ~Stmt() {
        if (!s) return;
        if (cached) {
            sqlite3_reset(s);
            sqlite3_clear_bindings(s);
            cached->in_use = false;
        } else {
            sqlite3_finalize(s);
        }
    }
    Stmt(const Stmt&) = delete;
    Stmt& operator=(const Stmt&) = delete;

    void bind_text(int i, const std::string& v) {
        sqlite3_bind_text(s, i, v.c_str(), -1, SQLITE_TRANSIENT);
    }
    void bind_int(int i, int v) { sqlite3_bind_int(s, i, v); }
    void bind_int64(int i, int64_t v) { sqlite3_bind_int64(s, i, v); }
    // Empty = NULL so a plaintext row never stores an empty blob.
    void bind_blob_or_null(int i, const std::string& v) {
        if (v.empty()) sqlite3_bind_null(s, i);
        else sqlite3_bind_blob(s, i, v.data(), static_cast<int>(v.size()), SQLITE_TRANSIENT);
    }

    int step() { return sqlite3_step(s); }

    std::string col_text(int i) const {
        const unsigned char* t = sqlite3_column_text(s, i);
        return t ? std::string(reinterpret_cast<const char*>(t)) : std::string();
    }
    int64_t col_int64(int i) const { return sqlite3_column_int64(s, i); }
    int col_int(int i) const { return sqlite3_column_int(s, i); }
    std::string col_blob(int i) const {
        const void* p = sqlite3_column_blob(s, i);
        const int n = sqlite3_column_bytes(s, i);
        return (p && n > 0) ? std::string(static_cast<const char*>(p), static_cast<size_t>(n)) : std::string();
    }
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
        stmt_cache().finalize_all();
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
    // Wait (retrying) up to 2 s for the write lock instead of failing
    // instantly with SQLITE_BUSY. All in-process access is already
    // serialized by mutex_, so this only matters when a SECOND process
    // touches the file (operator sqlite3 CLI, a backup / WAL-copy tool):
    // without it, BEGIN IMMEDIATE (and even single writes) would fail on
    // sight, and an unchecked BEGIN then degraded into autocommit partial
    // writes (DB1). Paired with begin_tx_() below.
    sqlite3_busy_timeout(db_, 2000);

    init_schema_();
    seed_if_empty_(owner_username, server_name, server_description);
    // Versioned, one-shot: a no-op once the DB has been stamped with the
    // current seed version (see ensure_default_channels_). It used to run
    // INSERT OR IGNORE on every boot, which resurrected any default
    // channel an operator had deleted via CHANNEL_DELETE_REQ.
    ensure_default_channels_();
    // Existing DBs may pre-date the text-above-voice ordering invariant.
    normalize_channel_order_();
    // ...and the "synced only under a syncing category" one (v12).
    enforce_sync_invariant_unlocked_();
    try { owner_cache_ = std::stoll(get_meta_("owner_uid")); } catch (...) { owner_cache_ = 0; }
    return true;
}

namespace { bool column_exists(sqlite3* db, const char* table, const char* column); }

void CommunityDb::init_schema_() {
    exec_sql(db_,
        "CREATE TABLE IF NOT EXISTS server_meta ("
        "  key TEXT PRIMARY KEY,"
        "  value TEXT NOT NULL"
        ");");
    // The version this DB was at before this boot's migrations (0 = fresh).
    const int prior_version = std::atoi(get_meta_("schema_version").c_str());

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

    // --- v5: roles + permissions ---
    migrate_to_v5_roles_();

    // --- v6: per-channel overwrites + v2 permission bits ---
    migrate_to_v6_overwrites_();

    // --- v7: timeouts, server voice flags, ban expiry, slowmode, audit log ---
    migrate_to_v7_moderation_();

    // --- v8: stable user ids (Theme A) ---
    migrate_to_v8_uid_();

    // --- v9: encrypted text channels (spec 2026-09-04) ---
    migrate_to_v9_e2ee_();

    // --- v10: upload activity, so the abandoned-upload sweep goes by idle time ---
    migrate_to_v10_upload_activity_();

    // --- v11: permissions v3 (new bits, voice-mod split, peers, category sync) ---
    migrate_to_v11_permissions_v3_();

    // --- v12: per-category sync switch (off = visual grouping only) ---
    migrate_to_v12_category_sync_(prior_version);

    // --- v13: channel topics + personal invites ---
    migrate_to_v13_topics_invites_();

    // --- v14: message search (FTS index) + sender uids ---
    migrate_to_v14_search_();

    // --- v15: users by uid ---
    migrate_to_v15_uid_keys_();

    // --- v16: the token that last named each member (renames) ---
    migrate_to_v16_name_iat_();
}

void CommunityDb::migrate_to_v16_name_iat_() {
    // Renames (uid design, stage 5): a member's name follows the newest
    // token. Tokens live 24 h, so a device that kept one across a rename
    // must not rename the member back.
    if (!column_exists(db_, "members", "name_iat")) {
        if (!exec_sql(db_, "ALTER TABLE members ADD COLUMN name_iat INTEGER NOT NULL DEFAULT 0;")) {
            std::cerr << "[DB] v16 (members.name_iat) migration failed\n";
            return;
        }
    }
    set_meta_("schema_version", "16");
}

void CommunityDb::migrate_to_v15_uid_keys_() {
    // Users by uid (docs/superpowers/specs/2026-10-15-uid-identity-design.md):
    // every table that named a user by username is keyed by the account uid,
    // so a rename never orphans a row and a later holder of a name inherits
    // nothing. Names stay only where they mean "the name at the time"
    // (message and key-blob senders, which a v1 envelope binds; the audit
    // log, as history).
    //
    // A name with no known uid (a member who hasn't connected since uids
    // reached this server, a name-only ban, …) gets a negative placeholder,
    // recorded in legacy_claims; the first token with that name claims it
    // (claim_legacy). One transaction: a failure leaves v14 intact.
    if (column_exists(db_, "members", "username") && !column_exists(db_, "member_roles", "uid")) {
        if (!exec_sql(db_, "BEGIN IMMEDIATE;")) return;   // DB1
        auto run = [&](const char* sql) { return exec_sql(db_, sql); };
        bool ok =
            // Name → uid: known uids first (members win over bans)...
            run("CREATE TEMP TABLE uid_map (username TEXT PRIMARY KEY, uid INTEGER NOT NULL);") &&
            run("INSERT OR IGNORE INTO uid_map SELECT username, uid FROM members WHERE uid > 0;") &&
            run("INSERT OR IGNORE INTO uid_map SELECT username, uid FROM bans WHERE uid > 0;") &&
            // ...then a placeholder for every other name any table holds.
            run("CREATE TEMP TABLE legacy_names (username TEXT PRIMARY KEY);") &&
            run("INSERT OR IGNORE INTO legacy_names "
                "SELECT username FROM members WHERE uid <= 0 "
                "UNION SELECT username FROM bans WHERE uid <= 0 "
                "UNION SELECT username FROM member_roles "
                "UNION SELECT target_id FROM channel_overwrites WHERE target_type = 1 "
                "UNION SELECT created_by FROM invites WHERE created_by <> '' "
                "UNION SELECT for_user FROM invites WHERE for_user <> '' "
                "UNION SELECT uploader FROM attachments WHERE uploader IS NOT NULL AND uploader <> '' "
                "UNION SELECT recipient FROM channel_key_blobs "
                "UNION SELECT sender FROM channel_key_blobs "
                "UNION SELECT created_by FROM channel_key_epochs "
                "UNION SELECT sender FROM messages WHERE sender_uid = 0 "
                "UNION SELECT value FROM server_meta WHERE key = 'owner' AND value <> '';") &&
            run("DELETE FROM legacy_names WHERE username IN (SELECT username FROM uid_map);") &&
            run("INSERT INTO uid_map SELECT username, -ROW_NUMBER() OVER (ORDER BY username) FROM legacy_names;") &&

            // members: uid primary key; the name is the latest seen.
            run("CREATE TABLE members_v15 ("
                "  uid INTEGER PRIMARY KEY,"
                "  username TEXT NOT NULL,"
                "  joined_at INTEGER NOT NULL,"
                "  nickname TEXT NOT NULL DEFAULT '',"
                "  timed_out_until INTEGER NOT NULL DEFAULT 0,"
                "  server_muted INTEGER NOT NULL DEFAULT 0,"
                "  server_deafened INTEGER NOT NULL DEFAULT 0"
                ");") &&
            run("INSERT OR IGNORE INTO members_v15 "
                "SELECT CASE WHEN m.uid > 0 THEN m.uid "
                "            ELSE (SELECT uid FROM uid_map WHERE username = m.username) END, "
                "       m.username, m.joined_at, m.nickname, m.timed_out_until, m.server_muted, m.server_deafened "
                "FROM members m ORDER BY m.joined_at ASC;") &&
            run("DROP TABLE members;") &&
            run("ALTER TABLE members_v15 RENAME TO members;") &&
            run("CREATE INDEX idx_members_username ON members(username);") &&

            // member_roles: (uid, role_id), members only.
            run("CREATE TABLE member_roles_v15 ("
                "  uid INTEGER NOT NULL,"
                "  role_id INTEGER NOT NULL,"
                "  PRIMARY KEY (uid, role_id)"
                ");") &&
            run("INSERT OR IGNORE INTO member_roles_v15 "
                "SELECT u.uid, mr.role_id FROM member_roles mr JOIN uid_map u ON u.username = mr.username "
                "WHERE u.uid IN (SELECT uid FROM members);") &&
            run("DROP TABLE member_roles;") &&
            run("ALTER TABLE member_roles_v15 RENAME TO member_roles;") &&
            run("CREATE INDEX idx_member_roles_role ON member_roles(role_id);") &&

            // Member overwrites: target_id becomes the uid in decimal (role
            // rows already hold the role id that way), members only.
            run("CREATE TEMP TABLE member_overwrites AS "
                "SELECT o.channel_id, CAST(u.uid AS TEXT) AS target_id, o.allow, o.deny "
                "FROM channel_overwrites o JOIN uid_map u ON u.username = o.target_id "
                "WHERE o.target_type = 1 AND u.uid IN (SELECT uid FROM members);") &&
            run("DELETE FROM channel_overwrites WHERE target_type = 1;") &&
            run("INSERT OR IGNORE INTO channel_overwrites(channel_id, target_type, target_id, allow, deny) "
                "SELECT channel_id, 1, target_id, allow, deny FROM member_overwrites;") &&

            // bans: uid primary key; a name-only ban keeps its placeholder
            // and matches by name until claimed.
            run("CREATE TABLE bans_v15 ("
                "  uid INTEGER PRIMARY KEY,"
                "  username TEXT NOT NULL,"
                "  banned_at INTEGER NOT NULL,"
                "  banned_by_uid INTEGER NOT NULL DEFAULT 0,"
                "  banned_by TEXT NOT NULL DEFAULT '',"
                "  reason TEXT NOT NULL DEFAULT '',"
                "  expires_at INTEGER NOT NULL DEFAULT 0"
                ");") &&
            run("INSERT OR IGNORE INTO bans_v15 "
                "SELECT CASE WHEN b.uid > 0 THEN b.uid "
                "            ELSE (SELECT uid FROM uid_map WHERE username = b.username) END, "
                "       b.username, b.banned_at, "
                "       COALESCE((SELECT uid FROM uid_map WHERE username = b.banned_by), 0), "
                "       b.banned_by, b.reason, b.expires_at "
                "FROM bans b;") &&
            run("DROP TABLE bans;") &&
            run("ALTER TABLE bans_v15 RENAME TO bans;") &&
            run("CREATE INDEX idx_bans_username ON bans(username) WHERE uid < 0;") &&

            // invites: creator / personal recipient by uid (the names stay
            // as they were when the invite was made).
            run("ALTER TABLE invites ADD COLUMN created_by_uid INTEGER NOT NULL DEFAULT 0;") &&
            run("ALTER TABLE invites ADD COLUMN for_uid INTEGER NOT NULL DEFAULT 0;") &&
            run("UPDATE invites SET created_by_uid = "
                "COALESCE((SELECT uid FROM uid_map WHERE username = invites.created_by), 0);") &&
            run("UPDATE invites SET for_uid = (SELECT uid FROM uid_map WHERE username = invites.for_user) "
                "WHERE for_user <> '';") &&

            // Uploads by uid.
            run("ALTER TABLE attachments ADD COLUMN uploader_uid INTEGER NOT NULL DEFAULT 0;") &&
            run("UPDATE attachments SET uploader_uid = "
                "COALESCE((SELECT uid FROM uid_map WHERE username = attachments.uploader), 0) "
                "WHERE uploader IS NOT NULL AND uploader <> '';") &&

            // Key escrow: epochs and blobs by uid; a blob keeps the names it
            // was sealed under.
            run("ALTER TABLE channel_key_epochs ADD COLUMN created_by_uid INTEGER NOT NULL DEFAULT 0;") &&
            run("UPDATE channel_key_epochs SET created_by_uid = "
                "COALESCE((SELECT uid FROM uid_map WHERE username = channel_key_epochs.created_by), 0);") &&
            run("CREATE TABLE channel_key_blobs_v15 ("
                "  channel_id TEXT NOT NULL,"
                "  epoch INTEGER NOT NULL,"
                "  recipient_uid INTEGER NOT NULL,"
                "  recipient TEXT NOT NULL,"
                "  sender_uid INTEGER NOT NULL,"
                "  sender TEXT NOT NULL,"
                "  blob BLOB NOT NULL,"
                "  created_at INTEGER NOT NULL,"
                "  PRIMARY KEY (channel_id, epoch, recipient_uid)"
                ");") &&
            run("INSERT OR IGNORE INTO channel_key_blobs_v15 "
                "SELECT b.channel_id, b.epoch, r.uid, b.recipient, s.uid, b.sender, b.blob, b.created_at "
                "FROM channel_key_blobs b "
                "JOIN uid_map r ON r.username = b.recipient "
                "JOIN uid_map s ON s.username = b.sender;") &&
            run("DROP TABLE channel_key_blobs;") &&
            run("ALTER TABLE channel_key_blobs_v15 RENAME TO channel_key_blobs;") &&
            run("CREATE INDEX idx_channel_key_blobs_recipient ON channel_key_blobs(recipient_uid, channel_id);") &&

            // Messages v14 couldn't place.
            run("UPDATE messages SET sender_uid = (SELECT uid FROM uid_map WHERE username = messages.sender) "
                "WHERE sender_uid = 0;") &&

            // Audit: uids next to the names (history keeps the names).
            run("ALTER TABLE audit_log ADD COLUMN actor_uid INTEGER NOT NULL DEFAULT 0;") &&
            run("ALTER TABLE audit_log ADD COLUMN target_uid INTEGER NOT NULL DEFAULT 0;") &&
            run("UPDATE audit_log SET actor_uid = "
                "COALESCE((SELECT uid FROM uid_map WHERE username = audit_log.actor), 0);") &&

            // The owner.
            run("INSERT OR REPLACE INTO server_meta(key, value) "
                "SELECT 'owner_uid', CAST(uid AS TEXT) FROM uid_map "
                "WHERE username = (SELECT value FROM server_meta WHERE key = 'owner');") &&
            run("DELETE FROM server_meta WHERE key = 'owner';") &&

            // What's left to claim.
            run("CREATE TABLE legacy_claims ("
                "  username TEXT PRIMARY KEY,"
                "  uid INTEGER NOT NULL UNIQUE"
                ");") &&
            run("INSERT INTO legacy_claims SELECT username, uid FROM uid_map WHERE uid < 0;") &&
            run("INSERT OR REPLACE INTO server_meta(key, value) "
                "SELECT 'next_placeholder', CAST(COALESCE(MIN(uid), 0) - 1 AS TEXT) FROM legacy_claims;");
        int64_t placeholders = 0;
        if (ok) {
            Stmt q(db_, "SELECT COUNT(*) FROM legacy_claims;");
            if (q.s && q.step() == SQLITE_ROW) placeholders = q.col_int64(0);
        }
        exec_sql(db_, "DROP TABLE IF EXISTS temp.uid_map;");
        exec_sql(db_, "DROP TABLE IF EXISTS temp.legacy_names;");
        exec_sql(db_, "DROP TABLE IF EXISTS temp.member_overwrites;");
        exec_sql(db_, ok ? "COMMIT;" : "ROLLBACK;");
        if (!ok) {
            std::cerr << "[DB] v15 (users by uid) migration failed; rolled back\n";
            return;
        }
        std::cout << "[DB] v15: users keyed by uid (" << placeholders
                  << " legacy names left to claim)\n";
    }
    set_meta_("schema_version", "15");
}

int64_t CommunityDb::next_placeholder_unlocked_() {
    int64_t next = -1;
    const std::string v = get_meta_("next_placeholder");
    if (!v.empty()) {
        try { next = std::stoll(v); } catch (...) {}
    }
    if (next >= 0) next = -1;
    set_meta_("next_placeholder", std::to_string(next - 1));
    return next;
}

void CommunityDb::migrate_to_v14_search_() {
    // Senders by account uid (owner rule: backend identity is the uid, so
    // a rename never drops someone's older messages from a filter, an
    // edit, a delete or a ban purge). Existing rows are matched by the
    // name they were sent under, once; rows nobody holds that name for
    // stay 0 and are only ever matched by name.
    if (!column_exists(db_, "messages", "sender_uid"))
        exec_sql(db_, "ALTER TABLE messages ADD COLUMN sender_uid INTEGER NOT NULL DEFAULT 0;");
    if (get_meta_("sender_uid_v14").empty()) {
        exec_sql(db_,
            "UPDATE messages SET sender_uid = "
            "  COALESCE((SELECT uid FROM members WHERE members.username = messages.sender), 0) "
            "WHERE sender_uid = 0;");
        set_meta_("sender_uid_v14", "1");
    }
    exec_sql(db_, "CREATE INDEX IF NOT EXISTS idx_messages_sender_uid ON messages(sender_uid, id);");
    search_available_ = create_search_index_();
    set_meta_("schema_version", "14");
}

bool CommunityDb::create_search_index_() {
    // Contentless-delete FTS5 (spec 2026-10-14): no second copy of the
    // text, and a delete is a tombstone by rowid rather than a
    // re-tokenisation of the old text (that cost is why the previous
    // triggers were dropped — wipes, prunes and ban purges tokenised every
    // deleted row). Needs SQLite >= 3.43 (built in: cmake/DecibellSQLite.cmake).
    //
    // What the index holds, by version (a different stored version
    // rebuilds it once at startup):
    //   1  the text and file names
    //   2  the same with the Turkish dotless ı stored as i, so ı and i
    //      match each other (unicode61 treats ı as a letter of its own)
    constexpr const char* kIndexVersion = "2";
    static const char* const kTriggers[] = {
        "messages_fts_insert", "messages_fts_delete", "messages_fts_edit", "attachments_fts_bind" };
    auto drop_triggers = [&] {
        for (const char* t : kTriggers)
            exec_sql(db_, (std::string("DROP TRIGGER IF EXISTS ") + t + ";").c_str());
    };
    bool fts5 = false;
    if (sqlite3_libversion_number() >= 3043000) {
        Stmt q(db_, "SELECT fts5_source_id();");
        fts5 = q.s && q.step() == SQLITE_ROW;
    }
    if (!fts5) {
        std::cerr << "[DB] message search disabled: SQLite " << sqlite3_libversion()
                  << " lacks FTS5 or is older than 3.43 (contentless_delete)\n";
        // A DB moved here from a host that had the index: its triggers
        // would make every message write fail on a table this library
        // can't open. Drop them and forget the index, so a later upgrade
        // rebuilds it from scratch rather than trusting a stale one.
        drop_triggers();
        set_meta_("search_index_version", "");
        return false;
    }
    if (get_meta_("search_index_version") == kIndexVersion) {
        Stmt q(db_, "SELECT 1 FROM sqlite_master WHERE type='table' AND name='messages_fts';");
        if (q.s && q.step() == SQLITE_ROW) return true;
        // Version set but the table is gone (dropped by hand): rebuild below.
    }
    // What gets indexed for a message: its text and its unencrypted file
    // names, ı as i.
    const std::string text_of_new = "replace(new.content, 'ı', 'i')";
    auto files_of = [](const char* id) {
        return std::string("replace(COALESCE((SELECT group_concat(filename, ' ') FROM attachments "
                           "WHERE message_id = ") + id + " AND encrypted = 0), ''), 'ı', 'i')";
    };
    // Only plaintext rows of plaintext channels: a sealed row's `content` is
    // a placeholder for old clients and must never match, and an encrypted
    // channel's text isn't searched (owner decision), not even the
    // plaintext left from before its encryption was switched on (when that
    // was still possible).
    const std::string insert_trigger =
        "CREATE TRIGGER messages_fts_insert AFTER INSERT ON messages "
        "WHEN new.envelope IS NULL "
        "  AND NOT EXISTS (SELECT 1 FROM channels WHERE id = new.channel_id AND encrypted = 1) BEGIN "
        "  INSERT INTO messages_fts(rowid, content, files) VALUES (new.id, " + text_of_new + ", ''); "
        "END;";
    // An edit re-indexes the text with the current file names (or drops the
    // row if it became sealed).
    const std::string edit_trigger =
        "CREATE TRIGGER messages_fts_edit AFTER UPDATE OF content, envelope ON messages BEGIN "
        "  DELETE FROM messages_fts WHERE rowid = old.id; "
        "  INSERT INTO messages_fts(rowid, content, files) "
        "    SELECT new.id, " + text_of_new + ", " + files_of("new.id") + " "
        "    WHERE new.envelope IS NULL "
        "      AND NOT EXISTS (SELECT 1 FROM channels WHERE id = new.channel_id AND encrypted = 1); "
        "END;";
    // Attachments bind after their message is inserted (CHANNEL_MSG →
    // bind_attachments): fold the file names in then. Encrypted uploads
    // carry stand-in names and are never indexed.
    const std::string bind_trigger =
        "CREATE TRIGGER attachments_fts_bind AFTER UPDATE OF message_id ON attachments "
        "WHEN old.message_id = 0 AND new.message_id > 0 AND new.encrypted = 0 BEGIN "
        "  INSERT OR REPLACE INTO messages_fts(rowid, content, files) "
        "    SELECT m.id, replace(m.content, 'ı', 'i'), " + files_of("m.id") + " "
        "    FROM messages m WHERE m.id = new.message_id AND m.envelope IS NULL "
        "      AND NOT EXISTS (SELECT 1 FROM channels WHERE id = m.channel_id AND encrypted = 1); "
        "END;";
    const std::string backfill =
        "INSERT INTO messages_fts(rowid, content, files) "
        "  SELECT m.id, replace(m.content, 'ı', 'i'), " + files_of("m.id") + " "
        "  FROM messages m WHERE m.envelope IS NULL "
        "    AND m.channel_id NOT IN (SELECT id FROM channels WHERE encrypted = 1);";

    if (!exec_sql(db_, "BEGIN IMMEDIATE;")) return false;   // DB1
    drop_triggers();
    bool ok =
        exec_sql(db_, "DROP TABLE IF EXISTS messages_fts;") &&
        exec_sql(db_,
            "CREATE VIRTUAL TABLE messages_fts USING fts5("
            "  content, files, content='', contentless_delete=1,"
            "  tokenize='unicode61 remove_diacritics 2');") &&
        exec_sql(db_, insert_trigger.c_str()) &&
        exec_sql(db_,
            "CREATE TRIGGER messages_fts_delete AFTER DELETE ON messages BEGIN "
            "  DELETE FROM messages_fts WHERE rowid = old.id; "
            "END;") &&
        exec_sql(db_, edit_trigger.c_str()) &&
        exec_sql(db_, bind_trigger.c_str()) &&
        exec_sql(db_, backfill.c_str());
    if (ok) {
        set_meta_("search_index_version", kIndexVersion);
        exec_sql(db_, "DELETE FROM server_meta WHERE key = 'search_fts_v14';");   // version 1's flag
    }
    exec_sql(db_, ok ? "COMMIT;" : "ROLLBACK;");
    if (!ok) {
        std::cerr << "[DB] message search disabled: could not create the FTS5 index "
                     "(SQLite built without FTS5?)\n";
        return false;
    }
    std::cout << "[DB] message search index built\n";
    return true;
}

void CommunityDb::migrate_to_v13_topics_invites_() {
    if (!column_exists(db_, "channels", "topic"))
        exec_sql(db_, "ALTER TABLE channels ADD COLUMN topic TEXT NOT NULL DEFAULT '';");
    if (!column_exists(db_, "invites", "for_user"))
        exec_sql(db_, "ALTER TABLE invites ADD COLUMN for_user TEXT NOT NULL DEFAULT '';");
    set_meta_("schema_version", "13");
}

void CommunityDb::migrate_to_v12_category_sync_(int prior_version) {
    if (!column_exists(db_, "channels", "category_sync"))
        exec_sql(db_, "ALTER TABLE channels ADD COLUMN category_sync INTEGER NOT NULL DEFAULT 0;");
    if (get_meta_("category_sync_v12").empty()) {
        // Categories start as plain groups — that's how every server behaved
        // before permissions v3. Only a DB that already ran v11 live had
        // categories whose rows applied to their channels; keep exactly
        // those applying (enforce_sync_invariant_unlocked_ unsyncs the rest,
        // which followed nothing).
        if (prior_version >= 11) {
            exec_sql(db_,
                "UPDATE channels SET category_sync = 1 "
                "WHERE type = 2 AND id IN (SELECT DISTINCT channel_id FROM channel_overwrites);");
        }
        set_meta_("category_sync_v12", "1");
    }
    set_meta_("schema_version", "12");
}

void CommunityDb::migrate_to_v11_permissions_v3_() {
    if (!column_exists(db_, "roles", "manage_each_other"))
        exec_sql(db_, "ALTER TABLE roles ADD COLUMN manage_each_other INTEGER NOT NULL DEFAULT 0;");
    // New channels follow their category; existing rows are fixed up once
    // below (only channels without rows of their own start synced).
    const bool adding_synced = !column_exists(db_, "channels", "perm_synced");
    if (adding_synced)
        exec_sql(db_, "ALTER TABLE channels ADD COLUMN perm_synced INTEGER NOT NULL DEFAULT 1;");
    if (!column_exists(db_, "messages", "suppress_embeds"))
        exec_sql(db_, "ALTER TABLE messages ADD COLUMN suppress_embeds INTEGER NOT NULL DEFAULT 0;");
    if (get_meta_("perm_bits_v11").empty()) {
        if (!exec_sql(db_, "BEGIN IMMEDIATE;")) return;   // DB1
        bool ok = true;
        // Everyone keeps doing what they could before: the new ordinary-
        // member bits go to `everyone`...
        {
            Stmt q(db_, "UPDATE roles SET permissions = permissions | ? WHERE is_default = 1;");
            ok = q.s != nullptr;
            if (ok) { q.bind_int64(1, static_cast<int64_t>(perms::kV3Bits)); ok = q.step() == SQLITE_DONE; }
        }
        // ...and whoever held VOICE_MODERATE (now MUTE_MEMBERS, same bit)
        // also gets DEAFEN / MOVE, in roles and in overwrite allow / deny.
        const int64_t old_bit = static_cast<int64_t>(perms::kMuteMembers);
        const int64_t split = static_cast<int64_t>(perms::kDeafenMembers | perms::kMoveMembers);
        for (const char* sql : {
                 "UPDATE roles SET permissions = permissions | ?2 WHERE (permissions & ?1) != 0;",
                 "UPDATE channel_overwrites SET allow = allow | ?2 WHERE (allow & ?1) != 0;",
                 "UPDATE channel_overwrites SET deny = deny | ?2 WHERE (deny & ?1) != 0;" }) {
            if (!ok) break;
            Stmt q(db_, sql);
            ok = q.s != nullptr;
            if (ok) { q.bind_int64(1, old_bit); q.bind_int64(2, split); ok = q.step() == SQLITE_DONE; }
        }
        if (ok && adding_synced) {
            ok = exec_sql(db_,
                "UPDATE channels SET perm_synced = 0 "
                "WHERE id IN (SELECT DISTINCT channel_id FROM channel_overwrites);");
        }
        if (ok) set_meta_("perm_bits_v11", "1");
        exec_sql(db_, ok ? "COMMIT;" : "ROLLBACK;");
        if (!ok) return;
    }
    set_meta_("schema_version", "11");
}

void CommunityDb::migrate_to_v10_upload_activity_() {
    // When a PATCH last landed. 0 = none yet; the sweep then falls back
    // to created_at.
    if (!column_exists(db_, "attachments", "last_activity_at"))
        exec_sql(db_, "ALTER TABLE attachments ADD COLUMN last_activity_at INTEGER NOT NULL DEFAULT 0;");
    set_meta_("schema_version", "10");
}

void CommunityDb::migrate_to_v9_e2ee_() {
    if (!column_exists(db_, "channels", "encrypted"))
        exec_sql(db_, "ALTER TABLE channels ADD COLUMN encrypted INTEGER NOT NULL DEFAULT 0;");
    // The sealed body of a message in an encrypted channel; NULL = plaintext.
    if (!column_exists(db_, "messages", "envelope"))
        exec_sql(db_, "ALTER TABLE messages ADD COLUMN envelope BLOB;");
    // Key escrow: epochs per channel and one opaque blob per (epoch,
    // recipient), sealed by a member to a member. The server never holds
    // a key.
    exec_sql(db_,
        "CREATE TABLE IF NOT EXISTS channel_key_epochs ("
        "  channel_id TEXT NOT NULL,"
        "  epoch INTEGER NOT NULL,"
        "  created_by TEXT NOT NULL,"
        "  created_at INTEGER NOT NULL,"
        "  PRIMARY KEY (channel_id, epoch)"
        ");");
    exec_sql(db_,
        "CREATE TABLE IF NOT EXISTS channel_key_blobs ("
        "  channel_id TEXT NOT NULL,"
        "  epoch INTEGER NOT NULL,"
        "  recipient TEXT NOT NULL,"
        "  sender TEXT NOT NULL,"
        "  blob BLOB NOT NULL,"
        "  created_at INTEGER NOT NULL,"
        "  PRIMARY KEY (channel_id, epoch, recipient)"
        ");");
    exec_sql(db_,
        "CREATE INDEX IF NOT EXISTS idx_channel_key_blobs_recipient "
        "ON channel_key_blobs(recipient, channel_id);");
    // Encrypted-channel uploads: ciphertext blobs whose real metadata
    // lives in the message envelope.
    if (!column_exists(db_, "attachments", "encrypted"))
        exec_sql(db_, "ALTER TABLE attachments ADD COLUMN encrypted INTEGER NOT NULL DEFAULT 0;");
    set_meta_("schema_version", "9");
}

void CommunityDb::migrate_to_v8_uid_() {
    // From v15 on, uid is the primary key of both tables.
    if (column_exists(db_, "member_roles", "uid")) return;
    if (!column_exists(db_, "members", "uid"))
        exec_sql(db_, "ALTER TABLE members ADD COLUMN uid INTEGER NOT NULL DEFAULT 0;");
    if (!column_exists(db_, "bans", "uid"))
        exec_sql(db_, "ALTER TABLE bans ADD COLUMN uid INTEGER NOT NULL DEFAULT 0;");
    exec_sql(db_, "CREATE INDEX IF NOT EXISTS idx_bans_uid ON bans(uid) WHERE uid > 0;");
    // Identity is one row per central uid. The partial unique index enforces
    // that going forward (many pre-Theme-A rows legitimately share uid=0, so
    // it is scoped to uid<>0). Creation fails only if a legacy DB already
    // holds a duplicate nonzero uid from before the C2 fix — log and carry
    // on; get_member_by_uid still resolves deterministically via LIMIT 1.
    if (!exec_sql(db_, "CREATE UNIQUE INDEX IF NOT EXISTS idx_members_uid ON members(uid) WHERE uid <> 0;")) {
        std::cerr << "[DB] idx_members_uid not created (pre-existing duplicate uid?); "
                     "uid resolution falls back to LIMIT 1\n";
    }
    set_meta_("schema_version", "8");
}

void CommunityDb::migrate_to_v7_moderation_() {
    struct Col { const char* table; const char* name; const char* ddl; } cols[] = {
        { "members",  "timed_out_until",  "timed_out_until INTEGER NOT NULL DEFAULT 0" },
        { "members",  "server_muted",     "server_muted INTEGER NOT NULL DEFAULT 0" },
        { "members",  "server_deafened",  "server_deafened INTEGER NOT NULL DEFAULT 0" },
        { "bans",     "expires_at",       "expires_at INTEGER NOT NULL DEFAULT 0" },
        { "channels", "slowmode_seconds", "slowmode_seconds INTEGER NOT NULL DEFAULT 0" },
    };
    for (const auto& c : cols) {
        if (!column_exists(db_, c.table, c.name)) {
            std::string sql = std::string("ALTER TABLE ") + c.table + " ADD COLUMN " + c.ddl + ";";
            exec_sql(db_, sql.c_str());
        }
    }
    exec_sql(db_,
        "CREATE TABLE IF NOT EXISTS audit_log ("
        "  id INTEGER PRIMARY KEY AUTOINCREMENT,"
        "  ts INTEGER NOT NULL,"
        "  actor TEXT NOT NULL,"
        "  action TEXT NOT NULL,"
        "  target TEXT NOT NULL DEFAULT '',"
        "  channel_id TEXT NOT NULL DEFAULT '',"
        "  details TEXT NOT NULL DEFAULT ''"
        ");");
    exec_sql(db_, "CREATE INDEX IF NOT EXISTS idx_audit_ts ON audit_log(ts);");
    set_meta_("schema_version", "7");
}

void CommunityDb::migrate_to_v6_overwrites_() {
    exec_sql(db_,
        "CREATE TABLE IF NOT EXISTS channel_overwrites ("
        "  channel_id TEXT NOT NULL,"
        "  target_type INTEGER NOT NULL,"
        "  target_id TEXT NOT NULL,"
        "  allow INTEGER NOT NULL DEFAULT 0,"
        "  deny INTEGER NOT NULL DEFAULT 0,"
        "  PRIMARY KEY (channel_id, target_type, target_id)"
        ");");
    // Grant the v2 bits (VIEW_CHANNEL / READ_HISTORY / ATTACH_FILES) to an
    // existing `everyone` exactly once, so an upgraded server keeps
    // behaving as it did until an operator tightens something. (A fresh
    // server seeds them via kDefaultEveryone.)
    if (get_meta_("perm_bits_v6").empty()) {
        Stmt q(db_, "UPDATE roles SET permissions = permissions | ? WHERE is_default = 1;");
        if (q.s) {
            q.bind_int64(1, static_cast<int64_t>(perms::kV2Bits));
            q.step();
        }
        set_meta_("perm_bits_v6", "1");
    }
    set_meta_("schema_version", "6");
}

void CommunityDb::migrate_to_v5_roles_() {
    exec_sql(db_,
        "CREATE TABLE IF NOT EXISTS roles ("
        "  id INTEGER PRIMARY KEY AUTOINCREMENT,"
        "  name TEXT NOT NULL,"
        "  color INTEGER NOT NULL DEFAULT 0,"
        "  position INTEGER NOT NULL DEFAULT 0,"
        "  permissions INTEGER NOT NULL DEFAULT 0,"
        "  is_default INTEGER NOT NULL DEFAULT 0"
        ");");
    exec_sql(db_,
        "CREATE TABLE IF NOT EXISTS member_roles ("
        "  username TEXT NOT NULL,"
        "  role_id INTEGER NOT NULL,"
        "  PRIMARY KEY (username, role_id)"
        ");");
    exec_sql(db_,
        "CREATE INDEX IF NOT EXISTS idx_member_roles_role "
        "ON member_roles(role_id);");

    // Seed the default `everyone` role exactly once. Position 0 is
    // reserved for it; it's implicit on every member and undeletable.
    {
        Stmt check(db_, "SELECT COUNT(*) FROM roles WHERE is_default=1;");
        if (check.s && check.step() == SQLITE_ROW && check.col_int(0) == 0) {
            Stmt ins(db_,
                "INSERT INTO roles(name, color, position, permissions, is_default) "
                "VALUES('everyone', 0, 0, ?, 1);");
            if (ins.s) {
                ins.bind_int64(1, static_cast<int64_t>(perms::kDefaultEveryone));
                ins.step();
            }
        }
    }
    set_meta_("schema_version", "5");
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
        "  timestamp INTEGER NOT NULL,"
        "  edited_at INTEGER NOT NULL DEFAULT 0,"
        "  reply_to INTEGER NOT NULL DEFAULT 0"
        ");");
    // edited_at / reply_to added later — backfill on existing message tables.
    if (!column_exists(db_, "messages", "edited_at"))
        exec_sql(db_, "ALTER TABLE messages ADD COLUMN edited_at INTEGER NOT NULL DEFAULT 0;");
    if (!column_exists(db_, "messages", "reply_to"))
        exec_sql(db_, "ALTER TABLE messages ADD COLUMN reply_to INTEGER NOT NULL DEFAULT 0;");
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
    // No FK on message_id: pending uploads carry message_id=0 as a "not yet
    // bound" sentinel, which would violate any FK to messages(id). We handle
    // orphan cleanup ourselves in prune_text_messages and the
    // pending-uploads sweep, so the FK isn't pulling weight here anyway.
    exec_sql(db_,
        "CREATE TABLE IF NOT EXISTS attachments ("
        "  id INTEGER PRIMARY KEY AUTOINCREMENT,"
        "  message_id INTEGER NOT NULL DEFAULT 0,"
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
        "  width INTEGER NOT NULL DEFAULT 0,"
        "  height INTEGER NOT NULL DEFAULT 0,"
        "  thumbnail_size_bytes INTEGER NOT NULL DEFAULT 0,"
        "  thumbnail_sizes_mask INTEGER NOT NULL DEFAULT 0,"
        "  duration_ms INTEGER NOT NULL DEFAULT 0"
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
        { "width",         "width INTEGER NOT NULL DEFAULT 0" },
        { "height",        "height INTEGER NOT NULL DEFAULT 0" },
        { "thumbnail_size_bytes",
                           "thumbnail_size_bytes INTEGER NOT NULL DEFAULT 0" },
        { "thumbnail_sizes_mask",
                           "thumbnail_sizes_mask INTEGER NOT NULL DEFAULT 0" },
        { "duration_ms",   "duration_ms INTEGER NOT NULL DEFAULT 0" },
        { "placeholder",   "placeholder TEXT NOT NULL DEFAULT ''" },
    };
    for (const auto& c : attach_cols) {
        if (!column_exists(db_, "attachments", c.name)) {
            std::string sql = std::string("ALTER TABLE attachments ADD COLUMN ") + c.ddl + ";";
            exec_sql(db_, sql.c_str());
        }
    }
    // Backfill: rows that were uploaded under the legacy single-size
    // thumbnail scheme have their bytes counted but no bit set in the
    // new mask. Mark them as having the 320 px size (bit 0) so the
    // size-aware GET handler can route to the legacy ".thumb.jpg"
    // file. Idempotent — re-running just touches the same rows.
    exec_sql(db_,
        "UPDATE attachments SET thumbnail_sizes_mask = 1 "
        "WHERE thumbnail_size_bytes > 0 AND thumbnail_sizes_mask = 0;");
    // For sweeping abandoned pending uploads cheaply.
    exec_sql(db_,
        "CREATE INDEX IF NOT EXISTS idx_attachments_pending "
        "ON attachments(created_at) WHERE message_id = 0;");
    // wipe_channel / delete_channel filter attachments by channel_id
    // (full scans without this). Rows from before channel_id existed
    // have '' and were orphaned by wipe/delete — backfill from the
    // bound message once; idempotent.
    exec_sql(db_,
        "CREATE INDEX IF NOT EXISTS idx_attachments_channel "
        "ON attachments(channel_id);");
    exec_sql(db_,
        "UPDATE attachments SET channel_id = "
        "  (SELECT channel_id FROM messages WHERE messages.id = attachments.message_id) "
        "WHERE channel_id = '' AND message_id > 0 "
        "  AND EXISTS (SELECT 1 FROM messages WHERE messages.id = attachments.message_id);");

    // Message search (FTS5) is NOT maintained yet (P6): the previous
    // schema mirrored every message into a content-less FTS5 table via
    // AFTER INSERT/DELETE/UPDATE triggers, which doubled every write and
    // made wipes / prunes / ban purges tokenise and delete row by row —
    // for a search UI that doesn't exist. Drop the triggers and the table
    // once; when search ships, recreate them and backfill with
    //   INSERT INTO messages_fts(messages_fts) VALUES('rebuild');
    if (get_meta_("fts_dropped_v8").empty()) {
        exec_sql(db_, "DROP TRIGGER IF EXISTS messages_ai;");
        exec_sql(db_, "DROP TRIGGER IF EXISTS messages_ad;");
        exec_sql(db_, "DROP TRIGGER IF EXISTS messages_au;");
        exec_sql(db_, "DROP TABLE IF EXISTS messages_fts;");
        set_meta_("fts_dropped_v8", "1");
    }

    set_meta_("schema_version", "3");

    // --- v4: drop FK + NOT NULL on attachments.message_id ---
    //
    // Earlier schemas declared `message_id INTEGER NOT NULL` plus a
    // `FOREIGN KEY (message_id) REFERENCES messages(id) ON DELETE CASCADE`.
    // The pending-upload flow uses message_id=0 as "not yet bound to a
    // message", which violates that FK and rejected every /attachments/init
    // with a 500. Recreate the table without the FK; orphan cleanup is now
    // explicit in prune_text_messages and the pending-uploads sweep.
    //
    // Detect via the literal CREATE TABLE text in sqlite_master rather than
    // PRAGMA foreign_key_list — the PRAGMA returns no rows in some sqlite
    // builds depending on PRAGMA foreign_keys state, which silently
    // skipped this migration in the field.
    {
        std::string create_sql;
        {
            Stmt q(db_,
                "SELECT sql FROM sqlite_master "
                "WHERE type='table' AND name='attachments';");
            if (q.s && q.step() == SQLITE_ROW) {
                create_sql = q.col_text(0);
            }
        }
        const bool has_fk =
            create_sql.find("FOREIGN KEY") != std::string::npos ||
            create_sql.find("foreign key") != std::string::npos ||
            create_sql.find("REFERENCES")  != std::string::npos;

        if (has_fk) {
            std::cerr << "[DB] migrating attachments table to v4 "
                         "(dropping FK on message_id)\n";

            // SQLite forbids changing PRAGMA foreign_keys mid-transaction —
            // toggle it before BEGIN, restore after COMMIT. Bail out
            // immediately on any failure so we don't leave the table in
            // half-renamed state.
            auto must = [&](const char* sql) -> bool {
                if (!exec_sql(db_, sql)) {
                    std::cerr << "[DB] v4 migration aborted at: " << sql << "\n";
                    return false;
                }
                return true;
            };

            if (!must("PRAGMA foreign_keys=OFF;")) return;
            // Wipe any leftover from a prior aborted migration attempt so
            // the CREATE below doesn't fail with "table already exists".
            // Safe in the no-leftover case — DROP IF EXISTS is a no-op.
            must("DROP TABLE IF EXISTS attachments_v4;");
            if (!must("BEGIN;")) return;
            // The v4 rebuild preserves every post-v2 column we've added
            // since — including width/height, back-filled by the ALTER
            // loop above. That way the rebuild's INSERT doesn't silently
            // drop fresh columns and we don't have to chase schema drift
            // across two launches.
            bool ok =
                must("CREATE TABLE attachments_v4 ("
                     "  id INTEGER PRIMARY KEY AUTOINCREMENT,"
                     "  message_id INTEGER NOT NULL DEFAULT 0,"
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
                     "  width INTEGER NOT NULL DEFAULT 0,"
                     "  height INTEGER NOT NULL DEFAULT 0,"
                     "  thumbnail_size_bytes INTEGER NOT NULL DEFAULT 0,"
                     "  thumbnail_sizes_mask INTEGER NOT NULL DEFAULT 0,"
                     "  duration_ms INTEGER NOT NULL DEFAULT 0,"
                     "  placeholder TEXT NOT NULL DEFAULT ''"
                     ");") &&
                must("INSERT INTO attachments_v4 "
                     "  (id, message_id, kind, filename, mime, size_bytes, "
                     "   storage_path, position, created_at, purged_at, "
                     "   upload_status, expected_size, uploader, channel_id, "
                     "   width, height, thumbnail_size_bytes, thumbnail_sizes_mask, "
                     "   duration_ms, placeholder) "
                     "SELECT id, message_id, kind, filename, mime, size_bytes, "
                     "       storage_path, position, created_at, purged_at, "
                     "       upload_status, expected_size, uploader, channel_id, "
                     "       width, height, thumbnail_size_bytes, thumbnail_sizes_mask, "
                     "       duration_ms, placeholder "
                     "FROM attachments;") &&
                must("DROP TABLE attachments;") &&
                must("ALTER TABLE attachments_v4 RENAME TO attachments;");

            if (!ok) {
                exec_sql(db_, "ROLLBACK;");
                exec_sql(db_, "PRAGMA foreign_keys=ON;");
                return;
            }
            if (!must("COMMIT;")) {
                exec_sql(db_, "ROLLBACK;");
                exec_sql(db_, "PRAGMA foreign_keys=ON;");
                return;
            }
            must("PRAGMA foreign_keys=ON;");

            // DROP TABLE drops indices too — recreate.
            exec_sql(db_,
                "CREATE INDEX IF NOT EXISTS idx_attachments_message "
                "ON attachments(message_id);");
            exec_sql(db_,
                "CREATE INDEX IF NOT EXISTS idx_attachments_kind_created "
                "ON attachments(kind, created_at) WHERE purged_at = 0;");
            exec_sql(db_,
                "CREATE INDEX IF NOT EXISTS idx_attachments_pending "
                "ON attachments(created_at) WHERE message_id = 0;");

            std::cerr << "[DB] attachments table migrated to v4 (no FK)\n";
        }
    }
    set_meta_("schema_version", "4");
}

void CommunityDb::seed_if_empty_(const std::string& owner,
                                 const std::string& name,
                                 const std::string& desc) {
    // Freshness is judged by the absence of the `owner_uid` meta key — NOT
    // by COUNT(*) on server_meta. init_schema_ runs before seeding and its
    // migrations already stamp schema_version into server_meta, so a
    // count-based check sees a "non-empty" table on a brand-new file and
    // skips seeding entirely: the DB comes up ownerless, every owner-gated
    // feature is dead, and the owner can't even join (membership requires
    // an invite only the owner could create). This also repairs DBs
    // created while that bug was live, the first time they boot with
    // DECIBELL_OWNER_USERNAME set.
    if (!get_meta_("owner_uid").empty()) {
        // Existing DB: the DB is the source of truth for name/description
        // (they're editable in-app via SERVER_UPDATE_REQ); env vars only
        // fill a still-empty name. Owner is never overwritten here —
        // ownership transfers go through TRANSFER_OWNERSHIP_REQ.
        if (get_meta_("server_name").empty() && !name.empty()) set_meta_("server_name", name);
        return;
    }
    if (owner.empty()) {
        // Nothing to seed with (main() normally refuses to start a fresh
        // DB without an owner; this guards the repair path).
        return;
    }

    // The operator names the owner, so their uid isn't known until their
    // token first authenticates: the owner (and their member row) start
    // under a placeholder that claim_legacy rewrites then.
    const int64_t placeholder = next_placeholder_unlocked_();
    {
        Stmt q(db_, "INSERT OR REPLACE INTO legacy_claims(username, uid) VALUES(?, ?);");
        if (q.s) { q.bind_text(1, owner); q.bind_int64(2, placeholder); q.step(); }
    }
    // schema_version is stamped by the migrations in init_schema_ —
    // don't overwrite it here.
    set_meta_("owner_uid", std::to_string(placeholder));
    set_meta_("server_name", name);
    set_meta_("server_description", desc);

    // Owner is automatically a member.
    {
        Stmt ins(db_,
            "INSERT OR IGNORE INTO members(uid, username, joined_at, nickname) "
            "VALUES(?, ?, ?, '');");
        if (ins.s) {
            ins.bind_int64(1, placeholder);
            ins.bind_text(2, owner);
            ins.bind_int64(3, now_seconds());
            ins.step();
        }
    }

    ensure_default_channels_();

    std::cout << "[DB] Seeded community DB. Owner: " << owner << "\n";
}

void CommunityDb::ensure_default_channels_() {
    // Canonical default channel set, applied exactly once per seed
    // version and recorded in server_meta. Deleted defaults must stay
    // deleted across restarts — a CHANNEL_DELETE_REQ is an operator
    // decision, not a drift to heal — so this is NOT an every-boot
    // INSERT OR IGNORE:
    //   - fresh DB (no channels yet): insert the whole set, stamp.
    //   - DB that pre-dates the stamp (upgraded server): the operator's
    //     channel list is authoritative; stamp without inserting.
    //   - future seed additions: bump kSeedChannelsVersion and insert
    //     only the NEW ids in a targeted branch below.
    constexpr const char* kSeedChannelsVersion = "1";
    if (get_meta_("seed_channels_version") == kSeedChannelsVersion) return;

    bool has_channels = false;
    {
        Stmt q(db_, "SELECT 1 FROM channels LIMIT 1;");
        if (q.s && q.step() == SQLITE_ROW) has_channels = true;
    }
    if (has_channels) {
        set_meta_("seed_channels_version", kSeedChannelsVersion);
        return;
    }

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
        { "voice-lounge-2","Voice Lounge 2",1, 3, 64 },
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
    set_meta_("seed_channels_version", kSeedChannelsVersion);
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
    if (key == "owner_uid") {
        try { owner_cache_ = std::stoll(value); } catch (...) { owner_cache_ = 0; }
        invalidate_perm_cache_();
    }
}

int64_t CommunityDb::owner_uid() const {
    std::lock_guard<std::mutex> lock(mutex_);
    return owner_cache_;
}

void CommunityDb::invalidate_perm_cache_() {
    perm_cache_.clear();
    channel_perm_cache_.clear();
}

void CommunityDb::invalidate_user_perms_(int64_t uid) {
    perm_cache_.erase(uid);
    channel_perm_cache_.erase(uid);
}

int64_t CommunityDb::central_server_id() const {
    std::lock_guard<std::mutex> lock(mutex_);
    auto s = get_meta_("central_server_id");
    if (s.empty()) return 0;
    try {
        return std::stoll(s);
    } catch (...) {
        return 0;
    }
}

void CommunityDb::set_central_server_id(int64_t id) {
    std::lock_guard<std::mutex> lock(mutex_);
    set_meta_("central_server_id", std::to_string(id));
}

std::string CommunityDb::central_cert_fingerprint() const {
    std::lock_guard<std::mutex> lock(mutex_);
    return get_meta_("central_cert_fingerprint");
}

void CommunityDb::set_central_cert_fingerprint(const std::string& fp) {
    std::lock_guard<std::mutex> lock(mutex_);
    set_meta_("central_cert_fingerprint", fp);
}

std::string CommunityDb::server_name() const {
    std::lock_guard<std::mutex> lock(mutex_);
    return get_meta_("server_name");
}

std::string CommunityDb::server_description() const {
    std::lock_guard<std::mutex> lock(mutex_);
    return get_meta_("server_description");
}

void CommunityDb::set_server_meta(const std::string& name, const std::string& description) {
    std::lock_guard<std::mutex> lock(mutex_);
    set_meta_("server_name", name);
    set_meta_("server_description", description);
}

bool CommunityDb::public_listing() const {
    std::lock_guard<std::mutex> lock(mutex_);
    return get_meta_("public_listing") == "1";
}

void CommunityDb::set_public_listing(bool on) {
    std::lock_guard<std::mutex> lock(mutex_);
    set_meta_("public_listing", on ? "1" : "0");
}

bool CommunityDb::transfer_ownership(int64_t new_owner_uid) {
    std::lock_guard<std::mutex> lock(mutex_);
    {
        Stmt q(db_, "SELECT 1 FROM members WHERE uid=?;");
        if (!q.s) return false;
        q.bind_int64(1, new_owner_uid);
        if (q.step() != SQLITE_ROW) return false;
    }
    set_meta_("owner_uid", std::to_string(new_owner_uid));   // refreshes owner_cache_ + invalidates perms
    return true;
}

bool CommunityDb::is_member(int64_t uid) const {
    std::lock_guard<std::mutex> lock(mutex_);
    Stmt q(db_, "SELECT 1 FROM members WHERE uid=?;");
    if (!q.s) return false;
    q.bind_int64(1, uid);
    return q.step() == SQLITE_ROW;
}

bool CommunityDb::add_member(int64_t uid, const std::string& username, int64_t name_iat) {
    if (uid <= 0) return false;
    std::lock_guard<std::mutex> lock(mutex_);
    // A newcomer starts clean: the row is keyed by their uid, so nothing
    // stored for another account (even under the same name) applies.
    invalidate_user_perms_(uid);
    Stmt q(db_,
        "INSERT INTO members(uid, username, joined_at, nickname, name_iat) VALUES(?, ?, ?, '', ?);");
    if (!q.s) return false;
    q.bind_int64(1, uid);
    q.bind_text(2, username);
    q.bind_int64(3, now_seconds());
    q.bind_int64(4, name_iat);
    return q.step() == SQLITE_DONE && sqlite3_changes(db_) > 0;
}

bool CommunityDb::claim_legacy(int64_t uid, const std::string& username) {
    if (uid <= 0 || username.empty()) return false;
    std::lock_guard<std::mutex> lock(mutex_);
    int64_t placeholder = 0;
    {
        Stmt q(db_, "SELECT uid FROM legacy_claims WHERE username=?;");
        if (!q.s) return false;
        q.bind_text(1, username);
        if (q.step() != SQLITE_ROW) return false;
        placeholder = q.col_int64(0);
    }
    if (placeholder >= 0) return false;
    // Only for an account that isn't a member under its uid yet: a member
    // renamed onto this name must not absorb what the name's previous
    // holder left here.
    {
        Stmt q(db_, "SELECT 1 FROM members WHERE uid=?;");
        if (!q.s) return false;
        q.bind_int64(1, uid);
        if (q.step() == SQLITE_ROW) return false;
    }
    invalidate_perm_cache_();
    overwrites_cache_.clear();
    if (!exec_sql(db_, "BEGIN IMMEDIATE;")) return false;   // DB1
    // Every column that can hold a uid. Tables keyed by it rewrite with OR
    // IGNORE and drop what collided (the real uid's row wins).
    static const char* const kRewrites[] = {
        "UPDATE OR IGNORE members SET uid=?1 WHERE uid=?2;",
        "DELETE FROM members WHERE uid=?2;",
        "UPDATE OR IGNORE member_roles SET uid=?1 WHERE uid=?2;",
        "DELETE FROM member_roles WHERE uid=?2;",
        "UPDATE OR IGNORE channel_overwrites SET target_id=CAST(?1 AS TEXT) "
        "  WHERE target_type=1 AND target_id=CAST(?2 AS TEXT);",
        "DELETE FROM channel_overwrites WHERE target_type=1 AND target_id=CAST(?2 AS TEXT);",
        "UPDATE OR IGNORE bans SET uid=?1 WHERE uid=?2;",
        "DELETE FROM bans WHERE uid=?2;",
        "UPDATE bans SET banned_by_uid=?1 WHERE banned_by_uid=?2;",
        "UPDATE invites SET created_by_uid=?1 WHERE created_by_uid=?2;",
        "UPDATE invites SET for_uid=?1 WHERE for_uid=?2;",
        "UPDATE attachments SET uploader_uid=?1 WHERE uploader_uid=?2;",
        "UPDATE channel_key_epochs SET created_by_uid=?1 WHERE created_by_uid=?2;",
        "UPDATE OR IGNORE channel_key_blobs SET recipient_uid=?1 WHERE recipient_uid=?2;",
        "DELETE FROM channel_key_blobs WHERE recipient_uid=?2;",
        "UPDATE channel_key_blobs SET sender_uid=?1 WHERE sender_uid=?2;",
        "UPDATE messages SET sender_uid=?1 WHERE sender_uid=?2;",
        "UPDATE audit_log SET actor_uid=?1 WHERE actor_uid=?2;",
        "UPDATE audit_log SET target_uid=?1 WHERE target_uid=?2;",
        "DELETE FROM legacy_claims WHERE uid=?2;",
    };
    bool ok = true;
    for (const char* sql : kRewrites) {
        Stmt q(db_, sql);
        ok = q.s != nullptr;
        if (!ok) break;
        q.bind_int64(1, uid);
        q.bind_int64(2, placeholder);
        ok = q.step() == SQLITE_DONE;
        if (!ok) break;
    }
    if (ok && owner_cache_ == placeholder) set_meta_("owner_uid", std::to_string(uid));
    exec_sql(db_, ok ? "COMMIT;" : "ROLLBACK;");
    if (ok) {
        // A placeholder never had a session, so no voice pass to move; the
        // owner cache was refreshed by set_meta_ inside the transaction.
        std::cout << "[DB] " << username << " claimed legacy uid " << placeholder
                  << " as " << uid << "\n";
    } else {
        try { owner_cache_ = std::stoll(get_meta_("owner_uid")); } catch (...) {}
        invalidate_perm_cache_();
    }
    return ok;
}

std::optional<std::string> CommunityDb::refresh_member_name(int64_t uid, const std::string& username,
                                                            int64_t iat, std::string* current) {
    if (current) *current = username;
    if (uid <= 0 || username.empty()) return std::nullopt;
    std::lock_guard<std::mutex> lock(mutex_);
    std::string old;
    int64_t named_at = 0;
    {
        Stmt q(db_, "SELECT username, name_iat FROM members WHERE uid=?;");
        if (!q.s) return std::nullopt;
        q.bind_int64(1, uid);
        if (q.step() != SQLITE_ROW) return std::nullopt;
        old = q.col_text(0);
        named_at = q.col_int64(1);
    }
    if (iat < named_at) {
        // An older token than the one that named them: keep the newer name.
        if (current) *current = old;
        return std::nullopt;
    }
    Stmt q(db_, "UPDATE members SET username=?, name_iat=? WHERE uid=?;");
    if (!q.s) return std::nullopt;
    q.bind_text(1, username);
    q.bind_int64(2, iat);
    q.bind_int64(3, uid);
    if (q.step() != SQLITE_DONE) return std::nullopt;
    if (old == username) return std::nullopt;
    return old;
}

bool CommunityDb::remove_member(int64_t uid) {
    std::lock_guard<std::mutex> lock(mutex_);
    invalidate_user_perms_(uid);
    overwrites_cache_.clear();
    voice_passes_.erase(uid);
    if (!exec_sql(db_, "BEGIN IMMEDIATE;")) return false;   // DB1
    {
        Stmt q(db_, "DELETE FROM channel_overwrites WHERE target_type=1 AND target_id=?;");
        if (q.s) { q.bind_text(1, std::to_string(uid)); q.step(); }
    }
    // Role assignments don't survive leaving/kick — a rejoining member
    // starts clean, matching Discord semantics.
    {
        Stmt q(db_, "DELETE FROM member_roles WHERE uid=?;");
        if (q.s) { q.bind_int64(1, uid); q.step(); }
    }
    bool was_member = false;
    {
        Stmt q(db_, "DELETE FROM members WHERE uid=?;");
        if (!q.s) { exec_sql(db_, "ROLLBACK;"); return false; }
        q.bind_int64(1, uid);
        if (q.step() != SQLITE_DONE) { exec_sql(db_, "ROLLBACK;"); return false; }
        was_member = sqlite3_changes(db_) > 0;
    }
    exec_sql(db_, "COMMIT;");
    return was_member;
}

namespace {
constexpr const char* kMemberCols =
    "uid, username, joined_at, nickname, timed_out_until, server_muted, server_deafened";
DbMember member_from_row(const Stmt& q) {
    DbMember m;
    m.uid = q.col_int64(0);
    m.username = q.col_text(1);
    m.joined_at = q.col_int64(2);
    m.nickname = q.col_text(3);
    m.timed_out_until = q.col_int64(4);
    m.server_muted = q.col_int(5) != 0;
    m.server_deafened = q.col_int(6) != 0;
    return m;
}
} // namespace

std::vector<DbMember> CommunityDb::list_members() const {
    std::lock_guard<std::mutex> lock(mutex_);
    std::vector<DbMember> out;
    const std::string sql = std::string("SELECT ") + kMemberCols + " FROM members ORDER BY joined_at ASC;";
    Stmt q(db_, sql.c_str());
    if (!q.s) return out;
    while (q.step() == SQLITE_ROW) out.push_back(member_from_row(q));
    return out;
}

std::optional<DbMember> CommunityDb::get_member(int64_t uid) const {
    std::lock_guard<std::mutex> lock(mutex_);
    const std::string sql = std::string("SELECT ") + kMemberCols + " FROM members WHERE uid=?;";
    Stmt q(db_, sql.c_str());
    if (!q.s) return std::nullopt;
    q.bind_int64(1, uid);
    if (q.step() != SQLITE_ROW) return std::nullopt;
    return member_from_row(q);
}

std::optional<DbMember> CommunityDb::find_member_by_name(const std::string& username) const {
    if (username.empty()) return std::nullopt;
    std::lock_guard<std::mutex> lock(mutex_);
    // Real accounts before placeholders, should both carry the name.
    const std::string sql = std::string("SELECT ") + kMemberCols +
                            " FROM members WHERE username=? ORDER BY uid > 0 DESC, joined_at ASC LIMIT 1;";
    Stmt q(db_, sql.c_str());
    if (!q.s) return std::nullopt;
    q.bind_text(1, username);
    if (q.step() != SQLITE_ROW) return std::nullopt;
    return member_from_row(q);
}

bool CommunityDb::set_timeout(int64_t uid, int64_t until) {
    std::lock_guard<std::mutex> lock(mutex_);
    Stmt q(db_, "UPDATE members SET timed_out_until=? WHERE uid=?;");
    if (!q.s) return false;
    q.bind_int64(1, until < 0 ? 0 : until);
    q.bind_int64(2, uid);
    if (q.step() != SQLITE_DONE) return false;
    return sqlite3_changes(db_) > 0;
}

bool CommunityDb::set_server_voice_flags(int64_t uid, bool muted, bool deafened) {
    std::lock_guard<std::mutex> lock(mutex_);
    Stmt q(db_, "UPDATE members SET server_muted=?, server_deafened=? WHERE uid=?;");
    if (!q.s) return false;
    q.bind_int(1, muted ? 1 : 0);
    q.bind_int(2, deafened ? 1 : 0);
    q.bind_int64(3, uid);
    if (q.step() != SQLITE_DONE) return false;
    return sqlite3_changes(db_) > 0;
}

int64_t CommunityDb::count_members() const {
    std::lock_guard<std::mutex> lock(mutex_);
    Stmt q(db_, "SELECT COUNT(*) FROM members;");
    if (!q.s || q.step() != SQLITE_ROW) return 0;
    return q.col_int64(0);
}

namespace {
constexpr const char* kBanCols =
    "uid, username, banned_at, banned_by_uid, banned_by, reason, expires_at";
DbBan ban_from_row(const Stmt& q) {
    DbBan b;
    b.uid = q.col_int64(0);
    b.username = q.col_text(1);
    b.banned_at = q.col_int64(2);
    b.banned_by_uid = q.col_int64(3);
    b.banned_by = q.col_text(4);
    b.reason = q.col_text(5);
    b.expires_at = q.col_int64(6);
    return b;
}
// Shared by is_banned / get_ban / redeem_invite (mutex held by caller):
// an expired ban is deleted on sight and reported as "not banned". By uid;
// a name-only ban (negative uid) by `username` until claimed.
bool ban_active_unlocked(sqlite3* db, int64_t uid, const std::string& username, DbBan* out) {
    const std::string sql = std::string("SELECT ") + kBanCols +
        " FROM bans WHERE (?1 > 0 AND uid=?1) OR (uid < 0 AND ?2 <> '' AND username=?2) LIMIT 1;";
    Stmt q(db, sql.c_str());
    if (!q.s) return false;
    q.bind_int64(1, uid);
    q.bind_text(2, username);
    if (q.step() != SQLITE_ROW) return false;
    DbBan b = ban_from_row(q);
    if (b.expires_at != 0 && b.expires_at <= now_seconds()) {
        Stmt del(db, "DELETE FROM bans WHERE uid=?;");
        if (del.s) { del.bind_int64(1, b.uid); del.step(); }
        return false;
    }
    if (out) *out = b;
    return true;
}
} // namespace

bool CommunityDb::is_banned(int64_t uid, const std::string& username) const {
    std::lock_guard<std::mutex> lock(mutex_);
    return ban_active_unlocked(db_, uid, username, nullptr);
}

std::optional<DbBan> CommunityDb::get_ban(int64_t uid) const {
    std::lock_guard<std::mutex> lock(mutex_);
    DbBan b;
    if (!ban_active_unlocked(db_, uid, "", &b)) return std::nullopt;
    return b;
}

int64_t CommunityDb::placeholder_for_name(const std::string& username) {
    std::lock_guard<std::mutex> lock(mutex_);
    {
        Stmt q(db_, "SELECT uid FROM legacy_claims WHERE username=?;");
        if (q.s) {
            q.bind_text(1, username);
            if (q.step() == SQLITE_ROW) return q.col_int64(0);
        }
    }
    const int64_t placeholder = next_placeholder_unlocked_();
    Stmt q(db_, "INSERT INTO legacy_claims(username, uid) VALUES(?, ?);");
    if (!q.s) return 0;
    q.bind_text(1, username);
    q.bind_int64(2, placeholder);
    return q.step() == SQLITE_DONE ? placeholder : 0;
}

bool CommunityDb::add_ban(int64_t uid, const std::string& username,
                          int64_t banned_by_uid, const std::string& banned_by,
                          const std::string& reason,
                          int64_t expires_at) {
    if (uid == 0) return false;
    std::lock_guard<std::mutex> lock(mutex_);
    invalidate_user_perms_(uid);
    overwrites_cache_.clear();
    voice_passes_.erase(uid);
    // Remove membership (and role assignments) and insert ban atomically.
    if (!exec_sql(db_, "BEGIN IMMEDIATE;")) return false;   // DB1
    {
        Stmt del(db_, "DELETE FROM channel_overwrites WHERE target_type=1 AND target_id=?;");
        if (del.s) { del.bind_text(1, std::to_string(uid)); del.step(); }
    }
    {
        Stmt del(db_, "DELETE FROM member_roles WHERE uid=?;");
        if (del.s) { del.bind_int64(1, uid); del.step(); }
    }
    {
        Stmt del(db_, "DELETE FROM members WHERE uid=?;");
        if (del.s) { del.bind_int64(1, uid); del.step(); }
    }
    bool ok = false;
    {
        Stmt ins(db_,
            "INSERT INTO bans(uid, username, banned_at, banned_by_uid, banned_by, reason, expires_at) "
            "VALUES(?, ?, ?, ?, ?, ?, ?) "
            "ON CONFLICT(uid) DO UPDATE SET "
            "  username=excluded.username, "
            "  banned_at=excluded.banned_at, "
            "  banned_by_uid=excluded.banned_by_uid, "
            "  banned_by=excluded.banned_by, "
            "  reason=excluded.reason, "
            "  expires_at=excluded.expires_at;");
        if (ins.s) {
            ins.bind_int64(1, uid);
            ins.bind_text(2, username);
            ins.bind_int64(3, now_seconds());
            ins.bind_int64(4, banned_by_uid);
            ins.bind_text(5, banned_by);
            ins.bind_text(6, reason);
            ins.bind_int64(7, expires_at < 0 ? 0 : expires_at);
            ok = (ins.step() == SQLITE_DONE);
        }
    }
    exec_sql(db_, ok ? "COMMIT;" : "ROLLBACK;");
    return ok;
}

bool CommunityDb::remove_ban(int64_t uid) {
    std::lock_guard<std::mutex> lock(mutex_);
    Stmt q(db_, "DELETE FROM bans WHERE uid=?;");
    if (!q.s) return false;
    q.bind_int64(1, uid);
    return q.step() == SQLITE_DONE;
}

std::vector<DbBan> CommunityDb::list_bans() const {
    std::lock_guard<std::mutex> lock(mutex_);
    std::vector<DbBan> out;
    const int64_t now = now_seconds();
    // Sweep expired rows first so the list (and its count) is honest.
    {
        Stmt del(db_, "DELETE FROM bans WHERE expires_at != 0 AND expires_at <= ?;");
        if (del.s) { del.bind_int64(1, now); del.step(); }
    }
    const std::string sql = std::string("SELECT ") + kBanCols + " FROM bans ORDER BY banned_at DESC;";
    Stmt q(db_, sql.c_str());
    if (!q.s) return out;
    while (q.step() == SQLITE_ROW) out.push_back(ban_from_row(q));
    return out;
}

// --- roles + permissions ---

namespace {
DbRole role_from_row(const Stmt& q) {
    DbRole r;
    r.id = q.col_int64(0);
    r.name = q.col_text(1);
    r.color = static_cast<uint32_t>(q.col_int64(2));
    r.position = q.col_int(3);
    r.permissions = static_cast<uint64_t>(q.col_int64(4));
    r.is_default = q.col_int(5) != 0;
    r.manage_each_other = q.col_int(6) != 0;
    return r;
}
constexpr const char* kRoleCols =
    "id, name, color, position, permissions, is_default, manage_each_other";
} // namespace

std::vector<DbRole> CommunityDb::list_roles() const {
    std::lock_guard<std::mutex> lock(mutex_);
    std::vector<DbRole> out;
    std::string sql = std::string("SELECT ") + kRoleCols +
                      " FROM roles ORDER BY position DESC, id ASC;";
    Stmt q(db_, sql.c_str());
    if (!q.s) return out;
    while (q.step() == SQLITE_ROW) out.push_back(role_from_row(q));
    return out;
}

std::optional<DbRole> CommunityDb::get_role_unlocked_(int64_t role_id) const {
    std::string sql = std::string("SELECT ") + kRoleCols +
                      " FROM roles WHERE id=?;";
    Stmt q(db_, sql.c_str());
    if (!q.s) return std::nullopt;
    q.bind_int64(1, role_id);
    if (q.step() != SQLITE_ROW) return std::nullopt;
    return role_from_row(q);
}

std::optional<DbRole> CommunityDb::get_role(int64_t role_id) const {
    std::lock_guard<std::mutex> lock(mutex_);
    return get_role_unlocked_(role_id);
}

int32_t CommunityDb::max_role_position_unlocked_() const {
    Stmt q(db_, "SELECT COALESCE(MAX(position), 0) FROM roles;");
    if (!q.s || q.step() != SQLITE_ROW) return 0;
    return q.col_int(0);
}

std::optional<DbRole> CommunityDb::create_role(const std::string& raw_name,
                                               uint32_t color,
                                               uint64_t permissions,
                                               bool manage_each_other) {
    // Clamp here too so no future caller can store a mid-codepoint cut.
    const std::string name = clamp_utf8(raw_name, kMaxRoleNameBytes);
    if (name.empty()) return std::nullopt;
    std::lock_guard<std::mutex> lock(mutex_);
    invalidate_perm_cache_();   // positions shift for every role
    if (!exec_sql(db_, "BEGIN IMMEDIATE;")) return std::nullopt;   // DB1
    // New roles land at position 1, directly above `everyone`; existing
    // roles shift up one to stay dense.
    exec_sql(db_, "UPDATE roles SET position = position + 1 WHERE is_default = 0;");
    DbRole r;
    r.name = name;
    r.color = color;
    r.position = 1;
    r.permissions = permissions;
    r.manage_each_other = manage_each_other;
    {
        Stmt ins(db_,
            "INSERT INTO roles(name, color, position, permissions, is_default, manage_each_other) "
            "VALUES(?, ?, 1, ?, 0, ?);");
        if (!ins.s) { exec_sql(db_, "ROLLBACK;"); return std::nullopt; }
        ins.bind_text(1, name);
        ins.bind_int64(2, static_cast<int64_t>(color));
        ins.bind_int64(3, static_cast<int64_t>(permissions));
        ins.bind_int(4, manage_each_other ? 1 : 0);
        if (ins.step() != SQLITE_DONE) { exec_sql(db_, "ROLLBACK;"); return std::nullopt; }
        r.id = sqlite3_last_insert_rowid(db_);
    }
    exec_sql(db_, "COMMIT;");
    return r;
}

bool CommunityDb::update_role(int64_t role_id,
                              const std::string& raw_name,
                              uint32_t color,
                              uint64_t permissions,
                              int32_t position,
                              bool manage_each_other) {
    const std::string name = clamp_utf8(raw_name, kMaxRoleNameBytes);
    std::lock_guard<std::mutex> lock(mutex_);
    auto cur = get_role_unlocked_(role_id);
    if (!cur) return false;
    invalidate_perm_cache_();

    if (cur->is_default) {
        // Only the permission bits (+ peer flag) of `everyone` are editable.
        Stmt q(db_, "UPDATE roles SET permissions=?, manage_each_other=? WHERE id=?;");
        if (!q.s) return false;
        q.bind_int64(1, static_cast<int64_t>(permissions));
        q.bind_int(2, manage_each_other ? 1 : 0);
        q.bind_int64(3, role_id);
        return q.step() == SQLITE_DONE;
    }
    if (name.empty()) return false;

    // Clamp the requested position into the valid dense range [1, N].
    const int32_t max_pos = max_role_position_unlocked_();
    int32_t new_pos = position;
    if (new_pos < 1) new_pos = 1;
    if (new_pos > max_pos) new_pos = max_pos;

    if (!exec_sql(db_, "BEGIN IMMEDIATE;")) return false;   // DB1
    bool ok = true;
    if (new_pos != cur->position) {
        // Standard list-move: close the gap at the old slot, open one at
        // the new slot. `everyone` (position 0) is never touched.
        if (new_pos > cur->position) {
            Stmt q(db_,
                "UPDATE roles SET position = position - 1 "
                "WHERE is_default = 0 AND position > ? AND position <= ?;");
            ok = q.s != nullptr;
            if (ok) {
                q.bind_int(1, cur->position);
                q.bind_int(2, new_pos);
                ok = q.step() == SQLITE_DONE;
            }
        } else {
            Stmt q(db_,
                "UPDATE roles SET position = position + 1 "
                "WHERE is_default = 0 AND position >= ? AND position < ?;");
            ok = q.s != nullptr;
            if (ok) {
                q.bind_int(1, new_pos);
                q.bind_int(2, cur->position);
                ok = q.step() == SQLITE_DONE;
            }
        }
    }
    if (ok) {
        Stmt q(db_,
            "UPDATE roles SET name=?, color=?, permissions=?, position=?, manage_each_other=? "
            "WHERE id=?;");
        ok = q.s != nullptr;
        if (ok) {
            q.bind_text(1, name);
            q.bind_int64(2, static_cast<int64_t>(color));
            q.bind_int64(3, static_cast<int64_t>(permissions));
            q.bind_int(4, new_pos);
            q.bind_int(5, manage_each_other ? 1 : 0);
            q.bind_int64(6, role_id);
            ok = q.step() == SQLITE_DONE;
        }
    }
    exec_sql(db_, ok ? "COMMIT;" : "ROLLBACK;");
    return ok;
}

bool CommunityDb::delete_role(int64_t role_id) {
    std::lock_guard<std::mutex> lock(mutex_);
    auto cur = get_role_unlocked_(role_id);
    if (!cur || cur->is_default) return false;
    invalidate_perm_cache_();
    overwrites_cache_.clear();

    if (!exec_sql(db_, "BEGIN IMMEDIATE;")) return false;   // DB1
    bool ok = true;
    {
        Stmt q(db_, "DELETE FROM channel_overwrites WHERE target_type=0 AND target_id=?;");
        ok = q.s != nullptr;
        if (ok) { q.bind_text(1, std::to_string(role_id)); ok = q.step() == SQLITE_DONE; }
    }
    if (ok) {
        Stmt q(db_, "DELETE FROM member_roles WHERE role_id=?;");
        ok = q.s != nullptr;
        if (ok) { q.bind_int64(1, role_id); ok = q.step() == SQLITE_DONE; }
    }
    if (ok) {
        Stmt q(db_, "DELETE FROM roles WHERE id=?;");
        ok = q.s != nullptr;
        if (ok) { q.bind_int64(1, role_id); ok = q.step() == SQLITE_DONE; }
    }
    if (ok) {
        // Close the gap so positions stay dense.
        Stmt q(db_,
            "UPDATE roles SET position = position - 1 "
            "WHERE is_default = 0 AND position > ?;");
        ok = q.s != nullptr;
        if (ok) { q.bind_int(1, cur->position); ok = q.step() == SQLITE_DONE; }
    }
    exec_sql(db_, ok ? "COMMIT;" : "ROLLBACK;");
    return ok;
}

std::vector<int64_t> CommunityDb::get_member_role_ids(int64_t uid) const {
    std::lock_guard<std::mutex> lock(mutex_);
    std::vector<int64_t> out;
    Stmt q(db_,
        "SELECT mr.role_id FROM member_roles mr "
        "JOIN roles r ON r.id = mr.role_id "
        "WHERE mr.uid=? ORDER BY r.position DESC;");
    if (!q.s) return out;
    q.bind_int64(1, uid);
    while (q.step() == SQLITE_ROW) out.push_back(q.col_int64(0));
    return out;
}

std::vector<std::pair<int64_t, int64_t>> CommunityDb::list_all_member_roles() const {
    std::lock_guard<std::mutex> lock(mutex_);
    std::vector<std::pair<int64_t, int64_t>> out;
    Stmt q(db_,
        "SELECT mr.uid, mr.role_id FROM member_roles mr "
        "JOIN roles r ON r.id = mr.role_id "
        "ORDER BY mr.uid ASC, r.position DESC;");
    if (!q.s) return out;
    while (q.step() == SQLITE_ROW) {
        out.emplace_back(q.col_int64(0), q.col_int64(1));
    }
    return out;
}

std::unordered_map<int64_t, int64_t> CommunityDb::role_member_counts() const {
    std::lock_guard<std::mutex> lock(mutex_);
    std::unordered_map<int64_t, int64_t> out;
    Stmt q(db_, "SELECT role_id, COUNT(*) FROM member_roles GROUP BY role_id;");
    if (!q.s) return out;
    while (q.step() == SQLITE_ROW) out[q.col_int64(0)] = q.col_int64(1);
    return out;
}

bool CommunityDb::set_member_roles(int64_t uid, const std::vector<int64_t>& role_ids) {
    std::lock_guard<std::mutex> lock(mutex_);
    // Validate every id references an existing, non-default role before
    // touching anything.
    for (int64_t id : role_ids) {
        auto r = get_role_unlocked_(id);
        if (!r || r->is_default) return false;
    }
    invalidate_user_perms_(uid);
    if (!exec_sql(db_, "BEGIN IMMEDIATE;")) return false;   // DB1
    bool ok = true;
    {
        Stmt q(db_, "DELETE FROM member_roles WHERE uid=?;");
        ok = q.s != nullptr;
        if (ok) { q.bind_int64(1, uid); ok = q.step() == SQLITE_DONE; }
    }
    for (int64_t id : role_ids) {
        if (!ok) break;
        Stmt q(db_,
            "INSERT OR IGNORE INTO member_roles(uid, role_id) VALUES(?, ?);");
        ok = q.s != nullptr;
        if (ok) {
            q.bind_int64(1, uid);
            q.bind_int64(2, id);
            ok = q.step() == SQLITE_DONE;
        }
    }
    exec_sql(db_, ok ? "COMMIT;" : "ROLLBACK;");
    return ok;
}

const CommunityDb::PermEntry& CommunityDb::perm_entry_unlocked_(int64_t uid) const {
    auto it = perm_cache_.find(uid);
    if (it != perm_cache_.end()) return it->second;

    PermEntry e;
    if (uid != 0 && owner_cache_ == uid) {
        e.permissions = perms::kAll;
        e.level = INT32_MAX;
    } else {
        // Base bits from `everyone` + OR of the member's assigned roles,
        // the highest assigned position, and that role's peer flag (the
        // `everyone` row stands at level 0), in one pass.
        Stmt q(db_,
            "SELECT permissions, 0, manage_each_other FROM roles WHERE is_default=1 "
            "UNION ALL "
            "SELECT r.permissions, r.position, r.manage_each_other FROM roles r "
            "JOIN member_roles mr ON mr.role_id = r.id "
            "WHERE mr.uid=?1;");
        uint64_t p = 0;
        int32_t level = -1;
        bool peers = false;
        if (q.s) {
            q.bind_int64(1, uid);
            while (q.step() == SQLITE_ROW) {
                p |= static_cast<uint64_t>(q.col_int64(0));
                if (q.col_int(1) > level) {
                    level = q.col_int(1);
                    peers = q.col_int(2) != 0;
                }
            }
        }
        e.permissions = (p & perms::kAdministrator) ? perms::kAll : p;
        e.level = std::max(level, 0);
        e.manage_peers = peers;
    }
    // Bounded: entries are per user and cleared on any role change; a
    // pathological churn of users still can't grow it past this.
    if (perm_cache_.size() >= 4096) perm_cache_.clear();
    return perm_cache_.emplace(uid, e).first->second;
}

uint64_t CommunityDb::effective_permissions_unlocked_(int64_t uid) const {
    return perm_entry_unlocked_(uid).permissions;
}

uint64_t CommunityDb::effective_permissions(int64_t uid) const {
    std::lock_guard<std::mutex> lock(mutex_);
    return effective_permissions_unlocked_(uid);
}

bool CommunityDb::has_permission(int64_t uid, uint64_t perm) const {
    std::lock_guard<std::mutex> lock(mutex_);
    return (effective_permissions_unlocked_(uid) & perm) == perm;
}

int32_t CommunityDb::member_level(int64_t uid) const {
    std::lock_guard<std::mutex> lock(mutex_);
    return perm_entry_unlocked_(uid).level;
}

bool CommunityDb::are_peers(int64_t a, int64_t b) const {
    if (a == 0 || b == 0 || a == b) return false;
    std::lock_guard<std::mutex> lock(mutex_);
    if (a == owner_cache_ || b == owner_cache_) return false;
    const PermEntry ea = perm_entry_unlocked_(a);
    const PermEntry& eb = perm_entry_unlocked_(b);
    // Same level ⇒ same highest role (positions are unique), so one flag.
    return ea.level == eb.level && ea.manage_peers;
}

// --- permissions v2: per-channel overwrites ---

const std::vector<DbOverwrite>& CommunityDb::overwrites_unlocked_(const std::string& channel_id) const {
    auto it = overwrites_cache_.find(channel_id);
    if (it != overwrites_cache_.end()) return it->second;
    std::vector<DbOverwrite> rows;
    Stmt q(db_,
        "SELECT channel_id, target_type, target_id, allow, deny "
        "FROM channel_overwrites WHERE channel_id=?;");
    if (q.s) {
        q.bind_text(1, channel_id);
        while (q.step() == SQLITE_ROW) {
            DbOverwrite ow;
            ow.channel_id = q.col_text(0);
            ow.target_type = q.col_int(1);
            ow.target_id = q.col_text(2);
            ow.allow = static_cast<uint64_t>(q.col_int64(3));
            ow.deny = static_cast<uint64_t>(q.col_int64(4));
            rows.push_back(std::move(ow));
        }
    }
    if (overwrites_cache_.size() >= 4096) overwrites_cache_.clear();
    return overwrites_cache_.emplace(channel_id, std::move(rows)).first->second;
}

std::vector<DbOverwrite> CommunityDb::list_overwrites(const std::string& channel_id) const {
    std::lock_guard<std::mutex> lock(mutex_);
    return overwrites_unlocked_(overwrite_source_unlocked_(channel_id));
}

// --- permissions v3: category sync ---

void CommunityDb::rebuild_channel_layout_unlocked_() const {
    parent_of_.clear();
    synced_of_.clear();
    category_sync_of_.clear();
    Stmt q(db_, "SELECT id, type, perm_synced, category_sync FROM channels ORDER BY position ASC, id ASC;");
    if (q.s) {
        std::string current;   // nearest CATEGORY above
        while (q.step() == SQLITE_ROW) {
            const std::string id = q.col_text(0);
            if (q.col_int(1) == 2) {
                current = id;
                parent_of_[id] = "";
                synced_of_[id] = false;
                category_sync_of_[id] = q.col_int(3) != 0;
            } else {
                parent_of_[id] = current;
                synced_of_[id] = q.col_int(2) != 0;
            }
        }
    }
    layout_valid_ = true;
}

void CommunityDb::invalidate_channel_layout_() {
    layout_valid_ = false;
    channel_perm_cache_.clear();
}

std::string CommunityDb::parent_category_unlocked_(const std::string& channel_id) const {
    if (!layout_valid_) rebuild_channel_layout_unlocked_();
    auto it = parent_of_.find(channel_id);
    return it == parent_of_.end() ? std::string() : it->second;
}

std::string CommunityDb::overwrite_source_unlocked_(const std::string& channel_id) const {
    if (!layout_valid_) rebuild_channel_layout_unlocked_();
    auto p = parent_of_.find(channel_id);
    if (p == parent_of_.end() || p->second.empty()) return channel_id;
    auto sy = synced_of_.find(channel_id);
    if (sy == synced_of_.end() || !sy->second) return channel_id;
    // Belt and braces next to the invariant: a category that doesn't sync
    // is never a source.
    auto cs = category_sync_of_.find(p->second);
    return (cs != category_sync_of_.end() && cs->second) ? p->second : channel_id;
}

void CommunityDb::enforce_sync_invariant_unlocked_() {
    if (!layout_valid_) rebuild_channel_layout_unlocked_();
    std::vector<std::string> stray;
    for (const auto& [id, synced] : synced_of_) {
        if (synced && overwrite_source_unlocked_(id) == id) stray.push_back(id);
    }
    if (stray.empty()) return;
    // A synced channel has no rows of its own and followed nothing, so
    // unsyncing it changes nothing it allows.
    if (!exec_sql(db_, "BEGIN IMMEDIATE;")) return;   // DB1
    bool ok = true;
    for (const auto& id : stray) {
        Stmt q(db_, "UPDATE channels SET perm_synced=0 WHERE id=?;");
        ok = q.s != nullptr;
        if (ok) { q.bind_text(1, id); ok = q.step() == SQLITE_DONE; }
        if (!ok) break;
    }
    exec_sql(db_, ok ? "COMMIT;" : "ROLLBACK;");
    invalidate_channel_layout_();
}

std::string CommunityDb::parent_category(const std::string& channel_id) const {
    std::lock_guard<std::mutex> lock(mutex_);
    return parent_category_unlocked_(channel_id);
}

std::vector<std::string> CommunityDb::synced_children(const std::string& category_id) const {
    std::lock_guard<std::mutex> lock(mutex_);
    std::vector<std::string> out;
    if (!layout_valid_) rebuild_channel_layout_unlocked_();
    for (const auto& [id, parent] : parent_of_) {
        if (parent == category_id && overwrite_source_unlocked_(id) == category_id) out.push_back(id);
    }
    return out;
}

std::vector<std::string> CommunityDb::category_children(const std::string& category_id) const {
    std::lock_guard<std::mutex> lock(mutex_);
    std::vector<std::string> out;
    Stmt q(db_, "SELECT id FROM channels ORDER BY position ASC, id ASC;");
    if (!q.s) return out;
    if (!layout_valid_) rebuild_channel_layout_unlocked_();
    while (q.step() == SQLITE_ROW) {
        std::string id = q.col_text(0);
        auto p = parent_of_.find(id);
        if (p != parent_of_.end() && p->second == category_id) out.push_back(std::move(id));
    }
    return out;
}

namespace {
// Writes `rows` as `channel_id`'s own overwrites (replacing any) and sets
// perm_synced. Caller holds the mutex and the transaction.
bool write_own_overwrites(sqlite3* db, const std::string& channel_id,
                          const std::vector<DbOverwrite>& rows, bool synced) {
    {
        Stmt del(db, "DELETE FROM channel_overwrites WHERE channel_id=?;");
        if (!del.s) return false;
        del.bind_text(1, channel_id);
        if (del.step() != SQLITE_DONE) return false;
    }
    for (const auto& ow : rows) {
        Stmt ins(db,
            "INSERT INTO channel_overwrites(channel_id, target_type, target_id, allow, deny) "
            "VALUES(?, ?, ?, ?, ?);");
        if (!ins.s) return false;
        ins.bind_text(1, channel_id);
        ins.bind_int(2, ow.target_type);
        ins.bind_text(3, ow.target_id);
        ins.bind_int64(4, static_cast<int64_t>(ow.allow));
        ins.bind_int64(5, static_cast<int64_t>(ow.deny));
        if (ins.step() != SQLITE_DONE) return false;
    }
    Stmt upd(db, "UPDATE channels SET perm_synced=? WHERE id=?;");
    if (!upd.s) return false;
    upd.bind_int(1, synced ? 1 : 0);
    upd.bind_text(2, channel_id);
    return upd.step() == SQLITE_DONE;
}
} // namespace

bool CommunityDb::sync_channel_permissions(const std::string& channel_id) {
    std::lock_guard<std::mutex> lock(mutex_);
    if (!layout_valid_) rebuild_channel_layout_unlocked_();
    auto p = parent_of_.find(channel_id);
    if (p == parent_of_.end() || p->second.empty()) return false;   // unknown / category / uncategorized
    if (auto cs = category_sync_of_.find(p->second);
        cs == category_sync_of_.end() || !cs->second) return false;  // a plain group
    if (!exec_sql(db_, "BEGIN IMMEDIATE;")) return false;   // DB1
    const bool ok = write_own_overwrites(db_, channel_id, {}, /*synced=*/true);
    exec_sql(db_, ok ? "COMMIT;" : "ROLLBACK;");
    overwrites_cache_.erase(channel_id);
    invalidate_channel_layout_();
    return ok;
}

bool CommunityDb::set_category_sync(const std::string& category_id, bool on) {
    std::lock_guard<std::mutex> lock(mutex_);
    if (!layout_valid_) rebuild_channel_layout_unlocked_();
    auto cs = category_sync_of_.find(category_id);
    if (cs == category_sync_of_.end()) return false;   // unknown / not a category
    if (cs->second == on) return true;
    // Turning it off: whoever follows the category keeps its rows as
    // their own — no channel changes what it allows.
    std::vector<std::string> followers;
    std::vector<DbOverwrite> rows;
    if (!on) {
        for (const auto& [id, parent] : parent_of_) {
            if (parent == category_id && overwrite_source_unlocked_(id) == category_id) followers.push_back(id);
        }
        if (!followers.empty()) rows = overwrites_unlocked_(category_id);
    }
    if (!exec_sql(db_, "BEGIN IMMEDIATE;")) return false;   // DB1
    bool ok = true;
    for (const auto& id : followers) {
        ok = write_own_overwrites(db_, id, rows, /*synced=*/false);
        overwrites_cache_.erase(id);
        if (!ok) break;
    }
    if (ok) {
        Stmt q(db_, "UPDATE channels SET category_sync=? WHERE id=? AND type=2;");
        ok = q.s != nullptr;
        if (ok) { q.bind_int(1, on ? 1 : 0); q.bind_text(2, category_id); ok = q.step() == SQLITE_DONE; }
    }
    exec_sql(db_, ok ? "COMMIT;" : "ROLLBACK;");
    invalidate_channel_layout_();
    return ok;
}

bool CommunityDb::restore_channel_overwrites(const std::string& channel_id,
                                             const std::vector<DbOverwrite>& rows) {
    std::lock_guard<std::mutex> lock(mutex_);
    if (!exec_sql(db_, "BEGIN IMMEDIATE;")) return false;   // DB1
    const bool ok = write_own_overwrites(db_, channel_id, rows, /*synced=*/false);
    exec_sql(db_, ok ? "COMMIT;" : "ROLLBACK;");
    overwrites_cache_.erase(channel_id);
    invalidate_channel_layout_();
    return ok;
}

// --- permissions v3: voice passes ---

void CommunityDb::grant_voice_pass(int64_t uid, const std::string& channel_id) {
    std::lock_guard<std::mutex> lock(mutex_);
    voice_passes_[uid].insert(channel_id);
    channel_perm_cache_.erase(uid);
}

bool CommunityDb::revoke_voice_pass(int64_t uid, const std::string& channel_id) {
    std::lock_guard<std::mutex> lock(mutex_);
    auto it = voice_passes_.find(uid);
    if (it == voice_passes_.end() || it->second.erase(channel_id) == 0) return false;
    if (it->second.empty()) voice_passes_.erase(it);
    channel_perm_cache_.erase(uid);
    return true;
}

bool CommunityDb::has_voice_pass(int64_t uid, const std::string& channel_id) const {
    std::lock_guard<std::mutex> lock(mutex_);
    auto it = voice_passes_.find(uid);
    return it != voice_passes_.end() && it->second.count(channel_id) > 0;
}

bool CommunityDb::set_overwrite(const DbOverwrite& in) {
    std::lock_guard<std::mutex> lock(mutex_);
    bool is_category = false;
    {
        Stmt q(db_, "SELECT type FROM channels WHERE id=?;");
        if (!q.s) return false;
        q.bind_text(1, in.channel_id);
        if (q.step() != SQLITE_ROW) return false;
        is_category = q.col_int(0) == 2;
    }
    if (in.target_type == 0) {
        int64_t rid = 0;
        try { rid = std::stoll(in.target_id); } catch (...) { return false; }
        if (!get_role_unlocked_(rid)) return false;
    } else if (in.target_type == 1) {
        // Member rows name the uid in decimal.
        int64_t uid = 0;
        try { uid = std::stoll(in.target_id); } catch (...) { return false; }
        Stmt q(db_, "SELECT 1 FROM members WHERE uid=?;");
        if (!q.s) return false;
        q.bind_int64(1, uid);
        if (q.step() != SQLITE_ROW) return false;
    } else {
        return false;
    }
    const uint64_t deny = in.deny & perms::kKnownMask;
    const uint64_t allow = (in.allow & perms::kKnownMask) & ~deny;

    // Category sync (v3): editing a synced channel first copies the
    // category's rows onto it (same effective result), and any write
    // leaves a non-category channel unsynced.
    const std::string source = overwrite_source_unlocked_(in.channel_id);
    std::vector<DbOverwrite> inherited;
    if (source != in.channel_id) inherited = overwrites_unlocked_(source);

    overwrites_cache_.erase(in.channel_id);
    invalidate_channel_layout_();

    if (!exec_sql(db_, "BEGIN IMMEDIATE;")) return false;   // DB1
    bool ok = true;
    if (!inherited.empty()) ok = write_own_overwrites(db_, in.channel_id, inherited, /*synced=*/false);
    if (ok && !is_category) {
        Stmt q(db_, "UPDATE channels SET perm_synced=0 WHERE id=?;");
        ok = q.s != nullptr;
        if (ok) { q.bind_text(1, in.channel_id); ok = q.step() == SQLITE_DONE; }
    }
    if (ok && allow == 0 && deny == 0) {
        Stmt q(db_,
            "DELETE FROM channel_overwrites "
            "WHERE channel_id=? AND target_type=? AND target_id=?;");
        ok = q.s != nullptr;
        if (ok) {
            q.bind_text(1, in.channel_id);
            q.bind_int(2, in.target_type);
            q.bind_text(3, in.target_id);
            ok = q.step() == SQLITE_DONE;
        }
    } else if (ok) {
        Stmt q(db_,
            "INSERT INTO channel_overwrites(channel_id, target_type, target_id, allow, deny) "
            "VALUES(?, ?, ?, ?, ?) "
            "ON CONFLICT(channel_id, target_type, target_id) DO UPDATE SET "
            "  allow=excluded.allow, deny=excluded.deny;");
        ok = q.s != nullptr;
        if (ok) {
            q.bind_text(1, in.channel_id);
            q.bind_int(2, in.target_type);
            q.bind_text(3, in.target_id);
            q.bind_int64(4, static_cast<int64_t>(allow));
            q.bind_int64(5, static_cast<int64_t>(deny));
            ok = q.step() == SQLITE_DONE;
        }
    }
    exec_sql(db_, ok ? "COMMIT;" : "ROLLBACK;");
    overwrites_cache_.erase(in.channel_id);
    invalidate_channel_layout_();
    return ok;
}

uint64_t CommunityDb::channel_permissions_unlocked_(int64_t uid,
                                                    const std::string& channel_id) const {
    auto& per_user = channel_perm_cache_[uid];
    if (auto it = per_user.find(channel_id); it != per_user.end()) return it->second;

    uint64_t result = 0;
    {
        Stmt q(db_, "SELECT 1 FROM channels WHERE id=?;");
        bool exists = false;
        if (q.s) { q.bind_text(1, channel_id); exists = q.step() == SQLITE_ROW; }
        if (!exists) {
            per_user.emplace(channel_id, 0);
            return 0;
        }
    }
    const PermEntry& base = perm_entry_unlocked_(uid);
    if (base.permissions == perms::kAll) {
        result = perms::kAll;   // owner / ADMINISTRATOR bypass overwrites
    } else {
        // Member overwrites name the uid in decimal (role rows the role id).
        const std::string me = std::to_string(uid);
        std::set<std::string> my_roles;
        {
            Stmt q(db_, "SELECT role_id FROM member_roles WHERE uid=?;");
            if (q.s) {
                q.bind_int64(1, uid);
                while (q.step() == SQLITE_ROW) my_roles.insert(std::to_string(q.col_int64(0)));
            }
        }
        std::string everyone_id;
        {
            Stmt q(db_, "SELECT id FROM roles WHERE is_default=1;");
            if (q.s && q.step() == SQLITE_ROW) everyone_id = std::to_string(q.col_int64(0));
        }
        uint64_t p = base.permissions;
        uint64_t ev_allow = 0, ev_deny = 0, role_allow = 0, role_deny = 0, me_allow = 0, me_deny = 0;
        // A synced channel resolves through its category's rows (v3).
        for (const auto& ow : overwrites_unlocked_(overwrite_source_unlocked_(channel_id))) {
            if (ow.target_type == 0) {
                if (ow.target_id == everyone_id) { ev_allow |= ow.allow; ev_deny |= ow.deny; }
                else if (my_roles.count(ow.target_id)) { role_allow |= ow.allow; role_deny |= ow.deny; }
            } else if (ow.target_id == me) {
                me_allow |= ow.allow; me_deny |= ow.deny;
            }
        }
        p = (p & ~ev_deny) | ev_allow;
        p = (p & ~role_deny) | role_allow;
        p = (p & ~me_deny) | me_allow;
        // Voice pass (v3): moved here by a moderator — VIEW + CONNECT while
        // in it; every other bit still follows the chain above.
        if (auto vp = voice_passes_.find(uid);
            vp != voice_passes_.end() && vp->second.count(channel_id)) {
            p |= perms::kViewChannel | perms::kConnectVoice;
        }
        result = (p & perms::kViewChannel) ? p : 0;
    }
    if (per_user.size() >= 1024) per_user.clear();
    per_user.emplace(channel_id, result);
    return result;
}

uint64_t CommunityDb::channel_permissions(int64_t uid, const std::string& channel_id) const {
    std::lock_guard<std::mutex> lock(mutex_);
    return channel_permissions_unlocked_(uid, channel_id);
}

bool CommunityDb::has_channel_permission(int64_t uid, const std::string& channel_id, uint64_t perm) const {
    std::lock_guard<std::mutex> lock(mutex_);
    return (channel_permissions_unlocked_(uid, channel_id) & perm) == perm;
}

namespace {
// Invites with the creator's current name (members) and the name a
// personal invite was made out to.
constexpr const char* kInviteSelect =
    "SELECT i.code, i.created_by_uid, COALESCE(m.username, ''), i.created_at, i.expires_at, "
    "       i.max_uses, i.uses, i.for_uid, i.for_user "
    "FROM invites i LEFT JOIN members m ON m.uid = i.created_by_uid ";
DbInvite invite_from_row(const Stmt& q) {
    DbInvite inv;
    inv.code = q.col_text(0);
    inv.created_by_uid = q.col_int64(1);
    inv.created_by = q.col_text(2);
    inv.created_at = q.col_int64(3);
    inv.expires_at = q.col_int64(4);
    inv.max_uses = q.col_int(5);
    inv.uses = q.col_int(6);
    inv.for_uid = q.col_int64(7);
    inv.for_user = q.col_text(8);
    return inv;
}
} // namespace

std::optional<DbInvite> CommunityDb::create_invite(int64_t created_by_uid,
                                                   int64_t expires_at,
                                                   int32_t max_uses,
                                                   int64_t for_uid,
                                                   const std::string& for_user) {
    std::lock_guard<std::mutex> lock(mutex_);
    std::string creator;
    {
        Stmt q(db_, "SELECT username FROM members WHERE uid=?;");
        if (q.s) { q.bind_int64(1, created_by_uid); if (q.step() == SQLITE_ROW) creator = q.col_text(0); }
    }

    // Retry a handful of times on the astronomically unlikely event of a code
    // collision (10 chars of Crockford base32 ~= 2^50 entropy).
    for (int attempt = 0; attempt < 8; ++attempt) {
        DbInvite inv;
        inv.code = random_invite_code();
        inv.created_by_uid = created_by_uid;
        inv.created_by = creator;
        inv.created_at = now_seconds();
        inv.expires_at = expires_at;
        inv.max_uses = max_uses;
        inv.uses = 0;
        inv.for_uid = for_uid;
        inv.for_user = for_uid ? for_user : std::string();

        Stmt ins(db_,
            "INSERT INTO invites(code, created_by, created_by_uid, created_at, expires_at, "
            "  max_uses, uses, for_user, for_uid) "
            "VALUES(?, ?, ?, ?, ?, ?, 0, ?, ?);");
        if (!ins.s) return std::nullopt;
        ins.bind_text(1, inv.code);
        ins.bind_text(2, inv.created_by);
        ins.bind_int64(3, inv.created_by_uid);
        ins.bind_int64(4, inv.created_at);
        ins.bind_int64(5, inv.expires_at);
        ins.bind_int(6, inv.max_uses);
        ins.bind_text(7, inv.for_user);
        ins.bind_int64(8, inv.for_uid);
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

std::optional<DbInvite> CommunityDb::find_personal_invite(int64_t created_by_uid, int64_t for_uid) const {
    std::lock_guard<std::mutex> lock(mutex_);
    const std::string sql = std::string(kInviteSelect) +
        "WHERE i.created_by_uid=? AND i.for_uid=? "
        "  AND (i.expires_at = 0 OR i.expires_at > ?) AND (i.max_uses = 0 OR i.uses < i.max_uses) "
        "ORDER BY i.created_at DESC LIMIT 1;";
    Stmt q(db_, sql.c_str());
    if (!q.s) return std::nullopt;
    q.bind_int64(1, created_by_uid);
    q.bind_int64(2, for_uid);
    q.bind_int64(3, now_seconds());
    if (q.step() != SQLITE_ROW) return std::nullopt;
    return invite_from_row(q);
}

std::vector<DbInvite> CommunityDb::list_invites() const {
    std::lock_guard<std::mutex> lock(mutex_);
    std::vector<DbInvite> out;
    const std::string sql = std::string(kInviteSelect) + "ORDER BY i.created_at DESC;";
    Stmt q(db_, sql.c_str());
    if (!q.s) return out;
    while (q.step() == SQLITE_ROW) out.push_back(invite_from_row(q));
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
                                        int64_t redeeming_uid,
                                        const std::string& redeeming_name,
                                        DbInvite* out_invite) {
    std::lock_guard<std::mutex> lock(mutex_);

    // Ban check first — banned users can never redeem, regardless of invite.
    if (ban_active_unlocked(db_, redeeming_uid, redeeming_name, nullptr)) return InviteResult::Banned;

    // If already a member, the invite code is moot — treat as success and skip
    // the uses increment so invites aren't wasted on double-joins.
    {
        Stmt q(db_, "SELECT 1 FROM members WHERE uid=?;");
        if (q.s) {
            q.bind_int64(1, redeeming_uid);
            if (q.step() == SQLITE_ROW) return InviteResult::AlreadyMember;
        }
    }

    // Not guarded like the other transactions (DB1): each path here performs a
    // single mutation (one UPDATE or one DELETE), so an unchecked BEGIN can't
    // leave partial state, and with the busy_timeout the statement still lands
    // atomically under external contention rather than failing spuriously.
    exec_sql(db_, "BEGIN IMMEDIATE;");

    DbInvite inv;
    {
        const std::string sql = std::string(kInviteSelect) + "WHERE i.code=?;";
        Stmt q(db_, sql.c_str());
        if (!q.s) { exec_sql(db_, "ROLLBACK;"); return InviteResult::Unknown; }
        q.bind_text(1, code);
        if (q.step() != SQLITE_ROW) { exec_sql(db_, "ROLLBACK;"); return InviteResult::Unknown; }
        inv = invite_from_row(q);
    }

    // A personal invite only admits the account it was made for — and isn't
    // spent by anyone else trying it.
    if (inv.for_uid != 0 && inv.for_uid != redeeming_uid) {
        exec_sql(db_, "ROLLBACK;");
        return InviteResult::NotForYou;
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
        "  retention_days_document, retention_days_audio, slowmode_seconds, encrypted, perm_synced, "
        "  category_sync, topic "
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
        c.slowmode_seconds        = q.col_int(10);
        c.encrypted               = q.col_int(11) != 0;
        c.perm_synced             = q.col_int(12) != 0;
        c.category_sync           = q.col_int(13) != 0;
        c.topic                   = q.col_text(14);
        out.push_back(std::move(c));
    }
    return out;
}

std::optional<DbChannel> CommunityDb::get_channel(const std::string& channel_id) const {
    std::lock_guard<std::mutex> lock(mutex_);
    Stmt q(db_,
        "SELECT id, name, type, position, voice_bitrate_kbps, "
        "  retention_days_text, retention_days_image, retention_days_video, "
        "  retention_days_document, retention_days_audio, slowmode_seconds, encrypted, perm_synced, "
        "  category_sync, topic "
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
    c.slowmode_seconds        = q.col_int(10);
    c.encrypted               = q.col_int(11) != 0;
    c.perm_synced             = q.col_int(12) != 0;
    c.category_sync           = q.col_int(13) != 0;
    c.topic                   = q.col_text(14);
    return c;
}

namespace {
// Filesystem-safe slug for channel ids: lowercase alnum with single
// dashes, trimmed, capped at 32 chars. The id doubles as the channel's
// attachment directory name, so nothing outside [a-z0-9-] survives.
std::string slugify_channel_name(const std::string& name) {
    std::string out;
    out.reserve(name.size());
    bool last_dash = true;  // suppress a leading dash
    for (char c : name) {
        const unsigned char uc = static_cast<unsigned char>(c);
        if ((uc >= 'a' && uc <= 'z') || (uc >= '0' && uc <= '9')) {
            out.push_back(static_cast<char>(uc));
            last_dash = false;
        } else if (uc >= 'A' && uc <= 'Z') {
            out.push_back(static_cast<char>(uc - 'A' + 'a'));
            last_dash = false;
        } else if (!last_dash) {
            out.push_back('-');
            last_dash = true;
        }
        if (out.size() >= 32) break;
    }
    while (!out.empty() && out.back() == '-') out.pop_back();
    if (out.empty()) out = "channel";
    return out;
}
} // namespace

std::optional<DbChannel> CommunityDb::create_channel(const std::string& raw_name,
                                                     int32_t type,
                                                     int32_t voice_bitrate_kbps,
                                                     const std::string& category_id,
                                                     bool encrypted) {
    const std::string name = clamp_utf8(raw_name, kMaxChannelNameBytes);
    if (name.empty() || (type != 0 && type != 1 && type != 2)) return std::nullopt;
    std::lock_guard<std::mutex> lock(mutex_);

    // Unique id: the slug, or slug-2, slug-3... on collision.
    const std::string base = slugify_channel_name(name);
    std::string id = base;
    for (int suffix = 2; suffix < 1000; ++suffix) {
        Stmt q(db_, "SELECT 1 FROM channels WHERE id=?;");
        if (!q.s) return std::nullopt;
        q.bind_text(1, id);
        if (q.step() != SQLITE_ROW) break;
        id = base + "-" + std::to_string(suffix);
    }

    // Compute the insertion INDEX in the ordered list (see the header
    // comment). Deletions leave gaps in the stored positions, so index
    // space and position space diverge — the insert below renumbers
    // every row densely (0..N) rather than shifting stored positions,
    // which healed a bug where "append at end" landed mid-list after a
    // delete.
    struct Row { std::string id; int32_t type; };
    std::vector<Row> rows;
    {
        Stmt q(db_,
            "SELECT id, type FROM channels ORDER BY position ASC, id ASC;");
        if (!q.s) return std::nullopt;
        while (q.step() == SQLITE_ROW) {
            rows.push_back({ q.col_text(0), q.col_int(1) });
        }
    }

    size_t idx = rows.size();  // default: very end
    if (type != 2) {
        if (category_id.empty()) {
            // End of the uncategorized area = first CATEGORY row.
            for (size_t i = 0; i < rows.size(); ++i) {
                if (rows[i].type == 2) { idx = i; break; }
            }
        } else {
            // End of the target category's block = the next CATEGORY
            // row after it.
            bool found = false;
            idx = rows.size();
            for (size_t i = 0; i < rows.size(); ++i) {
                if (!found) {
                    if (rows[i].id == category_id && rows[i].type == 2) {
                        found = true;
                    }
                } else if (rows[i].type == 2) {
                    idx = i;
                    break;
                }
            }
            if (!found) return std::nullopt;  // unknown category
        }
    }

    if (!exec_sql(db_, "BEGIN IMMEDIATE;")) return std::nullopt;   // DB1
    bool ok = true;
    // Dense renumber around the insertion point: rows before idx keep
    // 0..idx-1, rows from idx on move to idx+1.., the new row takes idx.
    for (size_t i = 0; i < rows.size() && ok; ++i) {
        Stmt upd(db_, "UPDATE channels SET position=? WHERE id=?;");
        ok = upd.s != nullptr;
        if (ok) {
            upd.bind_int(1, static_cast<int32_t>(i < idx ? i : i + 1));
            upd.bind_text(2, rows[i].id);
            ok = upd.step() == SQLITE_DONE;
        }
    }
    // A new channel inside a syncing category follows it from the start
    // (v3); anywhere else it starts with no permissions of its own.
    if (!layout_valid_) rebuild_channel_layout_unlocked_();
    const auto cat_sync = category_sync_of_.find(category_id);
    const bool start_synced = type != 2 && cat_sync != category_sync_of_.end() && cat_sync->second;
    if (ok) {
        Stmt ins(db_,
            "INSERT INTO channels(id, name, type, position, voice_bitrate_kbps, perm_synced, encrypted) "
            "VALUES(?, ?, ?, ?, ?, ?, ?);");
        ok = ins.s != nullptr;
        if (ok) {
            ins.bind_text(1, id);
            ins.bind_text(2, name);
            ins.bind_int(3, type);
            ins.bind_int(4, static_cast<int32_t>(idx));
            ins.bind_int(5, type == 1 ? voice_bitrate_kbps : 0);
            ins.bind_int(6, start_synced ? 1 : 0);
            // Text channels only; fixed for the channel's life.
            ins.bind_int(7, type == 0 && encrypted ? 1 : 0);
            ok = ins.step() == SQLITE_DONE;
        }
    }
    exec_sql(db_, ok ? "COMMIT;" : "ROLLBACK;");
    if (!ok) return std::nullopt;
    normalize_channel_order_();
    // Drop cached channel-permission resolutions: any user who probed this
    // slug before it existed cached a "(user, id) → 0" (VIEW denied) entry,
    // which would keep the freshly-created channel invisible to them until an
    // unrelated cache-clearing mutation. A delete+recreate under the same
    // slug is the common trigger (D1). The new row also shifts the
    // category layout (it starts synced to its category).
    invalidate_channel_layout_();

    DbChannel c;
    c.id = id;
    c.name = name;
    c.type = type;
    c.position = static_cast<int32_t>(idx);
    c.voice_bitrate_kbps = type == 1 ? voice_bitrate_kbps : 0;
    c.perm_synced = start_synced;
    c.encrypted = type == 0 && encrypted;
    return c;
}

void CommunityDb::normalize_channel_order_() {
    struct Row { std::string id; int32_t type; };
    std::vector<Row> rows;
    {
        Stmt q(db_,
            "SELECT id, type FROM channels ORDER BY position ASC, id ASC;");
        if (!q.s) return;
        while (q.step() == SQLITE_ROW) {
            rows.push_back({ q.col_text(0), q.col_int(1) });
        }
    }

    // Rebuild the order group by group: text channels first, then
    // voice, each keeping their relative order; category headers
    // delimit the groups and stay where they are.
    std::vector<std::string> out;
    out.reserve(rows.size());
    auto flush_group = [&](size_t start, size_t end) {
        for (size_t j = start; j < end; ++j) {
            if (rows[j].type == 0) out.push_back(rows[j].id);
        }
        for (size_t j = start; j < end; ++j) {
            if (rows[j].type != 0 && rows[j].type != 2) out.push_back(rows[j].id);
        }
    };
    size_t group_start = 0;
    for (size_t j = 0; j < rows.size(); ++j) {
        if (rows[j].type == 2) {
            flush_group(group_start, j);
            out.push_back(rows[j].id);
            group_start = j + 1;
        }
    }
    flush_group(group_start, rows.size());

    if (!exec_sql(db_, "BEGIN IMMEDIATE;")) return;   // DB1
    bool ok = true;
    for (size_t i = 0; i < out.size() && ok; ++i) {
        Stmt upd(db_, "UPDATE channels SET position=? WHERE id=?;");
        ok = upd.s != nullptr;
        if (ok) {
            upd.bind_int(1, static_cast<int32_t>(i));
            upd.bind_text(2, out[i]);
            ok = upd.step() == SQLITE_DONE;
        }
    }
    exec_sql(db_, ok ? "COMMIT;" : "ROLLBACK;");
    invalidate_channel_layout_();
}

bool CommunityDb::reorder_channels(const std::vector<std::string>& ordered_ids) {
    std::lock_guard<std::mutex> lock(mutex_);

    // The requested set must exactly match the table — same count, no
    // unknowns, no duplicates — so a reorder raced by a create/delete
    // fails whole instead of scrambling positions.
    {
        std::set<std::string> requested(ordered_ids.begin(), ordered_ids.end());
        if (requested.size() != ordered_ids.size()) return false;
        std::set<std::string> current;
        Stmt q(db_, "SELECT id FROM channels;");
        if (!q.s) return false;
        while (q.step() == SQLITE_ROW) current.insert(q.col_text(0));
        if (current != requested) return false;
    }

    // Who follows which category before the move (v3).
    if (!layout_valid_) rebuild_channel_layout_unlocked_();
    std::vector<std::pair<std::string, std::string>> followed;   // channel → category
    for (const auto& [id, parent] : parent_of_) {
        if (!parent.empty() && overwrite_source_unlocked_(id) == parent) followed.emplace_back(id, parent);
    }

    if (!exec_sql(db_, "BEGIN IMMEDIATE;")) return false;   // DB1
    bool ok = true;
    for (size_t i = 0; i < ordered_ids.size() && ok; ++i) {
        Stmt upd(db_, "UPDATE channels SET position=? WHERE id=?;");
        ok = upd.s != nullptr;
        if (ok) {
            upd.bind_int(1, static_cast<int32_t>(i));
            upd.bind_text(2, ordered_ids[i]);
            ok = upd.step() == SQLITE_DONE;
        }
    }
    exec_sql(db_, ok ? "COMMIT;" : "ROLLBACK;");
    invalidate_channel_layout_();
    if (!ok) return false;
    normalize_channel_order_();

    // A move never changes what a channel allows: one that followed a
    // category and now sits under another (or none) keeps that category's
    // rows as its own. Syncing with the new category is a separate,
    // explicit CHANNEL_PERMISSIONS_SYNC_REQ (the client asks on drop).
    rebuild_channel_layout_unlocked_();
    std::vector<std::pair<std::string, std::string>> moved;
    for (const auto& [id, old_parent] : followed) {
        if (parent_category_unlocked_(id) != old_parent) moved.emplace_back(id, old_parent);
    }
    if (!moved.empty() && exec_sql(db_, "BEGIN IMMEDIATE;")) {
        bool kept = true;
        for (const auto& [id, old_parent] : moved) {
            const std::vector<DbOverwrite> rows = overwrites_unlocked_(old_parent);
            kept = write_own_overwrites(db_, id, rows, /*synced=*/false);
            overwrites_cache_.erase(id);
            if (!kept) break;
        }
        exec_sql(db_, kept ? "COMMIT;" : "ROLLBACK;");
        invalidate_channel_layout_();
    }
    enforce_sync_invariant_unlocked_();
    return true;
}

bool CommunityDb::rename_channel(const std::string& channel_id,
                                 const std::string& raw_name) {
    const std::string name = clamp_utf8(raw_name, kMaxChannelNameBytes);
    if (name.empty()) return false;
    std::lock_guard<std::mutex> lock(mutex_);
    Stmt q(db_, "UPDATE channels SET name=? WHERE id=?;");
    if (!q.s) return false;
    q.bind_text(1, name);
    q.bind_text(2, channel_id);
    if (q.step() != SQLITE_DONE) return false;
    return sqlite3_changes(db_) > 0;
}

int64_t CommunityDb::count_channels_of_type(int32_t type) const {
    std::lock_guard<std::mutex> lock(mutex_);
    Stmt q(db_, "SELECT COUNT(*) FROM channels WHERE type=?;");
    if (!q.s) return 0;
    q.bind_int(1, type);
    if (q.step() != SQLITE_ROW) return 0;
    return q.col_int64(0);
}

std::optional<CommunityDb::WipeChannelResult> CommunityDb::delete_channel(
    const std::string& channel_id) {
    std::lock_guard<std::mutex> lock(mutex_);
    {
        Stmt q(db_, "SELECT 1 FROM channels WHERE id=?;");
        if (!q.s) return std::nullopt;
        q.bind_text(1, channel_id);
        if (q.step() != SQLITE_ROW) return std::nullopt;
    }

    // Deleting a category (v3): its synced channels would otherwise start
    // following whichever category ends up above them — a private
    // category's channels could turn public. Give them the category's
    // rows as their own first.
    std::vector<std::string> orphans;
    std::vector<DbOverwrite> category_rows;
    if (!layout_valid_) rebuild_channel_layout_unlocked_();
    for (const auto& [id, parent] : parent_of_) {
        if (parent == channel_id && overwrite_source_unlocked_(id) == channel_id) orphans.push_back(id);
    }
    if (!orphans.empty()) category_rows = overwrites_unlocked_(channel_id);

    WipeChannelResult out;
    // Collect blob paths before the rows go (mirrors wipe_channel).
    {
        Stmt q(db_,
            "SELECT COALESCE(storage_path, '') FROM attachments "
            "WHERE channel_id=?;");
        if (!q.s) return std::nullopt;
        q.bind_text(1, channel_id);
        while (q.step() == SQLITE_ROW) {
            std::string p = q.col_text(0);
            if (!p.empty()) out.unlink_paths.push_back(std::move(p));
        }
    }

    if (!exec_sql(db_, "BEGIN IMMEDIATE;")) return std::nullopt;
    bool ok = true;
    for (const auto& child : orphans) {
        if (!ok) break;
        ok = write_own_overwrites(db_, child, category_rows, /*synced=*/false);
        overwrites_cache_.erase(child);
    }
    {
        // Key escrow goes with the channel.
        Stmt del(db_, "DELETE FROM channel_key_blobs WHERE channel_id=?;");
        if (del.s) { del.bind_text(1, channel_id); del.step(); }
    }
    {
        Stmt del(db_, "DELETE FROM channel_key_epochs WHERE channel_id=?;");
        if (del.s) { del.bind_text(1, channel_id); del.step(); }
    }
    {
        Stmt del(db_, "DELETE FROM attachments WHERE channel_id=?;");
        ok = del.s != nullptr;
        if (ok) {
            del.bind_text(1, channel_id);
            ok = del.step() == SQLITE_DONE;
            if (ok) out.deleted_attachment_count = sqlite3_changes(db_);
        }
    }
    if (ok) {
        Stmt del(db_, "DELETE FROM messages WHERE channel_id=?;");
        ok = del.s != nullptr;
        if (ok) {
            del.bind_text(1, channel_id);
            ok = del.step() == SQLITE_DONE;
            if (ok) out.deleted_message_count = sqlite3_changes(db_);
        }
    }
    if (ok) {
        Stmt del(db_, "DELETE FROM channel_overwrites WHERE channel_id=?;");
        ok = del.s != nullptr;
        if (ok) {
            del.bind_text(1, channel_id);
            ok = del.step() == SQLITE_DONE;
        }
    }
    if (ok) {
        Stmt del(db_, "DELETE FROM channels WHERE id=?;");
        ok = del.s != nullptr;
        if (ok) {
            del.bind_text(1, channel_id);
            ok = del.step() == SQLITE_DONE;
        }
    }
    exec_sql(db_, ok ? "COMMIT;" : "ROLLBACK;");
    overwrites_cache_.erase(channel_id);
    invalidate_channel_layout_();
    if (!ok) return std::nullopt;
    normalize_channel_order_();
    return out;
}

bool CommunityDb::set_nickname(int64_t uid, const std::string& raw_nickname) {
    const std::string nickname = clamp_utf8(raw_nickname, kMaxNicknameBytes);
    std::lock_guard<std::mutex> lock(mutex_);
    Stmt q(db_, "UPDATE members SET nickname=? WHERE uid=?;");
    if (!q.s) return false;
    q.bind_text(1, nickname);
    q.bind_int64(2, uid);
    if (q.step() != SQLITE_DONE) return false;
    return sqlite3_changes(db_) > 0;
}

bool CommunityDb::set_channel_slowmode(const std::string& channel_id, int32_t seconds) {
    if (seconds < 0) seconds = 0;
    if (seconds > 21600) seconds = 21600;
    std::lock_guard<std::mutex> lock(mutex_);
    Stmt q(db_, "UPDATE channels SET slowmode_seconds=? WHERE id=? AND type=0;");
    if (!q.s) return false;
    q.bind_int(1, seconds);
    q.bind_text(2, channel_id);
    if (q.step() != SQLITE_DONE) return false;
    return sqlite3_changes(db_) > 0;
}

bool CommunityDb::set_channel_topic(const std::string& channel_id, const std::string& topic) {
    std::lock_guard<std::mutex> lock(mutex_);
    Stmt q(db_, "UPDATE channels SET topic=? WHERE id=? AND type=0;");
    if (!q.s) return false;
    q.bind_text(1, topic);
    q.bind_text(2, channel_id);
    if (q.step() != SQLITE_DONE) return false;
    return sqlite3_changes(db_) > 0;
}


bool CommunityDb::set_channel_voice_bitrate(const std::string& channel_id,
                                            int32_t kbps) {
    if (kbps < 0) kbps = 0;
    if (kbps > 512) kbps = 512;
    std::lock_guard<std::mutex> lock(mutex_);
    Stmt q(db_,
        "UPDATE channels SET voice_bitrate_kbps=? WHERE id=? AND type=1;");
    if (!q.s) return false;
    q.bind_int(1, kbps);
    q.bind_text(2, channel_id);
    if (q.step() != SQLITE_DONE) return false;
    return sqlite3_changes(db_) > 0;
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
                                    int64_t sender_uid,
                                    const std::string& sender,
                                    const std::string& content,
                                    int64_t timestamp,
                                    int64_t reply_to,
                                    const std::string& envelope,
                                    bool suppress_embeds) {
    std::lock_guard<std::mutex> lock(mutex_);
    // Only accept a reply_to that points at a real message in THIS channel;
    // otherwise store 0 (ignore a stale/cross-channel/forged reference).
    if (reply_to > 0) {
        Stmt chk(db_, "SELECT 1 FROM messages WHERE id=? AND channel_id=?;");
        bool ok = false;
        if (chk.s) {
            chk.bind_int64(1, reply_to);
            chk.bind_text(2, channel_id);
            ok = chk.step() == SQLITE_ROW;
        }
        if (!ok) reply_to = 0;
    }
    Stmt q(db_,
        "INSERT INTO messages(channel_id, sender, content, timestamp, reply_to, envelope, suppress_embeds, sender_uid) "
        "VALUES(?, ?, ?, ?, ?, ?, ?, ?);");
    if (!q.s) return 0;
    q.bind_text(1, channel_id);
    q.bind_text(2, sender);
    q.bind_text(3, content);
    q.bind_int64(4, timestamp);
    q.bind_int64(5, reply_to);
    q.bind_blob_or_null(6, envelope);
    q.bind_int(7, suppress_embeds ? 1 : 0);
    q.bind_int64(8, sender_uid);
    if (q.step() != SQLITE_DONE) return 0;
    return sqlite3_last_insert_rowid(db_);
}

namespace {
// Canonical message SELECT: the message columns plus the reply parent's
// sender/content via LEFT JOIN (COALESCEd to '' when not a reply or when the
// parent was deleted). Each sender comes as its uid, its current name (the
// member's; the sent-under name once they left) and the name it was sent
// under. Kept as one constant so every fetch path returns the same shape —
// read_message_row expects exactly this column order.
constexpr const char* kMessageSelect =
    "SELECT m.id, m.channel_id, m.sender_uid, COALESCE(sm.username, m.sender), m.sender, "
    "m.content, m.timestamp, m.edited_at, m.reply_to, "
    "COALESCE(p.sender_uid, 0), COALESCE(pm.username, p.sender, ''), COALESCE(p.sender, ''), "
    "COALESCE(p.content, ''), "
    // Parent's attachment kinds as "0,1,3" in position order ('' when none /
    // not a reply) — parsed by parse_kind_list below.
    "COALESCE((SELECT GROUP_CONCAT(kind, ',') FROM "
    "  (SELECT kind FROM attachments WHERE message_id = p.id ORDER BY position)), ''), "
    "m.envelope, p.envelope, m.suppress_embeds "
    "FROM messages m LEFT JOIN messages p "
    "ON p.id = m.reply_to AND p.channel_id = m.channel_id "
    "LEFT JOIN members sm ON sm.uid = m.sender_uid "
    "LEFT JOIN members pm ON pm.uid = p.sender_uid ";

// "0,1,3" → {0,1,3}; "" → {}. Digits only, so a malformed token is skipped.
std::vector<int32_t> parse_kind_list(const std::string& s) {
    std::vector<int32_t> out;
    int32_t v = 0;
    bool any = false;
    for (char c : s) {
        if (c >= '0' && c <= '9') {
            v = v * 10 + (c - '0');
            any = true;
        } else if (c == ',') {
            if (any) out.push_back(v);
            v = 0;
            any = false;
        }
    }
    if (any) out.push_back(v);
    return out;
}

// Reads the columns of kMessageSelect (in order) into a DbMessage.
DbMessage read_message_row(Stmt& q) {
    DbMessage m;
    m.id = q.col_int64(0);
    m.channel_id = q.col_text(1);
    m.sender_uid = q.col_int64(2);
    m.sender = q.col_text(3);
    m.sent_sender = q.col_text(4);
    m.content = q.col_text(5);
    m.timestamp = q.col_int64(6);
    m.edited_at = q.col_int64(7);
    m.reply_to = q.col_int64(8);
    m.reply_to_sender_uid = q.col_int64(9);
    m.reply_to_sender = q.col_text(10);
    m.reply_to_sent_sender = q.col_text(11);
    m.reply_to_content = q.col_text(12);
    m.reply_to_attachment_kinds = parse_kind_list(q.col_text(13));
    m.envelope = q.col_blob(14);
    m.reply_to_envelope = q.col_blob(15);
    m.suppress_embeds = q.col_int(16) != 0;
    return m;
}
} // namespace

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
    const std::string sql =
        before_id > 0
            ? std::string(kMessageSelect) +
                  "WHERE m.channel_id=? AND m.id<? ORDER BY m.id DESC LIMIT ?;"
            : std::string(kMessageSelect) +
                  "WHERE m.channel_id=? ORDER BY m.id DESC LIMIT ?;";
    Stmt q(db_, sql.c_str());
    if (!q.s) return out;
    q.bind_text(1, channel_id);
    int next_idx = 2;
    if (before_id > 0) {
        q.bind_int64(next_idx++, before_id);
    }
    q.bind_int(next_idx, fetch);

    while (q.step() == SQLITE_ROW) out.push_back(read_message_row(q));

    if (static_cast<int32_t>(out.size()) > limit) {
        out.pop_back();
        if (has_more) *has_more = true;
    }
    return out;
}

std::vector<DbMessage> CommunityDb::fetch_messages_around(const std::string& channel_id,
                                                         int64_t around_id,
                                                         int32_t limit,
                                                         bool* has_more_before,
                                                         bool* has_more_after) const {
    if (has_more_before) *has_more_before = false;
    if (has_more_after) *has_more_after = false;
    if (limit <= 0) limit = 25;
    if (limit > 100) limit = 100;
    std::lock_guard<std::mutex> lock(mutex_);

    // The target and older, newest-first; one extra to detect more older.
    std::vector<DbMessage> older;  // desc: around_id, then older
    {
        const std::string sql = std::string(kMessageSelect) +
            "WHERE m.channel_id=? AND m.id<=? ORDER BY m.id DESC LIMIT ?;";
        Stmt q(db_, sql.c_str());
        if (q.s) {
            q.bind_text(1, channel_id);
            q.bind_int64(2, around_id);
            q.bind_int(3, limit + 1);
            while (q.step() == SQLITE_ROW) older.push_back(read_message_row(q));
        }
    }
    if (static_cast<int32_t>(older.size()) > limit) {
        older.pop_back();
        if (has_more_before) *has_more_before = true;
    }

    // Strictly newer, oldest-first; one extra to detect more newer.
    std::vector<DbMessage> newer;  // asc
    {
        const std::string sql = std::string(kMessageSelect) +
            "WHERE m.channel_id=? AND m.id>? ORDER BY m.id ASC LIMIT ?;";
        Stmt q(db_, sql.c_str());
        if (q.s) {
            q.bind_text(1, channel_id);
            q.bind_int64(2, around_id);
            q.bind_int(3, limit + 1);
            while (q.step() == SQLITE_ROW) newer.push_back(read_message_row(q));
        }
    }
    if (static_cast<int32_t>(newer.size()) > limit) {
        newer.pop_back();
        if (has_more_after) *has_more_after = true;
    }

    // Combine oldest→newest: reverse(older) then newer.
    std::vector<DbMessage> out;
    out.reserve(older.size() + newer.size());
    out.assign(older.rbegin(), older.rend());
    for (auto& m : newer) out.push_back(std::move(m));
    return out;
}

std::vector<DbMessage> CommunityDb::fetch_messages_after(const std::string& channel_id,
                                                        int64_t after_id,
                                                        int32_t limit,
                                                        bool* has_more_after) const {
    if (has_more_after) *has_more_after = false;
    if (limit <= 0) limit = 50;
    if (limit > 200) limit = 200;
    std::lock_guard<std::mutex> lock(mutex_);
    std::vector<DbMessage> out;  // asc = oldest→newest
    const std::string sql = std::string(kMessageSelect) +
        "WHERE m.channel_id=? AND m.id>? ORDER BY m.id ASC LIMIT ?;";
    Stmt q(db_, sql.c_str());
    if (!q.s) return out;
    q.bind_text(1, channel_id);
    q.bind_int64(2, after_id);
    q.bind_int(3, limit + 1);
    while (q.step() == SQLITE_ROW) out.push_back(read_message_row(q));
    if (static_cast<int32_t>(out.size()) > limit) {
        out.pop_back();
        if (has_more_after) *has_more_after = true;
    }
    return out;
}

std::vector<DbMessage> CommunityDb::fetch_messages_by_ids(const std::vector<int64_t>& ids) const {
    std::vector<DbMessage> out;
    if (ids.empty()) return out;
    const size_t n = std::min<size_t>(ids.size(), 200);
    std::string placeholders;
    placeholders.reserve(n * 2);
    for (size_t i = 0; i < n; ++i) {
        if (i > 0) placeholders.push_back(',');
        placeholders.push_back('?');
    }
    const std::string sql = std::string(kMessageSelect) + "WHERE m.id IN (" + placeholders + ");";
    std::unordered_map<int64_t, DbMessage> by_id;
    {
        std::lock_guard<std::mutex> lock(mutex_);
        Stmt q(db_, sql.c_str());
        if (!q.s) return out;
        for (size_t i = 0; i < n; ++i) q.bind_int64(static_cast<int>(i + 1), ids[i]);
        while (q.step() == SQLITE_ROW) {
            DbMessage m = read_message_row(q);
            by_id.emplace(m.id, std::move(m));
        }
    }
    out.reserve(by_id.size());
    for (size_t i = 0; i < n; ++i) {
        auto it = by_id.find(ids[i]);
        if (it != by_id.end()) out.push_back(std::move(it->second));
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
        "  upload_status, expected_size, uploader_uid, width, height, "
        "  thumbnail_size_bytes, thumbnail_sizes_mask, duration_ms, "
        "  COALESCE(placeholder, ''), encrypted "
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
        a.uploader_uid = q.col_int64(12);
        a.width = q.col_int(13);
        a.height = q.col_int(14);
        a.thumbnail_size_bytes = q.col_int64(15);
        a.thumbnail_sizes_mask = q.col_int(16);
        a.duration_ms = q.col_int(17);
        a.placeholder = q.col_text(18);
        a.encrypted = q.col_int(19) != 0;
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
                                               int64_t uploader_uid,
                                               int32_t position,
                                               int32_t width,
                                               int32_t height,
                                               int32_t duration_ms,
                                               const std::string& placeholder,
                                               bool encrypted) {
    std::lock_guard<std::mutex> lock(mutex_);
    Stmt q(db_,
        "INSERT INTO attachments("
        "  message_id, kind, filename, mime, size_bytes, storage_path, "
        "  position, created_at, purged_at, upload_status, expected_size, "
        "  uploader_uid, channel_id, width, height, duration_ms, placeholder, encrypted"
        ") VALUES(0, ?, ?, ?, 0, ?, ?, ?, 0, 'uploading', ?, ?, ?, ?, ?, ?, ?, ?);");
    if (!q.s) {
        std::cerr << "[DB] insert_pending_attachment prepare failed: "
                  << sqlite3_errmsg(db_) << "\n";
        return 0;
    }
    // Clamp dimensions defensively — we don't want client-supplied values
    // to end up negative, and absurdly large numbers are almost certainly
    // junk that shouldn't drive placeholder sizing.
    const int32_t w = width  < 0 ? 0 : (width  > 16384 ? 16384 : width);
    const int32_t h = height < 0 ? 0 : (height > 16384 ? 16384 : height);
    q.bind_int(1, kind);
    q.bind_text(2, filename);
    q.bind_text(3, mime);
    q.bind_text(4, storage_path);
    q.bind_int(5, position);
    q.bind_int64(6, now_seconds());
    q.bind_int64(7, expected_size);
    q.bind_int64(8, uploader_uid);
    q.bind_text(9, channel_id);
    q.bind_int(10, w);
    q.bind_int(11, h);
    // Clamp duration to non-negative; we don't cap an upper bound — even
    // a 24-hour audio attachment fits comfortably in int32 ms.
    q.bind_int(12, duration_ms < 0 ? 0 : duration_ms);
    // Opaque base64 blob. Length-capped so a malicious client can't use
    // it as unbounded storage — a real ThumbHash is ~34 base64 chars.
    q.bind_text(13, placeholder.size() > 128 ? std::string() : placeholder);
    q.bind_int(14, encrypted ? 1 : 0);
    int rc = q.step();
    if (rc != SQLITE_DONE) {
        std::cerr << "[DB] insert_pending_attachment step failed (rc=" << rc
                  << "): " << sqlite3_errmsg(db_) << "\n";
        return 0;
    }
    return sqlite3_last_insert_rowid(db_);
}

std::optional<DbAttachment> CommunityDb::get_attachment(int64_t attachment_id) const {
    std::lock_guard<std::mutex> lock(mutex_);
    Stmt q(db_,
        "SELECT id, message_id, kind, filename, mime, size_bytes, "
        "  COALESCE(storage_path, ''), position, created_at, purged_at, "
        "  upload_status, expected_size, uploader_uid, channel_id, width, height, "
        "  thumbnail_size_bytes, thumbnail_sizes_mask, duration_ms, "
        "  COALESCE(placeholder, ''), encrypted "
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
    a.uploader_uid = q.col_int64(12);
    a.channel_id = q.col_text(13);
    a.width = q.col_int(14);
    a.height = q.col_int(15);
    a.thumbnail_size_bytes = q.col_int64(16);
    a.thumbnail_sizes_mask = q.col_int(17);
    a.duration_ms = q.col_int(18);
    a.placeholder = q.col_text(19);
    a.encrypted = q.col_int(20) != 0;
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

bool CommunityDb::set_attachment_thumbnail_size(int64_t attachment_id, int64_t size) {
    std::lock_guard<std::mutex> lock(mutex_);
    Stmt q(db_, "UPDATE attachments SET thumbnail_size_bytes=? WHERE id=?;");
    if (!q.s) return false;
    q.bind_int64(1, size < 0 ? 0 : size);
    q.bind_int64(2, attachment_id);
    if (q.step() != SQLITE_DONE) return false;
    return sqlite3_changes(db_) > 0;
}

bool CommunityDb::add_attachment_thumbnail_size(int64_t attachment_id,
                                                int32_t size_bit,
                                                int64_t bytes) {
    if (size_bit == 0 || bytes < 0) return false;
    std::lock_guard<std::mutex> lock(mutex_);
    // OR the bit and accumulate bytes. Re-uploads of the same size add
    // their new byte count again — the total drifts slightly upward but
    // it's only used as a presence/sizing hint, not for billing.
    Stmt q(db_,
        "UPDATE attachments "
        "SET thumbnail_size_bytes = thumbnail_size_bytes + ?, "
        "    thumbnail_sizes_mask = thumbnail_sizes_mask | ? "
        "WHERE id=?;");
    if (!q.s) return false;
    q.bind_int64(1, bytes);
    q.bind_int(2, size_bit);
    q.bind_int64(3, attachment_id);
    if (q.step() != SQLITE_DONE) return false;
    return sqlite3_changes(db_) > 0;
}

std::vector<int64_t> CommunityDb::bind_attachments(const std::vector<int64_t>& attachment_ids,
                                                    int64_t message_id,
                                                    const std::string& channel_id,
                                                    int64_t uploader_uid,
                                                    bool require_encrypted) {
    std::vector<int64_t> bound;
    if (attachment_ids.empty()) return bound;

    std::string placeholders;
    for (size_t i = 0; i < attachment_ids.size(); ++i) {
        if (i > 0) placeholders.push_back(',');
        placeholders.push_back('?');
    }

    std::lock_guard<std::mutex> lock(mutex_);
    if (!exec_sql(db_, "BEGIN IMMEDIATE;")) return {};   // DB1
    // Collect the ids that are actually eligible for binding. A UPDATE ...
    // RETURNING would do this in one shot but we stay portable to older SQLite.
    {
        const std::string select_sql =
            "SELECT id FROM attachments "
            "WHERE id IN (" + placeholders + ") "
            "  AND upload_status='ready' "
            "  AND message_id=0 "
            "  AND uploader_uid=? "
            "  AND channel_id=? "
            "  AND encrypted=?;";
        Stmt q(db_, select_sql.c_str());
        if (!q.s) { exec_sql(db_, "ROLLBACK;"); return bound; }
        for (size_t i = 0; i < attachment_ids.size(); ++i) {
            q.bind_int64(static_cast<int>(i + 1), attachment_ids[i]);
        }
        q.bind_int64(static_cast<int>(attachment_ids.size() + 1), uploader_uid);
        q.bind_text(static_cast<int>(attachment_ids.size() + 2), channel_id);
        q.bind_int(static_cast<int>(attachment_ids.size() + 3), require_encrypted ? 1 : 0);
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

std::vector<DbAttachment> CommunityDb::list_stale_pending_attachments(
    int64_t uploading_cutoff_ts, int64_t ready_cutoff_ts) const {
    std::vector<DbAttachment> out;
    std::lock_guard<std::mutex> lock(mutex_);
    // Anything not 'uploading' gets the longer ready-cutoff — a finished
    // upload waiting in a compose box must not vanish after an hour.
    Stmt q(db_,
        "SELECT id, message_id, kind, filename, mime, size_bytes, "
        "  COALESCE(storage_path, ''), position, created_at, purged_at, "
        "  upload_status, expected_size, uploader_uid, channel_id "
        "FROM attachments WHERE message_id=0 "
        "  AND ((upload_status='uploading' AND MAX(created_at, last_activity_at)<?) "
        "    OR (upload_status<>'uploading' AND created_at<?));");
    if (!q.s) return out;
    q.bind_int64(1, uploading_cutoff_ts);
    q.bind_int64(2, ready_cutoff_ts);
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
        a.uploader_uid = q.col_int64(12);
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

void CommunityDb::touch_attachment_activity(int64_t attachment_id, int64_t ts) {
    std::lock_guard<std::mutex> lock(mutex_);
    Stmt q(db_, "UPDATE attachments SET last_activity_at=? WHERE id=?;");
    if (!q.s) return;
    q.bind_int64(1, ts);
    q.bind_int64(2, attachment_id);
    q.step();
}

CommunityDb::WipeChannelResult CommunityDb::wipe_channel(const std::string& channel_id) {
    WipeChannelResult out;
    std::lock_guard<std::mutex> lock(mutex_);

    // Pull every storage_path the channel owns BEFORE we delete the
    // rows — once they're gone we can't reconstruct the disk paths.
    // Includes ready, uploading, and tombstoned rows; harmless to call
    // remove() on a path that's already missing.
    {
        Stmt q(db_,
            "SELECT COALESCE(storage_path, '') FROM attachments "
            "WHERE channel_id=?;");
        if (!q.s) return out;
        q.bind_text(1, channel_id);
        while (q.step() == SQLITE_ROW) {
            std::string p = q.col_text(0);
            if (!p.empty()) out.unlink_paths.push_back(std::move(p));
        }
    }

    // Wrap the destructive bit in a transaction so a failure mid-way
    // doesn't leave us with messages without their attachments (or vice
    // versa).
    if (!exec_sql(db_, "BEGIN;")) return out;

    auto rollback_and_return = [&](WipeChannelResult& r) -> WipeChannelResult& {
        exec_sql(db_, "ROLLBACK;");
        r.deleted_message_count = 0;
        r.deleted_attachment_count = 0;
        r.unlink_paths.clear();
        return r;
    };

    {
        Stmt del(db_, "DELETE FROM attachments WHERE channel_id=?;");
        if (!del.s) return rollback_and_return(out);
        del.bind_text(1, channel_id);
        if (del.step() != SQLITE_DONE) return rollback_and_return(out);
        out.deleted_attachment_count = sqlite3_changes(db_);
    }
    {
        Stmt del(db_, "DELETE FROM messages WHERE channel_id=?;");
        if (!del.s) return rollback_and_return(out);
        del.bind_text(1, channel_id);
        if (del.step() != SQLITE_DONE) return rollback_and_return(out);
        out.deleted_message_count = sqlite3_changes(db_);
    }

    if (!exec_sql(db_, "COMMIT;")) return rollback_and_return(out);
    return out;
}

// --- Per-message delete (see docs/superpowers/specs/
//     2026-05-15-message-deletion-design.md) ---

std::optional<CommunityDb::MessageSender> CommunityDb::get_message_sender(
    const std::string& channel_id, int64_t message_id) const {
    std::lock_guard<std::mutex> lock(mutex_);
    Stmt q(db_, "SELECT sender_uid, sender FROM messages WHERE id=? AND channel_id=?;");
    if (!q.s) return std::nullopt;
    q.bind_int64(1, message_id);
    q.bind_text(2, channel_id);
    if (q.step() == SQLITE_ROW) {
        return MessageSender{q.col_int64(0), q.col_text(1)};
    }
    return std::nullopt;
}

std::optional<CommunityDb::MessagePreview> CommunityDb::get_message_preview(
    const std::string& channel_id, int64_t message_id) const {
    std::lock_guard<std::mutex> lock(mutex_);
    MessagePreview out;
    {
        Stmt q(db_,
            "SELECT m.sender_uid, COALESCE(sm.username, m.sender), m.sender, m.content, m.envelope "
            "FROM messages m LEFT JOIN members sm ON sm.uid = m.sender_uid "
            "WHERE m.id=? AND m.channel_id=?;");
        if (!q.s) return std::nullopt;
        q.bind_int64(1, message_id);
        q.bind_text(2, channel_id);
        if (q.step() != SQLITE_ROW) return std::nullopt;
        out.sender_uid = q.col_int64(0);
        out.sender = q.col_text(1);
        out.sent_sender = q.col_text(2);
        out.content = q.col_text(3);
        out.envelope = q.col_blob(4);
    }
    Stmt a(db_, "SELECT kind FROM attachments WHERE message_id=? ORDER BY position;");
    if (a.s) {
        a.bind_int64(1, message_id);
        while (a.step() == SQLITE_ROW) {
            out.attachment_kinds.push_back(static_cast<int32_t>(a.col_int64(0)));
        }
    }
    return out;
}

bool CommunityDb::edit_message(const std::string& channel_id, int64_t message_id,
                               int64_t editor_uid,
                               const std::string& content,
                               int64_t edited_at, const std::string& envelope,
                               bool suppress_embeds) {
    std::lock_guard<std::mutex> lock(mutex_);
    // Ownership is enforced in the WHERE: the row updates only if it exists in
    // this channel AND was sent by the editor's account. changes()==0 → not
    // found or not the owner (indistinguishable to the caller, which is
    // fine). The envelope column is always rewritten (NULL for a plaintext
    // edit).
    Stmt q(db_,
        "UPDATE messages SET content=?, edited_at=?, envelope=?, suppress_embeds=? "
        "WHERE id=? AND channel_id=? AND sender_uid=?;");
    if (!q.s) return false;
    q.bind_text(1, content);
    q.bind_int64(2, edited_at);
    q.bind_blob_or_null(3, envelope);
    q.bind_int(4, suppress_embeds ? 1 : 0);
    q.bind_int64(5, message_id);
    q.bind_text(6, channel_id);
    q.bind_int64(7, editor_uid);
    if (q.step() != SQLITE_DONE) return false;
    return sqlite3_changes(db_) > 0;
}

// ─── Encrypted text channels: key escrow ─────────────────────────────────

uint32_t CommunityDb::channel_key_current_epoch(const std::string& channel_id) const {
    std::lock_guard<std::mutex> lock(mutex_);
    Stmt q(db_, "SELECT COALESCE(MAX(epoch), 0) FROM channel_key_epochs WHERE channel_id=?;");
    if (!q.s) return 0;
    q.bind_text(1, channel_id);
    if (q.step() != SQLITE_ROW) return 0;
    return static_cast<uint32_t>(q.col_int64(0));
}

bool CommunityDb::channel_key_create_epoch(const std::string& channel_id, uint32_t epoch,
                                           int64_t created_by_uid, int64_t now) {
    std::lock_guard<std::mutex> lock(mutex_);
    uint32_t current = 0;
    {
        Stmt q(db_, "SELECT COALESCE(MAX(epoch), 0) FROM channel_key_epochs WHERE channel_id=?;");
        if (!q.s) return false;
        q.bind_text(1, channel_id);
        if (q.step() == SQLITE_ROW) current = static_cast<uint32_t>(q.col_int64(0));
    }
    if (epoch != current + 1) return false;
    Stmt ins(db_,
        "INSERT INTO channel_key_epochs(channel_id, epoch, created_by_uid, created_by, created_at) "
        "VALUES(?1, ?2, ?3, COALESCE((SELECT username FROM members WHERE uid = ?3), ''), ?4);");
    if (!ins.s) return false;
    ins.bind_text(1, channel_id);
    ins.bind_int64(2, epoch);
    ins.bind_int64(3, created_by_uid);
    ins.bind_int64(4, now);
    return ins.step() == SQLITE_DONE;
}

int CommunityDb::channel_key_insert_blobs(const std::string& channel_id, uint32_t epoch,
                                          int64_t sender_uid, const std::string& sender,
                                          const std::vector<KeyBlobIn>& blobs,
                                          int64_t now) {
    std::lock_guard<std::mutex> lock(mutex_);
    int landed = 0;
    for (const auto& b : blobs) {
        Stmt q(db_,
            "INSERT OR IGNORE INTO channel_key_blobs"
            "(channel_id, epoch, recipient_uid, recipient, sender_uid, sender, blob, created_at) "
            "VALUES(?, ?, ?, ?, ?, ?, ?, ?);");
        if (!q.s) return landed;
        q.bind_text(1, channel_id);
        q.bind_int64(2, epoch);
        q.bind_int64(3, b.recipient_uid);
        q.bind_text(4, b.recipient);
        q.bind_int64(5, sender_uid);
        q.bind_text(6, sender);
        q.bind_blob_or_null(7, b.blob);
        q.bind_int64(8, now);
        if (q.step() == SQLITE_DONE && sqlite3_changes(db_) > 0) ++landed;
    }
    return landed;
}

std::vector<DbChannelKeyBlob> CommunityDb::channel_key_blobs_for(const std::string& channel_id,
                                                                 int64_t recipient_uid) const {
    std::lock_guard<std::mutex> lock(mutex_);
    std::vector<DbChannelKeyBlob> out;
    Stmt q(db_,
        "SELECT epoch, sender_uid, sender, recipient, blob FROM channel_key_blobs "
        "WHERE channel_id=? AND recipient_uid=? ORDER BY epoch;");
    if (!q.s) return out;
    q.bind_text(1, channel_id);
    q.bind_int64(2, recipient_uid);
    while (q.step() == SQLITE_ROW) {
        DbChannelKeyBlob b;
        b.epoch = static_cast<uint32_t>(q.col_int64(0));
        b.sender_uid = q.col_int64(1);
        b.sender = q.col_text(2);
        b.recipient_uid = recipient_uid;
        b.recipient = q.col_text(3);
        b.blob = q.col_blob(4);
        out.push_back(std::move(b));
    }
    return out;
}

std::vector<uint32_t> CommunityDb::channel_key_missing_epochs(const std::string& channel_id,
                                                              int64_t uid) const {
    std::lock_guard<std::mutex> lock(mutex_);
    std::vector<uint32_t> out;
    Stmt q(db_,
        "SELECT e.epoch FROM channel_key_epochs e "
        "WHERE e.channel_id=? AND NOT EXISTS ("
        "  SELECT 1 FROM channel_key_blobs b "
        "  WHERE b.channel_id=e.channel_id AND b.epoch=e.epoch AND b.recipient_uid=?) "
        "ORDER BY e.epoch;");
    if (!q.s) return out;
    q.bind_text(1, channel_id);
    q.bind_int64(2, uid);
    while (q.step() == SQLITE_ROW) out.push_back(static_cast<uint32_t>(q.col_int64(0)));
    return out;
}

std::vector<int64_t> CommunityDb::channel_key_holders(const std::string& channel_id, uint32_t epoch) const {
    std::lock_guard<std::mutex> lock(mutex_);
    std::vector<int64_t> out;
    Stmt q(db_,
        "SELECT recipient_uid FROM channel_key_blobs WHERE channel_id=? AND epoch=? ORDER BY recipient_uid;");
    if (!q.s) return out;
    q.bind_text(1, channel_id);
    q.bind_int64(2, epoch);
    while (q.step() == SQLITE_ROW) out.push_back(q.col_int64(0));
    return out;
}

void CommunityDb::channel_key_delete_recipient(int64_t recipient_uid) {
    std::lock_guard<std::mutex> lock(mutex_);
    Stmt q(db_, "DELETE FROM channel_key_blobs WHERE recipient_uid=?;");
    if (!q.s) return;
    q.bind_int64(1, recipient_uid);
    q.step();
}

std::vector<std::string> CommunityDb::channel_key_channels_with_epochs() const {
    std::lock_guard<std::mutex> lock(mutex_);
    std::vector<std::string> out;
    Stmt q(db_, "SELECT DISTINCT channel_id FROM channel_key_epochs;");
    if (!q.s) return out;
    while (q.step() == SQLITE_ROW) out.push_back(q.col_text(0));
    return out;
}

CommunityDb::DeleteMessageResult CommunityDb::delete_message(
    const std::string& channel_id, int64_t message_id) {
    std::lock_guard<std::mutex> lock(mutex_);
    DeleteMessageResult result;

    if (!exec_sql(db_, "BEGIN IMMEDIATE;")) return result;

    auto rollback = [&]() {
        exec_sql(db_, "ROLLBACK;");
        result.unlink_paths.clear();
        result.ok = false;
    };

    // Collect storage_paths of bound attachments before deletion.
    // Only rows with a non-empty storage_path get an unlink — 'uploading'
    // rows with placeholder paths and tombstoned rows where storage_path
    // is empty are skipped (they have no on-disk blob to remove).
    {
        Stmt q(db_,
            "SELECT COALESCE(storage_path, '') FROM attachments "
            "WHERE message_id=?;");
        if (!q.s) { rollback(); return result; }
        q.bind_int64(1, message_id);
        while (q.step() == SQLITE_ROW) {
            std::string p = q.col_text(0);
            if (!p.empty()) result.unlink_paths.push_back(std::move(p));
        }
    }

    // Delete attachment rows.
    {
        Stmt q(db_, "DELETE FROM attachments WHERE message_id=?;");
        if (!q.s) { rollback(); return result; }
        q.bind_int64(1, message_id);
        if (q.step() != SQLITE_DONE) { rollback(); return result; }
    }

    // Delete the message row.
    bool deleted = false;
    {
        Stmt q(db_, "DELETE FROM messages WHERE id=? AND channel_id=?;");
        if (!q.s) { rollback(); return result; }
        q.bind_int64(1, message_id);
        q.bind_text(2, channel_id);
        deleted = (q.step() == SQLITE_DONE) && sqlite3_changes(db_) == 1;
    }

    if (deleted) {
        if (!exec_sql(db_, "COMMIT;")) { rollback(); return result; }
        result.ok = true;
    } else {
        rollback();
    }
    return result;
}

bool CommunityDb::can_delete_others(int64_t uid) const {
    std::lock_guard<std::mutex> lock(mutex_);
    return (effective_permissions_unlocked_(uid) & perms::kManageMessages) != 0;
}

CommunityDb::PurgedMessages CommunityDb::delete_messages_by_sender_since(
    int64_t sender_uid, int64_t since_ts) {
    PurgedMessages out;
    if (sender_uid == 0) return out;
    std::lock_guard<std::mutex> lock(mutex_);
    // The account's rows by uid: a rename inside the window can't shield them.
    const std::string who = "sender_uid=?";
    auto bind_who = [&](Stmt& q) {
        q.bind_int64(1, sender_uid);
        q.bind_int64(2, since_ts);
    };
    {
        Stmt q(db_, ("SELECT id, channel_id FROM messages WHERE " + who + " AND timestamp>=?;").c_str());
        if (!q.s) return out;
        bind_who(q);
        while (q.step() == SQLITE_ROW) {
            out.messages.emplace_back(q.col_text(1), q.col_int64(0));
        }
    }
    if (out.messages.empty()) return out;
    {
        Stmt q(db_, ("SELECT storage_path FROM attachments "
                     "WHERE storage_path IS NOT NULL AND storage_path != '' AND message_id IN "
                     "  (SELECT id FROM messages WHERE " + who + " AND timestamp>=?);").c_str());
        if (q.s) {
            bind_who(q);
            while (q.step() == SQLITE_ROW) out.unlink_paths.push_back(q.col_text(0));
        }
    }
    if (!exec_sql(db_, "BEGIN IMMEDIATE;")) return {};   // DB1
    {
        Stmt q(db_, ("DELETE FROM attachments WHERE message_id IN "
                     "  (SELECT id FROM messages WHERE " + who + " AND timestamp>=?);").c_str());
        if (q.s) { bind_who(q); q.step(); }
    }
    {
        Stmt q(db_, ("DELETE FROM messages WHERE " + who + " AND timestamp>=?;").c_str());
        if (q.s) { bind_who(q); q.step(); }
    }
    exec_sql(db_, "COMMIT;");
    return out;
}

StorageUsage CommunityDb::storage_usage(int max_channels, int max_largest) const {
    StorageUsage out;
    std::lock_guard<std::mutex> lock(mutex_);
    {
        Stmt q(db_,
            "SELECT COALESCE(SUM(size_bytes),0), COALESCE(SUM(thumbnail_size_bytes),0), COUNT(*) "
            "FROM attachments WHERE purged_at=0;");
        if (q.s && q.step() == SQLITE_ROW) {
            out.attachments_bytes = q.col_int64(0);
            out.thumbnails_bytes  = q.col_int64(1);
            out.attachment_count  = q.col_int64(2);
        }
    }
    {
        Stmt q(db_,
            "SELECT kind, COALESCE(SUM(size_bytes),0), COUNT(*) "
            "FROM attachments WHERE purged_at=0 GROUP BY kind;");
        if (q.s) {
            while (q.step() == SQLITE_ROW) {
                out.by_kind.push_back({ q.col_int(0), q.col_int64(1), q.col_int64(2) });
            }
        }
    }
    if (max_channels > 0) {
        Stmt q(db_,
            "SELECT channel_id, COALESCE(SUM(size_bytes),0) AS b, COUNT(*) "
            "FROM attachments WHERE purged_at=0 GROUP BY channel_id "
            "ORDER BY b DESC LIMIT ?;");
        if (q.s) {
            q.bind_int(1, max_channels);
            while (q.step() == SQLITE_ROW) {
                StorageUsage::Channel c;
                c.channel_id = q.col_text(0);
                c.bytes = q.col_int64(1);
                c.count = q.col_int64(2);
                out.by_channel.push_back(std::move(c));
            }
        }
    }
    if (max_largest > 0) {
        Stmt q(db_,
            "SELECT id, filename, size_bytes, channel_id, kind "
            "FROM attachments WHERE purged_at=0 ORDER BY size_bytes DESC LIMIT ?;");
        if (q.s) {
            q.bind_int(1, max_largest);
            while (q.step() == SQLITE_ROW) {
                StorageUsage::Largest l;
                l.id = q.col_int64(0);
                l.filename = q.col_text(1);
                l.size_bytes = q.col_int64(2);
                l.channel_id = q.col_text(3);
                l.kind = q.col_int(4);
                out.largest.push_back(std::move(l));
            }
        }
    }
    return out;
}

int64_t CommunityDb::min_free_bytes() const {
    std::lock_guard<std::mutex> lock(mutex_);
    const std::string v = get_meta_("min_free_bytes");
    if (v.empty()) return kDefaultMinFreeBytes;
    try { return std::max<int64_t>(0, std::stoll(v)); }
    catch (...) { return kDefaultMinFreeBytes; }
}

int64_t CommunityDb::set_min_free_bytes(int64_t bytes) {
    if (bytes < 0) bytes = 0;
    std::lock_guard<std::mutex> lock(mutex_);
    set_meta_("min_free_bytes", std::to_string(bytes));
    return bytes;
}

void CommunityDb::add_audit(int64_t actor_uid, const std::string& actor, const std::string& action,
                            int64_t target_uid, const std::string& target,
                            const std::string& channel_id, const std::string& details) {
    std::lock_guard<std::mutex> lock(mutex_);
    Stmt q(db_,
        "INSERT INTO audit_log(ts, actor_uid, actor, action, target_uid, target, channel_id, details) "
        "VALUES(?, ?, ?, ?, ?, ?, ?, ?);");
    if (!q.s) return;
    q.bind_int64(1, now_seconds());
    q.bind_int64(2, actor_uid);
    q.bind_text(3, actor);
    q.bind_text(4, action);
    q.bind_int64(5, target_uid);
    q.bind_text(6, target);
    q.bind_text(7, channel_id);
    q.bind_text(8, clamp_utf8(details, 512));
    q.step();
}

std::vector<DbAuditEntry> CommunityDb::list_audit(int64_t before_id, int32_t limit,
                                                  bool* has_more,
                                                  const std::string& query,
                                                  const std::vector<std::string>& actions) const {
    if (has_more) *has_more = false;
    if (limit <= 0) limit = 50;
    if (limit > 100) limit = 100;
    // LIKE pattern with the user's text taken literally.
    std::string like;
    if (!query.empty()) {
        like.reserve(query.size() + 2);
        like.push_back('%');
        for (char c : clamp_utf8(query, 128)) {
            if (c == '%' || c == '_' || c == '\\') like.push_back('\\');
            like.push_back(c);
        }
        like.push_back('%');
    }
    // ",kick,ban," — matched with instr so the statement text never
    // varies (statements are cached by SQL). Verbs never contain commas.
    std::string verbs;
    for (size_t i = 0; i < actions.size() && i < 32; ++i) {
        if (actions[i].empty() || actions[i].find(',') != std::string::npos) continue;
        verbs += "," + actions[i];
    }
    if (!verbs.empty()) verbs += ",";
    std::lock_guard<std::mutex> lock(mutex_);
    std::vector<DbAuditEntry> out;
    // The names stored with an entry are the ones at the time; the actor
    // and a member target also match by their current name and nickname.
    Stmt q(db_,
        "SELECT id, ts, actor_uid, actor, action, target_uid, target, channel_id, details FROM audit_log "
        "WHERE (?1 <= 0 OR id < ?1) "
        "AND (?2 = '' OR instr(?2, ',' || action || ',') > 0) "
        "AND (?3 = '' "
        "  OR actor LIKE ?3 ESCAPE '\\' OR target LIKE ?3 ESCAPE '\\' "
        "  OR details LIKE ?3 ESCAPE '\\' OR replace(action, '_', ' ') LIKE ?3 ESCAPE '\\' "
        "  OR (channel_id <> '' AND channel_id IN (SELECT id FROM channels WHERE name LIKE ?3 ESCAPE '\\')) "
        "  OR (actor_uid <> 0 AND actor_uid IN (SELECT uid FROM members "
        "      WHERE nickname LIKE ?3 ESCAPE '\\' OR username LIKE ?3 ESCAPE '\\')) "
        "  OR (target_uid <> 0 AND target_uid IN (SELECT uid FROM members "
        "      WHERE nickname LIKE ?3 ESCAPE '\\' OR username LIKE ?3 ESCAPE '\\')) "
        "  OR target IN (SELECT 'role:' || id FROM roles WHERE name LIKE ?3 ESCAPE '\\')) "
        "ORDER BY id DESC LIMIT ?4;");
    if (!q.s) return out;
    q.bind_int64(1, before_id);
    q.bind_text(2, verbs);
    q.bind_text(3, like);
    q.bind_int(4, limit + 1);
    while (q.step() == SQLITE_ROW) {
        DbAuditEntry e;
        e.id = q.col_int64(0);
        e.timestamp = q.col_int64(1);
        e.actor_uid = q.col_int64(2);
        e.actor = q.col_text(3);
        e.action = q.col_text(4);
        e.target_uid = q.col_int64(5);
        e.target = q.col_text(6);
        e.channel_id = q.col_text(7);
        e.details = q.col_text(8);
        out.push_back(std::move(e));
    }
    if (static_cast<int32_t>(out.size()) > limit) {
        out.pop_back();
        if (has_more) *has_more = true;
    }
    return out;
}

void CommunityDb::prune_audit(int64_t cutoff_ts) {
    std::lock_guard<std::mutex> lock(mutex_);
    Stmt q(db_, "DELETE FROM audit_log WHERE ts < ?;");
    if (!q.s) return;
    q.bind_int64(1, cutoff_ts);
    q.step();
}

CommunityDb::PrunedTextResult CommunityDb::prune_text_messages(
    const std::string& channel_id, int64_t cutoff_ts) {
    PrunedTextResult out;
    std::lock_guard<std::mutex> lock(mutex_);

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

    // Collect any still-present attachment blobs so the caller can unlink
    // them from disk after the DB rows are gone.
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

    // Single transaction: delete attachment rows tied to the doomed messages,
    // then the messages themselves. We do this manually now that the FK +
    // CASCADE between attachments and messages has been dropped (FK was
    // incompatible with the message_id=0 sentinel for pending uploads).
    if (!exec_sql(db_, "BEGIN IMMEDIATE;")) return {};   // DB1
    {
        Stmt q(db_,
            "DELETE FROM attachments "
            "WHERE message_id IN (SELECT id FROM messages "
            "                     WHERE channel_id=? AND timestamp<?);");
        if (q.s) {
            q.bind_text(1, channel_id);
            q.bind_int64(2, cutoff_ts);
            q.step();
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
    exec_sql(db_, "COMMIT;");

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

    // Soft-delete: storage_path→NULL, purged_at→now, using the SAME
    // predicate as the SELECT above (nothing can change in between — one
    // connection, mutex held). This used to be `WHERE id IN (?,?,...)`
    // with one placeholder per row: the first sweep over an old channel
    // with thousands of images exceeded SQLITE_MAX_VARIABLE_NUMBER, the
    // prepare failed silently, and the caller still unlinked the blobs
    // and broadcast tombstones while the rows kept purged_at=0 — so the
    // next sweep found them again, forever.
    const int64_t now = now_seconds();
    Stmt q(db_,
        "UPDATE attachments SET storage_path=NULL, purged_at=? "
        "WHERE id IN (SELECT a.id FROM attachments a "
        "             JOIN messages m ON m.id = a.message_id "
        "             WHERE m.channel_id=? AND a.kind=? AND a.created_at<? "
        "               AND a.purged_at=0);");
    bool ok = false;
    if (q.s) {
        q.bind_int64(1, now);
        q.bind_text(2, channel_id);
        q.bind_int(3, kind);
        q.bind_int64(4, cutoff_ts);
        ok = q.step() == SQLITE_DONE;
        if (ok && sqlite3_changes(db_) != static_cast<int>(out.size())) {
            std::cerr << "[DB] prune_attachments: tombstoned "
                      << sqlite3_changes(db_) << " rows, expected "
                      << out.size() << "\n";
        }
    }
    if (!ok) {
        std::cerr << "[DB] prune_attachments: tombstone UPDATE failed for #"
                  << channel_id << " kind " << kind << ": "
                  << sqlite3_errmsg(db_) << "\n";
        out.clear();   // don't unlink / broadcast what we didn't commit
        return out;
    }
    for (auto& p : out) p.purged_at = now;
    return out;
}

} // namespace chatproj
