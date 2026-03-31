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
#include <deque>
#include <boost/asio.hpp>
#include <boost/asio/ssl.hpp>
#include "messages.pb.h"
#include "../common/net_utils.hpp"
#include "auth_utils.hpp"
#include "session_manager.hpp"
#include "auth_manager.hpp"

namespace ssl = boost::asio::ssl;
using boost::asio::ip::tcp;

class Session : public std::enable_shared_from_this<Session> {
public:
    Session(tcp::socket socket, SessionManager& manager, ssl::context& context, AuthManager& auth_manager)
        : socket_(std::move(socket), context), manager_(manager), auth_manager_(auth_manager) {}

    void start() {
        auto self(shared_from_this());
        socket_.async_handshake(ssl::stream_base::server,
            [this, self](const boost::system::error_code& error) {
                if (!error) {
                    do_read_header();
                } else {
                    std::cerr << "[Session] TLS Handshake failed: " << error.message() << "\n";
                    manager_.leave(shared_from_this());
                }
            });
    }

    void deliver(std::shared_ptr<std::vector<uint8_t>> framed_data) {
        bool write_in_progress = !write_queue_.empty();
        write_queue_.push_back(framed_data);
        if (!write_in_progress) {
            do_write();
        }
    }

    std::string username() const { return username_; }
    bool dm_friends_only() const { return dm_friends_only_; }

    SessionManager& manager_;

private:
    void do_write() {
        auto self(shared_from_this());
        boost::asio::async_write(socket_, boost::asio::buffer(*write_queue_.front()),
            [this, self](boost::system::error_code ec, std::size_t) {
                if (ec) {
                    manager_.leave(shared_from_this());
                    return;
                }
                write_queue_.pop_front();
                if (!write_queue_.empty()) {
                    do_write();
                }
            });
    }

    void do_read_header() {
        auto self(shared_from_this());
        boost::asio::async_read(socket_,
            boost::asio::buffer(inbound_header_, 4),
            [this, self](boost::system::error_code ec, std::size_t /*length*/) {
                if (!ec) {
                    uint32_t net_len = *reinterpret_cast<uint32_t*>(inbound_header_);
                    uint32_t body_length = ntohl(net_len);
                    if (body_length > 2 * 1024 * 1024) return;
                    inbound_body_.resize(body_length);
                    do_read_body(body_length);
                } 
                else {
                    std::cout << "[Session] Client disconnected: " << username_ << "\n";
                    manager_.leave(shared_from_this());
                }
            });
    }

    void do_read_body(uint32_t length) {
        auto self(shared_from_this());
        boost::asio::async_read(socket_,
            boost::asio::buffer(inbound_body_.data(), length),
            [this, self](boost::system::error_code ec, std::size_t /*length*/) {
                if (!ec) {
                    process_packet();
                    do_read_header(); 
                }
                else {
                    std::cout << "[Session] Error in body read: " << username_ << "\n";
                    manager_.leave(shared_from_this());
                }
            });
    }

    void process_packet() {
        chatproj::Packet packet;
        if (!packet.ParseFromArray(inbound_body_.data(), static_cast<int>(inbound_body_.size()))) {
            return;
        }

        // Log the raw integer type of every incoming packet
        std::cout << "[Server] Raw packet received, type ID: " << packet.type() << "\n";

        // --- ENFORCE JWT VALIDATION ---
        if (packet.type() != chatproj::Packet::REGISTER_REQ && 
            packet.type() != chatproj::Packet::LOGIN_REQ && 
            packet.type() != chatproj::Packet::HANDSHAKE) {
            
            if (!auth_manager_.validateToken(packet.auth_token())) {
                std::cout << "[Security] Dropped packet - Missing or invalid JWT.\n";
                manager_.leave(shared_from_this());
                return; 
            }
        }

        // --- REGISTRATION ---
        if (packet.type() == chatproj::Packet::REGISTER_REQ) {
            const auto& req = packet.register_req();
            std::string error_msg = auth_manager_.registerUser(req.username(), req.email(), req.password());
            bool success = error_msg.empty();
            send_response(chatproj::Packet::REGISTER_RES, success, success ? "Registration successful." : error_msg);
        }
        
        // --- LOGIN ---
        else if (packet.type() == chatproj::Packet::LOGIN_REQ) {
            const auto& req = packet.login_req();

            if (manager_.is_user_online(req.username())) {
                send_response(chatproj::Packet::LOGIN_RES, false, "User already logged in.");
                return;
            }

            auto token_opt = auth_manager_.authenticateUser(req.username(), req.password());
            
            if (token_opt.has_value()) {
                authenticated_ = true;
                username_ = req.username();
                send_response(chatproj::Packet::LOGIN_RES, true, "Login successful!", token_opt.value());
                manager_.broadcast_presence();
            } else {
                send_response(chatproj::Packet::LOGIN_RES, false, "Invalid username or password.");
            }
        }

        // --- DIRECT MESSAGE ---
        else if (packet.type() == chatproj::Packet::DIRECT_MSG) {
            if (!authenticated_) return; 

            auto now = std::chrono::system_clock::now();
            int64_t current_time = std::chrono::system_clock::to_time_t(now);

            chatproj::Packet routed_packet = packet;
            auto* dmsg = routed_packet.mutable_direct_msg();
            dmsg->set_sender(username_); // Enforce sender identity
            dmsg->set_timestamp(current_time);

            if (!manager_.check_dm_allowed(username_, dmsg->recipient(), auth_manager_)) {
                chatproj::Packet error_packet;
                error_packet.set_type(chatproj::Packet::DIRECT_MSG);
                auto* err_msg = error_packet.mutable_direct_msg();
                err_msg->set_sender(username_);
                err_msg->set_recipient(dmsg->recipient());
                err_msg->set_content("This user only accepts direct messages from users in their friends list.");
                err_msg->set_timestamp(current_time);

                std::string serialized;
                error_packet.SerializeToString(&serialized);
                auto framed = std::make_shared<std::vector<uint8_t>>(chatproj::create_framed_packet(serialized));
                deliver(framed);
                return;
            }

            bool delivered = manager_.send_private(routed_packet, dmsg->recipient());
            
            if (!delivered) {
                // Offline queuing to PostgreSQL will go here
                chatproj::Packet error_packet;
                error_packet.set_type(chatproj::Packet::DIRECT_MSG);
                auto* err_msg = error_packet.mutable_direct_msg();
                err_msg->set_sender(username_);
                err_msg->set_recipient(dmsg->recipient());
                err_msg->set_content("This user is currently offline. Your message could not be delivered.");
                err_msg->set_timestamp(current_time);
                
                std::string serialized;
                error_packet.SerializeToString(&serialized);
                auto framed = std::make_shared<std::vector<uint8_t>>(chatproj::create_framed_packet(serialized));
                deliver(framed);
            } else {
                // Echo back to sender
                std::string serialized;
                routed_packet.SerializeToString(&serialized);
                auto framed = std::make_shared<std::vector<uint8_t>>(chatproj::create_framed_packet(serialized));
                deliver(framed);
            }
        }

        // --- SERVER LIST DIRECTORY ---
        else if (packet.type() == chatproj::Packet::SERVER_LIST_REQ) {
            if (!authenticated_) {
                std::cout << "[Server] Dropped SERVER_LIST_REQ: User not authenticated.\n";
                return;
            }

            std::cout << "[Server] Received SERVER_LIST_REQ from " << username_ << "\n";
            auto servers = auth_manager_.getCommunityServers();
            std::cout << "[Server] Found " << servers.size() << " community servers in DB.\n";

            chatproj::Packet res_packet;
            res_packet.set_type(chatproj::Packet::SERVER_LIST_RES);
            auto* res = res_packet.mutable_server_list_res();

            for (const auto& srv : servers) {
                *res->add_servers() = srv;
            }

            std::string serialized;
            res_packet.SerializeToString(&serialized);
            auto framed = std::make_shared<std::vector<uint8_t>>(chatproj::create_framed_packet(serialized));
            deliver(framed);
        }

        // --- FRIEND SYSTEM ---
        else if (packet.type() == chatproj::Packet::FRIEND_ACTION_REQ) {
            if (!authenticated_) return;
            const auto& req = packet.friend_action_req();
            
            std::string error_msg = auth_manager_.handleFriendAction(username_, req.action(), req.target_username());
            bool success = error_msg.empty();
            
            chatproj::Packet res_packet;
            res_packet.set_type(chatproj::Packet::FRIEND_ACTION_RES);
            auto* res = res_packet.mutable_friend_action_res();
            res->set_success(success);
            res->set_message(success ? "Action successful" : error_msg);
            
            std::string serialized;
            res_packet.SerializeToString(&serialized);
            auto framed = std::make_shared<std::vector<uint8_t>>(chatproj::create_framed_packet(serialized));
            deliver(framed);

            // Push updated friend list to both users if the action was successful
            if (success) {
                // Send updated list to the requester
                auto my_friends = auth_manager_.getFriends(username_);
                chatproj::Packet my_list_pkt;
                my_list_pkt.set_type(chatproj::Packet::FRIEND_LIST_RES);
                auto* my_list = my_list_pkt.mutable_friend_list_res();
                for (auto& f : my_friends) {
                    if (f.status() == chatproj::FriendInfo::OFFLINE && manager_.is_user_online(f.username())) {
                        f.set_status(chatproj::FriendInfo::ONLINE);
                    }
                    *my_list->add_friends() = f;
                }
                std::string my_ser;
                my_list_pkt.SerializeToString(&my_ser);
                auto my_framed = std::make_shared<std::vector<uint8_t>>(chatproj::create_framed_packet(my_ser));
                deliver(my_framed);

                // Send updated list to the target user (if online)
                auto target_friends = auth_manager_.getFriends(req.target_username());
                chatproj::Packet target_list_pkt;
                target_list_pkt.set_type(chatproj::Packet::FRIEND_LIST_RES);
                auto* target_list = target_list_pkt.mutable_friend_list_res();
                for (auto& f : target_friends) {
                    if (f.status() == chatproj::FriendInfo::OFFLINE && manager_.is_user_online(f.username())) {
                        f.set_status(chatproj::FriendInfo::ONLINE);
                    }
                    *target_list->add_friends() = f;
                }
                manager_.send_private(target_list_pkt, req.target_username());
            }
        }
        else if (packet.type() == chatproj::Packet::FRIEND_LIST_REQ) {
            if (!authenticated_) return;

            auto friends = auth_manager_.getFriends(username_);
            
            chatproj::Packet res_packet;
            res_packet.set_type(chatproj::Packet::FRIEND_LIST_RES);
            auto* res = res_packet.mutable_friend_list_res();

            for (auto& f : friends) {
                // Determine online presence if they are ACCEPTED
                if (f.status() == chatproj::FriendInfo::OFFLINE) {
                    if (manager_.is_user_online(f.username())) {
                        f.set_status(chatproj::FriendInfo::ONLINE);
                    }
                }
                *res->add_friends() = f;
            }

            std::string serialized;
            res_packet.SerializeToString(&serialized);
            auto framed = std::make_shared<std::vector<uint8_t>>(chatproj::create_framed_packet(serialized));
            deliver(framed);
        }

        // --- DM PRIVACY SETTING ---
        else if (packet.type() == chatproj::Packet::DM_PRIVACY) {
            if (!authenticated_) return;
            dm_friends_only_ = packet.dm_privacy().friends_only();
            std::cout << "[Server] User " << username_ << " set dm_friends_only to " << dm_friends_only_ << "\n";
        }
    }

    void send_response(chatproj::Packet::Type type, bool success, const std::string& msg, const std::string& token = "") {
        chatproj::Packet resp_packet;
        resp_packet.set_type(type);
        
        if (type == chatproj::Packet::REGISTER_RES) {
            auto* resp = resp_packet.mutable_register_res();
            resp->set_success(success);
            resp->set_message(msg);
        } else if (type == chatproj::Packet::LOGIN_RES) {
            auto* resp = resp_packet.mutable_login_res();
            resp->set_success(success);
            resp->set_message(msg);
            if (!token.empty()) {
                resp->set_jwt_token(token);
            }
        }

        std::string serialized;
        resp_packet.SerializeToString(&serialized);

        auto framed = std::make_shared<std::vector<uint8_t>>(chatproj::create_framed_packet(serialized));
        deliver(framed);
    }

    ssl::stream<tcp::socket> socket_;
    char inbound_header_[4];
    std::vector<uint8_t> inbound_body_;
    
    bool authenticated_ = false;
    std::string username_;
    bool dm_friends_only_ = false;
    AuthManager& auth_manager_;
    std::deque<std::shared_ptr<std::vector<uint8_t>>> write_queue_;
};


// --- SessionManager Implementations ---

void SessionManager::broadcast(const chatproj::Packet& packet) {
    std::string serialized;
    packet.SerializeToString(&serialized);
    auto framed = std::make_shared<std::vector<uint8_t>>(chatproj::create_framed_packet(serialized));

    std::lock_guard<std::mutex> lock(mutex_);
    for (auto& session : sessions_) {
        session->deliver(framed);
    }
}

void SessionManager::leave(std::shared_ptr<Session> session) {
    bool removed = false;
    size_t total = 0;
    std::string user = session->username();
    
    {
        std::lock_guard<std::mutex> lock(mutex_);
        removed = sessions_.erase(session) > 0;
        total = sessions_.size();
    }
    
    if (removed) {
        std::cout << "[Manager] Session left. Total: " << total << "\n";
        if (!user.empty()) {
            broadcast_presence();  
        }
    }
}

void SessionManager::broadcast_presence() {
    chatproj::Packet packet;
    packet.set_type(chatproj::Packet::PRESENCE_UPDATE);
    auto* presence = packet.mutable_presence_update();
    
    std::vector<std::string> active_users;
    {
        std::lock_guard<std::mutex> lock(mutex_);
        for (const auto& session : sessions_) {
            std::string uname = session->username();
            if (!uname.empty()) {
                active_users.push_back(uname);
            }
        }
    }
    
    for (const auto& u : active_users) {
        presence->add_online_users(u);
    }
    
    broadcast(packet);
}

bool SessionManager::is_user_online(const std::string& username) {
    std::lock_guard<std::mutex> lock(mutex_);
    for (const auto& session : sessions_) {
        if (session->username() == username) {
            return true;
        }
    }
    return false;
}

bool SessionManager::send_private(const chatproj::Packet& packet, const std::string& target_username) {
    std::string serialized;
    packet.SerializeToString(&serialized);
    auto framed = std::make_shared<std::vector<uint8_t>>(chatproj::create_framed_packet(serialized));

    std::lock_guard<std::mutex> lock(mutex_);
    for (auto& session : sessions_) {
        if (session->username() == target_username) {
            session->deliver(framed);
            return true;
        }
    }
    return false;
}

bool SessionManager::check_dm_allowed(const std::string& sender, const std::string& recipient, AuthManager& auth_manager) {
    std::lock_guard<std::mutex> lock(mutex_);

    // Find recipient's session
    std::shared_ptr<Session> recipient_session;
    for (const auto& session : sessions_) {
        if (session->username() == recipient) {
            recipient_session = session;
            break;
        }
    }

    // If recipient is offline, allow (offline queuing will handle later)
    if (!recipient_session) {
        return true;
    }

    // If recipient allows DMs from everyone, allow
    if (!recipient_session->dm_friends_only()) {
        return true;
    }

    // Recipient only allows DMs from friends — check friend list
    auto friends = auth_manager.getFriends(recipient);
    for (const auto& f : friends) {
        if (f.username() == sender &&
            (f.status() == chatproj::FriendInfo::ONLINE || f.status() == chatproj::FriendInfo::OFFLINE)) {
            return true;
        }
    }

    return false;
}


// --- Server & Entry Point ---

class Server {
public:
    Server(boost::asio::io_context& io_context, short port, SessionManager& manager, AuthManager& auth_manager)
        : acceptor_(io_context, tcp::endpoint(tcp::v4(), port)),
          ssl_context_(ssl::context::tlsv12),
          manager_(manager),
          auth_manager_(auth_manager) { 

        ssl_context_.set_options(
            ssl::context::default_workarounds |
            ssl::context::no_sslv2 |
            ssl::context::no_sslv3 |
            ssl::context::no_tlsv1 |
            ssl::context::no_tlsv1_1);

        ssl_context_.use_certificate_chain_file("server.crt");
        ssl_context_.use_private_key_file("server.key", ssl::context::pem);

        do_accept();
    }
private:
    void do_accept() {
        acceptor_.async_accept(
            [this](boost::system::error_code ec, tcp::socket socket) {
                if (!ec) {
                    auto session = std::make_shared<Session>(std::move(socket), manager_, ssl_context_, auth_manager_);
                    manager_.join(session);
                    session->start();
                }
                do_accept();
            });
    }
    tcp::acceptor acceptor_;
    ssl::context ssl_context_;
    SessionManager& manager_; 
    AuthManager& auth_manager_;
};

int main() {
    try {
        const char* jwt_env = std::getenv("DECIBELL_JWT_SECRET");
        const char* db_env = std::getenv("DECIBELL_DB_CONN");

        if (!jwt_env || !db_env) {
            std::cerr << "Missing required environment variables:\n";
            if (!jwt_env) std::cerr << "  DECIBELL_JWT_SECRET\n";
            if (!db_env) std::cerr << "  DECIBELL_DB_CONN\n";
            return 1;
        }

        std::string jwt_secret = jwt_env;
        std::string db_conn = db_env;
        AuthManager auth_manager(jwt_secret, db_conn);

        boost::asio::io_context io_context;
        
        SessionManager manager; 
        Server s(io_context, 8080, manager, auth_manager);
        
        std::cout << "Decibell Central Server running on port 8080...\n";
        
        io_context.run();
    } catch (std::exception& e) {
        std::cerr << "Exception: " << e.what() << "\n";
    }
    return 0;
}