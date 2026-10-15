#pragma once
#include <string>
#include <optional>
#include <unordered_map>
#include <vector>
#include <jwt-cpp/traits/nlohmann-json/defaults.h>
#include <pqxx/pqxx>
#include "messages.pb.h"
#include "pg_pool.hpp"

class AuthManager {
public:
    /// `community_secret` authenticates community→central packets
    /// (heartbeat, invite / membership / picture sync). It is NOT a signing
    /// key: JWTs are Ed25519-signed with `jwt_private_pem`; community
    /// servers hold only the public key and can verify but never forge.
    AuthManager(const std::string& community_secret, const std::string& db_conn_str,
                const std::string& jwt_private_pem, const std::string& jwt_public_pem)
        : secret_key_(community_secret), db_conn_str_(db_conn_str),
          jwt_private_pem_(jwt_private_pem), jwt_public_pem_(jwt_public_pem),
          signer_(jwt_public_pem, jwt_private_pem),
          session_verifier_(jwt::verify()
              .allow_algorithm(jwt::algorithm::ed25519{jwt_public_pem, ""})
              .with_issuer(TOKEN_ISSUER)),
          resume_verifier_(jwt::verify()
              .allow_algorithm(jwt::algorithm::ed25519{jwt_public_pem, ""})
              .with_issuer(TOKEN_ISSUER)
              .with_audience(RESUME_AUDIENCE)),
          pool_(db_conn_str, 4) {
        initializeDatabase();
    }

    /// Idle Postgres connections kept for reuse: one per thread that queries
    /// (central-scaling-design.md). Every method is safe to call from any
    /// thread: each takes its own connection lease.
    void set_pool_idle(std::size_t max_idle) { pool_.set_max_idle(max_idle); }

    // Processes registration. Returns error message if failed, empty string if successful.
    std::string registerUser(const std::string& username, const std::string& email, const std::string& password);

    /// The registration rules for a name: "" when valid, else why not.
    static std::string validateUsername(const std::string& username);

    /// A successful rename: the account's names and a fresh token.
    struct RenameResult {
        std::string error;        // "" on success
        std::string old_username;
        std::string username;
        std::string token;
    };
    /// Change `uid`'s username (uid design, stage 5). Confirmed with the
    /// password; the old name is reserved for this account for
    /// NAME_RESERVATION_SECONDS; at most RENAMES_PER_DAY a day.
    RenameResult renameUser(int64_t uid, const std::string& new_username, const std::string& password);
    static constexpr int64_t NAME_RESERVATION_SECONDS = 14 * 24 * 3600;
    static constexpr int RENAMES_PER_DAY = 2;
    /// Forget renames past the reservation: nothing reads them (the
    /// reservation looks back 14 days, the daily limit one), and a
    /// lasting record of past names would follow people who renamed to
    /// leave one behind. Runs hourly and with every rename.
    void pruneNameChanges();

    /// A signed 24 h token naming the account (login, renames, renewal).
    std::optional<std::string> mintToken(int64_t uid, const std::string& username);

    /// A successful login: the signed JWT and the account it names.
    struct LoginResult {
        std::string token;
        int64_t uid = 0;
        std::string username;   // as stored
    };
    // Processes login. Returns the token and account, or std::nullopt if failed.
    std::optional<LoginResult> authenticateUser(const std::string& username, const std::string& password);

    /// A session token's claims.
    struct TokenClaims {
        int64_t uid = 0;
        int64_t expires_at = 0;  // unix seconds
    };
    /// A session token (the one every packet and every community sees):
    /// signed here, not expired, no audience. nullopt otherwise. The keys
    /// are parsed once, at startup.
    std::optional<TokenClaims> verifyToken(const std::string& token);

    /// Resume tokens (docs/superpowers/specs/2026-10-15-central-hardening-
    /// design.md): a dropped connection signs back in with one instead of
    /// the password. Central-only: their audience keeps communities, which
    /// see every member's session token, from ever passing one as a session
    /// token, and a session token is never accepted as one. Valid for
    /// RESUME_LIFETIME from the password sign-in; resuming doesn't extend it.
    static constexpr int64_t RESUME_LIFETIME_SECONDS = 30 * 24 * 3600;
    std::optional<std::string> mintResumeToken(int64_t uid, int64_t signed_in_at);
    /// Sign in with a resume token: the account (its current name) and a
    /// fresh session token; nullopt when the token is invalid or expired,
    /// the account is gone, or its password sign-in predates the account's
    /// `users.sessions_valid_after` (setting that column to now cancels
    /// every resume token the account has; the hook for "log out other
    /// devices" and password changes).
    std::optional<LoginResult> resumeSession(const std::string& resume_token);

    // Constant-time comparison of the community↔central secret. (Length is
    // allowed to short-circuit; the length isn't the secret.)
    bool verifySharedSecret(const std::string& secret) const {
        if (secret.size() != secret_key_.size()) return false;
        volatile unsigned char diff = 0;
        for (std::size_t i = 0; i < secret_key_.size(); ++i) {
            diff |= static_cast<unsigned char>(secret[i]) ^
                    static_cast<unsigned char>(secret_key_[i]);
        }
        return diff == 0;
    }
    std::vector<chatproj::CommunityServerInfo> getCommunityServers();
    /// Returns the assigned id (community_servers.id, SERIAL). Used by
    /// the heartbeat handler to ack the community with its
    /// central-side id via SERVER_HEARTBEAT_RES so it can later
    /// populate Membership{Register,Revoke}Req packets.
    // `known_id` > 0 (the id the community cached from an earlier heartbeat
    // response) updates THAT row — host/port included — so an IP or port
    // change keeps the server's identity and every membership row. 0 falls
    // back to the (host_ip, port) upsert.
    int upsertCommunityServer(const std::string& name, const std::string& description,
                              const std::string& host_ip, int port, int member_count,
                              int64_t known_id = 0,
                              const std::string& cert_fingerprint = "",
                              bool is_public = false);

    // Invite lookup (community servers register invites here so clients can
    // redeem a raw code without knowing the hosting server's host:port).
    void registerCommunityInvite(const std::string& code, const std::string& host, int port, int64_t expires_at);
    void unregisterCommunityInvite(const std::string& code);
    // Returns the endpoint (plus the community's directory row for the
    // pre-join preview) or nullopt if the code is unknown or expired.
    struct ResolvedInvite {
        std::string host;
        int port = 0;
        std::string cert_fingerprint;   // '' if the community hasn't reported one
        // From community_servers; zero / empty when the community has
        // never heartbeated (the invite still resolves to host:port).
        int64_t server_id = 0;
        std::string name;
        std::string description;
        int member_count = 0;
        std::string picture_version;
    };
    std::optional<ResolvedInvite> resolveCommunityInvite(const std::string& code);

    /// An account as a request names it (users by uid,
    /// docs/superpowers/specs/2026-10-15-uid-identity-design.md).
    struct UserRef {
        int64_t uid = 0;        // > 0 found; 0 no such account; -1 database error
        std::string username;   // the current name ('' unless found)
        bool found() const { return uid > 0; }
    };
    /// The account a request names: by `uid` when it is > 0, else by the
    /// exact `username` (typed names and older clients). Every request that
    /// names another user resolves it here before anything is written.
    UserRef resolveUser(int64_t uid, const std::string& username);

    // Friend System. Both sides are existing accounts (the caller resolved
    // the target); returns '' on success or the message for the requester.
    std::string handleFriendAction(int64_t requester, chatproj::FriendActionType action, int64_t target);
    /// The user's friendships, each with the other side's uid, current name
    /// and avatar_version.
    std::vector<chatproj::FriendInfo> getFriends(int64_t uid);
    /// True if there is a BLOCKED relationship between the two users
    /// (canonical pair, either direction). Used to reject DMs to/from a
    /// blocked user regardless of online state or friends-only setting.
    bool isBlocked(int64_t a, int64_t b);
    /// True if the two users are friends (an accepted request).
    bool areFriends(int64_t a, int64_t b);

    // Avatar storage (see docs/superpowers/specs/
    // 2026-05-12-custom-profile-pictures-design.md §5).
    //
    // setAvatar: write/remove the user's avatar. `data` empty means
    // remove (clears avatar + version). Returns the new sha256-hex
    // version on success ('' on removal). Throws on DB error.
    std::string setAvatar(int64_t uid, const std::string& data);

    // getAvatar: returns (version, bytes). Version is '' when the
    // user has no avatar; data is empty in that case. Returns
    // ('', '') for unknown users (callers can't distinguish — same
    // privacy model as letter avatars).
    std::pair<std::string, std::string> getAvatar(int64_t uid);

    /// The avatar versions of those of `uids` that have a picture, in one
    /// read (no bytes): what a subscription answer tells about accounts
    /// that aren't signed in. Empty on a database error.
    std::unordered_map<int64_t, std::string> getAvatarVersions(const std::vector<int64_t>& uids);

    // --- User statuses (docs/superpowers/specs/2026-10-11-user-status-design.md) ---
    /// What a login needs for presence, in one users-row read.
    struct PresenceProfile {
        std::string avatar_version;
        chatproj::PresenceStatus status = chatproj::PRESENCE_ONLINE;
        bool dm_friends_only = false;
        /// Custom status message; an expired one is cleared by the read.
        std::string status_text;
        std::string status_emoji;
        int64_t status_expires_at = 0;  // unix seconds, 0 = never
    };
    PresenceProfile getPresenceProfile(int64_t uid);
    /// `status` is a PresenceStatus value (the caller validated it).
    void setPresenceStatus(int64_t uid, int status);
    /// The caller normalised text / emoji; expires_at is unix seconds (0 = never).
    void setStatusMessage(int64_t uid, const std::string& text,
                          const std::string& emoji, int64_t expires_at);
    /// Clear the message that expires at `expires_at` (the sweep); a newer
    /// one the user set meanwhile is kept.
    void clearExpiredStatusMessage(int64_t uid, int64_t expires_at);
    void setDmFriendsOnly(int64_t uid, bool friends_only);
    /// For DMs to an offline (or invisible) recipient — an online one's
    /// session caches the same value.
    bool getDmFriendsOnly(int64_t uid);

    // --- Persistent DMs ---
    // (see docs/superpowers/specs/2026-05-14-persistent-dms-design.md)
    /// Each row keeps the names it was sent under (`sent_*`, what a v1
    /// envelope binds) next to the sender's uid and current name.
    struct DmHistoryRow {
        int64_t id = 0;
        int64_t sender_uid = 0;
        std::string sender;          // current name
        std::string sent_sender;     // name sent under
        std::string sent_recipient;
        std::string content;
        int64_t timestamp = 0;
        int64_t edited_at = 0;
        int64_t reply_to = 0;
        // Embedded parent preview, resolved by LEFT JOIN at fetch time.
        // reply_to_sender_uid 0 with reply_to>0 = parent was deleted.
        int64_t reply_to_sender_uid = 0;
        std::string reply_to_sender;
        std::string reply_to_sent_sender;
        std::string reply_to_sent_recipient;
        std::string reply_to_content;
        // E2EE envelopes (opaque bytes); empty for plaintext rows.
        std::string envelope;
        std::string reply_to_envelope;
    };
    struct DmConversationPreviewRow {
        int64_t peer_uid = 0;
        std::string peer;            // current name
        std::string last_message_content;
        int64_t last_message_sender_uid = 0;
        std::string last_message_sender;
        std::string last_sent_sender;
        std::string last_sent_recipient;
        int64_t last_message_id = 0;
        int64_t last_timestamp = 0;
        int64_t unread_count = 0;
        std::string last_message_envelope;
    };
    /// One DM's author + body, for the reply-parent preview.
    struct DmPreviewRow {
        int64_t sender_uid = 0;
        std::string sender;          // current name
        std::string sent_sender;
        std::string sent_recipient;
        std::string content;
        std::string envelope;
    };

    /// Insert a new DM, return its autoincrement id. Returns 0 on
    /// DB failure (caller surfaces a "could not deliver" error to
    /// the sender).
    /// `envelope` non-empty = an E2EE row: stored opaquely next to the
    /// placeholder `content`.
    /// The names are the ones the message is sent under (stored with it).
    int64_t insertDm(int64_t sender_uid, const std::string& sender,
                     int64_t recipient_uid, const std::string& recipient,
                     const std::string& content,
                     int64_t sent_at,
                     int64_t reply_to = 0,
                     const std::string& envelope = std::string());

    /// Fetch a page of messages between user_a and user_b, ordered
    /// newest first. before_id = 0 means "latest". limit is clamped
    /// to [1, 200] internally. Sets has_more to true if more
    /// messages exist older than the page.
    std::vector<DmHistoryRow> fetchDmHistory(int64_t user_a, int64_t user_b,
                                              int64_t before_id,
                                              int32_t limit,
                                              bool& has_more);

    /// Fetch a context window centered on `around_id` (the target DM):
    /// up to `limit` messages with id <= around (target included) and up
    /// to `limit` with id > around. Returned oldest→newest. limit defaults
    /// to 25 and clamps to [1, 100] per side. has_more_before/after report
    /// whether messages exist beyond each edge of the window. Used by the
    /// windowed jump-to-message flow.
    std::vector<DmHistoryRow> fetchDmHistoryAround(int64_t user_a, int64_t user_b,
                                                   int64_t around_id,
                                                   int32_t limit,
                                                   bool& has_more_before,
                                                   bool& has_more_after);

    /// Fetch up to `limit` messages newer than `after_id` (id > after_id),
    /// ordered oldest→newest. limit defaults to 50, clamps to [1, 200].
    /// Downward pagination for the windowed jump flow. has_more_after is
    /// set if still-newer messages exist.
    std::vector<DmHistoryRow> fetchDmHistoryAfter(int64_t user_a, int64_t user_b,
                                                  int64_t after_id,
                                                  int32_t limit,
                                                  bool& has_more_after);

    /// (sender, content) of one DM, constrained to the user_a↔user_b
    /// conversation — a forged reply_to must not leak another
    /// conversation's content. nullopt = not found / not this pair.
    /// Used to validate + embed the reply parent preview on send.
    std::optional<DmPreviewRow> fetchDmPreview(int64_t user_a, int64_t user_b, int64_t message_id);

    /// One row per conversation the user is part of, with the most
    /// recent message preview + unread count (messages from peer
    /// with id > dm_read_state.last_read_id).
    std::vector<DmConversationPreviewRow> fetchDmConversations(int64_t uid);

    /// Upsert dm_read_state, setting last_read_id =
    /// GREATEST(existing, up_to_id). Idempotent and race-safe.
    void markDmRead(int64_t reader, int64_t peer, int64_t up_to_id);

    /// Sender-enforced atomic delete. The WHERE clause is the
    /// authorization check — only the row's sender can delete it,
    /// and the recipient must match the requested peer. Returns
    /// true iff exactly one row was deleted.
    bool deleteDmMessage(int64_t sender, int64_t peer, int64_t message_id);

    /// Sender-enforced atomic edit. Like deleteDmMessage, the WHERE clause is
    /// the authorization check — only the row's sender may edit it. Stamps
    /// edited_at. Returns true iff exactly one row was updated.
    bool editDmMessage(int64_t sender, int64_t peer,
                       int64_t message_id,
                       const std::string& content,
                       int64_t edited_at,
                       const std::string& envelope = std::string());

    // --- End-to-end encrypted DMs ---
    // (see docs/superpowers/specs/2026-09-03-e2ee-dms-design.md)
    struct E2eeKeyRow {
        uint32_t key_id;
        std::string signed_name;   // the name a v1 bundle's signature covers
        std::string dh_pub;
        std::string sign_pub;
        std::string signature;
        int64_t created_at;
        int binding = 1;           // 1 = the signature binds signed_name, 2 = the uid
    };

    /// Store a new identity bundle for `uid`, assigning
    /// key_id = MAX(key_id) + 1 for that user. `signed_name` is the
    /// publisher's current name (what a v1 signature covers). Returns the
    /// assigned id, 0 on DB failure. Sizes are validated by the caller.
    uint32_t publishE2eeKeys(int64_t uid, const std::string& signed_name,
                             const std::string& dh_pub,
                             const std::string& sign_pub,
                             const std::string& signature,
                             int binding,
                             int64_t created_at);

    /// Re-sign the current bundle in place (uid design, stage 4): replaces
    /// its signature, binding and signed_name when `key_id` is the user's
    /// current one and the keys match it. False otherwise or on DB failure.
    bool rebindE2eeKeys(int64_t uid, uint32_t key_id,
                        const std::string& dh_pub,
                        const std::string& sign_pub,
                        const std::string& signature,
                        int binding,
                        const std::string& signed_name);

    /// A user's bundle: key_id 0 = the current (highest) one. nullopt when
    /// the user has never published or the id is unknown.
    std::optional<E2eeKeyRow> getE2eeKeys(int64_t uid, uint32_t key_id);

    /// Replace the user's passphrase-wrapped private-key backup. Returns
    /// false on DB failure.
    bool setE2eeBackup(int64_t uid, uint32_t key_id,
                       const std::string& blob, int64_t updated_at);

    /// (key_id, blob) of the user's backup; nullopt when none.
    std::optional<std::pair<uint32_t, std::string>> getE2eeBackup(int64_t uid);

    // --- Auto-rejoin community memberships ---
    // (see docs/superpowers/specs/2026-05-14-auto-rejoin-communities-design.md)

    /// Idempotent insert. Called on every successful community auth
    /// (via MEMBERSHIP_REGISTER_REQ). ON CONFLICT DO NOTHING — safe to
    /// re-fire on every auth and serves as bootstrap for pre-feature
    /// memberships. A uid with no account is ignored.
    void registerMembership(int64_t uid, int64_t server_id);

    /// Idempotent delete. Called by the community-side kick/ban/leave
    /// path (shared secret) and by the client-side stale-membership
    /// cleanup (JWT auth).
    void revokeMembership(int64_t uid, int64_t server_id);

    /// Returns every CommunityServerInfo the user is a member of.
    /// Orphan rows (server_id no longer in community_servers) filtered
    /// out via the JOIN, so the client never sees phantom tiles.
    std::vector<chatproj::CommunityServerInfo> getUserCommunities(int64_t uid);

    // --- Custom server pictures ---
    // (see docs/superpowers/specs/2026-05-15-custom-server-pictures-design.md)

    /// Atomic update of the picture bytes + sha256-hex version.
    /// Looks up the community_servers row by (host_ip, port) and
    /// writes the new picture + picture_version. Returns the
    /// assigned id, or 0 if no matching row exists yet (the
    /// community's first heartbeat hasn't landed). Empty `data`
    /// means removal: column is set NULL, version becomes ''.
    int setServerPicture(const std::string& host_ip, int port,
                         const std::string& data,
                         const std::string& version,
                         int64_t known_id);
    int setServerPicture(const std::string& host_ip, int port,
                          const std::string& data,
                          const std::string& version);

    /// Returns (version, data). Both empty when the server has no
    /// picture set, or when the id is unknown.
    std::pair<std::string, std::string> getServerPicture(int server_id);

    /// Every member's uid for this server_id.
    /// Drives central's SERVER_PICTURE_CHANGED broadcast.
    std::vector<int64_t> getServerMembers(int server_id);

private:
    std::string secret_key_;
    std::string db_conn_str_;
    std::string jwt_private_pem_;
    std::string jwt_public_pem_;
    static constexpr const char* TOKEN_ISSUER = "decibell_central_auth";
    static constexpr const char* RESUME_AUDIENCE = "decibell_central_resume";
    using JwtVerifier = decltype(jwt::verify());
    // Built once: each holds the parsed key. Only const calls after
    // construction, so every thread may use them at once.
    const jwt::algorithm::ed25519 signer_;
    const JwtVerifier session_verifier_;
    const JwtVerifier resume_verifier_;
    PgPool pool_;
    struct StoredCredentials {
        int64_t uid = 0;
        std::string username;
        std::string password_hash;
    };

    /// Builds the schema and runs pending migrations (central_schema).
    /// Throws when a migration fails: serving on a half-migrated schema
    /// would fail every request.
    void initializeDatabase();
    /// Migration 1: users by uid (2026-10-15-uid-identity-design.md).
    void migrateToUidKeys(pqxx::work& txn);
    /// The account a reserved old name belongs to (renamed away from it
    /// within the reservation), for signing in with it; nullopt otherwise.
    std::optional<StoredCredentials> getCredentialsByReservedName(const std::string& username);
    /// A name another account renamed away from within the reservation.
    bool nameReservedForOther(pqxx::work& txn, const std::string& username, int64_t uid);

    // Password hashing using bcrypt (cost factor 12)
    std::string hashPassword(const std::string& plain_password);
    bool verifyPassword(const std::string& plain_password, const std::string& hash);

    // Account rows (each takes a pool lease).
    bool userExists(const std::string& username, const std::string& email);
    bool insertUser(const std::string& username, const std::string& email, const std::string& hash);
    /// The uid, stored name and password hash from one read of the users
    /// row; nullopt when no account has the name or the database errors.
    std::optional<StoredCredentials> getCredentials(const std::string& username);
};