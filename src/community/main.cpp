#ifdef _WIN32
#ifndef WIN32_LEAN_AND_MEAN
#define WIN32_LEAN_AND_MEAN
#endif
#ifndef _WIN32_WINNT
#define _WIN32_WINNT 0x0A00
#endif
#endif

#include <iostream>
#include <string>
#include <memory>
#include <vector>
#include <set>
#include <unordered_map>
#include <mutex>
#include <utility>
#include <boost/asio.hpp>
#include <boost/asio/ssl.hpp>
#include <boost/asio/steady_timer.hpp>
#include <jwt-cpp/traits/nlohmann-json/defaults.h>
#include "messages.pb.h"
#include "../common/net_utils.hpp"
#include "../common/udp_packet.hpp"

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
    void join_channel(std::shared_ptr<Session> session, const std::string& new_channel, const std::string& old_channel);
    void join_voice_channel(std::shared_ptr<Session> session, const std::string& new_channel, const std::string& old_channel);
    void leave_voice_channel(std::shared_ptr<Session> session, const std::string& current_channel);
    void broadcast_to_channel(const chatproj::Packet& packet, const std::string& channel_id);
    void broadcast_to_voice_channel(const char* data, size_t length, const std::string& channel_id, std::shared_ptr<Session> sender, boost::asio::ip::udp::socket& udp_socket);
    void broadcast_to_voice_channel_tcp(const chatproj::Packet& packet, const std::string& channel_id);
    void relay_keyframe_request(const std::string& target_username, boost::asio::ip::udp::socket& udp_socket);
    void relay_nack(const char* data, size_t length, const std::string& target_username, boost::asio::ip::udp::socket& udp_socket);
    void broadcast_voice_presence(const std::string& channel_id);
    void send_initial_voice_presences(std::shared_ptr<Session> session);
    std::shared_ptr<Session> find_session_by_token(const std::string& token, const std::string& jwt_secret);
    std::vector<std::string> get_channel_usernames(const std::string& channel_id);
    void broadcast_channel_members(const std::string& channel_id);

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

private:
    std::set<std::shared_ptr<Session>> sessions_;
    std::unordered_map<std::string, std::set<std::shared_ptr<Session>>> channels_;
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

        // --- STATELESS AUTHENTICATION ---
        if (packet.type() == chatproj::Packet::COMMUNITY_AUTH_REQ) {
            std::string token = packet.community_auth_req().jwt_token();

            try {
                auto decoded = jwt::decode(token);
                auto verifier = jwt::verify()
                    .allow_algorithm(jwt::algorithm::hs256{jwt_secret_})
                    .with_issuer("decibell_central_auth");

                verifier.verify(decoded);

                authenticated_ = true;
                username_ = decoded.get_subject();
                token_ = token;

                // Register UDP key for O(1) lookup: last 31 chars of JWT
                // (matches what the client sends as sender_id in UDP packets)
                constexpr size_t UDP_KEY_LEN = chatproj::SENDER_ID_SIZE - 1; // 31
                if (token_.size() >= UDP_KEY_LEN) {
                    udp_key_ = token_.substr(token_.size() - UDP_KEY_LEN);
                } else {
                    udp_key_ = token_;
                }
                manager_.register_udp_key(udp_key_, shared_from_this());

                std::cout << "[Community] Authorized user: " << username_ << "\n";
                send_auth_response(true, "Authentication successful.");
                manager_.send_initial_voice_presences(shared_from_this());

            } catch (const std::exception& e) {
                std::cout << "[Community] Auth failed: " << e.what() << "\n";
                send_auth_response(false, "Invalid token.");
                manager_.leave(shared_from_this());
            }
            return;
        }

        // Client keepalive ping — just acknowledge, no response needed.
        // Skip auth check: pings may arrive before auth completes.
        if (packet.type() == chatproj::Packet::CLIENT_PING) {
            return;
        }

        // Drop unauthenticated traffic
        if (!authenticated_) return;

        // --- JOIN CHANNEL ---
        if (packet.type() == chatproj::Packet::JOIN_CHANNEL_REQ) {
            std::string target_channel = packet.join_channel_req().channel_id();
            std::string old_channel = current_channel_;
            manager_.join_channel(shared_from_this(), target_channel, old_channel);
            current_channel_ = target_channel;

            // Send join response directly to the joining user (ensures they always get members)
            {
                chatproj::Packet res_pkt;
                res_pkt.set_type(chatproj::Packet::JOIN_CHANNEL_RES);
                auto* res = res_pkt.mutable_join_channel_res();
                res->set_success(true);
                res->set_channel_id(target_channel);
                for (const auto& name : manager_.get_channel_usernames(target_channel)) {
                    res->add_active_users(name);
                }
                send_packet(res_pkt);
            }

            // Broadcast updated member list to other users in the channel
            manager_.broadcast_channel_members(target_channel);

            // Also broadcast updated list to old channel (user left it)
            if (!old_channel.empty() && old_channel != target_channel) {
                manager_.broadcast_channel_members(old_channel);
            }

            std::cout << "[Community] " << username_ << " joined #" << target_channel << "\n";
        }

        // --- JOIN VOICE CHANNEL ---
        else if (packet.type() == chatproj::Packet::JOIN_VOICE_REQ) {
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

            manager_.broadcast_to_channel(routed, msg->channel_id());
            std::cout << "[#" << msg->channel_id() << "] " << username_ << ": " << msg->content() << "\n";
        }
    }

    void send_auth_response(bool success, const std::string& msg) {
        chatproj::Packet p;
        p.set_type(chatproj::Packet::COMMUNITY_AUTH_RES);
        auto* res = p.mutable_community_auth_res();
        res->set_success(success);
        res->set_message(msg);

        if (success) {
            auto* ch1 = res->add_channels();
            ch1->set_id("general");
            ch1->set_name("general");
            ch1->set_type(chatproj::ChannelInfo::TEXT);

            auto* ch2 = res->add_channels();
            ch2->set_id("announcements");
            ch2->set_name("announcements");
            ch2->set_type(chatproj::ChannelInfo::TEXT);

            auto* ch3 = res->add_channels();
            ch3->set_id("voice-lounge");
            ch3->set_name("Voice Lounge");
            ch3->set_type(chatproj::ChannelInfo::VOICE);
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
    std::string current_channel_;
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
    std::vector<std::string> affected_text_channels;
    {
        std::lock_guard<std::mutex> lock(mutex_);
        if (!session->get_udp_key().empty()) {
            udp_key_index_.erase(session->get_udp_key());
        }
        sessions_.erase(session);
        for (auto& pair : channels_) {
            if (pair.second.erase(session) > 0) {
                affected_text_channels.push_back(pair.first);
            }
        }
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
    for (const auto& ch : affected_text_channels) {
        broadcast_channel_members(ch);
    }
    for (const auto& ch : affected_voice_channels) {
        broadcast_voice_presence(ch);
    }
    for (const auto& ch : affected_stream_channels) {
        broadcast_stream_presence(ch);
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

void SessionManager::join_channel(std::shared_ptr<Session> session, const std::string& new_channel, const std::string& old_channel) {
    std::lock_guard<std::mutex> lock(mutex_);
    if (!old_channel.empty()) {
        channels_[old_channel].erase(session);
    }
    channels_[new_channel].insert(session);
}

std::vector<std::string> SessionManager::get_channel_usernames(const std::string& channel_id) {
    std::lock_guard<std::mutex> lock(mutex_);
    std::vector<std::string> usernames;
    auto it = channels_.find(channel_id);
    if (it != channels_.end()) {
        for (const auto& session : it->second) {
            usernames.push_back(session->get_username());
        }
    }
    return usernames;
}

void SessionManager::broadcast_channel_members(const std::string& channel_id) {
    chatproj::Packet pkt;
    pkt.set_type(chatproj::Packet::JOIN_CHANNEL_RES);
    auto* res = pkt.mutable_join_channel_res();
    res->set_success(true);
    res->set_channel_id(channel_id);
    for (const auto& name : get_channel_usernames(channel_id)) {
        res->add_active_users(name);
    }
    broadcast_to_channel(pkt, channel_id);
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

void SessionManager::broadcast_to_channel(const chatproj::Packet& packet, const std::string& channel_id) {
    std::string serialized;
    packet.SerializeToString(&serialized);

    uint32_t length = htonl(static_cast<uint32_t>(serialized.size()));
    auto framed = std::make_shared<std::vector<uint8_t>>();
    framed->resize(4 + serialized.size());
    std::memcpy(framed->data(), &length, 4);
    std::memcpy(framed->data() + 4, serialized.data(), serialized.size());

    std::lock_guard<std::mutex> lock(mutex_);
    for (auto& session : channels_[channel_id]) {
        session->deliver(framed);
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
        int zero_skipped = 0;
        for (auto& watcher : st_it->second) {
            if (watcher->get_udp_media_endpoint().port() != 0) {
                targets.push_back(watcher->get_udp_media_endpoint());
            } else {
                zero_skipped++;
            }
        }
        static std::atomic<uint64_t> bcast_count{0};
        uint64_t bc = ++bcast_count;
        if (bc <= 5 || bc % 300 == 0) {
            std::cout << "[bcast] streamer=" << streamer_username
                      << " watchers_total=" << st_it->second.size()
                      << " targets=" << targets.size()
                      << " zero_skipped=" << zero_skipped << "\n";
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
                        static std::atomic<int> ping_log_count{0};
                        if (ping_log_count++ < 8) {
                            auto session = manager_.find_session_by_token(token_str, jwt_secret_);
                            std::cout << "[media-udp] PING from " << media_udp_sender_endpoint_
                                      << " token_len=" << token_str.size()
                                      << " session=" << (session ? session->get_username() : "<none>")
                                      << "\n";
                        }
                        if (!token_str.empty()) {
                            auto session = manager_.find_session_by_token(token_str, jwt_secret_);
                            if (session && session->get_udp_media_endpoint() != media_udp_sender_endpoint_) {
                                session->set_udp_media_endpoint(media_udp_sender_endpoint_);
                                std::cout << "[media-udp] registered media endpoint for "
                                          << session->get_username() << "\n";
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

                                    static std::atomic<uint64_t> video_recv_count{0};
                                    uint64_t c = ++video_recv_count;
                                    if (c <= 5 || c % 300 == 0) {
                                        std::cout << "[media-udp] VIDEO from " << uname
                                                  << " ch=" << channel << " cnt=" << c << "\n";
                                    }
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
    hb->set_member_count(static_cast<int>(manager.session_count()));

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

        if (!jwt_env) {
            std::cerr << "Missing required environment variable: DECIBELL_JWT_SECRET\n";
            return 1;
        }

        std::string jwt_secret = jwt_env;
        std::string central_host = central_host_env ? central_host_env : "127.0.0.1";
        std::string server_name = server_name_env ? server_name_env : "Community Server";
        std::string server_desc = server_desc_env ? server_desc_env : "";
        std::string public_ip = public_ip_env ? public_ip_env : "127.0.0.1";

        boost::asio::io_context io_context;
        SessionManager manager;
        CommunityServer s(io_context, 8082, manager, jwt_secret);
        std::cout << "Decibell Community Server running on port 8082...\n";

        // Start heartbeat timer
        boost::asio::steady_timer heartbeat_timer(io_context);
        send_heartbeat(io_context, heartbeat_timer, central_host, 8080,
                       server_name, server_desc, public_ip, 8082,
                       manager, jwt_secret);

        io_context.run();
    } catch (std::exception& e) {
        std::cerr << "Exception: " << e.what() << "\n";
    }
    return 0;
}