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
#include <mutex>
#include <thread>
#include <utility>
#include <system_error>
#include <boost/asio.hpp>
#include <boost/asio/ssl.hpp>
#include <boost/asio/steady_timer.hpp>
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
    // flips without having to re-open the server. The owner also receives
    // the ban list; everyone else gets members-only.
    void broadcast_members();
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
    void start_stream(std::shared_ptr<Session> session, const std::string& channel_id, bool has_audio);
    void stop_stream(std::shared_ptr<Session> session, const std::string& channel_id);
    void broadcast_stream_presence(const std::string& channel_id);

    // Watcher tracking
    void add_watcher(std::shared_ptr<Session> watcher, const std::string& channel_id, const std::string& streamer_username);
    void remove_watcher(std::shared_ptr<Session> watcher, const std::string& channel_id, const std::string& streamer_username);
    void broadcast_to_watchers(const char* data, size_t length, const std::string& channel_id, const std::string& streamer_username, boost::asio::ip::udp::socket& udp_socket);
    void set_udp_socket(boost::asio::ip::udp::socket* sock) { udp_socket_ptr_ = sock; }
    void set_media_udp_socket(boost::asio::ip::udp::socket* sock) { media_udp_socket_ptr_ = sock; }
    void register_udp_key(const std::string& udp_key, std::shared_ptr<Session> session);
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
    std::shared_ptr<Session> find_session_by_username(const std::string& username);

    // Forcibly disconnect a session — sends MEMBERSHIP_REVOKED then closes.
    // Also cleans up channel/voice/stream membership via leave().
    void force_disconnect(const std::string& username,
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
    struct StreamInfo { bool has_audio; };
    std::unordered_map<std::string, std::unordered_map<std::string, StreamInfo>> active_streams_;

    // channel_id -> streamer_username -> set of watcher sessions
    std::unordered_map<std::string,
        std::unordered_map<std::string, std::set<std::shared_ptr<Session>>>>
        stream_watchers_;

    uint32_t max_streams_per_channel_ = 8;  // 0 = unlimited
    boost::asio::ip::udp::socket* udp_socket_ptr_ = nullptr;
    boost::asio::ip::udp::socket* media_udp_socket_ptr_ = nullptr;

    // O(1) UDP sender_id → session lookup (key = last 31 chars of JWT)
    std::unordered_map<std::string, std::shared_ptr<Session>> udp_key_index_;

    chatproj::CommunityDb* db_ = nullptr;

    // Central-sync config (populated once at startup via set_central_sync).
    std::string central_host_;
    int central_port_ = 0;
    std::string central_jwt_secret_;
    std::string public_ip_;
    int community_port_ = 0;

    int attachment_port_ = 0;
    int64_t max_attachment_bytes_ = 0;

    std::mutex mutex_;
};

class Session : public std::enable_shared_from_this<Session> {
public:
    Session(tcp::socket socket, SessionManager& manager, ssl::context& context, const std::string& jwt_secret)
        : socket_(std::move(socket), context), manager_(manager), jwt_secret_(jwt_secret) {
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
        bool write_in_progress = false;
        {
            std::lock_guard<std::mutex> lock(write_mutex_);
            write_in_progress = !write_queue_.empty();
            write_queue_.push_back(framed_data);
        }
        
        if (!write_in_progress) {
            do_write();
        }
    }

    std::string get_username() const { return username_; }
    std::string get_token() const { return token_; }
    std::string get_udp_key() const { return udp_key_; }
    bool is_authenticated() const { return authenticated_; }

    // Forcibly close the underlying TCP socket. Any in-flight reads/writes
    // will error out, which triggers SessionManager::leave via the normal
    // error-handling path. Safe to call from any thread.
    void close_connection() {
        boost::system::error_code ec;
        socket_.lowest_layer().cancel(ec);
        socket_.lowest_layer().close(ec);
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
                    uint32_t length = ntohl(*reinterpret_cast<uint32_t*>(inbound_header_));
                    if (length > 2 * 1024 * 1024) return;
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
                    process_packet();
                    do_read_header();
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
                return;
            }

            // Step 2: Membership + invite gating.
            auto* db = manager_.db();
            if (!db) {
                send_auth_response(false, "Server misconfigured.", "auth");
                manager_.leave(shared_from_this());
                return;
            }

            if (db->is_banned(candidate_username)) {
                std::cout << "[Community] Blocked banned user: " << candidate_username << "\n";
                send_auth_response(false, "You are banned from this server.", "banned");
                manager_.leave(shared_from_this());
                return;
            }

            bool member = db->is_member(candidate_username);
            if (!member) {
                if (invite_code.empty()) {
                    send_auth_response(false,
                        "Membership required. An invite code is needed to join this server.",
                        "not_member");
                    manager_.leave(shared_from_this());
                    return;
                }
                chatproj::DbInvite consumed;
                auto result = db->redeem_invite(invite_code, candidate_username, &consumed);
                switch (result) {
                    case chatproj::InviteResult::Ok:
                        if (!db->add_member(candidate_username)) {
                            send_auth_response(false, "Failed to record membership.", "auth");
                            manager_.leave(shared_from_this());
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
                        return;
                    case chatproj::InviteResult::Unknown:
                    case chatproj::InviteResult::Expired:
                    case chatproj::InviteResult::Exhausted:
                    default:
                        send_auth_response(false,
                            "Invite code is invalid, expired, or has been used up.",
                            "invalid_invite");
                        manager_.leave(shared_from_this());
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
            manager_.register_udp_key(udp_key_, shared_from_this());

            std::cout << "[Community] Authorized user: " << username_ << "\n";
            send_auth_response(true, "Authentication successful.", "");
            manager_.send_initial_voice_presences(shared_from_this());
            // Tell every existing member about the roster change. Covers both
            // a brand-new member (just added via invite redemption) and a
            // returning member flipping from offline to online.
            manager_.broadcast_members();
            return;
        }

        // Client keepalive ping — just acknowledge, no response needed.
        // Skip auth check: pings may arrive before auth completes.
        if (packet.type() == chatproj::Packet::CLIENT_PING) {
            return;
        }

        // Drop unauthenticated traffic
        if (!authenticated_) return;

        // --- JOIN VOICE CHANNEL ---
        if (packet.type() == chatproj::Packet::JOIN_VOICE_REQ) {
            std::string target_channel = packet.join_voice_req().channel_id();
            manager_.join_voice_channel(shared_from_this(), target_channel, current_voice_channel_);
            current_voice_channel_ = target_channel;
            std::cout << "[Community] " << username_ << " joined voice channel " << target_channel << "\n";
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
            manager_.start_stream(shared_from_this(), req.channel_id(), req.has_audio());
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
            manager_.add_watcher(shared_from_this(), req.channel_id(), req.target_username());
            std::cout << "[Community] " << username_ << " watching " << req.target_username() << "'s stream in " << req.channel_id() << "\n";
            // Send PLI to streamer so new watcher gets a keyframe
            manager_.relay_keyframe_request_internal(req.target_username());
        }

        // --- STOP WATCHING STREAM ---
        else if (packet.type() == chatproj::Packet::STOP_WATCHING_REQ) {
            const auto& req = packet.stop_watching_req();
            manager_.remove_watcher(shared_from_this(), req.channel_id(), req.target_username());
            std::cout << "[Community] " << username_ << " stopped watching " << req.target_username() << "'s stream\n";
        }

        // --- STREAM THUMBNAIL UPDATE ---
        else if (packet.type() == chatproj::Packet::STREAM_THUMBNAIL_UPDATE) {
            auto* update = packet.mutable_stream_thumbnail_update();
            update->set_owner_username(username_); // Enforce identity
            std::string channel_id = update->channel_id();
            // Broadcast to all voice channel participants (not just watchers)
            manager_.broadcast_to_voice_channel_tcp(packet, channel_id);
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
            auto* msg = routed.mutable_channel_msg();
            msg->set_sender(username_); // Enforce identity

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
            if (db->owner() != username_) {
                res->set_success(false);
                res->set_message("Only the server owner can edit channels.");
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
            res->set_success(ok);
            res->set_message(ok ? "Channel updated." : "Channel not found.");
            if (ok) {
                if (auto ch = db->get_channel(req.channel_id())) {
                    auto* info = res->mutable_channel();
                    info->set_id(ch->id);
                    info->set_name(ch->name);
                    info->set_type(ch->type == 1
                                    ? chatproj::ChannelInfo::VOICE
                                    : chatproj::ChannelInfo::TEXT);
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

        // --- INVITE: CREATE ---
        else if (packet.type() == chatproj::Packet::INVITE_CREATE_REQ) {
            auto* db = manager_.db();
            if (!db) return;
            if (db->owner() != username_) {
                send_simple_mod_res(chatproj::Packet::INVITE_CREATE_RES, false,
                                    "Only the server owner can create invites.",
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
            if (db->owner() != username_) {
                res->set_success(false);
                res->set_message("Only the server owner can list invites.");
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
            if (db->owner() != username_) {
                res->set_success(false);
                res->set_message("Only the server owner can revoke invites.");
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
            for (const auto& m : db->list_members()) {
                auto* info = res->add_members();
                info->set_username(m.username);
                info->set_joined_at(m.joined_at);
                info->set_nickname(m.nickname);
                info->set_is_owner(m.username == owner_name);
                info->set_is_online(online_users.count(m.username) > 0);
            }
            // Only the owner sees the ban list, since it reveals moderation
            // history. Regular members just get the member roster.
            if (username_ == owner_name) {
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
            if (username_ != owner_name) {
                send_simple_mod_res(chatproj::Packet::MOD_ACTION_RES, false,
                                    "Only the server owner can kick members.",
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
            bool removed = db->remove_member(target);
            // Even if they weren't in the members table, force-disconnect
            // any live session so the UI reflects the action.
            manager_.force_disconnect(target, "kick", reason, username_);
            send_simple_mod_res(chatproj::Packet::MOD_ACTION_RES, removed,
                                removed ? "Member kicked." : "User is not a member.",
                                target, "kick");
        }

        // --- BAN MEMBER ---
        else if (packet.type() == chatproj::Packet::BAN_MEMBER_REQ) {
            auto* db = manager_.db();
            if (!db) return;
            const std::string& target = packet.ban_member_req().username();
            const std::string& reason = packet.ban_member_req().reason();
            const std::string owner_name = db->owner();
            if (username_ != owner_name) {
                send_simple_mod_res(chatproj::Packet::MOD_ACTION_RES, false,
                                    "Only the server owner can ban members.",
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
            bool ok = db->add_ban(target, username_, reason);
            manager_.force_disconnect(target, "ban", reason, username_);
            send_simple_mod_res(chatproj::Packet::MOD_ACTION_RES, ok,
                                ok ? "Member banned." : "Ban failed.",
                                target, "ban");
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
            send_simple_mod_res(chatproj::Packet::MOD_ACTION_RES, true,
                                "You have left the server.",
                                username_, "leave");
            std::cout << "[Community] " << username_ << " left the server\n";
            // Give the write queue a tick to flush, then close.
            manager_.force_disconnect(username_, "leave", "", username_);
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
                    info->set_type(ch.type == 1
                                   ? chatproj::ChannelInfo::VOICE
                                   : chatproj::ChannelInfo::TEXT);
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
    SessionManager& manager_;
    char inbound_header_[4];
    std::vector<uint8_t> inbound_body_;

    std::string jwt_secret_;
    bool authenticated_ = false;
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
        if (!session->get_udp_key().empty()) {
            udp_key_index_.erase(session->get_udp_key());
        }
        sessions_.erase(session);
        for (auto& pair : voice_channels_) {
            if (pair.second.erase(session) > 0) {
                affected_voice_channels.push_back(pair.first);
            }
        }
        for (auto& pair : active_streams_) {
            if (pair.second.erase(session->get_username()) > 0) {
                affected_stream_channels.push_back(pair.first);
            }
        }
        // Clean up any watcher entries for this session
        for (auto& [ch_id, streamers] : stream_watchers_) {
            for (auto& [streamer, watchers] : streamers) {
                watchers.erase(session);
            }
        }
        std::cout << "[Community] Session " << session->get_username() << " left. Total: " << sessions_.size() << "\n";
    }
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

void SessionManager::start_stream(std::shared_ptr<Session> session, const std::string& channel_id, bool has_audio) {
    {
        std::lock_guard<std::mutex> lock(mutex_);
        // Enforce stream limit (0 = unlimited)
        if (max_streams_per_channel_ > 0 && active_streams_[channel_id].size() >= max_streams_per_channel_) {
            std::cout << "[Community] Stream limit reached in " << channel_id << ", rejecting " << session->get_username() << "\n";
            return;
        }
        active_streams_[channel_id][session->get_username()] = { has_audio };
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
    if (removed) {
        broadcast_stream_presence(channel_id);
    }
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
        session->deliver(framed);
    }
}

void SessionManager::join_voice_channel(std::shared_ptr<Session> session, const std::string& new_channel, const std::string& old_channel) {
    {
        std::lock_guard<std::mutex> lock(mutex_);
        if (!old_channel.empty()) {
            voice_channels_[old_channel].erase(session);
            // Clean up watcher entries from old channel
            auto ch_it = stream_watchers_.find(old_channel);
            if (ch_it != stream_watchers_.end()) {
                for (auto& [streamer, watchers] : ch_it->second) {
                    watchers.erase(session);
                }
            }
        }
        voice_channels_[new_channel].insert(session);
    }
    if (!old_channel.empty()) {
        broadcast_voice_presence(old_channel);
    }
    broadcast_voice_presence(new_channel);
    // Send current stream presence for the new channel to the joining user
    broadcast_stream_presence(new_channel);
}

void SessionManager::leave_voice_channel(std::shared_ptr<Session> session, const std::string& current_channel) {
    {
        std::lock_guard<std::mutex> lock(mutex_);
        if (!current_channel.empty()) {
            voice_channels_[current_channel].erase(session);
            // Clean up any watcher entries for this session in this channel
            auto ch_it = stream_watchers_.find(current_channel);
            if (ch_it != stream_watchers_.end()) {
                for (auto& [streamer, watchers] : ch_it->second) {
                    watchers.erase(session);
                }
            }
        }
    }
    if (!current_channel.empty()) {
        broadcast_voice_presence(current_channel);
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

    // Compute online set + fan-out targets under session mutex.
    std::set<std::string> online;
    std::vector<std::pair<std::shared_ptr<Session>, bool>> targets;
    {
        std::lock_guard<std::mutex> lock(mutex_);
        targets.reserve(sessions_.size());
        for (const auto& s : sessions_) {
            if (s->is_authenticated() && !s->get_username().empty()) {
                online.insert(s->get_username());
                targets.emplace_back(s, s->get_username() == owner_name);
            }
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
        }
    }

    chatproj::Packet pkt_with_bans = pkt_no_bans;
    for (const auto& u : bans) {
        pkt_with_bans.mutable_member_list_res()->add_bans(u);
    }

    auto framed_no_bans = frame_pkt(pkt_no_bans);
    auto framed_with_bans = bans.empty() ? framed_no_bans : frame_pkt(pkt_with_bans);

    for (const auto& [session, is_owner] : targets) {
        session->deliver(is_owner ? framed_with_bans : framed_no_bans);
    }
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

    // Abandoned uploads — rows with message_id=0 still in 'uploading' status
    // after more than an hour. A client that crashes, loses power, or just
    // gives up will leave these behind; without this sweep they'd accumulate
    // indefinitely along with their .partial blobs.
    {
        constexpr int64_t kPendingTimeoutSeconds = 3600; // 1 hour
        const int64_t pending_cutoff = now - kPendingTimeoutSeconds;
        auto stale = db_->list_stale_pending_attachments(pending_cutoff);
        for (const auto& a : stale) {
            if (!a.storage_path.empty()) {
                std::error_code ec;
                std::filesystem::remove(a.storage_path + ".partial", ec);
                std::filesystem::remove(a.storage_path + ".thumb.jpg", ec);
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

    // Unlink attachment blobs from disk. Errors are tolerated — missing files
    // just mean a prior sweep already cleaned them. Also unlink the sibling
    // thumbnail (if present) so video posters don't outlive their parent.
    for (const auto& path : unlink_paths) {
        std::error_code ec;
        std::filesystem::remove(path, ec);
        std::filesystem::remove(path + ".thumb.jpg", ec);
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
            if (session != sender && session->get_udp_endpoint().port() != 0) {
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
    for (auto& session : sessions_) {
        if (session->get_username() == target_username && session->get_udp_media_endpoint().port() != 0) {
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
    for (auto& session : sessions_) {
        if (session->get_username() == target_username && session->get_udp_media_endpoint().port() != 0) {
            auto buffer = std::make_shared<std::vector<char>>(data, data + length);
            udp_socket.async_send_to(
                boost::asio::buffer(*buffer), session->get_udp_media_endpoint(),
                [buffer](boost::system::error_code, std::size_t) {});
            return;
        }
    }
}

void SessionManager::add_watcher(std::shared_ptr<Session> watcher, const std::string& channel_id, const std::string& streamer_username) {
    std::lock_guard<std::mutex> lock(mutex_);
    stream_watchers_[channel_id][streamer_username].insert(watcher);
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
        session->deliver(framed);
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

void SessionManager::register_udp_key(const std::string& udp_key, std::shared_ptr<Session> session) {
    std::lock_guard<std::mutex> lock(mutex_);
    udp_key_index_[udp_key] = session;
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
    for (const auto& s : sessions_) {
        if (s->get_username() == username) return s;
    }
    return nullptr;
}

void SessionManager::force_disconnect(const std::string& username,
                                      const std::string& action,
                                      const std::string& reason,
                                      const std::string& actor) {
    auto session = find_session_by_username(username);
    if (!session) return;

    // Best-effort notification before we close the socket. If the write is
    // already queued behind a slow client, the close below cancels it — the
    // target just won't see the reason, which is fine.
    chatproj::Packet p;
    p.set_type(chatproj::Packet::MEMBERSHIP_REVOKED);
    auto* rev = p.mutable_membership_revoked();
    rev->set_action(action);
    rev->set_reason(reason);
    rev->set_actor(actor);
    session->send_packet_external(p);

    session->close_connection();
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
// One-shot TLS send of a framed packet to central. Blocks briefly; call from
// a detached thread so packet-handler coroutines never stall.
void send_to_central_blocking(const std::string& host, int port,
                              const std::vector<uint8_t>& framed) {
    try {
        boost::asio::io_context io;
        ssl::context ctx(ssl::context::tlsv12_client);
        ctx.set_verify_mode(ssl::verify_none);

        tcp::resolver resolver(io);
        auto endpoints = resolver.resolve(host, std::to_string(port));

        tcp::socket raw_socket(io);
        boost::asio::connect(raw_socket, endpoints);

        ssl::stream<tcp::socket> ssl_socket(std::move(raw_socket), ctx);
        ssl_socket.handshake(ssl::stream_base::client);

        boost::asio::write(ssl_socket, boost::asio::buffer(framed));
        ssl_socket.lowest_layer().close();
    } catch (const std::exception& e) {
        std::cerr << "[InviteSync] Failed: " << e.what() << "\n";
    }
}
} // namespace

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
                                        // Stream audio stays on voice path (small, latency-sensitive)
                                        manager_.broadcast_to_watchers(
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

    // Connect to central server over TLS and send
    try {
        ssl::context ctx(ssl::context::tlsv12_client);
        ctx.set_verify_mode(ssl::verify_none);

        tcp::resolver resolver(io_context);
        auto endpoints = resolver.resolve(central_host, std::to_string(central_port));

        tcp::socket raw_socket(io_context);
        boost::asio::connect(raw_socket, endpoints);

        ssl::stream<tcp::socket> ssl_socket(std::move(raw_socket), ctx);
        ssl_socket.handshake(ssl::stream_base::client);

        boost::asio::write(ssl_socket, boost::asio::buffer(framed));
        ssl_socket.lowest_layer().close();

        std::cout << "[Heartbeat] Sent to central server (" << central_host << ":" << central_port << ")\n";
    } catch (const std::exception& e) {
        std::cerr << "[Heartbeat] Failed to send: " << e.what() << "\n";
    }

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