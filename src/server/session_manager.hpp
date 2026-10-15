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

    /// To every signed-in session; pre-login and community-sync
    /// connections get nothing.
    void broadcast(const chatproj::Packet& packet);

    // Sessions are matched on the account uid (users by uid,
    // docs/superpowers/specs/2026-10-15-uid-identity-design.md); names
    // only label what is sent.

    /// Deliver to the user's authenticated session; false when offline.
    bool send_private(const chatproj::Packet& packet, int64_t target_uid);

    /// Deliver `packet` to every online session whose uid is in `uids`.
    /// Used for SERVER_PICTURE_CHANGED broadcasts where the recipient set
    /// is the membership list of a specific community.
    void broadcast_to_users(const chatproj::Packet& packet,
                            const std::vector<int64_t>& uids);

    /// Online as others see it: some authenticated session of `uid` that
    /// isn't invisible. Every presence-revealing path (friend lists, call
    /// invites) asks this, never "is there a session".
    bool is_user_visible(int64_t uid);

    bool check_dm_allowed(int64_t sender, int64_t recipient, AuthManager& auth_manager);

    /// User statuses (docs/superpowers/specs/2026-10-11-user-status-design.md).
    /// Every visible online user, to one session (at its login).
    void send_presence_snapshot(std::shared_ptr<Session> session);
    /// The user's presence changed in a way others can see: a
    /// PresenceDelta to delta-capable sessions, a full snapshot to older
    /// clients. Callers decide whether a change is visible. `username`
    /// labels the delta when no session of theirs is left.
    void broadcast_presence_change(int64_t uid, const std::string& username);

    /// Push AvatarChanged to every active authenticated session.
    /// Fires on every UPDATE_AVATAR_REQ that mutates a user's avatar
    /// (including removal — version is '' in that case). Delegates
    /// to broadcast() which handles serialization + framing + the
    /// session-iteration mutex.
    void broadcast_avatar_changed(int64_t uid, const std::string& username,
                                  const std::string& version) {
        chatproj::Packet packet;
        packet.set_type(chatproj::Packet::AVATAR_CHANGED);
        auto* payload = packet.mutable_avatar_changed();
        payload->set_uid(uid);
        payload->set_username(username);
        payload->set_version(version);
        broadcast(packet);
    }

    /// Push E2EE_KEYS_CHANGED to every session (same fan-out as
    /// AVATAR_CHANGED): clients drop their cached "current" bundle for
    /// that user and re-fetch on the next send.
    void broadcast_e2ee_keys_changed(int64_t uid, const std::string& username, uint32_t key_id) {
        chatproj::Packet packet;
        packet.set_type(chatproj::Packet::E2EE_KEYS_CHANGED);
        auto* payload = packet.mutable_e2ee_keys_changed();
        payload->set_uid(uid);
        payload->set_username(username);
        payload->set_key_id(key_id);
        broadcast(packet);
    }

    /// Push USER_RENAMED {uid, new name} to every signed-in session (uid
    /// design, stage 5). Clients hold people by uid, so the old name isn't
    /// sent: it would hand every signed-in user the link from it.
    void broadcast_user_renamed(int64_t uid, const std::string& username) {
        chatproj::Packet packet;
        packet.set_type(chatproj::Packet::USER_RENAMED);
        auto* payload = packet.mutable_user_renamed();
        payload->set_uid(uid);
        payload->set_username(username);
        broadcast(packet);
    }

    /// Evict the user's session(s) other than `except` (the new login).
    void kick_user(int64_t uid, std::shared_ptr<Session> except);

    void sweep_stale(std::chrono::seconds timeout);

    /// Custom status messages whose clear-after ran out: cleared, saved,
    /// and announced. Runs on the 30 s sweep timer.
    void expire_status_messages();

private:
    // Callers hold mutex_.
    bool user_visible_locked(int64_t uid) const;
    void fill_presence_snapshot_locked(chatproj::PresenceUpdate* presence) const;

    std::set<std::shared_ptr<Session>> sessions_;
    std::mutex mutex_;
};