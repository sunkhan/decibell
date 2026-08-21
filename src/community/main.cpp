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
}

/// Builds a CHANNEL_LIST_UPDATE packet with the full ordered channel
/// list. Broadcast to every member after any create/rename/delete.
chatproj::Packet build_channel_list_packet(chatproj::CommunityDb* db) {
    chatproj::Packet p;
    p.set_type(chatproj::Packet::CHANNEL_LIST_UPDATE);
    auto* update = p.mutable_channel_list_update();
    if (db) {
        for (const auto& ch : db->list_channels()) {
            fill_channel_info(update->add_channels(), ch);
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
    // Broadcast to every authenticated session on this server. Text channels
    // use this: every member is implicitly subscribed to every channel, so a
    // CHANNEL_MSG fan-out goes to the whole server rather than a per-channel
    // presence set.
    void broadcast_to_members(const chatproj::Packet& packet);
    // Push a fresh MEMBER_LIST_RES to every authenticated session so their
    // members sidebar reflects joins, departures, kicks, bans, and online
    // flips without having to re-open the server. Sessions whose user
    // holds BAN_MEMBERS (or the owner) also receive the ban list;
    // everyone else gets members-only.
    void broadcast_members();
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
    void remove_watcher(std::shared_ptr<Session> watcher, const std::string& channel_id, const std::string& streamer_username);
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
                            const std::string& actor);

    // Central-hosted invite sync. Community servers register each live invite
    // with central so clients can redeem a raw code without knowing host:port.
    void set_central_sync(const std::string& central_host, int central_port,
                          const std::string& jwt_secret,
                          const std::string& public_ip, int community_port);
    void sync_invite_register(const std::string& code, int64_t expires_at);
    void sync_invite_unregister(const std::string& code);

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

    // Attachment config — reported to clients on CommunityAuthResponse so
    // they know where to upload and what size cap to pre-validate against.
    void set_attachment_config(int port, int64_t max_bytes) {
        attachment_port_ = port;
        max_attachment_bytes_ = max_bytes;
    }
    int attachment_port() const { return attachment_port_; }
    int64_t max_attachment_bytes() const { return max_attachment_bytes_; }

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
    boost::asio::ip::udp::socket* udp_socket_ptr_ = nullptr;
    boost::asio::ip::udp::socket* media_udp_socket_ptr_ = nullptr;

    // O(1) UDP sender_id → session lookup (key = last 31 chars of JWT)
    std::unordered_map<std::string, std::shared_ptr<Session>> udp_key_index_;
    // username → every authenticated live session for that user. Replaces
    // the linear scans of sessions_ on the NACK/keyframe relay path and
    // lets kick/ban reach all of a user's sessions, not just the first.
    std::unordered_map<std::string, std::vector<std::shared_ptr<Session>>> sessions_by_user_;

    chatproj::CommunityDb* db_ = nullptr;

    // Central-sync config (populated once at startup via set_central_sync).
    std::string central_host_;
    int central_port_ = 0;
    std::string central_jwt_secret_;
    std::string public_ip_;
    int community_port_ = 0;

    int attachment_port_ = 0;
    int64_t max_attachment_bytes_ = 0;

    // Auto-rejoin: central-assigned community_servers.id. 0 means
    // "not yet learned" — sync_membership_register/revoke silently
    // skip when 0. Loaded from CommunityDb at startup; refreshed by
    // every SERVER_HEARTBEAT_RES.
    std::atomic<int64_t> server_id_{0};

    std::mutex mutex_;
};

class Session : public std::enable_shared_from_this<Session> {
public:
    Session(tcp::socket socket, SessionManager& manager, ssl::context& context, const std::string& jwt_secret)
        : socket_(std::move(socket), context),
          close_timer_(socket_.lowest_layer().get_executor()),
          manager_(manager), jwt_secret_(jwt_secret) {
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
        socket_.async_handshake(ssl::stream_base::server,
            [this, self](const boost::system::error_code& error) {
                if (!error) {
                    do_read_header();
                } else {
                    manager_.leave(shared_from_this());
                }
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
                    if (length > 2 * 1024 * 1024) {
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

            // Step 1: JWT verification.
            std::string candidate_username;
            try {
                auto decoded = jwt::decode(token);
                auto verifier = jwt::verify()
                    .allow_algorithm(jwt::algorithm::hs256{jwt_secret_})
                    .with_issuer("decibell_central_auth");
                verifier.verify(decoded);
                candidate_username = decoded.get_subject();
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

            if (db->is_banned(candidate_username)) {
                std::cout << "[Community] Blocked banned user: " << candidate_username << "\n";
                send_auth_response(false, "You are banned from this server.", "banned");
                manager_.leave(shared_from_this());
                close_after_flush();
                return;
            }

            bool member = db->is_member(candidate_username);
            if (!member) {
                if (invite_code.empty()) {
                    send_auth_response(false,
                        "Membership required. An invite code is needed to join this server.",
                        "not_member");
                    manager_.leave(shared_from_this());
                    close_after_flush();
                    return;
                }
                chatproj::DbInvite consumed;
                auto result = db->redeem_invite(invite_code, candidate_username, &consumed);
                switch (result) {
                    case chatproj::InviteResult::Ok:
                        if (!db->add_member(candidate_username)) {
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

            // Step 3: accept.
            authenticated_ = true;
            username_ = candidate_username;
            token_ = token;

            constexpr size_t UDP_KEY_LEN = chatproj::SENDER_ID_SIZE - 1;
            if (token_.size() >= UDP_KEY_LEN) {
                udp_key_ = token_.substr(token_.size() - UDP_KEY_LEN);
            } else {
                udp_key_ = token_;
            }
            manager_.register_authenticated(shared_from_this());

            std::cout << "[Community] Authorized user: " << username_ << "\n";
            send_auth_response(true, "Authentication successful.", "");
            // Push the role list up-front so the client can resolve
            // member role_ids and gate its admin UI without a round trip.
            send_packet(build_role_list_packet(manager_.db()));
            manager_.send_initial_voice_presences(shared_from_this());
            // Tell every existing member about the roster change. Covers both
            // a brand-new member (just added via invite redemption) and a
            // returning member flipping from offline to online.
            manager_.broadcast_members();
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
            // Capture client capabilities (Plan A Group 7). Empty when sent
            // by a legacy client; treated downstream as "H.264 only".
            if (jvr.has_capabilities()) {
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
            manager_.remove_watcher(shared_from_this(), req.channel_id(), req.target_username());
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

            // Persist before broadcast so the id we echo to clients matches
            // what history_res will return. Server stamps the authoritative
            // timestamp at the same time to ensure retention ordering isn't
            // subject to client clock drift.
            const int64_t now_ts = static_cast<int64_t>(std::time(nullptr));
            msg->set_timestamp(now_ts);
            int64_t new_id = 0;
            if (auto* db = manager_.db()) {
                new_id = db->insert_message(
                    msg->channel_id(), username_, msg->content(), now_ts);
                if (new_id > 0) {
                    msg->set_id(new_id);
                } else {
                    std::cerr << "[Community] Failed to persist CHANNEL_MSG from "
                              << username_ << " in #" << msg->channel_id() << "\n";
                }
            }

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
                        pa->set_url(row.storage_path);
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

            manager_.broadcast_to_members(routed);
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

            bool has_more = false;
            auto msgs = db->fetch_messages(
                req.channel_id(), req.before_id(), req.limit(), &has_more);

            // Load attachments for this page in one query.
            std::vector<int64_t> msg_ids;
            msg_ids.reserve(msgs.size());
            for (const auto& m : msgs) msg_ids.push_back(m.id);
            auto attachments = db->fetch_attachments_for_messages(msg_ids);

            std::unordered_map<int64_t, std::vector<const chatproj::DbAttachment*>> by_msg;
            for (const auto& a : attachments) {
                by_msg[a.message_id].push_back(&a);
            }

            // Reverse so the client receives oldest→newest within the page,
            // matching the order they'll render.
            for (auto it = msgs.rbegin(); it != msgs.rend(); ++it) {
                auto* cm = res->add_messages();
                cm->set_id(it->id);
                cm->set_sender(it->sender);
                cm->set_channel_id(it->channel_id);
                cm->set_content(it->content);
                cm->set_timestamp(it->timestamp);
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
                        proto_a->set_url(a->storage_path);
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
            if (!db->has_permission(username_, chatproj::perms::kManageChannels)) {
                res->set_success(false);
                res->set_message("You don't have permission to edit channels.");
                send_packet(p);
                return;
            }
            const auto& req = packet.channel_update_req();
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
            res->set_success(ok);
            res->set_message(ok ? "Channel updated." : "Channel not found.");
            if (!ok) {
                // Failures go to the requester only — everyone else has
                // nothing to refresh.
                send_packet(p);
                return;
            }
            {
                if (auto ch = db->get_channel(req.channel_id())) {
                    auto* info = res->mutable_channel();
                    info->set_id(ch->id);
                    info->set_name(ch->name);
                    info->set_type(channel_type_to_proto(ch->type));
                    info->set_voice_bitrate_kbps(ch->voice_bitrate_kbps);
                    info->set_retention_days_text(ch->retention_days_text);
                    info->set_retention_days_image(ch->retention_days_image);
                    info->set_retention_days_video(ch->retention_days_video);
                    info->set_retention_days_document(ch->retention_days_document);
                    info->set_retention_days_audio(ch->retention_days_audio);
                }
            }
            // Fan out to every authenticated session so everyone sees the new
            // retention settings immediately (they need it rendered in the
            // channel sidebar + any open edit modals).
            manager_.broadcast_to_members(p);
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
            if (!db->has_permission(username_, chatproj::perms::kManageChannels)) {
                res->set_success(false);
                res->set_message("You don't have permission to wipe channel history.");
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
            manager_.broadcast_to_members(bcast);

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

            if (*sender != username_ && !db->can_delete_others(username_)) {
                res->set_success(false);
                res->set_message("You don't have permission to delete this message.");
                send_packet(rsp);
                return;
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
            manager_.broadcast_to_members(bcast);

            std::cout << "[Community] message " << req.message_id()
                      << " in #" << req.channel_id()
                      << " deleted by " << username_
                      << " (" << del.unlink_paths.size() << " attachments)\n";
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
            if (!db->has_permission(username_, chatproj::perms::kManageServer)) {
                res->set_success(false);
                res->set_message("You don't have permission to change the server picture.");
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
            if (!db->has_permission(username_, chatproj::perms::kManageInvites)) {
                send_simple_mod_res(chatproj::Packet::INVITE_CREATE_RES, false,
                                    "You don't have permission to create invites.",
                                    "", "");
                return;
            }
            const auto& req = packet.invite_create_req();
            auto created = db->create_invite(username_, req.expires_at(), req.max_uses());

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
            if (!db->has_permission(username_, chatproj::perms::kManageInvites)) {
                res->set_success(false);
                res->set_message("You don't have permission to list invites.");
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
            if (!db->has_permission(username_, chatproj::perms::kManageInvites)) {
                res->set_success(false);
                res->set_message("You don't have permission to revoke invites.");
                send_packet(p);
                return;
            }
            bool ok = db->revoke_invite(code);
            res->set_success(ok);
            res->set_message(ok ? "Invite revoked." : "Invite not found.");
            send_packet(p);
            if (ok) {
                manager_.sync_invite_unregister(code);
            }
        }

        // --- MEMBER LIST ---
        else if (packet.type() == chatproj::Packet::MEMBER_LIST_REQ) {
            auto* db = manager_.db();
            chatproj::Packet p;
            p.set_type(chatproj::Packet::MEMBER_LIST_RES);
            auto* res = p.mutable_member_list_res();
            if (!db) {
                res->set_success(false);
                res->set_message("Server misconfigured.");
                send_packet(p);
                return;
            }
            res->set_success(true);
            const std::string owner_name = db->owner();
            auto online_users = manager_.get_online_usernames();
            std::unordered_map<std::string, std::vector<int64_t>> roles_by_user;
            for (const auto& [user, role_id] : db->list_all_member_roles()) {
                roles_by_user[user].push_back(role_id);
            }
            for (const auto& m : db->list_members()) {
                auto* info = res->add_members();
                info->set_username(m.username);
                info->set_joined_at(m.joined_at);
                info->set_nickname(m.nickname);
                info->set_is_owner(m.username == owner_name);
                info->set_is_online(online_users.count(m.username) > 0);
                if (auto it = roles_by_user.find(m.username); it != roles_by_user.end()) {
                    for (int64_t rid : it->second) info->add_role_ids(rid);
                }
            }
            // The ban list reveals moderation history — only BAN_MEMBERS
            // holders (and the owner, who holds everything) see it.
            if (db->has_permission(username_, chatproj::perms::kBanMembers)) {
                for (const auto& u : db->list_bans()) {
                    res->add_bans(u);
                }
            }
            send_packet(p);
        }

        // --- KICK MEMBER ---
        else if (packet.type() == chatproj::Packet::KICK_MEMBER_REQ) {
            auto* db = manager_.db();
            if (!db) return;
            const std::string& target = packet.kick_member_req().username();
            const std::string& reason = packet.kick_member_req().reason();
            const std::string owner_name = db->owner();
            if (!db->has_permission(username_, chatproj::perms::kKickMembers)) {
                send_simple_mod_res(chatproj::Packet::MOD_ACTION_RES, false,
                                    "You don't have permission to kick members.",
                                    target, "kick");
                return;
            }
            if (target == owner_name) {
                send_simple_mod_res(chatproj::Packet::MOD_ACTION_RES, false,
                                    "Cannot kick the server owner.",
                                    target, "kick");
                return;
            }
            if (target == username_) {
                send_simple_mod_res(chatproj::Packet::MOD_ACTION_RES, false,
                                    "Use leave to remove yourself.",
                                    target, "kick");
                return;
            }
            // Hierarchy: only members strictly below your own highest role
            // can be kicked. Owner level is INT32_MAX, so the owner
            // bypasses this and can never be outranked.
            if (db->member_level(target) >= db->member_level(username_)) {
                send_simple_mod_res(chatproj::Packet::MOD_ACTION_RES, false,
                                    "You can't kick a member with an equal or higher role.",
                                    target, "kick");
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
            // An online target's socket close runs leave() → roster
            // broadcast. An OFFLINE target triggers nothing, so every
            // client (the actor's included) kept listing them as a
            // member — the action looked like it had failed.
            if (removed && closed == 0) {
                manager_.broadcast_members();
            }
        }

        // --- BAN MEMBER ---
        else if (packet.type() == chatproj::Packet::BAN_MEMBER_REQ) {
            auto* db = manager_.db();
            if (!db) return;
            const std::string& target = packet.ban_member_req().username();
            const std::string& reason = packet.ban_member_req().reason();
            const std::string owner_name = db->owner();
            if (!db->has_permission(username_, chatproj::perms::kBanMembers)) {
                send_simple_mod_res(chatproj::Packet::MOD_ACTION_RES, false,
                                    "You don't have permission to ban members.",
                                    target, "ban");
                return;
            }
            if (target == owner_name) {
                send_simple_mod_res(chatproj::Packet::MOD_ACTION_RES, false,
                                    "Cannot ban the server owner.",
                                    target, "ban");
                return;
            }
            if (target == username_) {
                send_simple_mod_res(chatproj::Packet::MOD_ACTION_RES, false,
                                    "Cannot ban yourself.",
                                    target, "ban");
                return;
            }
            // Hierarchy: same rule as kick — only strictly-lower members.
            if (db->member_level(target) >= db->member_level(username_)) {
                send_simple_mod_res(chatproj::Packet::MOD_ACTION_RES, false,
                                    "You can't ban a member with an equal or higher role.",
                                    target, "ban");
                return;
            }
            bool ok = db->add_ban(target, username_, reason);
            // Unconditional for the same reason as the kick path: the
            // target may be offline and force_disconnect would skip it.
            manager_.sync_membership_revoke(target);
            const size_t closed = manager_.force_disconnect(target, "ban", reason, username_);
            send_simple_mod_res(chatproj::Packet::MOD_ACTION_RES, ok,
                                ok ? "Member banned." : "Ban failed.",
                                target, "ban");
            // Offline target: no leave() will fire, so push the roster
            // (and the refreshed ban list for BAN_MEMBERS holders) now.
            if (ok && closed == 0) {
                manager_.broadcast_members();
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
            if (!db->has_permission(username_, chatproj::perms::kBanMembers)) {
                send_simple_mod_res(chatproj::Packet::MOD_ACTION_RES, false,
                                    "You don't have permission to unban members.",
                                    target, "unban");
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
                manager_.broadcast_members();
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
            if (!db->has_permission(username_, chatproj::perms::kManageRoles)) {
                res->set_success(false);
                res->set_message("You don't have permission to manage roles.");
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
            if (!db->has_permission(username_, chatproj::perms::kManageRoles)) {
                fail("You don't have permission to manage roles.");
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
            manager_.broadcast_roles();
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
            if (!db->has_permission(username_, chatproj::perms::kManageRoles)) {
                fail("You don't have permission to manage roles.");
                return;
            }
            auto role = db->get_role(req.role_id());
            if (!role) { fail("Role not found."); return; }
            if (role->is_default) { fail("The default role can't be deleted."); return; }
            if (role->position >= db->member_level(username_)) {
                fail("You can't delete a role at or above your highest role.");
                return;
            }
            if (!db->delete_role(req.role_id())) {
                fail("Failed to delete role.");
                return;
            }
            res->set_success(true);
            send_packet(p);
            std::cout << "[Community] Role '" << role->name
                      << "' deleted by " << username_ << "\n";
            manager_.broadcast_roles();
            // Members holding the role lost it (cascade) — refresh rosters.
            manager_.broadcast_members();
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
            if (!db->has_permission(username_, chatproj::perms::kManageRoles)) {
                fail("You don't have permission to manage roles.");
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
            // The refreshed roster (with role_ids) is the authoritative
            // confirmation for every client, requester included.
            manager_.broadcast_members();
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
            if (!db->has_permission(username_, chatproj::perms::kManageChannels)) {
                fail("You don't have permission to manage channels.");
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
            if (!db->has_permission(username_, chatproj::perms::kManageChannels)) {
                fail("You don't have permission to manage channels.");
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
            if (!db->has_permission(username_, chatproj::perms::kManageChannels)) {
                fail("You don't have permission to manage channels.");
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
            manager_.broadcast_channels();
        }

        // --- SET NICKNAME ---
        else if (packet.type() == chatproj::Packet::SET_NICKNAME_REQ) {
            auto* db = manager_.db();
            if (!db) return;
            const auto& req = packet.set_nickname_req();
            const std::string& target = req.username();
            std::string nickname = chatproj::clamp_utf8(req.nickname(), chatproj::kMaxNicknameBytes);

            if (target != username_) {
                // Changing someone else's nickname: MANAGE_NICKNAMES +
                // strictly-higher role (owner bypasses via level).
                if (!db->has_permission(username_, chatproj::perms::kManageNicknames)) {
                    send_simple_mod_res(chatproj::Packet::MOD_ACTION_RES, false,
                                        "You don't have permission to manage nicknames.",
                                        target, "nickname");
                    return;
                }
                if (db->member_level(target) >= db->member_level(username_)) {
                    send_simple_mod_res(chatproj::Packet::MOD_ACTION_RES, false,
                                        "You can't change the nickname of a member "
                                        "with an equal or higher role.",
                                        target, "nickname");
                    return;
                }
            }
            bool ok = db->set_nickname(target, nickname);
            send_simple_mod_res(chatproj::Packet::MOD_ACTION_RES, ok,
                                ok ? "" : "Not a member.",
                                target, "nickname");
            if (ok) {
                manager_.broadcast_members();
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
            if (!db->has_permission(username_, chatproj::perms::kManageChannels)) {
                fail("You don't have permission to manage channels.");
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
                for (const auto& ch : db->list_channels()) {
                    auto* info = res->add_channels();
                    info->set_id(ch.id);
                    info->set_name(ch.name);
                    info->set_type(channel_type_to_proto(ch.type));
                    info->set_voice_bitrate_kbps(ch.voice_bitrate_kbps);
                    info->set_retention_days_text(ch.retention_days_text);
                    info->set_retention_days_image(ch.retention_days_image);
                    info->set_retention_days_video(ch.retention_days_video);
                    info->set_retention_days_document(ch.retention_days_document);
                    info->set_retention_days_audio(ch.retention_days_audio);
                }
                res->set_server_name(db->server_name());
                res->set_server_description(db->server_description());
                res->set_owner_username(db->owner());
            }
            res->set_max_attachment_bytes(manager_.max_attachment_bytes());
            res->set_attachment_port(manager_.attachment_port());
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
    SessionManager& manager_;
    char inbound_header_[4];
    std::vector<uint8_t> inbound_body_;

    std::string jwt_secret_;
    bool authenticated_ = false;
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
                if (vec.empty()) sessions_by_user_.erase(by_user);
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
            if (it->second.erase(username) > 0) {
                affected_stream_channels.push_back(it->first);
            }
            it = it->second.empty() ? active_streams_.erase(it) : std::next(it);
        }
        // Clean up watcher state in both directions: entries where this
        // session watches someone, AND the watcher set of this session's
        // own stream — a disconnected streamer's set would otherwise
        // survive and resume relaying to stale watchers if they came
        // back and streamed again (stop_stream clears it; leave didn't).
        for (auto ch_it = stream_watchers_.begin(); ch_it != stream_watchers_.end();) {
            auto& streamers = ch_it->second;
            bool watcher_removed = false;
            for (auto st_it = streamers.begin(); st_it != streamers.end();) {
                if (st_it->second.erase(session) > 0) watcher_removed = true;
                if (st_it->first == username || st_it->second.empty()) {
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
    // Stream may have been active when this user dropped; clear the
    // popup-preview cache regardless. Idempotent on non-streamers.
    erase_thumbnail_cache(session->get_username());
    // Broadcast updated presence to remaining clients (outside lock to avoid deadlock)
    for (const auto& ch : affected_voice_channels) {
        broadcast_voice_presence(ch);
    }
    for (const auto& ch : affected_stream_channels) {
        broadcast_stream_presence(ch);
    }
    if (was_authenticated) {
        broadcast_members();
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
            if (it->second.erase(session->get_username()) > 0) {
                removed = true;
            }
            if (it->second.empty()) active_streams_.erase(it);
        }
        // Clean up watchers for this stream
        auto wch = stream_watchers_.find(channel_id);
        if (wch != stream_watchers_.end()) {
            wch->second.erase(session->get_username());
            if (wch->second.empty()) stream_watchers_.erase(wch);
        }
    }
    // Drop any cached thumbnail for this streamer; popup viewers
    // will now get an empty response (and they'll stop polling
    // once the next stream-presence event removes the entry from
    // their streamsByUser map).
    erase_thumbnail_cache(session->get_username());
    if (removed) {
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

    std::lock_guard<std::mutex> lock(mutex_);
    for (auto& session : sessions_) {
        // Only authenticated sessions receive presence. A peer that merely
        // completed the TLS handshake (never sent COMMUNITY_AUTH_REQ) must
        // not passively harvest usernames / codec caps / mute state.
        if (session->is_authenticated()) {
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

void SessionManager::broadcast_members() {
    if (!db_) return;

    // Snapshot DB state first — db_ takes its own mutex so doing this
    // outside mutex_ keeps lock acquisition orders consistent.
    const std::string owner_name = db_->owner();
    auto members = db_->list_members();
    auto bans = db_->list_bans();
    std::unordered_map<std::string, std::vector<int64_t>> roles_by_user;
    for (const auto& [user, role_id] : db_->list_all_member_roles()) {
        roles_by_user[user].push_back(role_id);
    }

    // Compute online set + fan-out targets under session mutex.
    std::set<std::string> online;
    std::vector<std::pair<std::shared_ptr<Session>, std::string>> targets;
    {
        std::lock_guard<std::mutex> lock(mutex_);
        targets.reserve(sessions_.size());
        for (const auto& s : sessions_) {
            if (s->is_authenticated() && !s->get_username().empty()) {
                online.insert(s->get_username());
                targets.emplace_back(s, s->get_username());
            }
        }
    }

    // Ban-list visibility follows BAN_MEMBERS (owner holds every
    // permission implicitly). Computed after releasing mutex_ — these
    // are DB lookups.
    std::set<std::string> sees_bans;
    for (const auto& u : online) {
        if (db_->has_permission(u, chatproj::perms::kBanMembers)) {
            sees_bans.insert(u);
        }
    }

    auto frame_pkt = [](const chatproj::Packet& p) {
        std::string serialized;
        p.SerializeToString(&serialized);
        uint32_t length = htonl(static_cast<uint32_t>(serialized.size()));
        auto framed = std::make_shared<std::vector<uint8_t>>();
        framed->resize(4 + serialized.size());
        std::memcpy(framed->data(), &length, 4);
        std::memcpy(framed->data() + 4, serialized.data(), serialized.size());
        return framed;
    };

    chatproj::Packet pkt_no_bans;
    pkt_no_bans.set_type(chatproj::Packet::MEMBER_LIST_RES);
    {
        auto* res = pkt_no_bans.mutable_member_list_res();
        res->set_success(true);
        for (const auto& m : members) {
            auto* info = res->add_members();
            info->set_username(m.username);
            info->set_joined_at(m.joined_at);
            info->set_nickname(m.nickname);
            info->set_is_owner(m.username == owner_name);
            info->set_is_online(online.count(m.username) > 0);
            if (auto it = roles_by_user.find(m.username); it != roles_by_user.end()) {
                for (int64_t rid : it->second) info->add_role_ids(rid);
            }
        }
    }

    chatproj::Packet pkt_with_bans = pkt_no_bans;
    for (const auto& u : bans) {
        pkt_with_bans.mutable_member_list_res()->add_bans(u);
    }

    auto framed_no_bans = frame_pkt(pkt_no_bans);
    auto framed_with_bans = bans.empty() ? framed_no_bans : frame_pkt(pkt_with_bans);

    for (const auto& [session, user] : targets) {
        session->deliver(sees_bans.count(user) > 0 ? framed_with_bans
                                                   : framed_no_bans);
    }
}

void SessionManager::broadcast_roles() {
    if (!db_) return;
    broadcast_to_members(build_role_list_packet(db_));
}

void SessionManager::broadcast_channels() {
    if (!db_) return;
    broadcast_to_members(build_channel_list_packet(db_));
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

    // Build one CHANNEL_PRUNED packet per affected channel and fan out to every
    // authenticated session so their local state stays in sync without reload.
    for (const auto& cp : sweeps) {
        chatproj::Packet p;
        p.set_type(chatproj::Packet::CHANNEL_PRUNED);
        auto* msg = p.mutable_channel_pruned();
        msg->set_channel_id(cp.channel_id);
        for (auto id : cp.deleted_message_ids) {
            msg->add_deleted_message_ids(id);
        }
        for (const auto& pa : cp.purged_attachments) {
            auto* t = msg->add_purged_attachments();
            t->set_attachment_id(pa.attachment_id);
            t->set_purged_at(pa.purged_at);
        }
        broadcast_to_members(p);
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

void SessionManager::broadcast_to_voice_channel(const char* data, size_t length, const std::string& channel_id, std::shared_ptr<Session> sender, boost::asio::ip::udp::socket& udp_socket) {
    // Copy the data into a shared buffer so it remains valid for the async sends,
    // since the caller's udp_buffer_ will be overwritten by the next received packet.
    auto buffer = std::make_shared<std::vector<char>>(data, data + length);

    // Snapshot recipient endpoints under the lock, then release it before
    // issuing async_send_to calls. Holding the SessionManager mutex across
    // per-recipient iteration serialized every other voice-channel operation
    // (joins, leaves, state updates) behind the fanout loop — the dominant
    // cause of voice glitches when more than two users shared a channel.
    std::vector<boost::asio::ip::udp::endpoint> targets;
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
                && !session->is_deafened()) {
                targets.push_back(session->get_udp_endpoint());
            }
        }
    }

    for (auto& ep : targets) {
        udp_socket.async_send_to(
            boost::asio::buffer(*buffer), ep,
            [buffer](boost::system::error_code /*ec*/, std::size_t /*bytes_sent*/) {
                // buffer captured to extend its lifetime until send completes
            });
    }
}

void SessionManager::relay_keyframe_request(const std::string& target_username, boost::asio::ip::udp::socket& udp_socket) {
    // Build a minimal KEYFRAME_REQUEST packet to send to the streamer
    chatproj::UdpKeyframeRequest req;
    req.packet_type = chatproj::UdpPacketType::KEYFRAME_REQUEST;
    std::memset(req.sender_id, 0, chatproj::SENDER_ID_SIZE);
    std::memset(req.target_username, 0, chatproj::SENDER_ID_SIZE);

    std::lock_guard<std::mutex> lock(mutex_);
    // Rate-limit PLIs per streamer (250ms): a client must not be able to force
    // continuous IDRs by spamming WATCH / UDP keyframe requests, and many
    // watchers' near-simultaneous requests coalesce into a single IDR.
    {
        auto now = std::chrono::steady_clock::now();
        auto it = last_keyframe_relay_.find(target_username);
        if (it != last_keyframe_relay_.end() &&
            now - it->second < std::chrono::milliseconds(250)) {
            return;
        }
        last_keyframe_relay_[target_username] = now;
    }
    auto it = sessions_by_user_.find(target_username);
    if (it == sessions_by_user_.end()) return;
    for (auto& session : it->second) {
        if (session->get_udp_media_endpoint().port() != 0) {
            auto buffer = std::make_shared<std::vector<char>>(reinterpret_cast<const char*>(&req),
                                                               reinterpret_cast<const char*>(&req) + sizeof(req));
            udp_socket.async_send_to(
                boost::asio::buffer(*buffer), session->get_udp_media_endpoint(),
                [buffer](boost::system::error_code, std::size_t) {});
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
            auto buffer = std::make_shared<std::vector<char>>(data, data + length);
            udp_socket.async_send_to(
                boost::asio::buffer(*buffer), session->get_udp_media_endpoint(),
                [buffer](boost::system::error_code, std::size_t) {});
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

void SessionManager::remove_watcher(std::shared_ptr<Session> watcher, const std::string& channel_id, const std::string& streamer_username) {
    std::lock_guard<std::mutex> lock(mutex_);
    auto ch_it = stream_watchers_.find(channel_id);
    if (ch_it != stream_watchers_.end()) {
        auto st_it = ch_it->second.find(streamer_username);
        if (st_it != ch_it->second.end()) {
            st_it->second.erase(watcher);
            if (st_it->second.empty()) ch_it->second.erase(st_it);
            if (ch_it->second.empty()) stream_watchers_.erase(ch_it);
        }
    }
}

void SessionManager::broadcast_to_watchers(const char* data, size_t length, const std::string& channel_id,
                                            const std::string& streamer_username, boost::asio::ip::udp::socket& udp_socket) {
    auto buffer = std::make_shared<std::vector<char>>(data, data + length);

    // Snapshot watcher endpoints under the lock, release, then send. Same
    // rationale as broadcast_to_voice_channel — keeps the manager mutex free
    // for joins/leaves while the video/audio fanout is in flight.
    std::vector<boost::asio::ip::udp::endpoint> targets;
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

    for (auto& ep : targets) {
        udp_socket.async_send_to(
            boost::asio::buffer(*buffer), ep,
            [buffer](boost::system::error_code, std::size_t) {});
    }
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
    auto buffer = std::make_shared<std::vector<char>>(data, data + length);
    std::vector<boost::asio::ip::udp::endpoint> targets;
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
    for (auto& ep : targets) {
        udp_socket.async_send_to(
            boost::asio::buffer(*buffer), ep,
            [buffer](boost::system::error_code, std::size_t) {});
    }
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

    std::lock_guard<std::mutex> lock(mutex_);
    for (auto& session : sessions_) {
        // Authenticated sessions only — an unauthenticated TLS peer must
        // not receive the voice roster (usernames + mute/deafen state).
        if (session->is_authenticated()) {
            session->deliver(framed);
        }
    }
}

void SessionManager::send_initial_voice_presences(std::shared_ptr<Session> session) {
    std::lock_guard<std::mutex> lock(mutex_);
    for (const auto& pair : voice_channels_) {
        const std::string& channel_id = pair.first;
        if (pair.second.empty()) continue;

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
    std::lock_guard<std::mutex> lock(mutex_);
    udp_key_index_[session->get_udp_key()] = session;
    auto& vec = sessions_by_user_[session->get_username()];
    if (std::find(vec.begin(), vec.end(), session) == vec.end()) {
        vec.push_back(session);
    }
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
                                        const std::string& actor) {
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

void SessionManager::set_central_sync(const std::string& central_host, int central_port,
                                      const std::string& jwt_secret,
                                      const std::string& public_ip, int community_port) {
    central_host_ = central_host;
    central_port_ = central_port;
    central_jwt_secret_ = jwt_secret;
    public_ip_ = public_ip;
    community_port_ = community_port;
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
                              bool read_response = false,
                              chatproj::Packet* out_response = nullptr) {
    try {
        boost::asio::io_context io;
        ssl::context ctx(ssl::context::tlsv12_client);
        ctx.set_verify_mode(ssl::verify_none);

        tcp::resolver resolver(io);
        ssl::stream<tcp::socket> ssl_socket(io, ctx);

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
    packet.set_auth_token(central_jwt_secret_);
    auto* req = packet.mutable_membership_revoke_req();
    req->set_username(username);
    req->set_server_id(sid);

    std::string serialized;
    packet.SerializeToString(&serialized);
    auto framed = chatproj::create_framed_packet(serialized);

    std::string host = central_host_;
    int port = central_port_;
    std::thread([host, port, framed = std::move(framed)]() {
        send_to_central_blocking(host, port, framed);
    }).detach();
}

void SessionManager::sync_server_picture(const std::string& data,
                                            const std::string& version) {
    if (central_host_.empty() || central_port_ == 0) return;
    chatproj::Packet packet;
    packet.set_type(chatproj::Packet::SYNC_SERVER_PICTURE_REQ);
    packet.set_auth_token(central_jwt_secret_);
    auto* req = packet.mutable_sync_server_picture_req();
    req->set_host(public_ip_);
    req->set_port(community_port_);
    req->set_data(data);
    req->set_version(version);

    std::string serialized;
    packet.SerializeToString(&serialized);
    auto framed = chatproj::create_framed_packet(serialized);

    std::string host = central_host_;
    int port = central_port_;
    std::thread([host, port, framed = std::move(framed)]() {
        send_to_central_blocking(host, port, framed);
    }).detach();
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
    packet.set_auth_token(central_jwt_secret_);
    auto* req = packet.mutable_membership_register_req();
    req->set_username(username);
    req->set_server_id(sid);

    std::string serialized;
    packet.SerializeToString(&serialized);
    auto framed = chatproj::create_framed_packet(serialized);

    std::string host = central_host_;
    int port = central_port_;
    std::thread([host, port, framed = std::move(framed)]() {
        send_to_central_blocking(host, port, framed);
    }).detach();
}

void SessionManager::sync_invite_register(const std::string& code, int64_t expires_at) {
    if (central_host_.empty() || central_port_ == 0) return;

    chatproj::Packet packet;
    packet.set_type(chatproj::Packet::INVITE_REGISTER_REQ);
    packet.set_auth_token(central_jwt_secret_);
    auto* req = packet.mutable_invite_register_req();
    req->set_code(code);
    req->set_host(public_ip_);
    req->set_port(static_cast<uint32_t>(community_port_));
    req->set_expires_at(expires_at);

    std::string serialized;
    packet.SerializeToString(&serialized);
    auto framed = chatproj::create_framed_packet(serialized);

    std::string host = central_host_;
    int port = central_port_;
    std::thread([host, port, framed = std::move(framed)]() {
        send_to_central_blocking(host, port, framed);
    }).detach();
}

void SessionManager::sync_invite_unregister(const std::string& code) {
    if (central_host_.empty() || central_port_ == 0) return;

    chatproj::Packet packet;
    packet.set_type(chatproj::Packet::INVITE_UNREGISTER_REQ);
    packet.set_auth_token(central_jwt_secret_);
    auto* req = packet.mutable_invite_unregister_req();
    req->set_code(code);

    std::string serialized;
    packet.SerializeToString(&serialized);
    auto framed = chatproj::create_framed_packet(serialized);

    std::string host = central_host_;
    int port = central_port_;
    std::thread([host, port, framed = std::move(framed)]() {
        send_to_central_blocking(host, port, framed);
    }).detach();
}

class CommunityServer {
public:
    CommunityServer(boost::asio::io_context& io_context, short port, SessionManager& manager, const std::string& jwt_secret)
        : acceptor_(io_context, tcp::endpoint(tcp::v4(), port)),
          udp_socket_(io_context, boost::asio::ip::udp::endpoint(boost::asio::ip::udp::v4(), port + 1)),
          media_udp_socket_(io_context, boost::asio::ip::udp::endpoint(boost::asio::ip::udp::v4(), port + 2)),
          ssl_context_(ssl::context::tlsv12),
          manager_(manager),
          jwt_secret_(jwt_secret) {

        ssl_context_.set_options(
            ssl::context::default_workarounds |
            ssl::context::no_sslv2 |
            ssl::context::no_sslv3 |
            ssl::context::no_tlsv1 |
            ssl::context::no_tlsv1_1);

        ssl_context_.use_certificate_chain_file("server.crt");
        ssl_context_.use_private_key_file("server.key", ssl::context::pem);

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
                    auto session = std::make_shared<Session>(std::move(socket), manager_, ssl_context_, jwt_secret_);
                    manager_.join(session);
                    session->start();
                }
                do_accept();
            });
    }

    // ── Voice UDP receive chain (AUDIO, STREAM_AUDIO, PING) ──────────────────
    void do_receive_voice_udp() {
        udp_socket_.async_receive_from(
            boost::asio::buffer(udp_buffer_, sizeof(udp_buffer_)), udp_sender_endpoint_,
            [this](boost::system::error_code ec, std::size_t bytes_recvd) {
                if (!ec && bytes_recvd >= 1) {
                    uint8_t packet_type = static_cast<uint8_t>(udp_buffer_[0]);

                    // PING: echo back immediately
                    if (packet_type == chatproj::UdpPacketType::PING) {
                        auto echo_buf = std::make_shared<std::vector<uint8_t>>(
                            udp_buffer_, udp_buffer_ + bytes_recvd);
                        udp_socket_.async_send_to(
                            boost::asio::buffer(*echo_buf), udp_sender_endpoint_,
                            [echo_buf](boost::system::error_code, std::size_t) {});
                        do_receive_voice_udp();
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
                                        manager_.broadcast_to_voice_channel(
                                            udp_buffer_, bytes_recvd, channel, session, udp_socket_);
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
                do_receive_voice_udp();
            });
    }

    // ── Media UDP receive chain (VIDEO, FEC, KEYFRAME_REQUEST, NACK) ────────
    void do_receive_media_udp() {
        media_udp_socket_.async_receive_from(
            boost::asio::buffer(media_udp_buffer_, sizeof(media_udp_buffer_)), media_udp_sender_endpoint_,
            [this](boost::system::error_code ec, std::size_t bytes_recvd) {
                if (!ec && bytes_recvd >= 1) {
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
                        auto echo_buf = std::make_shared<std::vector<uint8_t>>(
                            media_udp_buffer_, media_udp_buffer_ + bytes_recvd);
                        media_udp_socket_.async_send_to(
                            boost::asio::buffer(*echo_buf), media_udp_sender_endpoint_,
                            [echo_buf](boost::system::error_code, std::size_t) {});
                        do_receive_media_udp();
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
                            do_receive_media_udp();
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
                        do_receive_media_udp();
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
                            do_receive_media_udp();
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
                        do_receive_media_udp();
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
                                do_receive_media_udp();
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
                do_receive_media_udp();
            });
    }

    tcp::acceptor acceptor_;
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
                    const std::string& server_name, const std::string& server_desc,
                    const std::string& public_ip, int community_port,
                    SessionManager& manager, const std::string& jwt_secret) {
    // Build heartbeat packet
    chatproj::Packet packet;
    packet.set_type(chatproj::Packet::SERVER_HEARTBEAT);
    packet.set_auth_token(jwt_secret);
    auto* hb = packet.mutable_server_heartbeat();
    hb->set_name(server_name);
    hb->set_description(server_desc);
    hb->set_host_ip(public_ip);
    hb->set_port(community_port);
    hb->set_member_count(static_cast<int>(manager.member_count()));

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
    std::thread([central_host, central_port, framed = std::move(framed), &manager]() {
        chatproj::Packet resp;
        if (!send_to_central_blocking(central_host, central_port, framed,
                                      /*read_response=*/true, &resp)) {
            std::cerr << "[Heartbeat] Failed to send\n";
            return;
        }
        if (resp.type() == chatproj::Packet::SERVER_HEARTBEAT_RES) {
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
    }).detach();

    // Schedule next heartbeat in 60 seconds
    timer.expires_after(std::chrono::seconds(60));
    timer.async_wait([&io_context, &timer, &central_host, central_port,
                      &server_name, &server_desc, &public_ip, community_port,
                      &manager, &jwt_secret](boost::system::error_code ec) {
        if (!ec) {
            send_heartbeat(io_context, timer, central_host, central_port,
                           server_name, server_desc, public_ip, community_port,
                           manager, jwt_secret);
        }
    });
}

int main() {
    try {
        const char* jwt_env = std::getenv("DECIBELL_JWT_SECRET");
        const char* central_host_env = std::getenv("DECIBELL_CENTRAL_HOST");
        const char* server_name_env = std::getenv("DECIBELL_SERVER_NAME");
        const char* server_desc_env = std::getenv("DECIBELL_SERVER_DESC");
        const char* public_ip_env = std::getenv("DECIBELL_PUBLIC_IP");
        const char* owner_env = std::getenv("DECIBELL_OWNER_USERNAME");
        const char* db_path_env = std::getenv("DECIBELL_DB_PATH");
        const char* attachments_root_env = std::getenv("DECIBELL_ATTACHMENTS_ROOT");
        const char* max_attachment_env = std::getenv("DECIBELL_MAX_ATTACHMENT_BYTES");

        if (!jwt_env) {
            std::cerr << "Missing required environment variable: DECIBELL_JWT_SECRET\n";
            return 1;
        }

        std::string jwt_secret = jwt_env;
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
        manager.set_db(&db);
        // Auto-rejoin: hydrate the cached central server_id (learned
        // via SERVER_HEARTBEAT_RES on a previous run) so membership-
        // sync packets work as soon as the first user auths after
        // restart, even before the first heartbeat lands.
        if (int64_t cached = db.central_server_id(); cached > 0) {
            manager.set_server_id(cached);
            std::cout << "[Community] Loaded cached central server_id = "
                      << cached << "\n";
        }
        manager.set_central_sync(central_host, 8080, jwt_secret, public_ip, 8082);
        // Attachment HTTP/TLS listener. port+3 (= 8085 by default).
        const int attachment_port = 8082 + 3;
        manager.set_attachment_config(attachment_port, max_attachment_bytes);
        CommunityServer s(io_context, 8082, manager, jwt_secret);
        AttachmentHttpServer attachment_server(io_context,
                                               static_cast<unsigned short>(attachment_port),
                                               db, jwt_secret, attachments_root,
                                               max_attachment_bytes);
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
        std::string hb_name = db.server_name();
        std::string hb_desc = db.server_description();
        send_heartbeat(io_context, heartbeat_timer, central_host, 8080,
                       hb_name, hb_desc, public_ip, 8082,
                       manager, jwt_secret);

        // Retention pruner. Fires every 10 minutes — long enough to be
        // negligible overhead, short enough that users see retention-capped
        // content disappear within one coffee break of the cutoff.
        boost::asio::steady_timer retention_timer(io_context);
        std::function<void(const boost::system::error_code&)> retention_fn;
        retention_fn = [&](const boost::system::error_code& ec) {
            if (ec) return;
            manager.run_retention_sweep();
            retention_timer.expires_after(std::chrono::minutes(10));
            retention_timer.async_wait(retention_fn);
        };
        // First sweep after ~30s so the server has settled and any fresh-open
        // DB migrations are past.
        retention_timer.expires_after(std::chrono::seconds(30));
        retention_timer.async_wait(retention_fn);

        io_context.run();
    } catch (std::exception& e) {
        std::cerr << "Exception: " << e.what() << "\n";
    }
    return 0;
}