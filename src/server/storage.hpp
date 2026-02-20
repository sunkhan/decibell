#pragma once
#include <string>
#include <iostream>
#include <sqlite3.h>
#include <vector>
#include <optional>

namespace chatproj {

struct User {
    int id;
    std::string username;
    std::string password_hash;
    std::string salt;
};

struct StoredMessage {
    std::string sender;
    std::string content;
    int64_t timestamp;
    std::string channel;
};

class Storage {
public:
    Storage(const std::string& db_path) {
        if (sqlite3_open(db_path.c_str(), &db_ptr_) != SQLITE_OK) {
            std::cerr << "Can't open database: " << sqlite3_errmsg(db_ptr_) << "\n";
            db_ptr_ = nullptr;
        } else {
            init_schema();
        }
    }

    ~Storage() {
        if (db_ptr_) sqlite3_close(db_ptr_);
    }

    // Create the Users table if it doesn't exist
    void init_schema() {
        const char* sql = 
            "CREATE TABLE IF NOT EXISTS users ("
            "id INTEGER PRIMARY KEY AUTOINCREMENT, "
            "username TEXT UNIQUE NOT NULL, "
            "password_hash TEXT NOT NULL, "
            "salt TEXT NOT NULL);"
            "CREATE TABLE IF NOT EXISTS messages ("
            "id INTEGER PRIMARY KEY AUTOINCREMENT, "
            "sender TEXT NOT NULL, "
            "channel TEXT NOT NULL, " // Added channel column
            "content TEXT NOT NULL, "
            "timestamp INTEGER NOT NULL);";

        char* err_msg = 0;
        if (sqlite3_exec(db_ptr_, sql, 0, 0, &err_msg) != SQLITE_OK) {
            std::cerr << "SQL Error: " << err_msg << "\n";
            sqlite3_free(err_msg);
        } else {
            std::cout << "[Storage] Database schema initialized.\n";
        }
    }

    bool save_message(const std::string& sender, const std::string& channel, const std::string& content, int64_t timestamp) {
        std::string sql = "INSERT INTO messages (sender, channel, content, timestamp) VALUES (?, ?, ?, ?);";
        sqlite3_stmt* stmt;
        
        if (sqlite3_prepare_v2(db_ptr_, sql.c_str(), -1, &stmt, 0) != SQLITE_OK) return false;
        
        sqlite3_bind_text(stmt, 1, sender.c_str(), -1, SQLITE_STATIC);
        sqlite3_bind_text(stmt, 2, channel.c_str(), -1, SQLITE_STATIC);
        sqlite3_bind_text(stmt, 3, content.c_str(), -1, SQLITE_STATIC);
        sqlite3_bind_int64(stmt, 4, timestamp);

        bool success = (sqlite3_step(stmt) == SQLITE_DONE);
        sqlite3_finalize(stmt);
        return success;
    }

    std::vector<StoredMessage> get_recent_messages(const std::string& channel, int limit = 50) {
        std::vector<StoredMessage> msgs;
        std::string sql = "SELECT sender, content, timestamp FROM (SELECT id, sender, content, timestamp FROM messages WHERE channel = ? ORDER BY id DESC LIMIT ?) ORDER BY id ASC;";
        sqlite3_stmt* stmt;
        
        if (sqlite3_prepare_v2(db_ptr_, sql.c_str(), -1, &stmt, 0) == SQLITE_OK) {
            sqlite3_bind_text(stmt, 1, channel.c_str(), -1, SQLITE_STATIC);
            sqlite3_bind_int(stmt, 2, limit);
            while (sqlite3_step(stmt) == SQLITE_ROW) {
                StoredMessage m;
                m.sender = reinterpret_cast<const char*>(sqlite3_column_text(stmt, 0));
                m.content = reinterpret_cast<const char*>(sqlite3_column_text(stmt, 1));
                m.timestamp = sqlite3_column_int64(stmt, 2);
                m.channel = channel;
                msgs.push_back(m);
            }
        }
        sqlite3_finalize(stmt);
        return msgs;
    }

    bool create_user(const std::string& username, const std::string& hash, const std::string& salt) {
        std::string sql = "INSERT INTO users (username, password_hash, salt) VALUES (?, ?, ?);";
        sqlite3_stmt* stmt;
        
        sqlite3_prepare_v2(db_ptr_, sql.c_str(), -1, &stmt, 0);
        sqlite3_bind_text(stmt, 1, username.c_str(), -1, SQLITE_STATIC);
        sqlite3_bind_text(stmt, 2, hash.c_str(), -1, SQLITE_STATIC);
        sqlite3_bind_text(stmt, 3, salt.c_str(), -1, SQLITE_STATIC);

        int rc = sqlite3_step(stmt);
        sqlite3_finalize(stmt);
        
        if (rc != SQLITE_DONE) {
            std::cerr << "[Storage] Failed to create user (might already exist).\n";
            return false;
        }
        return true;
    }

    // Check if username exists and password hash matches
    bool verify_user(const std::string& username, const std::string& input_hash) {
        std::string sql = "SELECT password_hash FROM users WHERE username = ?;";
        sqlite3_stmt* stmt;
        
        if (sqlite3_prepare_v2(db_ptr_, sql.c_str(), -1, &stmt, 0) != SQLITE_OK) {
            return false;
        }

        sqlite3_bind_text(stmt, 1, username.c_str(), -1, SQLITE_STATIC);

        bool valid = false;
        if (sqlite3_step(stmt) == SQLITE_ROW) {
            // Fetch the stored hash from the database
            const char* stored_hash = reinterpret_cast<const char*>(sqlite3_column_text(stmt, 0));
            
            // Compare with the hash provided by the user
            if (stored_hash && input_hash == std::string(stored_hash)) {
                valid = true;
            }
        }
        
        sqlite3_finalize(stmt);
        return valid;
    }

    std::optional<User> get_user(const std::string& username) {
        std::string sql = "SELECT id, username, password_hash, salt FROM users WHERE username = ?;";
        sqlite3_stmt* stmt;
        sqlite3_prepare_v2(db_ptr_, sql.c_str(), -1, &stmt, 0);
        sqlite3_bind_text(stmt, 1, username.c_str(), -1, SQLITE_STATIC);

        if (sqlite3_step(stmt) == SQLITE_ROW) {
            User user;
            user.id = sqlite3_column_int(stmt, 0);
            user.username = reinterpret_cast<const char*>(sqlite3_column_text(stmt, 1));
            user.password_hash = reinterpret_cast<const char*>(sqlite3_column_text(stmt, 2));
            user.salt = reinterpret_cast<const char*>(sqlite3_column_text(stmt, 3));
            sqlite3_finalize(stmt);
            return user;
        }
        
        sqlite3_finalize(stmt);
        return std::nullopt; // User not found
    }

private:
    sqlite3* db_ptr_ = nullptr;
};

}