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
#include <optional>
#ifdef __linux__
#include <netinet/tcp.h>
#endif
#include <boost/asio.hpp>
#include <boost/asio/ssl.hpp>
#include "messages.pb.h"
#include "../common/net_utils.hpp"
#include "auth_utils.hpp"
#include "../common/ed25519_keys.hpp"
#include "session_manager.hpp"
#include "auth_manager.hpp"

namespace ssl = boost::asio::ssl;
using boost::asio::ip::tcp;

/// STUN servers ("host:port") handed to every client in LOGIN_RES for
/// P2P DM calls. Filled once in main() from DECIBELL_STUN_SERVERS; empty
/// means "use the client's built-in default list".
static std::vector<std::string> g_stun_servers;

/// E2EE DM envelope cap: the edit-body cap (64 KiB) plus the sealed
/// header + tag. A client never produces more; anything bigger is junk.
constexpr size_t MAX_DM_ENVELOPE = 64 * 1024 + 64;
/// Identity-bundle field sizes (X25519 / Ed25519 public keys, Ed25519
/// signature) and the passphrase-wrapped backup blob cap.
constexpr size_t E2EE_PUB_LEN = 32;
constexpr size_t E2EE_SIG_LEN = 64;
constexpr size_t MAX_E2EE_BACKUP = 8 * 1024;

/// Minimal per-session token bucket. Central has no general rate limiting
/// and runs a single io thread with synchronous Postgres, so an unthrottled
/// storm from one client would stall every user. Used for CALL_SIGNAL
/// (20 burst / 5 per second: far above a real handshake, ~6 signals per
/// call, and keeps the INVITE-only DB touch bounded), the E2EE key
/// endpoints, and status changes.
struct RateBucket {
    RateBucket(double capacity, double per_second)
        : capacity(capacity), per_second(per_second), tokens(capacity) {}
    double capacity;
    double per_second;
    double tokens;
    std::chrono::steady_clock::time_point last = std::chrono::steady_clock::now();
    bool take() {
        auto now = std::chrono::steady_clock::now();
        double elapsed = std::chrono::duration<double>(now - last).count();
        last = now;
        tokens += elapsed * per_second;
        if (tokens > capacity) tokens = capacity;
        if (tokens < 1.0) return false;
        tokens -= 1.0;
        return true;
    }
};

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
        // Cap the per-session backlog. A client that stops reading (or reads
        // slowly) would otherwise let broadcasts / presence / DMs pile up in
        // memory without bound. Over the cap, drop the connection.
        constexpr size_t MAX_QUEUED_MESSAGES = 1024;
        if (write_queue_.size() >= MAX_QUEUED_MESSAGES) {
            // Post the disconnect rather than calling leave() synchronously:
            // deliver() runs inside broadcast loops that iterate sessions_,
            // and erasing here would invalidate the iterator.
            auto self = shared_from_this();
            boost::asio::post(socket_.lowest_layer().get_executor(),
                              [this, self]() { manager_.leave(self); });
            return;
        }
        bool write_in_progress = !write_queue_.empty();
        write_queue_.push_back(framed_data);
        if (!write_in_progress) {
            do_write();
        }
    }

    std::string username() const { return username_; }
    bool authenticated() const { return authenticated_; }

    /// Cancel in-flight I/O and close the socket. SessionManager::leave()
    /// calls this: removing the session from the set was never enough —
    /// the pending async_read kept the Session alive, so a kicked or
    /// swept session stayed `authenticated_`, could still send DMs and
    /// friend actions, yet was invisible to presence / send_private and
    /// never swept again (sweep iterates sessions_).
    void close_connection() {
        boost::system::error_code ec;
        socket_.lowest_layer().cancel(ec);
        socket_.lowest_layer().close(ec);
    }
    /// The user's current avatar_version, loaded at login and
    /// refreshed inline on UPDATE_AVATAR_REQ. broadcast_presence
    /// reads this so each UserPresence entry carries the version
    /// without an extra DB query per broadcast.
    const std::string& avatar_version() const { return avatar_version_; }
    bool dm_friends_only() const { return dm_friends_only_; }

    // --- User statuses (docs/superpowers/specs/2026-10-11-user-status-design.md) ---
    /// Seen by others: authenticated and not invisible.
    bool presence_visible() const {
        return authenticated_ && presence_status_ != chatproj::PRESENCE_INVISIBLE;
    }
    /// What others see: the chosen status, AWAY while an ONLINE user idles.
    chatproj::PresenceStatus effective_status() const {
        if (presence_status_ == chatproj::PRESENCE_ONLINE && idle_) return chatproj::PRESENCE_AWAY;
        return presence_status_;
    }
    /// The client applies PresenceDelta (LoginRequest.presence_deltas).
    bool presence_deltas() const { return presence_deltas_; }

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
                    // memcpy rather than reinterpret_cast: reading a
                    // uint32_t through a char[4] violates strict aliasing.
                    uint32_t net_len;
                    std::memcpy(&net_len, inbound_header_, 4);
                    uint32_t body_length = ntohl(net_len);
                    if (body_length > 2 * 1024 * 1024) {
                        // Don't just `return` — that leaves the read loop
                        // dead but the socket open, lingering until the
                        // stale sweep. Drop the session (destroying it
                        // closes the socket).
                        std::cout << "[Session] Oversized frame (" << body_length
                                  << " bytes); closing connection.\n";
                        manager_.leave(shared_from_this());
                        return;
                    }
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
        // Heartbeats and invite/membership register/unregister/revoke from
        // community servers authenticate with the pre-shared secret, not a
        // JWT — they are verified below in their own handlers.
        //
        // MEMBERSHIP_REVOKE_REQ has dual origins: shared-secret community
        // path falls through here as unauthenticated (verified in handler);
        // JWT-authed client path comes in on an already-authenticated
        // session and never hits the unauthenticated branch below.
        if (packet.type() != chatproj::Packet::REGISTER_REQ &&
            packet.type() != chatproj::Packet::LOGIN_REQ &&
            packet.type() != chatproj::Packet::HANDSHAKE &&
            packet.type() != chatproj::Packet::SERVER_HEARTBEAT &&
            packet.type() != chatproj::Packet::CLIENT_PING &&
            packet.type() != chatproj::Packet::INVITE_REGISTER_REQ &&
            packet.type() != chatproj::Packet::INVITE_UNREGISTER_REQ &&
            packet.type() != chatproj::Packet::MEMBERSHIP_REGISTER_REQ &&
            packet.type() != chatproj::Packet::MEMBERSHIP_REVOKE_REQ &&
            packet.type() != chatproj::Packet::SYNC_SERVER_PICTURE_REQ) {

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

            auto token_opt = auth_manager_.authenticateUser(req.username(), req.password());

            if (token_opt.has_value()) {
                authenticated_ = true;
                username_ = req.username();
                presence_deltas_ = req.presence_deltas();
                // One users-row read for presence: avatar_version (each
                // UserPresence entry carries it; UPDATE_AVATAR_REQ refreshes
                // it inline), the saved status — applied before anyone hears
                // about this login, so an invisible user never flashes
                // online — and the friends-only DM setting.
                const auto profile = auth_manager_.getPresenceProfile(username_);
                avatar_version_ = profile.avatar_version;
                presence_status_ = profile.status;
                dm_friends_only_ = profile.dm_friends_only;
                idle_ = false;
                // Only now that the password is verified, force-kick any
                // stale session for this user (previous connection died
                // without a clean TCP close, or another device). Kicking
                // BEFORE the password check let any unauthenticated client
                // evict any user by name (send LOGIN_REQ{victim} with a
                // bogus password). After username_ is set, so the old
                // session's leave() sees this one and a reconnect doesn't
                // announce the user offline in between.
                manager_.kick_user(username_, shared_from_this());
                send_response(chatproj::Packet::LOGIN_RES, true, "Login successful!", token_opt.value());
                manager_.send_presence_snapshot(shared_from_this());
                // Announce only a login others can see: an "offline" delta
                // for an invisible login would itself give it away.
                if (presence_visible()) manager_.broadcast_presence_change(username_);
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

            // E2EE: the sealed body rides `envelope`; central only caps
            // its size and stores/relays it opaquely. reply_to_envelope
            // is server-populated below — never trust a client's.
            if (dmsg->envelope().size() > MAX_DM_ENVELOPE) {
                return;
            }
            dmsg->clear_reply_to_envelope();

            if (!manager_.check_dm_allowed(username_, dmsg->recipient(), auth_manager_)) {
                chatproj::Packet error_packet;
                error_packet.set_type(chatproj::Packet::DIRECT_MSG);
                auto* err_msg = error_packet.mutable_direct_msg();
                err_msg->set_sender(username_);
                err_msg->set_recipient(dmsg->recipient());
                err_msg->set_content("This user only accepts direct messages from users in their friends list.");
                err_msg->set_timestamp(current_time);
                err_msg->set_nonce(dmsg->nonce());

                std::string serialized;
                error_packet.SerializeToString(&serialized);
                auto framed = std::make_shared<std::vector<uint8_t>>(chatproj::create_framed_packet(serialized));
                deliver(framed);
                return;
            }

            // Validate + resolve the reply parent within THIS conversation
            // before persisting — a forged reply_to must neither persist nor
            // leak another conversation's content. Not found → not a reply.
            // When valid, embed the parent's author + content so both sides
            // render the quoted preview without needing the parent loaded.
            if (dmsg->reply_to() > 0) {
                auto parent = auth_manager_.fetchDmPreview(
                    username_, dmsg->recipient(), dmsg->reply_to());
                if (!parent) {
                    dmsg->set_reply_to(0);
                } else {
                    dmsg->set_reply_to_sender(parent->sender);
                    dmsg->set_reply_to_content(parent->content);
                    if (!parent->envelope.empty()) {
                        dmsg->set_reply_to_envelope(parent->envelope);
                    }
                }
            }

            // Persist before delivery. On DB failure, surface to
            // sender as a generic "couldn't deliver" — the message
            // is genuinely lost in that branch (rare).
            int64_t new_id = auth_manager_.insertDm(
                username_, dmsg->recipient(), dmsg->content(), current_time, dmsg->reply_to(),
                dmsg->envelope());
            if (new_id == 0) {
                chatproj::Packet error_packet;
                error_packet.set_type(chatproj::Packet::DIRECT_MSG);
                auto* err_msg = error_packet.mutable_direct_msg();
                err_msg->set_sender(username_);
                err_msg->set_recipient(dmsg->recipient());
                err_msg->set_content("The server couldn't deliver your message. Please try again.");
                err_msg->set_timestamp(current_time);
                err_msg->set_nonce(dmsg->nonce());

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

        // --- CALL SIGNAL (P2P DM calls) ---
        // Ephemeral relay for the DM-call handshake: INVITE / RINGING /
        // ACCEPT / REJECT / BUSY / CANCEL / HANGUP / STREAM_START /
        // STREAM_STOP. Nothing is persisted and the only DB touch is
        // check_dm_allowed on INVITE (same policy as a DM: blocked → no,
        // friends-only recipient → friends only). Central stamps `from`
        // so the peer never trusts a client-supplied sender, and answers
        // PEER_OFFLINE / NOT_ALLOWED itself from the recipient's POV.
        else if (packet.type() == chatproj::Packet::CALL_SIGNAL) {
            if (!authenticated_ || !packet.has_call_signal()) return;
            if (!call_bucket_.take()) {
                std::cout << "[Call] Signal rate limit hit for " << username_ << "\n";
                return;
            }

            chatproj::Packet routed = packet;
            auto* sig = routed.mutable_call_signal();
            if (sig->to().empty() || sig->to() == username_) return;
            if (sig->to().size() > 64 || sig->call_id().size() > 64 ||
                sig->pub_key().size() > 64 || sig->pub_key_sig().size() > 64 ||
                sig->candidates_size() > 16) {
                return;
            }
            sig->set_from(username_);
            sig->set_timestamp(std::chrono::system_clock::to_time_t(
                std::chrono::system_clock::now()));

            if (sig->kind() == chatproj::CallSignal::INVITE &&
                !manager_.check_dm_allowed(username_, sig->to(), auth_manager_)) {
                send_call_reply(chatproj::CallSignal::NOT_ALLOWED, sig->call_id(), sig->to());
                return;
            }
            // An invisible user looks offline to callers too (owner
            // decision). Only the INVITE: an invisible user can still place
            // calls, and the rest of that handshake must reach them.
            if (sig->kind() == chatproj::CallSignal::INVITE &&
                !manager_.is_user_visible(sig->to())) {
                send_call_reply(chatproj::CallSignal::PEER_OFFLINE, sig->call_id(), sig->to());
                return;
            }

            if (!manager_.send_private(routed, sig->to())) {
                send_call_reply(chatproj::CallSignal::PEER_OFFLINE, sig->call_id(), sig->to());
            }
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
                if (!c.last_message_envelope.empty()) {
                    preview->set_last_message_envelope(c.last_message_envelope);
                }
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

            // Route by request mode, mirroring the community channel-history
            // handler: around_id>0 → context window (replace on the client);
            // after_id>0 → newer page (append); else → older/most-recent page
            // (prepend). The DB helpers clamp their own limits.
            bool has_more = false;
            bool has_more_after = false;
            std::vector<AuthManager::DmHistoryRow> rows;
            if (req.around_id() > 0) {
                rows = auth_manager_.fetchDmHistoryAround(
                    username_, peer, req.around_id(), req.limit(),
                    has_more, has_more_after);
            } else if (req.after_id() > 0) {
                rows = auth_manager_.fetchDmHistoryAfter(
                    username_, peer, req.after_id(), req.limit(), has_more_after);
            } else {
                rows = auth_manager_.fetchDmHistory(
                    username_, peer, req.before_id(), limit, has_more);
            }

            chatproj::Packet response;
            response.set_type(chatproj::Packet::DM_HISTORY_RES);
            auto* res = response.mutable_dm_history_res();
            res->set_peer(peer);
            res->set_has_more(has_more);
            res->set_has_more_after(has_more_after);
            res->set_around_id(req.around_id());
            res->set_after_id(req.after_id());
            for (const auto& r : rows) {
                auto* msg = res->add_messages();
                msg->set_id(r.id);
                msg->set_sender(r.sender);
                msg->set_content(r.content);
                msg->set_timestamp(r.timestamp);
                msg->set_edited_at(r.edited_at);
                msg->set_reply_to(r.reply_to);
                msg->set_reply_to_sender(r.reply_to_sender);
                msg->set_reply_to_content(r.reply_to_content);
                if (!r.envelope.empty()) msg->set_envelope(r.envelope);
                if (!r.reply_to_envelope.empty()) msg->set_reply_to_envelope(r.reply_to_envelope);
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

        // --- DM_DELETE_REQ ---
        // Sender-only delete. The auth check happens inside the SQL
        // WHERE clause (sender = username_), so a forged packet with
        // someone else's message id is a no-op.
        else if (packet.type() == chatproj::Packet::DM_DELETE_REQ) {
            if (!authenticated_) return;
            const auto& req = packet.dm_delete_req();

            // Build RES skeleton early so failure branches can reuse.
            auto build_res = [&](bool success, const std::string& msg) {
                chatproj::Packet rsp;
                rsp.set_type(chatproj::Packet::DM_DELETE_RES);
                auto* res = rsp.mutable_dm_delete_res();
                res->set_success(success);
                res->set_message(msg);
                res->set_peer(req.peer());
                res->set_message_id(req.message_id());
                std::string serialized;
                rsp.SerializeToString(&serialized);
                auto framed = std::make_shared<std::vector<uint8_t>>(
                    chatproj::create_framed_packet(serialized));
                deliver(framed);
            };

            if (req.peer().empty() || req.message_id() == 0) {
                build_res(false, "Invalid request.");
                return;
            }

            bool ok = auth_manager_.deleteDmMessage(
                username_, req.peer(), req.message_id());

            build_res(ok, ok ? "" : "Message not found or not deletable.");

            if (!ok) return;

            // On success: broadcast DM_MESSAGE_DELETED to BOTH sessions.
            // peer field is rewritten per recipient so it's always "the
            // other user" from the receiving session's perspective.
            int64_t now_ts = static_cast<int64_t>(std::time(nullptr));

            // Echo to the sender (the requester themselves) — drives
            // the broadcast-handler dedupe with the optimistic remove.
            chatproj::Packet sender_bcast;
            sender_bcast.set_type(chatproj::Packet::DM_MESSAGE_DELETED);
            auto* sb = sender_bcast.mutable_dm_message_deleted();
            sb->set_peer(req.peer());     // from sender's POV, peer = recipient
            sb->set_message_id(req.message_id());
            sb->set_deleted_at(now_ts);
            std::string sender_ser;
            sender_bcast.SerializeToString(&sender_ser);
            auto sender_framed = std::make_shared<std::vector<uint8_t>>(
                chatproj::create_framed_packet(sender_ser));
            deliver(sender_framed);

            // To the recipient (if online): peer = the sender.
            chatproj::Packet recv_bcast;
            recv_bcast.set_type(chatproj::Packet::DM_MESSAGE_DELETED);
            auto* rb = recv_bcast.mutable_dm_message_deleted();
            rb->set_peer(username_);       // from recipient's POV, peer = sender
            rb->set_message_id(req.message_id());
            rb->set_deleted_at(now_ts);
            manager_.send_private(recv_bcast, req.peer());
        }

        // --- DM_EDIT_REQ ---
        // Sender-only edit; the SQL WHERE clause is the authorization check.
        else if (packet.type() == chatproj::Packet::DM_EDIT_REQ) {
            if (!authenticated_) return;
            const auto& req = packet.dm_edit_req();

            auto build_res = [&](bool success, const std::string& msg) {
                chatproj::Packet rsp;
                rsp.set_type(chatproj::Packet::DM_EDIT_RES);
                auto* res = rsp.mutable_dm_edit_res();
                res->set_success(success);
                res->set_message(msg);
                res->set_peer(req.peer());
                res->set_message_id(req.message_id());
                std::string serialized;
                rsp.SerializeToString(&serialized);
                deliver(std::make_shared<std::vector<uint8_t>>(
                    chatproj::create_framed_packet(serialized)));
            };

            if (req.peer().empty() || req.message_id() == 0) {
                build_res(false, "Invalid request.");
                return;
            }
            const std::string& content = req.content();
            const std::string& envelope = req.envelope();
            // An encrypted edit carries the placeholder in `content` and
            // the real body sealed in `envelope`; a plaintext edit has
            // no envelope. Either way something must be there.
            if (content.empty() && envelope.empty()) { build_res(false, "Message can't be empty."); return; }
            if (content.size() > 64 * 1024) { build_res(false, "Message too long."); return; }
            if (envelope.size() > MAX_DM_ENVELOPE) { build_res(false, "Message too long."); return; }

            const int64_t now_ts = static_cast<int64_t>(std::time(nullptr));
            bool ok = auth_manager_.editDmMessage(
                username_, req.peer(), req.message_id(), content, now_ts, envelope);
            build_res(ok, ok ? "" : "Message not found or not editable.");
            if (!ok) return;

            // Broadcast DM_MESSAGE_EDITED to both sessions, peer rewritten
            // per recipient (same convention as DM_MESSAGE_DELETED).
            chatproj::Packet sender_bcast;
            sender_bcast.set_type(chatproj::Packet::DM_MESSAGE_EDITED);
            auto* sb = sender_bcast.mutable_dm_message_edited();
            sb->set_peer(req.peer());      // sender's POV: peer = recipient
            sb->set_message_id(req.message_id());
            sb->set_content(content);
            sb->set_edited_at(now_ts);
            sb->set_sender(username_);
            if (!envelope.empty()) sb->set_envelope(envelope);
            std::string sender_ser;
            sender_bcast.SerializeToString(&sender_ser);
            deliver(std::make_shared<std::vector<uint8_t>>(
                chatproj::create_framed_packet(sender_ser)));

            chatproj::Packet recv_bcast;
            recv_bcast.set_type(chatproj::Packet::DM_MESSAGE_EDITED);
            auto* rb = recv_bcast.mutable_dm_message_edited();
            rb->set_peer(username_);       // recipient's POV: peer = sender
            rb->set_message_id(req.message_id());
            rb->set_content(content);
            rb->set_edited_at(now_ts);
            rb->set_sender(username_);
            if (!envelope.empty()) rb->set_envelope(envelope);
            manager_.send_private(recv_bcast, req.peer());
        }

        // --- E2EE: PUBLISH KEYS ---
        // Store the caller's identity bundle (central assigns key_id)
        // and/or replace their passphrase-wrapped backup. Central never
        // interprets the bytes beyond size checks. A new bundle is
        // announced to every session like an avatar change.
        else if (packet.type() == chatproj::Packet::E2EE_PUBLISH_KEYS_REQ) {
            if (!authenticated_) return;
            if (!key_bucket_.take()) return;
            const auto& req = packet.e2ee_publish_keys_req();

            auto reply = [&](bool success, const std::string& msg, uint32_t key_id) {
                chatproj::Packet rsp;
                rsp.set_type(chatproj::Packet::E2EE_PUBLISH_KEYS_RES);
                auto* res = rsp.mutable_e2ee_publish_keys_res();
                res->set_success(success);
                res->set_message(msg);
                res->set_key_id(key_id);
                std::string serialized;
                rsp.SerializeToString(&serialized);
                deliver(std::make_shared<std::vector<uint8_t>>(
                    chatproj::create_framed_packet(serialized)));
            };

            const bool has_bundle = req.has_bundle();
            const std::string& backup = req.backup();
            if (!has_bundle && backup.empty()) { reply(false, "Nothing to publish.", 0); return; }
            if (backup.size() > MAX_E2EE_BACKUP) { reply(false, "Backup too large.", 0); return; }

            const int64_t now_ts = static_cast<int64_t>(std::time(nullptr));
            uint32_t key_id = 0;
            if (has_bundle) {
                const auto& b = req.bundle();
                if (b.dh_pub().size() != E2EE_PUB_LEN || b.sign_pub().size() != E2EE_PUB_LEN ||
                    b.signature().size() != E2EE_SIG_LEN) {
                    reply(false, "Malformed key bundle.", 0);
                    return;
                }
                key_id = auth_manager_.publishE2eeKeys(
                    username_, b.dh_pub(), b.sign_pub(), b.signature(), now_ts);
                if (key_id == 0) { reply(false, "Storage error.", 0); return; }
            } else {
                // Backup-only update (passphrase change): it belongs to
                // the current bundle.
                auto cur = auth_manager_.getE2eeKeys(username_, 0);
                if (!cur) { reply(false, "No published keys to back up.", 0); return; }
                key_id = cur->key_id;
            }
            if (!backup.empty() &&
                !auth_manager_.setE2eeBackup(username_, key_id, backup, now_ts)) {
                reply(false, "Storage error.", key_id);
                return;
            }
            reply(true, "", key_id);
            if (has_bundle) {
                manager_.broadcast_e2ee_keys_changed(username_, key_id);
            }
        }

        // --- E2EE: FETCH KEYS ---
        // Any authenticated user may read anyone's public bundle (that is
        // the point of a public key); key_id 0 = current. Bucketed: the
        // lookup is a DB round-trip on the single io thread.
        else if (packet.type() == chatproj::Packet::E2EE_FETCH_KEYS_REQ) {
            if (!authenticated_) return;
            if (!key_bucket_.take()) return;
            const auto& req = packet.e2ee_fetch_keys_req();
            if (req.username().empty() || req.username().size() > 64) return;

            chatproj::Packet rsp;
            rsp.set_type(chatproj::Packet::E2EE_FETCH_KEYS_RES);
            auto* res = rsp.mutable_e2ee_fetch_keys_res();
            res->set_username(req.username());
            res->set_key_id(req.key_id());
            auto row = auth_manager_.getE2eeKeys(req.username(), req.key_id());
            res->set_found(row.has_value());
            if (row) {
                auto* b = res->mutable_bundle();
                b->set_username(req.username());
                b->set_key_id(row->key_id);
                b->set_dh_pub(row->dh_pub);
                b->set_sign_pub(row->sign_pub);
                b->set_signature(row->signature);
                b->set_created_at(row->created_at);
            }
            std::string serialized;
            rsp.SerializeToString(&serialized);
            deliver(std::make_shared<std::vector<uint8_t>>(
                chatproj::create_framed_packet(serialized)));
        }

        // --- E2EE: FETCH BACKUP ---
        // Only ever the caller's own blob.
        else if (packet.type() == chatproj::Packet::E2EE_FETCH_BACKUP_REQ) {
            if (!authenticated_) return;
            if (!key_bucket_.take()) return;

            chatproj::Packet rsp;
            rsp.set_type(chatproj::Packet::E2EE_FETCH_BACKUP_RES);
            auto* res = rsp.mutable_e2ee_fetch_backup_res();
            auto row = auth_manager_.getE2eeBackup(username_);
            res->set_found(row.has_value());
            if (row) {
                res->set_key_id(row->first);
                res->set_backup(row->second);
            }
            std::string serialized;
            rsp.SerializeToString(&serialized);
            deliver(std::make_shared<std::vector<uint8_t>>(
                chatproj::create_framed_packet(serialized)));
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
                    if (f.status() == chatproj::FriendInfo::OFFLINE && manager_.is_user_visible(f.username())) {
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
                    if (f.status() == chatproj::FriendInfo::OFFLINE && manager_.is_user_visible(f.username())) {
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
                    if (manager_.is_user_visible(f.username())) {
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
                if (data.size() > 1024 * 1024) {
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
        // Saved on the account (users.dm_friends_only) so it holds while
        // the user is offline too. It used to live only in this session,
        // which reset it on every reconnect and let a DM probe tell an
        // online friends-only user (rejected) from an offline one
        // (accepted) — i.e. see through Invisible.
        else if (packet.type() == chatproj::Packet::DM_PRIVACY) {
            if (!authenticated_) return;
            if (!status_bucket_.take()) return;
            const bool friends_only = packet.dm_privacy().friends_only();
            if (friends_only == dm_friends_only_) return;
            dm_friends_only_ = friends_only;
            auth_manager_.setDmFriendsOnly(username_, friends_only);
            std::cout << "[Server] User " << username_ << " set dm_friends_only to " << dm_friends_only_ << "\n";
        }

        // --- USER STATUS ---
        // The chosen status (online / away / dnd / invisible): saved on the
        // account and applied to this session. Others hear about it only
        // when what they can see changes.
        else if (packet.type() == chatproj::Packet::SET_STATUS_REQ) {
            if (!authenticated_ || !packet.has_set_status_req()) return;
            if (!status_bucket_.take()) {
                std::cout << "[Status] Rate limit hit for " << username_ << "\n";
                return;
            }
            const int raw = static_cast<int>(packet.set_status_req().status());
            if (!chatproj::PresenceStatus_IsValid(raw)) return;
            const auto status = static_cast<chatproj::PresenceStatus>(raw);
            if (status == presence_status_) return;
            const PresenceView before = presence_view();
            presence_status_ = status;
            auth_manager_.setPresenceStatus(username_, raw);
            if (presence_view() != before) manager_.broadcast_presence_change(username_);
        }

        // --- AUTO-AWAY ---
        // No input for the client's idle threshold, a locked screen or
        // sleep. Session-only: turns an ONLINE user AWAY until activity.
        else if (packet.type() == chatproj::Packet::SET_IDLE_REQ) {
            if (!authenticated_ || !packet.has_set_idle_req()) return;
            if (!status_bucket_.take()) return;
            const bool idle = packet.set_idle_req().idle();
            if (idle == idle_) return;
            const PresenceView before = presence_view();
            idle_ = idle;
            if (presence_view() != before) manager_.broadcast_presence_change(username_);
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
                hb.name(), hb.description(), hb.host_ip(), hb.port(), hb.member_count(),
                hb.server_id(), hb.cert_fingerprint(), hb.public_listing());

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

        // --- SYNC_SERVER_PICTURE_REQ ---
        // Community proxies an owner-uploaded picture to central.
        // Owner verification happened on the community side; central
        // verifies the shared secret and writes to community_servers,
        // then broadcasts SERVER_PICTURE_CHANGED to every online
        // session whose username is a member of this server.
        else if (packet.type() == chatproj::Packet::SYNC_SERVER_PICTURE_REQ) {
            if (!auth_manager_.verifySharedSecret(packet.auth_token())) {
                std::cout << "[Security] Dropped sync_server_picture - invalid shared secret.\n";
                return;
            }
            const auto& req = packet.sync_server_picture_req();
            int server_id = auth_manager_.setServerPicture(
                req.host(), req.port(), req.data(), req.version(), req.server_id());

            if (server_id == 0) {
                std::cout << "[Server] Dropped sync_server_picture - unknown community "
                          << req.host() << ":" << req.port() << "\n";
                return;
            }

            auto members = auth_manager_.getServerMembers(server_id);
            chatproj::Packet bcast;
            bcast.set_type(chatproj::Packet::SERVER_PICTURE_CHANGED);
            auto* b = bcast.mutable_server_picture_changed();
            b->set_server_id(server_id);
            b->set_version(req.version());
            manager_.broadcast_to_users(bcast, members);

            std::cout << "[Server] Server picture updated for community " << server_id
                      << " (" << members.size() << " online members broadcast)\n";
        }

        // --- FETCH_SERVER_PICTURE_REQ ---
        // Any authenticated user can fetch any server's picture by id.
        // Matches the public-fetch model of FETCH_AVATAR_REQ — server
        // pictures are visible to every member of a community anyway.
        else if (packet.type() == chatproj::Packet::FETCH_SERVER_PICTURE_REQ) {
            if (!authenticated_) return;
            const auto& req = packet.fetch_server_picture_req();

            auto [version, data] = auth_manager_.getServerPicture(req.server_id());

            chatproj::Packet rsp;
            rsp.set_type(chatproj::Packet::FETCH_SERVER_PICTURE_RES);
            auto* res = rsp.mutable_fetch_server_picture_res();
            res->set_server_id(req.server_id());
            res->set_version(version);
            res->set_data(data);

            std::string serialized;
            rsp.SerializeToString(&serialized);
            auto framed = std::make_shared<std::vector<uint8_t>>(
                chatproj::create_framed_packet(serialized));
            deliver(framed);
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
                body->set_host(resolved->host);
                body->set_port(static_cast<uint32_t>(resolved->port));
                body->set_cert_fingerprint(resolved->cert_fingerprint);
                // Pre-join preview for invite cards (see the proto note).
                body->set_server_id(resolved->server_id);
                body->set_server_name(resolved->name);
                body->set_server_description(resolved->description);
                body->set_member_count(resolved->member_count);
                body->set_picture_version(resolved->picture_version);
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

    /// Central-originated CallSignal back to this session (PEER_OFFLINE /
    /// NOT_ALLOWED). Written from the recipient's point of view — `from`
    /// is the peer the request was addressed to, `to` is us — the same
    /// convention DM_MESSAGE_DELETED uses, so the client's call state
    /// machine keys every signal on `from` regardless of origin.
    void send_call_reply(chatproj::CallSignal::Kind kind, const std::string& call_id,
                         const std::string& peer) {
        chatproj::Packet pkt;
        pkt.set_type(chatproj::Packet::CALL_SIGNAL);
        auto* sig = pkt.mutable_call_signal();
        sig->set_kind(kind);
        sig->set_call_id(call_id);
        sig->set_from(peer);
        sig->set_to(username_);
        sig->set_timestamp(std::chrono::system_clock::to_time_t(
            std::chrono::system_clock::now()));
        std::string serialized;
        pkt.SerializeToString(&serialized);
        auto framed = std::make_shared<std::vector<uint8_t>>(
            chatproj::create_framed_packet(serialized));
        deliver(framed);
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
            // Auto-rejoin: on a successful login, ship the user's
            // community memberships inline so the client can start
            // auto-connecting without an extra round-trip. By this
            // point in the LOGIN_REQ handler, username_ has been set.
            if (success && !username_.empty()) {
                for (const auto& info : auth_manager_.getUserCommunities(username_)) {
                    *resp->add_memberships() = info;
                }
            }
            // P2P DM calls: advertise that this central relays CALL_SIGNAL
            // and hand out the operator's STUN list (may be empty → the
            // client falls back to its built-in default).
            if (success) {
                for (const auto& stun : g_stun_servers) {
                    resp->add_stun_servers(stun);
                }
                resp->set_call_signaling(true);
                // E2EE DMs: this build serves the key endpoints.
                resp->set_e2ee_keys(true);
                // User statuses: this build understands SET_STATUS_REQ /
                // SET_IDLE_REQ; echo the saved choice.
                resp->set_presence_statuses(true);
                resp->set_status(presence_status_);
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
    /// Mirrors users.dm_friends_only: loaded at login, written by DM_PRIVACY.
    bool dm_friends_only_ = false;
    /// The saved status choice (users.presence_status), loaded at login.
    chatproj::PresenceStatus presence_status_ = chatproj::PRESENCE_ONLINE;
    /// Auto-away: the client reported no input (SET_IDLE_REQ). Session only.
    bool idle_ = false;
    bool presence_deltas_ = false;
    /// What others can see of this session's presence; a change is what
    /// warrants a PresenceDelta. Status is only meaningful while visible.
    struct PresenceView {
        bool visible;
        chatproj::PresenceStatus status;
        bool operator!=(const PresenceView& o) const {
            return visible != o.visible || (visible && status != o.status);
        }
    };
    PresenceView presence_view() const { return {presence_visible(), effective_status()}; }
    RateBucket call_bucket_{20.0, 5.0};
    /// Same shape for the E2EE key endpoints (publish / fetch / backup):
    /// each is a synchronous DB round-trip on the io thread.
    RateBucket key_bucket_{20.0, 5.0};
    /// Status / idle / DM-privacy changes: each is a DB write or a fan-out
    /// to every session. A person picks a status a few times a day and
    /// auto-away flips at most every few minutes; 10 burst / 1 per 2 s
    /// never refuses a real client.
    RateBucket status_bucket_{10.0, 0.5};
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
    const bool was_visible = session->presence_visible();
    bool still_visible = false;

    {
        std::lock_guard<std::mutex> lock(mutex_);
        removed = sessions_.erase(session) > 0;
        total = sessions_.size();
        if (removed && was_visible) still_visible = user_visible_locked(user);
    }

    if (removed) {
        // Actually end the connection. Idempotent on an already-closed
        // socket (the read/write error paths arrive here too).
        session->close_connection();
        std::cout << "[Manager] Session left. Total: " << total << "\n";
        // Announce only what others could see: the user's last visible
        // session going away. An invisible user's disconnect says nothing,
        // and neither does the kick of a reconnecting user's old session.
        if (was_visible && !still_visible) {
            broadcast_presence_change(user);
        }
    }
}

bool SessionManager::user_visible_locked(const std::string& username) const {
    for (const auto& session : sessions_) {
        if (session->username() == username && session->presence_visible()) return true;
    }
    return false;
}

void SessionManager::fill_presence_snapshot_locked(chatproj::PresenceUpdate* presence) const {
    // Read each session's cached avatar_version (updated at login + on
    // every UPDATE_AVATAR_REQ) rather than hitting the DB per broadcast.
    // Invisible users are left out: to everyone else they are offline.
    for (const auto& session : sessions_) {
        if (!session->presence_visible()) continue;
        auto* entry = presence->add_users();
        entry->set_username(session->username());
        entry->set_avatar_version(session->avatar_version());
        entry->set_status(session->effective_status());
    }
}

void SessionManager::send_presence_snapshot(std::shared_ptr<Session> session) {
    chatproj::Packet packet;
    packet.set_type(chatproj::Packet::PRESENCE_UPDATE);
    {
        std::lock_guard<std::mutex> lock(mutex_);
        fill_presence_snapshot_locked(packet.mutable_presence_update());
    }
    std::string serialized;
    packet.SerializeToString(&serialized);
    session->deliver(std::make_shared<std::vector<uint8_t>>(
        chatproj::create_framed_packet(serialized)));
}

void SessionManager::broadcast_presence_change(const std::string& username) {
    // One PresenceDelta with the user's full current state: online with
    // their effective status if any visible session is theirs, else
    // offline. Older clients (no LoginRequest.presence_deltas) get the
    // full snapshot instead, as before. Authenticated sessions only — the
    // pre-login and community-sync connections never needed presence.
    std::lock_guard<std::mutex> lock(mutex_);

    chatproj::Packet delta;
    delta.set_type(chatproj::Packet::PRESENCE_DELTA);
    auto* d = delta.mutable_presence_delta();
    d->mutable_user()->set_username(username);
    for (const auto& session : sessions_) {
        if (session->username() == username && session->presence_visible()) {
            d->set_online(true);
            d->mutable_user()->set_avatar_version(session->avatar_version());
            d->mutable_user()->set_status(session->effective_status());
            break;
        }
    }
    std::string delta_ser;
    delta.SerializeToString(&delta_ser);
    auto delta_framed = std::make_shared<std::vector<uint8_t>>(
        chatproj::create_framed_packet(delta_ser));

    std::shared_ptr<std::vector<uint8_t>> snapshot_framed;
    for (auto& session : sessions_) {
        if (!session->authenticated()) continue;
        if (session->presence_deltas()) {
            session->deliver(delta_framed);
            continue;
        }
        if (!snapshot_framed) {
            chatproj::Packet snapshot;
            snapshot.set_type(chatproj::Packet::PRESENCE_UPDATE);
            fill_presence_snapshot_locked(snapshot.mutable_presence_update());
            std::string snapshot_ser;
            snapshot.SerializeToString(&snapshot_ser);
            snapshot_framed = std::make_shared<std::vector<uint8_t>>(
                chatproj::create_framed_packet(snapshot_ser));
        }
        session->deliver(snapshot_framed);
    }
}

bool SessionManager::is_user_visible(const std::string& username) {
    std::lock_guard<std::mutex> lock(mutex_);
    return user_visible_locked(username);
}

void SessionManager::kick_user(const std::string& username, std::shared_ptr<Session> except) {
    std::shared_ptr<Session> stale;
    {
        std::lock_guard<std::mutex> lock(mutex_);
        for (const auto& session : sessions_) {
            if (session != except && session->username() == username) {
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

void SessionManager::broadcast_to_users(const chatproj::Packet& packet,
                                          const std::vector<std::string>& usernames) {
    if (usernames.empty()) return;
    std::string serialized;
    packet.SerializeToString(&serialized);
    auto framed = std::make_shared<std::vector<uint8_t>>(
        chatproj::create_framed_packet(serialized));

    std::set<std::string> targets(usernames.begin(), usernames.end());
    std::lock_guard<std::mutex> lock(mutex_);
    for (auto& session : sessions_) {
        if (!session) continue;
        if (targets.count(session->username()) == 0) continue;
        session->deliver(framed);
    }
}

bool SessionManager::check_dm_allowed(const std::string& sender, const std::string& recipient, AuthManager& auth_manager) {
    // Blocking applies regardless of the recipient's online state or
    // friends-only setting — a blocked user must not DM the person who
    // blocked them, even into the offline queue. Checked before taking the
    // lock (isBlocked hits the DB) so it doesn't extend the mutex hold.
    if (auth_manager.isBlocked(sender, recipient)) {
        return false;
    }

    // Friends-only is saved on the account, so an online recipient (the
    // session's cached copy) and an offline or invisible one (read from
    // the DB) get the same answer — the result must never reveal whether
    // the recipient is connected.
    std::optional<bool> friends_only;
    {
        std::lock_guard<std::mutex> lock(mutex_);
        for (const auto& session : sessions_) {
            if (session->authenticated() && session->username() == recipient) {
                friends_only = session->dm_friends_only();
                break;
            }
        }
    }
    if (!friends_only) friends_only = auth_manager.getDmFriendsOnly(recipient);
    if (!*friends_only) {
        return true;
    }

    // Recipient only allows DMs from friends — check friend list (outside
    // the lock: getFriends is a DB round trip).
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
        const char* secret_env = std::getenv("DECIBELL_COMMUNITY_SECRET");
        const char* db_env = std::getenv("DECIBELL_DB_CONN");
        const char* key_env = std::getenv("DECIBELL_JWT_KEY_FILE");

        // The community secret must be non-empty: verifySharedSecret("")
        // would otherwise accept an empty community auth_token.
        if (!secret_env || secret_env[0] == '\0' || !db_env) {
            std::cerr << "Missing or empty required environment variables:\n";
            if (!secret_env || secret_env[0] == '\0')
                std::cerr << "  DECIBELL_COMMUNITY_SECRET (must be non-empty; shared with community servers,\n"
                             "                             authenticates their heartbeats / sync — NOT a signing key)\n";
            if (!db_env) std::cerr << "  DECIBELL_DB_CONN\n";
            return 1;
        }
        if (std::getenv("DECIBELL_JWT_SECRET")) {
            std::cerr << "[Auth] DECIBELL_JWT_SECRET is no longer used: JWTs are Ed25519-signed "
                         "(DECIBELL_JWT_KEY_FILE); community servers verify with the public key.\n";
        }

        // Ed25519 signing key (Theme A). Generated on first boot; the
        // public half is written next to it (<file>.pub) to hand to
        // community servers via DECIBELL_JWT_PUBLIC_KEY_FILE.
        const std::string key_file = key_env ? key_env : "jwt_ed25519.pem";
        std::string jwt_private_pem, jwt_public_pem;
        if (!chatproj::load_or_create_ed25519(key_file, jwt_private_pem, jwt_public_pem)) {
            std::cerr << "[Auth] Could not load or create the JWT signing key at " << key_file << "\n";
            return 1;
        }
        std::cout << "[Auth] JWT signing key: " << key_file
                  << " (public key: " << key_file << ".pub)\n";

        std::string community_secret = secret_env;
        std::string db_conn = db_env;

        // P2P DM calls: optional comma-separated STUN list ("host:port,...").
        // Whitespace around entries is ignored; empty → clients use their
        // built-in default.
        if (const char* stun_env = std::getenv("DECIBELL_STUN_SERVERS")) {
            std::string entry;
            std::string all = stun_env;
            all.push_back(',');
            for (char c : all) {
                if (c == ',') {
                    size_t b = entry.find_first_not_of(" \t");
                    size_t e = entry.find_last_not_of(" \t");
                    if (b != std::string::npos) {
                        g_stun_servers.push_back(entry.substr(b, e - b + 1));
                    }
                    entry.clear();
                } else {
                    entry.push_back(c);
                }
            }
        }
        if (g_stun_servers.empty()) {
            std::cout << "[Call] DECIBELL_STUN_SERVERS unset — clients use their built-in STUN list\n";
        } else {
            std::cout << "[Call] STUN servers: ";
            for (const auto& stun : g_stun_servers) std::cout << stun << " ";
            std::cout << "\n";
        }
        AuthManager auth_manager(community_secret, db_conn, jwt_private_pem, jwt_public_pem);

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