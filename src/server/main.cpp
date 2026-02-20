#ifndef WIN32_LEAN_AND_MEAN
#define WIN32_LEAN_AND_MEAN
#endif
#ifndef _WIN32_WINNT
#define _WIN32_WINNT 0x0A00
#endif

#include <iostream>
#include <string>
#include <memory>
#include <vector>
#include <boost/asio.hpp>
#include <boost/asio/ssl.hpp>
#include "messages.pb.h"
#include "../common/net_utils.hpp"
#include "storage.hpp"
#include "auth_utils.hpp" // Don't forget this!
#include "session_manager.hpp"

namespace ssl = boost::asio::ssl;

using boost::asio::ip::tcp;

// Global DB for now
chatproj::Storage db("chat_server.db");

class Session : public std::enable_shared_from_this<Session> {
public:
    // Update constructor to take ssl::context
    Session(tcp::socket socket, SessionManager& manager, ssl::context& context)
        : socket_(std::move(socket), context), manager_(manager) {}

    void start() {
        auto self(shared_from_this());
        // Perform TLS Handshake before reading headers
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
        auto self(shared_from_this());
        boost::asio::async_write(socket_, boost::asio::buffer(*framed_data),
            [this, self, framed_data](boost::system::error_code ec, std::size_t) {
                // 'framed_data' in the capture list keeps the vector alive until this executes
            });
    }

    std::string username() const { return username_; }

    SessionManager& manager_;

private:
    void do_read_header() {
        auto self(shared_from_this());
        boost::asio::async_read(socket_,
            boost::asio::buffer(inbound_header_, 4),
            [this, self](boost::system::error_code ec, std::size_t /*length*/) {
                if (!ec) {
                    // ... (existing logic to process body length) ...
                    uint32_t net_len = *reinterpret_cast<uint32_t*>(inbound_header_);
                    uint32_t body_length = ntohl(net_len);
                    if (body_length > 2 * 1024 * 1024) return;
                    inbound_body_.resize(body_length);
                    do_read_body(body_length);
                } 
                else {
                    // ERROR or DISCONNECT
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
                    // ERROR or DISCONNECT
                    std::cout << "[Session] Error in body read: " << username_ << "\n";
                    manager_.leave(shared_from_this()); // <--- ADD THIS
                }
            });
    }

    void process_packet() {
        chatproj::Packet packet;
        if (!packet.ParseFromArray(inbound_body_.data(), static_cast<int>(inbound_body_.size()))) {
            return;
        }

        std::cout << "[Server] Packet type: " << packet.type() << std::endl;

        // --- REGISTRATION ---
        if (packet.type() == chatproj::Packet::REGISTER_REQ) {
            const auto& req = packet.register_req();
            std::cout << "  -> Registering: " << req.username() << std::endl;
            
            std::string hashed_pw = chatproj::sha256(req.password());
            bool success = db.create_user(req.username(), hashed_pw, "salt");
            
            send_response(chatproj::Packet::REGISTER_RES, success, success ? "Registered!" : "Username taken.");
        }
        
        // --- LOGIN ---
        else if (packet.type() == chatproj::Packet::LOGIN_REQ) {
            const auto& req = packet.login_req();
            std::cout << "  -> Login Attempt: " << req.username() << std::endl;

            // FIX: Check if already online FIRST
            if (manager_.is_user_online(req.username())) {
                std::cout << "  -> FAILED: " << req.username() << " is already connected.\n";
                send_response(chatproj::Packet::LOGIN_RES, false, "User already logged in.");
                return;
            }

            std::string hashed_pw = chatproj::sha256(req.password());
            
            if (db.verify_user(req.username(), hashed_pw)) {
                authenticated_ = true;
                username_ = req.username();
                std::cout << "  -> SUCCESS: " << username_ << " is online.\n";
                send_response(chatproj::Packet::LOGIN_RES, true, "Login successful!");
                manager_.join_channel(shared_from_this(), "global", "");
                current_channel_ = "global";
                
                // Broadcast join to other users
                manager_.broadcast_system_message(username_ + " joined the chat.");
            } else {
                std::cout << "  -> FAILED: Bad password for " << req.username() << "\n";
                send_response(chatproj::Packet::LOGIN_RES, false, "Invalid username or password.");
            }
        }

        // --- CHAT MESSAGE ---
        else if (packet.type() == chatproj::Packet::CHAT_MSG) {
            if (!authenticated_) return; 

            auto now = std::chrono::system_clock::now();
            int64_t current_time = std::chrono::system_clock::to_time_t(now);

            chatproj::Packet routed_packet = packet;
            auto* chat_msg = routed_packet.mutable_chat_msg();
            chat_msg->set_sender(username_); 
            chat_msg->set_timestamp(current_time);
            
            if (chat_msg->recipient().empty()) {
                // Channel Broadcast
                chat_msg->set_channel(current_channel_);
                std::cout << "[#" << current_channel_ << "] " << username_ << ": " << chat_msg->content() << "\n";
                
                // Pass current_channel_ to storage
                db.save_message(username_, current_channel_, chat_msg->content(), current_time); 
                
                manager_.broadcast_to_channel(routed_packet, current_channel_);
            } else {
                // Private Message
                std::cout << "[Private] " << username_ << " -> " << chat_msg->recipient() << ": " << chat_msg->content() << "\n";
                bool delivered = manager_.send_private(routed_packet, chat_msg->recipient());
                
                if (!delivered) {
                    // Send error back to sender if user is offline
                    chatproj::Packet error_packet;
                    error_packet.set_type(chatproj::Packet::CHAT_MSG);
                    auto* err_msg = error_packet.mutable_chat_msg();
                    err_msg->set_sender("SYSTEM");
                    err_msg->set_content("User " + chat_msg->recipient() + " is offline or does not exist.");
                    err_msg->set_timestamp(current_time);
                    
                    std::string serialized;
                    error_packet.SerializeToString(&serialized);
                    auto framed = std::make_shared<std::vector<uint8_t>>(chatproj::create_framed_packet(serialized));
                    deliver(framed);
                } else {
                    // Echo the private message back to the sender so they see what they typed
                    std::string serialized;
                    routed_packet.SerializeToString(&serialized);
                    auto framed = std::make_shared<std::vector<uint8_t>>(chatproj::create_framed_packet(serialized));
                    deliver(framed);
                }
            }
        }

        // --- JOIN CHANNEL ---
        else if (packet.type() == chatproj::Packet::JOIN_CHANNEL) {
            if (!authenticated_) return;
            std::string target_channel = packet.join_channel().channel_name();
            
            manager_.join_channel(shared_from_this(), target_channel, current_channel_);
            current_channel_ = target_channel;
            
            std::cout << "[Channel] " << username_ << " joined #" << target_channel << "\n";
            
            // System confirmation to the user
            chatproj::Packet sys_packet;
            sys_packet.set_type(chatproj::Packet::CHAT_MSG);
            auto* sys_msg = sys_packet.mutable_chat_msg();
            sys_msg->set_sender("SYSTEM");
            sys_msg->set_content("You joined #" + target_channel);
            sys_msg->set_channel(target_channel);
            
            std::string serialized;
            sys_packet.SerializeToString(&serialized);
            auto framed = std::make_shared<std::vector<uint8_t>>(chatproj::create_framed_packet(serialized));
            deliver(framed);

            // Deliver history for the specific channel
            auto history = db.get_recent_messages(target_channel, 50);
            for (const auto& m : history) {
                chatproj::Packet hist_packet;
                hist_packet.set_type(chatproj::Packet::CHAT_MSG);
                auto* cm = hist_packet.mutable_chat_msg();
                cm->set_sender(m.sender);
                cm->set_content(m.content);
                cm->set_timestamp(m.timestamp);
                cm->set_channel(target_channel);
                
                std::string serialized;
                hist_packet.SerializeToString(&serialized);
                auto framed = std::make_shared<std::vector<uint8_t>>(chatproj::create_framed_packet(serialized));
                deliver(framed);
            }
        }
    }

    // Generic helper to send either Register or Login responses
    void send_response(chatproj::Packet::Type type, bool success, const std::string& msg) {
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
        }

        std::string serialized;
        resp_packet.SerializeToString(&serialized);
        
        auto framed = std::make_shared<std::vector<uint8_t>>(chatproj::create_framed_packet(serialized));
        
        auto self(shared_from_this());
        boost::asio::async_write(socket_, boost::asio::buffer(*framed),
            [this, self, framed](boost::system::error_code ec, std::size_t) {
                if (ec) std::cerr << "Write failed.\n";
            });
    }

    ssl::stream<tcp::socket> socket_;
    char inbound_header_[4];
    std::vector<uint8_t> inbound_body_;
    
    // Session State
    bool authenticated_ = false;
    std::string username_;

    std::string current_channel_ = "global";
};

void SessionManager::broadcast(const chatproj::Packet& packet) {
    std::string serialized;
    packet.SerializeToString(&serialized);
    
    // Allocate on the heap via shared_ptr
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
        
        // Remove from any channels
        for (auto& pair : channels_) {
            pair.second.erase(session);
        }
    }
    
    if (removed) {
        std::cout << "[Manager] Session left. Total: " << total << "\n";
        if (!user.empty()) {
            broadcast_system_message(user + " left the chat.");
        }
    }
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
            return true; // Found and delivered
        }
    }
    return false; // Target user not online
}

void SessionManager::join_channel(std::shared_ptr<Session> session, const std::string& new_channel, const std::string& old_channel) {
    std::lock_guard<std::mutex> lock(mutex_);
    if (!old_channel.empty()) {
        channels_[old_channel].erase(session);
    }
    channels_[new_channel].insert(session);
}

void SessionManager::broadcast_to_channel(const chatproj::Packet& packet, const std::string& channel_name) {
    std::string serialized;
    packet.SerializeToString(&serialized);
    auto framed = std::make_shared<std::vector<uint8_t>>(chatproj::create_framed_packet(serialized));

    std::lock_guard<std::mutex> lock(mutex_);
    for (auto& session : channels_[channel_name]) {
        session->deliver(framed);
    }
}

class Server {
public:
    Server(boost::asio::io_context& io_context, short port)
        : acceptor_(io_context, tcp::endpoint(tcp::v4(), port)),
          ssl_context_(ssl::context::tlsv12) { // Require TLS v1.2 minimum

        // Configure strict SSL options
        ssl_context_.set_options(
            ssl::context::default_workarounds |
            ssl::context::no_sslv2 |
            ssl::context::no_sslv3 |
            ssl::context::no_tlsv1 |
            ssl::context::no_tlsv1_1);

        // Load the certificate and key (using absolute paths to avoid execution directory issues)
        ssl_context_.use_certificate_chain_file("C:/dev/chatproj-core/server.crt");
        ssl_context_.use_private_key_file("C:/dev/chatproj-core/server.key", ssl::context::pem);

        do_accept();
    }
private:
    void do_accept() {
        acceptor_.async_accept(
            [this](boost::system::error_code ec, tcp::socket socket) {
                if (!ec) {
                    // Pass ssl_context_ to the new Session
                    auto session = std::make_shared<Session>(std::move(socket), manager_, ssl_context_);
                    manager_.join(session);
                    session->start();
                }
                do_accept();
            });
    }
    tcp::acceptor acceptor_;
    ssl::context ssl_context_;
    SessionManager manager_;
};

int main() {
    try {
        boost::asio::io_context io_context;
        Server s(io_context, 8080);
        std::cout << "Server running on 8080...\n";
        io_context.run();
    } catch (std::exception& e) {
        std::cerr << "Exception: " << e.what() << "\n";
    }
    return 0;
}