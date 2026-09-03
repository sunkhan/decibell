#include "auth_manager.hpp"
#include "auth_utils.hpp"
#include "bcrypt.h"
#include <chrono>
#include <iostream>
#include <algorithm>

void AuthManager::initializeDatabase() {
    try {
        pqxx::connection conn(db_conn_str_);
        pqxx::work txn(conn);
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
        txn.exec(
            "CREATE TABLE IF NOT EXISTS community_invites ("
            "  code VARCHAR(32) PRIMARY KEY,"
            "  host VARCHAR(255) NOT NULL,"
            "  port INTEGER NOT NULL,"
            "  expires_at BIGINT NOT NULL,"  // 0 = never
            "  registered_at BIGINT NOT NULL"
            ")"
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
    }
}

std::string AuthManager::registerUser(const std::string& username, const std::string& email, const std::string& password) {
    if (username.length() < 3 || username.length() > 32) return "Invalid username length.";
    // Lowercase-only ASCII (2026-08-31). Case-mixed and non-ASCII names
    // were both a UX trap (exact-match friend lookups, "Usernames are
    // case-sensitive") and the trigger for the friends-pair CHECK
    // mismatch fixed above. Existing mixed-case accounts keep working —
    // this gate runs at registration only. `char` may be signed:
    // non-ASCII UTF-8 bytes land in the negative range and fail every
    // range test below, so multibyte names are rejected too.
    for (const char c : username) {
        const bool ok = (c >= 'a' && c <= 'z') || (c >= '0' && c <= '9') ||
                        c == '_' || c == '.' || c == '-';
        if (!ok) return "Usernames may only contain lowercase letters, digits, '.', '_' and '-'.";
    }
    if (email.empty() || email.find('@') == std::string::npos) return "Invalid email address.";
    // Reject empty / trivially-weak passwords at registration. Only affects
    // new accounts — existing users (any length) can still log in.
    if (password.length() < 8) return "Password must be at least 8 characters.";

    if (userExists(username, email)) {
        return "Username or email already exists.";
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

std::optional<std::string> AuthManager::authenticateUser(const std::string& username, const std::string& password) {
    auto stored_hash = getPasswordHash(username);

    if (!stored_hash) {
        // Equalize timing with the "user exists, wrong password" path so
        // login latency can't be used to enumerate valid usernames: run a
        // bcrypt hash at the same cost and discard it. Read the result
        // through a volatile so the optimizer can't elide the work.
        std::string sink = hashPassword(password);
        volatile char keep = sink.empty() ? 0 : sink[0];
        (void)keep;
        return std::nullopt;
    }
    if (!verifyPassword(password, *stored_hash)) {
        return std::nullopt;
    }

    // Generate a JWT valid for 24 hours. Ed25519 (EdDSA): only this
    // process holds the private key, so a community server — which gets
    // the public key — can verify tokens but never mint one.
    auto now = std::chrono::system_clock::now();
    try {
        auto token = jwt::create()
            .set_issuer("decibell_central_auth")
            .set_type("JWS")
            .set_subject(username)
            .set_payload_claim("uid", jwt::claim(nlohmann::json(getUserId(username))))
            .set_issued_at(now)
            .set_expires_at(now + std::chrono::hours(24))
            .sign(jwt::algorithm::ed25519{jwt_public_pem_, jwt_private_pem_});
        return token;
    } catch (const std::exception& e) {
        std::cerr << "[Auth] JWT signing failed: " << e.what() << "\n";
        return std::nullopt;
    }
}

bool AuthManager::validateToken(const std::string& token) {
    try {
        auto decoded = jwt::decode(token);
        auto verifier = jwt::verify()
            .allow_algorithm(jwt::algorithm::ed25519{jwt_public_pem_, ""})
            .with_issuer("decibell_central_auth");

        verifier.verify(decoded);
        return true;
    } catch (const std::exception& e) {
        return false;
    }
}

int64_t AuthManager::getUserId(const std::string& username) {
    try {
        pqxx::connection conn(db_conn_str_);
        pqxx::work txn(conn);
        pqxx::result res = txn.exec_params("SELECT uid FROM users WHERE username = $1", username);
        if (res.empty() || res[0][0].is_null()) return 0;
        return res[0][0].as<int64_t>();
    } catch (const std::exception& e) {
        std::cerr << "[DB Error] getUserId: " << e.what() << "\n";
        return 0;
    }
}

// --- Database & Hashing ---

bool AuthManager::userExists(const std::string& username, const std::string& email) {
    try {
        pqxx::connection conn(db_conn_str_);
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
        pqxx::connection conn(db_conn_str_);
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

std::optional<std::string> AuthManager::getPasswordHash(const std::string& username) {
    try {
        pqxx::connection conn(db_conn_str_);
        pqxx::work txn(conn);
        pqxx::result res = txn.exec_params("SELECT password_hash FROM users WHERE username = $1", username);
        
        if (res.empty()) {
            return std::nullopt;
        }
        return res[0][0].as<std::string>();
    } catch (const std::exception& e) {
        std::cerr << "[DB Error] getPasswordHash: " << e.what() << "\n";
        return std::nullopt;
    }
}

std::string AuthManager::hashPassword(const std::string& plain_password) {
    return bcrypt::hash(plain_password);
}

bool AuthManager::verifyPassword(const std::string& plain_password, const std::string& hash) {
    return bcrypt::verify(plain_password, hash);
}

std::vector<chatproj::CommunityServerInfo> AuthManager::getCommunityServers() {
    std::vector<chatproj::CommunityServerInfo> servers;
    try {
        pqxx::connection conn(db_conn_str_);
        pqxx::work txn(conn);
        
        // Fetch up to 50 LIVE public community servers sorted by member
        // count. Communities heartbeat every 60 s; a row without one for
        // 5 minutes is down (or gone for good) and used to sit in the
        // directory forever, since last_heartbeat was written but never
        // read. Memberships (getUserCommunities) are deliberately NOT
        // filtered — a member's server that's temporarily offline is
        // still their server.
        pqxx::result res = txn.exec(
            "SELECT id, name, description, host_ip, port, member_count, "
            "       COALESCE(picture_version, ''), COALESCE(cert_fingerprint, '') "
            "FROM community_servers "
            "WHERE is_public = TRUE "
            "  AND last_heartbeat > NOW() - INTERVAL '5 minutes' "
            "ORDER BY member_count DESC LIMIT 50"
        );

        for (auto row : res) {
            chatproj::CommunityServerInfo info;
            info.set_id(row[0].as<int>());
            info.set_name(row[1].as<std::string>());
            info.set_description(row[2].is_null() ? "" : row[2].as<std::string>());
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
        pqxx::connection conn(db_conn_str_);
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
        pqxx::connection conn(db_conn_str_);
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
        pqxx::connection conn(db_conn_str_);
        pqxx::work txn(conn);
        txn.exec_params("DELETE FROM community_invites WHERE code = $1", code);
        txn.commit();
    } catch (const std::exception& e) {
        std::cerr << "[DB Error] unregisterCommunityInvite: " << e.what() << "\n";
    }
}

std::optional<AuthManager::ResolvedInvite> AuthManager::resolveCommunityInvite(const std::string& code) {
    try {
        pqxx::connection conn(db_conn_str_);
        pqxx::work txn(conn);
        // LEFT JOIN, not a subselect: the invite must still resolve to
        // host:port when the community has no directory row yet. The
        // upsert keeps (host_ip, port) unique, so LIMIT 1 is belt and
        // braces.
        pqxx::result res = txn.exec_params(
            "SELECT i.host, i.port, i.expires_at, "
            "       COALESCE(cs.cert_fingerprint, ''), "
            "       COALESCE(cs.id, 0), COALESCE(cs.name, ''), COALESCE(cs.description, ''), "
            "       COALESCE(cs.member_count, 0), COALESCE(cs.picture_version, '') "
            "FROM community_invites i "
            "LEFT JOIN community_servers cs ON cs.host_ip = i.host AND cs.port = i.port "
            "WHERE i.code = $1 LIMIT 1",
            code
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
        out.description = res[0][6].as<std::string>();
        out.member_count = res[0][7].as<int>();
        out.picture_version = res[0][8].as<std::string>();
        return out;
    } catch (const std::exception& e) {
        std::cerr << "[DB Error] resolveCommunityInvite: " << e.what() << "\n";
        return std::nullopt;
    }
}

// --- Friend System ---

std::string AuthManager::handleFriendAction(const std::string& requester, chatproj::FriendActionType action, const std::string& target) {
    if (requester == target) return "Cannot perform action on yourself.";
    try {
        pqxx::connection conn(db_conn_str_);
        pqxx::work txn(conn);

        // Check if target exists
        pqxx::result res = txn.exec_params("SELECT 1 FROM users WHERE username = $1", target);
        if (res.empty()) return "User not found.";

        std::string u1 = (std::min)(requester, target);
        std::string u2 = (std::max)(requester, target);

        if (action == chatproj::FriendActionType::ADD) {
            pqxx::result rel = txn.exec_params("SELECT status FROM friends WHERE user1 = $1 AND user2 = $2", u1, u2);
            if (!rel.empty()) {
                std::string status = rel[0][0].as<std::string>();
                if (status == "BLOCKED") return "Cannot add user.";
                if (status == "ACCEPTED") return "Already friends.";
                return "Friend request already exists.";
            }
            txn.exec_params(
                "INSERT INTO friends (user1, user2, status, action_user) VALUES ($1, $2, 'PENDING', $3)",
                u1, u2, requester
            );
        } else if (action == chatproj::FriendActionType::ACCEPT) {
            pqxx::result rel = txn.exec_params("SELECT status, action_user FROM friends WHERE user1 = $1 AND user2 = $2", u1, u2);
            if (rel.empty() || rel[0][0].as<std::string>() != "PENDING" || rel[0][1].as<std::string>() == requester) {
                return "No pending friend request to accept.";
            }
            txn.exec_params("UPDATE friends SET status = 'ACCEPTED' WHERE user1 = $1 AND user2 = $2", u1, u2);
        } else if (action == chatproj::FriendActionType::REJECT || action == chatproj::FriendActionType::REMOVE) {
            txn.exec_params("DELETE FROM friends WHERE user1 = $1 AND user2 = $2", u1, u2);
        } else if (action == chatproj::FriendActionType::BLOCK) {
            txn.exec_params(
                "INSERT INTO friends (user1, user2, status, action_user) VALUES ($1, $2, 'BLOCKED', $3) "
                "ON CONFLICT (user1, user2) DO UPDATE SET status = 'BLOCKED', action_user = $3",
                u1, u2, requester
            );
        }

        txn.commit();
        return "";
    } catch (const std::exception& e) {
        std::cerr << "[DB Error] handleFriendAction: " << e.what() << "\n";
        return "Database error.";
    }
}

bool AuthManager::isBlocked(const std::string& a, const std::string& b) {
    try {
        pqxx::connection conn(db_conn_str_);
        pqxx::work txn(conn);
        const std::string u1 = (std::min)(a, b);
        const std::string u2 = (std::max)(a, b);
        pqxx::result res = txn.exec_params(
            "SELECT 1 FROM friends WHERE user1 = $1 AND user2 = $2 AND status = 'BLOCKED'",
            u1, u2
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

std::vector<chatproj::FriendInfo> AuthManager::getFriends(const std::string& username) {
    std::vector<chatproj::FriendInfo> friends;
    try {
        pqxx::connection conn(db_conn_str_);
        pqxx::work txn(conn);
        
        // JOIN users on the friend (the row's other-side username) so
        // each FriendInfo carries that user's avatar_version. Clients
        // use it to invalidate their per-user avatar cache without
        // an extra fetch per friend.
        pqxx::result res = txn.exec_params(
            "SELECT f.user1, f.user2, f.status, f.action_user, u.avatar_version "
            "FROM friends f "
            "JOIN users u ON u.username = "
            "  CASE WHEN f.user1 = $1 THEN f.user2 ELSE f.user1 END "
            "WHERE f.user1 = $1 OR f.user2 = $1",
            username
        );

        for (auto row : res) {
            std::string u1 = row[0].as<std::string>();
            std::string u2 = row[1].as<std::string>();
            std::string status = row[2].as<std::string>();
            std::string action_user = row[3].as<std::string>();
            std::string avatar_version = row[4].as<std::string>("");

            std::string friend_name = (u1 == username) ? u2 : u1;

            chatproj::FriendInfo info;
            info.set_username(friend_name);
            info.set_avatar_version(avatar_version);

            if (status == "ACCEPTED") {
                info.set_status(chatproj::FriendInfo::OFFLINE); // Default
            } else if (status == "PENDING") {
                if (action_user == username) {
                    info.set_status(chatproj::FriendInfo::PENDING_OUTGOING);
                } else {
                    info.set_status(chatproj::FriendInfo::PENDING_INCOMING);
                }
            } else if (status == "BLOCKED") {
                if (action_user == username) {
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

std::string AuthManager::setAvatar(const std::string& username,
                                    const std::string& data) {
    pqxx::connection conn(db_conn_str_);
    pqxx::work txn(conn);
    if (data.empty()) {
        // Remove: NULL the bytes, clear the version.
        txn.exec_params(
            "UPDATE users SET avatar = NULL, avatar_version = '' "
            "WHERE username = $1",
            username);
        txn.commit();
        return std::string();
    }
    const std::string version = chatproj::sha256(data);
    // pqxx::binarystring expects `const unsigned char*` in pqxx 7+;
    // std::string::data() is `const char*`. The cast is byte-identical.
    txn.exec_params(
        "UPDATE users SET avatar = $1, avatar_version = $2 "
        "WHERE username = $3",
        pqxx::binarystring(
            reinterpret_cast<const unsigned char*>(data.data()),
            data.size()),
        version, username);
    txn.commit();
    return version;
}

std::pair<std::string, std::string> AuthManager::getAvatar(
    const std::string& username) {
    try {
        pqxx::connection conn(db_conn_str_);
        pqxx::work txn(conn);
        pqxx::result rs = txn.exec_params(
            "SELECT avatar, avatar_version FROM users WHERE username = $1",
            username);
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

std::string AuthManager::getAvatarVersion(const std::string& username) {
    try {
        pqxx::connection conn(db_conn_str_);
        pqxx::work txn(conn);
        pqxx::result rs = txn.exec_params(
            "SELECT avatar_version FROM users WHERE username = $1",
            username);
        txn.commit();
        if (rs.empty()) return std::string();
        return rs[0][0].as<std::string>("");
    } catch (const std::exception& e) {
        std::cerr << "[DB Error] getAvatarVersion: " << e.what() << "\n";
        return std::string();
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

int64_t AuthManager::insertDm(const std::string& sender,
                               const std::string& recipient,
                               const std::string& content,
                               int64_t sent_at,
                               int64_t reply_to,
                               const std::string& envelope) {
    try {
        pqxx::connection conn(db_conn_str_);
        pqxx::work txn(conn);
        // Two statements rather than a nullable parameter: an empty
        // envelope must land as NULL (plaintext row), and passing a
        // std::optional<binarystring> through exec_params isn't something
        // we can verify against the deployed pqxx from here.
        pqxx::result rs = envelope.empty()
            ? txn.exec_params(
                  "INSERT INTO dm_messages (sender, recipient, content, sent_at, reply_to) "
                  "VALUES ($1, $2, $3, $4, $5) RETURNING id",
                  sender, recipient, content, sent_at, reply_to < 0 ? 0 : reply_to)
            : txn.exec_params(
                  "INSERT INTO dm_messages (sender, recipient, content, sent_at, reply_to, envelope) "
                  "VALUES ($1, $2, $3, $4, $5, $6) RETURNING id",
                  sender, recipient, content, sent_at, reply_to < 0 ? 0 : reply_to,
                  toBytea(envelope));
        txn.commit();
        if (rs.empty()) return 0;
        return rs[0][0].as<int64_t>();
    } catch (const std::exception& e) {
        std::cerr << "[DB Error] insertDm: " << e.what() << "\n";
        return 0;
    }
}

std::vector<AuthManager::DmHistoryRow> AuthManager::fetchDmHistory(
    const std::string& user_a, const std::string& user_b,
    int64_t before_id, int32_t limit, bool& has_more) {
    has_more = false;
    std::vector<DmHistoryRow> out;
    try {
        pqxx::connection conn(db_conn_str_);
        pqxx::work txn(conn);
        // Pull one extra row so we can detect has_more cheaply.
        const int32_t clamped = std::max(1, std::min(limit, 200));
        const int32_t fetch_n = clamped + 1;
        // before_id=0 means "latest". For real cursoring we filter on
        // id < before_id. Either way the pair_idx covers the predicate.
        const char* sql =
            "SELECT m.id, m.sender, m.content, m.sent_at, m.edited_at, m.reply_to, "
            "COALESCE(p.sender, ''), COALESCE(p.content, ''), m.envelope, p.envelope "
            "FROM dm_messages m LEFT JOIN dm_messages p ON p.id = m.reply_to "
            "  AND LEAST(p.sender, p.recipient) = LEAST($1, $2) "
            "  AND GREATEST(p.sender, p.recipient) = GREATEST($1, $2) "
            "WHERE LEAST(m.sender, m.recipient) = LEAST($1, $2) "
            "  AND GREATEST(m.sender, m.recipient) = GREATEST($1, $2) "
            "  AND ($3 = 0 OR m.id < $3) "
            "ORDER BY m.id DESC LIMIT $4";
        pqxx::result rs = txn.exec_params(sql, user_a, user_b, before_id, fetch_n);
        txn.commit();

        out.reserve(rs.size());
        for (const auto& row : rs) {
            DmHistoryRow r{
                row[0].as<int64_t>(),
                row[1].as<std::string>(),
                row[2].as<std::string>(),
                row[3].as<int64_t>(),
                row[4].as<int64_t>(),
                row[5].as<int64_t>(),
                row[6].as<std::string>(),
                row[7].as<std::string>(),
                byteaOrEmpty(row[8]),
                byteaOrEmpty(row[9]),
            };
            out.push_back(std::move(r));
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
    const std::string& user_a, const std::string& user_b,
    int64_t around_id, int32_t limit,
    bool& has_more_before, bool& has_more_after) {
    has_more_before = false;
    has_more_after = false;
    std::vector<DmHistoryRow> out;
    try {
        pqxx::connection conn(db_conn_str_);
        pqxx::work txn(conn);
        const int32_t clamped = limit > 0 ? std::min(limit, 100) : 25;
        const int32_t fetch_n = clamped + 1;
        // Older side incl. target (id <= around), newest-first.
        const char* sql_older =
            "SELECT m.id, m.sender, m.content, m.sent_at, m.edited_at, m.reply_to, "
            "COALESCE(p.sender, ''), COALESCE(p.content, ''), m.envelope, p.envelope "
            "FROM dm_messages m LEFT JOIN dm_messages p ON p.id = m.reply_to "
            "  AND LEAST(p.sender, p.recipient) = LEAST($1, $2) "
            "  AND GREATEST(p.sender, p.recipient) = GREATEST($1, $2) "
            "WHERE LEAST(m.sender, m.recipient) = LEAST($1, $2) "
            "  AND GREATEST(m.sender, m.recipient) = GREATEST($1, $2) "
            "  AND m.id <= $3 "
            "ORDER BY m.id DESC LIMIT $4";
        pqxx::result older = txn.exec_params(sql_older, user_a, user_b, around_id, fetch_n);
        // Newer side (id > around), oldest-first.
        const char* sql_newer =
            "SELECT m.id, m.sender, m.content, m.sent_at, m.edited_at, m.reply_to, "
            "COALESCE(p.sender, ''), COALESCE(p.content, ''), m.envelope, p.envelope "
            "FROM dm_messages m LEFT JOIN dm_messages p ON p.id = m.reply_to "
            "  AND LEAST(p.sender, p.recipient) = LEAST($1, $2) "
            "  AND GREATEST(p.sender, p.recipient) = GREATEST($1, $2) "
            "WHERE LEAST(m.sender, m.recipient) = LEAST($1, $2) "
            "  AND GREATEST(m.sender, m.recipient) = GREATEST($1, $2) "
            "  AND m.id > $3 "
            "ORDER BY m.id ASC LIMIT $4";
        pqxx::result newer = txn.exec_params(sql_newer, user_a, user_b, around_id, fetch_n);
        txn.commit();

        std::vector<DmHistoryRow> older_vec;
        older_vec.reserve(older.size());
        for (const auto& row : older) {
            older_vec.push_back(DmHistoryRow{
                row[0].as<int64_t>(), row[1].as<std::string>(),
                row[2].as<std::string>(), row[3].as<int64_t>(),
                row[4].as<int64_t>(), row[5].as<int64_t>(),
                row[6].as<std::string>(), row[7].as<std::string>(),
                byteaOrEmpty(row[8]), byteaOrEmpty(row[9]),
            });
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
            newer_vec.push_back(DmHistoryRow{
                row[0].as<int64_t>(), row[1].as<std::string>(),
                row[2].as<std::string>(), row[3].as<int64_t>(),
                row[4].as<int64_t>(), row[5].as<int64_t>(),
                row[6].as<std::string>(), row[7].as<std::string>(),
                byteaOrEmpty(row[8]), byteaOrEmpty(row[9]),
            });
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
    const std::string& user_a, const std::string& user_b,
    int64_t after_id, int32_t limit, bool& has_more_after) {
    has_more_after = false;
    std::vector<DmHistoryRow> out;
    try {
        pqxx::connection conn(db_conn_str_);
        pqxx::work txn(conn);
        const int32_t clamped = limit > 0 ? std::min(limit, 200) : 50;
        const int32_t fetch_n = clamped + 1;
        const char* sql =
            "SELECT m.id, m.sender, m.content, m.sent_at, m.edited_at, m.reply_to, "
            "COALESCE(p.sender, ''), COALESCE(p.content, ''), m.envelope, p.envelope "
            "FROM dm_messages m LEFT JOIN dm_messages p ON p.id = m.reply_to "
            "  AND LEAST(p.sender, p.recipient) = LEAST($1, $2) "
            "  AND GREATEST(p.sender, p.recipient) = GREATEST($1, $2) "
            "WHERE LEAST(m.sender, m.recipient) = LEAST($1, $2) "
            "  AND GREATEST(m.sender, m.recipient) = GREATEST($1, $2) "
            "  AND m.id > $3 "
            "ORDER BY m.id ASC LIMIT $4";
        pqxx::result rs = txn.exec_params(sql, user_a, user_b, after_id, fetch_n);
        txn.commit();

        out.reserve(rs.size());
        for (const auto& row : rs) {
            out.push_back(DmHistoryRow{
                row[0].as<int64_t>(), row[1].as<std::string>(),
                row[2].as<std::string>(), row[3].as<int64_t>(),
                row[4].as<int64_t>(), row[5].as<int64_t>(),
                row[6].as<std::string>(), row[7].as<std::string>(),
                byteaOrEmpty(row[8]), byteaOrEmpty(row[9]),
            });
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
    const std::string& user_a, const std::string& user_b, int64_t message_id) {
    try {
        pqxx::connection conn(db_conn_str_);
        pqxx::work txn(conn);
        pqxx::result rs = txn.exec_params(
            "SELECT sender, content, envelope FROM dm_messages "
            "WHERE id = $3 "
            "  AND LEAST(sender, recipient) = LEAST($1, $2) "
            "  AND GREATEST(sender, recipient) = GREATEST($1, $2)",
            user_a, user_b, message_id);
        txn.commit();
        if (rs.empty()) return std::nullopt;
        DmPreviewRow out;
        out.sender = rs[0][0].as<std::string>();
        out.content = rs[0][1].as<std::string>();
        out.envelope = byteaOrEmpty(rs[0][2]);
        return out;
    } catch (const std::exception& e) {
        std::cerr << "[DB Error] fetchDmPreview: " << e.what() << "\n";
        return std::nullopt;
    }
}

std::vector<AuthManager::DmConversationPreviewRow>
AuthManager::fetchDmConversations(const std::string& user) {
    std::vector<DmConversationPreviewRow> out;
    try {
        pqxx::connection conn(db_conn_str_);
        pqxx::work txn(conn);
        // Picks the latest message id per conversation (pair grouped
        // by LEAST/GREATEST), then joins back to dm_messages for the
        // preview content, and to dm_read_state via a correlated
        // subquery to derive unread count for messages from peer
        // with id > last_read_id (i.e. messages the local user
        // received, not their own outgoing).
        const char* sql =
            "WITH latest AS ( "
            "  SELECT LEAST(sender, recipient) AS a, "
            "         GREATEST(sender, recipient) AS b, "
            "         MAX(id) AS max_id "
            "  FROM dm_messages "
            "  WHERE sender = $1 OR recipient = $1 "
            "  GROUP BY 1, 2 "
            ") "
            "SELECT "
            "  CASE WHEN m.sender = $1 THEN m.recipient ELSE m.sender END AS peer, "
            "  m.content, m.sender, m.id, m.sent_at, "
            "  COALESCE(( "
            "    SELECT COUNT(*) FROM dm_messages d "
            "    WHERE d.recipient = $1 "
            "      AND d.sender = CASE WHEN m.sender = $1 THEN m.recipient ELSE m.sender END "
            "      AND d.id > COALESCE(( "
            "        SELECT last_read_id FROM dm_read_state rs "
            "        WHERE rs.reader = $1 AND rs.peer = "
            "          CASE WHEN m.sender = $1 THEN m.recipient ELSE m.sender END "
            "      ), 0) "
            "  ), 0) AS unread, "
            "  m.envelope "
            "FROM latest l "
            "JOIN dm_messages m ON m.id = l.max_id "
            "ORDER BY m.id DESC";
        pqxx::result rs = txn.exec_params(sql, user);
        txn.commit();

        out.reserve(rs.size());
        for (const auto& row : rs) {
            DmConversationPreviewRow p{
                row[0].as<std::string>(),       // peer
                row[1].as<std::string>(),       // last_message_content
                row[2].as<std::string>(),       // last_message_sender
                row[3].as<int64_t>(),           // last_message_id
                row[4].as<int64_t>(),           // last_timestamp
                row[5].as<int64_t>(),           // unread_count
                byteaOrEmpty(row[6]),           // last_message_envelope
            };
            out.push_back(std::move(p));
        }
        return out;
    } catch (const std::exception& e) {
        std::cerr << "[DB Error] fetchDmConversations: " << e.what() << "\n";
        return {};
    }
}

void AuthManager::markDmRead(const std::string& reader,
                              const std::string& peer,
                              int64_t up_to_id) {
    try {
        pqxx::connection conn(db_conn_str_);
        pqxx::work txn(conn);
        // Upsert with GREATEST so out-of-order or duplicate mark-read
        // calls never regress the read cursor.
        txn.exec_params(
            "INSERT INTO dm_read_state (reader, peer, last_read_id) "
            "VALUES ($1, $2, $3) "
            "ON CONFLICT (reader, peer) DO UPDATE "
            "SET last_read_id = GREATEST(dm_read_state.last_read_id, EXCLUDED.last_read_id)",
            reader, peer, up_to_id);
        txn.commit();
    } catch (const std::exception& e) {
        std::cerr << "[DB Error] markDmRead: " << e.what() << "\n";
    }
}

bool AuthManager::deleteDmMessage(const std::string& sender,
                                    const std::string& peer,
                                    int64_t message_id) {
    try {
        pqxx::connection conn(db_conn_str_);
        pqxx::work txn(conn);
        // Atomic auth: WHERE clause enforces sender-only + correct pair
        // + existence in one shot. affected_rows == 1 means all three
        // conditions held.
        pqxx::result rs = txn.exec_params(
            "DELETE FROM dm_messages "
            "WHERE id = $1 AND sender = $2 AND recipient = $3",
            message_id, sender, peer);
        txn.commit();
        return rs.affected_rows() == 1;
    } catch (const std::exception& e) {
        std::cerr << "[DB Error] deleteDmMessage: " << e.what() << "\n";
        return false;
    }
}

bool AuthManager::editDmMessage(const std::string& sender,
                                const std::string& peer,
                                int64_t message_id,
                                const std::string& content,
                                int64_t edited_at,
                                const std::string& envelope) {
    try {
        pqxx::connection conn(db_conn_str_);
        pqxx::work txn(conn);
        // WHERE enforces sender-only + correct pair + existence atomically.
        // An edit always rewrites the envelope column: a plaintext edit of
        // a formerly-encrypted row (or vice versa) must not leave a stale
        // sealed body behind.
        pqxx::result rs = envelope.empty()
            ? txn.exec_params(
                  "UPDATE dm_messages SET content = $4, edited_at = $5, envelope = NULL "
                  "WHERE id = $1 AND sender = $2 AND recipient = $3",
                  message_id, sender, peer, content, edited_at)
            : txn.exec_params(
                  "UPDATE dm_messages SET content = $4, edited_at = $5, envelope = $6 "
                  "WHERE id = $1 AND sender = $2 AND recipient = $3",
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

uint32_t AuthManager::publishE2eeKeys(const std::string& username,
                                      const std::string& dh_pub,
                                      const std::string& sign_pub,
                                      const std::string& signature,
                                      int64_t created_at) {
    try {
        pqxx::connection conn(db_conn_str_);
        pqxx::work txn(conn);
        // MAX+1 inside one transaction; central runs a single io thread
        // so two publishes from the same user can't interleave anyway.
        // The username is passed TWICE on purpose. Postgres types an
        // untyped parameter from its first use, and the two uses here
        // disagree: as an inserted value it takes the column's varchar,
        // while `username = $n` resolves through the text equality
        // operator and takes text — one $1 in both places fails with
        // "inconsistent types deduced for parameter $1: text versus
        // character varying" (production, 2026-09-03, with both the
        // INSERT … SELECT and the VALUES + subquery shapes).
        pqxx::result rs = txn.exec_params(
            "INSERT INTO user_e2ee_keys (username, key_id, dh_pub, sign_pub, signature, created_at) "
            "VALUES ($1, "
            "  (SELECT COALESCE(MAX(key_id), 0) + 1 FROM user_e2ee_keys WHERE username = $6), "
            "  $2, $3, $4, $5) "
            "RETURNING key_id",
            username, toBytea(dh_pub), toBytea(sign_pub), toBytea(signature), created_at,
            username);
        txn.commit();
        if (rs.empty()) return 0;
        return static_cast<uint32_t>(rs[0][0].as<int64_t>());
    } catch (const std::exception& e) {
        std::cerr << "[DB Error] publishE2eeKeys: " << e.what() << "\n";
        return 0;
    }
}

std::optional<AuthManager::E2eeKeyRow> AuthManager::getE2eeKeys(
    const std::string& username, uint32_t key_id) {
    try {
        pqxx::connection conn(db_conn_str_);
        pqxx::work txn(conn);
        pqxx::result rs = key_id == 0
            ? txn.exec_params(
                  "SELECT key_id, dh_pub, sign_pub, signature, created_at "
                  "FROM user_e2ee_keys WHERE username = $1 "
                  "ORDER BY key_id DESC LIMIT 1",
                  username)
            : txn.exec_params(
                  "SELECT key_id, dh_pub, sign_pub, signature, created_at "
                  "FROM user_e2ee_keys WHERE username = $1 AND key_id = $2",
                  username, static_cast<int64_t>(key_id));
        txn.commit();
        if (rs.empty()) return std::nullopt;
        E2eeKeyRow row;
        row.key_id = static_cast<uint32_t>(rs[0][0].as<int64_t>());
        row.dh_pub = byteaOrEmpty(rs[0][1]);
        row.sign_pub = byteaOrEmpty(rs[0][2]);
        row.signature = byteaOrEmpty(rs[0][3]);
        row.created_at = rs[0][4].as<int64_t>();
        return row;
    } catch (const std::exception& e) {
        std::cerr << "[DB Error] getE2eeKeys: " << e.what() << "\n";
        return std::nullopt;
    }
}

bool AuthManager::setE2eeBackup(const std::string& username, uint32_t key_id,
                                const std::string& blob, int64_t updated_at) {
    try {
        pqxx::connection conn(db_conn_str_);
        pqxx::work txn(conn);
        txn.exec_params(
            "INSERT INTO user_e2ee_backup (username, key_id, blob, updated_at) "
            "VALUES ($1, $2, $3, $4) "
            "ON CONFLICT (username) DO UPDATE "
            "SET key_id = EXCLUDED.key_id, blob = EXCLUDED.blob, updated_at = EXCLUDED.updated_at",
            username, static_cast<int64_t>(key_id), toBytea(blob), updated_at);
        txn.commit();
        return true;
    } catch (const std::exception& e) {
        std::cerr << "[DB Error] setE2eeBackup: " << e.what() << "\n";
        return false;
    }
}

std::optional<std::pair<uint32_t, std::string>> AuthManager::getE2eeBackup(
    const std::string& username) {
    try {
        pqxx::connection conn(db_conn_str_);
        pqxx::work txn(conn);
        pqxx::result rs = txn.exec_params(
            "SELECT key_id, blob FROM user_e2ee_backup WHERE username = $1",
            username);
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

void AuthManager::registerMembership(const std::string& username,
                                       int64_t server_id) {
    try {
        pqxx::connection conn(db_conn_str_);
        pqxx::work txn(conn);
        auto now = std::chrono::system_clock::now();
        int64_t now_ts = std::chrono::system_clock::to_time_t(now);
        txn.exec_params(
            "INSERT INTO user_communities (username, server_id, joined_at) "
            "VALUES ($1, $2, $3) "
            "ON CONFLICT (username, server_id) DO NOTHING",
            username, server_id, now_ts);
        txn.commit();
    } catch (const std::exception& e) {
        std::cerr << "[DB Error] registerMembership: " << e.what() << "\n";
    }
}

void AuthManager::revokeMembership(const std::string& username,
                                     int64_t server_id) {
    try {
        pqxx::connection conn(db_conn_str_);
        pqxx::work txn(conn);
        txn.exec_params(
            "DELETE FROM user_communities "
            "WHERE username = $1 AND server_id = $2",
            username, server_id);
        txn.commit();
    } catch (const std::exception& e) {
        std::cerr << "[DB Error] revokeMembership: " << e.what() << "\n";
    }
}

std::vector<chatproj::CommunityServerInfo>
AuthManager::getUserCommunities(const std::string& username) {
    std::vector<chatproj::CommunityServerInfo> out;
    try {
        pqxx::connection conn(db_conn_str_);
        pqxx::work txn(conn);
        pqxx::result rs = txn.exec_params(
            "SELECT cs.id, cs.name, cs.description, cs.host_ip, "
            "       cs.port, cs.member_count, "
            "       COALESCE(cs.picture_version, ''), COALESCE(cs.cert_fingerprint, '') "
            "FROM user_communities uc "
            "JOIN community_servers cs ON cs.id = uc.server_id "
            "WHERE uc.username = $1 "
            "ORDER BY uc.joined_at",
            username);
        txn.commit();

        out.reserve(rs.size());
        for (const auto& row : rs) {
            chatproj::CommunityServerInfo info;
            info.set_id(row[0].as<int>());
            info.set_name(row[1].as<std::string>());
            info.set_description(row[2].is_null() ? "" : row[2].as<std::string>());
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
            pqxx::connection conn(db_conn_str_);
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
        pqxx::connection conn(db_conn_str_);
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
        pqxx::connection conn(db_conn_str_);
        pqxx::work txn(conn);
        pqxx::result rs = txn.exec_params(
            "SELECT COALESCE(picture_version, ''), picture "
            "FROM community_servers WHERE id = $1",
            server_id);
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

std::vector<std::string> AuthManager::getServerMembers(int server_id) {
    std::vector<std::string> out;
    try {
        pqxx::connection conn(db_conn_str_);
        pqxx::work txn(conn);
        pqxx::result rs = txn.exec_params(
            "SELECT username FROM user_communities "
            "WHERE server_id = $1",
            server_id);
        txn.commit();
        out.reserve(rs.size());
        for (const auto& row : rs) {
            out.push_back(row[0].as<std::string>());
        }
        return out;
    } catch (const std::exception& e) {
        std::cerr << "[DB Error] getServerMembers: " << e.what() << "\n";
        return {};
    }
}