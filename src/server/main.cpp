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
#include <utility>
#include <functional>
#ifdef __linux__
#include <netinet/tcp.h>
#endif
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
        : socket_(std::move(socket), context), manager_(manager), auth_manager_(auth_manager),
          last_activity_(std::chrono::steady_clock::now()) {
        // Tighten TCP keepalive: 15s idle, 5s interval, 3 retries (~30s detection)
        socket_.lowest_layer().set_option(boost::asio::socket_base::keep_alive(true));
#ifdef __linux__
        int fd = socket_.lowest_layer().native_handle();
        int idle = 15, interval = 5, count = 3;
        setsockopt(fd, IPPROTO_TCP, TCP_KEEPIDLE, &idle, sizeof(idle));
        setsockopt(fd, IPPROTO_TCP, TCP_KEEPINTVL, &interval, sizeof(interval));
        setsockopt(fd, IPPROTO_TCP, TCP_KEEPCNT, &count, sizeof(count));
#endif
    }

    std::chrono::steady_clock::time_point last_activity() const { return last_activity_; }
    void touch() { last_activity_ = std::chrono::steady_clock::now(); }

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
    /// The user's current avatar_version, loaded at login and
    /// refreshed inline on UPDATE_AVATAR_REQ. broadcast_presence
    /// reads this so each UserPresence entry carries the version
    /// without an extra DB query per broadcast.
    const std::string& avatar_version() const { return avatar_version_; }
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
        touch(); // Update activity timestamp for stale session detection

        chatproj::Packet packet;
        if (!packet.ParseFromArray(inbound_body_.data(), static_cast<int>(inbound_body_.size()))) {
            return;
        }

        // Client keepalive — no processing needed, touch() already updated timestamp
        if (packet.type() == chatproj::Packet::CLIENT_PING) {
            return;
        }

        // Log the raw integer type of every incoming packet
        std::cout << "[Server] Raw packet received, type ID: " << packet.type() << "\n";

        // --- ENFORCE JWT VALIDATION ---
        // Heartbeats and invite register/unregister from community servers
        // authenticate with the pre-shared secret, not a JWT — they are
        // verified below in their own handlers.
        if (packet.type() != chatproj::Packet::REGISTER_REQ &&
            packet.type() != chatproj::Packet::LOGIN_REQ &&
            packet.type() != chatproj::Packet::HANDSHAKE &&
            packet.type() != chatproj::Packet::SERVER_HEARTBEAT &&
            packet.type() != chatproj::Packet::CLIENT_PING &&
            packet.type() != chatproj::Packet::INVITE_REGISTER_REQ &&
            packet.type() != chatproj::Packet::INVITE_UNREGISTER_REQ) {

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

            // If the user already has a session, force-kick the stale one.
            // This handles the case where the previous connection died without
            // a clean TCP close (e.g. client crashed, network dropped).
            manager_.kick_user(req.username());

            auto token_opt = auth_manager_.authenticateUser(req.username(), req.password());
            
            if (token_opt.has_value()) {
                authenticated_ = true;
                username_ = req.username();
                // Prime avatar_version_ so broadcast_presence below
                // includes the right version on the user's
                // UserPresence entry. Pulled from the users table
                // once at login; later UPDATE_AVATAR_REQ handlers
                // refresh it inline before broadcasting.
                avatar_version_ = auth_manager_.getAvatarVersion(username_);
                send_response(chatproj::Packet::LOGIN_RES, true, "Login successful!", token_opt.value());
                manager_.broadcast_presence();
            } else {
                send_response(chatproj::Packet::LOGIN_RES, false, "Invalid username or password.");
            }
        }

        // --- DIRECT MESSAGE ---
        // Persistence-first flow: identity stamp → self-DM guard →
        // friends-only check → insert into dm_messages → stamp the
        // persisted id back on the routed packet → live-deliver to
        // recipient if online → always echo to sender. The previous
        // "user is offline" error packet is gone — DMs are always
        // persisted, so the recipient will see them on their next
        // login via DM_CONVERSATIONS_REQ / DM_HISTORY_REQ.
        else if (packet.type() == chatproj::Packet::DIRECT_MSG) {
            if (!authenticated_) return;

            auto now = std::chrono::system_clock::now();
            int64_t current_time = std::chrono::system_clock::to_time_t(now);

            chatproj::Packet routed_packet = packet;
            auto* dmsg = routed_packet.mutable_direct_msg();
            dmsg->set_sender(username_); // Enforce sender identity
            dmsg->set_timestamp(current_time);

            // Self-DM guard. The DB schema allows self-rows, but the
            // UX doesn't make sense; reject explicitly so persistence
            // doesn't silently accumulate them.
            if (dmsg->recipient() == username_) {
                return;
            }

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

            // Persist before delivery. On DB failure, surface to
            // sender as a generic "couldn't deliver" — the message
            // is genuinely lost in that branch (rare).
            int64_t new_id = auth_manager_.insertDm(
                username_, dmsg->recipient(), dmsg->content(), current_time);
            if (new_id == 0) {
                chatproj::Packet error_packet;
                error_packet.set_type(chatproj::Packet::DIRECT_MSG);
                auto* err_msg = error_packet.mutable_direct_msg();
                err_msg->set_sender(username_);
                err_msg->set_recipient(dmsg->recipient());
                err_msg->set_content("The server couldn't deliver your message. Please try again.");
                err_msg->set_timestamp(current_time);

                std::string serialized;
                error_packet.SerializeToString(&serialized);
                auto framed = std::make_shared<std::vector<uint8_t>>(chatproj::create_framed_packet(serialized));
                deliver(framed);
                return;
            }

            // Stamp the persisted id onto the routed packet so the
            // client can use it as `up_to_id` in DmMarkReadReq.
            dmsg->set_id(new_id);

            // Best-effort live delivery — return value is informational
            // only. Recipient gets it now if online, on next login
            // via DM_CONVERSATIONS_REQ / DM_HISTORY_REQ otherwise.
            manager_.send_private(routed_packet, dmsg->recipient());

            // Always echo to sender so their UI shows the DM as
            // delivered, carrying the new id field.
            std::string serialized;
            routed_packet.SerializeToString(&serialized);
            auto framed = std::make_shared<std::vector<uint8_t>>(chatproj::create_framed_packet(serialized));
            deliver(framed);
        }

        // --- DM CONVERSATIONS REQ ---
        // One-shot pull of all conversation previews + unread counts
        // for the local user. Fired on login from the renderer to
        // populate the DmSidebar cards.
        else if (packet.type() == chatproj::Packet::DM_CONVERSATIONS_REQ) {
            if (!authenticated_) return;

            auto convs = auth_manager_.fetchDmConversations(username_);

            chatproj::Packet response;
            response.set_type(chatproj::Packet::DM_CONVERSATIONS_RES);
            auto* res = response.mutable_dm_conversations_res();
            for (const auto& c : convs) {
                auto* preview = res->add_conversations();
                preview->set_peer(c.peer);
                preview->set_last_message_content(c.last_message_content);
                preview->set_last_message_sender(c.last_message_sender);
                preview->set_last_message_id(c.last_message_id);
                preview->set_last_timestamp(c.last_timestamp);
                preview->set_unread_count(c.unread_count);
            }

            std::string s;
            response.SerializeToString(&s);
            deliver(std::make_shared<std::vector<uint8_t>>(
                chatproj::create_framed_packet(s)));
        }

        // --- DM HISTORY REQ ---
        // Paginated fetch of messages between the local user and
        // `peer`. before_id=0 returns the latest page; client
        // paginates upward by passing the oldest seen id.
        else if (packet.type() == chatproj::Packet::DM_HISTORY_REQ) {
            if (!authenticated_) return;
            const auto& req = packet.dm_history_req();
            const std::string& peer = req.peer();
            if (peer.empty()) return;

            int32_t limit = req.limit();
            if (limit <= 0) limit = 50;
            if (limit > 200) limit = 200;

            bool has_more = false;
            auto rows = auth_manager_.fetchDmHistory(
                username_, peer, req.before_id(), limit, has_more);

            chatproj::Packet response;
            response.set_type(chatproj::Packet::DM_HISTORY_RES);
            auto* res = response.mutable_dm_history_res();
            res->set_peer(peer);
            res->set_has_more(has_more);
            for (const auto& r : rows) {
                auto* msg = res->add_messages();
                msg->set_id(r.id);
                msg->set_sender(r.sender);
                msg->set_content(r.content);
                msg->set_timestamp(r.timestamp);
            }

            std::string s;
            response.SerializeToString(&s);
            deliver(std::make_shared<std::vector<uint8_t>>(
                chatproj::create_framed_packet(s)));
        }

        // --- DM MARK READ REQ ---
        // Fire-and-forget: update dm_read_state.last_read_id so the
        // next DM_CONVERSATIONS_REQ surfaces the correct unread
        // count. No response — TCP delivery is the implicit ack.
        else if (packet.type() == chatproj::Packet::DM_MARK_READ_REQ) {
            if (!authenticated_) return;
            const auto& req = packet.dm_mark_read_req();
            if (req.peer().empty()) return;
            auth_manager_.markDmRead(username_, req.peer(), req.up_to_id());
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

        // --- AVATAR UPLOAD ---
        // Empty bytes = remove. Otherwise validate JPEG magic + 200 KB cap,
        // store via AuthManager, broadcast AvatarChanged to every online
        // session. See docs/superpowers/specs/2026-05-12-custom-profile-
        // pictures-design.md §5.
        else if (packet.type() == chatproj::Packet::UPDATE_AVATAR_REQ) {
            if (!authenticated_) return;
            const auto& req = packet.update_avatar_req();
            const std::string& data = req.data();

            chatproj::Packet response;
            response.set_type(chatproj::Packet::UPDATE_AVATAR_RES);
            auto* res = response.mutable_update_avatar_res();

            if (!data.empty()) {
                if (data.size() < 2 ||
                    static_cast<unsigned char>(data[0]) != 0xFF ||
                    static_cast<unsigned char>(data[1]) != 0xD8) {
                    res->set_success(false);
                    res->set_message("Not a JPEG");
                    std::string s;
                    response.SerializeToString(&s);
                    deliver(std::make_shared<std::vector<uint8_t>>(
                        chatproj::create_framed_packet(s)));
                    return;
                }
                if (data.size() > 200 * 1024) {
                    res->set_success(false);
                    res->set_message("Avatar too large");
                    std::string s;
                    response.SerializeToString(&s);
                    deliver(std::make_shared<std::vector<uint8_t>>(
                        chatproj::create_framed_packet(s)));
                    return;
                }
            }

            std::string version;
            try {
                version = auth_manager_.setAvatar(username_, data);
            } catch (const std::exception& e) {
                std::cerr << "[Server] setAvatar failed: " << e.what() << "\n";
                res->set_success(false);
                res->set_message("Storage error");
                std::string s;
                response.SerializeToString(&s);
                deliver(std::make_shared<std::vector<uint8_t>>(
                    chatproj::create_framed_packet(s)));
                return;
            }

            // Refresh our cached version so subsequent
            // broadcast_presence calls (e.g. on a new client joining)
            // see this session's new avatar_version.
            avatar_version_ = version;

            res->set_success(true);
            res->set_version(version);
            std::string s;
            response.SerializeToString(&s);
            deliver(std::make_shared<std::vector<uint8_t>>(
                chatproj::create_framed_packet(s)));

            manager_.broadcast_avatar_changed(username_, version);
        }

        // --- AVATAR FETCH ---
        // Authenticated callers can fetch anyone's avatar. Missing users
        // or missing avatars both surface as empty version + empty data.
        else if (packet.type() == chatproj::Packet::FETCH_AVATAR_REQ) {
            if (!authenticated_) return;
            const auto& req = packet.fetch_avatar_req();
            const std::string& target = req.username();

            auto [version, data] = auth_manager_.getAvatar(target);

            chatproj::Packet response;
            response.set_type(chatproj::Packet::FETCH_AVATAR_RES);
            auto* res = response.mutable_fetch_avatar_res();
            res->set_username(target);
            res->set_version(version);
            if (!data.empty()) {
                res->set_data(data);
            }

            std::string s;
            response.SerializeToString(&s);
            deliver(std::make_shared<std::vector<uint8_t>>(
                chatproj::create_framed_packet(s)));
        }

        // --- DM PRIVACY SETTING ---
        else if (packet.type() == chatproj::Packet::DM_PRIVACY) {
            if (!authenticated_) return;
            dm_friends_only_ = packet.dm_privacy().friends_only();
            std::cout << "[Server] User " << username_ << " set dm_friends_only to " << dm_friends_only_ << "\n";
        }

        // --- COMMUNITY SERVER HEARTBEAT ---
        else if (packet.type() == chatproj::Packet::SERVER_HEARTBEAT) {
            if (!auth_manager_.verifySharedSecret(packet.auth_token())) {
                std::cout << "[Security] Dropped heartbeat - invalid shared secret.\n";
                return;
            }
            auto& hb = packet.server_heartbeat();
            std::cout << "[Server] Heartbeat from community server: " << hb.name() << " at " << hb.host_ip() << ":" << hb.port() << "\n";
            int server_id = auth_manager_.upsertCommunityServer(
                hb.name(), hb.description(), hb.host_ip(), hb.port(), hb.member_count());

            // Auto-rejoin: reply with the assigned server_id so the
            // community can populate Membership{Register,Revoke}Req on
            // future packets. Community uses a one-shot TLS connection
            // here — this is its only chance to read the id.
            chatproj::Packet resp;
            resp.set_type(chatproj::Packet::SERVER_HEARTBEAT_RES);
            resp.mutable_server_heartbeat_res()->set_server_id(server_id);
            std::string serialized;
            resp.SerializeToString(&serialized);
            auto framed = std::make_shared<std::vector<uint8_t>>(
                chatproj::create_framed_packet(serialized));
            deliver(framed);
        }

        // --- COMMUNITY SERVER: REGISTER AN INVITE ---
        else if (packet.type() == chatproj::Packet::INVITE_REGISTER_REQ) {
            if (!auth_manager_.verifySharedSecret(packet.auth_token())) {
                std::cout << "[Security] Dropped invite_register - invalid shared secret.\n";
                return;
            }
            const auto& req = packet.invite_register_req();
            auth_manager_.registerCommunityInvite(
                req.code(), req.host(), static_cast<int>(req.port()), req.expires_at());
            std::cout << "[Server] Registered invite " << req.code()
                      << " -> " << req.host() << ":" << req.port() << "\n";
        }

        // --- COMMUNITY SERVER: UNREGISTER AN INVITE ---
        else if (packet.type() == chatproj::Packet::INVITE_UNREGISTER_REQ) {
            if (!auth_manager_.verifySharedSecret(packet.auth_token())) {
                std::cout << "[Security] Dropped invite_unregister - invalid shared secret.\n";
                return;
            }
            const auto& req = packet.invite_unregister_req();
            auth_manager_.unregisterCommunityInvite(req.code());
            std::cout << "[Server] Unregistered invite " << req.code() << "\n";
        }

        // --- AUTO-REJOIN: COMMUNITY REGISTERS A MEMBERSHIP ---
        // Idempotent — fires on every successful community auth, so
        // re-firing is harmless and serves as the bootstrap mechanism
        // for pre-feature memberships.
        else if (packet.type() == chatproj::Packet::MEMBERSHIP_REGISTER_REQ) {
            if (!auth_manager_.verifySharedSecret(packet.auth_token())) {
                std::cout << "[Security] Dropped membership_register - invalid shared secret.\n";
                return;
            }
            const auto& req = packet.membership_register_req();
            if (req.username().empty() || req.server_id() == 0) return;
            auth_manager_.registerMembership(req.username(), req.server_id());
        }

        // --- AUTO-REJOIN: REVOKE A MEMBERSHIP (dual-origin) ---
        // Two callers can revoke: community server (shared-secret,
        // kick/ban/leave path) or JWT-authed client (stale-membership
        // cleanup). Community can revoke any user; client can only
        // revoke its own (enforced via session username).
        else if (packet.type() == chatproj::Packet::MEMBERSHIP_REVOKE_REQ) {
            const auto& req = packet.membership_revoke_req();
            if (req.server_id() == 0) return;

            std::string target_username;
            if (auth_manager_.verifySharedSecret(packet.auth_token())) {
                if (req.username().empty()) return;
                target_username = req.username();
            } else if (authenticated_) {
                target_username = username_;
            } else {
                std::cout << "[Security] Dropped membership_revoke - no valid auth.\n";
                return;
            }
            auth_manager_.revokeMembership(target_username, req.server_id());
        }

        // --- CLIENT: RESOLVE AN INVITE CODE TO HOST:PORT ---
        else if (packet.type() == chatproj::Packet::INVITE_RESOLVE_REQ) {
            if (!authenticated_) return;
            const auto& req = packet.invite_resolve_req();
            auto resolved = auth_manager_.resolveCommunityInvite(req.code());

            chatproj::Packet resp;
            resp.set_type(chatproj::Packet::INVITE_RESOLVE_RES);
            auto* body = resp.mutable_invite_resolve_res();
            body->set_code(req.code());
            if (resolved) {
                body->set_success(true);
                body->set_message("");
                body->set_host(resolved->first);
                body->set_port(static_cast<uint32_t>(resolved->second));
            } else {
                body->set_success(false);
                body->set_message("Unknown or expired invite");
            }

            std::string serialized;
            resp.SerializeToString(&serialized);
            auto framed = std::make_shared<std::vector<uint8_t>>(
                chatproj::create_framed_packet(serialized));
            deliver(framed);
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
    /// Cached on login from users.avatar_version; updated inline on
    /// UPDATE_AVATAR_REQ. Read by broadcast_presence.
    std::string avatar_version_;
    bool dm_friends_only_ = false;
    AuthManager& auth_manager_;
    std::deque<std::shared_ptr<std::vector<uint8_t>>> write_queue_;
    std::chrono::steady_clock::time_point last_activity_;
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

    // Collect (username, avatar_version) for each session. Read each
    // session's cached avatar_version (updated at login + on every
    // UPDATE_AVATAR_REQ) rather than hitting the DB per broadcast.
    std::vector<std::pair<std::string, std::string>> active_users;
    {
        std::lock_guard<std::mutex> lock(mutex_);
        for (const auto& session : sessions_) {
            std::string uname = session->username();
            if (!uname.empty()) {
                active_users.emplace_back(uname, session->avatar_version());
            }
        }
    }

    for (const auto& [uname, ver] : active_users) {
        auto* entry = presence->add_users();
        entry->set_username(uname);
        entry->set_avatar_version(ver);
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

void SessionManager::kick_user(const std::string& username) {
    std::shared_ptr<Session> stale;
    {
        std::lock_guard<std::mutex> lock(mutex_);
        for (const auto& session : sessions_) {
            if (session->username() == username) {
                stale = session;
                break;
            }
        }
    }
    if (stale) {
        std::cout << "[Manager] Kicking stale session for: " << username << "\n";
        leave(stale);
    }
}

void SessionManager::sweep_stale(std::chrono::seconds timeout) {
    auto now = std::chrono::steady_clock::now();
    std::vector<std::shared_ptr<Session>> stale;
    {
        std::lock_guard<std::mutex> lock(mutex_);
        for (const auto& session : sessions_) {
            if (now - session->last_activity() > timeout) {
                stale.push_back(session);
            }
        }
    }
    for (auto& s : stale) {
        std::cout << "[Manager] Sweeping stale session: " << s->username() << "\n";
        leave(s);
    }
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

        // Periodic sweep of stale sessions (no activity for 60s).
        // Catches dead connections where TCP FIN was never sent (e.g. client crash).
        boost::asio::steady_timer sweep_timer(io_context);
        std::function<void(const boost::system::error_code&)> sweep_fn;
        sweep_fn = [&](const boost::system::error_code& ec) {
            if (ec) return;
            manager.sweep_stale(std::chrono::seconds(60));
            sweep_timer.expires_after(std::chrono::seconds(30));
            sweep_timer.async_wait(sweep_fn);
        };
        sweep_timer.expires_after(std::chrono::seconds(30));
        sweep_timer.async_wait(sweep_fn);

        std::cout << "Decibell Central Server running on port 8080...\n";

        io_context.run();
    } catch (std::exception& e) {
        std::cerr << "Exception: " << e.what() << "\n";
    }
    return 0;
}