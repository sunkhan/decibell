#include "auth_manager.hpp"
#include "auth_utils.hpp"
#include "bcrypt.h"
#include <chrono>
#include <ctime>
#include <iostream>
#include <algorithm>

void AuthManager::initializeDatabase() {
    try {
        auto lease = pool_.acquire();
        pqxx::connection& conn = *lease;
        pqxx::work txn(conn);
        // Schema version (2026-10-15). Everything below `version < 1` is the
        // schema as it stood before users were keyed by uid: it brings an
        // older database (or a fresh one) up to that point, then migration
        // 1 converts it, in this same transaction. Later migrations go after
        // it as `version < 2`, and so on.
        txn.exec(
            "CREATE TABLE IF NOT EXISTS central_schema ("
            "  id SMALLINT PRIMARY KEY CHECK (id = 1),"
            "  version INTEGER NOT NULL"
            ")"
        );
        txn.exec("INSERT INTO central_schema (id, version) VALUES (1, 0) ON CONFLICT (id) DO NOTHING");
        const int version =
            txn.exec("SELECT version FROM central_schema WHERE id = 1 FOR UPDATE")[0][0].as<int>();
        if (version < 1) {
            txn.exec(
                "CREATE TABLE IF NOT EXISTS users ("
                "  username VARCHAR(32) PRIMARY KEY,"
                "  email VARCHAR(128) UNIQUE NOT NULL,"
                "  password_hash VARCHAR(128) NOT NULL"
                ")"
            );
            // Stable user id (Theme A, 2026-08-22): carried as the `uid` JWT
            // claim so community servers can key bans / audit rows on
            // something a username change or reuse can't defeat. BIGSERIAL in
            // ADD COLUMN creates the sequence and back-fills existing rows.
            txn.exec("ALTER TABLE users ADD COLUMN IF NOT EXISTS uid BIGSERIAL");
            txn.exec("CREATE UNIQUE INDEX IF NOT EXISTS users_uid_idx ON users (uid)");
            // Avatar columns added 2026-05-12 (see docs/superpowers/specs/
            // 2026-05-12-custom-profile-pictures-design.md §5). ADD COLUMN
            // IF NOT EXISTS makes this idempotent on already-deployed servers.
            txn.exec(
                "ALTER TABLE users ADD COLUMN IF NOT EXISTS avatar BYTEA"
            );
            txn.exec(
                "ALTER TABLE users ADD COLUMN IF NOT EXISTS "
                "avatar_version VARCHAR(64) NOT NULL DEFAULT ''"
            );
            // User statuses (2026-10-11, docs/superpowers/specs/
            // 2026-10-11-user-status-design.md): the chosen status as the
            // PresenceStatus enum value (0 online, 1 away, 2 dnd, 3 invisible),
            // and the friends-only DM setting, which used to live only in the
            // session (reset on every reconnect, and its online/offline
            // difference could see through Invisible).
            txn.exec(
                "ALTER TABLE users ADD COLUMN IF NOT EXISTS "
                "presence_status SMALLINT NOT NULL DEFAULT 0"
            );
            txn.exec(
                "ALTER TABLE users ADD COLUMN IF NOT EXISTS "
                "dm_friends_only BOOLEAN NOT NULL DEFAULT FALSE"
            );
            // Custom status messages (2026-10-12, docs/superpowers/specs/
            // 2026-10-12-status-messages-design.md): text, emoji, and when
            // central clears them (unix seconds, 0 = never).
            txn.exec(
                "ALTER TABLE users ADD COLUMN IF NOT EXISTS "
                "status_text TEXT NOT NULL DEFAULT ''"
            );
            txn.exec(
                "ALTER TABLE users ADD COLUMN IF NOT EXISTS "
                "status_emoji TEXT NOT NULL DEFAULT ''"
            );
            txn.exec(
                "ALTER TABLE users ADD COLUMN IF NOT EXISTS "
                "status_expires_at BIGINT NOT NULL DEFAULT 0"
            );
            // The (user1, user2) pair is stored in *byte* order — C++
            // std::min/std::max in handleFriendAction/isBlocked. The CHECK
            // must use the same ordering: an unqualified `user1 < user2`
            // compares with the database collation (en_US.UTF-8 on the
            // deployed box), which disagrees with byte order for case-mixed
            // or punctuated names ("Zeki"/"adam", "a_b"/"aab") — every such
            // ADD died with a check violation, surfaced as "Database
            // error." COLLATE "C" pins the CHECK to byte order.
            txn.exec(
                "CREATE TABLE IF NOT EXISTS friends ("
                "  user1 VARCHAR(32) NOT NULL,"
                "  user2 VARCHAR(32) NOT NULL,"
                "  status VARCHAR(16) NOT NULL,"
                "  action_user VARCHAR(32) NOT NULL,"
                "  PRIMARY KEY (user1, user2),"
                "  CONSTRAINT friends_pair_byteorder CHECK (user1 < user2 COLLATE \"C\")"
                ")"
            );
            // Existing deployments carry the old collation-order CHECK
            // under its auto-generated name "friends_check"; swap it once.
            // Every existing row satisfies the new check — rows were only
            // ever inserted with byte-ordered pairs (inserts where the two
            // orderings disagreed are exactly the ones that failed).
            txn.exec(
                "DO $mig$ BEGIN "
                "IF NOT EXISTS (SELECT 1 FROM pg_constraint "
                "    WHERE conrelid = 'friends'::regclass "
                "      AND conname = 'friends_pair_byteorder') THEN "
                "  ALTER TABLE friends DROP CONSTRAINT IF EXISTS friends_check; "
                "  ALTER TABLE friends ADD CONSTRAINT friends_pair_byteorder "
                "    CHECK (user1 < user2 COLLATE \"C\"); "
                "END IF; "
                "END $mig$"
            );
            // --- Persistent DMs (see docs/superpowers/specs/
            //     2026-05-14-persistent-dms-design.md §1) ---
            txn.exec(
                "CREATE TABLE IF NOT EXISTS dm_messages ("
                "  id BIGSERIAL PRIMARY KEY,"
                "  sender VARCHAR(32) NOT NULL,"
                "  recipient VARCHAR(32) NOT NULL,"
                "  content TEXT NOT NULL,"
                "  sent_at BIGINT NOT NULL,"
                "  edited_at BIGINT NOT NULL DEFAULT 0,"
                "  reply_to BIGINT NOT NULL DEFAULT 0"
                ")"
            );
            // edited_at / reply_to added later — backfill on existing deployments.
            txn.exec(
                "ALTER TABLE dm_messages "
                "ADD COLUMN IF NOT EXISTS edited_at BIGINT NOT NULL DEFAULT 0"
            );
            txn.exec(
                "ALTER TABLE dm_messages "
                "ADD COLUMN IF NOT EXISTS reply_to BIGINT NOT NULL DEFAULT 0"
            );
            // Two-direction lookup ("messages between A and B" hits the
            // same B-tree regardless of who sent which). The LEAST /
            // GREATEST normalisation is what makes a single index serve
            // both query directions.
            txn.exec(
                "CREATE INDEX IF NOT EXISTS dm_messages_pair_idx "
                "ON dm_messages "
                "(LEAST(sender, recipient), GREATEST(sender, recipient), id DESC)"
            );
            // Per-recipient unread queries —
            // `WHERE recipient = me AND id > last_read_id`.
            txn.exec(
                "CREATE INDEX IF NOT EXISTS dm_messages_recipient_idx "
                "ON dm_messages (recipient, id DESC)"
            );
            // E2EE (2026-09-03): the sealed body of an encrypted DM. NULL =
            // a plaintext row (everything written before the feature, and
            // messages between users who haven't set up encryption).
            txn.exec(
                "ALTER TABLE dm_messages "
                "ADD COLUMN IF NOT EXISTS envelope BYTEA"
            );
            txn.exec(
                "CREATE TABLE IF NOT EXISTS dm_read_state ("
                "  reader VARCHAR(32) NOT NULL,"
                "  peer VARCHAR(32) NOT NULL,"
                "  last_read_id BIGINT NOT NULL DEFAULT 0,"
                "  PRIMARY KEY (reader, peer)"
                ")"
            );

            // --- End-to-end encrypted DMs (see docs/superpowers/specs/
            //     2026-09-03-e2ee-dms-design.md) ---
            // Every bundle a user ever published, by monotonic key_id: old
            // envelopes name the key_id they were sealed under, so a peer
            // must be able to look up historical public keys forever.
            txn.exec(
                "CREATE TABLE IF NOT EXISTS user_e2ee_keys ("
                "  username VARCHAR(32) NOT NULL,"
                "  key_id INTEGER NOT NULL,"
                "  dh_pub BYTEA NOT NULL,"
                "  sign_pub BYTEA NOT NULL,"
                "  signature BYTEA NOT NULL,"
                "  created_at BIGINT NOT NULL,"
                "  PRIMARY KEY (username, key_id)"
                ")"
            );
            // The passphrase-wrapped private keys (opaque to central). One
            // blob per user, replaced on every publish / passphrase change.
            txn.exec(
                "CREATE TABLE IF NOT EXISTS user_e2ee_backup ("
                "  username VARCHAR(32) PRIMARY KEY,"
                "  key_id INTEGER NOT NULL,"
                "  blob BYTEA NOT NULL,"
                "  updated_at BIGINT NOT NULL"
                ")"
            );

            // --- Auto-rejoin community memberships (see docs/superpowers/
            //     specs/2026-05-14-auto-rejoin-communities-design.md §1) ---
            txn.exec(
                "CREATE TABLE IF NOT EXISTS user_communities ("
                "  username VARCHAR(32) NOT NULL,"
                "  server_id BIGINT NOT NULL,"
                "  joined_at BIGINT NOT NULL,"
                "  PRIMARY KEY (username, server_id)"
                ")"
            );
            txn.exec(
                "CREATE INDEX IF NOT EXISTS user_communities_user_idx "
                "ON user_communities (username)"
            );

            migrateToUidKeys(txn);
            txn.exec("UPDATE central_schema SET version = 1 WHERE id = 1");
        }
        if (version < 2) {
            // Migration 2: what each bundle's signature binds (uid design,
            // stage 4). Every bundle published so far signs the name.
            txn.exec("ALTER TABLE e2ee_keys ADD COLUMN binding SMALLINT NOT NULL DEFAULT 1");
            txn.exec("UPDATE central_schema SET version = 2 WHERE id = 1");
            std::cout << "[DB] Migration 2: e2ee_keys.binding\n";
        }
        if (version < 3) {
            // Migration 3: username changes (uid design, stage 5). One row
            // per rename: the old name is reserved for the account while
            // changed_at is within NAME_RESERVATION_SECONDS, and the rows
            // of the last day rate-limit renames.
            txn.exec(
                "CREATE TABLE name_changes ("
                "  id BIGSERIAL PRIMARY KEY,"
                "  uid BIGINT NOT NULL REFERENCES users (uid),"
                "  old_username VARCHAR(32) NOT NULL,"
                "  new_username VARCHAR(32) NOT NULL,"
                "  changed_at BIGINT NOT NULL"
                ")"
            );
            txn.exec("CREATE INDEX name_changes_old_idx ON name_changes (old_username, changed_at)");
            txn.exec("CREATE INDEX name_changes_uid_idx ON name_changes (uid, changed_at)");
            txn.exec("UPDATE central_schema SET version = 3 WHERE id = 1");
            std::cout << "[DB] Migration 3: name_changes\n";
        }
        if (version < 4) {
            // Migration 4: cancelling resume tokens (central-hardening-
            // design.md). A resume token from a password sign-in before
            // this time (unix seconds) is refused; 0 = none.
            txn.exec("ALTER TABLE users ADD COLUMN sessions_valid_after BIGINT NOT NULL DEFAULT 0");
            txn.exec("UPDATE central_schema SET version = 4 WHERE id = 1");
            std::cout << "[DB] Migration 4: users.sessions_valid_after\n";
        }
        if (version < 5) {
            // Migration 5: the DM conversation list from a table (review B11).
            // DM_CONVERSATIONS (every login and resume, every page) grouped all
            // of the user's DMs, `sender_uid = $1 OR recipient_uid = $1` with no
            // index on sender_uid: a full scan of dm_messages per call. Now one
            // row per pair (lo_uid < hi_uid) holds the conversation's newest
            // message id, and a page reads the user's rows through the primary
            // key's prefix and the hi_uid index, not their messages. Triggers
            // keep it current for every write path (and for rows tests seed with
            // SQL). One row per pair, last_id in no index and room left in the
            // pages (fillfactor): each DM makes one heap-only update. A row per
            // side with last_id indexed cost ~35% more PostgreSQL CPU at
            // 6,000–8,000 DMs/s in the load test.
            txn.exec(
                "CREATE TABLE dm_conversations ("
                "  lo_uid BIGINT NOT NULL REFERENCES users (uid),"
                "  hi_uid BIGINT NOT NULL REFERENCES users (uid),"
                "  last_id BIGINT NOT NULL,"
                "  PRIMARY KEY (lo_uid, hi_uid)"
                ") WITH (fillfactor = 70)"
            );
            txn.exec("CREATE INDEX dm_conversations_hi_idx ON dm_conversations (hi_uid)");
            // A new message is its pair's newest (GREATEST in case a smaller id
            // commits after a bigger one).
            txn.exec(
                "CREATE FUNCTION dm_conversations_on_insert() RETURNS trigger LANGUAGE plpgsql AS $$ "
                "BEGIN "
                "  INSERT INTO dm_conversations (lo_uid, hi_uid, last_id) "
                "  VALUES (LEAST(NEW.sender_uid, NEW.recipient_uid), GREATEST(NEW.sender_uid, NEW.recipient_uid), NEW.id) "
                "  ON CONFLICT (lo_uid, hi_uid) DO UPDATE "
                "    SET last_id = GREATEST(dm_conversations.last_id, EXCLUDED.last_id); "
                "  RETURN NULL; "
                "END $$"
            );
            // Deleting a pair's newest message falls back to the one before
            // it, and the last one ends the conversation. `last_id = OLD.id`
            // leaves the row alone when a message sent meanwhile took its place.
            txn.exec(
                "CREATE FUNCTION dm_conversations_on_delete() RETURNS trigger LANGUAGE plpgsql AS $$ "
                "DECLARE "
                "  lo BIGINT := LEAST(OLD.sender_uid, OLD.recipient_uid); "
                "  hi BIGINT := GREATEST(OLD.sender_uid, OLD.recipient_uid); "
                "BEGIN "
                "  UPDATE dm_conversations c SET last_id = COALESCE(( "
                "    SELECT MAX(m.id) FROM dm_messages m "
                "    WHERE LEAST(m.sender_uid, m.recipient_uid) = lo "
                "      AND GREATEST(m.sender_uid, m.recipient_uid) = hi), 0) "
                "  WHERE c.lo_uid = lo AND c.hi_uid = hi AND c.last_id = OLD.id; "
                "  DELETE FROM dm_conversations c WHERE c.lo_uid = lo AND c.hi_uid = hi AND c.last_id = 0; "
                "  RETURN NULL; "
                "END $$"
            );
            txn.exec(
                "CREATE TRIGGER dm_conversations_insert AFTER INSERT ON dm_messages "
                "FOR EACH ROW EXECUTE FUNCTION dm_conversations_on_insert()"
            );
            txn.exec(
                "CREATE TRIGGER dm_conversations_delete AFTER DELETE ON dm_messages "
                "FOR EACH ROW EXECUTE FUNCTION dm_conversations_on_delete()"
            );
            const auto filled = txn.exec(
                "INSERT INTO dm_conversations (lo_uid, hi_uid, last_id) "
                "SELECT LEAST(sender_uid, recipient_uid), GREATEST(sender_uid, recipient_uid), MAX(id) "
                "FROM dm_messages GROUP BY 1, 2"
            ).affected_rows();
            // The unread count per conversation: messages from that peer to
            // the reader above their cursor, a range scan of this index. It
            // replaces (recipient_uid, id), which only that count used (and
            // there it walked everything the reader received).
            txn.exec("CREATE INDEX dm_messages_unread_idx ON dm_messages (recipient_uid, sender_uid, id)");
            txn.exec("DROP INDEX IF EXISTS dm_messages_recipient_uid_idx");
            txn.exec("UPDATE central_schema SET version = 5 WHERE id = 1");
            std::cout << "[DB] Migration 5: dm_conversations filled: " << filled << "\n";
        }

        txn.exec(
            "CREATE TABLE IF NOT EXISTS community_invites ("
            "  code VARCHAR(32) PRIMARY KEY,"
            "  host VARCHAR(255) NOT NULL,"
            "  port INTEGER NOT NULL,"
            "  expires_at BIGINT NOT NULL,"  // 0 = never
            "  registered_at BIGINT NOT NULL"
            ")"
        );

        // --- Community server directory. Created here (once at startup)
        //     rather than lazily in upsertCommunityServer, which used to run
        //     this CREATE + two ALTERs on EVERY heartbeat.
        txn.exec(
            "CREATE TABLE IF NOT EXISTS community_servers ("
            "  id SERIAL PRIMARY KEY,"
            "  name VARCHAR(64) NOT NULL,"
            "  description TEXT,"
            "  host_ip VARCHAR(45) NOT NULL,"
            "  port INTEGER NOT NULL,"
            "  member_count INTEGER DEFAULT 0,"
            "  last_heartbeat TIMESTAMP DEFAULT NOW(),"
            "  UNIQUE(host_ip, port)"
            ")"
        );
        // Custom server pictures migration (idempotent on deployed servers).
        txn.exec(
            "ALTER TABLE community_servers "
            "ADD COLUMN IF NOT EXISTS picture BYTEA"
        );
        txn.exec(
            "ALTER TABLE community_servers "
            "ADD COLUMN IF NOT EXISTS picture_version VARCHAR(64) "
            "NOT NULL DEFAULT ''"
        );
        // TLS certificate fingerprint reported by the community (Theme A);
        // served to clients so they can pin the community connection.
        txn.exec(
            "ALTER TABLE community_servers "
            "ADD COLUMN IF NOT EXISTS cert_fingerprint VARCHAR(64) "
            "NOT NULL DEFAULT ''"
        );
        // Public-listing opt-in reported in the heartbeat. Only servers with
        // is_public = TRUE appear in the discovery directory; default FALSE so
        // a server is invite-only until its owner opts in.
        txn.exec(
            "ALTER TABLE community_servers "
            "ADD COLUMN IF NOT EXISTS is_public BOOLEAN NOT NULL DEFAULT FALSE"
        );

        txn.commit();
    } catch (const std::exception& e) {
        std::cerr << "[DB Error] initializeDatabase: " << e.what() << "\n";
        throw;
    }
}

static int64_t countRows(pqxx::work& txn, const char* table) {
    return txn.exec(std::string("SELECT COUNT(*) FROM ") + table)[0][0].as<int64_t>();
}

void AuthManager::migrateToUidKeys(pqxx::work& txn) {
    // Users by uid (docs/superpowers/specs/2026-10-15-uid-identity-design.md).
    // Every table that named a user by username is rebuilt keyed by uid, so a
    // rename can't orphan a row and a later holder of a name can't inherit
    // one. The old tables are renamed legacy_* (not dropped) until the
    // deploy is verified. Runs inside initializeDatabase's transaction: any
    // failure rolls all of it back.

    // users: uid becomes the primary key (reusing its unique index, which
    // the ADD renames to users_pkey); the name stays unique and required.
    txn.exec("ALTER TABLE users DROP CONSTRAINT users_pkey");
    txn.exec("ALTER TABLE users ADD CONSTRAINT users_pkey PRIMARY KEY USING INDEX users_uid_idx");
    txn.exec("ALTER TABLE users ALTER COLUMN username SET NOT NULL");
    txn.exec("ALTER TABLE users ADD CONSTRAINT users_username_key UNIQUE (username)");

    // friends → friendships: the pair in uid order.
    txn.exec(
        "CREATE TABLE friendships ("
        "  uid1 BIGINT NOT NULL REFERENCES users (uid),"
        "  uid2 BIGINT NOT NULL REFERENCES users (uid),"
        "  status VARCHAR(16) NOT NULL,"
        "  action_uid BIGINT NOT NULL REFERENCES users (uid),"
        "  PRIMARY KEY (uid1, uid2),"
        "  CONSTRAINT friendships_pair_order CHECK (uid1 < uid2)"
        ")"
    );
    txn.exec("CREATE INDEX friendships_uid2_idx ON friendships (uid2)");
    txn.exec(
        "INSERT INTO friendships (uid1, uid2, status, action_uid) "
        "SELECT LEAST(a.uid, b.uid), GREATEST(a.uid, b.uid), f.status, x.uid "
        "FROM friends f "
        "JOIN users a ON a.username = f.user1 "
        "JOIN users b ON b.username = f.user2 "
        "JOIN users x ON x.username = f.action_user"
    );
    const int64_t friends_before = countRows(txn, "friends");
    const int64_t friends_after = countRows(txn, "friendships");
    txn.exec("ALTER TABLE friends RENAME TO legacy_friends");

    // dm_messages: uid columns next to the names each row was sent under
    // (kept: a v1 envelope binds them). Rows whose names match no account
    // were left for names nobody held (fixed in 2081fb0); they go.
    const int64_t dms_before = countRows(txn, "dm_messages");
    txn.exec("ALTER TABLE dm_messages ADD COLUMN sender_uid BIGINT, ADD COLUMN recipient_uid BIGINT");
    txn.exec("UPDATE dm_messages m SET sender_uid = u.uid FROM users u WHERE u.username = m.sender");
    txn.exec("UPDATE dm_messages m SET recipient_uid = u.uid FROM users u WHERE u.username = m.recipient");
    txn.exec("DELETE FROM dm_messages WHERE sender_uid IS NULL OR recipient_uid IS NULL");
    const int64_t dms_after = countRows(txn, "dm_messages");
    txn.exec(
        "ALTER TABLE dm_messages "
        "ALTER COLUMN sender_uid SET NOT NULL, "
        "ALTER COLUMN recipient_uid SET NOT NULL, "
        "ADD CONSTRAINT dm_messages_sender_uid_fkey FOREIGN KEY (sender_uid) REFERENCES users (uid), "
        "ADD CONSTRAINT dm_messages_recipient_uid_fkey FOREIGN KEY (recipient_uid) REFERENCES users (uid)"
    );
    txn.exec("DROP INDEX IF EXISTS dm_messages_pair_idx");
    txn.exec("DROP INDEX IF EXISTS dm_messages_recipient_idx");
    // Same shapes as before, on the uid columns: LEAST / GREATEST let one
    // index serve both directions of a conversation.
    txn.exec(
        "CREATE INDEX dm_messages_pair_uid_idx ON dm_messages "
        "(LEAST(sender_uid, recipient_uid), GREATEST(sender_uid, recipient_uid), id DESC)"
    );
    txn.exec("CREATE INDEX dm_messages_recipient_uid_idx ON dm_messages (recipient_uid, id DESC)");

    // dm_read_state → dm_read_cursors.
    txn.exec(
        "CREATE TABLE dm_read_cursors ("
        "  reader_uid BIGINT NOT NULL REFERENCES users (uid),"
        "  peer_uid BIGINT NOT NULL REFERENCES users (uid),"
        "  last_read_id BIGINT NOT NULL DEFAULT 0,"
        "  PRIMARY KEY (reader_uid, peer_uid)"
        ")"
    );
    txn.exec(
        "INSERT INTO dm_read_cursors (reader_uid, peer_uid, last_read_id) "
        "SELECT r.uid, p.uid, s.last_read_id FROM dm_read_state s "
        "JOIN users r ON r.username = s.reader "
        "JOIN users p ON p.username = s.peer"
    );
    const int64_t cursors_before = countRows(txn, "dm_read_state");
    const int64_t cursors_after = countRows(txn, "dm_read_cursors");
    txn.exec("ALTER TABLE dm_read_state RENAME TO legacy_dm_read_state");

    // user_e2ee_keys → e2ee_keys. `signed_name` keeps the name each v1
    // bundle's signature covers: the publisher's name at the time.
    txn.exec(
        "CREATE TABLE e2ee_keys ("
        "  uid BIGINT NOT NULL REFERENCES users (uid),"
        "  key_id INTEGER NOT NULL,"
        "  signed_name VARCHAR(32) NOT NULL,"
        "  dh_pub BYTEA NOT NULL,"
        "  sign_pub BYTEA NOT NULL,"
        "  signature BYTEA NOT NULL,"
        "  created_at BIGINT NOT NULL,"
        "  PRIMARY KEY (uid, key_id)"
        ")"
    );
    txn.exec(
        "INSERT INTO e2ee_keys (uid, key_id, signed_name, dh_pub, sign_pub, signature, created_at) "
        "SELECT u.uid, k.key_id, k.username, k.dh_pub, k.sign_pub, k.signature, k.created_at "
        "FROM user_e2ee_keys k JOIN users u ON u.username = k.username"
    );
    const int64_t keys_before = countRows(txn, "user_e2ee_keys");
    const int64_t keys_after = countRows(txn, "e2ee_keys");
    txn.exec("ALTER TABLE user_e2ee_keys RENAME TO legacy_user_e2ee_keys");

    // user_e2ee_backup → e2ee_backups.
    txn.exec(
        "CREATE TABLE e2ee_backups ("
        "  uid BIGINT PRIMARY KEY REFERENCES users (uid),"
        "  key_id INTEGER NOT NULL,"
        "  blob BYTEA NOT NULL,"
        "  updated_at BIGINT NOT NULL"
        ")"
    );
    txn.exec(
        "INSERT INTO e2ee_backups (uid, key_id, blob, updated_at) "
        "SELECT u.uid, b.key_id, b.blob, b.updated_at "
        "FROM user_e2ee_backup b JOIN users u ON u.username = b.username"
    );
    const int64_t backups_before = countRows(txn, "user_e2ee_backup");
    const int64_t backups_after = countRows(txn, "e2ee_backups");
    txn.exec("ALTER TABLE user_e2ee_backup RENAME TO legacy_user_e2ee_backup");

    // user_communities → community_memberships (+ the server_id index the
    // picture fan-out needs).
    txn.exec(
        "CREATE TABLE community_memberships ("
        "  uid BIGINT NOT NULL REFERENCES users (uid),"
        "  server_id BIGINT NOT NULL,"
        "  joined_at BIGINT NOT NULL,"
        "  PRIMARY KEY (uid, server_id)"
        ")"
    );
    txn.exec("CREATE INDEX community_memberships_server_idx ON community_memberships (server_id)");
    txn.exec(
        "INSERT INTO community_memberships (uid, server_id, joined_at) "
        "SELECT u.uid, c.server_id, c.joined_at "
        "FROM user_communities c JOIN users u ON u.username = c.username"
    );
    const int64_t members_before = countRows(txn, "user_communities");
    const int64_t members_after = countRows(txn, "community_memberships");
    txn.exec("ALTER TABLE user_communities RENAME TO legacy_user_communities");

    std::cout << "[DB] Migration 1 (users by uid): "
              << "friendships " << friends_after << "/" << friends_before
              << ", dm_messages " << dms_after << "/" << dms_before
              << " (" << (dms_before - dms_after) << " for unknown names removed)"
              << ", read cursors " << cursors_after << "/" << cursors_before
              << ", e2ee keys " << keys_after << "/" << keys_before
              << ", e2ee backups " << backups_after << "/" << backups_before
              << ", memberships " << members_after << "/" << members_before
              << ". Old tables kept as legacy_*.\n";
}

std::string AuthManager::registerUser(const std::string& username, const std::string& email, const std::string& password) {
    if (auto bad = validateUsername(username); !bad.empty()) return bad;
    // The column holds 128 characters; a longer address would only fail the
    // insert as "Registration failed".
    if (email.empty() || email.size() > 128 || email.find('@') == std::string::npos) return "Invalid email address.";
    // Reject empty / trivially-weak passwords at registration. Only affects
    // new accounts — existing users (any length) can still log in.
    if (password.length() < 8) return "Password must be at least 8 characters.";

    if (userExists(username, email)) {
        return "Username or email already exists.";
    }
    // A name someone renamed away from stays theirs for the reservation.
    try {
        auto lease = pool_.acquire();
        pqxx::connection& conn = *lease;
        pqxx::work txn(conn);
        if (nameReservedForOther(txn, username, 0)) return "Username or email already exists.";
    } catch (const std::exception& e) {
        std::cerr << "[DB Error] registerUser reservation check: " << e.what() << "\n";
        return "Registration failed. Please try again.";
    }

    std::string hash = hashPassword(password);
    if (!insertUser(username, email, hash)) {
        // Insert failed (DB error, or a unique-constraint race between the
        // userExists() check above and here). Don't report success — the
        // account was not created, and the user would otherwise be told to
        // log in with credentials that don't exist.
        return "Registration failed. Please try again.";
    }
    return "";
}

std::string AuthManager::validateUsername(const std::string& username) {
    if (username.length() < 3 || username.length() > 32) return "Invalid username length.";
    // Lowercase-only ASCII (2026-08-31). Case-mixed and non-ASCII names
    // were both a UX trap (exact-match friend lookups, "Usernames are
    // case-sensitive") and the trigger for the friends-pair CHECK
    // mismatch fixed above. Existing mixed-case accounts keep working —
    // this gate runs at registration and on renames only. `char` may be signed:
    // non-ASCII UTF-8 bytes land in the negative range and fail every
    // range test below, so multibyte names are rejected too.
    for (const char c : username) {
        const bool ok = (c >= 'a' && c <= 'z') || (c >= '0' && c <= '9') ||
                        c == '_' || c == '.' || c == '-';
        if (!ok) return "Usernames may only contain lowercase letters, digits, '.', '_' and '-'.";
    }
    return "";
}

std::optional<AuthManager::LoginResult> AuthManager::authenticateUser(const std::string& username, const std::string& password,
                                                                      bool* server_error) {
    // The hash and the uid come from one row read, so a token is never
    // signed with a uid fetched separately (a second query that failed
    // used to mint uid 0, which sends communities down the name-only path).
    // A name the account renamed away from still signs it in while it is
    // reserved (a device that saved the old name); the token and the
    // response carry the current name.
    bool db_error = false;
    auto stored = getCredentials(username, db_error);
    if (!stored && !db_error) stored = getCredentialsByReservedName(username, db_error);
    if (db_error) {
        // Not a wrong password: the client keeps its saved sign-in for this
        // and the attempt isn't counted against the account.
        if (server_error) *server_error = true;
        return std::nullopt;
    }

    if (!stored) {
        // Equalize timing with the "user exists, wrong password" path so
        // login latency can't be used to enumerate valid usernames: run a
        // bcrypt hash at the same cost and discard it. Read the result
        // through a volatile so the optimizer can't elide the work.
        std::string sink = hashPassword(password);
        volatile char keep = sink.empty() ? 0 : sink[0];
        (void)keep;
        return std::nullopt;
    }
    if (!verifyPassword(password, stored->password_hash)) {
        return std::nullopt;
    }
    if (stored->uid <= 0) {
        std::cerr << "[Auth] Refusing to sign a token without a uid for " << username << "\n";
        return std::nullopt;
    }

    auto token = mintToken(stored->uid, stored->username);
    if (!token) {
        if (server_error) *server_error = true;
        return std::nullopt;
    }
    return LoginResult{*token, stored->uid, stored->username};
}

std::optional<std::string> AuthManager::mintToken(int64_t uid, const std::string& username) {
    // Generate a JWT valid for 24 hours. Ed25519 (EdDSA): only this
    // process holds the private key, so a community server — which gets
    // the public key — can verify tokens but never mint one.
    auto now = std::chrono::system_clock::now();
    try {
        return jwt::create()
            .set_issuer(TOKEN_ISSUER)
            .set_type("JWS")
            .set_subject(username)
            .set_payload_claim("uid", jwt::claim(nlohmann::json(uid)))
            .set_issued_at(now)
            .set_expires_at(now + std::chrono::hours(24))
            .sign(signer_);
    } catch (const std::exception& e) {
        std::cerr << "[Auth] JWT signing failed: " << e.what() << "\n";
        return std::nullopt;
    }
}

bool AuthManager::nameReservedForOther(pqxx::work& txn, const std::string& username, int64_t uid) {
    const int64_t cutoff = static_cast<int64_t>(std::time(nullptr)) - NAME_RESERVATION_SECONDS;
    pqxx::result rs = txn.exec_params(
        "SELECT 1 FROM name_changes WHERE old_username = $1 AND changed_at > $2 AND uid <> $3 LIMIT 1",
        username, cutoff, uid);
    return !rs.empty();
}

static void prune_name_changes(pqxx::work& txn) {
    const int64_t cutoff = static_cast<int64_t>(std::time(nullptr)) - AuthManager::NAME_RESERVATION_SECONDS;
    txn.exec_params("DELETE FROM name_changes WHERE changed_at <= $1", cutoff);
}

void AuthManager::pruneNameChanges() {
    try {
        auto lease = pool_.acquire();
        pqxx::connection& conn = *lease;
        pqxx::work txn(conn);
        prune_name_changes(txn);
        txn.commit();
    } catch (const std::exception& e) {
        std::cerr << "[DB Error] pruneNameChanges: " << e.what() << "\n";
    }
}

std::optional<AuthManager::StoredCredentials> AuthManager::getCredentialsByReservedName(const std::string& username,
                                                                                       bool& db_error) {
    try {
        auto lease = pool_.acquire();
        pqxx::connection& conn = *lease;
        pqxx::work txn(conn);
        const int64_t cutoff = static_cast<int64_t>(std::time(nullptr)) - NAME_RESERVATION_SECONDS;
        // The latest account to leave the name, if within the reservation.
        pqxx::result res = txn.exec_params(
            "SELECT u.uid, u.username, u.password_hash FROM name_changes c "
            "JOIN users u ON u.uid = c.uid "
            "WHERE c.old_username = $1 AND c.changed_at > $2 "
            "ORDER BY c.changed_at DESC, c.id DESC LIMIT 1",
            username, cutoff);
        if (res.empty()) return std::nullopt;
        StoredCredentials creds;
        creds.uid = res[0][0].as<int64_t>();
        creds.username = res[0][1].as<std::string>();
        creds.password_hash = res[0][2].as<std::string>();
        return creds;
    } catch (const std::exception& e) {
        std::cerr << "[DB Error] getCredentialsByReservedName: " << e.what() << "\n";
        db_error = true;
        return std::nullopt;
    }
}

AuthManager::RenameResult AuthManager::renameUser(int64_t uid, const std::string& new_username,
                                                  const std::string& password) {
    RenameResult out;
    if (auto bad = validateUsername(new_username); !bad.empty()) {
        out.error = bad;
        return out;
    }
    try {
        auto lease = pool_.acquire();
        pqxx::connection& conn = *lease;
        pqxx::work txn(conn);
        pqxx::result me = txn.exec_params(
            "SELECT username, password_hash FROM users WHERE uid = $1 FOR UPDATE", uid);
        if (me.empty()) {
            out.error = "Your account wasn't found.";
            return out;
        }
        out.old_username = me[0][0].as<std::string>();
        if (!verifyPassword(password, me[0][1].as<std::string>())) {
            out.error = "Wrong password.";
            return out;
        }
        if (new_username == out.old_username) {
            out.error = "That's already your username.";
            return out;
        }
        const int64_t now = static_cast<int64_t>(std::time(nullptr));
        const int64_t recent = txn.exec_params(
            "SELECT count(*) FROM name_changes WHERE uid = $1 AND changed_at > $2",
            uid, now - 24 * 3600)[0][0].as<int64_t>();
        if (recent >= RENAMES_PER_DAY) {
            out.error = "You can change your username twice a day. Try again tomorrow.";
            return out;
        }
        // The renamer's row is locked (FOR UPDATE above), so their own
        // renames serialize; another account taking the name meanwhile is
        // stopped by the unique index on users.username (the UPDATE fails
        // and the rename reports an error).
        if (!txn.exec_params("SELECT 1 FROM users WHERE username = $1", new_username).empty() ||
            nameReservedForOther(txn, new_username, uid)) {
            out.error = "That username is taken.";
            return out;
        }
        txn.exec_params("UPDATE users SET username = $1 WHERE uid = $2", new_username, uid);
        txn.exec_params(
            "INSERT INTO name_changes (uid, old_username, new_username, changed_at) VALUES ($1, $2, $3, $4)",
            uid, out.old_username, new_username, now);
        prune_name_changes(txn);
        txn.commit();
    } catch (const std::exception& e) {
        std::cerr << "[DB Error] renameUser: " << e.what() << "\n";
        out.error = "Couldn't change your username. Please try again.";
        return out;
    }
    out.username = new_username;
    // Without a token (signing failed) the client signs in again by name.
    if (auto token = mintToken(uid, new_username)) out.token = *token;
    return out;
}

/// The uid claim (integer, or a number from an older encoder); 0 if absent.
template <class Decoded>
static int64_t uid_claim(const Decoded& decoded) {
    if (!decoded.has_payload_claim("uid")) return 0;
    const auto claim = decoded.get_payload_claim("uid");
    if (claim.get_type() == jwt::json::type::integer) return claim.as_integer();
    if (claim.get_type() == jwt::json::type::number) return static_cast<int64_t>(claim.as_number());
    return 0;
}

std::optional<AuthManager::TokenClaims> AuthManager::verifyToken(const std::string& token) {
    try {
        const auto decoded = jwt::decode(token);
        session_verifier_.verify(decoded);  // signature, issuer, expiry
        if (decoded.has_audience()) return std::nullopt;  // a resume token
        TokenClaims claims;
        claims.uid = uid_claim(decoded);
        claims.expires_at = std::chrono::duration_cast<std::chrono::seconds>(
            decoded.get_expires_at().time_since_epoch()).count();
        return claims;
    } catch (const std::exception&) {
        return std::nullopt;
    }
}

std::optional<std::string> AuthManager::mintResumeToken(int64_t uid, int64_t signed_in_at) {
    using clock = std::chrono::system_clock;
    try {
        return jwt::create()
            .set_issuer(TOKEN_ISSUER)
            .set_audience(RESUME_AUDIENCE)
            .set_type("JWS")
            .set_payload_claim("uid", jwt::claim(nlohmann::json(uid)))
            .set_issued_at(clock::time_point(std::chrono::seconds(signed_in_at)))
            .set_expires_at(clock::time_point(std::chrono::seconds(signed_in_at + RESUME_LIFETIME_SECONDS)))
            .sign(signer_);
    } catch (const std::exception& e) {
        std::cerr << "[Auth] Resume token signing failed: " << e.what() << "\n";
        return std::nullopt;
    }
}

std::optional<AuthManager::LoginResult> AuthManager::resumeSession(const std::string& resume_token) {
    int64_t uid = 0;
    int64_t signed_in_at = 0;
    try {
        const auto decoded = jwt::decode(resume_token);
        resume_verifier_.verify(decoded);  // signature, issuer, audience, expiry
        uid = uid_claim(decoded);
        signed_in_at = std::chrono::duration_cast<std::chrono::seconds>(
            decoded.get_issued_at().time_since_epoch()).count();
    } catch (const std::exception&) {
        return std::nullopt;
    }
    if (uid <= 0) return std::nullopt;
    std::string username;
    try {
        auto lease = pool_.acquire();
        pqxx::connection& conn = *lease;
        pqxx::nontransaction txn(conn);
        const pqxx::result rs = txn.exec_params(
            "SELECT username, sessions_valid_after FROM users WHERE uid = $1", uid);
        if (rs.empty()) return std::nullopt;
        // Cancelled: the account's resume tokens from before the cutoff.
        if (signed_in_at < rs[0][1].as<int64_t>(0)) return std::nullopt;
        username = rs[0][0].as<std::string>();
    } catch (const std::exception& e) {
        std::cerr << "[DB Error] resumeSession: " << e.what() << "\n";
        return std::nullopt;
    }
    auto token = mintToken(uid, username);
    if (!token) return std::nullopt;
    return LoginResult{*token, uid, username};
}

AuthManager::UserRef AuthManager::resolveUser(int64_t uid, const std::string& username) {
    UserRef out;
    if (uid <= 0 && username.empty()) return out;
    try {
        auto lease = pool_.acquire();
        pqxx::connection& conn = *lease;
        pqxx::work txn(conn);
        pqxx::result res = uid > 0
            ? txn.exec_params("SELECT uid, username FROM users WHERE uid = $1", uid)
            : txn.exec_params("SELECT uid, username FROM users WHERE username = $1", username);
        if (res.empty()) return out;
        out.uid = res[0][0].as<int64_t>();
        out.username = res[0][1].as<std::string>();
        return out;
    } catch (const std::exception& e) {
        std::cerr << "[DB Error] resolveUser: " << e.what() << "\n";
        out.uid = -1;
        return out;
    }
}

// --- Database & Hashing ---

bool AuthManager::userExists(const std::string& username, const std::string& email) {
    try {
        auto lease = pool_.acquire();
        pqxx::connection& conn = *lease;
        pqxx::work txn(conn);
        // Check for either a conflicting username or conflicting email
        pqxx::result res = txn.exec_params(
            "SELECT 1 FROM users WHERE username = $1 OR email = $2", 
            username, email
        );
        return !res.empty();
    } catch (const std::exception& e) {
        std::cerr << "[DB Error] userExists: " << e.what() << "\n";
        return true; 
    }
}

bool AuthManager::insertUser(const std::string& username, const std::string& email, const std::string& hash) {
    try {
        auto lease = pool_.acquire();
        pqxx::connection& conn = *lease;
        pqxx::work txn(conn);
        txn.exec_params(
            "INSERT INTO users (username, email, password_hash) VALUES ($1, $2, $3)",
            username, email, hash
        );
        txn.commit();
        return true;
    } catch (const std::exception& e) {
        std::cerr << "[DB Error] insertUser: " << e.what() << "\n";
        return false;
    }
}

std::optional<AuthManager::StoredCredentials> AuthManager::getCredentials(const std::string& username, bool& db_error) {
    try {
        auto lease = pool_.acquire();
        pqxx::connection& conn = *lease;
        pqxx::work txn(conn);
        pqxx::result res = txn.exec_params(
            "SELECT uid, username, password_hash FROM users WHERE username = $1", username);

        if (res.empty()) {
            return std::nullopt;
        }
        StoredCredentials creds;
        creds.uid = res[0][0].is_null() ? 0 : res[0][0].as<int64_t>();
        creds.username = res[0][1].as<std::string>();
        creds.password_hash = res[0][2].as<std::string>();
        return creds;
    } catch (const std::exception& e) {
        std::cerr << "[DB Error] getCredentials: " << e.what() << "\n";
        db_error = true;
        return std::nullopt;
    }
}

std::string AuthManager::hashPassword(const std::string& plain_password) {
    return bcrypt::hash(plain_password);
}

bool AuthManager::verifyPassword(const std::string& plain_password, const std::string& hash) {
    return bcrypt::verify(plain_password, hash);
}

/// A community's stored description, cut to MAX_COMMUNITY_DESCRIPTION: the
/// heartbeat clamps it now, but rows from before that could hold anything a
/// frame could carry. Queries read it as LEFT(description, cap) (characters,
/// so never fewer bytes than the cap), so an oversized row isn't even
/// fetched whole.
static std::string communityDescription(const pqxx::field& f) {
    return f.is_null() ? std::string()
                       : chatproj::utf8_prefix(f.as<std::string>(), chatproj::MAX_COMMUNITY_DESCRIPTION);
}

std::vector<chatproj::CommunityServerInfo> AuthManager::getCommunityServers() {
    std::vector<chatproj::CommunityServerInfo> servers;
    try {
        auto lease = pool_.acquire();
        pqxx::connection& conn = *lease;
        pqxx::work txn(conn);
        
        // Fetch up to 50 LIVE public community servers sorted by member
        // count. Communities heartbeat every 60 s; a row without one for
        // 5 minutes is down (or gone for good) and used to sit in the
        // directory forever, since last_heartbeat was written but never
        // read. Memberships (getUserCommunities) are deliberately NOT
        // filtered — a member's server that's temporarily offline is
        // still their server.
        pqxx::result res = txn.exec_params(
            "SELECT id, name, LEFT(description, $1), host_ip, port, member_count, "
            "       COALESCE(picture_version, ''), COALESCE(cert_fingerprint, '') "
            "FROM community_servers "
            "WHERE is_public = TRUE "
            "  AND last_heartbeat > NOW() - INTERVAL '5 minutes' "
            "ORDER BY member_count DESC LIMIT 50",
            static_cast<int>(chatproj::MAX_COMMUNITY_DESCRIPTION)
        );

        for (auto row : res) {
            chatproj::CommunityServerInfo info;
            info.set_id(row[0].as<int>());
            info.set_name(row[1].as<std::string>());
            info.set_description(communityDescription(row[2]));
            info.set_host_ip(row[3].as<std::string>());
            info.set_port(row[4].as<int>());
            info.set_member_count(row[5].as<int>());
            info.set_picture_version(row[6].as<std::string>());
            info.set_cert_fingerprint(row[7].as<std::string>());
            servers.push_back(info);
        }
    } catch (const std::exception& e) {
        std::cerr << "[DB Error] getCommunityServers: " << e.what() << "\n";
    }
    return servers;
}

int AuthManager::upsertCommunityServer(const std::string& name, const std::string& description,
                                       const std::string& host_ip, int port, int member_count,
                                       int64_t known_id, const std::string& cert_fingerprint,
                                       bool is_public) {
    try {
        auto lease = pool_.acquire();
        pqxx::connection& conn = *lease;
        pqxx::work txn(conn);
        if (known_id > 0) {
            // Identity by id: the community moved (new public IP / port)
            // or simply restarted. Any OTHER row already holding the new
            // address is a stale former identity that no longer
            // heartbeats — drop it so the UNIQUE(host_ip, port) update
            // can land. (A shared-secret holder could already overwrite
            // any (host, port) row before this; Theme A separates the
            // secrets.)
            txn.exec_params(
                "DELETE FROM community_servers WHERE host_ip = $1 AND port = $2 AND id <> $3",
                host_ip, port, known_id);
            pqxx::result up = txn.exec_params(
                "UPDATE community_servers "
                "SET name = $1, description = $2, host_ip = $3, port = $4, "
                "    member_count = $5, last_heartbeat = NOW(), cert_fingerprint = $7, "
                "    is_public = $8 "
                "WHERE id = $6 RETURNING id",
                name, description, host_ip, port, member_count, known_id, cert_fingerprint, is_public);
            if (!up.empty()) {
                txn.commit();
                return up[0][0].as<int>();
            }
            // Unknown id (central DB was reset): fall through and mint a
            // new row keyed by address; the community re-caches the id.
        }
        // Schema for community_servers (incl. the picture columns) is
        // created once in initializeDatabase — no DDL on the heartbeat path.
        pqxx::result rs = txn.exec_params(
            "INSERT INTO community_servers (name, description, host_ip, port, member_count, last_heartbeat, cert_fingerprint, is_public) "
            "VALUES ($1, $2, $3, $4, $5, NOW(), $6, $7) "
            "ON CONFLICT (host_ip, port) DO UPDATE SET "
            "name = EXCLUDED.name, description = EXCLUDED.description, "
            "member_count = EXCLUDED.member_count, last_heartbeat = NOW(), "
            "cert_fingerprint = EXCLUDED.cert_fingerprint, "
            "is_public = EXCLUDED.is_public "
            "RETURNING id",
            name, description, host_ip, port, member_count, cert_fingerprint, is_public
        );
        txn.commit();
        if (rs.empty()) return 0;
        return rs[0][0].as<int>();
    } catch (const std::exception& e) {
        std::cerr << "[DB Error] upsertCommunityServer: " << e.what() << "\n";
        return 0;
    }
}

void AuthManager::registerCommunityInvite(const std::string& code, const std::string& host, int port, int64_t expires_at) {
    try {
        auto lease = pool_.acquire();
        pqxx::connection& conn = *lease;
        pqxx::work txn(conn);
        const int64_t now = std::chrono::duration_cast<std::chrono::seconds>(
                                std::chrono::system_clock::now().time_since_epoch())
                                .count();
        txn.exec_params(
            "INSERT INTO community_invites (code, host, port, expires_at, registered_at) "
            "VALUES ($1, $2, $3, $4, $5) "
            "ON CONFLICT (code) DO UPDATE SET "
            "host = EXCLUDED.host, port = EXCLUDED.port, "
            "expires_at = EXCLUDED.expires_at, registered_at = EXCLUDED.registered_at",
            code, host, port, expires_at, now
        );
        txn.commit();
    } catch (const std::exception& e) {
        std::cerr << "[DB Error] registerCommunityInvite: " << e.what() << "\n";
    }
}

void AuthManager::unregisterCommunityInvite(const std::string& code) {
    try {
        auto lease = pool_.acquire();
        pqxx::connection& conn = *lease;
        pqxx::work txn(conn);
        txn.exec_params("DELETE FROM community_invites WHERE code = $1", code);
        txn.commit();
    } catch (const std::exception& e) {
        std::cerr << "[DB Error] unregisterCommunityInvite: " << e.what() << "\n";
    }
}

std::optional<AuthManager::ResolvedInvite> AuthManager::resolveCommunityInvite(const std::string& code) {
    try {
        auto lease = pool_.acquire();
        pqxx::connection& conn = *lease;
        pqxx::work txn(conn);
        // LEFT JOIN, not a subselect: the invite must still resolve to
        // host:port when the community has no directory row yet. The
        // upsert keeps (host_ip, port) unique, so LIMIT 1 is belt and
        // braces.
        pqxx::result res = txn.exec_params(
            "SELECT i.host, i.port, i.expires_at, "
            "       COALESCE(cs.cert_fingerprint, ''), "
            "       COALESCE(cs.id, 0), COALESCE(cs.name, ''), COALESCE(LEFT(cs.description, $2), ''), "
            "       COALESCE(cs.member_count, 0), COALESCE(cs.picture_version, '') "
            "FROM community_invites i "
            "LEFT JOIN community_servers cs ON cs.host_ip = i.host AND cs.port = i.port "
            "WHERE i.code = $1 LIMIT 1",
            code, static_cast<int>(chatproj::MAX_COMMUNITY_DESCRIPTION)
        );
        if (res.empty()) return std::nullopt;

        const int64_t expires_at = res[0][2].as<int64_t>();
        if (expires_at != 0) {
            const int64_t now = std::chrono::duration_cast<std::chrono::seconds>(
                                    std::chrono::system_clock::now().time_since_epoch())
                                    .count();
            if (now >= expires_at) {
                // Lazily prune the expired entry so the table doesn't grow
                // forever. Reuse the ALREADY-OPEN transaction: opening a
                // second pqxx::work on the same connection throws
                // usage_error (libpqxx permits one active txn per
                // connection), which the old nested-cleanup swallowed — so
                // the prune never actually ran.
                try {
                    txn.exec_params("DELETE FROM community_invites WHERE code = $1", code);
                    txn.commit();
                } catch (const std::exception& e) {
                    std::cerr << "[DB Error] resolveCommunityInvite prune: " << e.what() << "\n";
                }
                return std::nullopt;
            }
        }

        ResolvedInvite out;
        out.host = res[0][0].as<std::string>();
        out.port = res[0][1].as<int>();
        out.cert_fingerprint = res[0][3].as<std::string>();
        out.server_id = res[0][4].as<int64_t>();
        out.name = res[0][5].as<std::string>();
        out.description = communityDescription(res[0][6]);
        out.member_count = res[0][7].as<int>();
        out.picture_version = res[0][8].as<std::string>();
        return out;
    } catch (const std::exception& e) {
        std::cerr << "[DB Error] resolveCommunityInvite: " << e.what() << "\n";
        return std::nullopt;
    }
}

// --- Friend System ---
// friendships stores each pair once, in uid order (uid1 < uid2).

std::string AuthManager::handleFriendAction(int64_t requester, chatproj::FriendActionType action, int64_t target) {
    if (requester == target) return "Cannot perform action on yourself.";
    try {
        auto lease = pool_.acquire();
        pqxx::connection& conn = *lease;
        pqxx::work txn(conn);

        const int64_t u1 = (std::min)(requester, target);
        const int64_t u2 = (std::max)(requester, target);

        // The pair's row, locked so two actions on one pair can't interleave.
        pqxx::result rel = txn.exec_params(
            "SELECT status, action_uid FROM friendships WHERE uid1 = $1 AND uid2 = $2 FOR UPDATE", u1, u2);
        const std::string status = rel.empty() ? std::string() : rel[0][0].as<std::string>();
        const int64_t action_uid = rel.empty() ? 0 : rel[0][1].as<int64_t>();
        // A block belongs to whoever placed it: only they can lift it. The
        // other side blocking back makes it two-sided (BLOCKED_BOTH) rather
        // than taking it over, and their REMOVE / REJECT is a no-op — the
        // blocked side never sees the row (getFriends) or learns of it here.
        const bool both = status == "BLOCKED_BOTH";
        const bool blocked_by_them = status == "BLOCKED" && action_uid != requester;
        const bool blocked_by_me = status == "BLOCKED" && action_uid == requester;

        if (action == chatproj::FriendActionType::ADD) {
            if (!rel.empty()) {
                if (both || blocked_by_them || blocked_by_me) return "Cannot add user.";
                if (status == "ACCEPTED") return "Already friends.";
                return "Friend request already exists.";
            }
            // Requests waiting for the target, capped so that strangers
            // can't grow someone's friend list without end (review A1).
            const int64_t waiting = txn.exec_params(
                "SELECT count(*) FROM friendships "
                "WHERE (uid1 = $1 OR uid2 = $1) AND status = 'PENDING' AND action_uid <> $1",
                target)[0][0].as<int64_t>();
            if (waiting >= MAX_PENDING_INCOMING) return "This user can't take more friend requests right now.";
            txn.exec_params(
                "INSERT INTO friendships (uid1, uid2, status, action_uid) VALUES ($1, $2, 'PENDING', $3)",
                u1, u2, requester
            );
        } else if (action == chatproj::FriendActionType::ACCEPT) {
            if (rel.empty() || status != "PENDING" || action_uid == requester) {
                return "No pending friend request to accept.";
            }
            txn.exec_params("UPDATE friendships SET status = 'ACCEPTED' WHERE uid1 = $1 AND uid2 = $2", u1, u2);
        } else if (action == chatproj::FriendActionType::REJECT || action == chatproj::FriendActionType::REMOVE) {
            if (both) {
                // Lift only my side; theirs stays.
                txn.exec_params(
                    "UPDATE friendships SET status = 'BLOCKED', action_uid = $3 WHERE uid1 = $1 AND uid2 = $2",
                    u1, u2, target);
            } else if (!blocked_by_them) {
                txn.exec_params("DELETE FROM friendships WHERE uid1 = $1 AND uid2 = $2", u1, u2);
            }
        } else if (action == chatproj::FriendActionType::BLOCK) {
            if (blocked_by_them) {
                txn.exec_params(
                    "UPDATE friendships SET status = 'BLOCKED_BOTH' WHERE uid1 = $1 AND uid2 = $2", u1, u2);
            } else if (!blocked_by_me && !both) {
                txn.exec_params(
                    "INSERT INTO friendships (uid1, uid2, status, action_uid) VALUES ($1, $2, 'BLOCKED', $3) "
                    "ON CONFLICT (uid1, uid2) DO UPDATE SET status = 'BLOCKED', action_uid = EXCLUDED.action_uid",
                    u1, u2, requester
                );
            }
        } else {
            return "Unknown action.";
        }

        txn.commit();
        return "";
    } catch (const std::exception& e) {
        std::cerr << "[DB Error] handleFriendAction: " << e.what() << "\n";
        return "Database error.";
    }
}

bool AuthManager::isBlocked(int64_t a, int64_t b) {
    try {
        auto lease = pool_.acquire();
        pqxx::connection& conn = *lease;
        pqxx::work txn(conn);
        pqxx::result res = txn.exec_params(
            "SELECT 1 FROM friendships WHERE uid1 = $1 AND uid2 = $2 AND status IN ('BLOCKED', 'BLOCKED_BOTH')",
            (std::min)(a, b), (std::max)(a, b)
        );
        return !res.empty();
    } catch (const std::exception& e) {
        std::cerr << "[DB Error] isBlocked: " << e.what() << "\n";
        // Fail open (treat as not-blocked): this gate runs for every DM,
        // so failing closed would drop ALL DMs during a DB blip. A blocked
        // user's message slipping through a rare outage is low harm.
        return false;
    }
}

bool AuthManager::areFriends(int64_t a, int64_t b) {
    try {
        auto lease = pool_.acquire();
        pqxx::connection& conn = *lease;
        pqxx::work txn(conn);
        pqxx::result res = txn.exec_params(
            "SELECT 1 FROM friendships WHERE uid1 = $1 AND uid2 = $2 AND status = 'ACCEPTED'",
            (std::min)(a, b), (std::max)(a, b)
        );
        return !res.empty();
    } catch (const std::exception& e) {
        std::cerr << "[DB Error] areFriends: " << e.what() << "\n";
        return false;
    }
}

std::optional<std::vector<int64_t>> AuthManager::friendsInCommunity(int64_t uid, int64_t server_id) {
    try {
        auto lease = pool_.acquire();
        pqxx::connection& conn = *lease;
        pqxx::work txn(conn);
        // Only about a community the caller is in: the answer is then their
        // mutual communities with friends, which its member list shows anyway.
        pqxx::result in = txn.exec_params(
            "SELECT 1 FROM community_memberships WHERE uid = $1 AND server_id = $2", uid, server_id);
        if (in.empty()) {
            txn.commit();
            return std::nullopt;
        }
        // Friends first (friendships' key and uid2 index), then each one's
        // membership (community_memberships' key).
        pqxx::result rs = txn.exec_params(
            "SELECT fr.u FROM ( "
            "  SELECT uid2 AS u FROM friendships WHERE uid1 = $1 AND status = 'ACCEPTED' "
            "  UNION ALL "
            "  SELECT uid1 FROM friendships WHERE uid2 = $1 AND status = 'ACCEPTED' "
            ") fr "
            "WHERE EXISTS (SELECT 1 FROM community_memberships cm WHERE cm.uid = fr.u AND cm.server_id = $2)",
            uid, server_id);
        txn.commit();
        std::vector<int64_t> out;
        out.reserve(rs.size());
        for (const auto& row : rs) out.push_back(row[0].as<int64_t>());
        return out;
    } catch (const std::exception& e) {
        std::cerr << "[DB Error] friendsInCommunity: " << e.what() << "\n";
        return std::nullopt;
    }
}

std::vector<chatproj::FriendInfo> AuthManager::getFriends(int64_t uid) {
    std::vector<chatproj::FriendInfo> friends;
    try {
        auto lease = pool_.acquire();
        pqxx::connection& conn = *lease;
        pqxx::work txn(conn);

        // JOIN users on the other side of each pair for its current name
        // and avatar_version. Clients use the version to invalidate their
        // per-user avatar cache without an extra fetch per friend.
        // At most MAX_FRIEND_ROWS, so the list fits one frame (review A1):
        // accepted friends first, then the user's blocks, requests they
        // sent, requests they received. A block placed on the user is never
        // shown, so it takes no place either.
        pqxx::result res = txn.exec_params(
            "SELECT u.uid, u.username, f.status, f.action_uid, u.avatar_version "
            "FROM friendships f "
            "JOIN users u ON u.uid = CASE WHEN f.uid1 = $1 THEN f.uid2 ELSE f.uid1 END "
            "WHERE (f.uid1 = $1 OR f.uid2 = $1) "
            "  AND NOT (f.status = 'BLOCKED' AND f.action_uid <> $1) "
            "ORDER BY CASE "
            "    WHEN f.status = 'ACCEPTED' THEN 0 "
            "    WHEN f.status IN ('BLOCKED', 'BLOCKED_BOTH') THEN 1 "
            "    WHEN f.action_uid = $1 THEN 2 "
            "    ELSE 3 END, u.uid "
            "LIMIT $2",
            uid, MAX_FRIEND_ROWS
        );

        for (const auto& row : res) {
            const int64_t friend_uid = row[0].as<int64_t>();
            std::string friend_name = row[1].as<std::string>();
            std::string status = row[2].as<std::string>();
            const int64_t action_uid = row[3].as<int64_t>();
            std::string avatar_version = row[4].as<std::string>("");

            chatproj::FriendInfo info;
            info.set_uid(friend_uid);
            info.set_username(friend_name);
            info.set_avatar_version(avatar_version);

            if (status == "ACCEPTED") {
                info.set_status(chatproj::FriendInfo::OFFLINE); // Default
            } else if (status == "PENDING") {
                if (action_uid == uid) {
                    info.set_status(chatproj::FriendInfo::PENDING_OUTGOING);
                } else {
                    info.set_status(chatproj::FriendInfo::PENDING_INCOMING);
                }
            } else if (status == "BLOCKED" || status == "BLOCKED_BOTH") {
                if (status == "BLOCKED_BOTH" || action_uid == uid) {
                    info.set_status(chatproj::FriendInfo::BLOCKED);
                } else {
                    continue; // Do not show to the blocked user
                }
            }
            friends.push_back(info);
        }
    } catch (const std::exception& e) {
        std::cerr << "[DB Error] getFriends: " << e.what() << "\n";
    }
    return friends;
}

// ─── Avatar storage ──────────────────────────────────────────────────────

std::string AuthManager::setAvatar(int64_t uid, const std::string& data) {
    auto lease = pool_.acquire();
    pqxx::connection& conn = *lease;
    pqxx::work txn(conn);
    if (data.empty()) {
        // Remove: NULL the bytes, clear the version.
        txn.exec_params(
            "UPDATE users SET avatar = NULL, avatar_version = '' "
            "WHERE uid = $1",
            uid);
        txn.commit();
        return std::string();
    }
    const std::string version = chatproj::sha256(data);
    // pqxx::binarystring expects `const unsigned char*` in pqxx 7+;
    // std::string::data() is `const char*`. The cast is byte-identical.
    txn.exec_params(
        "UPDATE users SET avatar = $1, avatar_version = $2 "
        "WHERE uid = $3",
        pqxx::binarystring(
            reinterpret_cast<const unsigned char*>(data.data()),
            data.size()),
        version, uid);
    txn.commit();
    return version;
}

std::pair<std::string, std::string> AuthManager::getAvatar(int64_t uid) {
    try {
        auto lease = pool_.acquire();
        pqxx::connection& conn = *lease;
        pqxx::work txn(conn);
        pqxx::result rs = txn.exec_params(
            "SELECT avatar, avatar_version FROM users WHERE uid = $1",
            uid);
        txn.commit();
        if (rs.empty()) return {std::string(), std::string()};
        std::string version = rs[0]["avatar_version"].as<std::string>("");
        std::string data;
        if (!rs[0]["avatar"].is_null()) {
            pqxx::binarystring blob(rs[0]["avatar"]);
            // blob.data() is `const unsigned char*` in pqxx 7+;
            // std::string::assign needs `const char*`. Byte-identical
            // cast — same regression in setAvatar above.
            data.assign(
                reinterpret_cast<const char*>(blob.data()),
                blob.size());
        }
        return {version, data};
    } catch (const std::exception& e) {
        std::cerr << "[DB Error] getAvatar: " << e.what() << "\n";
        return {std::string(), std::string()};
    }
}

int AuthManager::availableConnections() {
    try {
        auto lease = pool_.acquire();
        pqxx::connection& conn = *lease;
        pqxx::nontransaction txn(conn);
        const pqxx::result rs = txn.exec(
            "SELECT current_setting('max_connections')::int - current_setting('superuser_reserved_connections')::int");
        return rs[0][0].as<int>(0);
    } catch (const std::exception& e) {
        std::cerr << "[DB Error] availableConnections: " << e.what() << "\n";
        return 0;
    }
}

std::unordered_map<int64_t, std::string> AuthManager::getAvatarVersions(const std::vector<int64_t>& uids) {
    std::unordered_map<int64_t, std::string> versions;
    if (uids.empty()) return versions;
    // A bigint[] literal: the uids are numbers, so nothing needs quoting.
    std::string array = "{";
    for (size_t i = 0; i < uids.size(); ++i) {
        if (i) array += ',';
        array += std::to_string(uids[i]);
    }
    array += '}';
    try {
        auto lease = pool_.acquire();
        pqxx::connection& conn = *lease;
        pqxx::nontransaction txn(conn);
        pqxx::result rs = txn.exec_params(
            "SELECT uid, avatar_version FROM users "
            "WHERE uid = ANY($1::bigint[]) AND avatar_version <> ''",
            array);
        versions.reserve(rs.size());
        for (const auto& row : rs) {
            versions.emplace(row[0].as<int64_t>(), row[1].as<std::string>());
        }
    } catch (const std::exception& e) {
        std::cerr << "[DB Error] getAvatarVersions: " << e.what() << "\n";
        versions.clear();
    }
    return versions;
}

AuthManager::PresenceProfile AuthManager::getPresenceProfile(int64_t uid) {
    PresenceProfile profile;
    try {
        auto lease = pool_.acquire();
        pqxx::connection& conn = *lease;
        pqxx::work txn(conn);
        pqxx::result rs = txn.exec_params(
            "SELECT avatar_version, presence_status, dm_friends_only, "
            "status_text, status_emoji, status_expires_at "
            "FROM users WHERE uid = $1",
            uid);
        if (rs.empty()) {
            txn.commit();
            return profile;
        }
        profile.avatar_version = rs[0][0].as<std::string>("");
        const int status = rs[0][1].as<int>(0);
        if (chatproj::PresenceStatus_IsValid(status)) {
            profile.status = static_cast<chatproj::PresenceStatus>(status);
        }
        profile.dm_friends_only = rs[0][2].as<bool>(false);
        const int64_t expires_at = rs[0][5].as<int64_t>(0);
        const int64_t now = std::chrono::duration_cast<std::chrono::seconds>(
            std::chrono::system_clock::now().time_since_epoch()).count();
        if (expires_at > 0 && expires_at <= now) {
            // Ran out while the user was offline: clear it for good.
            txn.exec_params(
                "UPDATE users SET status_text = '', status_emoji = '', "
                "status_expires_at = 0 WHERE uid = $1",
                uid);
        } else {
            profile.status_text = rs[0][3].as<std::string>("");
            profile.status_emoji = rs[0][4].as<std::string>("");
            profile.status_expires_at = expires_at;
        }
        txn.commit();
    } catch (const std::exception& e) {
        std::cerr << "[DB Error] getPresenceProfile: " << e.what() << "\n";
    }
    return profile;
}

void AuthManager::setPresenceStatus(int64_t uid, int status) {
    try {
        auto lease = pool_.acquire();
        pqxx::connection& conn = *lease;
        pqxx::work txn(conn);
        txn.exec_params(
            "UPDATE users SET presence_status = $1 WHERE uid = $2",
            status, uid);
        txn.commit();
    } catch (const std::exception& e) {
        std::cerr << "[DB Error] setPresenceStatus: " << e.what() << "\n";
    }
}

void AuthManager::setStatusMessage(int64_t uid, const std::string& text,
                                   const std::string& emoji, int64_t expires_at) {
    try {
        auto lease = pool_.acquire();
        pqxx::connection& conn = *lease;
        pqxx::work txn(conn);
        txn.exec_params(
            "UPDATE users SET status_text = $1, status_emoji = $2, "
            "status_expires_at = $3 WHERE uid = $4",
            text, emoji, expires_at, uid);
        txn.commit();
    } catch (const std::exception& e) {
        std::cerr << "[DB Error] setStatusMessage: " << e.what() << "\n";
    }
}

void AuthManager::clearExpiredStatusMessage(int64_t uid, int64_t expires_at) {
    try {
        auto lease = pool_.acquire();
        pqxx::connection& conn = *lease;
        pqxx::work txn(conn);
        // Only the message that ran out: one the user set meanwhile stays.
        txn.exec_params(
            "UPDATE users SET status_text = '', status_emoji = '', status_expires_at = 0 "
            "WHERE uid = $1 AND status_expires_at = $2",
            uid, expires_at);
        txn.commit();
    } catch (const std::exception& e) {
        std::cerr << "[DB Error] clearExpiredStatusMessage: " << e.what() << "\n";
    }
}

void AuthManager::setDmFriendsOnly(int64_t uid, bool friends_only) {
    try {
        auto lease = pool_.acquire();
        pqxx::connection& conn = *lease;
        pqxx::work txn(conn);
        txn.exec_params(
            "UPDATE users SET dm_friends_only = $1 WHERE uid = $2",
            friends_only, uid);
        txn.commit();
    } catch (const std::exception& e) {
        std::cerr << "[DB Error] setDmFriendsOnly: " << e.what() << "\n";
    }
}

bool AuthManager::getDmFriendsOnly(int64_t uid) {
    try {
        auto lease = pool_.acquire();
        pqxx::connection& conn = *lease;
        pqxx::work txn(conn);
        pqxx::result rs = txn.exec_params(
            "SELECT dm_friends_only FROM users WHERE uid = $1",
            uid);
        txn.commit();
        if (rs.empty()) return false;
        return rs[0][0].as<bool>(false);
    } catch (const std::exception& e) {
        std::cerr << "[DB Error] getDmFriendsOnly: " << e.what() << "\n";
        return false;
    }
}

// ─── Persistent DMs ──────────────────────────────────────────────────────

/// A nullable BYTEA column as raw bytes ('' when NULL). Same
/// binarystring cast as getAvatar — `data()` is `const unsigned char*`
/// in pqxx 7+.
static std::string byteaOrEmpty(const pqxx::field& f) {
    if (f.is_null()) return std::string();
    pqxx::binarystring blob(f);
    return std::string(reinterpret_cast<const char*>(blob.data()), blob.size());
}

/// std::string → the BYTEA parameter type.
static pqxx::binarystring toBytea(const std::string& bytes) {
    return pqxx::binarystring(
        reinterpret_cast<const unsigned char*>(bytes.data()), bytes.size());
}

int64_t AuthManager::insertDm(int64_t sender_uid, const std::string& sender,
                               int64_t recipient_uid, const std::string& recipient,
                               const std::string& content,
                               int64_t sent_at,
                               int64_t reply_to,
                               const std::string& envelope) {
    try {
        auto lease = pool_.acquire();
        pqxx::connection& conn = *lease;
        pqxx::work txn(conn);
        // Two statements rather than a nullable parameter: an empty
        // envelope must land as NULL (plaintext row), and passing a
        // std::optional<binarystring> through exec_params isn't something
        // we can verify against the deployed pqxx from here.
        pqxx::result rs = envelope.empty()
            ? txn.exec_params(
                  "INSERT INTO dm_messages (sender_uid, sender, recipient_uid, recipient, "
                  "content, sent_at, reply_to) "
                  "VALUES ($1, $2, $3, $4, $5, $6, $7) RETURNING id",
                  sender_uid, sender, recipient_uid, recipient, content, sent_at,
                  reply_to < 0 ? 0 : reply_to)
            : txn.exec_params(
                  "INSERT INTO dm_messages (sender_uid, sender, recipient_uid, recipient, "
                  "content, sent_at, reply_to, envelope) "
                  "VALUES ($1, $2, $3, $4, $5, $6, $7, $8) RETURNING id",
                  sender_uid, sender, recipient_uid, recipient, content, sent_at,
                  reply_to < 0 ? 0 : reply_to, toBytea(envelope));
        txn.commit();
        if (rs.empty()) return 0;
        return rs[0][0].as<int64_t>();
    } catch (const std::exception& e) {
        std::cerr << "[DB Error] insertDm: " << e.what() << "\n";
        return 0;
    }
}

/// A stored DM body as read: at most `max_bytes`, ending in "…" when cut.
/// Queries read the column through leftSql, so a body stored before the
/// content cap isn't even fetched whole; this makes the cut exact.
static std::string dmText(const pqxx::field& f, size_t max_bytes) {
    return chatproj::utf8_preview(f.as<std::string>(), max_bytes);
}

/// LEFT(<column>, max_bytes + 1), for dmText. LEFT counts characters, so
/// what it keeps is never shorter than the cap in bytes, and the extra one
/// makes anything it cut longer than the cap: dmText then marks it.
static std::string leftSql(const char* column, size_t max_bytes) {
    return std::string("LEFT(") + column + ", " + std::to_string(max_bytes + 1) + ")";
}

// What every DM history read selects (dmRowFrom parses it): the row with
// its sender's current name and the names it was sent under, and the reply
// parent the same way — joined only within the same conversation, so a
// forged reply_to can't pull in another conversation's content. $1 / $2
// are the two users' uids. The body comes cut to MAX_DM_CONTENT and the
// parent's to a preview (review A1).
static const std::string DM_ROW_SELECT =
    "SELECT m.id, m.sender_uid, su.username, m.sender, m.recipient, " +
    leftSql("m.content", chatproj::MAX_DM_CONTENT) + ", "
    "m.sent_at, m.edited_at, m.reply_to, "
    "COALESCE(p.sender_uid, 0), COALESCE(pu.username, ''), COALESCE(p.sender, ''), "
    "COALESCE(p.recipient, ''), COALESCE(" + leftSql("p.content", chatproj::DM_PREVIEW_MAX_BYTES) + ", ''), "
    "m.envelope, p.envelope "
    "FROM dm_messages m "
    "JOIN users su ON su.uid = m.sender_uid "
    "LEFT JOIN dm_messages p ON p.id = m.reply_to "
    "  AND LEAST(p.sender_uid, p.recipient_uid) = LEAST($1::bigint, $2::bigint) "
    "  AND GREATEST(p.sender_uid, p.recipient_uid) = GREATEST($1::bigint, $2::bigint) "
    "LEFT JOIN users pu ON pu.uid = p.sender_uid "
    "WHERE LEAST(m.sender_uid, m.recipient_uid) = LEAST($1::bigint, $2::bigint) "
    "  AND GREATEST(m.sender_uid, m.recipient_uid) = GREATEST($1::bigint, $2::bigint) ";

static AuthManager::DmHistoryRow dmRowFrom(const pqxx::row& row) {
    AuthManager::DmHistoryRow r;
    r.id = row[0].as<int64_t>();
    r.sender_uid = row[1].as<int64_t>();
    r.sender = row[2].as<std::string>();
    r.sent_sender = row[3].as<std::string>();
    r.sent_recipient = row[4].as<std::string>();
    r.content = dmText(row[5], chatproj::MAX_DM_CONTENT);
    r.timestamp = row[6].as<int64_t>();
    r.edited_at = row[7].as<int64_t>();
    r.reply_to = row[8].as<int64_t>();
    r.reply_to_sender_uid = row[9].as<int64_t>();
    r.reply_to_sender = row[10].as<std::string>();
    r.reply_to_sent_sender = row[11].as<std::string>();
    r.reply_to_sent_recipient = row[12].as<std::string>();
    r.reply_to_content = dmText(row[13], chatproj::DM_PREVIEW_MAX_BYTES);
    r.envelope = byteaOrEmpty(row[14]);
    r.reply_to_envelope = byteaOrEmpty(row[15]);
    return r;
}

std::vector<AuthManager::DmHistoryRow> AuthManager::fetchDmHistory(
    int64_t user_a, int64_t user_b,
    int64_t before_id, int32_t limit, bool& has_more) {
    has_more = false;
    std::vector<DmHistoryRow> out;
    try {
        auto lease = pool_.acquire();
        pqxx::connection& conn = *lease;
        pqxx::work txn(conn);
        // Pull one extra row so we can detect has_more cheaply.
        const int32_t clamped = std::max(1, std::min(limit, 200));
        const int32_t fetch_n = clamped + 1;
        // before_id=0 means "latest". For real cursoring we filter on
        // id < before_id. Either way the pair index covers the predicate.
        pqxx::result rs = txn.exec_params(
            DM_ROW_SELECT + "AND ($3::bigint = 0 OR m.id < $3::bigint) ORDER BY m.id DESC LIMIT $4",
            user_a, user_b, before_id, fetch_n);
        txn.commit();

        out.reserve(rs.size());
        for (const auto& row : rs) {
            out.push_back(dmRowFrom(row));
        }
        if (static_cast<int32_t>(out.size()) > clamped) {
            out.pop_back();
            has_more = true;
        }
        return out;
    } catch (const std::exception& e) {
        std::cerr << "[DB Error] fetchDmHistory: " << e.what() << "\n";
        return {};
    }
}

std::vector<AuthManager::DmHistoryRow> AuthManager::fetchDmHistoryAround(
    int64_t user_a, int64_t user_b,
    int64_t around_id, int32_t limit,
    bool& has_more_before, bool& has_more_after) {
    has_more_before = false;
    has_more_after = false;
    std::vector<DmHistoryRow> out;
    try {
        auto lease = pool_.acquire();
        pqxx::connection& conn = *lease;
        pqxx::work txn(conn);
        const int32_t clamped = limit > 0 ? std::min(limit, 100) : 25;
        const int32_t fetch_n = clamped + 1;
        // Older side incl. target (id <= around), newest-first.
        pqxx::result older = txn.exec_params(
            DM_ROW_SELECT + "AND m.id <= $3::bigint ORDER BY m.id DESC LIMIT $4",
            user_a, user_b, around_id, fetch_n);
        // Newer side (id > around), oldest-first.
        pqxx::result newer = txn.exec_params(
            DM_ROW_SELECT + "AND m.id > $3::bigint ORDER BY m.id ASC LIMIT $4",
            user_a, user_b, around_id, fetch_n);
        txn.commit();

        std::vector<DmHistoryRow> older_vec;
        older_vec.reserve(older.size());
        for (const auto& row : older) {
            older_vec.push_back(dmRowFrom(row));
        }
        if (static_cast<int32_t>(older_vec.size()) > clamped) {
            older_vec.pop_back();
            has_more_before = true;
        }
        // older_vec is newest→oldest; reverse into oldest→newest.
        out.assign(older_vec.rbegin(), older_vec.rend());

        std::vector<DmHistoryRow> newer_vec;
        newer_vec.reserve(newer.size());
        for (const auto& row : newer) {
            newer_vec.push_back(dmRowFrom(row));
        }
        if (static_cast<int32_t>(newer_vec.size()) > clamped) {
            newer_vec.pop_back();
            has_more_after = true;
        }
        for (auto& r : newer_vec) out.push_back(std::move(r));
        return out;  // oldest→newest, target included
    } catch (const std::exception& e) {
        std::cerr << "[DB Error] fetchDmHistoryAround: " << e.what() << "\n";
        return {};
    }
}

std::vector<AuthManager::DmHistoryRow> AuthManager::fetchDmHistoryAfter(
    int64_t user_a, int64_t user_b,
    int64_t after_id, int32_t limit, bool& has_more_after) {
    has_more_after = false;
    std::vector<DmHistoryRow> out;
    try {
        auto lease = pool_.acquire();
        pqxx::connection& conn = *lease;
        pqxx::work txn(conn);
        const int32_t clamped = limit > 0 ? std::min(limit, 200) : 50;
        const int32_t fetch_n = clamped + 1;
        pqxx::result rs = txn.exec_params(
            DM_ROW_SELECT + "AND m.id > $3::bigint ORDER BY m.id ASC LIMIT $4",
            user_a, user_b, after_id, fetch_n);
        txn.commit();

        out.reserve(rs.size());
        for (const auto& row : rs) {
            out.push_back(dmRowFrom(row));
        }
        if (static_cast<int32_t>(out.size()) > clamped) {
            out.pop_back();
            has_more_after = true;
        }
        return out;  // oldest→newest
    } catch (const std::exception& e) {
        std::cerr << "[DB Error] fetchDmHistoryAfter: " << e.what() << "\n";
        return {};
    }
}

std::optional<AuthManager::DmPreviewRow> AuthManager::fetchDmPreview(
    int64_t user_a, int64_t user_b, int64_t message_id) {
    try {
        auto lease = pool_.acquire();
        pqxx::connection& conn = *lease;
        pqxx::work txn(conn);
        pqxx::result rs = txn.exec_params(
            "SELECT m.sender_uid, su.username, m.sender, m.recipient, " +
            leftSql("m.content", chatproj::DM_PREVIEW_MAX_BYTES) + ", m.envelope "
            "FROM dm_messages m JOIN users su ON su.uid = m.sender_uid "
            "WHERE m.id = $3 "
            "  AND LEAST(m.sender_uid, m.recipient_uid) = LEAST($1::bigint, $2::bigint) "
            "  AND GREATEST(m.sender_uid, m.recipient_uid) = GREATEST($1::bigint, $2::bigint)",
            user_a, user_b, message_id);
        txn.commit();
        if (rs.empty()) return std::nullopt;
        DmPreviewRow out;
        out.sender_uid = rs[0][0].as<int64_t>();
        out.sender = rs[0][1].as<std::string>();
        out.sent_sender = rs[0][2].as<std::string>();
        out.sent_recipient = rs[0][3].as<std::string>();
        out.content = dmText(rs[0][4], chatproj::DM_PREVIEW_MAX_BYTES);
        out.envelope = byteaOrEmpty(rs[0][5]);
        return out;
    } catch (const std::exception& e) {
        std::cerr << "[DB Error] fetchDmPreview: " << e.what() << "\n";
        return std::nullopt;
    }
}

std::vector<AuthManager::DmConversationPreviewRow>
AuthManager::fetchDmConversations(int64_t uid, int64_t before_id, bool& has_more) {
    has_more = false;
    std::vector<DmConversationPreviewRow> out;
    try {
        auto lease = pool_.acquire();
        pqxx::connection& conn = *lease;
        pqxx::work txn(conn);
        // One page of the user's conversations, newest first, below the
        // cursor, plus one to tell whether more follow: the user's rows of
        // dm_conversations (migration 5, kept by triggers; one per pair, the
        // user on either side), sorted. Joins back to
        // dm_messages for the preview, to users for the current names, and
        // to dm_read_cursors via a correlated subquery for the unread count:
        // messages from the peer with id > last_read_id (dm_messages_unread_idx).
        const std::string sql =
            "WITH latest AS ( "
            "  SELECT CASE WHEN lo_uid = $1 THEN hi_uid ELSE lo_uid END AS peer_uid, last_id "
            "  FROM dm_conversations "
            "  WHERE (lo_uid = $1 OR hi_uid = $1) AND ($2::bigint = 0 OR last_id < $2::bigint) "
            "  ORDER BY last_id DESC "
            "  LIMIT $3 "
            ") "
            "SELECT pu.uid, pu.username, " + leftSql("m.content", chatproj::DM_PREVIEW_MAX_BYTES) + ", "
            "  m.sender_uid, su.username, "
            "  m.sender, m.recipient, m.id, m.sent_at, "
            "  COALESCE(( "
            "    SELECT COUNT(*) FROM dm_messages d "
            "    WHERE d.recipient_uid = $1 "
            "      AND d.sender_uid = l.peer_uid "
            "      AND d.id > COALESCE(( "
            "        SELECT last_read_id FROM dm_read_cursors rc "
            "        WHERE rc.reader_uid = $1 AND rc.peer_uid = l.peer_uid "
            "      ), 0) "
            "  ), 0) AS unread, "
            "  m.envelope "
            "FROM latest l "
            "JOIN dm_messages m ON m.id = l.last_id "
            "JOIN users pu ON pu.uid = l.peer_uid "
            "JOIN users su ON su.uid = m.sender_uid "
            "ORDER BY l.last_id DESC";
        pqxx::result rs = txn.exec_params(sql, uid, before_id, DM_CONVERSATIONS_PAGE + 1);
        txn.commit();

        out.reserve(rs.size());
        for (const auto& row : rs) {
            if (static_cast<int>(out.size()) == DM_CONVERSATIONS_PAGE) {
                has_more = true;
                break;
            }
            DmConversationPreviewRow p;
            p.peer_uid = row[0].as<int64_t>();
            p.peer = row[1].as<std::string>();
            p.last_message_content = dmText(row[2], chatproj::DM_PREVIEW_MAX_BYTES);
            p.last_message_sender_uid = row[3].as<int64_t>();
            p.last_message_sender = row[4].as<std::string>();
            p.last_sent_sender = row[5].as<std::string>();
            p.last_sent_recipient = row[6].as<std::string>();
            p.last_message_id = row[7].as<int64_t>();
            p.last_timestamp = row[8].as<int64_t>();
            p.unread_count = row[9].as<int64_t>();
            p.last_message_envelope = byteaOrEmpty(row[10]);
            out.push_back(std::move(p));
        }
        return out;
    } catch (const std::exception& e) {
        std::cerr << "[DB Error] fetchDmConversations: " << e.what() << "\n";
        return {};
    }
}

void AuthManager::markDmRead(int64_t reader, int64_t peer, int64_t up_to_id) {
    try {
        auto lease = pool_.acquire();
        pqxx::connection& conn = *lease;
        pqxx::work txn(conn);
        // The peer must be an account (the caller resolved it; this keeps
        // a forged uid from tripping the foreign key into an error log).
        if (txn.exec_params("SELECT 1 FROM users WHERE uid = $1", peer).empty()) {
            return;
        }
        // Upsert with GREATEST so out-of-order or duplicate mark-read
        // calls never regress the read cursor.
        txn.exec_params(
            "INSERT INTO dm_read_cursors (reader_uid, peer_uid, last_read_id) "
            "VALUES ($1, $2, $3) "
            "ON CONFLICT (reader_uid, peer_uid) DO UPDATE "
            "SET last_read_id = GREATEST(dm_read_cursors.last_read_id, EXCLUDED.last_read_id)",
            reader, peer, up_to_id);
        txn.commit();
    } catch (const std::exception& e) {
        std::cerr << "[DB Error] markDmRead: " << e.what() << "\n";
    }
}

bool AuthManager::deleteDmMessage(int64_t sender, int64_t peer, int64_t message_id) {
    try {
        auto lease = pool_.acquire();
        pqxx::connection& conn = *lease;
        pqxx::work txn(conn);
        // Atomic auth: WHERE clause enforces sender-only + correct pair
        // + existence in one shot. affected_rows == 1 means all three
        // conditions held.
        pqxx::result rs = txn.exec_params(
            "DELETE FROM dm_messages "
            "WHERE id = $1 AND sender_uid = $2 AND recipient_uid = $3",
            message_id, sender, peer);
        txn.commit();
        return rs.affected_rows() == 1;
    } catch (const std::exception& e) {
        std::cerr << "[DB Error] deleteDmMessage: " << e.what() << "\n";
        return false;
    }
}

bool AuthManager::editDmMessage(int64_t sender, int64_t peer,
                                int64_t message_id,
                                const std::string& content,
                                int64_t edited_at,
                                const std::string& envelope) {
    try {
        auto lease = pool_.acquire();
        pqxx::connection& conn = *lease;
        pqxx::work txn(conn);
        // WHERE enforces sender-only + correct pair + existence atomically.
        // An edit always rewrites the envelope column: a plaintext edit of
        // a formerly-encrypted row (or vice versa) must not leave a stale
        // sealed body behind.
        pqxx::result rs = envelope.empty()
            ? txn.exec_params(
                  "UPDATE dm_messages SET content = $4, edited_at = $5, envelope = NULL "
                  "WHERE id = $1 AND sender_uid = $2 AND recipient_uid = $3",
                  message_id, sender, peer, content, edited_at)
            : txn.exec_params(
                  "UPDATE dm_messages SET content = $4, edited_at = $5, envelope = $6 "
                  "WHERE id = $1 AND sender_uid = $2 AND recipient_uid = $3",
                  message_id, sender, peer, content, edited_at, toBytea(envelope));
        txn.commit();
        return rs.affected_rows() == 1;
    } catch (const std::exception& e) {
        std::cerr << "[DB Error] editDmMessage: " << e.what() << "\n";
        return false;
    }
}

// ─── End-to-end encrypted DMs ────────────────────────────────────────────
// (see docs/superpowers/specs/2026-09-03-e2ee-dms-design.md)

uint32_t AuthManager::publishE2eeKeys(int64_t uid, const std::string& signed_name,
                                      const std::string& dh_pub,
                                      const std::string& sign_pub,
                                      const std::string& signature,
                                      int binding,
                                      int64_t created_at) {
    try {
        auto lease = pool_.acquire();
        pqxx::connection& conn = *lease;
        pqxx::work txn(conn);
        // MAX+1 under the account's row lock: two publishes of the same
        // account (a connection being replaced, on another thread) take
        // turns; the (uid, key_id) primary key backs it up.
        txn.exec_params("SELECT 1 FROM users WHERE uid = $1 FOR UPDATE", uid);
        // The uid is passed TWICE on purpose: Postgres types an untyped
        // parameter from its first use, and one $n used both as an
        // inserted value and in a comparison has failed before with
        // "inconsistent types deduced for parameter" (production,
        // 2026-09-03, with the username in this same statement).
        pqxx::result rs = txn.exec_params(
            "INSERT INTO e2ee_keys (uid, key_id, signed_name, dh_pub, sign_pub, signature, created_at, binding) "
            "VALUES ($1, "
            "  (SELECT COALESCE(MAX(key_id), 0) + 1 FROM e2ee_keys WHERE uid = $7), "
            "  $2, $3, $4, $5, $6, $8) "
            "RETURNING key_id",
            uid, signed_name, toBytea(dh_pub), toBytea(sign_pub), toBytea(signature), created_at,
            uid, binding);
        txn.commit();
        if (rs.empty()) return 0;
        return static_cast<uint32_t>(rs[0][0].as<int64_t>());
    } catch (const std::exception& e) {
        std::cerr << "[DB Error] publishE2eeKeys: " << e.what() << "\n";
        return 0;
    }
}

bool AuthManager::rebindE2eeKeys(int64_t uid, uint32_t key_id,
                                 const std::string& dh_pub,
                                 const std::string& sign_pub,
                                 const std::string& signature,
                                 int binding,
                                 const std::string& signed_name) {
    try {
        auto lease = pool_.acquire();
        pqxx::connection& conn = *lease;
        pqxx::work txn(conn);
        // Only the current row, and only with the same keys: a rebind
        // changes what the signature binds, never which keys are published.
        pqxx::result rs = txn.exec_params(
            "UPDATE e2ee_keys SET signature = $1, binding = $2, signed_name = $3 "
            "WHERE uid = $4 AND key_id = $5 AND dh_pub = $6 AND sign_pub = $7 "
            "  AND key_id = (SELECT MAX(key_id) FROM e2ee_keys WHERE uid = $8)",
            toBytea(signature), binding, signed_name, uid, static_cast<int64_t>(key_id),
            toBytea(dh_pub), toBytea(sign_pub), uid);
        txn.commit();
        return rs.affected_rows() == 1;
    } catch (const std::exception& e) {
        std::cerr << "[DB Error] rebindE2eeKeys: " << e.what() << "\n";
        return false;
    }
}

std::optional<AuthManager::E2eeKeyRow> AuthManager::getE2eeKeys(int64_t uid, uint32_t key_id) {
    try {
        auto lease = pool_.acquire();
        pqxx::connection& conn = *lease;
        pqxx::work txn(conn);
        pqxx::result rs = key_id == 0
            ? txn.exec_params(
                  "SELECT key_id, signed_name, dh_pub, sign_pub, signature, created_at, binding "
                  "FROM e2ee_keys WHERE uid = $1 "
                  "ORDER BY key_id DESC LIMIT 1",
                  uid)
            : txn.exec_params(
                  "SELECT key_id, signed_name, dh_pub, sign_pub, signature, created_at, binding "
                  "FROM e2ee_keys WHERE uid = $1 AND key_id = $2",
                  uid, static_cast<int64_t>(key_id));
        txn.commit();
        if (rs.empty()) return std::nullopt;
        E2eeKeyRow row;
        row.key_id = static_cast<uint32_t>(rs[0][0].as<int64_t>());
        row.signed_name = rs[0][1].as<std::string>();
        row.dh_pub = byteaOrEmpty(rs[0][2]);
        row.sign_pub = byteaOrEmpty(rs[0][3]);
        row.signature = byteaOrEmpty(rs[0][4]);
        row.created_at = rs[0][5].as<int64_t>();
        row.binding = rs[0][6].as<int>();
        return row;
    } catch (const std::exception& e) {
        std::cerr << "[DB Error] getE2eeKeys: " << e.what() << "\n";
        return std::nullopt;
    }
}

bool AuthManager::setE2eeBackup(int64_t uid, uint32_t key_id,
                                const std::string& blob, int64_t updated_at) {
    try {
        auto lease = pool_.acquire();
        pqxx::connection& conn = *lease;
        pqxx::work txn(conn);
        txn.exec_params(
            "INSERT INTO e2ee_backups (uid, key_id, blob, updated_at) "
            "VALUES ($1, $2, $3, $4) "
            "ON CONFLICT (uid) DO UPDATE "
            "SET key_id = EXCLUDED.key_id, blob = EXCLUDED.blob, updated_at = EXCLUDED.updated_at",
            uid, static_cast<int64_t>(key_id), toBytea(blob), updated_at);
        txn.commit();
        return true;
    } catch (const std::exception& e) {
        std::cerr << "[DB Error] setE2eeBackup: " << e.what() << "\n";
        return false;
    }
}

std::optional<std::pair<uint32_t, std::string>> AuthManager::getE2eeBackup(int64_t uid) {
    try {
        auto lease = pool_.acquire();
        pqxx::connection& conn = *lease;
        pqxx::work txn(conn);
        pqxx::result rs = txn.exec_params(
            "SELECT key_id, blob FROM e2ee_backups WHERE uid = $1",
            uid);
        txn.commit();
        if (rs.empty()) return std::nullopt;
        return std::make_pair(static_cast<uint32_t>(rs[0][0].as<int64_t>()),
                              byteaOrEmpty(rs[0][1]));
    } catch (const std::exception& e) {
        std::cerr << "[DB Error] getE2eeBackup: " << e.what() << "\n";
        return std::nullopt;
    }
}

// --- Auto-rejoin community memberships ---
// (see docs/superpowers/specs/2026-05-14-auto-rejoin-communities-design.md)

std::optional<chatproj::CommunityServerInfo> AuthManager::registerMembership(int64_t uid, int64_t server_id) {
    try {
        auto lease = pool_.acquire();
        pqxx::connection& conn = *lease;
        pqxx::work txn(conn);
        auto now = std::chrono::system_clock::now();
        int64_t now_ts = std::chrono::system_clock::to_time_t(now);
        // Only for an existing account (a forged uid would trip the
        // foreign key into an error log).
        if (txn.exec_params("SELECT 1 FROM users WHERE uid = $1", uid).empty()) {
            return std::nullopt;
        }
        const pqxx::result inserted = txn.exec_params(
            "INSERT INTO community_memberships (uid, server_id, joined_at) "
            "VALUES ($1, $2, $3) "
            "ON CONFLICT (uid, server_id) DO NOTHING",
            uid, server_id, now_ts);
        std::optional<chatproj::CommunityServerInfo> joined;
        if (inserted.affected_rows() == 1) {
            // The same columns and caps as getUserCommunities.
            const pqxx::result rs = txn.exec_params(
                "SELECT id, name, LEFT(description, $2), host_ip, port, member_count, "
                "       COALESCE(picture_version, ''), COALESCE(cert_fingerprint, '') "
                "FROM community_servers WHERE id = $1",
                server_id, static_cast<int>(chatproj::MAX_COMMUNITY_DESCRIPTION));
            if (!rs.empty()) {
                const auto& row = rs[0];
                chatproj::CommunityServerInfo info;
                info.set_id(row[0].as<int>());
                info.set_name(row[1].as<std::string>());
                info.set_description(communityDescription(row[2]));
                info.set_host_ip(row[3].as<std::string>());
                info.set_port(row[4].as<int>());
                info.set_member_count(row[5].as<int>());
                info.set_picture_version(row[6].as<std::string>());
                info.set_cert_fingerprint(row[7].as<std::string>());
                joined = std::move(info);
            }
        }
        txn.commit();
        return joined;
    } catch (const std::exception& e) {
        std::cerr << "[DB Error] registerMembership: " << e.what() << "\n";
        return std::nullopt;
    }
}

void AuthManager::revokeMembership(int64_t uid, int64_t server_id) {
    try {
        auto lease = pool_.acquire();
        pqxx::connection& conn = *lease;
        pqxx::work txn(conn);
        txn.exec_params(
            "DELETE FROM community_memberships "
            "WHERE uid = $1 AND server_id = $2",
            uid, server_id);
        txn.commit();
    } catch (const std::exception& e) {
        std::cerr << "[DB Error] revokeMembership: " << e.what() << "\n";
    }
}

std::vector<chatproj::CommunityServerInfo>
AuthManager::getUserCommunities(int64_t uid) {
    std::vector<chatproj::CommunityServerInfo> out;
    try {
        auto lease = pool_.acquire();
        pqxx::connection& conn = *lease;
        pqxx::work txn(conn);
        // Capped, so a LOGIN_RES always fits one frame (review A1).
        pqxx::result rs = txn.exec_params(
            "SELECT cs.id, cs.name, LEFT(cs.description, $2), cs.host_ip, "
            "       cs.port, cs.member_count, "
            "       COALESCE(cs.picture_version, ''), COALESCE(cs.cert_fingerprint, '') "
            "FROM community_memberships cm "
            "JOIN community_servers cs ON cs.id = cm.server_id "
            "WHERE cm.uid = $1 "
            "ORDER BY cm.joined_at, cm.server_id "
            "LIMIT $3",
            uid, static_cast<int>(chatproj::MAX_COMMUNITY_DESCRIPTION), MAX_LOGIN_MEMBERSHIPS);
        txn.commit();

        out.reserve(rs.size());
        for (const auto& row : rs) {
            chatproj::CommunityServerInfo info;
            info.set_id(row[0].as<int>());
            info.set_name(row[1].as<std::string>());
            info.set_description(communityDescription(row[2]));
            info.set_host_ip(row[3].as<std::string>());
            info.set_port(row[4].as<int>());
            info.set_member_count(row[5].as<int>());
            info.set_picture_version(row[6].as<std::string>());
            info.set_cert_fingerprint(row[7].as<std::string>());
            out.push_back(std::move(info));
        }
        return out;
    } catch (const std::exception& e) {
        std::cerr << "[DB Error] getUserCommunities: " << e.what() << "\n";
        return {};
    }
}

// --- Custom server pictures ---
// (see docs/superpowers/specs/2026-05-15-custom-server-pictures-design.md)

int AuthManager::setServerPicture(const std::string& host_ip, int port,
                                    const std::string& data,
                                    const std::string& version,
                                    int64_t known_id) {
    // Prefer the stable id (see upsertCommunityServer); fall back to the
    // address for communities that haven't learned their id yet.
    if (known_id > 0) {
        try {
            auto lease = pool_.acquire();
            pqxx::connection& conn = *lease;
            pqxx::work txn(conn);
            pqxx::result rs;
            if (data.empty()) {
                rs = txn.exec_params(
                    "UPDATE community_servers SET picture = NULL, picture_version = '' "
                    "WHERE id = $1 RETURNING id", known_id);
            } else {
                rs = txn.exec_params(
                    "UPDATE community_servers SET picture = $1, picture_version = $2 "
                    "WHERE id = $3 RETURNING id",
                    pqxx::binarystring(data.data(), data.size()), version, known_id);
            }
            txn.commit();
            if (!rs.empty()) return rs[0][0].as<int>();
        } catch (const std::exception& e) {
            std::cerr << "[DB Error] setServerPicture(id): " << e.what() << "\n";
        }
    }
    return setServerPicture(host_ip, port, data, version);
}

int AuthManager::setServerPicture(const std::string& host_ip, int port,
                                    const std::string& data,
                                    const std::string& version) {
    try {
        auto lease = pool_.acquire();
        pqxx::connection& conn = *lease;
        pqxx::work txn(conn);
        pqxx::result rs;
        if (data.empty()) {
            rs = txn.exec_params(
                "UPDATE community_servers "
                "SET picture = NULL, picture_version = '' "
                "WHERE host_ip = $1 AND port = $2 "
                "RETURNING id",
                host_ip, port);
        } else {
            rs = txn.exec_params(
                "UPDATE community_servers "
                "SET picture = $1, picture_version = $2 "
                "WHERE host_ip = $3 AND port = $4 "
                "RETURNING id",
                pqxx::binarystring(data.data(), data.size()),
                version, host_ip, port);
        }
        txn.commit();
        if (rs.empty()) return 0;
        return rs[0][0].as<int>();
    } catch (const std::exception& e) {
        std::cerr << "[DB Error] setServerPicture: " << e.what() << "\n";
        return 0;
    }
}

std::pair<std::string, std::string>
AuthManager::getServerPicture(int server_id) {
    try {
        auto lease = pool_.acquire();
        pqxx::connection& conn = *lease;
        pqxx::work txn(conn);
        // A picture over the cap (stored before central checked it) is
        // served as none: whole, it could pass the client's frame cap.
        pqxx::result rs = txn.exec_params(
            "SELECT COALESCE(picture_version, ''), picture "
            "FROM community_servers WHERE id = $1 "
            "  AND (picture IS NULL OR octet_length(picture) <= $2)",
            server_id, static_cast<int64_t>(MAX_SERVER_PICTURE));
        txn.commit();
        if (rs.empty()) return {"", ""};
        std::string version = rs[0][0].as<std::string>();
        std::string data;
        if (!rs[0][1].is_null()) {
            pqxx::binarystring blob(rs[0][1]);
            // blob.data() is `const unsigned char*` in pqxx 7+;
            // std::string::assign needs `const char*`. Byte-identical
            // cast — same pattern as getAvatar above.
            data.assign(
                reinterpret_cast<const char*>(blob.data()),
                blob.size());
        }
        return {std::move(version), std::move(data)};
    } catch (const std::exception& e) {
        std::cerr << "[DB Error] getServerPicture: " << e.what() << "\n";
        return {"", ""};
    }
}

std::vector<int64_t> AuthManager::getServerMembers(int server_id) {
    std::vector<int64_t> out;
    try {
        auto lease = pool_.acquire();
        pqxx::connection& conn = *lease;
        pqxx::work txn(conn);
        pqxx::result rs = txn.exec_params(
            "SELECT uid FROM community_memberships "
            "WHERE server_id = $1",
            server_id);
        txn.commit();
        out.reserve(rs.size());
        for (const auto& row : rs) {
            out.push_back(row[0].as<int64_t>());
        }
        return out;
    } catch (const std::exception& e) {
        std::cerr << "[DB Error] getServerMembers: " << e.what() << "\n";
        return {};
    }
}
