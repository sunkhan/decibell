#pragma once
#include <chrono>
#include <cstdint>
#include <functional>
#include <memory>
#include <mutex>
#include <string>
#include <unordered_map>
#include <unordered_set>
#include <vector>
#include "messages.pb.h"

class Session;
class AuthManager;

/// One serialized, framed packet, shared by every recipient.
using Frame = std::shared_ptr<const std::vector<uint8_t>>;
Frame make_frame(const chatproj::Packet& packet);

/// What others can see of a signed-in account, and what fan-out needs to
/// know about it. Lives in the directory, never read off a Session.
struct Presence {
    std::string username;
    std::string avatar_version;  // '' when no avatar
    chatproj::PresenceStatus status = chatproj::PRESENCE_ONLINE;  // the chosen one
    bool idle = false;           // auto-away (session-only)
    std::string status_text;
    std::string status_emoji;
    int64_t status_expires_at = 0;  // unix seconds; 0 = never (or no message)
    bool dm_friends_only = false;

    bool visible() const { return status != chatproj::PRESENCE_INVISIBLE; }
    /// What others see: the chosen status, AWAY while an ONLINE user idles.
    chatproj::PresenceStatus effective() const {
        return status == chatproj::PRESENCE_ONLINE && idle ? chatproj::PRESENCE_AWAY : status;
    }
};

/// The connection directory (docs/superpowers/specs/2026-10-15-central-
/// scaling-design.md): every connection, the signed-in accounts with their
/// presence, and who watches whom. All state sessions share lives here,
/// behind one mutex; a session never reads another session's fields.
/// Delivery collects its targets under the lock and posts to them after
/// releasing it (Session::deliver is thread-safe and only posts, so it
/// may also be called under the lock: a subscription answer is, to keep
/// its order against later pushes). Every method may be called from any
/// thread.
///
/// `deliver_to_user` is the one way to reach an account. In a multi-node
/// deployment it is where a message bus plugs in; nothing else needs to know.
class SessionManager {
public:
    /// Most accounts one connection may watch; a longer set is cut.
    static constexpr size_t MAX_WATCH = 5000;

    // --- Connections ---
    void join(const std::shared_ptr<Session>& session);
    /// Forget the connection and close it (posted to its strand). Its
    /// account, if this was the account's connection, goes offline for its
    /// watchers. Idempotent.
    void leave(const std::shared_ptr<Session>& session);

    struct Bound {
        bool ok = false;  // false: the connection left meanwhile
        std::shared_ptr<Session> replaced;  // the account's previous connection, to close
    };
    /// Sign `session` in as `uid`. A login replaces the account's previous
    /// connection (one per account); the caller closes `replaced`. Watchers
    /// see the login when it is visible.
    Bound bind(const std::shared_ptr<Session>& session, int64_t uid, Presence presence);

    // --- Delivery ---
    /// False when the account has no connection here.
    bool deliver_to_user(int64_t uid, const Frame& frame);
    bool deliver_to_user(int64_t uid, const chatproj::Packet& packet);
    void deliver_to_users(const std::vector<int64_t>& uids, const chatproj::Packet& packet);
    void deliver_to_watchers(int64_t uid, const chatproj::Packet& packet);

    // --- Presence ---
    /// Online as others see it: signed in and not invisible. Every
    /// presence-revealing path (friend lists, call invites) asks this.
    bool is_user_visible(int64_t uid);
    /// Apply `change` to the signed-in account's presence; when what others
    /// see changes, its watchers get a PresenceDelta. False when the account
    /// isn't signed in here.
    bool update_presence(int64_t uid, const std::function<void(Presence&)>& change);
    /// Replace the set of accounts `session` (signed in as `uid`) shows;
    /// the newly watched are answered with a PRESENCE_UPDATE: presence for
    /// the visible, the avatar version for the rest (read from the
    /// database outside the lock for accounts not signed in). Cut at
    /// MAX_WATCH; non-positive uids are dropped.
    void watch(const std::shared_ptr<Session>& session, int64_t uid, const std::vector<int64_t>& uids,
               AuthManager& auth_manager);

    // --- Profile pushes (to the account's watchers) ---
    void push_user_renamed(int64_t uid, const std::string& username);
    void push_avatar_changed(int64_t uid, const std::string& version);
    void push_e2ee_keys_changed(int64_t uid, uint32_t key_id);

    bool check_dm_allowed(int64_t sender, int64_t recipient, AuthManager& auth_manager);

    // --- Housekeeping (the sweep timer) ---
    /// Close connections idle for longer than `timeout`.
    void sweep_stale(std::chrono::seconds timeout);
    /// Clear custom status messages whose time ran out: saved, and
    /// announced to watchers when they were visible.
    void expire_status_messages(AuthManager& auth_manager);

private:
    struct Account {
        std::shared_ptr<Session> session;
        Presence presence;
        std::unordered_set<int64_t> watching;
    };

    // Callers hold mutex_.
    std::vector<std::shared_ptr<Session>> watcher_sessions_locked(int64_t uid) const;
    void unwatch_all_locked(int64_t uid, Account& account);
    static void fill_presence(chatproj::UserPresence* entry, int64_t uid, const Presence& p);
    /// A PresenceDelta for `uid`: online with `p` when given, else offline.
    static chatproj::Packet presence_delta(int64_t uid, const std::string& username, const Presence* p);

    /// Every connection → the account it signed in as (0 before login).
    std::unordered_map<std::shared_ptr<Session>, int64_t> sessions_;
    std::unordered_map<int64_t, Account> accounts_;
    /// uid → the accounts watching it (the reverse of Account::watching).
    std::unordered_map<int64_t, std::unordered_set<int64_t>> watchers_;
    std::mutex mutex_;
};
