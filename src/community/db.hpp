#pragma once

#include <cstdint>
#include <mutex>
#include <optional>
#include <string>
#include <vector>

struct sqlite3;

namespace chatproj {

struct DbMember {
    std::string username;
    int64_t joined_at = 0;
    std::string nickname;
};

struct DbInvite {
    std::string code;
    std::string created_by;
    int64_t created_at = 0;
    int64_t expires_at = 0; // 0 = never
    int32_t max_uses = 0;   // 0 = unlimited
    int32_t uses = 0;
};

struct DbChannel {
    std::string id;
    std::string name;
    int32_t type = 0; // matches chatproj::ChannelInfo::Type
    int32_t position = 0;
    int32_t voice_bitrate_kbps = 0;
};

struct DbBan {
    std::string username;
    int64_t banned_at = 0;
    std::string banned_by;
    std::string reason;
};

// Reason an invite cannot be redeemed.
enum class InviteResult {
    Ok,
    Unknown,
    Expired,
    Exhausted,
    Banned,
    AlreadyMember
};

// Thin RAII wrapper around SQLite3 for the community server.
// All methods are thread-safe (serialized internally). Errors are logged and
// return sensible defaults — the server never throws out of this layer.
class CommunityDb {
public:
    CommunityDb();
    ~CommunityDb();

    CommunityDb(const CommunityDb&) = delete;
    CommunityDb& operator=(const CommunityDb&) = delete;

    // Opens/creates the DB file. If the file is freshly created, seeds
    // server_meta (owner/name/description), the default channels, and
    // adds the owner to members. Returns false on unrecoverable errors.
    bool open(const std::string& path,
              const std::string& owner_username,
              const std::string& server_name,
              const std::string& server_description);

    // --- server_meta ---
    std::string owner() const;
    std::string server_name() const;
    std::string server_description() const;

    // --- membership ---
    bool is_member(const std::string& username) const;
    bool add_member(const std::string& username);
    bool remove_member(const std::string& username);
    std::vector<DbMember> list_members() const;

    // --- bans ---
    bool is_banned(const std::string& username) const;
    bool add_ban(const std::string& username,
                 const std::string& banned_by,
                 const std::string& reason);
    bool remove_ban(const std::string& username);
    std::vector<std::string> list_bans() const;

    // --- invites ---
    // Generates a code and inserts a new invite. Returns the created invite on
    // success, or std::nullopt on failure.
    std::optional<DbInvite> create_invite(const std::string& created_by,
                                          int64_t expires_at,
                                          int32_t max_uses);
    std::vector<DbInvite> list_invites() const;
    bool revoke_invite(const std::string& code);

    // Atomically validates and consumes an invite (increments uses, deletes if
    // exhausted). On success, `out_invite` is populated. Does NOT add the
    // redeeming user to members — the caller does that after the connection is
    // accepted.
    InviteResult redeem_invite(const std::string& code,
                               const std::string& redeeming_user,
                               DbInvite* out_invite);

    // --- channels ---
    std::vector<DbChannel> list_channels() const;

private:
    void init_schema_();
    void seed_if_empty_(const std::string& owner,
                        const std::string& name,
                        const std::string& desc);
    std::string get_meta_(const std::string& key) const;
    void set_meta_(const std::string& key, const std::string& value);

    sqlite3* db_ = nullptr;
    mutable std::mutex mutex_;
};

} // namespace chatproj
