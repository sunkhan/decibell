#pragma once
#include <atomic>
#include <chrono>
#include <cstdint>
#include <functional>
#include <memory>
#include <mutex>
#include <string>
#include <unordered_map>
#include <unordered_set>
#include <utility>
#include <vector>
#include "messages.pb.h"

class Session;
class AuthManager;

/// One serialized, framed packet, shared by every recipient.
using Frame = std::shared_ptr<const std::vector<uint8_t>>;
Frame make_frame(const chatproj::Packet& packet);

/// What others can see of a signed-in account, and what fan-out needs to
/// know about it. Lives in the directory, never read off a Session. One per
/// account, whatever number of devices it is signed in on.
struct Presence {
    std::string username;
    std::string avatar_version;  // '' when no avatar
    chatproj::PresenceStatus status = chatproj::PRESENCE_ONLINE;  // the chosen one
    bool idle = false;           // auto-away: every device of the account is idle
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
/// scaling-design.md, 2026-10-18-multi-device-design.md): every connection,
/// the signed-in accounts with their devices and presence, who watches
/// whom, and the calls being set up. All state sessions share lives here,
/// behind one mutex; a session never reads another session's fields.
/// Delivery collects its targets under the lock and posts to them after
/// releasing it (Session::deliver is thread-safe and only posts, so it
/// may also be called under the lock: a subscription answer is, to keep
/// its order against later pushes). Every method may be called from any
/// thread.
///
/// An account may be signed in on up to MAX_DEVICES connections at once. It
/// is online while any of them is, its presence is the account's, and
/// `deliver_to_user` reaches all of them. That is the one way to reach an
/// account; in a multi-node deployment it is where a message bus plugs in.
class SessionManager {
public:
    /// Most accounts one connection may watch; a longer set is cut.
    static constexpr size_t MAX_WATCH = 5000;
    /// Most connections one account may have signed in at once. A
    /// community keeps at most 8 sessions per account and evicts the oldest
    /// past that, so a ninth device would knock the others out there.
    static constexpr size_t MAX_DEVICES = 8;

    // --- Connections ---
    void join(const std::shared_ptr<Session>& session);
    /// Forget the connection and close it (posted to its strand). Its
    /// account goes offline for its watchers when this was its last
    /// connection. Idempotent.
    void leave(const std::shared_ptr<Session>& session);
    /// leave(), but tell the connection why first (SESSION_CLOSED) and
    /// close it once that is written.
    void retire(const std::shared_ptr<Session>& session, chatproj::SessionClosed::Reason reason);
    /// Retire every connection of `uid` but `keep` (null: all of them) with
    /// `reason` (SIGNED_OUT, PASSWORD_CHANGED), which tells clients to sign
    /// out rather than reconnect: "sign out all other devices", a password
    /// change. Sign the devices out at central first (their rows), or their
    /// resume tokens sign them straight back in. Returns how many were closed.
    size_t close_other_sessions(int64_t uid, const std::shared_ptr<Session>& keep,
                                chatproj::SessionClosed::Reason reason);
    /// The same for the connections of the account's `devices` (device ids).
    size_t close_device_sessions(int64_t uid, const std::vector<std::string>& devices,
                                 const std::shared_ptr<Session>& keep, chatproj::SessionClosed::Reason reason);
    /// The device ids the account is connected from.
    std::unordered_set<std::string> online_devices(int64_t uid);

    struct Bound {
        bool ok = false;    // false: refused (`full`) or the connection left meanwhile
        bool full = false;  // the account has MAX_DEVICES other devices signed in
        /// The account's previous connection from the same device id,
        /// already forgotten here: the caller retires it (REPLACED).
        std::shared_ptr<Session> replaced;
        /// The account's presence after signing in: the live one when it was
        /// already signed in elsewhere (newer than the login's database
        /// read), else the one passed in.
        Presence presence;
    };
    /// Sign `session` in as `uid` from `device_id`. A connection from the
    /// same non-empty device id is replaced; other devices stay. Over
    /// MAX_DEVICES the login is refused. Watchers see the account come
    /// online (or stop idling) when that is visible.
    ///
    /// On success the sign-in's reply goes to `session` from under the lock,
    /// so nothing another device changes reaches the connection before it:
    /// `reply` (built with `presence`), or `rebuild(live)` when the account
    /// was signed in already and its live state differs.
    Bound bind(const std::shared_ptr<Session>& session, int64_t uid, const std::string& device_id,
               Presence presence, const Frame& reply, const std::function<Frame(const Presence&)>& rebuild);

    // --- Delivery ---
    /// Every connection of the account. False when it has none here.
    bool deliver_to_user(int64_t uid, const Frame& frame);
    bool deliver_to_user(int64_t uid, const chatproj::Packet& packet);
    /// Every connection of the account but `except` (the one a change came
    /// from, which learns it from its own reply).
    void deliver_to_user_except(int64_t uid, const Session* except, const chatproj::Packet& packet);
    void deliver_to_users(const std::vector<int64_t>& uids, const chatproj::Packet& packet);
    void deliver_to_watchers(int64_t uid, const chatproj::Packet& packet);

    // --- Presence ---
    /// Online as others see it: signed in and not invisible. Every
    /// presence-revealing path (friend lists, call invites) asks this.
    bool is_user_visible(int64_t uid);
    /// Apply `change` to the signed-in account's presence; when what others
    /// see changes, its watchers get a PresenceDelta, and when its own state
    /// changes (status, message, DM privacy) every connection of the account
    /// gets an OWN_PRESENCE. False when the account isn't signed in here.
    /// `save` (a database write) runs after, outside the lock but in order
    /// with the account's other changes: two devices changing it at once
    /// leave the database with the directory's last state.
    bool update_presence(int64_t uid, const std::function<void(Presence&)>& change,
                         const std::function<void(const Presence&)>& save = nullptr);
    /// Auto-away for one connection. The account is idle while all of its
    /// connections are.
    void set_idle(const std::shared_ptr<Session>& session, bool idle);
    /// Replace the set of accounts `session` (signed in as `uid`) shows;
    /// the newly watched are answered with a PRESENCE_UPDATE: presence for
    /// the visible, the avatar version for the rest (read from the
    /// database outside the lock for accounts not signed in). Cut at
    /// MAX_WATCH; non-positive uids are dropped.
    void watch(const std::shared_ptr<Session>& session, int64_t uid, const std::vector<int64_t>& uids,
               AuthManager& auth_manager);

    // --- Profile pushes (to the account's watchers and its other connections) ---
    void push_user_renamed(int64_t uid, const std::string& username, const Session* from);
    void push_avatar_changed(int64_t uid, const std::string& version, const Session* from);
    void push_e2ee_keys_changed(int64_t uid, uint32_t key_id, const Session* from);
    /// Bumped by every rename: a session whose copy differs re-reads its
    /// account's name (account_name) before handling its next packet.
    uint64_t rename_epoch() const { return rename_epoch_.load(std::memory_order_acquire); }
    /// The signed-in account's current name, or `fallback`.
    std::string account_name(int64_t uid, const std::string& fallback);

    bool check_dm_allowed(int64_t sender, int64_t recipient, AuthManager& auth_manager);

    // --- Calls (multi-device design §Calls) ---
    enum class CallRoute {
        sent,     // delivered (or deliberately swallowed, e.g. a BUSY while another device rings)
        offline,  // an INVITE to an account with no connection: answer PEER_OFFLINE
        dropped,  // no such call between these accounts, or not this connection's to send
    };
    /// Route the CALL_SIGNAL `routed` (built and stamped by the handler:
    /// from = the sender `from`, signed in as `from_uid`; to = `to_uid`)
    /// through the call registry: an INVITE rings every connection of the
    /// callee; the first to answer takes the call and the others are told;
    /// after that, signals go only between the two connections in the call.
    CallRoute route_call(const std::shared_ptr<Session>& from, int64_t from_uid, int64_t to_uid,
                         const chatproj::Packet& routed);

    // --- Housekeeping (the sweep timer) ---
    /// Close connections idle for longer than `timeout`.
    void sweep_stale(std::chrono::seconds timeout);
    /// Clear custom status messages whose time ran out: saved, and
    /// announced to watchers when they were visible.
    void expire_status_messages(AuthManager& auth_manager);
    /// Forget calls nobody can finish any more.
    void sweep_calls();

private:
    /// One connection.
    struct Connection {
        int64_t uid = 0;  // 0 before login
        std::string device_id;
        bool idle = false;
        std::unordered_set<int64_t> watching;
    };
    struct Account {
        /// Its signed-in connections, in sign-in order.
        std::vector<std::shared_ptr<Session>> sessions;
        Presence presence;
        /// Held (never with mutex_) across one presence change and its
        /// database save (update_presence).
        std::shared_ptr<std::mutex> saving = std::make_shared<std::mutex>();
    };
    /// A call between two accounts, by call id. The caller's connection,
    /// the callee connections still ringing, and once answered the one that
    /// took it. A side whose connection left is null and keeps its device
    /// id: the same device signing back in takes its place.
    struct Call {
        int64_t caller_uid = 0;
        int64_t callee_uid = 0;
        std::string caller_name;
        std::string callee_name;
        std::shared_ptr<Session> caller;
        std::string caller_device;
        std::vector<std::shared_ptr<Session>> ringing;
        std::shared_ptr<Session> callee;
        std::string callee_device;
        bool answered = false;
        bool rang = false;  // a RINGING went to the caller
        bool busy = false;  // a ringing device answered BUSY
        std::chrono::steady_clock::time_point started;
        /// When a side of an answered call left (unset while both are here).
        std::chrono::steady_clock::time_point detached{};
    };
    /// Deliveries gathered under the lock, made after it.
    struct Fanout {
        std::vector<std::pair<std::vector<std::shared_ptr<Session>>, Frame>> sends;
        void add(std::vector<std::shared_ptr<Session>> to, Frame frame);
        void run();
    };

    // Callers hold mutex_.
    std::vector<std::shared_ptr<Session>> watcher_sessions_locked(int64_t uid) const;
    void unwatch_all_locked(Session* session, Connection& conn);
    /// Remove `session` from its account (which goes offline, or recomputes
    /// idle, for watchers and its other connections) and detach it from its
    /// calls. Leaves its Connection in sessions_, signed out.
    void sign_out_locked(const std::shared_ptr<Session>& session, Connection& conn, Fanout& out);
    /// Announce what changed between `before` / `before_own` and the
    /// account's presence now: a delta to watchers, OWN_PRESENCE to its
    /// connections but `except`.
    void presence_changed_locked(int64_t uid, const Account& account, const Presence& before,
                                 const Session* except, Fanout& out);
    void recompute_idle_locked(Account& account);
    /// The account's connection from `device`, other than `except`.
    std::shared_ptr<Session> device_session_locked(int64_t uid, const std::string& device,
                                                   const Session* except = nullptr) const;
    /// A side of a call whose connection left: the same device signed back in
    /// (or replacing it) takes its place.
    void reattach_locked(Call& c, const std::string& id, const Session* except = nullptr);
    void call_leave_locked(Session* session, const std::string& device, Fanout& out);
    void erase_call_locked(const std::string& id);
    void index_call_locked(Session* session, const std::string& id);
    void unindex_call_locked(Session* session, const std::string& id);
    static void fill_presence(chatproj::UserPresence* entry, int64_t uid, const Presence& p);
    /// A PresenceDelta for `uid`: online with `p` when given, else offline.
    static chatproj::Packet presence_delta(int64_t uid, const std::string& username, const Presence* p);
    static chatproj::Packet own_presence(const Presence& p);
    static Frame session_closed(chatproj::SessionClosed::Reason reason);

    /// Every connection.
    std::unordered_map<std::shared_ptr<Session>, Connection> sessions_;
    std::unordered_map<int64_t, Account> accounts_;
    /// uid → the connections watching it (the reverse of Connection::
    /// watching). A connection is in here only while it is in sessions_,
    /// which keeps it alive.
    std::unordered_map<int64_t, std::unordered_set<Session*>> watchers_;
    std::unordered_map<std::string, Call> calls_;
    /// Connection → the calls it is in (sparse: only connections in a call).
    std::unordered_map<Session*, std::vector<std::string>> call_index_;
    std::atomic<uint64_t> rename_epoch_{0};
    std::mutex mutex_;
};
