#pragma once
#include <memory>
#include <set>
#include <vector>
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

    /// Deliver `packet` to every online session whose username is in
    /// `usernames`. Used for SERVER_PICTURE_CHANGED broadcasts where
    /// the recipient set is the membership list of a specific community.
    void broadcast_to_users(const chatproj::Packet& packet,
                            const std::vector<std::string>& usernames);

    /// Online as others see it: some authenticated session of `username`
    /// that isn't invisible. Every presence-revealing path (friend lists,
    /// call invites) asks this, never "is there a session".
    bool is_user_visible(const std::string& username);

    bool check_dm_allowed(const std::string& sender, const std::string& recipient, AuthManager& auth_manager);

    /// User statuses (docs/superpowers/specs/2026-10-11-user-status-design.md).
    /// Every visible online user, to one session (at its login).
    void send_presence_snapshot(std::shared_ptr<Session> session);
    /// `username`'s presence changed in a way others can see: a
    /// PresenceDelta to delta-capable sessions, a full snapshot to older
    /// clients. Callers decide whether a change is visible.
    void broadcast_presence_change(const std::string& username);

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

    /// Push E2EE_KEYS_CHANGED to every session (same fan-out as
    /// AVATAR_CHANGED): clients drop their cached "current" bundle for
    /// that user and re-fetch on the next send.
    void broadcast_e2ee_keys_changed(const std::string& username, uint32_t key_id) {
        chatproj::Packet packet;
        packet.set_type(chatproj::Packet::E2EE_KEYS_CHANGED);
        auto* payload = packet.mutable_e2ee_keys_changed();
        payload->set_username(username);
        payload->set_key_id(key_id);
        broadcast(packet);
    }

    /// Evict `username`'s session(s) other than `except` (the new login).
    void kick_user(const std::string& username, std::shared_ptr<Session> except);

    void sweep_stale(std::chrono::seconds timeout);

    /// Custom status messages whose clear-after ran out: cleared, saved,
    /// and announced. Runs on the 30 s sweep timer.
    void expire_status_messages();

private:
    // Callers hold mutex_.
    bool user_visible_locked(const std::string& username) const;
    void fill_presence_snapshot_locked(chatproj::PresenceUpdate* presence) const;

    std::set<std::shared_ptr<Session>> sessions_;
    std::mutex mutex_;
};