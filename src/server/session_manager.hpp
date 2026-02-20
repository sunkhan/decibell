#pragma once
#include <memory>
#include <set>
#include <mutex>
#include <iostream>
#include <chrono>
#include <unordered_map>
#include "messages.pb.h"

// Forward declaration (we promise this class exists)
class Session;

class SessionManager {
public:
    // Add a new session to the "room"
    void join(std::shared_ptr<Session> session) {
        std::lock_guard<std::mutex> lock(mutex_);
        sessions_.insert(session);
        std::cout << "[Manager] Session joined. Total: " << sessions_.size() << "\n";
    }

    void broadcast_system_message(const std::string& text) {
        auto now = std::chrono::system_clock::now();
        int64_t current_time = std::chrono::system_clock::to_time_t(now);

        chatproj::Packet packet;
        packet.set_type(chatproj::Packet::CHAT_MSG);
        auto* chat_msg = packet.mutable_chat_msg();
        chat_msg->set_sender("SYSTEM");
        chat_msg->set_content(text);
        chat_msg->set_timestamp(current_time);
        broadcast(packet);
    }

    // Declaration only. The definition moves to main.cpp.
    void leave(std::shared_ptr<Session> session);

    // Broadcast a message to EVERYONE (The core chat function)
    // We will implement the actual "deliver" method in Session later
    void broadcast(const chatproj::Packet& packet);

    // Declaration for private message routing
    bool send_private(const chatproj::Packet& packet, const std::string& target_username);

    // Check if a specific username is already connected
    bool is_user_online(const std::string& username);

    void join_channel(std::shared_ptr<Session> session, const std::string& new_channel, const std::string& old_channel);

    void broadcast_to_channel(const chatproj::Packet& packet, const std::string& channel_name);

private:
    std::set<std::shared_ptr<Session>> sessions_; // Tracks all active connections
    std::unordered_map<std::string, std::set<std::shared_ptr<Session>>> channels_; // Tracks connections per channel
    std::mutex mutex_;  // Thread safety
};