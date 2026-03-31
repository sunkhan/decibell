#pragma once
#include <string>
#include <optional>
#include <vector>
#include <jwt-cpp/traits/nlohmann-json/defaults.h>
#include <pqxx/pqxx>
#include "messages.pb.h"

class AuthManager {
public:
    AuthManager(const std::string& secret_key, const std::string& db_conn_str) 
        : secret_key_(secret_key), db_conn_str_(db_conn_str) {
        initializeDatabase();
    }

    // Processes registration. Returns error message if failed, empty string if successful.
    std::string registerUser(const std::string& username, const std::string& email, const std::string& password);

    // Processes login. Returns the JWT if successful, or std::nullopt if failed.
    std::optional<std::string> authenticateUser(const std::string& username, const std::string& password);

    // Verifies an incoming JWT from a client or community server
    bool validateToken(const std::string& token);

    std::vector<chatproj::CommunityServerInfo> getCommunityServers();
    void upsertCommunityServer(const std::string& name, const std::string& description, const std::string& host_ip, int port, int member_count);

    // Friend System
    std::string handleFriendAction(const std::string& requester, chatproj::FriendActionType action, const std::string& target);
    std::vector<chatproj::FriendInfo> getFriends(const std::string& username);

private:
    std::string secret_key_;
    std::string db_conn_str_; // Add this member variable

    void initializeDatabase();

    // Hashing functions using Argon2 or bcrypt
    std::string hashPassword(const std::string& plain_password);
    bool verifyPassword(const std::string& plain_password, const std::string& hash);

    // Database interaction stubs (to be replaced with PostgreSQL pool)
    bool userExists(const std::string& username, const std::string& email);
    void insertUser(const std::string& username, const std::string& email, const std::string& hash);
    std::optional<std::string> getPasswordHash(const std::string& username);
};