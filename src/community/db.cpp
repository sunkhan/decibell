#include "db.hpp"

#include <sqlite3.h>

#include <algorithm>
#include <chrono>
#include <cstdint>
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

    init_schema_();
    seed_if_empty_(owner_username, server_name, server_description);
    // Versioned, one-shot: a no-op once the DB has been stamped with the
    // current seed version (see ensure_default_channels_). It used to run
    // INSERT OR IGNORE on every boot, which resurrected any default
    // channel an operator had deleted via CHANNEL_DELETE_REQ.
    ensure_default_channels_();
    // Existing DBs may pre-date the text-above-voice ordering invariant.
    normalize_channel_order_();
    owner_cache_ = get_meta_("owner");
    return true;
}

namespace { bool column_exists(sqlite3* db, const char* table, const char* column); }

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

    // --- v5: roles + permissions ---
    migrate_to_v5_roles_();

    // --- v6: per-channel overwrites + v2 permission bits ---
    migrate_to_v6_overwrites_();

    // --- v7: timeouts, server voice flags, ban expiry, slowmode, audit log ---
    migrate_to_v7_moderation_();
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
    // Freshness is judged by the absence of the `owner` meta key — NOT by
    // COUNT(*) on server_meta. init_schema_ runs before seeding and its
    // migrations already stamp schema_version into server_meta, so a
    // count-based check sees a "non-empty" table on a brand-new file and
    // skips seeding entirely: the DB comes up ownerless, every owner-gated
    // feature is dead, and the owner can't even join (membership requires
    // an invite only the owner could create). This also repairs DBs
    // created while that bug was live, the first time they boot with
    // DECIBELL_OWNER_USERNAME set.
    if (!get_meta_("owner").empty()) {
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

    // schema_version is stamped by the migrations in init_schema_ —
    // don't overwrite it here.
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
    if (key == "owner") {
        owner_cache_ = value;
        invalidate_perm_cache_();
    }
}

std::string CommunityDb::owner() const {
    std::lock_guard<std::mutex> lock(mutex_);
    return owner_cache_;
}

void CommunityDb::invalidate_perm_cache_() {
    perm_cache_.clear();
    channel_perm_cache_.clear();
}

void CommunityDb::invalidate_user_perms_(const std::string& username) {
    perm_cache_.erase(username);
    channel_perm_cache_.erase(username);
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

bool CommunityDb::transfer_ownership(const std::string& new_owner) {
    std::lock_guard<std::mutex> lock(mutex_);
    {
        Stmt q(db_, "SELECT 1 FROM members WHERE username=?;");
        if (!q.s) return false;
        q.bind_text(1, new_owner);
        if (q.step() != SQLITE_ROW) return false;
    }
    set_meta_("owner", new_owner);   // refreshes owner_cache_ + invalidates perms
    return true;
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
    invalidate_user_perms_(username);
    overwrites_cache_.clear();
    exec_sql(db_, "BEGIN IMMEDIATE;");
    {
        Stmt q(db_, "DELETE FROM channel_overwrites WHERE target_type=1 AND target_id=?;");
        if (q.s) { q.bind_text(1, username); q.step(); }
    }
    // Role assignments don't survive leaving/kick — a rejoining member
    // starts clean, matching Discord semantics.
    {
        Stmt q(db_, "DELETE FROM member_roles WHERE username=?;");
        if (q.s) { q.bind_text(1, username); q.step(); }
    }
    bool was_member = false;
    {
        Stmt q(db_, "DELETE FROM members WHERE username=?;");
        if (!q.s) { exec_sql(db_, "ROLLBACK;"); return false; }
        q.bind_text(1, username);
        if (q.step() != SQLITE_DONE) { exec_sql(db_, "ROLLBACK;"); return false; }
        was_member = sqlite3_changes(db_) > 0;
    }
    exec_sql(db_, "COMMIT;");
    return was_member;
}

std::vector<DbMember> CommunityDb::list_members() const {
    std::lock_guard<std::mutex> lock(mutex_);
    std::vector<DbMember> out;
    Stmt q(db_,
        "SELECT username, joined_at, nickname, timed_out_until, server_muted, server_deafened "
        "FROM members ORDER BY joined_at ASC;");
    if (!q.s) return out;
    while (q.step() == SQLITE_ROW) {
        DbMember m;
        m.username = q.col_text(0);
        m.joined_at = q.col_int64(1);
        m.nickname = q.col_text(2);
        m.timed_out_until = q.col_int64(3);
        m.server_muted = q.col_int(4) != 0;
        m.server_deafened = q.col_int(5) != 0;
        out.push_back(std::move(m));
    }
    return out;
}

std::optional<DbMember> CommunityDb::get_member(const std::string& username) const {
    std::lock_guard<std::mutex> lock(mutex_);
    Stmt q(db_,
        "SELECT username, joined_at, nickname, timed_out_until, server_muted, server_deafened "
        "FROM members WHERE username=?;");
    if (!q.s) return std::nullopt;
    q.bind_text(1, username);
    if (q.step() != SQLITE_ROW) return std::nullopt;
    DbMember m;
    m.username = q.col_text(0);
    m.joined_at = q.col_int64(1);
    m.nickname = q.col_text(2);
    m.timed_out_until = q.col_int64(3);
    m.server_muted = q.col_int(4) != 0;
    m.server_deafened = q.col_int(5) != 0;
    return m;
}

bool CommunityDb::set_timeout(const std::string& username, int64_t until) {
    std::lock_guard<std::mutex> lock(mutex_);
    Stmt q(db_, "UPDATE members SET timed_out_until=? WHERE username=?;");
    if (!q.s) return false;
    q.bind_int64(1, until < 0 ? 0 : until);
    q.bind_text(2, username);
    if (q.step() != SQLITE_DONE) return false;
    return sqlite3_changes(db_) > 0;
}

bool CommunityDb::set_server_voice_flags(const std::string& username, bool muted, bool deafened) {
    std::lock_guard<std::mutex> lock(mutex_);
    Stmt q(db_, "UPDATE members SET server_muted=?, server_deafened=? WHERE username=?;");
    if (!q.s) return false;
    q.bind_int(1, muted ? 1 : 0);
    q.bind_int(2, deafened ? 1 : 0);
    q.bind_text(3, username);
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
// Shared by is_banned / get_ban / redeem_invite (mutex held by caller):
// an expired ban is deleted on sight and reported as "not banned".
bool ban_active_unlocked(sqlite3* db, const std::string& username, DbBan* out) {
    Stmt q(db, "SELECT username, banned_at, banned_by, reason, expires_at FROM bans WHERE username=?;");
    if (!q.s) return false;
    q.bind_text(1, username);
    if (q.step() != SQLITE_ROW) return false;
    DbBan b;
    b.username = q.col_text(0);
    b.banned_at = q.col_int64(1);
    b.banned_by = q.col_text(2);
    b.reason = q.col_text(3);
    b.expires_at = q.col_int64(4);
    if (b.expires_at != 0 && b.expires_at <= now_seconds()) {
        Stmt del(db, "DELETE FROM bans WHERE username=?;");
        if (del.s) { del.bind_text(1, username); del.step(); }
        return false;
    }
    if (out) *out = b;
    return true;
}
} // namespace

bool CommunityDb::is_banned(const std::string& username) const {
    std::lock_guard<std::mutex> lock(mutex_);
    return ban_active_unlocked(db_, username, nullptr);
}

std::optional<DbBan> CommunityDb::get_ban(const std::string& username) const {
    std::lock_guard<std::mutex> lock(mutex_);
    DbBan b;
    if (!ban_active_unlocked(db_, username, &b)) return std::nullopt;
    return b;
}

bool CommunityDb::add_ban(const std::string& username,
                          const std::string& banned_by,
                          const std::string& reason,
                          int64_t expires_at) {
    std::lock_guard<std::mutex> lock(mutex_);
    invalidate_user_perms_(username);
    overwrites_cache_.clear();
    // Remove membership (and role assignments) and insert ban atomically.
    exec_sql(db_, "BEGIN IMMEDIATE;");
    {
        Stmt del(db_, "DELETE FROM channel_overwrites WHERE target_type=1 AND target_id=?;");
        if (del.s) { del.bind_text(1, username); del.step(); }
    }
    {
        Stmt del(db_, "DELETE FROM member_roles WHERE username=?;");
        if (del.s) {
            del.bind_text(1, username);
            del.step();
        }
    }
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
            "INSERT INTO bans(username, banned_at, banned_by, reason, expires_at) "
            "VALUES(?, ?, ?, ?, ?) "
            "ON CONFLICT(username) DO UPDATE SET "
            "  banned_at=excluded.banned_at, "
            "  banned_by=excluded.banned_by, "
            "  reason=excluded.reason, "
            "  expires_at=excluded.expires_at;");
        if (ins.s) {
            ins.bind_text(1, username);
            ins.bind_int64(2, now_seconds());
            ins.bind_text(3, banned_by);
            ins.bind_text(4, reason);
            ins.bind_int64(5, expires_at < 0 ? 0 : expires_at);
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

std::vector<DbBan> CommunityDb::list_bans() const {
    std::lock_guard<std::mutex> lock(mutex_);
    std::vector<DbBan> out;
    const int64_t now = now_seconds();
    // Sweep expired rows first so the list (and its count) is honest.
    {
        Stmt del(db_, "DELETE FROM bans WHERE expires_at != 0 AND expires_at <= ?;");
        if (del.s) { del.bind_int64(1, now); del.step(); }
    }
    Stmt q(db_, "SELECT username, banned_at, banned_by, reason, expires_at FROM bans ORDER BY banned_at DESC;");
    if (!q.s) return out;
    while (q.step() == SQLITE_ROW) {
        DbBan b;
        b.username = q.col_text(0);
        b.banned_at = q.col_int64(1);
        b.banned_by = q.col_text(2);
        b.reason = q.col_text(3);
        b.expires_at = q.col_int64(4);
        out.push_back(std::move(b));
    }
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
    return r;
}
constexpr const char* kRoleCols =
    "id, name, color, position, permissions, is_default";
} // namespace

std::vector<DbRole> CommunityDb::list_roles() const {
    std::lock_guard<std::mutex> lock(mutex_);
    std::vector<DbRole> out;
    Stmt q(db_,
        "SELECT id, name, color, position, permissions, is_default "
        "FROM roles ORDER BY position DESC, id ASC;");
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
                                               uint64_t permissions) {
    // Clamp here too so no future caller can store a mid-codepoint cut.
    const std::string name = clamp_utf8(raw_name, kMaxRoleNameBytes);
    if (name.empty()) return std::nullopt;
    std::lock_guard<std::mutex> lock(mutex_);
    invalidate_perm_cache_();   // positions shift for every role
    exec_sql(db_, "BEGIN IMMEDIATE;");
    // New roles land at position 1, directly above `everyone`; existing
    // roles shift up one to stay dense.
    exec_sql(db_, "UPDATE roles SET position = position + 1 WHERE is_default = 0;");
    DbRole r;
    r.name = name;
    r.color = color;
    r.position = 1;
    r.permissions = permissions;
    {
        Stmt ins(db_,
            "INSERT INTO roles(name, color, position, permissions, is_default) "
            "VALUES(?, ?, 1, ?, 0);");
        if (!ins.s) { exec_sql(db_, "ROLLBACK;"); return std::nullopt; }
        ins.bind_text(1, name);
        ins.bind_int64(2, static_cast<int64_t>(color));
        ins.bind_int64(3, static_cast<int64_t>(permissions));
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
                              int32_t position) {
    const std::string name = clamp_utf8(raw_name, kMaxRoleNameBytes);
    std::lock_guard<std::mutex> lock(mutex_);
    auto cur = get_role_unlocked_(role_id);
    if (!cur) return false;
    invalidate_perm_cache_();

    if (cur->is_default) {
        // Only the permission bits of `everyone` are editable.
        Stmt q(db_, "UPDATE roles SET permissions=? WHERE id=?;");
        if (!q.s) return false;
        q.bind_int64(1, static_cast<int64_t>(permissions));
        q.bind_int64(2, role_id);
        return q.step() == SQLITE_DONE;
    }
    if (name.empty()) return false;

    // Clamp the requested position into the valid dense range [1, N].
    const int32_t max_pos = max_role_position_unlocked_();
    int32_t new_pos = position;
    if (new_pos < 1) new_pos = 1;
    if (new_pos > max_pos) new_pos = max_pos;

    exec_sql(db_, "BEGIN IMMEDIATE;");
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
            "UPDATE roles SET name=?, color=?, permissions=?, position=? "
            "WHERE id=?;");
        ok = q.s != nullptr;
        if (ok) {
            q.bind_text(1, name);
            q.bind_int64(2, static_cast<int64_t>(color));
            q.bind_int64(3, static_cast<int64_t>(permissions));
            q.bind_int(4, new_pos);
            q.bind_int64(5, role_id);
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

    exec_sql(db_, "BEGIN IMMEDIATE;");
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

std::vector<int64_t> CommunityDb::get_member_role_ids(const std::string& username) const {
    std::lock_guard<std::mutex> lock(mutex_);
    std::vector<int64_t> out;
    Stmt q(db_,
        "SELECT mr.role_id FROM member_roles mr "
        "JOIN roles r ON r.id = mr.role_id "
        "WHERE mr.username=? ORDER BY r.position DESC;");
    if (!q.s) return out;
    q.bind_text(1, username);
    while (q.step() == SQLITE_ROW) out.push_back(q.col_int64(0));
    return out;
}

std::vector<std::pair<std::string, int64_t>> CommunityDb::list_all_member_roles() const {
    std::lock_guard<std::mutex> lock(mutex_);
    std::vector<std::pair<std::string, int64_t>> out;
    Stmt q(db_,
        "SELECT mr.username, mr.role_id FROM member_roles mr "
        "JOIN roles r ON r.id = mr.role_id "
        "ORDER BY mr.username ASC, r.position DESC;");
    if (!q.s) return out;
    while (q.step() == SQLITE_ROW) {
        out.emplace_back(q.col_text(0), q.col_int64(1));
    }
    return out;
}

bool CommunityDb::set_member_roles(const std::string& username,
                                   const std::vector<int64_t>& role_ids) {
    std::lock_guard<std::mutex> lock(mutex_);
    // Validate every id references an existing, non-default role before
    // touching anything.
    for (int64_t id : role_ids) {
        auto r = get_role_unlocked_(id);
        if (!r || r->is_default) return false;
    }
    invalidate_user_perms_(username);
    exec_sql(db_, "BEGIN IMMEDIATE;");
    bool ok = true;
    {
        Stmt q(db_, "DELETE FROM member_roles WHERE username=?;");
        ok = q.s != nullptr;
        if (ok) { q.bind_text(1, username); ok = q.step() == SQLITE_DONE; }
    }
    for (int64_t id : role_ids) {
        if (!ok) break;
        Stmt q(db_,
            "INSERT OR IGNORE INTO member_roles(username, role_id) VALUES(?, ?);");
        ok = q.s != nullptr;
        if (ok) {
            q.bind_text(1, username);
            q.bind_int64(2, id);
            ok = q.step() == SQLITE_DONE;
        }
    }
    exec_sql(db_, ok ? "COMMIT;" : "ROLLBACK;");
    return ok;
}

const CommunityDb::PermEntry& CommunityDb::perm_entry_unlocked_(const std::string& username) const {
    auto it = perm_cache_.find(username);
    if (it != perm_cache_.end()) return it->second;

    PermEntry e;
    if (!username.empty() && owner_cache_ == username) {
        e.permissions = perms::kAll;
        e.level = INT32_MAX;
    } else {
        // Base bits from `everyone` + OR of the member's assigned roles,
        // and the highest assigned position, in one pass.
        Stmt q(db_,
            "SELECT permissions, 0 FROM roles WHERE is_default=1 "
            "UNION ALL "
            "SELECT r.permissions, r.position FROM roles r "
            "JOIN member_roles mr ON mr.role_id = r.id "
            "WHERE mr.username=?1;");
        uint64_t p = 0;
        int32_t level = 0;
        if (q.s) {
            q.bind_text(1, username);
            while (q.step() == SQLITE_ROW) {
                p |= static_cast<uint64_t>(q.col_int64(0));
                level = std::max(level, q.col_int(1));
            }
        }
        e.permissions = (p & perms::kAdministrator) ? perms::kAll : p;
        e.level = level;
    }
    // Bounded: entries are per username and cleared on any role change;
    // a pathological churn of usernames still can't grow it past this.
    if (perm_cache_.size() >= 4096) perm_cache_.clear();
    return perm_cache_.emplace(username, e).first->second;
}

uint64_t CommunityDb::effective_permissions_unlocked_(const std::string& username) const {
    return perm_entry_unlocked_(username).permissions;
}

uint64_t CommunityDb::effective_permissions(const std::string& username) const {
    std::lock_guard<std::mutex> lock(mutex_);
    return effective_permissions_unlocked_(username);
}

bool CommunityDb::has_permission(const std::string& username, uint64_t perm) const {
    std::lock_guard<std::mutex> lock(mutex_);
    return (effective_permissions_unlocked_(username) & perm) == perm;
}

int32_t CommunityDb::member_level(const std::string& username) const {
    std::lock_guard<std::mutex> lock(mutex_);
    return perm_entry_unlocked_(username).level;
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
    return overwrites_unlocked_(channel_id);
}

bool CommunityDb::set_overwrite(const DbOverwrite& in) {
    std::lock_guard<std::mutex> lock(mutex_);
    {
        Stmt q(db_, "SELECT 1 FROM channels WHERE id=?;");
        if (!q.s) return false;
        q.bind_text(1, in.channel_id);
        if (q.step() != SQLITE_ROW) return false;
    }
    if (in.target_type == 0) {
        int64_t rid = 0;
        try { rid = std::stoll(in.target_id); } catch (...) { return false; }
        if (!get_role_unlocked_(rid)) return false;
    } else if (in.target_type == 1) {
        Stmt q(db_, "SELECT 1 FROM members WHERE username=?;");
        if (!q.s) return false;
        q.bind_text(1, in.target_id);
        if (q.step() != SQLITE_ROW) return false;
    } else {
        return false;
    }
    const uint64_t deny = in.deny & perms::kKnownMask;
    const uint64_t allow = (in.allow & perms::kKnownMask) & ~deny;

    overwrites_cache_.erase(in.channel_id);
    channel_perm_cache_.clear();

    if (allow == 0 && deny == 0) {
        Stmt q(db_,
            "DELETE FROM channel_overwrites "
            "WHERE channel_id=? AND target_type=? AND target_id=?;");
        if (!q.s) return false;
        q.bind_text(1, in.channel_id);
        q.bind_int(2, in.target_type);
        q.bind_text(3, in.target_id);
        return q.step() == SQLITE_DONE;
    }
    Stmt q(db_,
        "INSERT INTO channel_overwrites(channel_id, target_type, target_id, allow, deny) "
        "VALUES(?, ?, ?, ?, ?) "
        "ON CONFLICT(channel_id, target_type, target_id) DO UPDATE SET "
        "  allow=excluded.allow, deny=excluded.deny;");
    if (!q.s) return false;
    q.bind_text(1, in.channel_id);
    q.bind_int(2, in.target_type);
    q.bind_text(3, in.target_id);
    q.bind_int64(4, static_cast<int64_t>(allow));
    q.bind_int64(5, static_cast<int64_t>(deny));
    return q.step() == SQLITE_DONE;
}

uint64_t CommunityDb::channel_permissions_unlocked_(const std::string& username,
                                                    const std::string& channel_id) const {
    auto& per_user = channel_perm_cache_[username];
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
    const PermEntry& base = perm_entry_unlocked_(username);
    if (base.permissions == perms::kAll) {
        result = perms::kAll;   // owner / ADMINISTRATOR bypass overwrites
    } else {
        std::set<std::string> my_roles;
        {
            Stmt q(db_, "SELECT role_id FROM member_roles WHERE username=?;");
            if (q.s) {
                q.bind_text(1, username);
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
        for (const auto& ow : overwrites_unlocked_(channel_id)) {
            if (ow.target_type == 0) {
                if (ow.target_id == everyone_id) { ev_allow |= ow.allow; ev_deny |= ow.deny; }
                else if (my_roles.count(ow.target_id)) { role_allow |= ow.allow; role_deny |= ow.deny; }
            } else if (ow.target_id == username) {
                me_allow |= ow.allow; me_deny |= ow.deny;
            }
        }
        p = (p & ~ev_deny) | ev_allow;
        p = (p & ~role_deny) | role_allow;
        p = (p & ~me_deny) | me_allow;
        result = (p & perms::kViewChannel) ? p : 0;
    }
    if (per_user.size() >= 1024) per_user.clear();
    per_user.emplace(channel_id, result);
    return result;
}

uint64_t CommunityDb::channel_permissions(const std::string& username,
                                          const std::string& channel_id) const {
    std::lock_guard<std::mutex> lock(mutex_);
    return channel_permissions_unlocked_(username, channel_id);
}

bool CommunityDb::has_channel_permission(const std::string& username,
                                         const std::string& channel_id,
                                         uint64_t perm) const {
    std::lock_guard<std::mutex> lock(mutex_);
    return (channel_permissions_unlocked_(username, channel_id) & perm) == perm;
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
    if (ban_active_unlocked(db_, redeeming_user, nullptr)) return InviteResult::Banned;

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
        "  retention_days_document, retention_days_audio, slowmode_seconds "
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
        out.push_back(std::move(c));
    }
    return out;
}

std::optional<DbChannel> CommunityDb::get_channel(const std::string& channel_id) const {
    std::lock_guard<std::mutex> lock(mutex_);
    Stmt q(db_,
        "SELECT id, name, type, position, voice_bitrate_kbps, "
        "  retention_days_text, retention_days_image, retention_days_video, "
        "  retention_days_document, retention_days_audio, slowmode_seconds "
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
                                                     const std::string& category_id) {
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

    exec_sql(db_, "BEGIN IMMEDIATE;");
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
    if (ok) {
        Stmt ins(db_,
            "INSERT INTO channels(id, name, type, position, voice_bitrate_kbps) "
            "VALUES(?, ?, ?, ?, ?);");
        ok = ins.s != nullptr;
        if (ok) {
            ins.bind_text(1, id);
            ins.bind_text(2, name);
            ins.bind_int(3, type);
            ins.bind_int(4, static_cast<int32_t>(idx));
            ins.bind_int(5, type == 1 ? voice_bitrate_kbps : 0);
            ok = ins.step() == SQLITE_DONE;
        }
    }
    exec_sql(db_, ok ? "COMMIT;" : "ROLLBACK;");
    if (!ok) return std::nullopt;
    normalize_channel_order_();

    DbChannel c;
    c.id = id;
    c.name = name;
    c.type = type;
    c.position = static_cast<int32_t>(idx);
    c.voice_bitrate_kbps = type == 1 ? voice_bitrate_kbps : 0;
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

    exec_sql(db_, "BEGIN IMMEDIATE;");
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

    exec_sql(db_, "BEGIN IMMEDIATE;");
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
    if (ok) normalize_channel_order_();
    return ok;
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
    if (!ok) return std::nullopt;
    overwrites_cache_.erase(channel_id);
    channel_perm_cache_.clear();
    normalize_channel_order_();
    return out;
}

bool CommunityDb::set_nickname(const std::string& username,
                               const std::string& raw_nickname) {
    const std::string nickname = clamp_utf8(raw_nickname, kMaxNicknameBytes);
    std::lock_guard<std::mutex> lock(mutex_);
    Stmt q(db_, "UPDATE members SET nickname=? WHERE username=?;");
    if (!q.s) return false;
    q.bind_text(1, nickname);
    q.bind_text(2, username);
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
        "  upload_status, expected_size, uploader, width, height, "
        "  thumbnail_size_bytes, thumbnail_sizes_mask, duration_ms, "
        "  COALESCE(placeholder, '') "
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
        a.width = q.col_int(13);
        a.height = q.col_int(14);
        a.thumbnail_size_bytes = q.col_int64(15);
        a.thumbnail_sizes_mask = q.col_int(16);
        a.duration_ms = q.col_int(17);
        a.placeholder = q.col_text(18);
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
                                               int32_t position,
                                               int32_t width,
                                               int32_t height,
                                               int32_t duration_ms,
                                               const std::string& placeholder) {
    std::lock_guard<std::mutex> lock(mutex_);
    Stmt q(db_,
        "INSERT INTO attachments("
        "  message_id, kind, filename, mime, size_bytes, storage_path, "
        "  position, created_at, purged_at, upload_status, expected_size, "
        "  uploader, channel_id, width, height, duration_ms, placeholder"
        ") VALUES(0, ?, ?, ?, 0, ?, ?, ?, 0, 'uploading', ?, ?, ?, ?, ?, ?, ?);");
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
    q.bind_text(8, uploader);
    q.bind_text(9, channel_id);
    q.bind_int(10, w);
    q.bind_int(11, h);
    // Clamp duration to non-negative; we don't cap an upper bound — even
    // a 24-hour audio attachment fits comfortably in int32 ms.
    q.bind_int(12, duration_ms < 0 ? 0 : duration_ms);
    // Opaque base64 blob. Length-capped so a malicious client can't use
    // it as unbounded storage — a real ThumbHash is ~34 base64 chars.
    q.bind_text(13, placeholder.size() > 128 ? std::string() : placeholder);
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
        "  upload_status, expected_size, uploader, channel_id, width, height, "
        "  thumbnail_size_bytes, thumbnail_sizes_mask, duration_ms, "
        "  COALESCE(placeholder, '') "
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
    a.channel_id = q.col_text(13);
    a.width = q.col_int(14);
    a.height = q.col_int(15);
    a.thumbnail_size_bytes = q.col_int64(16);
    a.thumbnail_sizes_mask = q.col_int(17);
    a.duration_ms = q.col_int(18);
    a.placeholder = q.col_text(19);
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

std::vector<DbAttachment> CommunityDb::list_stale_pending_attachments(
    int64_t uploading_cutoff_ts, int64_t ready_cutoff_ts) const {
    std::vector<DbAttachment> out;
    std::lock_guard<std::mutex> lock(mutex_);
    // Anything not 'uploading' gets the longer ready-cutoff — a finished
    // upload waiting in a compose box must not vanish after an hour.
    Stmt q(db_,
        "SELECT id, message_id, kind, filename, mime, size_bytes, "
        "  COALESCE(storage_path, ''), position, created_at, purged_at, "
        "  upload_status, expected_size, uploader, channel_id "
        "FROM attachments WHERE message_id=0 "
        "  AND ((upload_status='uploading' AND created_at<?) "
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

std::optional<std::string> CommunityDb::get_message_sender(
    const std::string& channel_id, int64_t message_id) const {
    std::lock_guard<std::mutex> lock(mutex_);
    Stmt q(db_, "SELECT sender FROM messages WHERE id=? AND channel_id=?;");
    if (!q.s) return std::nullopt;
    q.bind_int64(1, message_id);
    q.bind_text(2, channel_id);
    if (q.step() == SQLITE_ROW) {
        return q.col_text(0);
    }
    return std::nullopt;
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

bool CommunityDb::can_delete_others(const std::string& username) const {
    std::lock_guard<std::mutex> lock(mutex_);
    return (effective_permissions_unlocked_(username) & perms::kManageMessages) != 0;
}

CommunityDb::PurgedMessages CommunityDb::delete_messages_by_sender_since(
    const std::string& sender, int64_t since_ts) {
    PurgedMessages out;
    std::lock_guard<std::mutex> lock(mutex_);
    {
        Stmt q(db_, "SELECT id, channel_id FROM messages WHERE sender=? AND timestamp>=?;");
        if (!q.s) return out;
        q.bind_text(1, sender);
        q.bind_int64(2, since_ts);
        while (q.step() == SQLITE_ROW) {
            out.messages.emplace_back(q.col_text(1), q.col_int64(0));
        }
    }
    if (out.messages.empty()) return out;
    {
        Stmt q(db_,
            "SELECT storage_path FROM attachments "
            "WHERE storage_path IS NOT NULL AND storage_path != '' AND message_id IN "
            "  (SELECT id FROM messages WHERE sender=? AND timestamp>=?);");
        if (q.s) {
            q.bind_text(1, sender);
            q.bind_int64(2, since_ts);
            while (q.step() == SQLITE_ROW) out.unlink_paths.push_back(q.col_text(0));
        }
    }
    exec_sql(db_, "BEGIN IMMEDIATE;");
    {
        Stmt q(db_,
            "DELETE FROM attachments WHERE message_id IN "
            "  (SELECT id FROM messages WHERE sender=? AND timestamp>=?);");
        if (q.s) { q.bind_text(1, sender); q.bind_int64(2, since_ts); q.step(); }
    }
    {
        Stmt q(db_, "DELETE FROM messages WHERE sender=? AND timestamp>=?;");
        if (q.s) { q.bind_text(1, sender); q.bind_int64(2, since_ts); q.step(); }
    }
    exec_sql(db_, "COMMIT;");
    return out;
}

void CommunityDb::add_audit(const std::string& actor, const std::string& action,
                            const std::string& target, const std::string& channel_id,
                            const std::string& details) {
    std::lock_guard<std::mutex> lock(mutex_);
    Stmt q(db_,
        "INSERT INTO audit_log(ts, actor, action, target, channel_id, details) "
        "VALUES(?, ?, ?, ?, ?, ?);");
    if (!q.s) return;
    q.bind_int64(1, now_seconds());
    q.bind_text(2, actor);
    q.bind_text(3, action);
    q.bind_text(4, target);
    q.bind_text(5, channel_id);
    q.bind_text(6, clamp_utf8(details, 512));
    q.step();
}

std::vector<DbAuditEntry> CommunityDb::list_audit(int64_t before_id, int32_t limit,
                                                  bool* has_more) const {
    if (has_more) *has_more = false;
    if (limit <= 0) limit = 50;
    if (limit > 100) limit = 100;
    std::lock_guard<std::mutex> lock(mutex_);
    std::vector<DbAuditEntry> out;
    Stmt q(db_,
        before_id > 0
            ? "SELECT id, ts, actor, action, target, channel_id, details FROM audit_log "
              "WHERE id<? ORDER BY id DESC LIMIT ?;"
            : "SELECT id, ts, actor, action, target, channel_id, details FROM audit_log "
              "ORDER BY id DESC LIMIT ?;");
    if (!q.s) return out;
    int idx = 1;
    if (before_id > 0) q.bind_int64(idx++, before_id);
    q.bind_int(idx, limit + 1);
    while (q.step() == SQLITE_ROW) {
        DbAuditEntry e;
        e.id = q.col_int64(0);
        e.timestamp = q.col_int64(1);
        e.actor = q.col_text(2);
        e.action = q.col_text(3);
        e.target = q.col_text(4);
        e.channel_id = q.col_text(5);
        e.details = q.col_text(6);
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
    exec_sql(db_, "BEGIN IMMEDIATE;");
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
