#pragma once
#include <memory>
#include <set>
#include <mutex>
#include <iostream>
#include <chrono>
#include "messages.pb.h"

class Session;
class AuthManager;

class SessionManager {
public:
    SessionManager() = default;

    void join(std::shared_ptr<Session> session) {
        std::lock_guard<std::mutex> lock(mutex_);
        sessions_.insert(session);
        std::cout << "[Manager] Session joined. Total: " << sessions_.size() << "\n";
    }

    void leave(std::shared_ptr<Session> session);

    void broadcast_system_message(const std::string& text) {
        auto now = std::chrono::system_clock::now();
        int64_t current_time = std::chrono::system_clock::to_time_t(now);

        chatproj::Packet packet;
        packet.set_type(chatproj::Packet::DIRECT_MSG);
        auto* msg = packet.mutable_direct_msg();
        msg->set_sender("SYSTEM");
        msg->set_content(text);
        msg->set_timestamp(current_time);
        
        broadcast(packet);
    }

    void broadcast(const chatproj::Packet& packet);

    bool send_private(const chatproj::Packet& packet, const std::string& target_username);

    bool is_user_online(const std::string& username);

    bool check_dm_allowed(const std::string& sender, const std::string& recipient, AuthManager& auth_manager);

    void broadcast_presence();

    /// Push AvatarChanged to every active authenticated session.
    /// Fires on every UPDATE_AVATAR_REQ that mutates a user's avatar
    /// (including removal — version is '' in that case). Delegates
    /// to broadcast() which handles serialization + framing + the
    /// session-iteration mutex.
    void broadcast_avatar_changed(const std::string& username,
                                  const std::string& version) {
        chatproj::Packet packet;
        packet.set_type(chatproj::Packet::AVATAR_CHANGED);
        auto* payload = packet.mutable_avatar_changed();
        payload->set_username(username);
        payload->set_version(version);
        broadcast(packet);
    }

    void kick_user(const std::string& username);

    void sweep_stale(std::chrono::seconds timeout);

private:
    std::set<std::shared_ptr<Session>> sessions_;
    std::mutex mutex_;
};