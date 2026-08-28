#ifdef _WIN32
#ifndef WIN32_LEAN_AND_MEAN
#define WIN32_LEAN_AND_MEAN
#endif
#ifndef _WIN32_WINNT
#define _WIN32_WINNT 0x0A00
#endif
#endif

#include <ctime>
#include <filesystem>
#include <iostream>
#include <fstream>
#include <string>
#include <memory>
#include <vector>
#include <set>
#include <unordered_map>
#include <chrono>
#include <mutex>
#include <thread>
#include <utility>
#include <algorithm>
#include <functional>
#include <system_error>
#include <boost/asio.hpp>
#include <boost/asio/ssl.hpp>
#include <boost/asio/steady_timer.hpp>
#include <openssl/sha.h>
#include <jwt-cpp/traits/nlohmann-json/defaults.h>
#include "messages.pb.h"
#include "../common/net_utils.hpp"
#include "../common/udp_packet.hpp"
#include "db.hpp"
#include "attachment_http.hpp"
#include "rate_limit.hpp"
#include "central_sync.hpp"
#include "authz.hpp"
#include "../common/ed25519_keys.hpp"

namespace ssl = boost::asio::ssl;
using boost::asio::ip::tcp;

#include <deque>
#ifdef __linux__
#include <netinet/tcp.h>
#endif

namespace {
/// Returns the lowercase sha256 hex digest of `data`. Used to derive
/// the picture_version for SyncServerPictureReq — central trusts the
/// community to compute it consistently with what it can re-verify on
/// the bytes it stores. Hoisted up here so Session::process_packet
/// (which is defined below) can call it.
std::string sha256_hex(const std::string& data) {
    unsigned char digest[SHA256_DIGEST_LENGTH];
    SHA256(reinterpret_cast<const unsigned char*>(data.data()),
           data.size(), digest);
    static const char kHex[] = "0123456789abcdef";
    std::string out;
    out.reserve(SHA256_DIGEST_LENGTH * 2);
    for (unsigned char b : digest) {
        out.push_back(kHex[b >> 4]);
        out.push_back(kHex[b & 0x0F]);
    }
    return out;
}

/// Strips the client-only envelope fields before a packet received from
/// one member is forwarded to others. The Electron client puts its bearer
/// JWT in Packet.auth_token on EVERY packet it sends (the central server
/// requires that; community handlers never read it) — so forwarding a
/// client packet verbatim broadcast the sender's JWT to every member with
/// every CHANNEL_MSG / thumbnail / codec-change notify. Anyone in the
/// server could have impersonated the sender for the token's lifetime.
void strip_client_envelope(chatproj::Packet& p) {
    p.clear_auth_token();
    p.clear_timestamp();
}

/// Length-prefixed wire frame for a packet, shared across fan-out paths.
std::shared_ptr<std::vector<uint8_t>> frame_packet(const chatproj::Packet& p) {
    std::string serialized;
    p.SerializeToString(&serialized);
    uint32_t length = htonl(static_cast<uint32_t>(serialized.size()));
    auto framed = std::make_shared<std::vector<uint8_t>>();
    framed->resize(4 + serialized.size());
    std::memcpy(framed->data(), &length, 4);
    std::memcpy(framed->data() + 4, serialized.data(), serialized.size());
    return framed;
}

/// Builds a ROLE_LIST_RES packet from the current DB state. Pushed to a
/// session after auth, sent on ROLE_LIST_REQ, and broadcast to every
/// member whenever a role is created/updated/deleted.
chatproj::Packet build_role_list_packet(chatproj::CommunityDb* db) {
    chatproj::Packet p;
    p.set_type(chatproj::Packet::ROLE_LIST_RES);
    auto* res = p.mutable_role_list_res();
    res->set_success(db != nullptr);
    if (db) {
        for (const auto& r : db->list_roles()) {
            auto* info = res->add_roles();
            info->set_id(r.id);
            info->set_name(r.name);
            info->set_color(r.color);
            info->set_position(r.position);
            info->set_permissions(r.permissions);
            info->set_is_default(r.is_default);
        }
    }
    return p;
}

/// Copies a DbRole into a RoleActionResponse's role field.
void fill_role_info(chatproj::RoleInfo* info, const chatproj::DbRole& r) {
    info->set_id(r.id);
    info->set_name(r.name);
    info->set_color(r.color);
    info->set_position(r.position);
    info->set_permissions(r.permissions);
    info->set_is_default(r.is_default);
}

/// DbChannel.type → wire enum (0 text, 1 voice, 2 category).
chatproj::ChannelInfo::Type channel_type_to_proto(int32_t type) {
    switch (type) {
        case 1: return chatproj::ChannelInfo::VOICE;
        case 2: return chatproj::ChannelInfo::CATEGORY;
        default: return chatproj::ChannelInfo::TEXT;
    }
}

/// Copies a DbChannel into a ChannelInfo.
void fill_channel_info(chatproj::ChannelInfo* info, const chatproj::DbChannel& ch) {
    info->set_id(ch.id);
    info->set_name(ch.name);
    info->set_type(channel_type_to_proto(ch.type));
    info->set_voice_bitrate_kbps(ch.voice_bitrate_kbps);
    info->set_retention_days_text(ch.retention_days_text);
    info->set_retention_days_image(ch.retention_days_image);
    info->set_retention_days_video(ch.retention_days_video);
    info->set_retention_days_document(ch.retention_days_document);
    info->set_retention_days_audio(ch.retention_days_audio);
    info->set_slowmode_seconds(ch.slowmode_seconds);
}

/// Builds a CHANNEL_LIST_UPDATE packet for ONE recipient: only channels
/// they can VIEW (categories always), each stamped with the recipient's
/// resolved permissions. Channel lists are per recipient since
/// permissions v2 — see the spec.
chatproj::Packet build_channel_list_packet(const chatproj::Authorizer& authz,
                                           const std::string& username) {
    chatproj::Packet p;
    p.set_type(chatproj::Packet::CHANNEL_LIST_UPDATE);
    auto* update = p.mutable_channel_list_update();
    for (const auto& ch : authz.visible_channels(username)) {
        auto* info = update->add_channels();
        fill_channel_info(info, ch);
        info->set_my_permissions(authz.channel_permissions(username, ch.id));
    }
    return p;
}

/// Copies a DbOverwrite into the wire shape.
void fill_overwrite(chatproj::ChannelOverwrite* out, const chatproj::DbOverwrite& ow) {
    out->set_channel_id(ow.channel_id);
    out->set_target_type(ow.target_type == 1 ? chatproj::ChannelOverwrite::MEMBER
                                             : chatproj::ChannelOverwrite::ROLE);
    out->set_target_id(ow.target_id);
    out->set_allow(ow.allow);
    out->set_deny(ow.deny);
}

void fill_ban_list(chatproj::BanListResponse* res, chatproj::CommunityDb* db, uint64_t revision) {
    res->set_success(db != nullptr);
    res->set_revision(revision);
    if (!db) return;
    for (const auto& b : db->list_bans()) {
        auto* e = res->add_entries();
        e->set_username(b.username);
        e->set_banned_by(b.banned_by);
        e->set_reason(b.reason);
        e->set_banned_at(b.banned_at);
        e->set_expires_at(b.expires_at);
    }
}

std::string format_utc(int64_t ts) {
    std::time_t t = static_cast<std::time_t>(ts);
    std::tm tm{};
#ifdef _WIN32
    gmtime_s(&tm, &t);
#else
    gmtime_r(&t, &tm);
#endif
    char buf[32];
    std::strftime(buf, sizeof(buf), "%Y-%m-%d %H:%M UTC", &tm);
    return buf;
}

chatproj::Packet build_overwrites_packet(chatproj::CommunityDb* db, const std::string& channel_id) {
    chatproj::Packet p;
    p.set_type(chatproj::Packet::CHANNEL_OVERWRITES_RES);
    auto* res = p.mutable_channel_overwrites_res();
    res->set_success(db != nullptr);
    res->set_channel_id(channel_id);
    if (db) {
        for (const auto& ow : db->list_overwrites(channel_id)) {
            fill_overwrite(res->add_overwrites(), ow);
        }
    }
    return p;
}
} // namespace

class Session;

class SessionManager {
public:
    void join(std::shared_ptr<Session> session);
    void leave(std::shared_ptr<Session> session);
    void join_voice_channel(std::shared_ptr<Session> session, const std::string& new_channel, const std::string& old_channel);
    void leave_voice_channel(std::shared_ptr<Session> session, const std::string& current_channel);
    // Broadcast to every authenticated session on this server (server-wide
    // packets: roster, roles, member-level events).
    void broadcast_to_members(const chatproj::Packet& packet);
    // Broadcast to every authenticated session that can VIEW `channel_id`
    // (permissions v2). Every channel-scoped packet — messages, deletes,
    // wipes, prunes, retention updates, voice/stream presence — goes
    // through this so a private channel's traffic never reaches members
    // who can't see it.
    void broadcast_to_channel(const chatproj::Packet& packet, const std::string& channel_id);
    // Batched CHANNEL_MESSAGE_DELETED fan-out for a bulk purge (ban-with-
    // purge, future bulk delete). Frames each tombstone once, snapshots the
    // session list once and resolves each viewer's channel access once —
    // instead of a full sessions_ scan + permission resolve per message,
    // which stalled the single io thread when purging a prolific spammer (A2).
    void broadcast_message_deletions(
        const std::string& deleted_by, int64_t deleted_at,
        const std::vector<std::pair<std::string, int64_t>>& messages);
    // Push CHANNEL_OVERWRITES_RES for a channel to every session allowed
    // to view them (MANAGE_ROLES / MANAGE_CHANNELS there).
    void broadcast_overwrites(const std::string& channel_id);
    // Re-send the (per-recipient) channel list to one user's sessions —
    // after a role assignment changed what they can see.
    void send_channels_to_user(const std::string& username);
    // SERVER_META_UPDATE (name / description / owner) to every session.
    void broadcast_server_meta();
    // Voice moderation on every live session of `username`. Returns the
    // number of sessions affected (0 = offline / not in voice for
    // move/disconnect).
    size_t apply_server_voice_flags(const std::string& username, bool muted, bool deafened);
    size_t move_to_voice_channel(const std::string& username, const std::string& channel_id,
                                 const std::string& actor);
    size_t disconnect_from_voice(const std::string& username, const std::string& actor);
    // Slowmode, keyed by USERNAME (not session) so a second connection or a
    // reconnect can't reset the window (M3). Split check/record so a message
    // that is ultimately rejected (oversized, empty, persist failure) doesn't
    // consume the window: slowmode_remaining() only reads, slowmode_record()
    // stamps the accept. `remaining` returns 0 when a message is allowed now.
    int64_t slowmode_remaining(const std::string& username, const std::string& channel_id,
                               int32_t slowmode_seconds);
    void slowmode_record(const std::string& username, const std::string& channel_id);
    // Roster deltas (see "Roster protocol" in messages.proto). The full
    // roster used to be re-pushed to every session on every change —
    // O(members × online) per event. Now one MemberInfo goes out per
    // change, O(online).
    //   emit_member_upsert: join, nickname, roles, online/offline flip.
    //   emit_member_remove: leave / kick / ban.
    //   broadcast_bans:     BAN_LIST_RES to BAN_MEMBERS holders.
    // Every packet carries the monotonically increasing roster revision.
    void emit_member_upsert(const std::string& username);
    void emit_member_remove(const std::string& username);
    void broadcast_bans();
    uint64_t roster_revision() const { return roster_revision_; }
    // Builds the wire MemberInfo for one member (nullopt if not a member).
    std::optional<chatproj::MemberInfo> build_member_info(const std::string& username);
    // Paged snapshot for MEMBER_LIST_REQ. First page (after == "") = every
    // online member + the first `limit` offline members by username;
    // later pages = offline only.
    void fill_member_page(const std::string& after, int32_t limit,
                          chatproj::MemberListResponse* res);
    // Push the full role list to every authenticated session. Fired on
    // any role create/update/delete so hierarchy + colors stay live.
    void broadcast_roles();
    // Push the full channel list to every authenticated session. Fired
    // on any channel create/rename/delete.
    void broadcast_channels();
    // True when any live session is currently in this voice channel.
    // Guards channel deletion ("channel is in use").
    bool voice_channel_occupied(const std::string& channel_id);
    // Runs one retention sweep across every channel in the DB — deletes
    // messages past `retention_days_text` and tombstones attachments past
    // their per-kind cutoff. Broadcasts a CHANNEL_PRUNED to every
    // authenticated session for each channel that had anything removed so
    // live UIs drop stale messages/attachments without a reload.
    void run_retention_sweep();
    void broadcast_to_voice_channel(const char* data, size_t length, const std::string& channel_id, std::shared_ptr<Session> sender, boost::asio::ip::udp::socket& udp_socket);
    void broadcast_to_voice_channel_tcp(const chatproj::Packet& packet, const std::string& channel_id);
    void relay_keyframe_request(const std::string& target_username, boost::asio::ip::udp::socket& udp_socket);
    void relay_nack(const char* data, size_t length, const std::string& target_username, boost::asio::ip::udp::socket& udp_socket);
    void broadcast_voice_presence(const std::string& channel_id);
    void send_initial_voice_presences(std::shared_ptr<Session> session);
    std::shared_ptr<Session> find_session_by_token(const std::string& token, const std::string& jwt_secret);

    // Screen Sharing
    void start_stream(std::shared_ptr<Session> session, const std::string& channel_id,
                      bool has_audio, uint32_t fps, uint32_t width, uint32_t height,
                      chatproj::VideoCodec chosen_codec, chatproj::VideoCodec enforced_codec);
    void stop_stream(std::shared_ptr<Session> session, const std::string& channel_id);
    void broadcast_stream_presence(const std::string& channel_id);
    // True when `username` has a registered live stream in `channel_id`.
    // Gates STREAM_THUMBNAIL_UPDATE so only actual streamers can push
    // into the thumbnail cache / broadcast to the channel.
    bool has_active_stream(const std::string& channel_id, const std::string& username);

    // On-demand thumbnail cache for the UserPopup live preview.
    // Written by update_thumbnail_cache() (called from the
    // STREAM_THUMBNAIL_UPDATE handler), read by get_thumbnail()
    // (called from the FETCH_STREAM_THUMBNAIL_REQ handler), erased by
    // erase_thumbnail_cache() (called on stream stop + session
    // disconnect). All three take their own lock on mutex_.
    void update_thumbnail_cache(const std::string& username,
                                const std::string& bytes);
    void erase_thumbnail_cache(const std::string& username);
    bool get_thumbnail(const std::string& username,
                       std::vector<uint8_t>& out);

    // Watcher tracking. Returns true if the watcher was newly added (so the
    // caller fires the join-notify + one keyframe only on the FIRST subscribe).
    bool add_watcher(std::shared_ptr<Session> watcher, const std::string& channel_id, const std::string& streamer_username);
    // Returns true if the watcher was actually subscribed.
    bool remove_watcher(std::shared_ptr<Session> watcher, const std::string& channel_id, const std::string& streamer_username);
    // Plan C: tell the streamer that a watcher just joined or left their stream.
    // Drives the streamer's CodecSelector (LCD picker + debounce/cooldown).
    void notify_streamer_of_watcher(const std::string& channel_id,
                                    const std::string& streamer_username,
                                    const std::string& watcher_username,
                                    chatproj::StreamWatcherNotify::Action action);
    // Plan C: returns true if the stream has enforced_codec set AND the
    // candidate watcher's caps don't include that codec. Defensive — UI
    // should already gray out the watch button.
    bool watcher_blocked_by_enforcement(const std::string& channel_id,
                                        const std::string& streamer_username,
                                        std::shared_ptr<Session> watcher);
    // Plan C: streamer announces a mid-stream codec change. Validate
    // ownership, update registry, rebroadcast presence + forward notify
    // for toast text.
    void handle_stream_codec_changed(const chatproj::Packet& packet,
                                     const std::string& sender_username);
    void broadcast_to_watchers(const char* data, size_t length, const std::string& channel_id, const std::string& streamer_username, boost::asio::ip::udp::socket& udp_socket);
    void broadcast_to_watchers_voice(const char* data, size_t length, const std::string& channel_id, const std::string& streamer_username, boost::asio::ip::udp::socket& udp_socket);
    void send_udp_to_targets(const char* data, size_t length, boost::asio::ip::udp::socket& udp_socket);
    void set_udp_socket(boost::asio::ip::udp::socket* sock) { udp_socket_ptr_ = sock; }
    void set_media_udp_socket(boost::asio::ip::udp::socket* sock) { media_udp_socket_ptr_ = sock; }
    // Index an authenticated session by its udp_key (UDP sender lookup)
    // and username (moderation / relay lookup). Called once, after the
    // session's username_/udp_key_ are set.
    void register_authenticated(std::shared_ptr<Session> session);
    void unregister_udp_key(const std::string& udp_key);
    void relay_keyframe_request_internal(const std::string& target_username);
    size_t session_count() { std::lock_guard<std::mutex> lock(mutex_); return sessions_.size(); }

    // Returns the set of usernames that currently have an authenticated live
    // session. Used for the members-list "online" flag.
    std::set<std::string> get_online_usernames();

    // Persistent state.
    void set_db(chatproj::CommunityDb* db) { db_ = db; }
    chatproj::CommunityDb* db() { return db_; }
    void set_authz(const chatproj::Authorizer* a) { authz_ = a; }
    const chatproj::Authorizer& authz() const { return *authz_; }

    // Member count (authoritative from DB), used by the central-server heartbeat.
    size_t member_count();

    // Find an active session by username. Returns nullptr if not connected.
    // A user may hold several live sessions (two devices, a reconnect
    // racing its predecessor); this returns an arbitrary one — use
    // find_sessions_by_username when the action must hit all of them.
    std::shared_ptr<Session> find_session_by_username(const std::string& username);
    std::vector<std::shared_ptr<Session>> find_sessions_by_username(const std::string& username);

    // Forcibly disconnect EVERY live session of `username` — sends
    // MEMBERSHIP_REVOKED then closes each. Returns how many sessions were
    // hit (0 = target offline; the caller must then refresh rosters
    // itself, since no leave() will fire to do it). Cleanup of
    // voice/stream state happens via leave() on the socket close.
    size_t force_disconnect(const std::string& username,
                            const std::string& action,
                            const std::string& reason,
                            const std::string& actor,
                            int64_t expires_at = 0);

    // Central-hosted invite sync. Community servers register each live invite
    // with central so clients can redeem a raw code without knowing host:port.
    void set_central_sync(const std::string& central_host, int central_port,
                          const std::string& community_secret,
                          const std::string& public_ip, int community_port,
                          const std::string& central_cert_pin);
    // sha256-hex of our own TLS certificate, reported in heartbeats so
    // clients can pin the community connection.
    void set_cert_fingerprint(const std::string& fp) { cert_fingerprint_ = fp; }
    const std::string& cert_fingerprint() const { return cert_fingerprint_; }
    void sync_invite_register(const std::string& code, int64_t expires_at);
    void sync_invite_unregister(const std::string& code);
    // Queue a framed packet for the central-sync worker (no-op when no
    // central is configured). `done` runs on the worker thread.
    void enqueue_central(std::vector<uint8_t> framed, bool read_response = false,
                         chatproj::CentralSyncWorker::Done done = nullptr);

    // --- Auto-rejoin: server_id learned from SERVER_HEARTBEAT_RES ---
    // (see docs/superpowers/specs/2026-05-14-auto-rejoin-communities-design.md)
    int64_t server_id() const { return server_id_.load(); }
    void set_server_id(int64_t id) { server_id_.store(id); }
    // Idempotent membership sync (fire-and-forget over a one-shot
    // TLS connection to central). Silently skips if server_id_ is 0.
    void sync_membership_register(const std::string& username);
    /// Forward an owner-authorized picture (or empty data = removal)
    /// to central via shared-secret one-shot TLS. Picture_version is
    /// the pre-computed sha256-hex of `data` (or '' on removal).
    void sync_server_picture(const std::string& data,
                              const std::string& version);
    void sync_membership_revoke(const std::string& username);

    // Session deadlines (seconds). auth: time a freshly-connected peer
    // has to complete COMMUNITY_AUTH_REQ; idle: time an authenticated
    // session may go without any frame (clients CLIENT_PING every 30 s).
    void set_timeouts(int auth_seconds, int idle_seconds) {
        auth_timeout_seconds_ = auth_seconds;
        idle_timeout_seconds_ = idle_seconds;
    }
    int auth_timeout_seconds() const { return auth_timeout_seconds_; }
    int idle_timeout_seconds() const { return idle_timeout_seconds_; }

    // Executor for manager-owned timers (roster coalescing). Must be set
    // before any timer-backed manager operation.
    void set_io_context(boost::asio::io_context& io);

    // Attachment config — reported to clients on CommunityAuthResponse so
    // they know where to upload and what size cap to pre-validate against.
    void set_attachment_config(int port, int64_t max_bytes) {
        attachment_port_ = port;
        max_attachment_bytes_ = max_bytes;
    }
    int attachment_port() const { return attachment_port_; }
    int64_t max_attachment_bytes() const { return max_attachment_bytes_; }
    // Paths for the Storage tab: the attachment store's volume (free/total)
    // and the SQLite file sizes. Set once at startup.
    void set_storage_paths(std::string db_path, std::string attachments_root) {
        db_path_ = std::move(db_path);
        attachments_root_ = std::move(attachments_root);
    }
    // Fills a StorageInfoResponse: DB-derived footprint + host volume space
    // (std::filesystem) + SQLite file size. Sets success=true.
    void fill_storage_info(chatproj::StorageInfoResponse* res);
    // Capacity of the attachment store's volume, or 0 if unknown. Used to
    // clamp the min-free headroom — requiring more free space than the disk
    // physically holds is nonsensical.
    int64_t volume_capacity() const;

private:
    std::set<std::shared_ptr<Session>> sessions_;
    std::unordered_map<std::string, std::set<std::shared_ptr<Session>>> voice_channels_;
    
    // channel_id -> map of username -> stream info
    struct StreamInfo {
        bool has_audio;
        // Codec/resolution/fps tracked here so STREAM_PRESENCE_UPDATE can
        // broadcast them to all viewers (drives the codec/lock badge and
        // the grayed-out watch button on the viewer side). Populated from
        // StartStreamRequest and updated mid-stream by STREAM_CODEC_CHANGED_NOTIFY
        // (Plan C). Defaults match legacy clients pre-negotiation.
        uint32_t fps = 0;
        uint32_t width = 0;
        uint32_t height = 0;
        chatproj::VideoCodec current_codec = chatproj::CODEC_H264_HW;
        chatproj::VideoCodec enforced_codec = chatproj::CODEC_UNKNOWN;
        // Session that started this stream. On a reconnect the same username
        // re-registers a stream under a new session; the stale session's
        // leave() must not tear the live one down (I1) — user-keyed state is
        // only erased when this weak_ptr locks to the acting session (or is
        // expired). Mirrors the udp_key_index_ identity guard.
        std::weak_ptr<Session> owner;
    };
    std::unordered_map<std::string, std::unordered_map<std::string, StreamInfo>> active_streams_;

    // Latest thumbnail JPEG per streamer username. Written by
    // update_thumbnail_cache(), served by get_thumbnail(), erased by
    // erase_thumbnail_cache(). Guarded by mutex_.
    std::unordered_map<std::string, std::vector<uint8_t>> latest_thumbnails_;

    // channel_id -> streamer_username -> set of watcher sessions
    std::unordered_map<std::string,
        std::unordered_map<std::string, std::set<std::shared_ptr<Session>>>>
        stream_watchers_;

    // streamer_username -> last time a keyframe request was relayed to them.
    // Rate-limits watcher-driven PLIs so a client can't force continuous IDRs
    // by spamming WATCH, and coalesces many watchers' near-simultaneous
    // requests into a single IDR. Guarded by mutex_.
    std::unordered_map<std::string, std::chrono::steady_clock::time_point>
        last_keyframe_relay_;

    uint32_t max_streams_per_channel_ = 8;  // 0 = unlimited
    // Reusable fan-out scratch (io thread only). Avoids a heap allocation
    // per relayed datagram.
    std::vector<boost::asio::ip::udp::endpoint> udp_targets_;
    boost::asio::ip::udp::socket* udp_socket_ptr_ = nullptr;
    boost::asio::ip::udp::socket* media_udp_socket_ptr_ = nullptr;

    // O(1) UDP sender_id → session lookup (key = last 31 chars of JWT)
    std::unordered_map<std::string, std::shared_ptr<Session>> udp_key_index_;
    // username → every authenticated live session for that user. Replaces
    // the linear scans of sessions_ on the NACK/keyframe relay path and
    // lets kick/ban reach all of a user's sessions, not just the first.
    std::unordered_map<std::string, std::vector<std::shared_ptr<Session>>> sessions_by_user_;
    // Max live authenticated sessions per user. Beyond this the oldest is
    // evicted — bounds the per-session rate-limit / slowmode multiplication
    // (M3) while leaving room for a few genuine devices.
    static constexpr size_t kMaxSessionsPerUser = 8;

    // username → channel → last accepted-message time (slowmode; M3). Guarded
    // by its own mutex — it's on the hot chat path and shouldn't contend on
    // mutex_. Survives reconnects on purpose; cleared wholesale if it ever
    // grows past a sane bound (mirrors overwrites_cache_).
    std::mutex slowmode_mutex_;
    std::unordered_map<std::string,
        std::unordered_map<std::string, std::chrono::steady_clock::time_point>> slowmode_last_;

    chatproj::CommunityDb* db_ = nullptr;
    const chatproj::Authorizer* authz_ = nullptr;

    // Central-sync config (populated once at startup via set_central_sync).
    std::string central_host_;
    int central_port_ = 0;
    std::string central_secret_;
    std::string public_ip_;
    int community_port_ = 0;
    std::string cert_fingerprint_;

    int attachment_port_ = 0;
    int64_t max_attachment_bytes_ = 0;
    std::string db_path_;
    std::string attachments_root_;
    int auth_timeout_seconds_ = 10;
    int idle_timeout_seconds_ = 90;

    boost::asio::io_context* io_ = nullptr;
    uint64_t roster_revision_ = 0;


    // Auto-rejoin: central-assigned community_servers.id. 0 means
    // "not yet learned" — sync_membership_register/revoke silently
    // skip when 0. Loaded from CommunityDb at startup; refreshed by
    // every SERVER_HEARTBEAT_RES.
    std::atomic<int64_t> server_id_{0};

    std::mutex mutex_;

    // Central-sync worker (see central_sync.hpp). Created by
    // set_central_sync; null when no central host is configured. LAST
    // member on purpose: its destructor joins the worker thread, whose
    // callbacks touch server_id_ / db_, so it must go first.
    std::unique_ptr<chatproj::CentralSyncWorker> central_worker_;
};

class Session : public std::enable_shared_from_this<Session> {
public:
    Session(tcp::socket socket, SessionManager& manager, ssl::context& context, const std::string& jwt_public_pem)
        : socket_(std::move(socket), context),
          close_timer_(socket_.lowest_layer().get_executor()),
          deadline_timer_(socket_.lowest_layer().get_executor()),
          manager_(manager), jwt_public_pem_(jwt_public_pem) {
        // Enable TCP keepalive to detect dead client connections.
        // Tighten from system defaults (~2h) to 15s idle + 5s interval + 3 retries = ~30s detection.
        socket_.lowest_layer().set_option(boost::asio::socket_base::keep_alive(true));
#ifdef __linux__
        int fd = socket_.lowest_layer().native_handle();
        int idle = 15, interval = 5, count = 3;
        setsockopt(fd, IPPROTO_TCP, TCP_KEEPIDLE, &idle, sizeof(idle));
        setsockopt(fd, IPPROTO_TCP, TCP_KEEPINTVL, &interval, sizeof(interval));
        setsockopt(fd, IPPROTO_TCP, TCP_KEEPCNT, &count, sizeof(count));
#endif
    }

    void start() {
        auto self(shared_from_this());
        // The auth deadline covers the TLS handshake too: a peer that
        // connects and never completes either used to pin a Session
        // (and its fd) forever — there was no timer of any kind here.
        arm_deadline(manager_.auth_timeout_seconds());
        socket_.async_handshake(ssl::stream_base::server,
            [this, self](const boost::system::error_code& error) {
                if (!error) {
                    do_read_header();
                } else {
                    manager_.leave(shared_from_this());
                }
            });
    }

    // (Re)arm the inactivity deadline. Pre-auth it's the auth deadline
    // (armed once, never extended by junk frames); post-auth it's the
    // idle deadline, re-armed on every frame incl. CLIENT_PING.
    void arm_deadline(int seconds) {
        if (seconds <= 0) return;
        auto self(shared_from_this());
        deadline_timer_.expires_after(std::chrono::seconds(seconds));
        deadline_timer_.async_wait([this, self](const boost::system::error_code& ec) {
            if (ec) return;   // re-armed or cancelled
            if (closing_) return;
            std::cout << "[Community] Dropping "
                      << (authenticated_ ? ("idle session of " + username_)
                                         : std::string("unauthenticated session"))
                      << " (deadline)\n";
            manager_.leave(shared_from_this());
            close_connection();
        });
    }

    void deliver(std::shared_ptr<std::vector<uint8_t>> framed_data) {
        // Cap the per-session backlog. A client that stops reading (or reads
        // slowly) would otherwise let broadcasts / presence / channel
        // messages pile up in memory without bound.
        constexpr size_t MAX_QUEUED_MESSAGES = 1024;
        bool write_in_progress = false;
        bool overflow = false;
        {
            std::lock_guard<std::mutex> lock(write_mutex_);
            if (write_queue_.size() >= MAX_QUEUED_MESSAGES) {
                overflow = true;
            } else {
                write_in_progress = !write_queue_.empty();
                write_queue_.push_back(framed_data);
            }
        }

        if (overflow) {
            // Post the disconnect rather than calling leave() synchronously:
            // deliver() runs inside broadcast loops that iterate sessions_,
            // and erasing here would invalidate the iterator. Close the
            // socket too — leave() only detaches the session from the
            // manager, and without the close the slow reader stays
            // connected as a zombie whose messages still get broadcast
            // while its 1024-frame backlog stays pinned in memory.
            auto self = shared_from_this();
            boost::asio::post(socket_.lowest_layer().get_executor(),
                              [this, self]() {
                                  manager_.leave(self);
                                  close_connection();
                              });
            return;
        }

        if (!write_in_progress) {
            do_write();
        }
    }

    std::string get_username() const { return username_; }
    std::string get_token() const { return token_; }
    std::string get_udp_key() const { return udp_key_; }
    bool is_authenticated() const { return authenticated_; }

    // Codec capabilities advertised by this client. Populated from
    // JoinVoiceRequest.capabilities and updated mid-session by
    // UPDATE_CAPABILITIES_REQ. Read-only access for SessionManager when
    // building VoicePresenceUpdate.user_capabilities.
    void set_capabilities(const chatproj::ClientCapabilities& caps) {
        std::lock_guard<std::mutex> lock(capabilities_mutex_);
        capabilities_ = caps;
    }
    chatproj::ClientCapabilities get_capabilities() const {
        std::lock_guard<std::mutex> lock(capabilities_mutex_);
        return capabilities_;
    }

    // Forcibly close the underlying TCP socket. Any in-flight reads/writes
    // will error out, which triggers SessionManager::leave via the normal
    // error-handling path. Safe to call from any thread.
    void close_connection() {
        boost::system::error_code ec;
        socket_.lowest_layer().cancel(ec);
        socket_.lowest_layer().close(ec);
    }

    // Gracefully end the session: stop re-arming the read loop, let any
    // queued writes drain, then close. Used on protocol-level rejections
    // (failed auth) where the peer should still receive the final
    // response packet before the socket goes away. Without this, a
    // rejected session's read loop stayed armed and the client could
    // re-auth on the same socket into a half-registered zombie state.
    void close_after_flush() {
        closing_ = true;
        bool pending;
        {
            std::lock_guard<std::mutex> lock(write_mutex_);
            pending = !write_queue_.empty();
        }
        if (!pending) { finish_close(); return; }
        // Hard deadline: a peer that stops reading must not keep a
        // rejected / kicked session (and its queued frames) alive
        // indefinitely waiting for the drain.
        auto self = shared_from_this();
        close_timer_.expires_after(std::chrono::seconds(3));
        close_timer_.async_wait([this, self](const boost::system::error_code& ec) {
            if (!ec) finish_close();
        });
    }

    // Detach from the manager, then close. Needed because a closing
    // session no longer re-arms its read loop, so the usual
    // "socket close → read error → manager_.leave()" path never runs
    // for it; without this a kicked/left user stayed in sessions_ and
    // the voice/stream maps as a ghost. leave() is idempotent.
    void finish_close() {
        close_timer_.cancel();
        manager_.leave(shared_from_this());
        close_connection();
    }

    // Send a pre-built packet. Public so SessionManager can push notifications
    // (MEMBERSHIP_REVOKED, SERVER_META_UPDATE, etc.) directly.
    void send_packet_external(const chatproj::Packet& packet) { send_packet(packet); }
    void set_udp_endpoint(const boost::asio::ip::udp::endpoint& ep) { udp_endpoint_ = ep; }
    boost::asio::ip::udp::endpoint get_udp_endpoint() const { return udp_endpoint_; }
    void set_udp_media_endpoint(const boost::asio::ip::udp::endpoint& ep) { udp_media_endpoint_ = ep; }
    boost::asio::ip::udp::endpoint get_udp_media_endpoint() const { return udp_media_endpoint_; }
    std::string get_current_voice_channel() const { return current_voice_channel_; }
    void set_current_voice_channel(const std::string& ch) { current_voice_channel_ = ch; }
    // Moderator-applied voice flags (VOICE_MOD_REQ), loaded from the
    // member row at auth and updated live. The relay drops a server-muted
    // user's audio and skips a server-deafened user as a target.
    bool is_server_muted() const { return server_muted_ || server_deafened_; }
    bool is_server_deafened() const { return server_deafened_; }
    void set_server_voice_flags(bool muted, bool deafened) { server_muted_ = muted; server_deafened_ = deafened; }
    bool is_muted() const { return is_muted_; }
    bool is_deafened() const { return is_deafened_; }
    void set_muted(bool m) { is_muted_ = m; }
    void set_deafened(bool d) { is_deafened_ = d; }

private:
    void do_write() {
        auto self(shared_from_this());
        std::shared_ptr<std::vector<uint8_t>> data_to_write;
        {
            std::lock_guard<std::mutex> lock(write_mutex_);
            if (write_queue_.empty()) return;
            data_to_write = write_queue_.front();
        }

        boost::asio::async_write(socket_, boost::asio::buffer(*data_to_write),
            [this, self](boost::system::error_code ec, std::size_t) {
                if (!ec) {
                    bool more_to_write = false;
                    {
                        std::lock_guard<std::mutex> lock(write_mutex_);
                        write_queue_.pop_front();
                        more_to_write = !write_queue_.empty();
                    }
                    if (more_to_write) {
                        do_write();
                    } else if (closing_) {
                        finish_close();
                    }
                } else {
                    manager_.leave(shared_from_this());
                }
            });
    }
    void do_read_header() {
        auto self(shared_from_this());
        boost::asio::async_read(socket_, boost::asio::buffer(inbound_header_, 4),
            [this, self](boost::system::error_code ec, std::size_t) {
                if (!ec) {
                    // memcpy rather than reinterpret_cast: reading a
                    // uint32_t through char[4] violates strict aliasing.
                    uint32_t net_len;
                    std::memcpy(&net_len, inbound_header_, 4);
                    uint32_t length = ntohl(net_len);
                    // Pre-auth the only legitimate frame is
                    // COMMUNITY_AUTH_REQ (a JWT + invite code, < 4 KB).
                    // Without this cap an unauthenticated peer could make
                    // us allocate the full 2 MiB per connection.
                    constexpr uint32_t kPreAuthMaxFrame = 64 * 1024;
                    if (length > 2 * 1024 * 1024 ||
                        (!authenticated_ && length > kPreAuthMaxFrame)) {
                        // Drop the session instead of a bare `return`,
                        // which would leave the read loop dead but the
                        // socket open (and the session still receiving
                        // broadcasts) until the stale sweep. Close
                        // explicitly — queued writes to a non-reading
                        // peer would otherwise keep the session alive.
                        manager_.leave(shared_from_this());
                        close_connection();
                        return;
                    }
                    inbound_body_.resize(length);
                    do_read_body(length);
                } else {
                    manager_.leave(shared_from_this());
                }
            });
    }

    void do_read_body(uint32_t length) {
        auto self(shared_from_this());
        boost::asio::async_read(socket_, boost::asio::buffer(inbound_body_.data(), length),
            [this, self](boost::system::error_code ec, std::size_t) {
                if (!ec) {
                    // A closing session (failed auth, kick/ban, leave)
                    // must not act on a frame that was already in
                    // flight when close_after_flush() was called.
                    if (closing_) return;
                    process_packet();
                    if (authenticated_) arm_deadline(manager_.idle_timeout_seconds());
                    // Don't keep a 2 MiB buffer pinned per session after
                    // one large frame (thumbnail / big paste).
                    if (inbound_body_.capacity() > 256 * 1024) {
                        inbound_body_.clear();
                        inbound_body_.shrink_to_fit();
                    }
                    if (!closing_) do_read_header();
                } else {
                    manager_.leave(shared_from_this());
                }
            });
    }

    void process_packet() {
        chatproj::Packet packet;
        if (!packet.ParseFromArray(inbound_body_.data(), static_cast<int>(inbound_body_.size()))) return;

        // --- AUTHENTICATION + MEMBERSHIP GATE ---
        if (packet.type() == chatproj::Packet::COMMUNITY_AUTH_REQ) {
            // One auth per connection. Re-running the flow on a live
            // session would re-register a fresh udp_key without ever
            // unregistering the old one (leaking index entries that pin
            // the session), and lets a session swap identity mid-stream.
            // No shipping client re-auths on the same socket — reconnects
            // always open a new connection.
            if (authenticated_) {
                std::cout << "[Community] Ignoring repeat COMMUNITY_AUTH_REQ from "
                          << username_ << "\n";
                return;
            }
            const auto& req = packet.community_auth_req();
            std::string token = req.jwt_token();
            std::string invite_code = req.invite_code();

            // Step 1: JWT verification — Ed25519 with central's PUBLIC key
            // (Theme A). This server can check a token but can't mint one.
            std::string candidate_username;
            int64_t candidate_uid = 0;
            try {
                auto decoded = jwt::decode(token);
                auto verifier = jwt::verify()
                    .allow_algorithm(jwt::algorithm::ed25519{jwt_public_pem_, ""})
                    .with_issuer("decibell_central_auth");
                verifier.verify(decoded);
                candidate_username = decoded.get_subject();
                if (decoded.has_payload_claim("uid")) {
                    const auto& c = decoded.get_payload_claim("uid");
                    if (c.get_type() == jwt::json::type::integer) candidate_uid = c.as_integer();
                    else if (c.get_type() == jwt::json::type::number) candidate_uid = static_cast<int64_t>(c.as_number());
                    else if (c.get_type() == jwt::json::type::string) { try { candidate_uid = std::stoll(c.as_string()); } catch (...) {} }
                }
                if (candidate_username.empty()) throw std::runtime_error("empty subject");
            } catch (const std::exception& e) {
                std::cout << "[Community] Auth failed (JWT): " << e.what() << "\n";
                send_auth_response(false, "Invalid token.", "auth");
                manager_.leave(shared_from_this());
                close_after_flush();
                return;
            }

            // Step 2: Membership + invite gating.
            auto* db = manager_.db();
            if (!db) {
                send_auth_response(false, "Server misconfigured.", "auth");
                manager_.leave(shared_from_this());
                close_after_flush();
                return;
            }

            if (db->is_banned(candidate_username, candidate_uid)) {
                std::cout << "[Community] Blocked banned user: " << candidate_username << "\n";
                send_auth_response(false, "You are banned from this server.", "banned");
                manager_.leave(shared_from_this());
                close_after_flush();
                return;
            }

            // Step 2b: resolve membership by the stable uid, not the
            // (reusable) username. A username freed at central and
            // re-registered by someone else must NOT inherit the previous
            // holder's member row / roles (this was C2); a member renamed at
            // central keeps theirs.
            bool member = false;
            if (candidate_uid > 0) {
                if (auto by_uid = db->get_member_by_uid(candidate_uid)) {
                    if (by_uid->username != candidate_username) {
                        // Same account, new display name — carry roles over.
                        db->rename_member(by_uid->username, candidate_username);
                        std::cout << "[Community] uid " << candidate_uid << " renamed "
                                  << by_uid->username << " -> " << candidate_username << "\n";
                    }
                    member = true;
                } else if (auto by_name = db->get_member(candidate_username);
                           by_name && by_name->uid == 0) {
                    // Pre-Theme-A row reclaiming its identity via TOFU.
                    db->set_member_uid(candidate_username, candidate_uid);
                    member = true;
                }
                // A row under this name with a different nonzero uid is a
                // reused username — not this member; fall through to invite,
                // where add_member() evicts the stale row without inheritance.
            } else {
                // Legacy token with no uid claim: the name is all we have.
                member = db->is_member(candidate_username);
            }
            if (!member) {
                if (invite_code.empty()) {
                    // Public servers accept invite-less joins straight from the
                    // discovery list; private servers stay invite-only. (Bans
                    // were already rejected above, so a public join can't admit
                    // a banned user.)
                    if (!db->public_listing()) {
                        send_auth_response(false,
                            "Membership required. An invite code is needed to join this server.",
                            "not_member");
                        manager_.leave(shared_from_this());
                        close_after_flush();
                        return;
                    }
                    if (!db->add_member(candidate_username, candidate_uid)) {
                        send_auth_response(false, "Failed to record membership.", "auth");
                        manager_.leave(shared_from_this());
                        close_after_flush();
                        return;
                    }
                    std::cout << "[Community] " << candidate_username
                              << " joined public server directly\n";
                } else {
                    chatproj::DbInvite consumed;
                    auto result = db->redeem_invite(invite_code, candidate_username, candidate_uid, &consumed);
                    switch (result) {
                        case chatproj::InviteResult::Ok:
                            if (!db->add_member(candidate_username, candidate_uid)) {
                                send_auth_response(false, "Failed to record membership.", "auth");
                                manager_.leave(shared_from_this());
                                close_after_flush();
                                return;
                            }
                            std::cout << "[Community] " << candidate_username
                                      << " joined via invite " << invite_code << "\n";
                            break;
                        case chatproj::InviteResult::AlreadyMember:
                            // Race — someone was added between is_member and redeem.
                            // Accept the connection; nothing more to do.
                            break;
                        case chatproj::InviteResult::Banned:
                            send_auth_response(false, "You are banned from this server.", "banned");
                            manager_.leave(shared_from_this());
                            close_after_flush();
                            return;
                        case chatproj::InviteResult::Unknown:
                        case chatproj::InviteResult::Expired:
                        case chatproj::InviteResult::Exhausted:
                        default:
                            send_auth_response(false,
                                "Invite code is invalid, expired, or has been used up.",
                                "invalid_invite");
                            manager_.leave(shared_from_this());
                            close_after_flush();
                            return;
                    }
                }
            }

            // Step 3: accept.
            authenticated_ = true;
            username_ = candidate_username;
            uid_ = candidate_uid;
            token_ = token;
            // Back-fill the stable uid on members that joined before Theme A.
            if (uid_ > 0) db->set_member_uid(username_, uid_);

            constexpr size_t UDP_KEY_LEN = chatproj::SENDER_ID_SIZE - 1;
            if (token_.size() >= UDP_KEY_LEN) {
                udp_key_ = token_.substr(token_.size() - UDP_KEY_LEN);
            } else {
                udp_key_ = token_;
            }
            manager_.register_authenticated(shared_from_this());
            arm_deadline(manager_.idle_timeout_seconds());
            if (auto m = db->get_member(username_)) {
                set_server_voice_flags(m->server_muted, m->server_deafened);
            }

            std::cout << "[Community] Authorized user: " << username_ << "\n";
            send_auth_response(true, "Authentication successful.", "");
            // Push the role list up-front so the client can resolve
            // member role_ids and gate its admin UI without a round trip.
            send_packet(build_role_list_packet(manager_.db()));
            manager_.send_initial_voice_presences(shared_from_this());
            // Roster delta: a brand-new member (invite redemption) or a
            // returning member flipping online — one MemberInfo to everyone.
            manager_.emit_member_upsert(username_);
            // BAN_MEMBERS holders get the ban list up-front.
            if (manager_.authz().check(chatproj::Action::ViewBans, {username_, "", ""})) {
                chatproj::Packet bl;
                bl.set_type(chatproj::Packet::BAN_LIST_RES);
                fill_ban_list(bl.mutable_ban_list_res(), manager_.db(), manager_.roster_revision());
                send_packet(bl);
            }
            // Auto-rejoin: push membership to central so this user gets
            // auto-rejoined on future logins. Idempotent — fires on
            // every successful auth and serves as the bootstrap for
            // pre-feature memberships.
            manager_.sync_membership_register(username_);
            return;
        }

        // Client keepalive ping — just acknowledge, no response needed.
        // Skip auth check: pings may arrive before auth completes.
        if (packet.type() == chatproj::Packet::CLIENT_PING) {
            return;
        }

        // Drop unauthenticated traffic
        if (!authenticated_) return;

        // Membership is re-validated on every post-auth packet (one PK
        // lookup). Kick/ban now close every session of the target, but
        // this closes the residual window — a session whose membership
        // row vanished by any path (ban via another node in the future,
        // manual DB edit, a race with its own leave) must not keep
        // posting, joining voice or streaming on a stale auth.
        if (auto* db = manager_.db(); db && !db->is_member(username_)) {
            std::cout << "[Community] Dropping session of " << username_
                      << ": no longer a member\n";
            manager_.leave(shared_from_this());
            close_connection();
            return;
        }

        // Per-session rate limits. Each packet class draws from its own
        // bucket; an exhausted bucket drops the packet (CHANNEL_MSG tells
        // the sender so its optimistic bubble doesn't hang). Limits are
        // far above anything a human produces through the UI.
        if (!rate_limit_allows(packet.type())) {
            if (rate_limit_log_bucket_.try_take()) {
                std::cout << "[Community] Rate-limited " << username_
                          << " on packet type " << packet.type() << "\n";
            }
            if (packet.type() == chatproj::Packet::CHANNEL_MSG) {
                reject_channel_msg(packet.channel_msg(), "You're sending messages too fast.");
            }
            return;
        }

        // --- JOIN VOICE CHANNEL ---
        if (packet.type() == chatproj::Packet::JOIN_VOICE_REQ) {
            const auto& jvr = packet.join_voice_req();
            std::string target_channel = jvr.channel_id();
            // Only real voice channels. Without this check any string
            // becomes a key in voice_channels_ (unbounded map growth) and
            // ghost channels leak into every member's presence snapshots.
            std::optional<chatproj::DbChannel> ch;
            if (auto* db = manager_.db()) ch = db->get_channel(target_channel);
            if (!ch || ch->type != 1) {
                std::cout << "[Community] Rejected JOIN_VOICE_REQ from " << username_
                          << ": unknown or non-voice channel '" << target_channel << "'\n";
                return;
            }
            if (auto a = manager_.authz().check(chatproj::Action::ConnectVoice,
                                                {username_, target_channel, ""}); !a) {
                std::cout << "[Community] Rejected JOIN_VOICE_REQ from " << username_
                          << ": " << a.reason << "\n";
                send_simple_mod_res(chatproj::Packet::MOD_ACTION_RES, false, a.reason,
                                    username_, "voice");
                return;
            }
            // Capture client capabilities (Plan A Group 7). Empty when sent
            // by a legacy client; treated downstream as "H.264 only".
            if (jvr.has_capabilities()) {
                if (!capabilities_within_limits(jvr.capabilities())) {
                    std::cout << "[Community] Rejected JOIN_VOICE_REQ from " << username_
                              << ": oversized capabilities\n";
                    return;
                }
                set_capabilities(jvr.capabilities());
            }
            // A stream is bound to the channel it was started in. Moving
            // to another channel must end it (LEAVE_VOICE_REQ already
            // does) — otherwise active_streams_[old][user] lingers: the
            // old channel keeps advertising a live stream nobody can
            // watch, it counts against max_streams_per_channel_, and
            // thumbnails can still be pushed into it.
            if (!current_voice_channel_.empty() && current_voice_channel_ != target_channel) {
                manager_.stop_stream(shared_from_this(), current_voice_channel_);
            }
            manager_.join_voice_channel(shared_from_this(), target_channel, current_voice_channel_);
            current_voice_channel_ = target_channel;
            std::cout << "[Community] " << username_ << " joined voice channel " << target_channel << "\n";
        }

        // --- UPDATE CAPABILITIES (mid-session caps refresh — spec §4.6) ---
        else if (packet.type() == chatproj::Packet::UPDATE_CAPABILITIES_REQ) {
            const auto& req = packet.update_capabilities_req();
            if (req.has_capabilities()) {
                if (!capabilities_within_limits(req.capabilities())) {
                    std::cout << "[Community] Rejected UPDATE_CAPABILITIES_REQ from "
                              << username_ << ": oversized capabilities\n";
                    return;
                }
                set_capabilities(req.capabilities());
                if (!current_voice_channel_.empty()) {
                    manager_.broadcast_voice_presence(current_voice_channel_);
                }
            }
        }

        // --- LEAVE VOICE CHANNEL ---
        else if (packet.type() == chatproj::Packet::LEAVE_VOICE_REQ) {
            manager_.stop_stream(shared_from_this(), current_voice_channel_); // Stop streaming if they leave
            manager_.leave_voice_channel(shared_from_this(), current_voice_channel_);
            std::cout << "[Community] " << username_ << " left voice channel " << current_voice_channel_ << "\n";
            current_voice_channel_ = "";
            is_muted_ = false;
            is_deafened_ = false;
        }

        // --- START STREAM ---
        else if (packet.type() == chatproj::Packet::START_STREAM_REQ) {
            const auto& req = packet.start_stream_req();
            // Streaming happens in the voice channel you're connected to —
            // the client always sends its connected channel. Enforcing it
            // here keeps active_streams_ free of arbitrary channel keys.
            if (current_voice_channel_.empty() ||
                req.channel_id() != current_voice_channel_) {
                std::cout << "[Community] Rejected START_STREAM_REQ from " << username_
                          << ": not in voice channel '" << req.channel_id() << "'\n";
                return;
            }
            if (auto a = manager_.authz().check(chatproj::Action::Stream,
                                                {username_, req.channel_id(), ""}); !a) {
                std::cout << "[Community] Rejected START_STREAM_REQ from " << username_
                          << ": " << a.reason << "\n";
                send_simple_mod_res(chatproj::Packet::MOD_ACTION_RES, false, a.reason,
                                    username_, "stream");
                return;
            }
            // Codec defaults: legacy clients (pre-negotiation) leave both
            // chosen_codec and enforced_codec as CODEC_UNKNOWN. Treat
            // chosen_codec=UNKNOWN as H264_HW (the only codec they ever sent)
            // so existing viewers' badges show something sensible. enforced
            // stays UNKNOWN — no enforcement.
            chatproj::VideoCodec chosen = req.chosen_codec();
            if (chosen == chatproj::CODEC_UNKNOWN) chosen = chatproj::CODEC_H264_HW;
            manager_.start_stream(
                shared_from_this(), req.channel_id(), req.has_audio(),
                static_cast<uint32_t>(req.target_fps()),
                req.resolution_width(), req.resolution_height(),
                chosen, req.enforced_codec());
            std::cout << "[Community] " << username_ << " started screen share in " << req.channel_id() << "\n";
        }

        // --- STOP STREAM ---
        else if (packet.type() == chatproj::Packet::STOP_STREAM_REQ) {
            const auto& req = packet.stop_stream_req();
            manager_.stop_stream(shared_from_this(), req.channel_id());
            std::cout << "[Community] " << username_ << " stopped screen share in " << req.channel_id() << "\n";
        }

        // --- WATCH STREAM ---
        else if (packet.type() == chatproj::Packet::WATCH_STREAM_REQ) {
            const auto& req = packet.watch_stream_req();
            // Watching requires being in that voice channel (the client
            // only offers watch buttons for the connected channel).
            if (current_voice_channel_.empty() ||
                req.channel_id() != current_voice_channel_) {
                std::cout << "[Community] Rejected WATCH_STREAM_REQ from " << username_
                          << ": not in voice channel '" << req.channel_id() << "'\n";
                return;
            }
            if (!manager_.authz().check(chatproj::Action::ViewChannel,
                                        {username_, req.channel_id(), ""})) {
                return;
            }
            // Ignore watch requests for a user who isn't actually streaming:
            // avoids bloating the watcher map and firing a keyframe/notify for
            // a non-stream (part of the keyframe-amplification surface).
            if (!manager_.has_active_stream(req.channel_id(), req.target_username())) {
                std::cout << "[Community] Rejected WATCH_STREAM_REQ from " << username_
                          << ": '" << req.target_username() << "' is not streaming\n";
                return;
            }
            // Plan C: defensive — drop the request if the stream is
            // codec-locked and the watcher can't decode that codec.
            if (manager_.watcher_blocked_by_enforcement(
                    req.channel_id(), req.target_username(), shared_from_this())) {
                std::cout << "[Community] dropped WATCH_STREAM_REQ from " << username_
                          << " (can't decode enforced codec on " << req.target_username()
                          << "'s stream)\n";
            } else {
                bool newly_watching = manager_.add_watcher(
                    shared_from_this(), req.channel_id(), req.target_username());
                std::cout << "[Community] " << username_ << " watching " << req.target_username() << "'s stream in " << req.channel_id() << "\n";
                // Only on the FIRST subscribe: notify the streamer (LCD
                // recompute) and force one keyframe. Re-sending WATCH for an
                // already-watched stream must not re-trigger these — that was a
                // keyframe-amplification vector (loop WATCH to force continuous
                // IDRs, collapsing quality + spiking the streamer's uplink).
                // relay_keyframe_request_internal is rate-limited per streamer
                // as a backstop for the WATCH/STOP/WATCH toggle variant.
                if (newly_watching) {
                    manager_.notify_streamer_of_watcher(
                        req.channel_id(), req.target_username(), username_,
                        chatproj::StreamWatcherNotify::JOINED);
                    manager_.relay_keyframe_request_internal(req.target_username());
                    // Push the updated watcher count to everyone in the channel.
                    manager_.broadcast_stream_presence(req.channel_id());
                }
            }
        }

        // --- STOP WATCHING STREAM ---
        else if (packet.type() == chatproj::Packet::STOP_WATCHING_REQ) {
            const auto& req = packet.stop_watching_req();
            // Only a real watcher triggers the streamer notify + presence
            // rebroadcast — otherwise STOP_WATCHING for a stream you never
            // subscribed to was a free all-session broadcast and a fake
            // LEFT signal into the streamer's cooldown logic.
            if (!manager_.remove_watcher(shared_from_this(), req.channel_id(), req.target_username())) {
                return;
            }
            std::cout << "[Community] " << username_ << " stopped watching " << req.target_username() << "'s stream\n";
            // Plan C: notify streamer so cooldown timer can start.
            manager_.notify_streamer_of_watcher(
                req.channel_id(), req.target_username(), username_,
                chatproj::StreamWatcherNotify::LEFT);
            // Push the updated watcher count to everyone in the channel.
            manager_.broadcast_stream_presence(req.channel_id());
        }

        // --- STREAM CODEC CHANGED (Plan C) ---
        else if (packet.type() == chatproj::Packet::STREAM_CODEC_CHANGED_NOTIFY) {
            manager_.handle_stream_codec_changed(packet, username_);
        }

        // --- STREAM THUMBNAIL UPDATE ---
        else if (packet.type() == chatproj::Packet::STREAM_THUMBNAIL_UPDATE) {
            strip_client_envelope(packet);
            auto* update = packet.mutable_stream_thumbnail_update();
            // Cap thumbnail size — these are small JPEG previews. Without a
            // cap any member could repeatedly push ~2 MB blobs (up to the
            // TCP frame limit) into the per-username cache. Oversized
            // updates are dropped silently.
            constexpr size_t MAX_STREAM_THUMB_BYTES = 128 * 1024;
            if (update->thumbnail_data().size() <= MAX_STREAM_THUMB_BYTES &&
                manager_.has_active_stream(update->channel_id(), username_)) {
                update->set_owner_username(username_); // Enforce identity
                std::string channel_id = update->channel_id();
                // Stash a copy for on-demand popup fetches before the
                // broadcast — bytes are owned by the protobuf so the
                // helper copies them.
                manager_.update_thumbnail_cache(username_, update->thumbnail_data());
                // Broadcast to all voice channel participants (not just watchers)
                manager_.broadcast_to_voice_channel_tcp(packet, channel_id);
            }
        }

        // --- FETCH_STREAM_THUMBNAIL_REQ ---
        // Sent by clients when a UserPopup opens for a streaming user.
        // Replies with the latest cached JPEG (or empty bytes if no
        // frame has arrived yet). Authenticated callers only — the
        // session is already authenticated to this community server,
        // and the only way to know the streamer's username is to have
        // received a stream-presence event from us, so no extra ACL
        // check is needed.
        else if (packet.type() == chatproj::Packet::FETCH_STREAM_THUMBNAIL_REQ) {
            if (!authenticated_) return;
            const auto& req = packet.fetch_stream_thumbnail_req();
            const std::string& target = req.owner_username();

            chatproj::Packet response;
            response.set_type(chatproj::Packet::FETCH_STREAM_THUMBNAIL_RES);
            auto* res = response.mutable_fetch_stream_thumbnail_res();
            res->set_owner_username(target);
            std::vector<uint8_t> bytes;
            if (manager_.get_thumbnail(target, bytes)) {
                res->set_thumbnail_data(bytes.data(), bytes.size());
            }
            send_packet(response);
        }

        // --- VOICE STATE NOTIFY (mute/deafen) ---
        else if (packet.type() == chatproj::Packet::VOICE_STATE_NOTIFY) {
            const auto& notify = packet.voice_state_notify();
            is_muted_ = notify.is_muted();
            is_deafened_ = notify.is_deafened();
            if (!current_voice_channel_.empty()) {
                manager_.broadcast_voice_presence(current_voice_channel_);
            }
        }

        // --- CHANNEL MESSAGE ROUTING ---
        else if (packet.type() == chatproj::Packet::CHANNEL_MSG) {
            chatproj::Packet routed = packet;
            strip_client_envelope(routed);
            auto* msg = routed.mutable_channel_msg();
            msg->set_sender(username_); // Enforce identity

            // The channel must exist and not be a category header.
            // Otherwise any member can persist rows under arbitrary (or
            // message-less) channel ids that no retention sweep, wipe,
            // or history fetch would ever visit.
            {
                std::optional<chatproj::DbChannel> ch;
                if (auto* db = manager_.db()) ch = db->get_channel(msg->channel_id());
                if (!ch || ch->type == 2) {
                    std::cout << "[Community] Dropped CHANNEL_MSG from " << username_
                              << " to unknown/category channel '"
                              << msg->channel_id() << "'\n";
                    return;
                }
            }
            // Set once the message passes slowmode and is about to be
            // accepted — the window is stamped only AFTER a successful
            // persist (below), so an oversized / empty / unsaved message
            // never consumes it (M3).
            bool record_slowmode = false;
            // Permissions v2: SEND_MESSAGES (and ATTACH_FILES when the
            // message carries attachments) resolved for this channel.
            {
                const chatproj::AuthCtx ctx{username_, msg->channel_id(), ""};
                auto a = manager_.authz().check(chatproj::Action::SendMessage, ctx);
                if (a && msg->attachments_size() > 0) {
                    a = manager_.authz().check(chatproj::Action::AttachFiles, ctx);
                }
                if (!a) {
                    reject_channel_msg(*msg, a.reason);
                    return;
                }
                // Slowmode: one message per `slowmode_seconds` per channel
                // unless the sender may MANAGE_MESSAGES there. Keyed per user
                // in the manager (not per session) so a second connection or
                // a reconnect can't reset it.
                std::optional<chatproj::DbChannel> sm;
                if (auto* db = manager_.db()) sm = db->get_channel(msg->channel_id());
                if (sm && sm->slowmode_seconds > 0 &&
                    !(manager_.authz().channel_permissions(username_, msg->channel_id()) & chatproj::perms::kManageMessages)) {
                    const int64_t wait = manager_.slowmode_remaining(
                        username_, msg->channel_id(), sm->slowmode_seconds);
                    if (wait > 0) {
                        reject_channel_msg(*msg, "Slowmode is on — wait " + std::to_string(wait) + "s.");
                        return;
                    }
                    record_slowmode = true;
                }
            }
            // Size cap — the 2 MB frame cap alone would let one message
            // carry ~2 MB of text, persisted and fanned out to every
            // member. 64 KB is far above anything typed by hand and
            // comfortably above big code-block pastes (the client has no
            // composer limit yet, so the cap must not silently eat a
            // legitimate paste). Empty messages (no text, no attachments)
            // are dropped too.
            constexpr size_t MAX_CHANNEL_MSG_BYTES = 64 * 1024;
            if (msg->content().size() > MAX_CHANNEL_MSG_BYTES) {
                std::cout << "[Community] Dropped oversized CHANNEL_MSG from "
                          << username_ << " (" << msg->content().size() << " bytes)\n";
                return;
            }
            if (msg->content().empty() && msg->attachments_size() == 0) {
                return;
            }
            // The client caps a message at 10 attachments; enforce it
            // here too so bind_attachments' IN-list stays bounded.
            constexpr int MAX_ATTACHMENTS_PER_MESSAGE = 10;
            if (msg->attachments_size() > MAX_ATTACHMENTS_PER_MESSAGE) {
                reject_channel_msg(*msg, "Too many attachments on one message.");
                return;
            }

            // Persist before broadcast so the id we echo to clients matches
            // what history_res will return. Server stamps the authoritative
            // timestamp at the same time to ensure retention ordering isn't
            // subject to client clock drift.
            const int64_t now_ts = static_cast<int64_t>(std::time(nullptr));
            msg->set_timestamp(now_ts);
            int64_t new_id = 0;
            if (auto* db = manager_.db()) {
                new_id = db->insert_message(
                    msg->channel_id(), username_, msg->content(), now_ts, msg->reply_to());
                if (new_id > 0) {
                    msg->set_id(new_id);
                    // insert_message drops an invalid reply_to (missing / wrong
                    // channel); reflect the stored value in the broadcast so
                    // clients don't render a preview for a reference we ignored.
                    // When valid, embed the parent's author + content so every
                    // client renders the quoted preview without needing the
                    // parent in its loaded window.
                    if (msg->reply_to() > 0) {
                        auto parent = db->get_message_preview(msg->channel_id(), msg->reply_to());
                        if (!parent) {
                            msg->set_reply_to(0);
                        } else {
                            msg->set_reply_to_sender(parent->sender);
                            msg->set_reply_to_content(parent->content);
                            for (int32_t k : parent->attachment_kinds)
                                msg->add_reply_to_attachment_kinds(k);
                        }
                    }
                } else {
                    // Don't broadcast a message that isn't in history:
                    // clients would get an id=0 row they can never
                    // delete and that vanishes on the next reload. Tell
                    // the sender instead so their optimistic bubble is
                    // withdrawn.
                    std::cerr << "[Community] Failed to persist CHANNEL_MSG from "
                              << username_ << " in #" << msg->channel_id() << "\n";
                    reject_channel_msg(*msg, "Message could not be saved — try again.");
                    return;
                }
            }
            // Message accepted — now stamp the slowmode window (M3).
            if (record_slowmode) manager_.slowmode_record(username_, msg->channel_id());

            // Bind any pre-uploaded attachments the client referenced. Only
            // the client's own ready uploads for this channel bind; anything
            // else is silently dropped (reject without ceremony — we never
            // want one user attaching another's upload).
            if (auto* db = manager_.db(); db && new_id > 0 && msg->attachments_size() > 0) {
                std::vector<int64_t> requested;
                requested.reserve(msg->attachments_size());
                for (const auto& a : msg->attachments()) {
                    if (a.id() > 0) requested.push_back(a.id());
                }
                auto bound_ids = db->bind_attachments(
                    requested, new_id, msg->channel_id(), username_);

                // Rebuild the attachments field with authoritative rows so
                // downstream consumers see every field (filename, mime, size,
                // created_at, position, etc.) without trusting client input.
                msg->clear_attachments();
                if (!bound_ids.empty()) {
                    auto rows = db->fetch_attachments_for_messages({ new_id });
                    for (const auto& row : rows) {
                        auto* pa = msg->add_attachments();
                        pa->set_id(row.id);
                        pa->set_message_id(row.message_id);
                        pa->set_kind(static_cast<chatproj::Attachment::Kind>(row.kind));
                        pa->set_filename(row.filename);
                        pa->set_mime(row.mime);
                        pa->set_size_bytes(row.size_bytes);
                        pa->set_url("/attachments/" + std::to_string(row.id));
                        pa->set_position(row.position);
                        pa->set_created_at(row.created_at);
                        pa->set_purged_at(row.purged_at);
                        pa->set_width(static_cast<uint32_t>(row.width));
                        pa->set_height(static_cast<uint32_t>(row.height));
                        pa->set_thumbnail_size_bytes(
                            static_cast<uint32_t>(row.thumbnail_size_bytes));
                        pa->set_thumbnail_sizes_mask(
                            static_cast<uint32_t>(row.thumbnail_sizes_mask));
                        pa->set_duration_ms(static_cast<uint32_t>(row.duration_ms));
                        pa->set_placeholder(row.placeholder);
                    }
                }
            } else {
                // No attachments or no DB — drop any stale client-sent attachment
                // list so we never broadcast unverified data.
                msg->clear_attachments();
            }

            manager_.broadcast_to_channel(routed, msg->channel_id());
            std::cout << "[#" << msg->channel_id() << "] " << username_
                      << ": " << msg->content()
                      << (msg->attachments_size() > 0
                          ? (" [+" + std::to_string(msg->attachments_size()) + " attachment(s)]")
                          : "") << "\n";
        }

        // --- CHANNEL HISTORY REQUEST ---
        else if (packet.type() == chatproj::Packet::CHANNEL_HISTORY_REQ) {
            auto* db = manager_.db();
            chatproj::Packet p;
            p.set_type(chatproj::Packet::CHANNEL_HISTORY_RES);
            auto* res = p.mutable_channel_history_res();
            const auto& req = packet.channel_history_req();
            res->set_channel_id(req.channel_id());
            if (!db) { send_packet(p); return; }
            // Permissions v2: no READ_HISTORY (or no VIEW) → empty page.
            if (!manager_.authz().check(chatproj::Action::ReadHistory,
                                        {username_, req.channel_id(), ""})) {
                send_packet(p);
                return;
            }

            bool has_more = false;        // older messages exist (above)
            bool has_more_after = false;  // newer messages exist (below)
            std::vector<chatproj::DbMessage> msgs;  // oldest→newest
            if (req.around_id() > 0) {
                // Jump-to-message: a window centered on around_id.
                msgs = db->fetch_messages_around(
                    req.channel_id(), req.around_id(),
                    req.limit() > 0 ? req.limit() : 25,
                    &has_more, &has_more_after);
            } else if (req.after_id() > 0) {
                // Downward pagination from a jumped/windowed view.
                msgs = db->fetch_messages_after(
                    req.channel_id(), req.after_id(), req.limit(), &has_more_after);
            } else {
                // Most-recent / older page (newest-first from the DB → reverse).
                auto desc = db->fetch_messages(
                    req.channel_id(), req.before_id(), req.limit(), &has_more);
                msgs.assign(desc.rbegin(), desc.rend());
            }

            // Load attachments for this page in one query.
            std::vector<int64_t> msg_ids;
            msg_ids.reserve(msgs.size());
            for (const auto& m : msgs) msg_ids.push_back(m.id);
            auto attachments = db->fetch_attachments_for_messages(msg_ids);

            std::unordered_map<int64_t, std::vector<const chatproj::DbAttachment*>> by_msg;
            for (const auto& a : attachments) {
                by_msg[a.message_id].push_back(&a);
            }

            // msgs is already oldest→newest (the fetch branches normalize it),
            // matching the order the client renders.
            for (auto it = msgs.begin(); it != msgs.end(); ++it) {
                auto* cm = res->add_messages();
                cm->set_id(it->id);
                cm->set_sender(it->sender);
                cm->set_channel_id(it->channel_id);
                cm->set_content(it->content);
                cm->set_timestamp(it->timestamp);
                cm->set_edited_at(it->edited_at);
                cm->set_reply_to(it->reply_to);
                cm->set_reply_to_sender(it->reply_to_sender);
                cm->set_reply_to_content(it->reply_to_content);
                for (int32_t k : it->reply_to_attachment_kinds)
                    cm->add_reply_to_attachment_kinds(k);
                auto atts_it = by_msg.find(it->id);
                if (atts_it != by_msg.end()) {
                    for (const auto* a : atts_it->second) {
                        auto* proto_a = cm->add_attachments();
                        proto_a->set_id(a->id);
                        proto_a->set_message_id(a->message_id);
                        proto_a->set_kind(
                            static_cast<chatproj::Attachment::Kind>(a->kind));
                        proto_a->set_filename(a->filename);
                        proto_a->set_mime(a->mime);
                        proto_a->set_size_bytes(a->size_bytes);
                        proto_a->set_url("/attachments/" + std::to_string(a->id));
                        proto_a->set_position(a->position);
                        proto_a->set_created_at(a->created_at);
                        proto_a->set_purged_at(a->purged_at);
                        proto_a->set_width(static_cast<uint32_t>(a->width));
                        proto_a->set_height(static_cast<uint32_t>(a->height));
                        proto_a->set_thumbnail_size_bytes(
                            static_cast<uint32_t>(a->thumbnail_size_bytes));
                        proto_a->set_thumbnail_sizes_mask(
                            static_cast<uint32_t>(a->thumbnail_sizes_mask));
                        proto_a->set_duration_ms(static_cast<uint32_t>(a->duration_ms));
                        proto_a->set_placeholder(a->placeholder);
                    }
                }
            }
            res->set_has_more(has_more);
            res->set_has_more_after(has_more_after);
            res->set_around_id(req.around_id());
            res->set_after_id(req.after_id());
            send_packet(p);
        }

        // --- CHANNEL UPDATE (retention settings) ---
        else if (packet.type() == chatproj::Packet::CHANNEL_UPDATE_REQ) {
            auto* db = manager_.db();
            chatproj::Packet p;
            p.set_type(chatproj::Packet::CHANNEL_UPDATE_RES);
            auto* res = p.mutable_channel_update_res();
            if (!db) {
                res->set_success(false);
                res->set_message("Server misconfigured.");
                send_packet(p);
                return;
            }
            const auto& req = packet.channel_update_req();
            if (auto a = manager_.authz().check(chatproj::Action::ManageChannel,
                                                {username_, req.channel_id(), ""}); !a) {
                res->set_success(false);
                res->set_message(a.reason);
                send_packet(p);
                return;
            }
            bool ok = db->update_channel_retention(
                req.channel_id(),
                req.retention_days_text(),
                req.retention_days_image(),
                req.retention_days_video(),
                req.retention_days_document(),
                req.retention_days_audio());
            // Voice bitrate rides the same update when present (proto3
            // optional — absent means "leave unchanged", so retention-only
            // updates from older clients can't reset it). The DB setter
            // only touches voice rows, so a stray value on a text channel
            // is a no-op.
            if (ok && req.has_voice_bitrate_kbps()) {
                db->set_channel_voice_bitrate(req.channel_id(),
                                              req.voice_bitrate_kbps());
            }
            if (ok && req.has_slowmode_seconds()) {
                db->set_channel_slowmode(req.channel_id(), req.slowmode_seconds());
            }
            if (ok) {
                db->add_audit(username_, "channel_update", "", req.channel_id(),
                              req.has_slowmode_seconds()
                                  ? "slowmode " + std::to_string(req.slowmode_seconds()) + "s"
                                  : "retention/bitrate");
            }
            res->set_success(ok);
            res->set_message(ok ? "Channel updated." : "Channel not found.");
            if (!ok) {
                // Failures go to the requester only — everyone else has
                // nothing to refresh.
                send_packet(p);
                return;
            }
            if (auto ch = db->get_channel(req.channel_id())) {
                fill_channel_info(res->mutable_channel(), *ch);
            }
            // Fan out to everyone who can see the channel so they get the new
            // retention settings immediately (rendered in the channel
            // sidebar + any open edit modals).
            manager_.broadcast_to_channel(p, req.channel_id());
        }

        // --- CHANNEL WIPE (owner-only nuke of all history in a channel) ---
        else if (packet.type() == chatproj::Packet::CHANNEL_WIPE_REQ) {
            auto* db = manager_.db();
            chatproj::Packet rsp;
            rsp.set_type(chatproj::Packet::CHANNEL_WIPE_RES);
            auto* res = rsp.mutable_channel_wipe_res();
            const auto& req = packet.channel_wipe_req();
            res->set_channel_id(req.channel_id());

            if (!db) {
                res->set_success(false);
                res->set_message("Server misconfigured.");
                send_packet(rsp);
                return;
            }
            if (auto a = manager_.authz().check(chatproj::Action::WipeChannel,
                                                {username_, req.channel_id(), ""}); !a) {
                res->set_success(false);
                res->set_message(a.reason);
                send_packet(rsp);
                return;
            }
            if (!db->get_channel(req.channel_id())) {
                res->set_success(false);
                res->set_message("Channel not found.");
                send_packet(rsp);
                return;
            }

            auto wipe = db->wipe_channel(req.channel_id());
            db->add_audit(username_, "channel_wipe", "", req.channel_id(),
                          std::to_string(wipe.deleted_message_count) + " messages");
            res->set_success(true);
            res->set_message("Channel wiped.");
            res->set_deleted_message_count(wipe.deleted_message_count);
            res->set_deleted_attachment_count(wipe.deleted_attachment_count);
            send_packet(rsp);

            // Filesystem cleanup — every blob plus its sibling thumbnail
            // variants. Errors are tolerated; a stray file just shows up
            // in the next retention sweep or a future wipe.
            for (const auto& path : wipe.unlink_paths) {
                std::error_code ec;
                std::filesystem::remove(path, ec);
                std::filesystem::remove(path + ".partial", ec);
                std::filesystem::remove(path + ".thumb.jpg", ec);
                std::filesystem::remove(path + ".thumb-320px.jpg", ec);
                std::filesystem::remove(path + ".thumb-640px.jpg", ec);
                std::filesystem::remove(path + ".thumb-1280px.jpg", ec);
            }

            // Broadcast to every authenticated session so they drop the
            // channel's locally-cached messages without re-fetching.
            chatproj::Packet bcast;
            bcast.set_type(chatproj::Packet::CHANNEL_WIPED);
            auto* bw = bcast.mutable_channel_wiped();
            bw->set_channel_id(req.channel_id());
            bw->set_wiped_at(static_cast<int64_t>(std::time(nullptr)));
            bw->set_wiped_by(username_);
            manager_.broadcast_to_channel(bcast, req.channel_id());

            std::cout << "[Community] #" << req.channel_id()
                      << " wiped by " << username_
                      << ": " << wipe.deleted_message_count << " messages, "
                      << wipe.deleted_attachment_count << " attachments\n";
        }

        // --- MESSAGE_DELETE_REQ ---
        // Per-message delete. Self-or-can_delete_others gate. Reuses
        // the wipe filesystem-unlink pattern for attachment blobs.
        else if (packet.type() == chatproj::Packet::MESSAGE_DELETE_REQ) {
            auto* db = manager_.db();
            const auto& req = packet.message_delete_req();

            // Always echo a RES so the renderer can clear the pending
            // snapshot. Build it now; populate success/message below.
            chatproj::Packet rsp;
            rsp.set_type(chatproj::Packet::MESSAGE_DELETE_RES);
            auto* res = rsp.mutable_message_delete_res();
            res->set_channel_id(req.channel_id());
            res->set_message_id(req.message_id());

            if (!db) {
                res->set_success(false);
                res->set_message("Server misconfigured.");
                send_packet(rsp);
                return;
            }
            if (req.channel_id().empty() || req.message_id() == 0) {
                res->set_success(false);
                res->set_message("Invalid request.");
                send_packet(rsp);
                return;
            }

            auto sender = db->get_message_sender(req.channel_id(), req.message_id());
            if (!sender) {
                res->set_success(false);
                res->set_message("Message not found.");
                send_packet(rsp);
                return;
            }

            if (*sender != username_) {
                if (auto a = manager_.authz().check(chatproj::Action::DeleteOthersMessage,
                                                    {username_, req.channel_id(), ""}); !a) {
                    res->set_success(false);
                    res->set_message(a.reason);
                    send_packet(rsp);
                    return;
                }
            }

            auto del = db->delete_message(req.channel_id(), req.message_id());
            if (!del.ok) {
                res->set_success(false);
                res->set_message("Failed to delete message.");
                send_packet(rsp);
                return;
            }

            res->set_success(true);
            res->set_message("");
            send_packet(rsp);
            if (*sender != username_) {
                db->add_audit(username_, "message_delete", *sender, req.channel_id(),
                              "message " + std::to_string(req.message_id()));
            }

            // Filesystem cleanup — mirror the CHANNEL_WIPE pattern.
            // Each storage_path may have sibling thumbnail variants;
            // remove them all, ignore errors (orphan files get swept
            // by retention or a future delete).
            for (const auto& path : del.unlink_paths) {
                std::error_code ec;
                std::filesystem::remove(path, ec);
                std::filesystem::remove(path + ".partial", ec);
                std::filesystem::remove(path + ".thumb.jpg", ec);
                std::filesystem::remove(path + ".thumb-320px.jpg", ec);
                std::filesystem::remove(path + ".thumb-640px.jpg", ec);
                std::filesystem::remove(path + ".thumb-1280px.jpg", ec);
            }

            // Broadcast deletion to every authenticated session.
            chatproj::Packet bcast;
            bcast.set_type(chatproj::Packet::CHANNEL_MESSAGE_DELETED);
            auto* bw = bcast.mutable_channel_message_deleted();
            bw->set_channel_id(req.channel_id());
            bw->set_message_id(req.message_id());
            bw->set_deleted_at(static_cast<int64_t>(std::time(nullptr)));
            bw->set_deleted_by(username_);
            manager_.broadcast_to_channel(bcast, req.channel_id());

            std::cout << "[Community] message " << req.message_id()
                      << " in #" << req.channel_id()
                      << " deleted by " << username_
                      << " (" << del.unlink_paths.size() << " attachments)\n";
        }

        // --- MESSAGE_EDIT_REQ ---
        // Edit your OWN message only (ownership enforced in the DB UPDATE).
        // Requires SEND_MESSAGES in the channel (which also blocks a timed-out
        // editor). Broadcasts CHANNEL_MESSAGE_EDITED on success.
        else if (packet.type() == chatproj::Packet::MESSAGE_EDIT_REQ) {
            auto* db = manager_.db();
            const auto& req = packet.message_edit_req();

            // Always echo a RES so the renderer can settle its optimistic edit.
            chatproj::Packet rsp;
            rsp.set_type(chatproj::Packet::MESSAGE_EDIT_RES);
            auto* res = rsp.mutable_message_edit_res();
            res->set_channel_id(req.channel_id());
            res->set_message_id(req.message_id());
            auto fail = [&](const std::string& m) {
                res->set_success(false); res->set_message(m); send_packet(rsp);
            };

            if (!db) { fail("Server misconfigured."); return; }
            if (req.channel_id().empty() || req.message_id() == 0) { fail("Invalid request."); return; }

            // Content bounds: same 64 KB cap as CHANNEL_MSG; empty rejected
            // (deleting is a separate action). Clamp is unnecessary — the
            // client's prost decodes what we store, and the cap guards size.
            const std::string& content = req.content();
            if (content.empty()) { fail("Message can't be empty."); return; }
            if (content.size() > 64 * 1024) { fail("Message too long."); return; }

            // Must be able to send here (permission + not timed out).
            if (auto a = manager_.authz().check(chatproj::Action::SendMessage,
                                                {username_, req.channel_id(), ""}); !a) {
                fail(a.reason); return;
            }

            const int64_t edited_at = static_cast<int64_t>(std::time(nullptr));
            // Ownership is enforced inside edit_message (sender must match).
            if (!db->edit_message(req.channel_id(), req.message_id(), username_, content, edited_at)) {
                fail("You can only edit your own messages.");
                return;
            }

            res->set_success(true);
            res->set_message("");
            send_packet(rsp);

            chatproj::Packet bcast;
            bcast.set_type(chatproj::Packet::CHANNEL_MESSAGE_EDITED);
            auto* ed = bcast.mutable_channel_message_edited();
            ed->set_channel_id(req.channel_id());
            ed->set_message_id(req.message_id());
            ed->set_content(content);
            ed->set_edited_at(edited_at);
            ed->set_editor(username_);
            manager_.broadcast_to_channel(bcast, req.channel_id());

            std::cout << "[Community] message " << req.message_id()
                      << " in #" << req.channel_id() << " edited by " << username_ << "\n";
        }

        // --- UPDATE_SERVER_PICTURE_REQ ---
        // Owner-only. Verifies size + ownership locally, then forwards
        // to central via shared-secret one-shot TLS (same pattern as
        // sync_invite_register / sync_membership_register).
        else if (packet.type() == chatproj::Packet::UPDATE_SERVER_PICTURE_REQ) {
            auto* db = manager_.db();
            const auto& req = packet.update_server_picture_req();

            chatproj::Packet rsp;
            rsp.set_type(chatproj::Packet::UPDATE_SERVER_PICTURE_RES);
            auto* res = rsp.mutable_update_server_picture_res();

            if (!db) {
                res->set_success(false);
                res->set_message("Server misconfigured.");
                send_packet(rsp);
                return;
            }
            if (auto a = manager_.authz().check(chatproj::Action::ManageServer, {username_, "", ""}); !a) {
                res->set_success(false);
                res->set_message(a.reason);
                send_packet(rsp);
                return;
            }
            if (req.data().size() > 1024 * 1024) {
                res->set_success(false);
                res->set_message("Image exceeds 1 MB.");
                send_packet(rsp);
                return;
            }

            std::string version = req.data().empty() ? "" : sha256_hex(req.data());
            res->set_success(true);
            res->set_message("");
            res->set_version(version);
            send_packet(rsp);

            // Forward to central — fire-and-forget. SessionManager
            // owns the central-host/port/secret config.
            manager_.sync_server_picture(req.data(), version);

            std::cout << "[Community] server picture "
                      << (req.data().empty() ? "removed" : "updated")
                      << " by " << username_
                      << " (" << req.data().size() << " bytes)\n";
        }

        // --- INVITE: CREATE ---
        else if (packet.type() == chatproj::Packet::INVITE_CREATE_REQ) {
            auto* db = manager_.db();
            if (!db) return;
            if (auto a = manager_.authz().check(chatproj::Action::ManageInvites, {username_, "", ""}); !a) {
                send_simple_mod_res(chatproj::Packet::INVITE_CREATE_RES, false, a.reason, "", "");
                return;
            }
            const auto& req = packet.invite_create_req();
            const int64_t now_ts = static_cast<int64_t>(std::time(nullptr));
            if (req.expires_at() != 0 && req.expires_at() <= now_ts) {
                send_simple_mod_res(chatproj::Packet::INVITE_CREATE_RES, false,
                                    "Invite expiry is in the past.", "", "");
                return;
            }
            // Negative max_uses used to be stored verbatim and read as
            // "unlimited" by every `> 0` check — normalise to 0.
            const int32_t max_uses = req.max_uses() < 0 ? 0 : req.max_uses();
            auto created = db->create_invite(username_, req.expires_at(), max_uses);

            chatproj::Packet p;
            p.set_type(chatproj::Packet::INVITE_CREATE_RES);
            auto* res = p.mutable_invite_create_res();
            if (created) {
                res->set_success(true);
                res->set_message("Invite created.");
                auto* info = res->mutable_invite();
                info->set_code(created->code);
                info->set_created_by(created->created_by);
                info->set_created_at(created->created_at);
                info->set_expires_at(created->expires_at);
                info->set_max_uses(created->max_uses);
                info->set_uses(created->uses);
            } else {
                res->set_success(false);
                res->set_message("Failed to create invite.");
            }
            send_packet(p);
            if (created) {
                manager_.sync_invite_register(created->code, created->expires_at);
                db->add_audit(username_, "invite_create", created->code, "",
                              max_uses ? "max uses " + std::to_string(max_uses) : "unlimited");
            }
        }

        // --- INVITE: LIST ---
        else if (packet.type() == chatproj::Packet::INVITE_LIST_REQ) {
            auto* db = manager_.db();
            chatproj::Packet p;
            p.set_type(chatproj::Packet::INVITE_LIST_RES);
            auto* res = p.mutable_invite_list_res();
            if (!db) {
                res->set_success(false);
                res->set_message("Server misconfigured.");
                send_packet(p);
                return;
            }
            if (auto a = manager_.authz().check(chatproj::Action::ManageInvites, {username_, "", ""}); !a) {
                res->set_success(false);
                res->set_message(a.reason);
                send_packet(p);
                return;
            }
            res->set_success(true);
            for (const auto& inv : db->list_invites()) {
                auto* info = res->add_invites();
                info->set_code(inv.code);
                info->set_created_by(inv.created_by);
                info->set_created_at(inv.created_at);
                info->set_expires_at(inv.expires_at);
                info->set_max_uses(inv.max_uses);
                info->set_uses(inv.uses);
            }
            send_packet(p);
        }

        // --- INVITE: REVOKE ---
        else if (packet.type() == chatproj::Packet::INVITE_REVOKE_REQ) {
            auto* db = manager_.db();
            chatproj::Packet p;
            p.set_type(chatproj::Packet::INVITE_REVOKE_RES);
            auto* res = p.mutable_invite_revoke_res();
            const std::string& code = packet.invite_revoke_req().code();
            res->set_code(code);
            if (!db) {
                res->set_success(false);
                res->set_message("Server misconfigured.");
                send_packet(p);
                return;
            }
            if (auto a = manager_.authz().check(chatproj::Action::ManageInvites, {username_, "", ""}); !a) {
                res->set_success(false);
                res->set_message(a.reason);
                send_packet(p);
                return;
            }
            bool ok = db->revoke_invite(code);
            res->set_success(ok);
            res->set_message(ok ? "Invite revoked." : "Invite not found.");
            send_packet(p);
            if (ok) {
                manager_.sync_invite_unregister(code);
                db->add_audit(username_, "invite_revoke", code, "", "");
            }
        }

        // --- MEMBER LIST (paged snapshot; deltas keep it live) ---
        else if (packet.type() == chatproj::Packet::MEMBER_LIST_REQ) {
            const auto& req = packet.member_list_req();
            chatproj::Packet p;
            p.set_type(chatproj::Packet::MEMBER_LIST_RES);
            manager_.fill_member_page(req.after(), req.limit(), p.mutable_member_list_res());
            send_packet(p);
        }

        // --- BAN LIST ---
        else if (packet.type() == chatproj::Packet::BAN_LIST_REQ) {
            auto* db = manager_.db();
            chatproj::Packet p;
            p.set_type(chatproj::Packet::BAN_LIST_RES);
            auto* res = p.mutable_ban_list_res();
            res->set_revision(manager_.roster_revision());
            if (!db) { res->set_success(false); res->set_message("Server misconfigured."); send_packet(p); return; }
            if (auto a = manager_.authz().check(chatproj::Action::ViewBans, {username_, "", ""}); !a) {
                res->set_success(false);
                res->set_message(a.reason);
                send_packet(p);
                return;
            }
            fill_ban_list(res, db, manager_.roster_revision());
            send_packet(p);
        }

        // --- KICK MEMBER ---
        else if (packet.type() == chatproj::Packet::KICK_MEMBER_REQ) {
            auto* db = manager_.db();
            if (!db) return;
            const std::string& target = packet.kick_member_req().username();
            const std::string& reason = packet.kick_member_req().reason();
            // Permission + owner/self guards + hierarchy all live in authz.
            if (auto a = manager_.authz().check(chatproj::Action::KickMember,
                                                {username_, "", target}); !a) {
                send_simple_mod_res(chatproj::Packet::MOD_ACTION_RES, false, a.reason, target, "kick");
                return;
            }
            bool removed = db->remove_member(target);
            // Sync the revoke to central unconditionally — force_disconnect
            // early-returns for offline targets, and leaving the central
            // membership row behind would auto-rejoin the kicked user into
            // a server that then rejects them. Idempotent on central.
            manager_.sync_membership_revoke(target);
            // Even if they weren't in the members table, force-disconnect
            // any live session so the UI reflects the action.
            const size_t closed = manager_.force_disconnect(target, "kick", reason, username_);
            send_simple_mod_res(chatproj::Packet::MOD_ACTION_RES, removed,
                                removed ? "Member kicked." : "User is not a member.",
                                target, "kick");
            (void)closed;
            if (removed) {
                manager_.emit_member_remove(target);
                db->add_audit(username_, "kick", target, "", reason.empty() ? "" : "reason: " + reason);
            }
        }

        // --- BAN MEMBER ---
        else if (packet.type() == chatproj::Packet::BAN_MEMBER_REQ) {
            auto* db = manager_.db();
            if (!db) return;
            const std::string& target = packet.ban_member_req().username();
            const std::string& reason = packet.ban_member_req().reason();
            if (auto a = manager_.authz().check(chatproj::Action::BanMember,
                                                {username_, "", target}); !a) {
                send_simple_mod_res(chatproj::Packet::MOD_ACTION_RES, false, a.reason, target, "ban");
                return;
            }
            const int64_t now_ts = static_cast<int64_t>(std::time(nullptr));
            int64_t expires_at = packet.ban_member_req().expires_at();
            if (expires_at != 0 && expires_at <= now_ts) {
                send_simple_mod_res(chatproj::Packet::MOD_ACTION_RES, false,
                                    "Ban expiry is in the past.", target, "ban");
                return;
            }
            // "Delete last N seconds of messages" — clamped to 7 days.
            int32_t purge_seconds = packet.ban_member_req().delete_message_seconds();
            if (purge_seconds < 0) purge_seconds = 0;
            if (purge_seconds > 7 * 86400) purge_seconds = 7 * 86400;

            bool ok = db->add_ban(target, username_, reason, expires_at);
            // Unconditional for the same reason as the kick path: the
            // target may be offline and force_disconnect would skip it.
            manager_.sync_membership_revoke(target);
            const size_t closed = manager_.force_disconnect(target, "ban", reason, username_, expires_at);
            send_simple_mod_res(chatproj::Packet::MOD_ACTION_RES, ok,
                                ok ? "Member banned." : "Ban failed.",
                                target, "ban");
            (void)closed;
            if (ok) {
                manager_.emit_member_remove(target);
                manager_.broadcast_bans();
                std::string details = reason.empty() ? "" : "reason: " + reason;
                if (expires_at) details += (details.empty() ? "" : "; ") + std::string("until ") + format_utc(expires_at);
                if (purge_seconds > 0) {
                    auto purged = db->delete_messages_by_sender_since(target, now_ts - purge_seconds);
                    for (const auto& path : purged.unlink_paths) {
                        std::error_code ec;
                        std::filesystem::remove(path, ec);
                        std::filesystem::remove(path + ".thumb.jpg", ec);
                        std::filesystem::remove(path + ".thumb-320px.jpg", ec);
                        std::filesystem::remove(path + ".thumb-640px.jpg", ec);
                        std::filesystem::remove(path + ".thumb-1280px.jpg", ec);
                    }
                    // One batched fan-out for the whole purge — a per-message
                    // broadcast_to_channel re-scanned every session and
                    // re-resolved permissions per message, stalling the io
                    // thread when purging a prolific spammer (A2).
                    manager_.broadcast_message_deletions(username_, now_ts, purged.messages);
                    details += (details.empty() ? "" : "; ") + std::to_string(purged.messages.size()) + " messages purged";
                }
                db->add_audit(username_, "ban", target, "", details);
            }
        }

        // --- LEAVE SERVER ---
        else if (packet.type() == chatproj::Packet::LEAVE_SERVER_REQ) {
            auto* db = manager_.db();
            if (!db) return;
            if (db->owner() == username_) {
                send_simple_mod_res(chatproj::Packet::MOD_ACTION_RES, false,
                                    "The server owner cannot leave their own server.",
                                    username_, "leave");
                return;
            }
            db->remove_member(username_);
            manager_.sync_membership_revoke(username_);
            manager_.emit_member_remove(username_);
            send_simple_mod_res(chatproj::Packet::MOD_ACTION_RES, true,
                                "You have left the server.",
                                username_, "leave");
            std::cout << "[Community] " << username_ << " left the server\n";
            // Give the write queue a tick to flush, then close.
            manager_.force_disconnect(username_, "leave", "", username_);
        }

        // --- UNBAN MEMBER ---
        else if (packet.type() == chatproj::Packet::UNBAN_MEMBER_REQ) {
            auto* db = manager_.db();
            if (!db) return;
            const std::string& target = packet.unban_member_req().username();
            if (auto a = manager_.authz().check(chatproj::Action::UnbanMember, {username_, "", ""}); !a) {
                send_simple_mod_res(chatproj::Packet::MOD_ACTION_RES, false, a.reason, target, "unban");
                return;
            }
            bool ok = db->remove_ban(target);
            send_simple_mod_res(chatproj::Packet::MOD_ACTION_RES, ok,
                                ok ? "Member unbanned." : "User is not banned.",
                                target, "unban");
            if (ok) {
                std::cout << "[Community] " << target << " unbanned by "
                          << username_ << "\n";
                // Refresh ban lists for every BAN_MEMBERS holder.
                manager_.broadcast_bans();
                db->add_audit(username_, "unban", target, "", "");
            }
        }

        // --- ROLE: LIST ---
        else if (packet.type() == chatproj::Packet::ROLE_LIST_REQ) {
            send_packet(build_role_list_packet(manager_.db()));
        }

        // --- ROLE: CREATE ---
        else if (packet.type() == chatproj::Packet::ROLE_CREATE_REQ) {
            auto* db = manager_.db();
            const auto& req = packet.role_create_req();
            chatproj::Packet p;
            p.set_type(chatproj::Packet::ROLE_ACTION_RES);
            auto* res = p.mutable_role_action_res();
            res->set_action("create");
            if (!db) {
                res->set_success(false);
                res->set_message("Server misconfigured.");
                send_packet(p);
                return;
            }
            if (auto a = manager_.authz().check(chatproj::Action::ManageRoles, {username_, "", ""}); !a) {
                res->set_success(false);
                res->set_message(a.reason);
                send_packet(p);
                return;
            }
            // Drop bits this server build doesn't define — undefined
            // bits must never reach the DB (see perms::kKnownMask).
            const uint64_t requested_perms =
                req.permissions() & chatproj::perms::kKnownMask;
            // Escalation guard: a role can only carry bits its creator
            // holds. (Owner holds everything, so this never blocks them.)
            const uint64_t actor_perms = db->effective_permissions(username_);
            if ((requested_perms & ~actor_perms) != 0) {
                res->set_success(false);
                res->set_message("You can't grant permissions you don't have.");
                send_packet(p);
                return;
            }
            std::string name = chatproj::clamp_utf8(req.name(), chatproj::kMaxRoleNameBytes);
            if (name.empty()) {
                res->set_success(false);
                res->set_message("Role name can't be empty.");
                send_packet(p);
                return;
            }
            auto created = db->create_role(name, req.color() & 0xFFFFFF,
                                           requested_perms);
            if (!created) {
                res->set_success(false);
                res->set_message("Failed to create role.");
                send_packet(p);
                return;
            }
            res->set_success(true);
            fill_role_info(res->mutable_role(), *created);
            send_packet(p);
            std::cout << "[Community] Role '" << created->name
                      << "' created by " << username_ << "\n";
            db->add_audit(username_, "role_create", created->name, "", "");
            manager_.broadcast_roles();
        }

        // --- ROLE: UPDATE ---
        else if (packet.type() == chatproj::Packet::ROLE_UPDATE_REQ) {
            auto* db = manager_.db();
            const auto& req = packet.role_update_req();
            chatproj::Packet p;
            p.set_type(chatproj::Packet::ROLE_ACTION_RES);
            auto* res = p.mutable_role_action_res();
            res->set_action("update");
            auto fail = [&](const char* msg) {
                res->set_success(false);
                res->set_message(msg);
                send_packet(p);
            };
            if (!db) { fail("Server misconfigured."); return; }
            if (auto a = manager_.authz().check(chatproj::Action::ManageRoles, {username_, "", ""}); !a) {
                fail(a.reason.c_str());
                return;
            }
            auto role = db->get_role(req.role_id());
            if (!role) { fail("Role not found."); return; }

            const int32_t actor_level = db->member_level(username_);
            if (!role->is_default) {
                // Hierarchy: only roles strictly below your highest are
                // editable, and they can't be moved to/above it either.
                if (role->position >= actor_level) {
                    fail("You can't manage a role at or above your highest role.");
                    return;
                }
                if (req.position() >= actor_level) {
                    fail("You can't move a role to or above your highest role.");
                    return;
                }
            }
            // Mask to defined bits (kKnownMask), then escalation-guard
            // the CHANGED bits only — bits the role already carries that
            // the actor doesn't hold may stay as-is, they just can't be
            // toggled.
            const uint64_t requested_perms =
                req.permissions() & chatproj::perms::kKnownMask;
            const uint64_t actor_perms = db->effective_permissions(username_);
            if (((requested_perms ^ role->permissions) & ~actor_perms) != 0) {
                fail("You can't change permissions you don't have.");
                return;
            }
            std::string name = chatproj::clamp_utf8(req.name(), chatproj::kMaxRoleNameBytes);
            if (!db->update_role(req.role_id(), name, req.color() & 0xFFFFFF,
                                 requested_perms, req.position())) {
                fail("Failed to update role.");
                return;
            }
            res->set_success(true);
            if (auto updated = db->get_role(req.role_id())) {
                fill_role_info(res->mutable_role(), *updated);
            }
            send_packet(p);
            db->add_audit(username_, "role_update", name, "",
                          requested_perms != role->permissions ? "permissions changed" : "");
            manager_.broadcast_roles();
            // Permission bits changed → every member's per-channel
            // my_permissions (and possibly visibility) may have changed.
            manager_.broadcast_channels();
        }

        // --- ROLE: DELETE ---
        else if (packet.type() == chatproj::Packet::ROLE_DELETE_REQ) {
            auto* db = manager_.db();
            const auto& req = packet.role_delete_req();
            chatproj::Packet p;
            p.set_type(chatproj::Packet::ROLE_ACTION_RES);
            auto* res = p.mutable_role_action_res();
            res->set_action("delete");
            auto fail = [&](const char* msg) {
                res->set_success(false);
                res->set_message(msg);
                send_packet(p);
            };
            if (!db) { fail("Server misconfigured."); return; }
            if (auto a = manager_.authz().check(chatproj::Action::ManageRoles, {username_, "", ""}); !a) {
                fail(a.reason.c_str());
                return;
            }
            auto role = db->get_role(req.role_id());
            if (!role) { fail("Role not found."); return; }
            if (role->is_default) { fail("The default role can't be deleted."); return; }
            if (role->position >= db->member_level(username_)) {
                fail("You can't delete a role at or above your highest role.");
                return;
            }
            // Holders lose the role (cascade) — collect them first so each
            // gets a MEMBER_UPSERT afterwards.
            std::vector<std::string> holders;
            for (const auto& [user, rid] : db->list_all_member_roles()) {
                if (rid == req.role_id()) holders.push_back(user);
            }
            if (!db->delete_role(req.role_id())) {
                fail("Failed to delete role.");
                return;
            }
            res->set_success(true);
            send_packet(p);
            std::cout << "[Community] Role '" << role->name
                      << "' deleted by " << username_ << "\n";
            db->add_audit(username_, "role_delete", role->name, "", "");
            manager_.broadcast_roles();
            for (const auto& u : holders) manager_.emit_member_upsert(u);
            manager_.broadcast_channels();
        }

        // --- MEMBER ROLES: UPDATE (replace a member's role set) ---
        else if (packet.type() == chatproj::Packet::MEMBER_ROLES_UPDATE_REQ) {
            auto* db = manager_.db();
            const auto& req = packet.member_roles_update_req();
            chatproj::Packet p;
            p.set_type(chatproj::Packet::ROLE_ACTION_RES);
            auto* res = p.mutable_role_action_res();
            res->set_action("assign");
            auto fail = [&](const char* msg) {
                res->set_success(false);
                res->set_message(msg);
                send_packet(p);
            };
            if (!db) { fail("Server misconfigured."); return; }
            if (auto a = manager_.authz().check(chatproj::Action::ManageRoles, {username_, "", ""}); !a) {
                fail(a.reason.c_str());
                return;
            }
            if (!db->is_member(req.username())) { fail("Not a member."); return; }

            // Hierarchy applies to the DELTA: every role being added or
            // removed must sit strictly below the actor's highest role.
            // Roles above the actor that the target already holds are
            // untouched by construction (they're in both sets or the
            // request is rejected).
            const int32_t actor_level = db->member_level(username_);
            std::set<int64_t> current;
            for (int64_t id : db->get_member_role_ids(req.username())) {
                current.insert(id);
            }
            std::set<int64_t> requested;
            for (int64_t id : req.role_ids()) requested.insert(id);

            std::vector<int64_t> added, removed;
            for (int64_t id : requested) {
                if (current.count(id) == 0) added.push_back(id);
            }
            for (int64_t id : current) {
                if (requested.count(id) == 0) removed.push_back(id);
            }
            // Escalation guard, mirroring ROLE_CREATE/UPDATE: a role you
            // ADD must not carry permission bits you don't hold yourself.
            // Hierarchy alone isn't enough — a lower-positioned role can
            // still carry ADMINISTRATOR, and assigning it (to yourself or
            // anyone) would hand out bits the actor was never granted.
            // Removing such a role is always fine (de-escalation).
            const uint64_t actor_perms = db->effective_permissions(username_);
            for (int64_t id : added) {
                auto role = db->get_role(id);
                if (!role || role->is_default) { fail("Unknown role in request."); return; }
                if (role->position >= actor_level) {
                    fail("You can't assign a role at or above your highest role.");
                    return;
                }
                if ((role->permissions & ~actor_perms) != 0) {
                    fail("You can't assign a role that grants permissions you don't have.");
                    return;
                }
            }
            for (int64_t id : removed) {
                auto role = db->get_role(id);
                if (!role || role->is_default) { fail("Unknown role in request."); return; }
                if (role->position >= actor_level) {
                    fail("You can't remove a role at or above your highest role.");
                    return;
                }
            }
            if (!db->set_member_roles(req.username(),
                                      std::vector<int64_t>(requested.begin(),
                                                           requested.end()))) {
                fail("Failed to update member roles.");
                return;
            }
            res->set_success(true);
            send_packet(p);
            {
                std::string details;
                for (int64_t id : added) if (auto r = db->get_role(id)) details += "+" + r->name + " ";
                for (int64_t id : removed) if (auto r = db->get_role(id)) details += "-" + r->name + " ";
                db->add_audit(username_, "member_roles", req.username(), "", details);
            }
            // The delta (with role_ids) is the authoritative confirmation
            // for every client, requester included.
            manager_.emit_member_upsert(req.username());
            // The target's channel visibility / my_permissions changed.
            manager_.send_channels_to_user(req.username());
        }

        // --- CHANNEL: CREATE ---
        else if (packet.type() == chatproj::Packet::CHANNEL_CREATE_REQ) {
            auto* db = manager_.db();
            const auto& req = packet.channel_create_req();
            chatproj::Packet p;
            p.set_type(chatproj::Packet::CHANNEL_ACTION_RES);
            auto* res = p.mutable_channel_action_res();
            res->set_action("create");
            auto fail = [&](const char* msg) {
                res->set_success(false);
                res->set_message(msg);
                send_packet(p);
            };
            if (!db) { fail("Server misconfigured."); return; }
            if (auto a = manager_.authz().check(chatproj::Action::CreateChannel, {username_, "", ""}); !a) {
                fail(a.reason.c_str());
                return;
            }
            std::string name = chatproj::clamp_utf8(req.name(), chatproj::kMaxChannelNameBytes);
            if (name.empty()) { fail("Channel name can't be empty."); return; }
            int32_t type = 0;
            if (req.type() == chatproj::ChannelInfo::VOICE) type = 1;
            else if (req.type() == chatproj::ChannelInfo::CATEGORY) type = 2;
            int32_t bitrate = req.voice_bitrate_kbps();
            if (bitrate < 0) bitrate = 0;
            if (bitrate > 512) bitrate = 512;
            // Placement target must be a real category when given
            // (create_channel re-checks under its lock; this gives the
            // clearer error message).
            if (type != 2 && !req.category_id().empty()) {
                auto cat = db->get_channel(req.category_id());
                if (!cat || cat->type != 2) {
                    fail("Unknown category.");
                    return;
                }
            }
            auto created = db->create_channel(name, type, bitrate,
                                              type == 2 ? "" : req.category_id());
            if (!created) { fail("Failed to create channel."); return; }
            res->set_success(true);
            fill_channel_info(res->mutable_channel(), *created);
            send_packet(p);
            std::cout << "[Community] Channel #" << created->id
                      << " created by " << username_ << "\n";
            db->add_audit(username_, "channel_create", created->name, created->id, "");
            manager_.broadcast_channels();
        }

        // --- CHANNEL: RENAME ---
        else if (packet.type() == chatproj::Packet::CHANNEL_RENAME_REQ) {
            auto* db = manager_.db();
            const auto& req = packet.channel_rename_req();
            chatproj::Packet p;
            p.set_type(chatproj::Packet::CHANNEL_ACTION_RES);
            auto* res = p.mutable_channel_action_res();
            res->set_action("rename");
            auto fail = [&](const char* msg) {
                res->set_success(false);
                res->set_message(msg);
                send_packet(p);
            };
            if (!db) { fail("Server misconfigured."); return; }
            if (auto a = manager_.authz().check(chatproj::Action::ManageChannel,
                                                {username_, req.channel_id(), ""}); !a) {
                fail(a.reason.c_str());
                return;
            }
            std::string name = chatproj::clamp_utf8(req.name(), chatproj::kMaxChannelNameBytes);
            if (name.empty()) { fail("Channel name can't be empty."); return; }
            if (!db->rename_channel(req.channel_id(), name)) {
                fail("Channel not found.");
                return;
            }
            res->set_success(true);
            if (auto ch = db->get_channel(req.channel_id())) {
                fill_channel_info(res->mutable_channel(), *ch);
            }
            send_packet(p);
            db->add_audit(username_, "channel_rename", name, req.channel_id(), "");
            manager_.broadcast_channels();
        }

        // --- CHANNEL: DELETE ---
        else if (packet.type() == chatproj::Packet::CHANNEL_DELETE_REQ) {
            auto* db = manager_.db();
            const auto& req = packet.channel_delete_req();
            chatproj::Packet p;
            p.set_type(chatproj::Packet::CHANNEL_ACTION_RES);
            auto* res = p.mutable_channel_action_res();
            res->set_action("delete");
            auto fail = [&](const char* msg) {
                res->set_success(false);
                res->set_message(msg);
                send_packet(p);
            };
            if (!db) { fail("Server misconfigured."); return; }
            if (auto a = manager_.authz().check(chatproj::Action::ManageChannel,
                                                {username_, req.channel_id(), ""}); !a) {
                fail(a.reason.c_str());
                return;
            }
            auto ch = db->get_channel(req.channel_id());
            if (!ch) { fail("Channel not found."); return; }
            // Clients assume at least one text channel exists (it's the
            // landing channel after auth) — never delete the last one.
            if (ch->type == 0 && db->count_channels_of_type(0) <= 1) {
                fail("Can't delete the last text channel.");
                return;
            }
            // A voice channel with people in it stays: deleting it under
            // their feet would strand live audio sessions server-side.
            if (ch->type == 1 && manager_.voice_channel_occupied(ch->id)) {
                fail("Channel is in use — ask everyone to leave first.");
                return;
            }
            auto wipe = db->delete_channel(req.channel_id());
            if (!wipe) { fail("Failed to delete channel."); return; }
            res->set_success(true);
            send_packet(p);

            // Blob cleanup — same sibling-variant pattern as CHANNEL_WIPE.
            for (const auto& path : wipe->unlink_paths) {
                std::error_code ec;
                std::filesystem::remove(path, ec);
                std::filesystem::remove(path + ".partial", ec);
                std::filesystem::remove(path + ".thumb.jpg", ec);
                std::filesystem::remove(path + ".thumb-320px.jpg", ec);
                std::filesystem::remove(path + ".thumb-640px.jpg", ec);
                std::filesystem::remove(path + ".thumb-1280px.jpg", ec);
            }

            std::cout << "[Community] Channel #" << req.channel_id()
                      << " deleted by " << username_ << " ("
                      << wipe->deleted_message_count << " messages, "
                      << wipe->deleted_attachment_count << " attachments)\n";
            db->add_audit(username_, "channel_delete", ch->name, req.channel_id(),
                          std::to_string(wipe->deleted_message_count) + " messages");
            manager_.broadcast_channels();
        }

        // --- SET NICKNAME ---
        else if (packet.type() == chatproj::Packet::SET_NICKNAME_REQ) {
            auto* db = manager_.db();
            if (!db) return;
            const auto& req = packet.set_nickname_req();
            const std::string& target = req.username();
            std::string nickname = chatproj::clamp_utf8(req.nickname(), chatproj::kMaxNicknameBytes);

            // Self always allowed; others need MANAGE_NICKNAMES + hierarchy.
            if (auto a = manager_.authz().check(chatproj::Action::ManageNicknameOf,
                                                {username_, "", target}); !a) {
                send_simple_mod_res(chatproj::Packet::MOD_ACTION_RES, false, a.reason,
                                    target, "nickname");
                return;
            }
            bool ok = db->set_nickname(target, nickname);
            send_simple_mod_res(chatproj::Packet::MOD_ACTION_RES, ok,
                                ok ? "" : "Not a member.",
                                target, "nickname");
            if (ok) {
                manager_.emit_member_upsert(target);
                if (target != username_) {
                    db->add_audit(username_, "nickname", target, "",
                                  nickname.empty() ? "cleared" : "set to " + nickname);
                }
            }
        }

        // --- CHANNEL: REORDER (drag-and-drop, full new order) ---
        else if (packet.type() == chatproj::Packet::CHANNEL_REORDER_REQ) {
            auto* db = manager_.db();
            const auto& req = packet.channel_reorder_req();
            chatproj::Packet p;
            p.set_type(chatproj::Packet::CHANNEL_ACTION_RES);
            auto* res = p.mutable_channel_action_res();
            res->set_action("reorder");
            auto fail = [&](const char* msg) {
                res->set_success(false);
                res->set_message(msg);
                send_packet(p);
            };
            if (!db) { fail("Server misconfigured."); return; }
            if (auto a = manager_.authz().check(chatproj::Action::ReorderChannels, {username_, "", ""}); !a) {
                fail(a.reason.c_str());
                return;
            }
            std::vector<std::string> ordered(req.channel_ids().begin(),
                                             req.channel_ids().end());
            if (!db->reorder_channels(ordered)) {
                // Set mismatch usually means a concurrent create/delete —
                // the follow-up broadcast below resyncs the client.
                fail("Reorder rejected — channel list changed, try again.");
                manager_.broadcast_channels();
                return;
            }
            res->set_success(true);
            send_packet(p);
            manager_.broadcast_channels();
        }

        // --- CHANNEL OVERWRITES: LIST ---
        else if (packet.type() == chatproj::Packet::CHANNEL_OVERWRITES_REQ) {
            auto* db = manager_.db();
            const std::string& channel_id = packet.channel_overwrites_req().channel_id();
            chatproj::Packet p;
            p.set_type(chatproj::Packet::CHANNEL_OVERWRITES_RES);
            auto* res = p.mutable_channel_overwrites_res();
            res->set_channel_id(channel_id);
            if (!db) { res->set_success(false); res->set_message("Server misconfigured."); send_packet(p); return; }
            if (auto a = manager_.authz().check(chatproj::Action::ViewOverwrites,
                                                {username_, channel_id, ""}); !a) {
                res->set_success(false);
                res->set_message(a.reason);
                send_packet(p);
                return;
            }
            send_packet(build_overwrites_packet(db, channel_id));
        }

        // --- CHANNEL OVERWRITES: SET / CLEAR ---
        else if (packet.type() == chatproj::Packet::CHANNEL_OVERWRITE_SET_REQ) {
            auto* db = manager_.db();
            const auto& ow = packet.channel_overwrite_set_req().overwrite();
            chatproj::Packet p;
            p.set_type(chatproj::Packet::CHANNEL_ACTION_RES);
            auto* res = p.mutable_channel_action_res();
            res->set_action("overwrite");
            auto fail = [&](const std::string& msg) {
                res->set_success(false);
                res->set_message(msg);
                send_packet(p);
            };
            if (!db) { fail("Server misconfigured."); return; }
            auto ch = db->get_channel(ow.channel_id());
            if (!ch) { fail("Channel not found."); return; }
            if (auto a = manager_.authz().check(chatproj::Action::ManageOverwrites,
                                                {username_, ow.channel_id(), ""}); !a) {
                fail(a.reason);
                return;
            }
            chatproj::DbOverwrite row;
            row.channel_id = ow.channel_id();
            row.target_type = ow.target_type() == chatproj::ChannelOverwrite::MEMBER ? 1 : 0;
            row.target_id = ow.target_id();
            row.deny = ow.deny() & chatproj::perms::kKnownMask;
            row.allow = (ow.allow() & chatproj::perms::kKnownMask) & ~row.deny;

            // Escalation guard (same spirit as roles): only bits the actor
            // holds IN THIS CHANNEL may change, in either direction. Bits a
            // row already carries that the actor lacks survive untouched.
            const uint64_t actor_perms = manager_.authz().channel_permissions(username_, row.channel_id);
            uint64_t cur_allow = 0, cur_deny = 0;
            for (const auto& existing : db->list_overwrites(row.channel_id)) {
                if (existing.target_type == row.target_type && existing.target_id == row.target_id) {
                    cur_allow = existing.allow; cur_deny = existing.deny;
                }
            }
            const uint64_t changed = (row.allow ^ cur_allow) | (row.deny ^ cur_deny);
            if ((changed & ~actor_perms) != 0) {
                fail("You can't change permissions you don't have in this channel.");
                return;
            }
            // Role targets: strictly below your level (everyone, position 0,
            // is always editable). Member targets: any member.
            if (row.target_type == 0) {
                int64_t rid = 0;
                try { rid = std::stoll(row.target_id); } catch (...) { fail("Unknown role."); return; }
                auto role = db->get_role(rid);
                if (!role) { fail("Unknown role."); return; }
                if (!role->is_default && role->position >= db->member_level(username_)) {
                    fail("You can't edit overwrites for a role at or above your highest role.");
                    return;
                }
            } else if (!db->is_member(row.target_id)) {
                fail("Not a member.");
                return;
            }
            if (!db->set_overwrite(row)) { fail("Failed to save overwrite."); return; }
            // Lock-out guard: if the actor just lost VIEW on this channel
            // (e.g. denied it for a role they hold), revert.
            if (!(manager_.authz().channel_permissions(username_, row.channel_id) & chatproj::perms::kViewChannel)) {
                chatproj::DbOverwrite revert = row;
                revert.allow = cur_allow; revert.deny = cur_deny;
                db->set_overwrite(revert);
                fail("That would remove your own access to this channel.");
                return;
            }
            res->set_success(true);
            fill_channel_info(res->mutable_channel(), *ch);
            res->mutable_channel()->set_my_permissions(
                manager_.authz().channel_permissions(username_, row.channel_id));
            send_packet(p);
            std::cout << "[Community] Overwrite on #" << row.channel_id << " for "
                      << (row.target_type == 1 ? "member " : "role ") << row.target_id
                      << " set by " << username_ << "\n";
            db->add_audit(username_, "overwrite_set",
                          std::string(row.target_type == 1 ? "member:" : "role:") + row.target_id,
                          row.channel_id,
                          (row.allow == 0 && row.deny == 0) ? "cleared"
                              : "allow " + std::to_string(row.allow) + " deny " + std::to_string(row.deny));
            // Visibility / my_permissions may have changed for anyone.
            manager_.broadcast_channels();
            manager_.broadcast_overwrites(row.channel_id);
        }

        // --- SERVER UPDATE (name / description) ---
        else if (packet.type() == chatproj::Packet::SERVER_UPDATE_REQ) {
            auto* db = manager_.db();
            const auto& req = packet.server_update_req();
            chatproj::Packet p;
            p.set_type(chatproj::Packet::SERVER_UPDATE_RES);
            auto* res = p.mutable_server_update_res();
            auto fail = [&](const std::string& msg) { res->set_success(false); res->set_message(msg); send_packet(p); };
            if (!db) { fail("Server misconfigured."); return; }
            if (auto a = manager_.authz().check(chatproj::Action::ManageServer, {username_, "", ""}); !a) {
                fail(a.reason); return;
            }
            const std::string name = chatproj::clamp_utf8(req.name(), 64);
            const std::string desc = chatproj::clamp_utf8(req.description(), 512);
            if (name.empty()) { fail("Server name can't be empty."); return; }
            db->set_server_meta(name, desc);
            std::string details = "name: " + name;
            // Optional public-listing toggle (absent on a name/description-only
            // edit, so it isn't reset). Takes effect in the central directory
            // within one heartbeat; the auth gate reads it live.
            if (req.has_public_listing()) {
                db->set_public_listing(req.public_listing());
                details += req.public_listing() ? "; public: on" : "; public: off";
            }
            db->add_audit(username_, "server_update", "", "", details);
            res->set_success(true);
            send_packet(p);
            manager_.broadcast_server_meta();
            std::cout << "[Community] Server updated ('" << name << "'"
                      << (req.has_public_listing()
                              ? (req.public_listing() ? ", public on" : ", public off")
                              : "")
                      << ") by " << username_ << "\n";
        }

        // --- AUDIT LOG ---
        else if (packet.type() == chatproj::Packet::AUDIT_LOG_REQ) {
            auto* db = manager_.db();
            const auto& req = packet.audit_log_req();
            chatproj::Packet p;
            p.set_type(chatproj::Packet::AUDIT_LOG_RES);
            auto* res = p.mutable_audit_log_res();
            if (!db) { res->set_success(false); res->set_message("Server misconfigured."); send_packet(p); return; }
            if (auto a = manager_.authz().check(chatproj::Action::ViewAuditLog, {username_, "", ""}); !a) {
                res->set_success(false); res->set_message(a.reason); send_packet(p); return;
            }
            bool has_more = false;
            for (const auto& e : db->list_audit(req.before_id(), req.limit(), &has_more)) {
                auto* out = res->add_entries();
                out->set_id(e.id);
                out->set_timestamp(e.timestamp);
                out->set_actor(e.actor);
                out->set_action(e.action);
                out->set_target(e.target);
                out->set_channel_id(e.channel_id);
                out->set_details(e.details);
            }
            res->set_success(true);
            res->set_has_more(has_more);
            send_packet(p);
        }

        // --- TIMEOUT ---
        else if (packet.type() == chatproj::Packet::TIMEOUT_MEMBER_REQ) {
            auto* db = manager_.db();
            if (!db) return;
            const auto& req = packet.timeout_member_req();
            const std::string& target = req.username();
            if (auto a = manager_.authz().check(chatproj::Action::TimeoutMember, {username_, "", target}); !a) {
                send_simple_mod_res(chatproj::Packet::MOD_ACTION_RES, false, a.reason, target, "timeout");
                return;
            }
            const int64_t now_ts = static_cast<int64_t>(std::time(nullptr));
            int64_t until = req.until();
            if (until != 0 && until <= now_ts) until = 0;                 // past = clear
            if (until > now_ts + 28 * 86400) until = now_ts + 28 * 86400; // Discord's cap
            if (!db->set_timeout(target, until)) {
                send_simple_mod_res(chatproj::Packet::MOD_ACTION_RES, false, "Not a member.", target, "timeout");
                return;
            }
            send_simple_mod_res(chatproj::Packet::MOD_ACTION_RES, true, "", target, "timeout");
            manager_.emit_member_upsert(target);
            // Bench them immediately: a member timed out mid-call must be
            // pulled out of voice (which also stops any stream they're
            // running) — not left talking until they leave on their own
            // (M1). Only on an actual timeout, not when clearing one.
            if (until > now_ts) manager_.disconnect_from_voice(target, username_);
            db->add_audit(username_, until ? "timeout" : "timeout_clear", target, "",
                          until ? "until " + format_utc(until) + (req.reason().empty() ? "" : "; reason: " + req.reason())
                                : "");
            std::cout << "[Community] " << target << (until ? " timed out by " : " timeout cleared by ")
                      << username_ << "\n";
        }

        // --- VOICE MODERATION ---
        else if (packet.type() == chatproj::Packet::VOICE_MOD_REQ) {
            auto* db = manager_.db();
            if (!db) return;
            const auto& req = packet.voice_mod_req();
            const std::string& target = req.username();
            if (auto a = manager_.authz().check(chatproj::Action::VoiceModerate, {username_, "", target}); !a) {
                send_simple_mod_res(chatproj::Packet::MOD_ACTION_RES, false, a.reason, target, "voice_mod");
                return;
            }
            auto member = db->get_member(target);
            if (!member) {
                send_simple_mod_res(chatproj::Packet::MOD_ACTION_RES, false, "Not a member.", target, "voice_mod");
                return;
            }
            std::string verb;
            switch (req.action()) {
                case chatproj::VoiceModRequest::SERVER_MUTE:
                case chatproj::VoiceModRequest::SERVER_UNMUTE:
                case chatproj::VoiceModRequest::SERVER_DEAFEN:
                case chatproj::VoiceModRequest::SERVER_UNDEAFEN: {
                    bool muted = member->server_muted, deafened = member->server_deafened;
                    if (req.action() == chatproj::VoiceModRequest::SERVER_MUTE) { muted = true; verb = "server_mute"; }
                    if (req.action() == chatproj::VoiceModRequest::SERVER_UNMUTE) { muted = false; verb = "server_unmute"; }
                    if (req.action() == chatproj::VoiceModRequest::SERVER_DEAFEN) { deafened = true; verb = "server_deafen"; }
                    if (req.action() == chatproj::VoiceModRequest::SERVER_UNDEAFEN) { deafened = false; verb = "server_undeafen"; }
                    db->set_server_voice_flags(target, muted, deafened);
                    manager_.apply_server_voice_flags(target, muted, deafened);
                    break;
                }
                case chatproj::VoiceModRequest::MOVE: {
                    auto ch = db->get_channel(req.channel_id());
                    if (!ch || ch->type != 1) {
                        send_simple_mod_res(chatproj::Packet::MOD_ACTION_RES, false, "Unknown voice channel.", target, "voice_mod");
                        return;
                    }
                    // The target still needs CONNECT in the destination.
                    if (!manager_.authz().check(chatproj::Action::ConnectVoice, {target, req.channel_id(), ""})) {
                        send_simple_mod_res(chatproj::Packet::MOD_ACTION_RES, false,
                                            "That member can't connect to the destination channel.", target, "voice_mod");
                        return;
                    }
                    if (manager_.move_to_voice_channel(target, req.channel_id(), username_) == 0) {
                        send_simple_mod_res(chatproj::Packet::MOD_ACTION_RES, false, "Member is not in voice.", target, "voice_mod");
                        return;
                    }
                    verb = "voice_move";
                    break;
                }
                case chatproj::VoiceModRequest::DISCONNECT: {
                    if (manager_.disconnect_from_voice(target, username_) == 0) {
                        send_simple_mod_res(chatproj::Packet::MOD_ACTION_RES, false, "Member is not in voice.", target, "voice_mod");
                        return;
                    }
                    verb = "voice_disconnect";
                    break;
                }
                default:
                    send_simple_mod_res(chatproj::Packet::MOD_ACTION_RES, false, "Unknown action.", target, "voice_mod");
                    return;
            }
            send_simple_mod_res(chatproj::Packet::MOD_ACTION_RES, true, "", target, "voice_mod");
            db->add_audit(username_, "voice_mod", target, req.channel_id(), verb);
        }

        // --- TRANSFER OWNERSHIP ---
        else if (packet.type() == chatproj::Packet::TRANSFER_OWNERSHIP_REQ) {
            auto* db = manager_.db();
            if (!db) return;
            const std::string& new_owner = packet.transfer_ownership_req().new_owner();
            if (auto a = manager_.authz().check(chatproj::Action::TransferOwnership, {username_, "", new_owner}); !a) {
                send_simple_mod_res(chatproj::Packet::MOD_ACTION_RES, false, a.reason, new_owner, "transfer");
                return;
            }
            if (new_owner == username_) {
                send_simple_mod_res(chatproj::Packet::MOD_ACTION_RES, false, "You already own this server.", new_owner, "transfer");
                return;
            }
            const std::string old_owner = username_;
            if (!db->transfer_ownership(new_owner)) {
                send_simple_mod_res(chatproj::Packet::MOD_ACTION_RES, false, "Not a member.", new_owner, "transfer");
                return;
            }
            send_simple_mod_res(chatproj::Packet::MOD_ACTION_RES, true, "", new_owner, "transfer");
            db->add_audit(old_owner, "ownership_transfer", new_owner, "", "");
            manager_.broadcast_server_meta();
            manager_.emit_member_upsert(old_owner);
            manager_.emit_member_upsert(new_owner);
            // Both users' effective permissions changed everywhere.
            manager_.broadcast_channels();
            manager_.broadcast_bans();
            std::cout << "[Community] Ownership transferred from " << old_owner << " to " << new_owner << "\n";
        }

        // --- STORAGE INFO (MANAGE_SERVER) ---
        else if (packet.type() == chatproj::Packet::STORAGE_INFO_REQ) {
            chatproj::Packet p;
            p.set_type(chatproj::Packet::STORAGE_INFO_RES);
            auto* res = p.mutable_storage_info_res();
            if (auto a = manager_.authz().check(chatproj::Action::ManageServer, {username_, "", ""}); !a) {
                res->set_success(false);
                res->set_message(a.reason);
                send_packet(p);
                return;
            }
            manager_.fill_storage_info(res);   // sets success + all figures
            send_packet(p);
        }

        // --- STORAGE CONFIG (MANAGE_SERVER): set the min-free headroom ---
        else if (packet.type() == chatproj::Packet::STORAGE_CONFIG_SET_REQ) {
            chatproj::Packet p;
            p.set_type(chatproj::Packet::STORAGE_INFO_RES);
            auto* res = p.mutable_storage_info_res();
            if (auto a = manager_.authz().check(chatproj::Action::ManageServer, {username_, "", ""}); !a) {
                res->set_success(false);
                res->set_message(a.reason);
                send_packet(p);
                return;
            }
            auto* db = manager_.db();
            if (!db) {
                res->set_success(false);
                res->set_message("Server misconfigured.");
                send_packet(p);
                return;
            }
            int64_t v = packet.storage_config_set_req().min_free_bytes();
            // Clamp to [0, volume capacity]: requiring more free space than the
            // disk holds is nonsensical. Fall back to a 1 TiB ceiling if the
            // volume can't be queried.
            const int64_t cap = manager_.volume_capacity();
            const int64_t ceiling = cap > 0 ? cap : (1024LL * 1024 * 1024 * 1024);
            if (v < 0) v = 0;
            if (v > ceiling) v = ceiling;
            const int64_t applied = db->set_min_free_bytes(v);
            db->add_audit(username_, "storage_min_free", "", "", std::to_string(applied));
            // Reply with a fresh snapshot so the tab reflects the new threshold.
            manager_.fill_storage_info(res);
            send_packet(p);
            std::cout << "[Community] " << username_ << " set min-free headroom to "
                      << applied << " bytes\n";
        }
    }

    // Helper used by moderation paths to send a short response (KICK/BAN/LEAVE
    // all share MOD_ACTION_RES; invite paths have their own response types but
    // follow the same shape).
    void send_simple_mod_res(chatproj::Packet::Type type, bool success,
                             const std::string& message,
                             const std::string& target_username,
                             const std::string& action) {
        chatproj::Packet p;
        p.set_type(type);
        if (type == chatproj::Packet::MOD_ACTION_RES) {
            auto* res = p.mutable_mod_action_res();
            res->set_success(success);
            res->set_message(message);
            res->set_username(target_username);
            res->set_action(action);
        } else if (type == chatproj::Packet::INVITE_CREATE_RES) {
            auto* res = p.mutable_invite_create_res();
            res->set_success(success);
            res->set_message(message);
        }
        send_packet(p);
    }

    // A CHANNEL_MSG this server won't accept. The sender gets a typed
    // CHANNEL_MSG_REJECTED naming the request's nonce — so the client
    // withdraws exactly that optimistic bubble instead of leaving a
    // ghost row anchored at its tail — followed by the legacy
    // MOD_ACTION_RES(action="message") for clients that predate it.
    void reject_channel_msg(const chatproj::ChannelMessage& msg, const std::string& reason) {
        chatproj::Packet p;
        p.set_type(chatproj::Packet::CHANNEL_MSG_REJECTED);
        auto* r = p.mutable_channel_msg_rejected();
        r->set_channel_id(msg.channel_id());
        r->set_nonce(msg.nonce());
        r->set_reason(reason);
        send_packet(p);
        send_simple_mod_res(chatproj::Packet::MOD_ACTION_RES, false, reason, username_, "message");
    }

    // Routes a packet type to its rate-limit bucket. Unlisted types
    // (pings, stop-stream, leave) are not limited.
    bool rate_limit_allows(chatproj::Packet::Type type) {
        using T = chatproj::Packet;
        switch (type) {
            case T::CHANNEL_MSG:
            case T::MESSAGE_EDIT_REQ:
                return msg_bucket_.try_take();
            case T::STREAM_THUMBNAIL_UPDATE:
                return thumb_bucket_.try_take();
            // LEAVE_VOICE_REQ / STOP_STREAM_REQ are deliberately unlimited:
            // dropping a leave would desync the client from the server's
            // voice state, whereas a dropped join just gets retried.
            case T::JOIN_VOICE_REQ:
            case T::VOICE_STATE_NOTIFY:
            case T::UPDATE_CAPABILITIES_REQ:
            case T::START_STREAM_REQ:
            case T::WATCH_STREAM_REQ:
            case T::STOP_WATCHING_REQ:
            case T::STREAM_CODEC_CHANGED_NOTIFY:
                return presence_bucket_.try_take();
            case T::CHANNEL_HISTORY_REQ:
            case T::MEMBER_LIST_REQ:
            case T::BAN_LIST_REQ:
            case T::AUDIT_LOG_REQ:
            case T::ROLE_LIST_REQ:
            case T::INVITE_LIST_REQ:
            case T::FETCH_STREAM_THUMBNAIL_REQ:
                return query_bucket_.try_take();
            case T::CHANNEL_UPDATE_REQ:
            case T::CHANNEL_WIPE_REQ:
            case T::MESSAGE_DELETE_REQ:
            case T::UPDATE_SERVER_PICTURE_REQ:
            case T::INVITE_CREATE_REQ:
            case T::INVITE_REVOKE_REQ:
            case T::KICK_MEMBER_REQ:
            case T::BAN_MEMBER_REQ:
            case T::UNBAN_MEMBER_REQ:
            case T::ROLE_CREATE_REQ:
            case T::ROLE_UPDATE_REQ:
            case T::ROLE_DELETE_REQ:
            case T::MEMBER_ROLES_UPDATE_REQ:
            case T::CHANNEL_CREATE_REQ:
            case T::CHANNEL_RENAME_REQ:
            case T::CHANNEL_DELETE_REQ:
            case T::CHANNEL_REORDER_REQ:
            case T::SET_NICKNAME_REQ:
            case T::CHANNEL_OVERWRITE_SET_REQ:
            case T::SERVER_UPDATE_REQ:
            case T::TIMEOUT_MEMBER_REQ:
            case T::VOICE_MOD_REQ:
            case T::TRANSFER_OWNERSHIP_REQ:
            case T::STORAGE_INFO_REQ:
            case T::STORAGE_CONFIG_SET_REQ:
                return admin_bucket_.try_take();
            default:
                return true;
        }
    }

    // ClientCapabilities is stored per session and re-serialised into
    // every VOICE_PRESENCE_UPDATE for the channel — an unbounded list was
    // an amplification vector (one fat caps blob × N members × every
    // mute toggle). Real clients advertise a handful of codecs.
    static bool capabilities_within_limits(const chatproj::ClientCapabilities& caps) {
        constexpr int kMaxCodecEntries = 16;
        return caps.encode_size() <= kMaxCodecEntries &&
               caps.decode_size() <= kMaxCodecEntries;
    }

    void send_auth_response(bool success, const std::string& msg,
                            const std::string& error_code) {
        chatproj::Packet p;
        p.set_type(chatproj::Packet::COMMUNITY_AUTH_RES);
        auto* res = p.mutable_community_auth_res();
        res->set_success(success);
        res->set_message(msg);
        res->set_error_code(error_code);

        if (success) {
            auto* db = manager_.db();
            if (db) {
                // Per-recipient: only channels this user can VIEW, each
                // with their resolved permissions.
                for (const auto& ch : manager_.authz().visible_channels(username_)) {
                    auto* info = res->add_channels();
                    fill_channel_info(info, ch);
                    info->set_my_permissions(manager_.authz().channel_permissions(username_, ch.id));
                }
                res->set_server_name(db->server_name());
                res->set_server_description(db->server_description());
                res->set_owner_username(db->owner());
                res->set_public_listing(db->public_listing());
            }
            res->set_max_attachment_bytes(manager_.max_attachment_bytes());
            res->set_attachment_port(manager_.attachment_port());
            // Central-assigned id (0 until the first heartbeat response).
            res->set_server_id(manager_.server_id());
        }

        send_packet(p);
    }

    void send_packet(const chatproj::Packet& packet) {
        std::string serialized;
        packet.SerializeToString(&serialized);
        // Pack length prefix
        uint32_t length = htonl(static_cast<uint32_t>(serialized.size()));
        auto framed = std::make_shared<std::vector<uint8_t>>();
        framed->resize(4 + serialized.size());
        std::memcpy(framed->data(), &length, 4);
        std::memcpy(framed->data() + 4, serialized.data(), serialized.size());
        deliver(framed);
    }

    ssl::stream<tcp::socket> socket_;
    boost::asio::steady_timer close_timer_;
    boost::asio::steady_timer deadline_timer_;
    SessionManager& manager_;
    char inbound_header_[4];

    // Rate-limit buckets (burst capacity, sustained per second).
    // Chat messages + edits. 10 burst / 3 per s: a human firing one-word
    // replies stays under it (the client paces to it too), a flood
    // doesn't. Mirrored in the client's sendPacing.ts — keep in step.
    chatproj::TokenBucket msg_bucket_{10, 3.0};
    chatproj::TokenBucket thumb_bucket_{6, 1.0};      // stream thumbnails
    chatproj::TokenBucket presence_bucket_{10, 2.0};  // voice/stream signalling
    chatproj::TokenBucket query_bucket_{20, 4.0};     // history / list fetches
    chatproj::TokenBucket admin_bucket_{20, 2.0};     // management + moderation
    chatproj::TokenBucket rate_limit_log_bucket_{3, 0.2};
    std::vector<uint8_t> inbound_body_;

    std::string jwt_public_pem_;
    bool authenticated_ = false;
    int64_t uid_ = 0;
    // Set by close_after_flush(): the session has been rejected, the
    // read loop must not re-arm, and the socket closes once the write
    // queue drains.
    bool closing_ = false;
    std::string username_;
    std::string token_;
    std::string udp_key_;
    std::string current_voice_channel_;
    boost::asio::ip::udp::endpoint udp_endpoint_;
    boost::asio::ip::udp::endpoint udp_media_endpoint_;
    bool is_muted_ = false;
    bool is_deafened_ = false;
    bool server_muted_ = false;
    bool server_deafened_ = false;
    std::deque<std::shared_ptr<std::vector<uint8_t>>> write_queue_;
    std::mutex write_mutex_;
    chatproj::ClientCapabilities capabilities_;
    mutable std::mutex capabilities_mutex_;
};

// Implementations of SessionManager methods

void SessionManager::join(std::shared_ptr<Session> session) {
    std::lock_guard<std::mutex> lock(mutex_);
    sessions_.insert(session);
    std::cout << "[Community] Session connected. Total: " << sessions_.size() << "\n";
}

void SessionManager::leave(std::shared_ptr<Session> session) {
    std::vector<std::string> affected_voice_channels;
    std::vector<std::string> affected_stream_channels;
    // Did THIS session own the user's live stream? Gates the streamer-side
    // watcher-set + thumbnail teardown so a sibling session's disconnect
    // can't strand a reconnected session's stream (I1).
    bool owns_live_stream = false;
    // Capture auth state before erasing so we know whether to push a roster
    // refresh to the rest of the server. Kicks/bans/self-leaves all funnel
    // through here via force_disconnect → socket close → read error → leave.
    const bool was_authenticated = session->is_authenticated();
    {
        std::lock_guard<std::mutex> lock(mutex_);
        // Idempotent: the read-error, write-error and backlog-overflow
        // paths can each call leave() for the same session. The second
        // call used to re-run the full roster broadcast and wipe the
        // thumbnail cache of a *second* session of the same user.
        if (sessions_.erase(session) == 0) return;
        if (!session->get_udp_key().empty()) {
            // Only erase the index entry if it still points at THIS
            // session. On reconnect the same JWT-derived udp_key is
            // re-registered onto the new session; erasing unconditionally
            // when the old session finally errors would drop the live
            // session's UDP routing (find_session_by_token → no match →
            // that user's voice/video silently dies).
            auto it = udp_key_index_.find(session->get_udp_key());
            if (it != udp_key_index_.end() && it->second == session) {
                udp_key_index_.erase(it);
            }
        }
        const std::string username = session->get_username();
        if (!username.empty()) {
            auto by_user = sessions_by_user_.find(username);
            if (by_user != sessions_by_user_.end()) {
                auto& vec = by_user->second;
                vec.erase(std::remove(vec.begin(), vec.end(), session), vec.end());
                if (vec.empty()) {
                    sessions_by_user_.erase(by_user);
                    last_keyframe_relay_.erase(username);
                }
            }
        }
        // Empty entries are pruned as we go — these maps are keyed by
        // whatever channel ids sessions brought in, and entries that
        // only ever get emptied (never erased) accumulate for the
        // lifetime of the process.
        for (auto it = voice_channels_.begin(); it != voice_channels_.end();) {
            if (it->second.erase(session) > 0) {
                affected_voice_channels.push_back(it->first);
            }
            it = it->second.empty() ? voice_channels_.erase(it) : std::next(it);
        }
        for (auto it = active_streams_.begin(); it != active_streams_.end();) {
            auto su = it->second.find(username);
            if (su != it->second.end()) {
                // Only tear the stream down if THIS session owns it. On a
                // reconnect the same username re-registers a stream under a
                // new session; the stale session's leave must leave the live
                // one alone. Expired owner = the owning session is already
                // gone, so cleanup is safe.
                auto owner = su->second.owner.lock();
                if (!owner || owner == session) {
                    it->second.erase(su);
                    owns_live_stream = true;
                    affected_stream_channels.push_back(it->first);
                }
            }
            it = it->second.empty() ? active_streams_.erase(it) : std::next(it);
        }
        // Clean up watcher state in both directions: entries where this
        // session watches someone (always — session-keyed), AND the watcher
        // set of this session's OWN stream, but only when this session
        // actually owned it (owns_live_stream) — otherwise a sibling
        // session's disconnect would strand a reconnected stream's viewers.
        for (auto ch_it = stream_watchers_.begin(); ch_it != stream_watchers_.end();) {
            auto& streamers = ch_it->second;
            bool watcher_removed = false;
            for (auto st_it = streamers.begin(); st_it != streamers.end();) {
                if (st_it->second.erase(session) > 0) watcher_removed = true;
                const bool own_stream_entry = (st_it->first == username && owns_live_stream);
                if (own_stream_entry || st_it->second.empty()) {
                    st_it = streamers.erase(st_it);
                } else {
                    ++st_it;
                }
            }
            // If this session was watching a stream here, the watcher count
            // changed — re-broadcast so remaining viewers see the drop. (May
            // duplicate a channel already flagged for the streamer path above;
            // broadcast_stream_presence is idempotent.)
            if (watcher_removed) affected_stream_channels.push_back(ch_it->first);
            ch_it = streamers.empty() ? stream_watchers_.erase(ch_it) : std::next(ch_it);
        }
        std::cout << "[Community] Session " << username << " left. Total: " << sessions_.size() << "\n";
    }
    // Clear the popup-preview cache only if this session owned the live
    // stream — wiping it for any sibling disconnect would blank a
    // reconnected session's thumbnail until its next ~1 Hz push (I1).
    if (owns_live_stream) erase_thumbnail_cache(session->get_username());
    // Broadcast updated presence to remaining clients (outside lock to avoid deadlock)
    for (const auto& ch : affected_voice_channels) {
        broadcast_voice_presence(ch);
    }
    for (const auto& ch : affected_stream_channels) {
        broadcast_stream_presence(ch);
    }
    if (was_authenticated) {
        // Presence flip only once the user's LAST session is gone; a
        // kicked/left user is no longer a member and emit_member_upsert
        // then skips (their MEMBER_REMOVE was already broadcast).
        bool still_online;
        {
            std::lock_guard<std::mutex> lock(mutex_);
            auto it = sessions_by_user_.find(session->get_username());
            still_online = it != sessions_by_user_.end() && !it->second.empty();
        }
        if (!still_online) emit_member_upsert(session->get_username());
    }
}

void SessionManager::start_stream(std::shared_ptr<Session> session, const std::string& channel_id,
                                  bool has_audio, uint32_t fps, uint32_t width, uint32_t height,
                                  chatproj::VideoCodec chosen_codec, chatproj::VideoCodec enforced_codec) {
    {
        std::lock_guard<std::mutex> lock(mutex_);
        // Enforce stream limit (0 = unlimited)
        if (max_streams_per_channel_ > 0 && active_streams_[channel_id].size() >= max_streams_per_channel_) {
            std::cout << "[Community] Stream limit reached in " << channel_id << ", rejecting " << session->get_username() << "\n";
            return;
        }
        StreamInfo info;
        info.has_audio = has_audio;
        info.fps = fps;
        info.width = width;
        info.height = height;
        info.current_codec = chosen_codec;
        info.enforced_codec = enforced_codec;
        info.owner = session;
        active_streams_[channel_id][session->get_username()] = info;
    }
    broadcast_stream_presence(channel_id);
}

void SessionManager::stop_stream(std::shared_ptr<Session> session, const std::string& channel_id) {
    bool removed = false;
    {
        std::lock_guard<std::mutex> lock(mutex_);
        auto it = active_streams_.find(channel_id);
        if (it != active_streams_.end()) {
            auto su = it->second.find(session->get_username());
            if (su != it->second.end()) {
                // Only the owning session (or an expired owner) tears the
                // stream down — a stale session must not stop a reconnected
                // session's live stream (I1).
                auto owner = su->second.owner.lock();
                if (!owner || owner == session) {
                    it->second.erase(su);
                    removed = true;
                }
            }
            if (it->second.empty()) active_streams_.erase(it);
        }
        // Clean up watchers + keyframe throttle only when we actually stopped
        // this session's stream, so a no-op stop from a stale session leaves
        // the live stream's watcher set intact.
        if (removed) {
            auto wch = stream_watchers_.find(channel_id);
            if (wch != stream_watchers_.end()) {
                wch->second.erase(session->get_username());
                if (wch->second.empty()) stream_watchers_.erase(wch);
            }
            last_keyframe_relay_.erase(session->get_username());
        }
    }
    if (removed) {
        // Drop any cached thumbnail for this streamer; popup viewers will now
        // get an empty response and stop polling once the next stream-presence
        // event removes the entry from their streamsByUser map.
        erase_thumbnail_cache(session->get_username());
        broadcast_stream_presence(channel_id);
    }
}

bool SessionManager::has_active_stream(const std::string& channel_id,
                                       const std::string& username) {
    std::lock_guard<std::mutex> lock(mutex_);
    auto it = active_streams_.find(channel_id);
    return it != active_streams_.end() && it->second.count(username) > 0;
}

void SessionManager::broadcast_stream_presence(const std::string& channel_id) {
    chatproj::Packet packet;
    packet.set_type(chatproj::Packet::STREAM_PRESENCE_UPDATE);
    auto* update = packet.mutable_stream_presence_update();
    update->set_channel_id(channel_id);

    std::string serialized;
    {
        std::lock_guard<std::mutex> lock(mutex_);
        if (active_streams_.find(channel_id) != active_streams_.end()) {
            for (const auto& pair : active_streams_[channel_id]) {
                auto* info = update->add_active_streams();
                info->set_stream_id(pair.first + "_screen");
                info->set_owner_username(pair.first);
                info->set_has_audio(pair.second.has_audio);
                // Plan A Group 7: ship live codec/resolution/fps so viewers
                // can drive the codec badge and watch-button gating.
                info->set_resolution_width(pair.second.width);
                info->set_resolution_height(pair.second.height);
                info->set_fps(pair.second.fps);
                info->set_current_codec(pair.second.current_codec);
                info->set_enforced_codec(pair.second.enforced_codec);
                // Live watcher count for this stream (0 if nobody is watching).
                // Both maps are guarded by mutex_, held here. find() (not [])
                // avoids inserting empty watcher entries.
                uint32_t watchers = 0;
                auto wch = stream_watchers_.find(channel_id);
                if (wch != stream_watchers_.end()) {
                    auto sit = wch->second.find(pair.first);
                    if (sit != wch->second.end())
                        watchers = static_cast<uint32_t>(sit->second.size());
                }
                info->set_watcher_count(watchers);
            }
        }
        packet.SerializeToString(&serialized);
    }

    uint32_t length = htonl(static_cast<uint32_t>(serialized.size()));
    auto framed = std::make_shared<std::vector<uint8_t>>();
    framed->resize(4 + serialized.size());
    std::memcpy(framed->data(), &length, 4);
    std::memcpy(framed->data() + 4, serialized.data(), serialized.size());

    std::vector<std::pair<std::shared_ptr<Session>, std::string>> targets;
    {
        std::lock_guard<std::mutex> lock(mutex_);
        for (auto& session : sessions_) {
            // Only authenticated sessions receive presence. A peer that merely
            // completed the TLS handshake (never sent COMMUNITY_AUTH_REQ) must
            // not passively harvest usernames / codec caps / mute state.
            if (session->is_authenticated()) targets.emplace_back(session, session->get_username());
        }
    }
    for (const auto& [session, user] : targets) {
        if (!authz_ || (authz_->channel_permissions(user, channel_id) & chatproj::perms::kViewChannel)) {
            session->deliver(framed);
        }
    }
}

void SessionManager::update_thumbnail_cache(const std::string& username,
                                             const std::string& bytes) {
    std::lock_guard<std::mutex> lock(mutex_);
    latest_thumbnails_[username].assign(bytes.begin(), bytes.end());
}

void SessionManager::erase_thumbnail_cache(const std::string& username) {
    std::lock_guard<std::mutex> lock(mutex_);
    latest_thumbnails_.erase(username);
}

bool SessionManager::get_thumbnail(const std::string& username,
                                    std::vector<uint8_t>& out) {
    std::lock_guard<std::mutex> lock(mutex_);
    auto it = latest_thumbnails_.find(username);
    if (it == latest_thumbnails_.end() || it->second.empty()) return false;
    out = it->second;
    return true;
}

void SessionManager::join_voice_channel(std::shared_ptr<Session> session, const std::string& new_channel, const std::string& old_channel) {
    bool old_watcher_removed = false;
    {
        std::lock_guard<std::mutex> lock(mutex_);
        if (!old_channel.empty()) {
            if (auto vc_it = voice_channels_.find(old_channel); vc_it != voice_channels_.end()) {
                vc_it->second.erase(session);
                if (vc_it->second.empty()) voice_channels_.erase(vc_it);
            }
            // Clean up watcher entries from old channel (prune empties)
            auto ch_it = stream_watchers_.find(old_channel);
            if (ch_it != stream_watchers_.end()) {
                auto& streamers = ch_it->second;
                for (auto st_it = streamers.begin(); st_it != streamers.end();) {
                    if (st_it->second.erase(session) > 0) old_watcher_removed = true;
                    st_it = st_it->second.empty() ? streamers.erase(st_it) : std::next(st_it);
                }
                if (streamers.empty()) stream_watchers_.erase(ch_it);
            }
        }
        voice_channels_[new_channel].insert(session);
    }
    if (!old_channel.empty()) {
        broadcast_voice_presence(old_channel);
        // If this session was watching in the old channel, its watcher counts
        // dropped — update the members still there.
        if (old_watcher_removed) broadcast_stream_presence(old_channel);
    }
    broadcast_voice_presence(new_channel);
    // Send current stream presence for the new channel to the joining user
    broadcast_stream_presence(new_channel);
}

void SessionManager::leave_voice_channel(std::shared_ptr<Session> session, const std::string& current_channel) {
    // Plan C: collect (channel, streamer) pairs we need to notify AFTER
    // releasing the lock — notify_streamer_of_watcher takes its own lock.
    std::vector<std::string> streamers_to_notify;
    {
        std::lock_guard<std::mutex> lock(mutex_);
        if (!current_channel.empty()) {
            if (auto vc_it = voice_channels_.find(current_channel); vc_it != voice_channels_.end()) {
                vc_it->second.erase(session);
                if (vc_it->second.empty()) voice_channels_.erase(vc_it);
            }
            // Clean up any watcher entries for this session in this channel
            auto ch_it = stream_watchers_.find(current_channel);
            if (ch_it != stream_watchers_.end()) {
                auto& streamers = ch_it->second;
                for (auto st_it = streamers.begin(); st_it != streamers.end();) {
                    if (st_it->second.erase(session) > 0) {
                        streamers_to_notify.push_back(st_it->first);
                    }
                    st_it = st_it->second.empty() ? streamers.erase(st_it) : std::next(st_it);
                }
                if (streamers.empty()) stream_watchers_.erase(ch_it);
            }
        }
    }
    if (!current_channel.empty()) {
        broadcast_voice_presence(current_channel);
        // Plan C: tell each streamer the watcher left (drives cooldown).
        for (const auto& streamer : streamers_to_notify) {
            notify_streamer_of_watcher(current_channel, streamer, session->get_username(),
                                       chatproj::StreamWatcherNotify::LEFT);
        }
        // If this session was watching anything here, the counts dropped —
        // update the members still in the channel.
        if (!streamers_to_notify.empty()) broadcast_stream_presence(current_channel);
    }
}

void SessionManager::broadcast_to_members(const chatproj::Packet& packet) {
    std::string serialized;
    packet.SerializeToString(&serialized);

    uint32_t length = htonl(static_cast<uint32_t>(serialized.size()));
    auto framed = std::make_shared<std::vector<uint8_t>>();
    framed->resize(4 + serialized.size());
    std::memcpy(framed->data(), &length, 4);
    std::memcpy(framed->data() + 4, serialized.data(), serialized.size());

    std::lock_guard<std::mutex> lock(mutex_);
    for (auto& session : sessions_) {
        if (session->is_authenticated()) {
            session->deliver(framed);
        }
    }
}

void SessionManager::set_io_context(boost::asio::io_context& io) {
    io_ = &io;
}

std::optional<chatproj::MemberInfo> SessionManager::build_member_info(const std::string& username) {
    if (!db_) return std::nullopt;
    auto m = db_->get_member(username);
    if (!m) return std::nullopt;
    chatproj::MemberInfo info;
    info.set_username(m->username);
    info.set_joined_at(m->joined_at);
    info.set_nickname(m->nickname);
    info.set_is_owner(m->username == db_->owner());
    info.set_timed_out_until(m->timed_out_until);
    {
        std::lock_guard<std::mutex> lock(mutex_);
        auto it = sessions_by_user_.find(username);
        info.set_is_online(it != sessions_by_user_.end() && !it->second.empty());
    }
    for (int64_t rid : db_->get_member_role_ids(username)) info.add_role_ids(rid);
    return info;
}

void SessionManager::emit_member_upsert(const std::string& username) {
    auto info = build_member_info(username);
    if (!info) return;   // not a member (any more) — a MEMBER_REMOVE covers it
    chatproj::Packet p;
    p.set_type(chatproj::Packet::MEMBER_UPSERT);
    auto* up = p.mutable_member_upsert();
    *up->mutable_member() = std::move(*info);
    up->set_revision(++roster_revision_);
    broadcast_to_members(p);
}

void SessionManager::emit_member_remove(const std::string& username) {
    chatproj::Packet p;
    p.set_type(chatproj::Packet::MEMBER_REMOVE);
    auto* rm = p.mutable_member_remove();
    rm->set_username(username);
    rm->set_revision(++roster_revision_);
    broadcast_to_members(p);
}

void SessionManager::fill_member_page(const std::string& after, int32_t limit,
                                      chatproj::MemberListResponse* res) {
    if (!db_) { res->set_success(false); res->set_message("Server misconfigured."); return; }
    if (limit <= 0) limit = 100;
    if (limit > 200) limit = 200;
    const std::string owner_name = db_->owner();
    auto online = get_online_usernames();
    // One roster scan + one member_roles scan; both are cheap at the
    // scale a single community server serves, and this only runs on
    // demand (page fetches), never on every roster event.
    std::unordered_map<std::string, std::vector<int64_t>> roles_by_user;
    for (const auto& [user, role_id] : db_->list_all_member_roles()) {
        roles_by_user[user].push_back(role_id);
    }
    auto members = db_->list_members();
    std::sort(members.begin(), members.end(),
              [](const chatproj::DbMember& a, const chatproj::DbMember& b) {
                  return a.username < b.username;
              });
    const bool first_page = after.empty();
    auto emit = [&](const chatproj::DbMember& m) {
        auto* info = res->add_members();
        info->set_username(m.username);
        info->set_joined_at(m.joined_at);
        info->set_nickname(m.nickname);
        info->set_is_owner(m.username == owner_name);
        info->set_is_online(online.count(m.username) > 0);
        info->set_timed_out_until(m.timed_out_until);
        if (auto it = roles_by_user.find(m.username); it != roles_by_user.end()) {
            for (int64_t rid : it->second) info->add_role_ids(rid);
        }
    };
    if (first_page) {
        for (const auto& m : members) {
            if (online.count(m.username)) emit(m);
        }
    }
    int32_t offline_sent = 0;
    bool has_more = false;
    std::string next_after;
    for (const auto& m : members) {
        if (online.count(m.username)) continue;
        if (!first_page && m.username <= after) continue;
        if (offline_sent >= limit) { has_more = true; break; }
        emit(m);
        ++offline_sent;
        next_after = m.username;
    }
    res->set_success(true);
    res->set_revision(roster_revision_);
    res->set_total_members(static_cast<int64_t>(members.size()));
    res->set_has_more(has_more);
    res->set_next_after(has_more ? next_after : "");
    res->set_first_page(first_page);
}

void SessionManager::broadcast_bans() {
    if (!db_ || !authz_) return;
    chatproj::Packet p;
    p.set_type(chatproj::Packet::BAN_LIST_RES);
    fill_ban_list(p.mutable_ban_list_res(), db_, roster_revision_);
    auto framed = frame_packet(p);
    std::vector<std::pair<std::shared_ptr<Session>, std::string>> targets;
    {
        std::lock_guard<std::mutex> lock(mutex_);
        for (const auto& s : sessions_) {
            if (s->is_authenticated()) targets.emplace_back(s, s->get_username());
        }
    }
    for (const auto& [session, user] : targets) {
        if (authz_->check(chatproj::Action::ViewBans, {user, "", ""})) session->deliver(framed);
    }
}

void SessionManager::broadcast_roles() {
    if (!db_) return;
    broadcast_to_members(build_role_list_packet(db_));
}

void SessionManager::broadcast_channels() {
    if (!db_ || !authz_) return;
    // Per recipient (permissions v2): each user gets only the channels they
    // can VIEW, stamped with their own my_permissions. Identical lists
    // across users share one serialisation via the per-username cache.
    std::vector<std::pair<std::shared_ptr<Session>, std::string>> targets;
    {
        std::lock_guard<std::mutex> lock(mutex_);
        for (const auto& s : sessions_) {
            if (s->is_authenticated()) targets.emplace_back(s, s->get_username());
        }
    }
    std::unordered_map<std::string, std::shared_ptr<std::vector<uint8_t>>> by_user;
    for (const auto& [session, user] : targets) {
        auto it = by_user.find(user);
        if (it == by_user.end()) {
            it = by_user.emplace(user, frame_packet(build_channel_list_packet(*authz_, user))).first;
        }
        session->deliver(it->second);
    }
}

void SessionManager::send_channels_to_user(const std::string& username) {
    if (!db_ || !authz_) return;
    auto sessions = find_sessions_by_username(username);
    if (sessions.empty()) return;
    auto framed = frame_packet(build_channel_list_packet(*authz_, username));
    for (const auto& s : sessions) s->deliver(framed);
}

void SessionManager::broadcast_server_meta() {
    if (!db_) return;
    chatproj::Packet p;
    p.set_type(chatproj::Packet::SERVER_META_UPDATE);
    auto* m = p.mutable_server_meta_update();
    m->set_server_name(db_->server_name());
    m->set_server_description(db_->server_description());
    m->set_owner_username(db_->owner());
    m->set_public_listing(db_->public_listing());
    broadcast_to_members(p);
}

size_t SessionManager::apply_server_voice_flags(const std::string& username, bool muted, bool deafened) {
    auto sessions = find_sessions_by_username(username);
    std::set<std::string> channels;
    for (const auto& s : sessions) {
        s->set_server_voice_flags(muted, deafened);
        if (!s->get_current_voice_channel().empty()) channels.insert(s->get_current_voice_channel());
    }
    for (const auto& ch : channels) broadcast_voice_presence(ch);
    return sessions.size();
}

size_t SessionManager::move_to_voice_channel(const std::string& username, const std::string& channel_id,
                                             const std::string& actor) {
    size_t moved = 0;
    for (const auto& s : find_sessions_by_username(username)) {
        const std::string old = s->get_current_voice_channel();
        if (old.empty() || old == channel_id) continue;
        stop_stream(s, old);
        join_voice_channel(s, channel_id, old);
        s->set_current_voice_channel(channel_id);
        chatproj::Packet p;
        p.set_type(chatproj::Packet::VOICE_FORCE_NOTIFY);
        auto* n = p.mutable_voice_force_notify();
        n->set_action(chatproj::VoiceForceNotify::MOVED);
        n->set_channel_id(channel_id);
        n->set_actor(actor);
        s->send_packet_external(p);
        ++moved;
    }
    return moved;
}

size_t SessionManager::disconnect_from_voice(const std::string& username, const std::string& actor) {
    size_t n = 0;
    for (const auto& s : find_sessions_by_username(username)) {
        const std::string old = s->get_current_voice_channel();
        if (old.empty()) continue;
        stop_stream(s, old);
        leave_voice_channel(s, old);
        s->set_current_voice_channel("");
        s->set_muted(false);
        s->set_deafened(false);
        chatproj::Packet p;
        p.set_type(chatproj::Packet::VOICE_FORCE_NOTIFY);
        auto* fn = p.mutable_voice_force_notify();
        fn->set_action(chatproj::VoiceForceNotify::DISCONNECTED);
        fn->set_actor(actor);
        s->send_packet_external(p);
        ++n;
    }
    return n;
}

void SessionManager::fill_storage_info(chatproj::StorageInfoResponse* res) {
    if (!db_) { res->set_success(false); res->set_message("Server misconfigured."); return; }
    // DB-derived footprint (cheap SUM/COUNT queries, no filesystem walk).
    auto u = db_->storage_usage(/*max_channels=*/16, /*max_largest=*/12);
    res->set_attachments_bytes(u.attachments_bytes);
    res->set_thumbnails_bytes(u.thumbnails_bytes);
    res->set_attachment_count(u.attachment_count);
    res->set_min_free_bytes(db_->min_free_bytes());
    for (const auto& k : u.by_kind) {
        auto* o = res->add_by_kind();
        o->set_kind(k.kind); o->set_bytes(k.bytes); o->set_count(k.count);
    }
    for (const auto& c : u.by_channel) {
        auto* o = res->add_by_channel();
        o->set_channel_id(c.channel_id); o->set_bytes(c.bytes); o->set_count(c.count);
    }
    for (const auto& l : u.largest) {
        auto* o = res->add_largest();
        o->set_attachment_id(l.id); o->set_filename(l.filename);
        o->set_size_bytes(l.size_bytes); o->set_channel_id(l.channel_id); o->set_kind(l.kind);
    }
    // Host volume holding the attachment store (cross-platform; ec overload
    // so a stat failure never throws on the io thread).
    if (!attachments_root_.empty()) {
        std::error_code ec;
        const auto info = std::filesystem::space(attachments_root_, ec);
        if (!ec) {
            res->set_volume_total_bytes(static_cast<int64_t>(info.capacity));
            res->set_volume_available_bytes(static_cast<int64_t>(info.available));
        }
    }
    // SQLite file + WAL + SHM.
    int64_t db_bytes = 0;
    if (!db_path_.empty()) {
        for (const char* suffix : {"", "-wal", "-shm"}) {
            std::error_code ec;
            const auto sz = std::filesystem::file_size(db_path_ + suffix, ec);
            if (!ec) db_bytes += static_cast<int64_t>(sz);
        }
    }
    res->set_database_bytes(db_bytes);
    res->set_success(true);
}

int64_t SessionManager::volume_capacity() const {
    if (attachments_root_.empty()) return 0;
    std::error_code ec;
    const auto info = std::filesystem::space(attachments_root_, ec);
    return ec ? 0 : static_cast<int64_t>(info.capacity);
}

int64_t SessionManager::slowmode_remaining(const std::string& username,
                                           const std::string& channel_id,
                                           int32_t slowmode_seconds) {
    if (slowmode_seconds <= 0) return 0;
    std::lock_guard<std::mutex> lock(slowmode_mutex_);
    auto uit = slowmode_last_.find(username);
    if (uit == slowmode_last_.end()) return 0;
    auto cit = uit->second.find(channel_id);
    if (cit == uit->second.end()) return 0;
    const auto elapsed = std::chrono::duration_cast<std::chrono::seconds>(
        std::chrono::steady_clock::now() - cit->second).count();
    return elapsed < slowmode_seconds ? (slowmode_seconds - elapsed) : 0;
}

void SessionManager::slowmode_record(const std::string& username,
                                     const std::string& channel_id) {
    std::lock_guard<std::mutex> lock(slowmode_mutex_);
    // Wholesale reset if it ever grows unreasonable — entries are tiny and
    // stale ones (older than any slowmode window) are harmless, so a rare
    // clear just grants one free message, never a correctness problem.
    if (slowmode_last_.size() > 8192) slowmode_last_.clear();
    slowmode_last_[username][channel_id] = std::chrono::steady_clock::now();
}

void SessionManager::broadcast_to_channel(const chatproj::Packet& packet, const std::string& channel_id) {
    if (!authz_) { broadcast_to_members(packet); return; }
    auto framed = frame_packet(packet);
    std::vector<std::pair<std::shared_ptr<Session>, std::string>> targets;
    {
        std::lock_guard<std::mutex> lock(mutex_);
        for (const auto& s : sessions_) {
            if (s->is_authenticated()) targets.emplace_back(s, s->get_username());
        }
    }
    for (const auto& [session, user] : targets) {
        if (authz_->channel_permissions(user, channel_id) & chatproj::perms::kViewChannel) {
            session->deliver(framed);
        }
    }
}

void SessionManager::broadcast_message_deletions(
    const std::string& deleted_by, int64_t deleted_at,
    const std::vector<std::pair<std::string, int64_t>>& messages) {
    if (messages.empty() || !authz_) return;
    // Frame each tombstone once, grouped by channel.
    std::unordered_map<std::string,
        std::vector<std::shared_ptr<std::vector<uint8_t>>>> by_channel;
    for (const auto& [ch, mid] : messages) {
        chatproj::Packet bcast;
        bcast.set_type(chatproj::Packet::CHANNEL_MESSAGE_DELETED);
        auto* bw = bcast.mutable_channel_message_deleted();
        bw->set_channel_id(ch);
        bw->set_message_id(mid);
        bw->set_deleted_at(deleted_at);
        bw->set_deleted_by(deleted_by);
        by_channel[ch].push_back(frame_packet(bcast));
    }
    std::vector<std::pair<std::shared_ptr<Session>, std::string>> targets;
    {
        std::lock_guard<std::mutex> lock(mutex_);
        for (const auto& s : sessions_) {
            if (s->is_authenticated()) targets.emplace_back(s, s->get_username());
        }
    }
    // One cached channel-permission lookup per (viewer, channel), then deliver
    // all of that channel's tombstones to the viewer.
    for (const auto& [session, user] : targets) {
        for (const auto& [ch, frames] : by_channel) {
            if (authz_->channel_permissions(user, ch) & chatproj::perms::kViewChannel) {
                for (const auto& f : frames) session->deliver(f);
            }
        }
    }
}

void SessionManager::broadcast_overwrites(const std::string& channel_id) {
    if (!db_ || !authz_) return;
    auto framed = frame_packet(build_overwrites_packet(db_, channel_id));
    std::vector<std::pair<std::shared_ptr<Session>, std::string>> targets;
    {
        std::lock_guard<std::mutex> lock(mutex_);
        for (const auto& s : sessions_) {
            if (s->is_authenticated()) targets.emplace_back(s, s->get_username());
        }
    }
    for (const auto& [session, user] : targets) {
        if (authz_->check(chatproj::Action::ViewOverwrites, {user, channel_id, ""})) {
            session->deliver(framed);
        }
    }
}

bool SessionManager::voice_channel_occupied(const std::string& channel_id) {
    std::lock_guard<std::mutex> lock(mutex_);
    auto it = voice_channels_.find(channel_id);
    return it != voice_channels_.end() && !it->second.empty();
}

void SessionManager::run_retention_sweep() {
    if (!db_) return;
    const int64_t now = static_cast<int64_t>(std::time(nullptr));

    struct ChannelPrune {
        std::string channel_id;
        std::vector<int64_t> deleted_message_ids;
        std::vector<chatproj::PurgedAttachmentInfo> purged_attachments;
    };
    std::vector<ChannelPrune> sweeps;
    std::vector<std::string> unlink_paths;

    for (const auto& ch : db_->list_channels()) {
        ChannelPrune cp;
        cp.channel_id = ch.id;

        // Attachments first — if text retention also fires, message-row
        // deletion CASCADEs the remaining attachment rows anyway.
        struct KindMap { int kind; int32_t days; } kinds[] = {
            { 0, ch.retention_days_image },    // chatproj::Attachment::IMAGE
            { 1, ch.retention_days_video },    // VIDEO
            { 2, ch.retention_days_document }, // DOCUMENT
            { 3, ch.retention_days_audio },    // AUDIO
        };
        for (const auto& k : kinds) {
            if (k.days <= 0) continue;
            const int64_t cutoff = now - (static_cast<int64_t>(k.days) * 86400);
            auto purged = db_->prune_attachments(ch.id, k.kind, cutoff);
            for (auto& p : purged) {
                if (!p.storage_path.empty()) {
                    unlink_paths.push_back(p.storage_path);
                }
                cp.purged_attachments.push_back(std::move(p));
            }
        }

        // Text retention: remove whole message rows past their cutoff. Any
        // still-present attachment blobs belonging to them get collected so
        // the server can unlink them from disk.
        if (ch.retention_days_text > 0) {
            const int64_t cutoff = now - (static_cast<int64_t>(ch.retention_days_text) * 86400);
            auto pruned = db_->prune_text_messages(ch.id, cutoff);
            cp.deleted_message_ids = std::move(pruned.deleted_ids);
            for (auto& p : pruned.unlink_paths) {
                unlink_paths.push_back(std::move(p));
            }
        }

        if (!cp.deleted_message_ids.empty() || !cp.purged_attachments.empty()) {
            sweeps.push_back(std::move(cp));
        }
    }

    // Abandoned uploads — unbound rows (message_id=0). Two cutoffs:
    // 'uploading' rows die after an hour (client crashed / gave up
    // mid-upload), but 'ready' rows get a day — a finished upload can
    // legitimately sit in a compose box for a while before the message
    // referencing it is sent, and sweeping it early silently strips the
    // attachment from that eventual message.
    {
        constexpr int64_t kUploadingTimeoutSeconds = 3600;      // 1 hour
        constexpr int64_t kReadyUnboundTimeoutSeconds = 86400;  // 24 hours
        auto stale = db_->list_stale_pending_attachments(
            now - kUploadingTimeoutSeconds, now - kReadyUnboundTimeoutSeconds);
        for (const auto& a : stale) {
            if (!a.storage_path.empty()) {
                std::error_code ec;
                std::filesystem::remove(a.storage_path + ".partial", ec);
                std::filesystem::remove(a.storage_path + ".thumb.jpg", ec);
                std::filesystem::remove(a.storage_path + ".thumb-320px.jpg", ec);
                std::filesystem::remove(a.storage_path + ".thumb-640px.jpg", ec);
                std::filesystem::remove(a.storage_path + ".thumb-1280px.jpg", ec);
                // The final path usually doesn't exist for pending rows, but
                // clean it too just in case a complete() landed with a DB
                // failure afterwards.
                std::filesystem::remove(a.storage_path, ec);
            }
            db_->delete_attachment_row(a.id);
        }
        if (!stale.empty()) {
            std::cout << "[Community] Retention sweep cleaned up "
                      << stale.size() << " abandoned pending upload(s)\n";
        }
    }

    // Unlink attachment blobs from disk. Errors are tolerated — missing
    // files just mean a prior sweep already cleaned them. Also unlink
    // every sibling thumbnail variant (legacy single-file + the three
    // pre-generated sizes) so posters don't outlive their parent.
    for (const auto& path : unlink_paths) {
        std::error_code ec;
        std::filesystem::remove(path, ec);
        std::filesystem::remove(path + ".thumb.jpg", ec);
        std::filesystem::remove(path + ".thumb-320px.jpg", ec);
        std::filesystem::remove(path + ".thumb-640px.jpg", ec);
        std::filesystem::remove(path + ".thumb-1280px.jpg", ec);
    }

    if (sweeps.empty()) return;

    // Fan CHANNEL_PRUNED out to every authenticated session so their
    // local state stays in sync without reload. Batched: the first sweep
    // after enabling retention on an old channel can prune hundreds of
    // thousands of rows, and one packet carrying every id would be
    // multi-MB (the server's own frame cap is 2 MiB). Clients treat each
    // CHANNEL_PRUNED independently, so splitting is transparent.
    constexpr size_t kPrunedBatch = 2000;
    for (const auto& cp : sweeps) {
        size_t mi = 0, ai = 0;
        do {
            chatproj::Packet p;
            p.set_type(chatproj::Packet::CHANNEL_PRUNED);
            auto* msg = p.mutable_channel_pruned();
            msg->set_channel_id(cp.channel_id);
            for (size_t n = 0; mi < cp.deleted_message_ids.size() && n < kPrunedBatch; ++mi, ++n) {
                msg->add_deleted_message_ids(cp.deleted_message_ids[mi]);
            }
            for (size_t n = 0; ai < cp.purged_attachments.size() && n < kPrunedBatch; ++ai, ++n) {
                auto* t = msg->add_purged_attachments();
                t->set_attachment_id(cp.purged_attachments[ai].attachment_id);
                t->set_purged_at(cp.purged_attachments[ai].purged_at);
            }
            broadcast_to_channel(p, cp.channel_id);
        } while (mi < cp.deleted_message_ids.size() || ai < cp.purged_attachments.size());
        std::cout << "[Community] Retention sweep on #" << cp.channel_id
                  << ": " << cp.deleted_message_ids.size() << " messages, "
                  << cp.purged_attachments.size() << " attachments\n";
    }
}

void SessionManager::broadcast_to_voice_channel_tcp(const chatproj::Packet& packet, const std::string& channel_id) {
    std::string serialized;
    packet.SerializeToString(&serialized);

    uint32_t length = htonl(static_cast<uint32_t>(serialized.size()));
    auto framed = std::make_shared<std::vector<uint8_t>>();
    framed->resize(4 + serialized.size());
    std::memcpy(framed->data(), &length, 4);
    std::memcpy(framed->data() + 4, serialized.data(), serialized.size());

    std::lock_guard<std::mutex> lock(mutex_);
    if (voice_channels_.find(channel_id) != voice_channels_.end()) {
        for (auto& session : voice_channels_[channel_id]) {
            session->deliver(framed);
        }
    }
}

void SessionManager::send_udp_to_targets(const char* data, size_t length,
                                         boost::asio::ip::udp::socket& udp_socket) {
    // Synchronous sends on a non-blocking socket: the kernel copies the
    // datagram immediately (or reports would_block, in which case we drop
    // — it's real-time media, a late packet is worse than a lost one). No
    // per-datagram heap buffer, no per-recipient completion handler.
    for (const auto& ep : udp_targets_) {
        boost::system::error_code ec;
        udp_socket.send_to(boost::asio::buffer(data, length), ep, 0, ec);
    }
}

void SessionManager::broadcast_to_voice_channel(const char* data, size_t length, const std::string& channel_id, std::shared_ptr<Session> sender, boost::asio::ip::udp::socket& udp_socket) {
    // Snapshot recipient endpoints under the lock into the reusable
    // scratch vector, release, then send. Holding the SessionManager mutex
    // across the fan-out serialized every other voice-channel operation
    // behind it — the dominant cause of voice glitches with >2 users.
    auto& targets = udp_targets_;
    targets.clear();
    {
        std::lock_guard<std::mutex> lock(mutex_);
        auto it = voice_channels_.find(channel_id);
        if (it == voice_channels_.end()) return;
        targets.reserve(it->second.size());
        for (auto& session : it->second) {
            // Skip the sender, endpoints we haven't learned yet, and
            // deafened listeners — a deafened user has muted all incoming
            // audio client-side, so relaying to them is pure waste. Their
            // endpoint is still refreshed from their own keepalive packets,
            // so audio resumes the instant they un-deafen.
            if (session != sender
                && session->get_udp_endpoint().port() != 0
                && !session->is_deafened()
                && !session->is_server_deafened()) {
                targets.push_back(session->get_udp_endpoint());
            }
        }
    }
    send_udp_to_targets(data, length, udp_socket);
}

void SessionManager::relay_keyframe_request(const std::string& target_username, boost::asio::ip::udp::socket& udp_socket) {
    // Build a minimal KEYFRAME_REQUEST packet to send to the streamer
    chatproj::UdpKeyframeRequest req;
    req.packet_type = chatproj::UdpPacketType::KEYFRAME_REQUEST;
    std::memset(req.sender_id, 0, chatproj::SENDER_ID_SIZE);
    std::memset(req.target_username, 0, chatproj::SENDER_ID_SIZE);

    std::lock_guard<std::mutex> lock(mutex_);
    // Resolve the target FIRST: the rate-limit map is keyed by whatever
    // name the datagram carried, and recording before the lookup let any
    // member grow it by one entry per packet of random targets, forever
    // (entries are pruned in leave() / stop_stream now, too).
    auto it = sessions_by_user_.find(target_username);
    if (it == sessions_by_user_.end()) return;
    // Rate-limit PLIs per streamer (250ms): a client must not be able to force
    // continuous IDRs by spamming WATCH / UDP keyframe requests, and many
    // watchers' near-simultaneous requests coalesce into a single IDR.
    {
        auto now = std::chrono::steady_clock::now();
        auto rl = last_keyframe_relay_.find(target_username);
        if (rl != last_keyframe_relay_.end() &&
            now - rl->second < std::chrono::milliseconds(250)) {
            return;
        }
        last_keyframe_relay_[target_username] = now;
    }
    for (auto& session : it->second) {
        if (session->get_udp_media_endpoint().port() != 0) {
            boost::system::error_code ec;
            udp_socket.send_to(boost::asio::buffer(&req, sizeof(req)),
                               session->get_udp_media_endpoint(), 0, ec);
            return;
        }
    }
}

void SessionManager::relay_nack(const char* data, size_t length, const std::string& target_username, boost::asio::ip::udp::socket& udp_socket) {
    std::lock_guard<std::mutex> lock(mutex_);
    auto it = sessions_by_user_.find(target_username);
    if (it == sessions_by_user_.end()) return;
    for (auto& session : it->second) {
        if (session->get_udp_media_endpoint().port() != 0) {
            boost::system::error_code ec;
            udp_socket.send_to(boost::asio::buffer(data, length),
                               session->get_udp_media_endpoint(), 0, ec);
            return;
        }
    }
}

bool SessionManager::add_watcher(std::shared_ptr<Session> watcher, const std::string& channel_id, const std::string& streamer_username) {
    std::lock_guard<std::mutex> lock(mutex_);
    return stream_watchers_[channel_id][streamer_username].insert(watcher).second;
}

bool SessionManager::watcher_blocked_by_enforcement(
    const std::string& channel_id,
    const std::string& streamer_username,
    std::shared_ptr<Session> watcher) {
    chatproj::VideoCodec enforced;
    {
        std::lock_guard<std::mutex> lock(mutex_);
        auto ch_it = active_streams_.find(channel_id);
        if (ch_it == active_streams_.end()) return false;
        auto st_it = ch_it->second.find(streamer_username);
        if (st_it == ch_it->second.end()) return false;
        enforced = st_it->second.enforced_codec;
    }
    if (enforced == chatproj::CODEC_UNKNOWN) return false;
    auto caps = watcher->get_capabilities();
    for (const auto& dec : caps.decode()) {
        if (dec.codec() == enforced) return false;
    }
    return true;
}

void SessionManager::notify_streamer_of_watcher(
    const std::string& channel_id,
    const std::string& streamer_username,
    const std::string& watcher_username,
    chatproj::StreamWatcherNotify::Action action) {
    auto streamer = find_session_by_username(streamer_username);
    if (!streamer) return; // streamer offline / not connected

    chatproj::Packet pkt;
    pkt.set_type(chatproj::Packet::STREAM_WATCHER_NOTIFY);
    pkt.set_timestamp(std::chrono::duration_cast<std::chrono::milliseconds>(
        std::chrono::system_clock::now().time_since_epoch()).count());
    auto* notify = pkt.mutable_stream_watcher_notify();
    notify->set_channel_id(channel_id);
    notify->set_streamer_username(streamer_username);
    notify->set_watcher_username(watcher_username);
    notify->set_action(action);

    std::string serialized;
    pkt.SerializeToString(&serialized);
    uint32_t length = htonl(static_cast<uint32_t>(serialized.size()));
    auto framed = std::make_shared<std::vector<uint8_t>>();
    framed->resize(4 + serialized.size());
    std::memcpy(framed->data(), &length, 4);
    std::memcpy(framed->data() + 4, serialized.data(), serialized.size());
    streamer->deliver(framed);
}

void SessionManager::handle_stream_codec_changed(
    const chatproj::Packet& packet,
    const std::string& sender_username) {
    if (!packet.has_stream_codec_changed_notify()) return;
    const auto& notify = packet.stream_codec_changed_notify();
    // Validate sender owns the stream — otherwise drop.
    if (notify.streamer_username() != sender_username) {
        std::cout << "[Community] STREAM_CODEC_CHANGED_NOTIFY from non-owner ignored ("
                  << sender_username << " vs " << notify.streamer_username() << ")\n";
        return;
    }
    {
        std::lock_guard<std::mutex> lock(mutex_);
        auto ch_it = active_streams_.find(notify.channel_id());
        if (ch_it == active_streams_.end()) return;
        auto st_it = ch_it->second.find(sender_username);
        if (st_it == ch_it->second.end()) return;
        st_it->second.current_codec = notify.new_codec();
        st_it->second.width = notify.new_width();
        st_it->second.height = notify.new_height();
        st_it->second.fps = notify.new_fps();
    }
    // Rebroadcast presence so all viewers' badges update.
    broadcast_stream_presence(notify.channel_id());
    // Forward the notify so viewers get the toast text + reason — minus
    // the sender's envelope (auth_token carries their JWT).
    chatproj::Packet forwarded = packet;
    strip_client_envelope(forwarded);
    broadcast_to_voice_channel_tcp(forwarded, notify.channel_id());
}

bool SessionManager::remove_watcher(std::shared_ptr<Session> watcher, const std::string& channel_id, const std::string& streamer_username) {
    std::lock_guard<std::mutex> lock(mutex_);
    bool removed = false;
    auto ch_it = stream_watchers_.find(channel_id);
    if (ch_it != stream_watchers_.end()) {
        auto st_it = ch_it->second.find(streamer_username);
        if (st_it != ch_it->second.end()) {
            removed = st_it->second.erase(watcher) > 0;
            if (st_it->second.empty()) ch_it->second.erase(st_it);
            if (ch_it->second.empty()) stream_watchers_.erase(ch_it);
        }
    }
    return removed;
}

void SessionManager::broadcast_to_watchers(const char* data, size_t length, const std::string& channel_id,
                                            const std::string& streamer_username, boost::asio::ip::udp::socket& udp_socket) {
    // Snapshot watcher endpoints under the lock, release, then send. Same
    // rationale as broadcast_to_voice_channel — keeps the manager mutex free
    // for joins/leaves while the video/audio fanout is in flight.
    auto& targets = udp_targets_;
    targets.clear();
    {
        std::lock_guard<std::mutex> lock(mutex_);
        auto ch_it = stream_watchers_.find(channel_id);
        if (ch_it == stream_watchers_.end()) return;
        auto st_it = ch_it->second.find(streamer_username);
        if (st_it == ch_it->second.end()) return;
        targets.reserve(st_it->second.size());
        for (auto& watcher : st_it->second) {
            if (watcher->get_udp_media_endpoint().port() != 0) {
                targets.push_back(watcher->get_udp_media_endpoint());
            }
        }
    }
    send_udp_to_targets(data, length, udp_socket);
}

void SessionManager::broadcast_to_watchers_voice(const char* data, size_t length, const std::string& channel_id,
                                                 const std::string& streamer_username, boost::asio::ip::udp::socket& udp_socket) {
    // Same as broadcast_to_watchers, but routes to each watcher's *voice*
    // UDP endpoint instead of their media endpoint. Used for STREAM_AUDIO
    // which travels on the voice socket end-to-end (small Opus packets,
    // sits next to the regular AUDIO traffic). Routing it to the media
    // endpoint instead would land it on the receiver's media socket recv
    // loop — which only knows VIDEO / FEC and silently drops everything
    // else, the bug that left watchers hearing nothing despite the
    // streamer's encode loop chugging along.
    auto& targets = udp_targets_;
    targets.clear();
    {
        std::lock_guard<std::mutex> lock(mutex_);
        auto ch_it = stream_watchers_.find(channel_id);
        if (ch_it == stream_watchers_.end()) return;
        auto st_it = ch_it->second.find(streamer_username);
        if (st_it == ch_it->second.end()) return;
        targets.reserve(st_it->second.size());
        for (auto& watcher : st_it->second) {
            if (watcher->get_udp_endpoint().port() != 0) {
                targets.push_back(watcher->get_udp_endpoint());
            }
        }
    }
    send_udp_to_targets(data, length, udp_socket);
}

void SessionManager::relay_keyframe_request_internal(const std::string& target_username) {
    if (!media_udp_socket_ptr_) return;
    relay_keyframe_request(target_username, *media_udp_socket_ptr_);
}

void SessionManager::broadcast_voice_presence(const std::string& channel_id) {
    chatproj::Packet packet;
    packet.set_type(chatproj::Packet::VOICE_PRESENCE_UPDATE);
    auto* update = packet.mutable_voice_presence_update();
    update->set_channel_id(channel_id);

    std::string serialized;
    {
        std::lock_guard<std::mutex> lock(mutex_);
        if (voice_channels_.find(channel_id) != voice_channels_.end()) {
            for (auto& session : voice_channels_[channel_id]) {
                update->add_active_users(session->get_username());
                auto* state = update->add_user_states();
                state->set_username(session->get_username());
                state->set_is_muted(session->is_muted());
                state->set_is_deafened(session->is_deafened());
                state->set_is_server_muted(session->is_server_muted());
                state->set_is_server_deafened(session->is_server_deafened());
                // Parallel array: user_capabilities[i] belongs to active_users[i].
                // Plan A Group 7: ship per-user caps so peers can drive the
                // LCD picker, watch-button gating, and codec badge locally.
                *update->add_user_capabilities() = session->get_capabilities();
            }
        }

        packet.SerializeToString(&serialized);
    } // release lock to send to everyone

    uint32_t length = htonl(static_cast<uint32_t>(serialized.size()));
    auto framed = std::make_shared<std::vector<uint8_t>>();
    framed->resize(4 + serialized.size());
    std::memcpy(framed->data(), &length, 4);
    std::memcpy(framed->data() + 4, serialized.data(), serialized.size());

    std::vector<std::pair<std::shared_ptr<Session>, std::string>> targets;
    {
        std::lock_guard<std::mutex> lock(mutex_);
        for (auto& session : sessions_) {
            // Authenticated sessions only — an unauthenticated TLS peer must
            // not receive the voice roster (usernames + mute/deafen state).
            if (session->is_authenticated()) targets.emplace_back(session, session->get_username());
        }
    }
    for (const auto& [session, user] : targets) {
        if (!authz_ || (authz_->channel_permissions(user, channel_id) & chatproj::perms::kViewChannel)) {
            session->deliver(framed);
        }
    }
}

void SessionManager::send_initial_voice_presences(std::shared_ptr<Session> session) {
    std::lock_guard<std::mutex> lock(mutex_);
    const std::string me = session->get_username();
    auto can_view = [&](const std::string& channel_id) {
        return !authz_ || (authz_->channel_permissions(me, channel_id) & chatproj::perms::kViewChannel);
    };
    for (const auto& pair : voice_channels_) {
        const std::string& channel_id = pair.first;
        if (pair.second.empty()) continue;
        if (!can_view(channel_id)) continue;

        chatproj::Packet packet;
        packet.set_type(chatproj::Packet::VOICE_PRESENCE_UPDATE);
        auto* update = packet.mutable_voice_presence_update();
        update->set_channel_id(channel_id);
        
        for (auto& s : pair.second) {
            update->add_active_users(s->get_username());
            auto* state = update->add_user_states();
            state->set_username(s->get_username());
            state->set_is_muted(s->is_muted());
            state->set_is_deafened(s->is_deafened());
            state->set_is_server_muted(s->is_server_muted());
            state->set_is_server_deafened(s->is_server_deafened());
            *update->add_user_capabilities() = s->get_capabilities();
        }

        std::string serialized;
        packet.SerializeToString(&serialized);

        uint32_t length = htonl(static_cast<uint32_t>(serialized.size()));
        auto framed = std::make_shared<std::vector<uint8_t>>();
        framed->resize(4 + serialized.size());
        std::memcpy(framed->data(), &length, 4);
        std::memcpy(framed->data() + 4, serialized.data(), serialized.size());

        session->deliver(framed);
    }

    // Send active streams as well
    for (const auto& pair : active_streams_) {
        const std::string& channel_id = pair.first;
        if (pair.second.empty()) continue;
        if (!can_view(channel_id)) continue;

        chatproj::Packet packet;
        packet.set_type(chatproj::Packet::STREAM_PRESENCE_UPDATE);
        auto* update = packet.mutable_stream_presence_update();
        update->set_channel_id(channel_id);

        for (const auto& stream : pair.second) {
            auto* info = update->add_active_streams();
            info->set_stream_id(stream.first + "_screen");
            info->set_owner_username(stream.first);
            info->set_has_audio(stream.second.has_audio);
            info->set_resolution_width(stream.second.width);
            info->set_resolution_height(stream.second.height);
            info->set_fps(stream.second.fps);
            info->set_current_codec(stream.second.current_codec);
            info->set_enforced_codec(stream.second.enforced_codec);
        }

        std::string serialized;
        packet.SerializeToString(&serialized);

        uint32_t length = htonl(static_cast<uint32_t>(serialized.size()));
        auto framed = std::make_shared<std::vector<uint8_t>>();
        framed->resize(4 + serialized.size());
        std::memcpy(framed->data(), &length, 4);
        std::memcpy(framed->data() + 4, serialized.data(), serialized.size());

        session->deliver(framed);
    }
}

void SessionManager::register_authenticated(std::shared_ptr<Session> session) {
    std::vector<std::shared_ptr<Session>> evict;
    {
        std::lock_guard<std::mutex> lock(mutex_);
        udp_key_index_[session->get_udp_key()] = session;
        auto& vec = sessions_by_user_[session->get_username()];
        if (std::find(vec.begin(), vec.end(), session) == vec.end()) {
            vec.push_back(session);
        }
        // Cap concurrent sessions per user (M3): an unbounded fan-out let one
        // JWT multiply every per-session rate limit and reset slowmode per
        // connection. Evict the oldest beyond the cap; the same-JWT udp_key
        // was just re-pointed at the new session, so the evicted session's
        // leave() won't touch the live routing (identity guard).
        while (vec.size() > kMaxSessionsPerUser) {
            evict.push_back(vec.front());
            vec.erase(vec.begin());
        }
    }
    // Close evicted sessions outside the lock (close_after_flush queues work).
    for (auto& s : evict) s->close_after_flush();
}

void SessionManager::unregister_udp_key(const std::string& udp_key) {
    std::lock_guard<std::mutex> lock(mutex_);
    udp_key_index_.erase(udp_key);
}

std::shared_ptr<Session> SessionManager::find_session_by_token(const std::string& udp_id, const std::string& /*jwt_secret*/) {
    std::lock_guard<std::mutex> lock(mutex_);
    auto it = udp_key_index_.find(udp_id);
    if (it != udp_key_index_.end()) {
        return it->second;
    }
    return nullptr;
}

size_t SessionManager::member_count() {
    if (db_) {
        return db_->list_members().size();
    }
    std::lock_guard<std::mutex> lock(mutex_);
    return sessions_.size();
}

std::set<std::string> SessionManager::get_online_usernames() {
    std::lock_guard<std::mutex> lock(mutex_);
    std::set<std::string> out;
    for (const auto& s : sessions_) {
        if (s->is_authenticated() && !s->get_username().empty()) {
            out.insert(s->get_username());
        }
    }
    return out;
}

std::shared_ptr<Session> SessionManager::find_session_by_username(const std::string& username) {
    std::lock_guard<std::mutex> lock(mutex_);
    auto it = sessions_by_user_.find(username);
    if (it == sessions_by_user_.end() || it->second.empty()) return nullptr;
    return it->second.front();
}

std::vector<std::shared_ptr<Session>> SessionManager::find_sessions_by_username(const std::string& username) {
    std::lock_guard<std::mutex> lock(mutex_);
    auto it = sessions_by_user_.find(username);
    if (it == sessions_by_user_.end()) return {};
    return it->second;
}

size_t SessionManager::force_disconnect(const std::string& username,
                                        const std::string& action,
                                        const std::string& reason,
                                        const std::string& actor,
                                        int64_t expires_at) {
    // Every live session of the user — a ban that closed only the first
    // match left a second device fully authenticated and posting.
    auto sessions = find_sessions_by_username(username);
    if (sessions.empty()) return 0;

    // NOTE: the central membership revoke is NOT synced here — this
    // returns 0 for offline targets, so the kick/ban/leave handlers
    // call sync_membership_revoke() themselves, unconditionally.

    chatproj::Packet p;
    p.set_type(chatproj::Packet::MEMBERSHIP_REVOKED);
    auto* rev = p.mutable_membership_revoked();
    rev->set_action(action);
    rev->set_reason(reason);
    rev->set_actor(actor);
    rev->set_expires_at(expires_at);

    for (const auto& session : sessions) {
        // Deliver the reason, then close once the write queue drains
        // (close_after_flush stops the read loop immediately, so the
        // session can't act on anything else in the meantime). The
        // previous close_connection() right after the send relied on
        // asio's speculative write to get MEMBERSHIP_REVOKED out.
        session->send_packet_external(p);
        session->close_after_flush();
    }
    return sessions.size();
}

namespace {
bool send_to_central_blocking(const std::string& host, int port,
                              const std::vector<uint8_t>& framed,
                              bool read_response,
                              chatproj::Packet* out_response,
                              const std::function<bool(const std::string&)>& pin_check);
} // namespace

void SessionManager::set_central_sync(const std::string& central_host, int central_port,
                                      const std::string& community_secret,
                                      const std::string& public_ip, int community_port,
                                      const std::string& central_cert_pin) {
    central_host_ = central_host;
    central_port_ = central_port;
    central_secret_ = community_secret;
    public_ip_ = public_ip;
    community_port_ = community_port;
    if (central_host_.empty() || central_port_ == 0) return;
    const std::string host = central_host_;
    const int port = central_port_;
    // Central certificate pinning. DECIBELL_CENTRAL_CERT_FINGERPRINT pins
    // explicitly; otherwise trust-on-first-use: the first fingerprint seen
    // is stored in server_meta and every later connection must match it.
    // A mismatch is logged loudly and the exchange is refused.
    chatproj::CommunityDb* db = db_;
    auto pin_check = [db, central_cert_pin](const std::string& seen) -> bool {
        if (!central_cert_pin.empty()) {
            if (seen == central_cert_pin) return true;
            std::cerr << "[CentralSync] REFUSED: central certificate " << seen
                      << " does not match DECIBELL_CENTRAL_CERT_FINGERPRINT\n";
            return false;
        }
        if (!db) return true;
        const std::string pinned = db->central_cert_fingerprint();
        if (pinned.empty()) {
            db->set_central_cert_fingerprint(seen);
            std::cout << "[CentralSync] Pinned central certificate (TOFU): " << seen << "\n";
            return true;
        }
        if (pinned == seen) return true;
        std::cerr << "[CentralSync] REFUSED: central certificate changed (pinned " << pinned
                  << ", seen " << seen << "). If central was legitimately re-keyed, "
                     "set DECIBELL_CENTRAL_CERT_FINGERPRINT or clear the pin "
                     "(server_meta key central_cert_fingerprint).\n";
        return false;
    };
    central_worker_ = std::make_unique<chatproj::CentralSyncWorker>(
        [host, port, pin_check](const std::vector<uint8_t>& framed, bool read_response,
                                std::vector<uint8_t>* response) {
            chatproj::Packet resp;
            const bool ok = send_to_central_blocking(host, port, framed, read_response,
                                                     read_response ? &resp : nullptr, pin_check);
            if (ok && read_response && response) {
                std::string serialized;
                resp.SerializeToString(&serialized);
                response->assign(serialized.begin(), serialized.end());
            }
            return ok;
        });
    central_worker_->start();
}

void SessionManager::enqueue_central(std::vector<uint8_t> framed, bool read_response,
                                     chatproj::CentralSyncWorker::Done done) {
    if (!central_worker_) return;
    if (!central_worker_->enqueue(std::move(framed), read_response, std::move(done))) {
        std::cerr << "[CentralSync] queue full, dropping packet ("
                  << central_worker_->pending() << " pending)\n";
    }
}

namespace {
// One-shot TLS send of a framed packet to central. Blocks the calling
// thread for at most kCentralSyncTimeout; call from a detached thread so
// packet handlers never stall.
//
// The whole exchange (resolve → connect → handshake → write → optional
// framed response) runs as an async chain on a private io_context driven
// by run_for(), so the timeout bounds every stage — including connect,
// which a blocking implementation could hang on for minutes against a
// SYN-blackholed host. (The previous version scheduled a deadline timer
// on an io_context nobody ran, so its 2-second timeout could never fire
// and a stalled central hung the calling thread forever.)
//
// If `read_response` is true and `out_response` is non-null, reads one
// framed response packet from central before closing. Returns true on
// success (or when no response was requested).
constexpr auto kCentralSyncTimeout = std::chrono::seconds(5);

bool send_to_central_blocking(const std::string& host, int port,
                              const std::vector<uint8_t>& framed,
                              bool read_response,
                              chatproj::Packet* out_response,
                              const std::function<bool(const std::string&)>& pin_check) {
    try {
        boost::asio::io_context io;
        ssl::context ctx(ssl::context::tlsv12_client);
        // Pin the leaf certificate by fingerprint (self-signed certs never
        // chain to a CA, so chain verification is irrelevant — the
        // callback's verdict on depth 0 is the whole decision).
        ctx.set_verify_mode(ssl::verify_peer);

        tcp::resolver resolver(io);
        ssl::stream<tcp::socket> ssl_socket(io, ctx);
        ssl_socket.set_verify_callback([&pin_check](bool /*preverified*/, ssl::verify_context& vctx) {
            X509_STORE_CTX* store = vctx.native_handle();
            if (X509_STORE_CTX_get_error_depth(store) != 0) return true;
            X509* cert = X509_STORE_CTX_get_current_cert(store);
            return pin_check(chatproj::cert_fingerprint_from_x509(cert));
        });

        bool completed = false;
        uint32_t len_be = 0;
        std::vector<uint8_t> body;

        // Nested async chain. Everything captured by reference outlives
        // io.run_for() below; handlers that never run are destroyed with
        // the io_context without touching the captures.
        resolver.async_resolve(host, std::to_string(port),
            [&](const boost::system::error_code& ec,
                tcp::resolver::results_type endpoints) {
                if (ec) return;
                boost::asio::async_connect(ssl_socket.lowest_layer(), endpoints,
                    [&](const boost::system::error_code& ec, const tcp::endpoint&) {
                        if (ec) return;
                        ssl_socket.async_handshake(ssl::stream_base::client,
                            [&](const boost::system::error_code& ec) {
                                if (ec) return;
                                boost::asio::async_write(ssl_socket, boost::asio::buffer(framed),
                                    [&](const boost::system::error_code& ec, std::size_t) {
                                        if (ec) return;
                                        if (!read_response || !out_response) {
                                            completed = true;
                                            return;
                                        }
                                        boost::asio::async_read(ssl_socket,
                                            boost::asio::buffer(&len_be, 4),
                                            [&](const boost::system::error_code& ec, std::size_t) {
                                                if (ec) return;
                                                const uint32_t len = ntohl(len_be);
                                                if (len == 0 || len > (1u << 20)) return;
                                                body.resize(len);
                                                boost::asio::async_read(ssl_socket,
                                                    boost::asio::buffer(body),
                                                    [&](const boost::system::error_code& ec, std::size_t) {
                                                        if (!ec) completed = true;
                                                    });
                                            });
                                    });
                            });
                    });
            });

        io.run_for(kCentralSyncTimeout);

        boost::system::error_code ignore;
        ssl_socket.lowest_layer().close(ignore);

        if (!completed) {
            std::cerr << "[CentralSync] Timed out or failed talking to "
                      << host << ":" << port << "\n";
            return false;
        }
        if (read_response && out_response) {
            return out_response->ParseFromArray(body.data(),
                                                static_cast<int>(body.size()));
        }
        return true;
    } catch (const std::exception& e) {
        std::cerr << "[CentralSync] Failed: " << e.what() << "\n";
        return false;
    }
}
} // namespace

void SessionManager::sync_membership_revoke(const std::string& username) {
    if (central_host_.empty() || central_port_ == 0) return;
    int64_t sid = server_id();
    if (sid <= 0) return;
    chatproj::Packet packet;
    packet.set_type(chatproj::Packet::MEMBERSHIP_REVOKE_REQ);
    packet.set_auth_token(central_secret_);
    auto* req = packet.mutable_membership_revoke_req();
    req->set_username(username);
    req->set_server_id(sid);

    std::string serialized;
    packet.SerializeToString(&serialized);
    auto framed = chatproj::create_framed_packet(serialized);

    enqueue_central(std::move(framed));
}

void SessionManager::sync_server_picture(const std::string& data,
                                            const std::string& version) {
    if (central_host_.empty() || central_port_ == 0) return;
    chatproj::Packet packet;
    packet.set_type(chatproj::Packet::SYNC_SERVER_PICTURE_REQ);
    packet.set_auth_token(central_secret_);
    auto* req = packet.mutable_sync_server_picture_req();
    req->set_host(public_ip_);
    req->set_port(community_port_);
    req->set_data(data);
    req->set_version(version);
    req->set_server_id(server_id());

    std::string serialized;
    packet.SerializeToString(&serialized);
    auto framed = chatproj::create_framed_packet(serialized);

    enqueue_central(std::move(framed));
}

void SessionManager::sync_membership_register(const std::string& username) {
    if (central_host_.empty() || central_port_ == 0) return;
    int64_t sid = server_id();
    if (sid <= 0) {
        // server_id not yet learned — skip. Next successful auth after
        // the heartbeat response lands will pick this user up.
        return;
    }
    chatproj::Packet packet;
    packet.set_type(chatproj::Packet::MEMBERSHIP_REGISTER_REQ);
    packet.set_auth_token(central_secret_);
    auto* req = packet.mutable_membership_register_req();
    req->set_username(username);
    req->set_server_id(sid);

    std::string serialized;
    packet.SerializeToString(&serialized);
    auto framed = chatproj::create_framed_packet(serialized);

    enqueue_central(std::move(framed));
}

void SessionManager::sync_invite_register(const std::string& code, int64_t expires_at) {
    if (central_host_.empty() || central_port_ == 0) return;

    chatproj::Packet packet;
    packet.set_type(chatproj::Packet::INVITE_REGISTER_REQ);
    packet.set_auth_token(central_secret_);
    auto* req = packet.mutable_invite_register_req();
    req->set_code(code);
    req->set_host(public_ip_);
    req->set_port(static_cast<uint32_t>(community_port_));
    req->set_expires_at(expires_at);

    std::string serialized;
    packet.SerializeToString(&serialized);
    auto framed = chatproj::create_framed_packet(serialized);

    enqueue_central(std::move(framed));
}

void SessionManager::sync_invite_unregister(const std::string& code) {
    if (central_host_.empty() || central_port_ == 0) return;

    chatproj::Packet packet;
    packet.set_type(chatproj::Packet::INVITE_UNREGISTER_REQ);
    packet.set_auth_token(central_secret_);
    auto* req = packet.mutable_invite_unregister_req();
    req->set_code(code);

    std::string serialized;
    packet.SerializeToString(&serialized);
    auto framed = chatproj::create_framed_packet(serialized);

    enqueue_central(std::move(framed));
}

class CommunityServer {
public:
    CommunityServer(boost::asio::io_context& io_context, short port, SessionManager& manager, const std::string& jwt_public_pem)
        : acceptor_(io_context, tcp::endpoint(tcp::v4(), port)),
          accept_backoff_(io_context),
          udp_socket_(io_context, boost::asio::ip::udp::endpoint(boost::asio::ip::udp::v4(), port + 1)),
          media_udp_socket_(io_context, boost::asio::ip::udp::endpoint(boost::asio::ip::udp::v4(), port + 2)),
          ssl_context_(ssl::context::tlsv12),
          manager_(manager),
          jwt_secret_(jwt_public_pem) {

        ssl_context_.set_options(
            ssl::context::default_workarounds |
            ssl::context::no_sslv2 |
            ssl::context::no_sslv3 |
            ssl::context::no_tlsv1 |
            ssl::context::no_tlsv1_1);

        ssl_context_.use_certificate_chain_file("server.crt");
        ssl_context_.use_private_key_file("server.key", ssl::context::pem);

        // User-level non-blocking: the relay uses synchronous send_to /
        // receive_from (drop on would_block) instead of one heap-allocated
        // async operation per datagram per recipient.
        udp_socket_.non_blocking(true);
        media_udp_socket_.non_blocking(true);

        // Voice UDP socket buffers
        udp_socket_.set_option(boost::asio::socket_base::receive_buffer_size(2 * 1024 * 1024));
        udp_socket_.set_option(boost::asio::socket_base::send_buffer_size(2 * 1024 * 1024));

        // Media UDP socket buffers
        media_udp_socket_.set_option(boost::asio::socket_base::receive_buffer_size(2 * 1024 * 1024));
        media_udp_socket_.set_option(boost::asio::socket_base::send_buffer_size(2 * 1024 * 1024));

        manager_.set_udp_socket(&udp_socket_);
        manager_.set_media_udp_socket(&media_udp_socket_);

        std::cout << "Community Server TCP running on port " << port << "...\n";
        std::cout << "Community Server Voice UDP running on port " << port + 1 << "...\n";
        std::cout << "Community Server Media UDP running on port " << port + 2 << "...\n";

        do_accept();
        do_receive_voice_udp();
        do_receive_media_udp();
    }
private:
    void do_accept() {
        acceptor_.async_accept(
            [this](boost::system::error_code ec, tcp::socket socket) {
                if (!ec) {
                    // A throw while standing up the session must not skip the
                    // re-arm below, or the accept chain dies silently (R1).
                    try {
                        auto session = std::make_shared<Session>(std::move(socket), manager_, ssl_context_, jwt_secret_);
                        manager_.join(session);
                        session->start();
                    } catch (const std::exception& e) {
                        std::cerr << "[Community] accept handler threw: " << e.what() << " — continuing\n";
                    } catch (...) {
                        std::cerr << "[Community] accept handler threw (unknown) — continuing\n";
                    }
                    do_accept();
                    return;
                }
                if (ec == boost::asio::error::operation_aborted) return;
                // Back off before re-arming: under fd exhaustion accept()
                // fails instantly (EMFILE) and an unconditional re-arm
                // spun the io thread at 100 %, starving the UDP relay.
                std::cerr << "[Community] accept failed: " << ec.message()
                          << " — retrying in 500 ms\n";
                accept_backoff_.expires_after(std::chrono::milliseconds(500));
                accept_backoff_.async_wait([this](const boost::system::error_code& tec) {
                    if (!tec) do_accept();
                });
            });
    }

    // ── Voice UDP receive chain (AUDIO, STREAM_AUDIO, PING) ──────────────────
    void do_receive_voice_udp() {
        udp_socket_.async_receive_from(
            boost::asio::buffer(udp_buffer_, sizeof(udp_buffer_)), udp_sender_endpoint_,
            [this](boost::system::error_code ec, std::size_t bytes_recvd) {
                // A throw in datagram handling (e.g. bad_alloc in a fan-out
                // serialize) must not skip the tail re-arm — that used to
                // kill the voice receive chain silently for the whole
                // process while it "kept serving" (R1).
                try {
                    if (!ec) handle_voice_datagram(bytes_recvd);
                    // Drain everything else already queued in the kernel
                    // before yielding to the reactor: one datagram per epoll
                    // wakeup was the dominant per-packet cost under load.
                    // Bounded so TCP handlers still get the thread.
                    for (int i = 0; i < kMaxDrainPerWakeup; ++i) {
                        boost::system::error_code dec;
                        const std::size_t n = udp_socket_.receive_from(
                            boost::asio::buffer(udp_buffer_, sizeof(udp_buffer_)), udp_sender_endpoint_, 0, dec);
                        if (dec) break;   // would_block = drained
                        handle_voice_datagram(n);
                    }
                } catch (const std::exception& e) {
                    std::cerr << "[Community] voice datagram handler threw: " << e.what() << " — continuing\n";
                } catch (...) {
                    std::cerr << "[Community] voice datagram handler threw (unknown) — continuing\n";
                }
                do_receive_voice_udp();
            });
    }

    void handle_voice_datagram(std::size_t bytes_recvd) {
                if (bytes_recvd >= 1) {
                    uint8_t packet_type = static_cast<uint8_t>(udp_buffer_[0]);

                    // PING: echo back immediately
                    if (packet_type == chatproj::UdpPacketType::PING) {
                        boost::system::error_code sec;
                        udp_socket_.send_to(boost::asio::buffer(udp_buffer_, bytes_recvd),
                                            udp_sender_endpoint_, 0, sec);
                        return;
                    }

                    // AUDIO or STREAM_AUDIO
                    constexpr int SID = chatproj::SENDER_ID_SIZE;
                    if ((packet_type == chatproj::UdpPacketType::AUDIO ||
                         packet_type == chatproj::UdpPacketType::STREAM_AUDIO) &&
                        bytes_recvd >= 1 + SID + 4) {

                        std::string token_str;
                        chatproj::UdpAudioPacket* packet = reinterpret_cast<chatproj::UdpAudioPacket*>(udp_buffer_);
                        for (int i = 0; i < SID; ++i) {
                            if (packet->sender_id[i] == '\0') break;
                            token_str.push_back(packet->sender_id[i]);
                        }

                        // Same defence as the VIDEO relay: a declared
                        // payload_size that overruns the datagram is a lie
                        // that would otherwise be amplified to the channel.
                        constexpr size_t AUDIO_HEADER = 1 + SID + 2 + 2;
                        if (static_cast<size_t>(packet->payload_size) > bytes_recvd - AUDIO_HEADER) {
                            token_str.clear();
                        }

                        if (!token_str.empty()) {
                            auto session = manager_.find_session_by_token(token_str, jwt_secret_);
                            if (session) {
                                if (session->get_udp_endpoint() != udp_sender_endpoint_) {
                                    session->set_udp_endpoint(udp_sender_endpoint_);
                                }
                                std::string channel = session->get_current_voice_channel();
                                if (!channel.empty()) {
                                    std::string uname = session->get_username();
                                    std::memset(udp_buffer_ + 1, 0, SID);
                                    std::memcpy(udp_buffer_ + 1, uname.c_str(),
                                                std::min(uname.size(), size_t(SID - 1)));

                                    if (packet_type == chatproj::UdpPacketType::AUDIO) {
                                        // Server-muted (or -deafened) by a moderator:
                                        // drop at the relay; the client can't bypass it.
                                        if (!session->is_server_muted()) {
                                            manager_.broadcast_to_voice_channel(
                                                udp_buffer_, bytes_recvd, channel, session, udp_socket_);
                                        }
                                    } else if (packet_type == chatproj::UdpPacketType::STREAM_AUDIO) {
                                        // Stream audio stays on voice path (small, latency-sensitive).
                                        // Send to each watcher's *voice* endpoint so it lands on
                                        // their voice recv loop alongside regular AUDIO — the media
                                        // recv loop only handles VIDEO / FEC.
                                        manager_.broadcast_to_watchers_voice(
                                            udp_buffer_, bytes_recvd, channel, uname, udp_socket_);
                                    }
                                }
                            }
                        }
                    }
                }
    }

    // ── Media UDP receive chain (VIDEO, FEC, KEYFRAME_REQUEST, NACK) ────────
    void do_receive_media_udp() {
        media_udp_socket_.async_receive_from(
            boost::asio::buffer(media_udp_buffer_, sizeof(media_udp_buffer_)), media_udp_sender_endpoint_,
            [this](boost::system::error_code ec, std::size_t bytes_recvd) {
                // See do_receive_voice_udp: guarantee the tail re-arm even if
                // a datagram handler throws, so the media chain never dies
                // silently (R1).
                try {
                    if (!ec) handle_media_datagram(bytes_recvd);
                    // Drain everything else already queued in the kernel
                    // before yielding to the reactor: one datagram per epoll
                    // wakeup was the dominant per-packet cost under load.
                    // Bounded so TCP handlers still get the thread.
                    for (int i = 0; i < kMaxDrainPerWakeup; ++i) {
                        boost::system::error_code dec;
                        const std::size_t n = media_udp_socket_.receive_from(
                            boost::asio::buffer(media_udp_buffer_, sizeof(media_udp_buffer_)), media_udp_sender_endpoint_, 0, dec);
                        if (dec) break;   // would_block = drained
                        handle_media_datagram(n);
                    }
                } catch (const std::exception& e) {
                    std::cerr << "[Community] media datagram handler threw: " << e.what() << " — continuing\n";
                } catch (...) {
                    std::cerr << "[Community] media datagram handler threw (unknown) — continuing\n";
                }
                do_receive_media_udp();
            });
    }

    void handle_media_datagram(std::size_t bytes_recvd) {
                if (bytes_recvd >= 1) {
                    uint8_t packet_type = static_cast<uint8_t>(media_udp_buffer_[0]);
                    constexpr int SID = chatproj::SENDER_ID_SIZE;

                    // PING: authenticate, register/refresh the sender's media
                    // endpoint so broadcast_to_watchers can reach them, then
                    // echo back for RTT measurement. Pure watchers (who never
                    // send VIDEO themselves) rely on this to receive relay.
                    if (packet_type == chatproj::UdpPacketType::PING &&
                        bytes_recvd >= 1 + SID + 4) {
                        std::string token_str;
                        chatproj::UdpAudioPacket* packet =
                            reinterpret_cast<chatproj::UdpAudioPacket*>(media_udp_buffer_);
                        for (int i = 0; i < SID; ++i) {
                            if (packet->sender_id[i] == '\0') break;
                            token_str.push_back(packet->sender_id[i]);
                        }
                        if (!token_str.empty()) {
                            auto session = manager_.find_session_by_token(token_str, jwt_secret_);
                            if (session && session->get_udp_media_endpoint() != media_udp_sender_endpoint_) {
                                session->set_udp_media_endpoint(media_udp_sender_endpoint_);
                            }
                        }
                        boost::system::error_code sec;
                        media_udp_socket_.send_to(boost::asio::buffer(media_udp_buffer_, bytes_recvd),
                                                  media_udp_sender_endpoint_, 0, sec);
                        return;
                    }

                    // KEYFRAME_REQUEST: relay to the target streamer
                    if (packet_type == chatproj::UdpPacketType::KEYFRAME_REQUEST &&
                        bytes_recvd >= sizeof(chatproj::UdpKeyframeRequest)) {
                        chatproj::UdpKeyframeRequest* packet =
                            reinterpret_cast<chatproj::UdpKeyframeRequest*>(media_udp_buffer_);
                        // Require the requester to be an authenticated
                        // session. Without this, any host that can reach
                        // this port could flood keyframe requests at a
                        // named streamer, forcing continuous IDRs and
                        // collapsing quality for every real viewer.
                        std::string requester_token;
                        for (int i = 0; i < SID; ++i) {
                            if (packet->sender_id[i] == '\0') break;
                            requester_token.push_back(packet->sender_id[i]);
                        }
                        if (requester_token.empty() ||
                            !manager_.find_session_by_token(requester_token, jwt_secret_)) {
                            return;
                        }
                        std::string target;
                        for (int i = 0; i < SID; ++i) {
                            if (packet->target_username[i] == '\0') break;
                            target.push_back(packet->target_username[i]);
                        }
                        if (!target.empty()) {
                            manager_.relay_keyframe_request(target, media_udp_socket_);
                        }
                        return;
                    }

                    // NACK: relay to the target streamer
                    if (packet_type == chatproj::UdpPacketType::NACK &&
                        bytes_recvd >= sizeof(chatproj::UdpNackPacket) - sizeof(uint16_t) * chatproj::NACK_MAX_ENTRIES) {
                        chatproj::UdpNackPacket* packet =
                            reinterpret_cast<chatproj::UdpNackPacket*>(media_udp_buffer_);
                        // Require an authenticated requester before relaying
                        // (and amplifying) attacker-controlled NACK payloads
                        // to a streamer — see the KEYFRAME_REQUEST branch.
                        std::string requester_token;
                        for (int i = 0; i < SID; ++i) {
                            if (packet->sender_id[i] == '\0') break;
                            requester_token.push_back(packet->sender_id[i]);
                        }
                        if (requester_token.empty() ||
                            !manager_.find_session_by_token(requester_token, jwt_secret_)) {
                            return;
                        }
                        std::string target;
                        for (int i = 0; i < SID; ++i) {
                            if (packet->target_username[i] == '\0') break;
                            target.push_back(packet->target_username[i]);
                        }
                        if (!target.empty()) {
                            manager_.relay_nack(media_udp_buffer_, bytes_recvd, target, media_udp_socket_);
                        }
                        return;
                    }

                    // VIDEO or FEC: authenticate, rewrite sender_id, broadcast to watchers
                    if ((packet_type == chatproj::UdpPacketType::VIDEO ||
                         packet_type == chatproj::UdpPacketType::FEC) &&
                        bytes_recvd >= 1 + SID + 8) {

                        std::string token_str;
                        chatproj::UdpVideoPacket* packet =
                            reinterpret_cast<chatproj::UdpVideoPacket*>(media_udp_buffer_);

                        // Defence-in-depth: a VIDEO packet is relayed
                        // verbatim to every watcher, so reject one whose
                        // declared payload_size overruns the datagram before
                        // it's amplified. Rejects only lies — a well-formed
                        // compact packet has bytes_recvd == 45 + payload_size,
                        // and a legacy full-size one has payload_size <= 1200
                        // <= bytes_recvd - 45. (The || short-circuit means
                        // payload_size is only read once the 45-byte header
                        // is known to be present. FEC uses a different layout
                        // and is left to the receiver's own bounds checks.)
                        if (packet_type == chatproj::UdpPacketType::VIDEO) {
                            constexpr size_t VIDEO_HEADER = 45;
                            if (bytes_recvd < VIDEO_HEADER ||
                                static_cast<size_t>(packet->payload_size) >
                                    bytes_recvd - VIDEO_HEADER) {
                                return;
                            }
                        }

                        for (int i = 0; i < SID; ++i) {
                            if (packet->sender_id[i] == '\0') break;
                            token_str.push_back(packet->sender_id[i]);
                        }

                        if (!token_str.empty()) {
                            auto session = manager_.find_session_by_token(token_str, jwt_secret_);
                            if (session) {
                                if (session->get_udp_media_endpoint() != media_udp_sender_endpoint_) {
                                    session->set_udp_media_endpoint(media_udp_sender_endpoint_);
                                }
                                std::string channel = session->get_current_voice_channel();
                                if (!channel.empty()) {
                                    std::string uname = session->get_username();
                                    std::memset(media_udp_buffer_ + 1, 0, SID);
                                    std::memcpy(media_udp_buffer_ + 1, uname.c_str(),
                                                std::min(uname.size(), size_t(SID - 1)));

                                    manager_.broadcast_to_watchers(
                                        media_udp_buffer_, bytes_recvd, channel, uname, media_udp_socket_);
                                }
                            }
                        }
                    }
                }
    }

    static constexpr int kMaxDrainPerWakeup = 256;
    tcp::acceptor acceptor_;
    boost::asio::steady_timer accept_backoff_;
    boost::asio::ip::udp::socket udp_socket_;
    boost::asio::ip::udp::socket media_udp_socket_;
    char udp_buffer_[sizeof(chatproj::UdpVideoPacket) > sizeof(chatproj::UdpFecPacket) ? sizeof(chatproj::UdpVideoPacket) : sizeof(chatproj::UdpFecPacket)];
    char media_udp_buffer_[sizeof(chatproj::UdpVideoPacket) > sizeof(chatproj::UdpFecPacket) ? sizeof(chatproj::UdpVideoPacket) : sizeof(chatproj::UdpFecPacket)];
    boost::asio::ip::udp::endpoint udp_sender_endpoint_;
    boost::asio::ip::udp::endpoint media_udp_sender_endpoint_;
    ssl::context ssl_context_;
    SessionManager& manager_;
    std::string jwt_secret_;
};

void send_heartbeat(boost::asio::io_context& io_context, boost::asio::steady_timer& timer,
                    const std::string& central_host, int central_port,
                    const std::string& public_ip, int community_port,
                    SessionManager& manager, const std::string& community_secret) {
    // Build heartbeat packet. Name/description come from the DB every
    // tick so an in-app rename (SERVER_UPDATE_REQ) reaches the central
    // directory within a minute.
    chatproj::Packet packet;
    packet.set_type(chatproj::Packet::SERVER_HEARTBEAT);
    packet.set_auth_token(community_secret);
    auto* hb = packet.mutable_server_heartbeat();
    if (auto* db = manager.db()) {
        hb->set_name(db->server_name());
        hb->set_description(db->server_description());
        hb->set_public_listing(db->public_listing());
    }
    hb->set_host_ip(public_ip);
    hb->set_port(community_port);
    hb->set_member_count(static_cast<int>(manager.member_count()));
    // Stable identity across IP/port changes (0 until first learned).
    hb->set_server_id(manager.server_id());
    hb->set_cert_fingerprint(manager.cert_fingerprint());

    std::string serialized;
    packet.SerializeToString(&serialized);
    auto framed = chatproj::create_framed_packet(serialized);

    // Run the whole exchange on a detached thread. The previous inline
    // version did blocking resolve/connect/handshake/read on the ONE
    // io_context thread — which also relays every voice/video UDP packet
    // — so a slow or blackholed central froze the entire server for
    // however long the kernel took to give up on the connect. The read
    // deadline it carried was inert for the same reason: the timer's
    // handler needed the very thread that was blocked. The detached
    // thread is bounded by kCentralSyncTimeout inside
    // send_to_central_blocking, so threads can't pile up either.
    // `manager` and its CommunityDb outlive io_context.run() in main(),
    // so the reference captures are safe; server_id_ is atomic and the
    // DB serializes internally.
    // Goes through the single central-sync worker (see central_sync.hpp)
    // like every other central exchange; the response callback runs on
    // that thread — server_id_ is atomic and the DB serialises internally.
    manager.enqueue_central(std::move(framed), /*read_response=*/true,
        [central_host, central_port, &manager](bool ok, const std::vector<uint8_t>& body) {
            if (!ok) {
                std::cerr << "[Heartbeat] Failed to send\n";
                return;
            }
            chatproj::Packet resp;
            if (resp.ParseFromArray(body.data(), static_cast<int>(body.size())) &&
                resp.type() == chatproj::Packet::SERVER_HEARTBEAT_RES) {
                int64_t id = resp.server_heartbeat_res().server_id();
                if (id > 0 && manager.server_id() != id) {
                    manager.set_server_id(id);
                    if (auto* db = manager.db()) {
                        db->set_central_server_id(id);
                    }
                    std::cout << "[Heartbeat] Cached central server_id = "
                              << id << "\n";
                }
            }
            std::cout << "[Heartbeat] Sent to central server (" << central_host
                      << ":" << central_port << ")\n";
        });

    // Schedule next heartbeat in 60 seconds
    timer.expires_after(std::chrono::seconds(60));
    timer.async_wait([&io_context, &timer, &central_host, central_port,
                      &public_ip, community_port,
                      &manager, &community_secret](boost::system::error_code ec) {
        if (!ec) {
            send_heartbeat(io_context, timer, central_host, central_port,
                           public_ip, community_port,
                           manager, community_secret);
        }
    });
}

int main() {
    try {
        const char* jwt_pub_env = std::getenv("DECIBELL_JWT_PUBLIC_KEY_FILE");
        const char* secret_env = std::getenv("DECIBELL_COMMUNITY_SECRET");
        const char* central_pin_env = std::getenv("DECIBELL_CENTRAL_CERT_FINGERPRINT");
        const char* central_host_env = std::getenv("DECIBELL_CENTRAL_HOST");
        const char* server_name_env = std::getenv("DECIBELL_SERVER_NAME");
        const char* server_desc_env = std::getenv("DECIBELL_SERVER_DESC");
        const char* public_ip_env = std::getenv("DECIBELL_PUBLIC_IP");
        const char* owner_env = std::getenv("DECIBELL_OWNER_USERNAME");
        const char* db_path_env = std::getenv("DECIBELL_DB_PATH");
        const char* attachments_root_env = std::getenv("DECIBELL_ATTACHMENTS_ROOT");
        const char* max_attachment_env = std::getenv("DECIBELL_MAX_ATTACHMENT_BYTES");
        const char* auth_timeout_env = std::getenv("DECIBELL_AUTH_TIMEOUT_SECONDS");
        const char* idle_timeout_env = std::getenv("DECIBELL_IDLE_TIMEOUT_SECONDS");
        const char* retention_interval_env = std::getenv("DECIBELL_RETENTION_INTERVAL_SECONDS");

        if (std::getenv("DECIBELL_JWT_SECRET")) {
            std::cerr << "[Community] DECIBELL_JWT_SECRET is no longer used (Theme A): tokens are verified "
                         "with central's Ed25519 public key (DECIBELL_JWT_PUBLIC_KEY_FILE), and the "
                         "central sync secret is DECIBELL_COMMUNITY_SECRET.\n";
        }
        if (!jwt_pub_env || !secret_env || secret_env[0] == '\0') {
            std::cerr << "Missing required environment variables:\n";
            if (!jwt_pub_env) std::cerr << "  DECIBELL_JWT_PUBLIC_KEY_FILE (central's jwt_ed25519.pem.pub)\n";
            if (!secret_env || secret_env[0] == '\0')
                std::cerr << "  DECIBELL_COMMUNITY_SECRET (shared with central; authenticates heartbeats / sync)\n";
            return 1;
        }
        const std::string jwt_public_pem = chatproj::read_file(jwt_pub_env);
        if (jwt_public_pem.find("PUBLIC KEY") == std::string::npos) {
            std::cerr << "[Community] " << jwt_pub_env << " is not a PEM public key\n";
            return 1;
        }
        std::string community_secret = secret_env;
        std::string central_cert_pin = central_pin_env ? central_pin_env : "";
        // Our own certificate's fingerprint, advertised to central so
        // clients can pin us.
        const std::string own_cert_fingerprint =
            chatproj::cert_fingerprint_from_pem(chatproj::read_file("server.crt"));
        if (own_cert_fingerprint.empty()) {
            std::cerr << "[Community] Could not read server.crt to compute its fingerprint\n";
            return 1;
        }
        std::cout << "[Community] TLS certificate fingerprint: " << own_cert_fingerprint << "\n";
        std::string central_host = central_host_env ? central_host_env : "127.0.0.1";
        std::string server_name = server_name_env ? server_name_env : "Community Server";
        std::string server_desc = server_desc_env ? server_desc_env : "";
        std::string public_ip = public_ip_env ? public_ip_env : "127.0.0.1";
        std::string owner_username = owner_env ? owner_env : "";
        std::string db_path = db_path_env ? db_path_env : "decibell_community.db";
        std::string attachments_root = attachments_root_env ? attachments_root_env : "attachments";
        int64_t max_attachment_bytes = 100LL * 1024 * 1024; // 100 MB default
        if (max_attachment_env) {
            try { max_attachment_bytes = std::stoll(max_attachment_env); }
            catch (...) { /* keep default on parse failure */ }
        }
        auto env_seconds = [](const char* v, int def) {
            if (!v) return def;
            try { int n = std::stoi(v); return n > 0 ? n : def; } catch (...) { return def; }
        };
        const int auth_timeout_s = env_seconds(auth_timeout_env, 10);
        const int idle_timeout_s = env_seconds(idle_timeout_env, 90);
        const int retention_interval_s = env_seconds(retention_interval_env, 600);

        // Open (or create) the persistent DB. If the file doesn't exist yet
        // we require DECIBELL_OWNER_USERNAME so we know who to seed as owner.
        chatproj::CommunityDb db;
        {
            std::ifstream probe(db_path);
            bool fresh = !probe.good();
            if (fresh && owner_username.empty()) {
                std::cerr << "[Community] DB " << db_path
                          << " does not exist yet and DECIBELL_OWNER_USERNAME is unset.\n"
                             "          Set DECIBELL_OWNER_USERNAME to the username that "
                             "should own this server and restart.\n";
                return 1;
            }
            if (!db.open(db_path, owner_username, server_name, server_desc)) {
                std::cerr << "[Community] Failed to open database.\n";
                return 1;
            }
        }

        boost::asio::io_context io_context;
        SessionManager manager;
        chatproj::Authorizer authz(db);
        manager.set_db(&db);
        manager.set_authz(&authz);
        manager.set_io_context(io_context);
        manager.set_timeouts(auth_timeout_s, idle_timeout_s);
        // Auto-rejoin: hydrate the cached central server_id (learned
        // via SERVER_HEARTBEAT_RES on a previous run) so membership-
        // sync packets work as soon as the first user auths after
        // restart, even before the first heartbeat lands.
        if (int64_t cached = db.central_server_id(); cached > 0) {
            manager.set_server_id(cached);
            std::cout << "[Community] Loaded cached central server_id = "
                      << cached << "\n";
        }
        manager.set_cert_fingerprint(own_cert_fingerprint);
        manager.set_central_sync(central_host, 8080, community_secret, public_ip, 8082, central_cert_pin);
        // Attachment HTTP/TLS listener. port+3 (= 8085 by default).
        const int attachment_port = 8082 + 3;
        manager.set_attachment_config(attachment_port, max_attachment_bytes);
        manager.set_storage_paths(db_path, attachments_root);
        // Seed the min-free headroom from the env var while it's still at the
        // built-in default; the Storage tab is the source of truth thereafter
        // (same DB-wins pattern as server name).
        if (const char* mf = std::getenv("DECIBELL_MIN_FREE_BYTES")) {
            if (db.min_free_bytes() == chatproj::kDefaultMinFreeBytes) {
                try { db.set_min_free_bytes(std::stoll(mf)); } catch (...) {}
            }
        }
        CommunityServer s(io_context, 8082, manager, jwt_public_pem);
        AttachmentHttpServer attachment_server(io_context,
                                               static_cast<unsigned short>(attachment_port),
                                               db, jwt_public_pem, attachments_root,
                                               max_attachment_bytes);
        attachment_server.set_authz(&authz);
        std::cout << "Decibell Community Server running on port 8082...\n";
        std::cout << "[Community] Owner: " << db.owner()
                  << " | Members: " << manager.member_count() << "\n";

        // Re-register every still-live invite with central so clients can
        // resolve raw codes after a restart. Central does UPSERT so this is
        // safe to call unconditionally.
        {
            const int64_t now = static_cast<int64_t>(std::time(nullptr));
            int registered = 0;
            for (const auto& inv : db.list_invites()) {
                if (inv.expires_at != 0 && inv.expires_at <= now) continue;
                manager.sync_invite_register(inv.code, inv.expires_at);
                ++registered;
            }
            if (registered > 0) {
                std::cout << "[Community] Re-registered " << registered
                          << " active invite(s) with central.\n";
            }
        }

        // Start heartbeat timer. Pull the authoritative server name/description
        // from the DB so the central directory reflects any rename.
        boost::asio::steady_timer heartbeat_timer(io_context);
        send_heartbeat(io_context, heartbeat_timer, central_host, 8080,
                       public_ip, 8082, manager, community_secret);

        // Retention pruner. Fires every 10 minutes — long enough to be
        // negligible overhead, short enough that users see retention-capped
        // content disappear within one coffee break of the cutoff.
        boost::asio::steady_timer retention_timer(io_context);
        std::function<void(const boost::system::error_code&)> retention_fn;
        retention_fn = [&](const boost::system::error_code& ec) {
            if (ec) return;
            manager.run_retention_sweep();
            // Audit entries older than 180 days.
            db.prune_audit(static_cast<int64_t>(std::time(nullptr)) - 180LL * 86400);
            retention_timer.expires_after(std::chrono::seconds(retention_interval_s));
            retention_timer.async_wait(retention_fn);
        };
        // First sweep after ~30s so the server has settled and any fresh-open
        // DB migrations are past.
        retention_timer.expires_after(std::chrono::seconds(std::min(30, retention_interval_s)));
        retention_timer.async_wait(retention_fn);

        // A handler that throws (an unexpected std::filesystem error, a
        // bad_alloc on one session) must not take the whole server down:
        // log it and keep serving. run() returns normally only when there
        // is no more work, which never happens while the acceptors live.
        for (;;) {
            try {
                io_context.run();
                break;
            } catch (const std::exception& e) {
                std::cerr << "[Community] Unhandled exception in io loop: "
                          << e.what() << " — continuing\n";
            }
        }
    } catch (std::exception& e) {
        std::cerr << "Exception: " << e.what() << "\n";
    }
    return 0;
}