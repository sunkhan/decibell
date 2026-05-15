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

    bool verifySharedSecret(const std::string& secret) const { return secret == secret_key_; }
    std::vector<chatproj::CommunityServerInfo> getCommunityServers();
    /// Returns the assigned id (community_servers.id, SERIAL). Used by
    /// the heartbeat handler to ack the community with its
    /// central-side id via SERVER_HEARTBEAT_RES so it can later
    /// populate Membership{Register,Revoke}Req packets.
    int upsertCommunityServer(const std::string& name, const std::string& description, const std::string& host_ip, int port, int member_count);

    // Invite lookup (community servers register invites here so clients can
    // redeem a raw code without knowing the hosting server's host:port).
    void registerCommunityInvite(const std::string& code, const std::string& host, int port, int64_t expires_at);
    void unregisterCommunityInvite(const std::string& code);
    // Returns (host, port) or nullopt if the code is unknown or expired.
    std::optional<std::pair<std::string, int>> resolveCommunityInvite(const std::string& code);

    // Friend System
    std::string handleFriendAction(const std::string& requester, chatproj::FriendActionType action, const std::string& target);
    std::vector<chatproj::FriendInfo> getFriends(const std::string& username);

    // Avatar storage (see docs/superpowers/specs/
    // 2026-05-12-custom-profile-pictures-design.md §5).
    //
    // setAvatar: write/remove the user's avatar. `data` empty means
    // remove (clears avatar + version). Returns the new sha256-hex
    // version on success ('' on removal). Throws on DB error.
    std::string setAvatar(const std::string& username, const std::string& data);

    // getAvatar: returns (version, bytes). Version is '' when the
    // user has no avatar; data is empty in that case. Returns
    // ('', '') for unknown users (callers can't distinguish — same
    // privacy model as letter avatars).
    std::pair<std::string, std::string> getAvatar(const std::string& username);

    // Lookup just the version (no bytes). Used by broadcast_presence
    // to populate UserPresence.avatar_version cheaply without
    // reading the BYTEA payload.
    std::string getAvatarVersion(const std::string& username);

    // --- Persistent DMs ---
    // (see docs/superpowers/specs/2026-05-14-persistent-dms-design.md)
    struct DmHistoryRow {
        int64_t id;
        std::string sender;
        std::string content;
        int64_t timestamp;
    };
    struct DmConversationPreviewRow {
        std::string peer;
        std::string last_message_content;
        std::string last_message_sender;
        int64_t last_message_id;
        int64_t last_timestamp;
        int64_t unread_count;
    };

    /// Insert a new DM, return its autoincrement id. Returns 0 on
    /// DB failure (caller surfaces a "could not deliver" error to
    /// the sender).
    int64_t insertDm(const std::string& sender,
                     const std::string& recipient,
                     const std::string& content,
                     int64_t sent_at);

    /// Fetch a page of messages between user_a and user_b, ordered
    /// newest first. before_id = 0 means "latest". limit is clamped
    /// to [1, 200] internally. Sets has_more to true if more
    /// messages exist older than the page.
    std::vector<DmHistoryRow> fetchDmHistory(const std::string& user_a,
                                              const std::string& user_b,
                                              int64_t before_id,
                                              int32_t limit,
                                              bool& has_more);

    /// One row per conversation the user is part of, with the most
    /// recent message preview + unread count (messages from peer
    /// with id > dm_read_state.last_read_id).
    std::vector<DmConversationPreviewRow> fetchDmConversations(
        const std::string& user);

    /// Upsert dm_read_state, setting last_read_id =
    /// GREATEST(existing, up_to_id). Idempotent and race-safe.
    void markDmRead(const std::string& reader,
                    const std::string& peer,
                    int64_t up_to_id);

    /// Sender-enforced atomic delete. The WHERE clause is the
    /// authorization check — only the row's sender can delete it,
    /// and the recipient must match the requested peer. Returns
    /// true iff exactly one row was deleted.
    bool deleteDmMessage(const std::string& sender,
                         const std::string& peer,
                         int64_t message_id);

    // --- Auto-rejoin community memberships ---
    // (see docs/superpowers/specs/2026-05-14-auto-rejoin-communities-design.md)

    /// Idempotent insert. Called on every successful community auth
    /// (via MEMBERSHIP_REGISTER_REQ). ON CONFLICT DO NOTHING — safe to
    /// re-fire on every auth and serves as bootstrap for pre-feature
    /// memberships.
    void registerMembership(const std::string& username, int64_t server_id);

    /// Idempotent delete. Called by the community-side kick/ban/leave
    /// path (shared secret) and by the client-side stale-membership
    /// cleanup (JWT auth).
    void revokeMembership(const std::string& username, int64_t server_id);

    /// Returns every CommunityServerInfo the user is a member of.
    /// Orphan rows (server_id no longer in community_servers) filtered
    /// out via the JOIN, so the client never sees phantom tiles.
    std::vector<chatproj::CommunityServerInfo> getUserCommunities(
        const std::string& username);

private:
    std::string secret_key_;
    std::string db_conn_str_; // Add this member variable

    void initializeDatabase();

    // Password hashing using bcrypt (cost factor 12)
    std::string hashPassword(const std::string& plain_password);
    bool verifyPassword(const std::string& plain_password, const std::string& hash);

    // Database interaction stubs (to be replaced with PostgreSQL pool)
    bool userExists(const std::string& username, const std::string& email);
    void insertUser(const std::string& username, const std::string& email, const std::string& hash);
    std::optional<std::string> getPasswordHash(const std::string& username);
};