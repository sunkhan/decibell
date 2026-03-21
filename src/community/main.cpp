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
#include <boost/asio.hpp>
#include <boost/asio/ssl.hpp>
#include <jwt-cpp/traits/nlohmann-json/defaults.h>
#include "messages.pb.h"
#include "../common/udp_packet.hpp"

namespace ssl = boost::asio::ssl;
using boost::asio::ip::tcp;

#include <deque>

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

private:
    std::set<std::shared_ptr<Session>> sessions_;
    std::unordered_map<std::string, std::set<std::shared_ptr<Session>>> channels_;
    std::unordered_map<std::string, std::set<std::shared_ptr<Session>>> voice_channels_;
    
    // channel_id -> map of username -> stream info
    struct StreamInfo { bool has_audio; };
    std::unordered_map<std::string, std::unordered_map<std::string, StreamInfo>> active_streams_;

    std::mutex mutex_;
};

class Session : public std::enable_shared_from_this<Session> {
public:
    Session(tcp::socket socket, SessionManager& manager, ssl::context& context, const std::string& jwt_secret)
        : socket_(std::move(socket), context), manager_(manager), jwt_secret_(jwt_secret) {
        // Enable TCP keepalive to detect dead client connections
        socket_.lowest_layer().set_option(boost::asio::socket_base::keep_alive(true));
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
    void set_udp_endpoint(const boost::asio::ip::udp::endpoint& ep) { udp_endpoint_ = ep; }
    boost::asio::ip::udp::endpoint get_udp_endpoint() const { return udp_endpoint_; }
    std::string get_current_voice_channel() const { return current_voice_channel_; }

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
    std::string current_channel_;
    std::string current_voice_channel_;
    boost::asio::ip::udp::endpoint udp_endpoint_;
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
        active_streams_[channel_id][session->get_username()] = { has_audio };
    }
    broadcast_stream_presence(channel_id);
}

void SessionManager::stop_stream(std::shared_ptr<Session> session, const std::string& channel_id) {
    bool removed = false;
    {
        std::lock_guard<std::mutex> lock(mutex_);
        if (active_streams_[channel_id].erase(session->get_username()) > 0) {
            removed = true;
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
        }
        voice_channels_[new_channel].insert(session);
    }
    if (!old_channel.empty()) {
        broadcast_voice_presence(old_channel);
    }
    broadcast_voice_presence(new_channel);
}

void SessionManager::leave_voice_channel(std::shared_ptr<Session> session, const std::string& current_channel) {
    {
        std::lock_guard<std::mutex> lock(mutex_);
        if (!current_channel.empty()) {
            voice_channels_[current_channel].erase(session);
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

void SessionManager::broadcast_to_voice_channel(const char* data, size_t length, const std::string& channel_id, std::shared_ptr<Session> sender, boost::asio::ip::udp::socket& udp_socket) {
    // Copy the data into a shared buffer so it remains valid for the async sends,
    // since the caller's udp_buffer_ will be overwritten by the next received packet.
    auto buffer = std::make_shared<std::vector<char>>(data, data + length);

    std::lock_guard<std::mutex> lock(mutex_);
    for (auto& session : voice_channels_[channel_id]) {
        if (session != sender && session->get_udp_endpoint().port() != 0) {
            udp_socket.async_send_to(
                boost::asio::buffer(*buffer), session->get_udp_endpoint(),
                [buffer](boost::system::error_code /*ec*/, std::size_t /*bytes_sent*/) {
                    // buffer captured to extend its lifetime until send completes
                });
        }
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
        if (session->get_username() == target_username && session->get_udp_endpoint().port() != 0) {
            auto buffer = std::make_shared<std::vector<char>>(reinterpret_cast<const char*>(&req),
                                                               reinterpret_cast<const char*>(&req) + sizeof(req));
            udp_socket.async_send_to(
                boost::asio::buffer(*buffer), session->get_udp_endpoint(),
                [buffer](boost::system::error_code, std::size_t) {});
            return;
        }
    }
}

void SessionManager::relay_nack(const char* data, size_t length, const std::string& target_username, boost::asio::ip::udp::socket& udp_socket) {
    std::lock_guard<std::mutex> lock(mutex_);
    for (auto& session : sessions_) {
        if (session->get_username() == target_username && session->get_udp_endpoint().port() != 0) {
            auto buffer = std::make_shared<std::vector<char>>(data, data + length);
            udp_socket.async_send_to(
                boost::asio::buffer(*buffer), session->get_udp_endpoint(),
                [buffer](boost::system::error_code, std::size_t) {});
            return;
        }
    }
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

std::shared_ptr<Session> SessionManager::find_session_by_token(const std::string& udp_id, const std::string& jwt_secret) {
    std::lock_guard<std::mutex> lock(mutex_);
    for (auto& session : sessions_) {
        // Clients send the last SENDER_ID_SIZE-1 chars of their JWT as a compact
        // UDP identifier.  Match by checking if the session's full token ends with it.
        const std::string& full_token = session->get_token();
        if (!udp_id.empty() && full_token.size() >= udp_id.size() &&
            full_token.compare(full_token.size() - udp_id.size(), udp_id.size(), udp_id) == 0) {
            return session;
        }
    }
    return nullptr;
}

class CommunityServer {
public:
    CommunityServer(boost::asio::io_context& io_context, short port, SessionManager& manager, const std::string& jwt_secret)
        : acceptor_(io_context, tcp::endpoint(tcp::v4(), port)),
          udp_socket_(io_context, boost::asio::ip::udp::endpoint(boost::asio::ip::udp::v4(), port + 1)),
          // UDP buffer sizes are set after construction below
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

        // Increase UDP socket buffers to handle video traffic bursts
        udp_socket_.set_option(boost::asio::socket_base::receive_buffer_size(2 * 1024 * 1024));
        udp_socket_.set_option(boost::asio::socket_base::send_buffer_size(2 * 1024 * 1024));

        std::cout << "Community Server TCP running on port " << port << "...\n";
        std::cout << "Community Server UDP running on port " << port + 1 << "...\n";

        do_accept();
        do_receive_udp();
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

    void do_receive_udp() {
        udp_socket_.async_receive_from(
            boost::asio::buffer(udp_buffer_, sizeof(udp_buffer_)), udp_sender_endpoint_,
            [this](boost::system::error_code ec, std::size_t bytes_recvd) {
                if (!ec && bytes_recvd >= 1) {
                    uint8_t packet_type = static_cast<uint8_t>(udp_buffer_[0]);
                    std::string token_str;

                    constexpr int SID = chatproj::SENDER_ID_SIZE;
                    // Minimum packet sizes: 1 (type) + SENDER_ID_SIZE + fields
                    if (packet_type == chatproj::UdpPacketType::AUDIO && bytes_recvd >= 1 + SID + 4) {
                        chatproj::UdpAudioPacket* packet = reinterpret_cast<chatproj::UdpAudioPacket*>(udp_buffer_);
                        for (int i = 0; i < SID; ++i) {
                            if (packet->sender_id[i] == '\0') break;
                            token_str.push_back(packet->sender_id[i]);
                        }
                    } else if ((packet_type == chatproj::UdpPacketType::VIDEO || packet_type == chatproj::UdpPacketType::FEC) && bytes_recvd >= 1 + SID + 8) {
                        // VIDEO and FEC packets both have sender_id at offset 1
                        chatproj::UdpVideoPacket* packet = reinterpret_cast<chatproj::UdpVideoPacket*>(udp_buffer_);
                        for (int i = 0; i < SID; ++i) {
                            if (packet->sender_id[i] == '\0') break;
                            token_str.push_back(packet->sender_id[i]);
                        }
                    } else if (packet_type == chatproj::UdpPacketType::KEYFRAME_REQUEST && bytes_recvd >= sizeof(chatproj::UdpKeyframeRequest)) {
                        chatproj::UdpKeyframeRequest* packet = reinterpret_cast<chatproj::UdpKeyframeRequest*>(udp_buffer_);
                        // Extract the target streamer username and relay PLI to them
                        std::string target;
                        for (int i = 0; i < SID; ++i) {
                            if (packet->target_username[i] == '\0') break;
                            target.push_back(packet->target_username[i]);
                        }
                        if (!target.empty()) {
                            manager_.relay_keyframe_request(target, udp_socket_);
                        }
                        // Don't process further — this is not audio/video data
                        do_receive_udp();
                        return;
                    } else if (packet_type == chatproj::UdpPacketType::NACK &&
                               bytes_recvd >= sizeof(chatproj::UdpNackPacket) - sizeof(uint16_t) * chatproj::NACK_MAX_ENTRIES) {
                        chatproj::UdpNackPacket* packet = reinterpret_cast<chatproj::UdpNackPacket*>(udp_buffer_);
                        std::string target;
                        for (int i = 0; i < SID; ++i) {
                            if (packet->target_username[i] == '\0') break;
                            target.push_back(packet->target_username[i]);
                        }
                        if (!target.empty()) {
                            manager_.relay_nack(udp_buffer_, bytes_recvd, target, udp_socket_);
                        }
                        do_receive_udp();
                        return;
                    } else if (packet_type == 5) { // PING
                        // Echo the packet back to the sender
                        auto echo_buf = std::make_shared<std::vector<uint8_t>>(
                            udp_buffer_, udp_buffer_ + bytes_recvd);
                        udp_socket_.async_send_to(
                            boost::asio::buffer(*echo_buf), udp_sender_endpoint_,
                            [echo_buf](boost::system::error_code, std::size_t) {});
                        do_receive_udp();
                        return;
                    }

                    if (!token_str.empty()) {
                        auto session = manager_.find_session_by_token(token_str, jwt_secret_);

                        if (session) {
                            // Update endpoint if it changed
                            if (session->get_udp_endpoint() != udp_sender_endpoint_) {
                                session->set_udp_endpoint(udp_sender_endpoint_);
                            }

                            std::string channel = session->get_current_voice_channel();
                            if (!channel.empty()) {
                                // Overwrite sender_id with username before broadcasting for security and identification
                                std::string uname = session->get_username();

                                // Rewrite sender_id with authenticated username for all broadcast packet types
                                // sender_id is at offset 1 in AUDIO, VIDEO, and FEC packets
                                if (packet_type == chatproj::UdpPacketType::AUDIO ||
                                    packet_type == chatproj::UdpPacketType::VIDEO ||
                                    packet_type == chatproj::UdpPacketType::FEC) {
                                    // sender_id is at bytes [1..32] for all three packet types
                                    std::memset(udp_buffer_ + 1, 0, chatproj::SENDER_ID_SIZE);
                                    std::memcpy(udp_buffer_ + 1, uname.c_str(), std::min(uname.size(), size_t(chatproj::SENDER_ID_SIZE - 1)));
                                }

                                manager_.broadcast_to_voice_channel(udp_buffer_, bytes_recvd, channel, session, udp_socket_);
                            }
                        }
                    }
                }
                do_receive_udp();
            });
    }
    tcp::acceptor acceptor_;
    boost::asio::ip::udp::socket udp_socket_;
    char udp_buffer_[sizeof(chatproj::UdpVideoPacket) > sizeof(chatproj::UdpFecPacket) ? sizeof(chatproj::UdpVideoPacket) : sizeof(chatproj::UdpFecPacket)];
    boost::asio::ip::udp::endpoint udp_sender_endpoint_;
    ssl::context ssl_context_;
    SessionManager& manager_;
    std::string jwt_secret_;
};

int main() {
    try {
        std::string jwt_secret = "super_secret_decibell_key_change_in_production";
        boost::asio::io_context io_context;
        SessionManager manager; 
        CommunityServer s(io_context, 8082, manager, jwt_secret);
        io_context.run();
    } catch (std::exception& e) {
        std::cerr << "Exception: " << e.what() << "\n";
    }
    return 0;
}