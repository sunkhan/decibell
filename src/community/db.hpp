#pragma once

#include <cstdint>
#include <mutex>
#include <optional>
#include <set>
#include <string>
#include <unordered_map>
#include <utility>
#include <vector>

struct sqlite3;

namespace chatproj {

// Users are keyed by central's account uid (users by uid,
// docs/superpowers/specs/2026-10-15-uid-identity-design.md). A negative uid
// is a legacy placeholder: a name seen before uids reached this server,
// rewritten to the real uid when a token with that name first
// authenticates (claim_legacy). Names are the latest one seen.
struct DbMember {
    int64_t uid = 0;
    std::string username;          // latest name seen (from the member's token)
    int64_t joined_at = 0;
    std::string nickname;
    int64_t timed_out_until = 0;   // unix seconds; 0 = not timed out
    bool server_muted = false;     // moderator-applied voice mute (persists)
    bool server_deafened = false;
};

struct DbInvite {
    std::string code;
    int64_t created_by_uid = 0;
    std::string created_by;   // current name ('' once they left)
    int64_t created_at = 0;
    int64_t expires_at = 0; // 0 = never
    int32_t max_uses = 0;   // 0 = unlimited
    int32_t uses = 0;
    // Personal invite: only this account can redeem it (0 = anyone).
    int64_t for_uid = 0;
    std::string for_user;     // their name when the invite was made
};

struct DbChannel {
    std::string id;
    std::string name;
    int32_t type = 0; // matches chatproj::ChannelInfo::Type
    int32_t position = 0;
    int32_t voice_bitrate_kbps = 0;
    // Per-channel retention in days. 0 = forever.
    int32_t retention_days_text = 0;
    int32_t retention_days_image = 0;
    int32_t retention_days_video = 0;
    int32_t retention_days_document = 0;
    int32_t retention_days_audio = 0;
    int32_t slowmode_seconds = 0;  // text channels; 0 = off
    // Text channels: end-to-end encrypted with member-held epoch keys
    // (spec 2026-09-04). The server enforces the wire format either way.
    bool encrypted = false;
    // Follows its category's overwrites (permissions v3). Invariants: a
    // synced channel has no overwrite rows of its own, and its category
    // has category_sync on.
    bool perm_synced = false;
    // CATEGORY rows: channels may follow this category's overwrites.
    // Off (default) = the category only groups channels.
    bool category_sync = false;
    // Text channels: what the channel is for ("" = none).
    std::string topic;
};

struct DbMessage {
    int64_t id = 0;
    std::string channel_id;
    int64_t sender_uid = 0;
    std::string sender;        // current name (the member's), else sent_sender
    std::string sent_sender;   // the name it was sent under (a v1 envelope binds it)
    std::string content;
    int64_t timestamp = 0;
    int64_t edited_at = 0;   // unix seconds; 0 = never edited
    int64_t reply_to = 0;    // parent message id; 0 = not a reply
    // Embedded parent preview, resolved by LEFT JOIN at fetch time so reply
    // previews render even when the parent is outside the client's loaded
    // window. Empty sender with reply_to>0 = parent was deleted.
    int64_t reply_to_sender_uid = 0;
    std::string reply_to_sender;        // current name
    std::string reply_to_sent_sender;   // the name it was sent under
    std::string reply_to_content;
    // Attachment.Kind of each of the parent's attachments, position order —
    // labels an attachment-only parent ("Image", "Video", …) on the client.
    std::vector<int32_t> reply_to_attachment_kinds;
    // Encrypted channels: the sealed body (opaque); empty = plaintext row.
    std::string envelope;
    std::string reply_to_envelope;
    // Sender lacked EMBED_LINKS when sending / last editing.
    bool suppress_embeds = false;
};

// One escrowed epoch key blob (opaque to the server).
// The names are the ones it was sealed under (a v1 pairwise envelope binds them).
struct DbChannelKeyBlob {
    uint32_t epoch = 0;
    int64_t sender_uid = 0;
    std::string sender;
    int64_t recipient_uid = 0;
    std::string recipient;
    std::string blob;
};

struct DbAttachment {
    int64_t id = 0;
    int64_t message_id = 0;
    int32_t kind = 0;      // matches chatproj::Attachment::Kind
    std::string filename;
    std::string mime;
    int64_t size_bytes = 0;
    std::string storage_path; // empty when purged
    int32_t position = 0;
    int64_t created_at = 0;
    int64_t purged_at = 0;  // 0 = still present
    // Upload lifecycle:
    //   'uploading' — bytes still arriving via PATCH, not safe to bind or serve
    //   'ready'     — full bytes on disk, hash optionally verified, can be
    //                 bound to a message and downloaded
    std::string upload_status;
    int64_t expected_size = 0; // declared at init; used to validate final size
    int64_t uploader_uid = 0;  // account that POSTed /init
    std::string channel_id;    // channel the upload was initiated for (VIEW gate on GET)
    // Intrinsic pixel dimensions; 0 if unknown (older row or non-image kind).
    // Populated from the uploader's init metadata.
    int32_t width = 0;
    int32_t height = 0;
    // Total bytes across all server-stored JPEG thumbnail sizes for
    // this attachment. 0 = no thumbnails. Clients use >0 as a "fetch
    // makes sense" flag, then look at thumbnail_sizes_mask to pick a
    // size. Legacy rows have a single 320px thumbnail at the old
    // ".thumb.jpg" path with mask = 0 — the GET handler falls back.
    int64_t thumbnail_size_bytes = 0;
    // Bitmask of pre-generated thumbnail sizes available on disk.
    // bit 0 = 320 px long-edge, bit 1 = 640 px, bit 2 = 1280 px.
    int32_t thumbnail_sizes_mask = 0;
    // Duration in milliseconds for audio/video attachments; 0 unknown.
    // Populated from the uploader's /init metadata.
    int32_t duration_ms = 0;
    // base64 ThumbHash supplied by the uploader in /init. Opaque to the
    // server — stored and echoed verbatim, never decoded here. Empty for
    // non-image kinds and pre-thumbhash uploads.
    std::string placeholder;
    // Encrypted-channel upload: bytes and thumbnails are the uploader's
    // ciphertext; metadata here is a stand-in (spec 2026-09-04).
    bool encrypted = false;
};

// Returned from prune_attachments so the server can broadcast tombstone
// updates to live clients and unlink blobs from disk.
struct PurgedAttachmentInfo {
    int64_t attachment_id = 0;
    int64_t message_id = 0;
    int64_t purged_at = 0;
    std::string storage_path; // path that needs to be unlink()'d from disk
};

struct DbBan {
    int64_t uid = 0;           // negative: a name-only ban, matched by `username` until claimed
    std::string username;      // their name when banned
    int64_t banned_at = 0;
    int64_t banned_by_uid = 0;
    std::string banned_by;
    std::string reason;
    int64_t expires_at = 0;   // 0 = permanent
};

// Storage-usage aggregation for the Storage settings tab. All byte counts
// are computed from the DB (SUM over live, non-purged rows), never a
// filesystem walk. The volume total/available and DB file size are filled in
// by the caller (main.cpp) via std::filesystem.
struct StorageUsage {
    int64_t attachments_bytes = 0;   // SUM(size_bytes) where purged_at=0
    int64_t thumbnails_bytes = 0;    // SUM(thumbnail_size_bytes) where purged_at=0
    int64_t attachment_count = 0;    // COUNT(*) where purged_at=0
    struct Kind    { int32_t kind = 0; int64_t bytes = 0; int64_t count = 0; };
    struct Channel { std::string channel_id; int64_t bytes = 0; int64_t count = 0; };
    struct Largest { int64_t id = 0; std::string filename; int64_t size_bytes = 0;
                     std::string channel_id; int32_t kind = 0; };
    std::vector<Kind> by_kind;        // any order
    std::vector<Channel> by_channel;  // highest bytes first
    std::vector<Largest> largest;     // highest bytes first, capped by caller
};

struct DbAuditEntry {
    int64_t id = 0;
    int64_t timestamp = 0;
    int64_t actor_uid = 0;     // 0 for entries from before users by uid
    std::string actor;         // the actor's name at the time
    std::string action;
    int64_t target_uid = 0;    // set when the target is a member
    std::string target;
    std::string channel_id;
    std::string details;
};

// Mirrors chatproj::RoleInfo. `position` is a dense hierarchy index:
// 0 is reserved for the default `everyone` role, real roles occupy 1..N
// contiguously (create/delete/move keep them dense).
struct DbRole {
    int64_t id = 0;
    std::string name;
    uint32_t color = 0;        // 0xRRGGBB, 0 = no color
    int32_t position = 0;
    uint64_t permissions = 0;  // chatproj::Permission bitfield
    bool is_default = false;
    // Members whose highest role is this one may manage each other
    // (nicknames, voice moderation) — permissions v3.
    bool manage_each_other = false;
};

// Permission bits — mirror of chatproj::Permission (kept here so the DB
// layer doesn't depend on the generated protobuf headers).
namespace perms {
constexpr uint64_t kAdministrator   = 1ull << 0;
constexpr uint64_t kManageServer    = 1ull << 1;
constexpr uint64_t kManageChannels  = 1ull << 2;
constexpr uint64_t kManageRoles     = 1ull << 3;
constexpr uint64_t kKickMembers     = 1ull << 4;
constexpr uint64_t kBanMembers      = 1ull << 5;
constexpr uint64_t kManageMessages  = 1ull << 6;
constexpr uint64_t kManageInvites   = 1ull << 7;
constexpr uint64_t kManageNicknames = 1ull << 8;
constexpr uint64_t kSendMessages    = 1ull << 10;
constexpr uint64_t kConnectVoice    = 1ull << 11;
constexpr uint64_t kStream          = 1ull << 12;
// permissions v2 — per-channel overwrites
constexpr uint64_t kViewChannel     = 1ull << 13;
constexpr uint64_t kReadHistory     = 1ull << 14;
constexpr uint64_t kAttachFiles     = 1ull << 15;
// server management + moderation
constexpr uint64_t kViewAuditLog    = 1ull << 16;
constexpr uint64_t kModerateMembers = 1ull << 17;   // timeouts
// permissions v3 — docs/superpowers/specs/2026-10-09-permissions-v3-design.md
constexpr uint64_t kMuteMembers     = 1ull << 18;   // was VOICE_MODERATE
constexpr uint64_t kDeafenMembers   = 1ull << 19;
constexpr uint64_t kMoveMembers     = 1ull << 20;   // move + disconnect
constexpr uint64_t kCreateInvite    = 1ull << 21;
constexpr uint64_t kChangeNickname  = 1ull << 22;
constexpr uint64_t kEmbedLinks      = 1ull << 23;
constexpr uint64_t kSpeak           = 1ull << 24;
constexpr uint64_t kAll             = ~0ull;
// Every currently-defined permission bit. Role create/update mask
// client-supplied bitfields with this so undefined bits never reach the
// DB — a new bit becomes storable exactly when a server build that
// defines (and enforces) it ships. Keeps stored bitfields small enough
// that JS clients can treat them as plain numbers, and stops a crafted
// client from parking garbage in the high bits.
constexpr uint64_t kKnownMask =
    kAdministrator | kManageServer | kManageChannels | kManageRoles |
    kKickMembers | kBanMembers | kManageMessages | kManageInvites |
    kManageNicknames | kSendMessages | kConnectVoice | kStream |
    kViewChannel | kReadHistory | kAttachFiles |
    kViewAuditLog | kModerateMembers |
    kMuteMembers | kDeafenMembers | kMoveMembers |
    kCreateInvite | kChangeNickname | kEmbedLinks | kSpeak;
// What the seeded `everyone` role starts with: every "ordinary member"
// bit on, so a fresh server behaves like one without permissions until
// an operator tightens something. Migrations v6 / v11 OR the bits that
// arrived later into an existing `everyone` once.
constexpr uint64_t kV2Bits = kViewChannel | kReadHistory | kAttachFiles;
constexpr uint64_t kV3Bits = kCreateInvite | kChangeNickname | kEmbedLinks | kSpeak;
constexpr uint64_t kDefaultEveryone =
    kSendMessages | kConnectVoice | kStream | kV2Bits | kV3Bits;
} // namespace perms

// Bounds a user-supplied display string (role/channel name, nickname) to
// at most `max_bytes` WITHOUT cutting through a UTF-8 sequence, and strips
// ASCII control characters (incl. CR/LF, which would corrupt log lines and
// any future audit rendering). A plain std::string::resize() used to be
// applied here: it can leave a dangling lead byte, C++ protobuf still
// serialises that (it only logs), and the client's prost decoder then
// rejects the WHOLE packet — one 33-byte emoji nickname made every
// MEMBER_LIST_RES undecodable for every client, and a long channel
// rename poisoned COMMUNITY_AUTH_RES (it embeds the channel list) so
// nobody could authenticate until the DB was hand-edited.
inline std::string clamp_utf8(const std::string& in, size_t max_bytes) {
    std::string out;
    out.reserve(in.size() < max_bytes ? in.size() : max_bytes);
    for (unsigned char c : in) {
        if (c < 0x20 || c == 0x7F) continue;
        out.push_back(static_cast<char>(c));
    }
    if (out.size() <= max_bytes) return out;
    // out[max_bytes] is the first byte we drop. If it is a continuation
    // byte (10xxxxxx) the sequence it belongs to straddles the boundary:
    // walk back to that sequence's lead byte and drop the whole sequence.
    // (Input is valid UTF-8 — protobuf rejects invalid proto3 strings at
    // parse time — so this is the only way the cut can land mid-sequence.)
    size_t cut = max_bytes;
    while (cut > 0 && (static_cast<unsigned char>(out[cut]) & 0xC0) == 0x80) --cut;
    out.resize(cut);
    return out;
}

// Byte caps shared by the wire handlers and the DB layer.
constexpr size_t kMaxRoleNameBytes    = 64;
constexpr size_t kMaxChannelNameBytes = 64;
constexpr size_t kMaxNicknameBytes    = 32;

// Default upload headroom: refuse uploads that would leave the store's
// volume with less than this much free space, until an operator changes it
// (env seed / Storage tab). 2 GiB.
constexpr int64_t kDefaultMinFreeBytes = 2LL * 1024 * 1024 * 1024;

// One per-channel permission overwrite (Discord model). target_type 0 =
// role (target_id = role id as decimal text), 1 = member (uid as decimal
// text).
struct DbOverwrite {
    std::string channel_id;
    int32_t target_type = 0;
    std::string target_id;
    uint64_t allow = 0;
    uint64_t deny = 0;
};

// Reason an invite cannot be redeemed.
enum class InviteResult {
    Ok,
    Unknown,
    Expired,
    Exhausted,
    Banned,
    AlreadyMember,
    NotForYou       // a personal invite for someone else
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
    // adds the owner to members — under a legacy placeholder uid, claimed
    // when the owner's token first authenticates (the operator configures
    // a name). Returns false on unrecoverable errors.
    bool open(const std::string& path,
              const std::string& owner_username,
              const std::string& server_name,
              const std::string& server_description);

    // --- server_meta ---
    // The owner's uid (negative until a fresh server's owner first connects).
    int64_t owner_uid() const;
    std::string server_name() const;
    std::string server_description() const;
    // In-app server rename (MANAGE_SERVER). The DB is the source of truth;
    // env vars only seed a fresh DB.
    void set_server_meta(const std::string& name, const std::string& description);
    // Public listing (MANAGE_SERVER): when true the server opts into the
    // central discovery directory and accepts invite-less joins; when false
    // it's hidden and invite-only. Persisted in server_meta; defaults false
    // (invite-only) on a fresh/upgraded DB.
    bool public_listing() const;
    void set_public_listing(bool on);
    // Ownership transfer: new owner must be a member. Invalidates all
    // permission caches (owner is resolved from server_meta).
    bool transfer_ownership(int64_t new_owner_uid);

    // --- auto-rejoin: cached central-assigned server_id ---
    // Survives community restarts so we don't depend on a fresh
    // heartbeat landing before the first user authenticates after
    // restart. Returns 0 if not yet learned.
    int64_t central_server_id() const;
    void set_central_server_id(int64_t id);
    // TOFU pin of central's TLS certificate (sha256-hex); '' = not yet seen.
    std::string central_cert_fingerprint() const;
    void set_central_cert_fingerprint(const std::string& fp);

    // --- membership ---
    bool is_member(int64_t uid) const;
    // Records a new membership. True only if a row was actually inserted.
    bool add_member(int64_t uid, const std::string& username, int64_t name_iat = 0);
    // A legacy placeholder for `username` (a name seen before uids reached
    // this server, or a fresh server's configured owner) becomes `uid` in
    // every table, in one transaction. Only when `uid` has no member row
    // yet. True if a placeholder was claimed.
    bool claim_legacy(int64_t uid, const std::string& username);
    // The member's token carries another name than their row (renamed at
    // central): store the new one. Returns the previous name, or nullopt
    // when unchanged / not a member.
    /// `iat` is the token's issue time: a token older than the one that
    /// last set the name can't change it back (a device that held its
    /// token across a rename). `current` gets the name the row holds after.
    std::optional<std::string> refresh_member_name(int64_t uid, const std::string& username,
                                                   int64_t iat, std::string* current = nullptr);
    bool remove_member(int64_t uid);
    std::vector<DbMember> list_members() const;

    // --- Encrypted text channels (spec 2026-09-04) ---
    // Text channels only; the caller clamps the length.
    bool set_channel_topic(const std::string& channel_id, const std::string& topic);
    // Current epoch of a channel's key escrow; 0 = none yet.
    uint32_t channel_key_current_epoch(const std::string& channel_id) const;
    // Create `epoch` (must be current + 1). False when the sequence is wrong.
    bool channel_key_create_epoch(const std::string& channel_id, uint32_t epoch,
                                  int64_t created_by_uid, int64_t now);
    // One blob to store: its recipient and the names it is sealed under.
    struct KeyBlobIn {
        int64_t recipient_uid = 0;
        std::string recipient;
        std::string blob;
    };
    // Insert blobs that don't exist yet for (epoch, recipient); returns how many landed.
    int channel_key_insert_blobs(const std::string& channel_id, uint32_t epoch,
                                 int64_t sender_uid, const std::string& sender,
                                 const std::vector<KeyBlobIn>& blobs,
                                 int64_t now);
    // Every blob sealed to `recipient_uid` for the channel (all epochs).
    std::vector<DbChannelKeyBlob> channel_key_blobs_for(const std::string& channel_id,
                                                        int64_t recipient_uid) const;
    // Epochs (1..=current) `uid` has no blob for.
    std::vector<uint32_t> channel_key_missing_epochs(const std::string& channel_id,
                                                     int64_t uid) const;
    // Accounts holding a blob for `epoch`.
    std::vector<int64_t> channel_key_holders(const std::string& channel_id, uint32_t epoch) const;
    // Drop every blob sealed to `recipient_uid` (member removed).
    void channel_key_delete_recipient(int64_t recipient_uid);
    // Text channels with a non-zero epoch (for removal-driven rotations).
    std::vector<std::string> channel_key_channels_with_epochs() const;
    std::optional<DbMember> get_member(int64_t uid) const;
    // The member currently known by `username` — for requests that name a
    // target by name (older clients, typed names). Never an identity check.
    std::optional<DbMember> find_member_by_name(const std::string& username) const;
    int64_t count_members() const;
    // Timeout (unix seconds; 0 clears). False if not a member.
    bool set_timeout(int64_t uid, int64_t until);
    // Moderator voice flags, persisted across sessions.
    bool set_server_voice_flags(int64_t uid, bool muted, bool deafened);

    // --- bans ---
    // Expired bans (expires_at != 0 && <= now) count as not banned and are
    // lazily deleted. A ban matches by uid; a name-only ban (negative uid:
    // legacy, or a name never seen here) matches `username` until that
    // name's first token claims it.
    bool is_banned(int64_t uid, const std::string& username) const;
    std::optional<DbBan> get_ban(int64_t uid) const;
    // Removes the membership (roles, overwrites) and records the ban.
    bool add_ban(int64_t uid, const std::string& username,
                 int64_t banned_by_uid, const std::string& banned_by,
                 const std::string& reason,
                 int64_t expires_at = 0);
    bool remove_ban(int64_t uid);
    std::vector<DbBan> list_bans() const;
    // The placeholder uid for a name nobody here has a uid for yet (a ban
    // of a never-seen name), allocated on first use.
    int64_t placeholder_for_name(const std::string& username);

    // --- invites ---
    // Generates a code and inserts a new invite. Returns the created invite on
    // success, or std::nullopt on failure.
    std::optional<DbInvite> create_invite(int64_t created_by_uid,
                                          int64_t expires_at,
                                          int32_t max_uses,
                                          int64_t for_uid = 0,
                                          const std::string& for_user = "");
    // A still-redeemable personal invite `created_by_uid` made for
    // `for_uid`, so asking again hands out the same one instead of piling
    // up more.
    std::optional<DbInvite> find_personal_invite(int64_t created_by_uid, int64_t for_uid) const;
    std::vector<DbInvite> list_invites() const;
    bool revoke_invite(const std::string& code);

    // Atomically validates and consumes an invite (increments uses, deletes if
    // exhausted). On success, `out_invite` is populated. Does NOT add the
    // redeeming user to members — the caller does that after the connection is
    // accepted.
    InviteResult redeem_invite(const std::string& code,
                               int64_t redeeming_uid,
                               const std::string& redeeming_name,
                               DbInvite* out_invite);

    // --- roles + permissions ---
    // Roles ordered most-senior first (position DESC, id ASC); the
    // default `everyone` role (position 0) comes last.
    std::vector<DbRole> list_roles() const;
    std::optional<DbRole> get_role(int64_t role_id) const;
    // Inserts at position 1 (just above `everyone`), shifting existing
    // roles up. Returns the created role, or nullopt on failure.
    std::optional<DbRole> create_role(const std::string& name,
                                      uint32_t color,
                                      uint64_t permissions,
                                      bool manage_each_other = false);
    // Full-snapshot update. For the default role only `permissions` and
    // `manage_each_other` are applied (name/color/position are fixed). `position` is clamped to
    // [1, N] and the move keeps positions dense. Returns false if the
    // role doesn't exist or on DB error.
    bool update_role(int64_t role_id,
                     const std::string& name,
                     uint32_t color,
                     uint64_t permissions,
                     int32_t position,
                     bool manage_each_other = false);
    // Refuses the default role. Cascades member_roles rows and closes
    // the position gap. Returns false if missing/default/DB error.
    bool delete_role(int64_t role_id);
    // Assigned role ids for one member (default role not included).
    std::vector<int64_t> get_member_role_ids(int64_t uid) const;
    // (uid, assigned role id) pairs, for building member lists in one query.
    std::vector<std::pair<int64_t, int64_t>> list_all_member_roles() const;
    // role id -> how many members hold it (roles nobody holds are absent).
    std::unordered_map<int64_t, int64_t> role_member_counts() const;
    // Replaces the member's role set. Caller validates hierarchy; this
    // only checks that every id references an existing non-default role.
    bool set_member_roles(int64_t uid, const std::vector<int64_t>& role_ids);
    // OR of `everyone` + the member's roles. Owner → perms::kAll.
    // A set kAdministrator bit expands to perms::kAll.
    uint64_t effective_permissions(int64_t uid) const;
    bool has_permission(int64_t uid, uint64_t perm) const;
    // Hierarchy level: highest assigned role position (0 with no roles).
    // Owner → INT32_MAX. Used for "can only act below yourself" checks.
    int32_t member_level(int64_t uid) const;
    // Peers (permissions v3): both members' highest role is the same role
    // (same level — positions are unique; level 0 = `everyone`) and it has
    // manage_each_other set. Never true for the owner or for oneself.
    bool are_peers(int64_t a, int64_t b) const;

    // --- permissions v2: per-channel overwrites ---
    // See docs/superpowers/specs/2026-08-22-permissions-v2-design.md.
    // The rows that APPLY to `channel_id`: its category's when the channel
    // is synced to one (permissions v3), else its own. Row channel_id is
    // the source channel's.
    std::vector<DbOverwrite> list_overwrites(const std::string& channel_id) const;
    // Upserts; allow == deny == 0 deletes the row. Caller validates
    // authorization; this only checks the channel/target exist. Bits are
    // masked to perms::kKnownMask and a bit set in both allow and deny
    // is treated as deny. On a synced channel the category's rows are
    // first copied onto it; any write leaves a non-category channel
    // unsynced.
    bool set_overwrite(const DbOverwrite& ow);

    // --- permissions v3: category sync ---
    // Nothing but an explicit sync (or creating a channel inside a syncing
    // category) ever makes a channel start following a category: moves,
    // turning a category's sync off and deleting a category all leave the
    // channel with the rows it was following, as its own.
    //
    // Nearest CATEGORY row above the channel ("" when uncategorized or a
    // category itself).
    std::string parent_category(const std::string& channel_id) const;
    // Drops the channel's own rows and marks it synced. False when the
    // channel is unknown / a category / has no parent category / the
    // parent doesn't sync.
    bool sync_channel_permissions(const std::string& channel_id);
    // Turns a category's sync on / off. Off: its synced channels get its
    // rows as their own first. On: nothing changes until channels sync.
    bool set_category_sync(const std::string& category_id, bool on);
    // Non-category channels under `category_id`, display order.
    std::vector<std::string> category_children(const std::string& category_id) const;
    // Undo for sync_channel_permissions: restores `rows` as the channel's
    // own and unsyncs it (lock-out guard revert).
    bool restore_channel_overwrites(const std::string& channel_id,
                                    const std::vector<DbOverwrite>& rows);
    // Synced channels whose source is `category_id`.
    std::vector<std::string> synced_children(const std::string& category_id) const;

    // --- permissions v3: voice passes ---
    // A member moved into a voice channel they can't otherwise view /
    // join holds VIEW_CHANNEL + CONNECT there while in it. Runtime only
    // (never persisted); the session layer grants on move and revokes
    // when the user's last session leaves the channel.
    void grant_voice_pass(int64_t uid, const std::string& channel_id);
    // True if a pass was actually removed.
    bool revoke_voice_pass(int64_t uid, const std::string& channel_id);
    bool has_voice_pass(int64_t uid, const std::string& channel_id) const;
    // The user's resolved permissions in `channel_id`:
    //   base (effective_permissions) → everyone overwrite → OR of role
    //   overwrites → member overwrite; owner / ADMINISTRATOR bypass
    //   overwrites; a result without VIEW_CHANNEL collapses to 0.
    // Unknown channel → 0. Cached per (user, channel).
    uint64_t channel_permissions(int64_t uid, const std::string& channel_id) const;
    bool has_channel_permission(int64_t uid, const std::string& channel_id, uint64_t perm) const;

    // --- channels ---
    std::vector<DbChannel> list_channels() const;

    // Creates a channel/category. The id is a filesystem-safe slug of
    // the name, made unique with a numeric suffix on collision (it
    // doubles as the attachment directory name and is immutable after
    // creation).
    //
    // Placement (positions stay dense, later rows shift):
    //  - type CATEGORY (2): always appended at the very end.
    //  - channel with category_id "": end of the uncategorized area
    //    (just before the first CATEGORY row, or list end if none).
    //  - channel with a category_id: end of that category's block
    //    (just before the next CATEGORY row after it, or list end).
    // Returns the created row, or nullopt on invalid input / unknown
    // category / DB error.
    std::optional<DbChannel> create_channel(const std::string& name,
                                            int32_t type,
                                            int32_t voice_bitrate_kbps,
                                            const std::string& category_id,
                                            bool encrypted = false);
    // Rewrites positions to match `ordered_ids` (0..N-1) in one
    // transaction. Fails without touching anything unless the id set
    // exactly matches the current channels table. The stored order is
    // then normalized (text above voice within each group). A synced
    // channel that ends up under another category (or none) keeps the
    // rows it was following, as its own, unsynced.
    bool reorder_channels(const std::vector<std::string>& ordered_ids);
    // Display-name change only — the id/slug stays. False if the
    // channel doesn't exist or the name is empty.
    bool rename_channel(const std::string& channel_id,
                        const std::string& name);
    // How many channels of `type` exist (0 text / 1 voice). Guards the
    // "can't delete the last text channel" rule.
    int64_t count_channels_of_type(int32_t type) const;
    // delete_channel lives below WipeChannelResult's declaration.

    // --- nicknames ---
    // Sets (empty = clears) a member's nickname. False if not a member.
    bool set_nickname(int64_t uid, const std::string& nickname);

    // Update all five retention values at once. Returns false if the channel
    // doesn't exist or on DB error. Negative values are clamped to 0.
    bool update_channel_retention(const std::string& channel_id,
                                  int32_t text_days,
                                  int32_t image_days,
                                  int32_t video_days,
                                  int32_t document_days,
                                  int32_t audio_days);
    // Slowmode for a text channel (seconds; 0 = off; clamped to 21600).
    bool set_channel_slowmode(const std::string& channel_id, int32_t seconds);
    // Set a voice channel's Opus bitrate (kbps; 0 = client default).
    // Clamped to [0, 512]. Returns false if the channel doesn't exist,
    // isn't a voice channel, or on DB error.
    bool set_channel_voice_bitrate(const std::string& channel_id, int32_t kbps);
    // Fetch a single channel with retention populated. nullopt if unknown.
    std::optional<DbChannel> get_channel(const std::string& channel_id) const;

    // --- messages ---
    // Insert a new message, return its autoincrement id (0 on failure).
    // `reply_to` is the parent message id (0 = not a reply); it's stored only
    // if it points at an existing message in the same channel, else 0.
    // `sender_uid` is the sender's account (what search, edits, deletes
    // and purges key on); `sender` the name it is sent under.
    int64_t insert_message(const std::string& channel_id,
                           int64_t sender_uid,
                           const std::string& sender,
                           const std::string& content,
                           int64_t timestamp,
                           int64_t reply_to = 0,
                           const std::string& envelope = std::string(),
                           bool suppress_embeds = false);
    // Newest-first page. `before_id = 0` means "most recent". Results are
    // ordered newest→oldest; caller reverses if they want oldest→newest.
    // `has_more` is set to true if more messages exist older than the page.
    std::vector<DbMessage> fetch_messages(const std::string& channel_id,
                                          int64_t before_id,
                                          int32_t limit,
                                          bool* has_more) const;
    // Jump-to-message context window, ordered OLDEST→newest: up to `limit`
    // messages with id <= around_id (including it) plus up to `limit` with
    // id > around_id. `has_more_before` / `has_more_after` report whether more
    // exist beyond each edge of the window.
    std::vector<DbMessage> fetch_messages_around(const std::string& channel_id,
                                                 int64_t around_id,
                                                 int32_t limit,
                                                 bool* has_more_before,
                                                 bool* has_more_after) const;
    // Downward pagination, ordered OLDEST→newest: up to `limit` messages with
    // id > after_id. `has_more_after` = more newer than the page exist.
    std::vector<DbMessage> fetch_messages_after(const std::string& channel_id,
                                                int64_t after_id,
                                                int32_t limit,
                                                bool* has_more_after) const;
    // Message search: the rows behind a result page, in the order given.
    // Ids that no longer exist are skipped. At most 200.
    std::vector<DbMessage> fetch_messages_by_ids(const std::vector<int64_t>& ids) const;
    // Message search (spec 2026-10-14): the FTS index exists and is kept
    // current. False when the SQLite library lacks FTS5 / contentless-delete
    // (< 3.43); searches then answer "isn't available".
    bool search_available() const { return search_available_; }
    // Load all attachments belonging to any of the given message ids.
    // Ordered by (message_id ASC, position ASC).
    std::vector<DbAttachment> fetch_attachments_for_messages(
        const std::vector<int64_t>& message_ids) const;

    // --- attachment uploads ---
    // Create a pending attachment row. Returns the new id (0 on failure).
    // message_id is 0 until CHANNEL_MSG binds it; status is 'uploading'
    // until complete_attachment is called.
    int64_t insert_pending_attachment(const std::string& channel_id,
                                      int32_t kind,
                                      const std::string& filename,
                                      const std::string& mime,
                                      int64_t expected_size,
                                      const std::string& storage_path,
                                      int64_t uploader_uid,
                                      int32_t position,
                                      int32_t width,
                                      int32_t height,
                                      int32_t duration_ms,
                                      const std::string& placeholder,
                                      bool encrypted = false);
    std::optional<DbAttachment> get_attachment(int64_t attachment_id) const;
    // Set/overwrite the storage_path for an attachment. Used at upload init:
    // we need the row's autoincrement id to build the final path, so the
    // row is inserted with a placeholder and then updated once we know the id.
    bool update_attachment_storage_path(int64_t attachment_id,
                                        const std::string& path);
    // Called by POST /attachments/:id/complete. Marks the row ready and
    // writes the final size. Only succeeds on rows in 'uploading' state.
    bool complete_attachment(int64_t attachment_id, int64_t final_size);
    // Abort a pending upload — deletes the row and returns the storage_path
    // so the caller can unlink the partial file on disk.
    std::optional<std::string> abort_pending_attachment(int64_t attachment_id);
    // Record the size of a JPEG thumbnail saved next to the main attachment
    // file. Called from POST /attachments/:id/thumbnail after the bytes are
    // on disk. Setting size=0 effectively clears the flag. Legacy single-
    // size path; new uploads call add_attachment_thumbnail_size instead.
    bool set_attachment_thumbnail_size(int64_t attachment_id, int64_t size);
    // Adds a pre-generated thumbnail at a specific size (one of 320/640/
    // 1280 px long-edge → bit 0/1/2 of thumbnail_sizes_mask). Adds the
    // bytes to thumbnail_size_bytes and OR-s the bit. Idempotent across
    // re-uploads of the same size (bit doesn't double-count, but bytes
    // get adjusted to the new size).
    bool add_attachment_thumbnail_size(int64_t attachment_id,
                                       int32_t size_bit,
                                       int64_t bytes);
    // Bind previously-uploaded attachments to a message. Only attachments
    // that are 'ready', uploaded by `uploader_uid`, currently unbound
    // (message_id=0), and belong to `channel_id` get bound. Returns the
    // list of ids that were successfully bound.
    std::vector<int64_t> bind_attachments(const std::vector<int64_t>& attachment_ids,
                                          int64_t message_id,
                                          const std::string& channel_id,
                                          int64_t uploader_uid,
                                          bool require_encrypted = false);
    // Unbound attachments (message_id=0) the retention sweep should clean
    // up, with a cutoff per lifecycle state (created_at in unix seconds):
    //   - 'uploading' rows idle since before `uploading_cutoff_ts` (no
    //     PATCH landed; see touch_attachment_activity) — the client
    //     crashed or gave up mid-upload. Idle, not old: a slow (capped)
    //     or paused upload that is still alive must not be swept;
    //   - 'ready' rows older than `ready_cutoff_ts` — uploaded but never
    //     referenced by a CHANNEL_MSG. Kept on a much longer leash than
    //     uploading rows, because a finished upload may legitimately sit
    //     in a compose box for a while before the message is sent.
    // Returns the rows themselves so the caller can unlink the .partial /
    // final files before the rows are deleted.
    std::vector<DbAttachment> list_stale_pending_attachments(
        int64_t uploading_cutoff_ts, int64_t ready_cutoff_ts) const;
    bool delete_attachment_row(int64_t attachment_id);
    // Stamp upload progress (each accepted PATCH) for the sweep above.
    void touch_attachment_activity(int64_t attachment_id, int64_t ts);

    // --- owner-initiated wipe ---
    // Result of a full channel wipe — the count fields are returned to
    // the calling client for confirmation feedback, the unlink_paths
    // are filesystem cleanup the server applies after the DB commit.
    struct WipeChannelResult {
        int64_t deleted_message_count = 0;
        int64_t deleted_attachment_count = 0;
        std::vector<std::string> unlink_paths;
    };

    // Wipes the channel's messages + attachments (same shape as
    // wipe_channel) and removes the channel row, in one transaction.
    // Returns the unlink list for filesystem cleanup; nullopt if the
    // channel doesn't exist or on DB error.
    std::optional<WipeChannelResult> delete_channel(const std::string& channel_id);

    // --- per-message delete ---
    // (see docs/superpowers/specs/2026-05-15-message-deletion-design.md)
    struct DeleteMessageResult {
        bool ok = false;
        std::vector<std::string> unlink_paths;
    };

    /// The sender of the given message in this channel (the account and
    /// the name it was sent under), or nullopt if no such row exists.
    struct MessageSender {
        int64_t uid = 0;
        std::string username;
    };
    std::optional<MessageSender> get_message_sender(
        const std::string& channel_id, int64_t message_id) const;

    struct MessagePreview {
        int64_t sender_uid = 0;
        std::string sender;        // current name
        std::string sent_sender;   // the name it was sent under
        std::string content;
        std::vector<int32_t> attachment_kinds;  // Attachment.Kind, position order
        std::string envelope;                   // encrypted channels
    };
    /// Parent-preview fields of the given message in this channel, or nullopt
    /// if no such row exists. Used to validate a reply_to and embed the parent
    /// preview on the CHANNEL_MSG broadcast.
    std::optional<MessagePreview> get_message_preview(
        const std::string& channel_id, int64_t message_id) const;

    /// Edits a message's content in place, enforcing ownership in SQL (the
    /// row's sender_uid must be `editor_uid`). Stamps edited_at. Returns
    /// true only if a row was updated (exists, in this channel, owned by
    /// editor).
    bool edit_message(const std::string& channel_id, int64_t message_id,
                      int64_t editor_uid,
                      const std::string& content,
                      int64_t edited_at,
                      const std::string& envelope = std::string(),
                      bool suppress_embeds = false);

    /// Hard-deletes the message + its bound attachments in one
    /// transaction. Returns storage_paths the caller should unlink
    /// from disk (matches WipeChannelResult.unlink_paths pattern).
    DeleteMessageResult delete_message(
        const std::string& channel_id, int64_t message_id);

    /// Forward-compat permission helper: the owner today.
    bool can_delete_others(int64_t uid) const;
    // Delete every message + attachment row for `channel_id`, returning
    // the counts and the storage paths whose blobs (and thumbnails) the
    // server should unlink. Wrapped in a transaction so a failure mid-way
    // leaves the channel intact.
    WipeChannelResult wipe_channel(const std::string& channel_id);

    // --- ban purge ---
    // Deletes every message the account posted since `since_ts` (plus
    // their attachment rows), by uid, so a rename can't shield them.
    // Returns (channel_id, message_id) pairs for CHANNEL_MESSAGE_DELETED
    // broadcasts and blob paths to unlink.
    struct PurgedMessages {
        std::vector<std::pair<std::string, int64_t>> messages;
        std::vector<std::string> unlink_paths;
    };
    PurgedMessages delete_messages_by_sender_since(int64_t sender_uid, int64_t since_ts);

    // --- storage ---
    // Aggregates this community's attachment footprint from the DB.
    // `max_channels` / `max_largest` cap the by_channel / largest lists
    // (<=0 = omit that list). Cheap: three grouped SUM queries + one
    // ORDER BY … LIMIT, all over the attachments table.
    StorageUsage storage_usage(int max_channels, int max_largest) const;
    // Minimum free bytes to keep on the store's volume — uploads that would
    // cross it are refused (507). Persisted in server_meta; a fresh DB
    // returns the built-in default until set. set_min_free_bytes clamps < 0
    // to 0 and returns the stored value.
    int64_t min_free_bytes() const;
    int64_t set_min_free_bytes(int64_t bytes);

    // --- audit log ---
    // `actor` / `target` are names at the time (kept as history); the uids
    // are 0 when not a user (a role, an invite code, …).
    void add_audit(int64_t actor_uid, const std::string& actor, const std::string& action,
                   int64_t target_uid, const std::string& target,
                   const std::string& channel_id, const std::string& details);
    // Newest first; `before_id` 0 = newest. Limit clamped to [1, 100].
    // `query` (empty = any) matches actor, target, action, details, the
    // channel's name, and the nickname or role name behind actor/target,
    // case-insensitively for ASCII. `actions` (empty = any) keeps only
    // those verbs.
    std::vector<DbAuditEntry> list_audit(int64_t before_id, int32_t limit,
                                         bool* has_more,
                                         const std::string& query = "",
                                         const std::vector<std::string>& actions = {}) const;
    // Drops entries older than `cutoff_ts`.
    void prune_audit(int64_t cutoff_ts);

    // --- retention pruning ---
    // Delete messages in `channel_id` whose timestamp is strictly older than
    // `cutoff_ts`. Returns (deleted_message_ids, storage_paths_of_remaining_
    // attachments_that_need_unlink). CASCADE handles attachment rows; the
    // caller handles filesystem cleanup of blobs.
    struct PrunedTextResult {
        std::vector<int64_t> deleted_ids;
        std::vector<std::string> unlink_paths;
    };
    PrunedTextResult prune_text_messages(const std::string& channel_id,
                                         int64_t cutoff_ts);
    // Soft-delete attachments in `channel_id` of `kind` whose created_at is
    // strictly older than `cutoff_ts`. Sets storage_path=NULL, purged_at=now,
    // returns metadata needed to broadcast tombstones + unlink blobs.
    std::vector<PurgedAttachmentInfo> prune_attachments(
        const std::string& channel_id,
        int32_t kind,
        int64_t cutoff_ts);

private:
    void init_schema_();
    void migrate_to_v2_();
    void migrate_to_v5_roles_();
    // Enforces the display invariant "text channels above voice
    // channels within each group" (the uncategorized area and every
    // category block), preserving relative order within each kind, and
    // rewrites positions densely (0..N-1). Runs its own transaction;
    // caller holds mutex_ and must NOT be inside a transaction.
    void normalize_channel_order_();
    // Unlocked internals shared by the public role API (mutex_ held by caller).
    std::optional<DbRole> get_role_unlocked_(int64_t role_id) const;
    int32_t max_role_position_unlocked_() const;
    uint64_t effective_permissions_unlocked_(int64_t uid) const;
    // Per-user (effective permissions, hierarchy level), computed once and
    // cached until any role / member-role / membership / owner change
    // (invalidate_perm_cache_ or targeted erase). has_permission() runs on
    // every roster broadcast per online user and on the hot message path
    // once SEND_MESSAGES is enforced, so it must not hit SQLite each time.
    // manage_peers: the highest role (everyone at level 0) has
    // manage_each_other.
    struct PermEntry { uint64_t permissions = 0; int32_t level = 0; bool manage_peers = false; };
    const PermEntry& perm_entry_unlocked_(int64_t uid) const;
    void invalidate_perm_cache_();
    // Targeted: one user's server-wide + per-channel entries.
    void invalidate_user_perms_(int64_t uid);
    mutable std::unordered_map<int64_t, PermEntry> perm_cache_;
    // (user, channel) → resolved channel permissions. Cleared with
    // perm_cache_ and on any overwrite / channel change.
    mutable std::unordered_map<int64_t,
        std::unordered_map<std::string, uint64_t>> channel_perm_cache_;
    // channel → overwrites, loaded lazily; cleared on overwrite writes,
    // channel delete, role delete, member removal.
    mutable std::unordered_map<std::string, std::vector<DbOverwrite>> overwrites_cache_;
    const std::vector<DbOverwrite>& overwrites_unlocked_(const std::string& channel_id) const;
    // Channel whose overwrite rows apply to `channel_id` (its category
    // when synced to one, else itself). Cached; cleared with
    // invalidate_channel_layout_().
    std::string overwrite_source_unlocked_(const std::string& channel_id) const;
    std::string parent_category_unlocked_(const std::string& channel_id) const;
    void rebuild_channel_layout_unlocked_() const;
    // perm_synced only where the parent category syncs (open + after any
    // layout change). Caller holds the mutex, no open transaction.
    void enforce_sync_invariant_unlocked_();
    // Order / parent / sync changed: drop the layout + every resolution.
    void invalidate_channel_layout_();
    mutable bool layout_valid_ = false;
    mutable std::unordered_map<std::string, std::string> parent_of_;   // channel → category ("" none)
    mutable std::unordered_map<std::string, bool> synced_of_;
    mutable std::unordered_map<std::string, bool> category_sync_of_;
    // uid → channels they hold a voice pass for.
    std::unordered_map<int64_t, std::set<std::string>> voice_passes_;
    uint64_t channel_permissions_unlocked_(int64_t uid, const std::string& channel_id) const;
    void migrate_to_v6_overwrites_();
    void migrate_to_v7_moderation_();
    void migrate_to_v8_uid_();
    void migrate_to_v9_e2ee_();
    void migrate_to_v10_upload_activity_();
    void migrate_to_v11_permissions_v3_();
    void migrate_to_v12_category_sync_(int prior_version);
    void migrate_to_v13_topics_invites_();
    void migrate_to_v14_search_();
    // Users by uid (2026-10-15-uid-identity-design.md).
    void migrate_to_v15_uid_keys_();
    void migrate_to_v16_name_iat_();
    // Next free placeholder (negative) uid; mutex held.
    int64_t next_placeholder_unlocked_();
    // Creates messages_fts + its triggers and backfills it (once).
    bool create_search_index_();
    bool search_available_ = false;
    // server_meta.owner_uid, loaded at open() and kept in sync by set_meta_.
    int64_t owner_cache_ = 0;
    void seed_if_empty_(const std::string& owner,
                        const std::string& name,
                        const std::string& desc);
    // INSERT OR IGNOREs the canonical default channel set on every
    // startup. New defaults added to the table show up on existing
    // server DBs without touching any channels the operator already
    // renamed/repositioned (the IGNORE clause skips collisions on `id`).
    void ensure_default_channels_();
    std::string get_meta_(const std::string& key) const;
    void set_meta_(const std::string& key, const std::string& value);

    sqlite3* db_ = nullptr;
    mutable std::mutex mutex_;
};

} // namespace chatproj
