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
    // Per-channel retention in days. 0 = forever.
    int32_t retention_days_text = 0;
    int32_t retention_days_image = 0;
    int32_t retention_days_video = 0;
    int32_t retention_days_document = 0;
    int32_t retention_days_audio = 0;
};

struct DbMessage {
    int64_t id = 0;
    std::string channel_id;
    std::string sender;
    std::string content;
    int64_t timestamp = 0;
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
    std::string uploader;      // username that POSTed /init
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

    // Update all five retention values at once. Returns false if the channel
    // doesn't exist or on DB error. Negative values are clamped to 0.
    bool update_channel_retention(const std::string& channel_id,
                                  int32_t text_days,
                                  int32_t image_days,
                                  int32_t video_days,
                                  int32_t document_days,
                                  int32_t audio_days);
    // Fetch a single channel with retention populated. nullopt if unknown.
    std::optional<DbChannel> get_channel(const std::string& channel_id) const;

    // --- messages ---
    // Insert a new message, return its autoincrement id (0 on failure).
    int64_t insert_message(const std::string& channel_id,
                           const std::string& sender,
                           const std::string& content,
                           int64_t timestamp);
    // Newest-first page. `before_id = 0` means "most recent". Results are
    // ordered newest→oldest; caller reverses if they want oldest→newest.
    // `has_more` is set to true if more messages exist older than the page.
    std::vector<DbMessage> fetch_messages(const std::string& channel_id,
                                          int64_t before_id,
                                          int32_t limit,
                                          bool* has_more) const;
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
                                      const std::string& uploader,
                                      int32_t position,
                                      int32_t width,
                                      int32_t height);
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
    // that are 'ready', owned by `uploader`, currently unbound (message_id=0),
    // and belong to `channel_id` get bound. Returns the list of ids that
    // were successfully bound.
    std::vector<int64_t> bind_attachments(const std::vector<int64_t>& attachment_ids,
                                          int64_t message_id,
                                          const std::string& channel_id,
                                          const std::string& uploader);
    // Pending attachments (message_id=0) older than `cutoff_ts` (created_at
    // in unix seconds). Used by the retention sweep to clean up abandoned
    // uploads. Returns the rows themselves so the caller can unlink .partial
    // files before the rows are deleted.
    std::vector<DbAttachment> list_stale_pending_attachments(int64_t cutoff_ts) const;
    bool delete_attachment_row(int64_t attachment_id);

    // --- owner-initiated wipe ---
    // Result of a full channel wipe — the count fields are returned to
    // the calling client for confirmation feedback, the unlink_paths
    // are filesystem cleanup the server applies after the DB commit.
    struct WipeChannelResult {
        int64_t deleted_message_count = 0;
        int64_t deleted_attachment_count = 0;
        std::vector<std::string> unlink_paths;
    };
    // Delete every message + attachment row for `channel_id`, returning
    // the counts and the storage paths whose blobs (and thumbnails) the
    // server should unlink. The AFTER DELETE trigger on messages mirrors
    // the deletes into the FTS5 index. Wrapped in a transaction so a
    // failure mid-way leaves the channel intact.
    WipeChannelResult wipe_channel(const std::string& channel_id);

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
    void seed_if_empty_(const std::string& owner,
                        const std::string& name,
                        const std::string& desc);
    std::string get_meta_(const std::string& key) const;
    void set_meta_(const std::string& key, const std::string& value);

    sqlite3* db_ = nullptr;
    mutable std::mutex mutex_;
};

} // namespace chatproj
