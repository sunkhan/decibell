#include "auth_manager.hpp"
#include "auth_utils.hpp" // Use your existing SHA256 utility
#include <chrono>
#include <iostream>
#include <algorithm>

void AuthManager::initializeDatabase() {
    try {
        pqxx::connection conn(db_conn_str_);
        pqxx::work txn(conn);
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
    return chatproj::sha256(plain_password);
}

bool AuthManager::verifyPassword(const std::string& plain_password, const std::string& hash) {
    return chatproj::sha256(plain_password) == hash;
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
        
        pqxx::result res = txn.exec_params(
            "SELECT user1, user2, status, action_user FROM friends WHERE user1 = $1 OR user2 = $1", 
            username
        );
        
        for (auto row : res) {
            std::string u1 = row[0].as<std::string>();
            std::string u2 = row[1].as<std::string>();
            std::string status = row[2].as<std::string>();
            std::string action_user = row[3].as<std::string>();
            
            std::string friend_name = (u1 == username) ? u2 : u1;
            
            chatproj::FriendInfo info;
            info.set_username(friend_name);
            
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