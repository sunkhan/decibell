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
        txn.exec(
            "CREATE TABLE IF NOT EXISTS friends ("
            "  user1 VARCHAR(32) NOT NULL,"
            "  user2 VARCHAR(32) NOT NULL,"
            "  status VARCHAR(16) NOT NULL,"
            "  action_user VARCHAR(32) NOT NULL,"
            "  PRIMARY KEY (user1, user2),"
            "  CHECK (user1 < user2)"
            ")"
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
            "  sent_at BIGINT NOT NULL"
            ")"
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
        txn.exec(
            "CREATE TABLE IF NOT EXISTS dm_read_state ("
            "  reader VARCHAR(32) NOT NULL,"
            "  peer VARCHAR(32) NOT NULL,"
            "  last_read_id BIGINT NOT NULL DEFAULT 0,"
            "  PRIMARY KEY (reader, peer)"
            ")"
        );

        txn.commit();
    } catch (const std::exception& e) {
        std::cerr << "[DB Error] initializeDatabase: " << e.what() << "\n";
    }
}

std::string AuthManager::registerUser(const std::string& username, const std::string& email, const std::string& password) {
    if (username.length() < 3 || username.length() > 32) return "Invalid username length.";
    if (email.empty() || email.find('@') == std::string::npos) return "Invalid email address.";
    
    if (userExists(username, email)) {
        return "Username or email already exists.";
    }

    std::string hash = hashPassword(password);
    insertUser(username, email, hash);
    return "";
}

std::optional<std::string> AuthManager::authenticateUser(const std::string& username, const std::string& password) {
    auto stored_hash = getPasswordHash(username);
    
    if (!stored_hash || !verifyPassword(password, *stored_hash)) {
        return std::nullopt;
    }

    // Generate a JWT valid for 24 hours
    auto now = std::chrono::system_clock::now();
    auto token = jwt::create()
        .set_issuer("decibell_central_auth")
        .set_type("JWS")
        .set_subject(username)
        .set_issued_at(now)
        .set_expires_at(now + std::chrono::hours(24))
        .sign(jwt::algorithm::hs256{secret_key_});

    return token;
}

bool AuthManager::validateToken(const std::string& token) {
    try {
        auto decoded = jwt::decode(token);
        auto verifier = jwt::verify()
            .allow_algorithm(jwt::algorithm::hs256{secret_key_})
            .with_issuer("decibell_central_auth");
            
        verifier.verify(decoded);
        return true;
    } catch (const std::exception& e) {
        return false;
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

void AuthManager::insertUser(const std::string& username, const std::string& email, const std::string& hash) {
    try {
        pqxx::connection conn(db_conn_str_);
        pqxx::work txn(conn);
        txn.exec_params(
            "INSERT INTO users (username, email, password_hash) VALUES ($1, $2, $3)", 
            username, email, hash
        );
        txn.commit();
    } catch (const std::exception& e) {
        std::cerr << "[DB Error] insertUser: " << e.what() << "\n";
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
        
        // Fetch up to 50 public community servers sorted by member count
        pqxx::result res = txn.exec(
            "SELECT id, name, description, host_ip, port, member_count "
            "FROM community_servers ORDER BY member_count DESC LIMIT 50"
        );
        
        for (auto row : res) {
            chatproj::CommunityServerInfo info;
            info.set_id(row[0].as<int>());
            info.set_name(row[1].as<std::string>());
            info.set_description(row[2].is_null() ? "" : row[2].as<std::string>());
            info.set_host_ip(row[3].as<std::string>());
            info.set_port(row[4].as<int>());
            info.set_member_count(row[5].as<int>());
            servers.push_back(info);
        }
    } catch (const std::exception& e) {
        std::cerr << "[DB Error] getCommunityServers: " << e.what() << "\n";
    }
    return servers;
}

void AuthManager::upsertCommunityServer(const std::string& name, const std::string& description, const std::string& host_ip, int port, int member_count) {
    try {
        pqxx::connection conn(db_conn_str_);
        pqxx::work txn(conn);
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
        txn.exec_params(
            "INSERT INTO community_servers (name, description, host_ip, port, member_count, last_heartbeat) "
            "VALUES ($1, $2, $3, $4, $5, NOW()) "
            "ON CONFLICT (host_ip, port) DO UPDATE SET "
            "name = EXCLUDED.name, description = EXCLUDED.description, "
            "member_count = EXCLUDED.member_count, last_heartbeat = NOW()",
            name, description, host_ip, port, member_count
        );
        txn.commit();
    } catch (const std::exception& e) {
        std::cerr << "[DB Error] upsertCommunityServer: " << e.what() << "\n";
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

std::optional<std::pair<std::string, int>> AuthManager::resolveCommunityInvite(const std::string& code) {
    try {
        pqxx::connection conn(db_conn_str_);
        pqxx::work txn(conn);
        pqxx::result res = txn.exec_params(
            "SELECT host, port, expires_at FROM community_invites WHERE code = $1",
            code
        );
        if (res.empty()) return std::nullopt;

        const int64_t expires_at = res[0][2].as<int64_t>();
        if (expires_at != 0) {
            const int64_t now = std::chrono::duration_cast<std::chrono::seconds>(
                                    std::chrono::system_clock::now().time_since_epoch())
                                    .count();
            if (now >= expires_at) {
                // Lazily prune expired entries so the table doesn't grow forever.
                try {
                    pqxx::work cleanup(conn);
                    cleanup.exec_params("DELETE FROM community_invites WHERE code = $1", code);
                    cleanup.commit();
                } catch (...) {}
                return std::nullopt;
            }
        }

        return std::make_pair(res[0][0].as<std::string>(), res[0][1].as<int>());
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

int64_t AuthManager::insertDm(const std::string& sender,
                               const std::string& recipient,
                               const std::string& content,
                               int64_t sent_at) {
    try {
        pqxx::connection conn(db_conn_str_);
        pqxx::work txn(conn);
        pqxx::result rs = txn.exec_params(
            "INSERT INTO dm_messages (sender, recipient, content, sent_at) "
            "VALUES ($1, $2, $3, $4) RETURNING id",
            sender, recipient, content, sent_at);
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
            "SELECT id, sender, content, sent_at FROM dm_messages "
            "WHERE LEAST(sender, recipient) = LEAST($1, $2) "
            "  AND GREATEST(sender, recipient) = GREATEST($1, $2) "
            "  AND ($3 = 0 OR id < $3) "
            "ORDER BY id DESC LIMIT $4";
        pqxx::result rs = txn.exec_params(sql, user_a, user_b, before_id, fetch_n);
        txn.commit();

        out.reserve(rs.size());
        for (const auto& row : rs) {
            DmHistoryRow r{
                row[0].as<int64_t>(),
                row[1].as<std::string>(),
                row[2].as<std::string>(),
                row[3].as<int64_t>(),
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
            "  ), 0) AS unread "
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