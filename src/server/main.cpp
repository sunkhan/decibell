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
#include <algorithm>
#include <chrono>
#include <cstdint>
#ifdef __linux__
#include <netinet/tcp.h>
#endif
#include <atomic>
#include <cctype>
#include <cerrno>
#include <csignal>
#include <cstring>
#include <thread>
#include <unordered_set>
#include <boost/asio.hpp>
#include <boost/asio/ssl.hpp>
#include <boost/asio/strand.hpp>
#include <boost/asio/thread_pool.hpp>
#ifndef _WIN32
#include <sys/resource.h>
#endif
#include "messages.pb.h"
#include "../common/net_utils.hpp"
#include "auth_utils.hpp"
#include "../common/ed25519_keys.hpp"
#include "session_manager.hpp"
#include "auth_manager.hpp"
#include "abuse_limits.hpp"

namespace ssl = boost::asio::ssl;
using boost::asio::ip::tcp;

/// STUN servers ("host:port") handed to every client in LOGIN_RES for
/// P2P DM calls. Filled once in main() from DECIBELL_STUN_SERVERS; empty
/// means "use the client's built-in default list".
static std::vector<std::string> g_stun_servers;

using chatproj::MAX_CLIENT_FRAME;
using chatproj::MAX_DM_CONTENT;  // the plaintext body cap (wire_limits.hpp)
using chatproj::MAX_RESPONSE_BYTES;

/// E2EE DM envelope cap: the edit-body cap (64 KiB) plus the sealed
/// header + tag. A client never produces more; anything bigger is junk.
constexpr size_t MAX_DM_ENVELOPE = 64 * 1024 + 64;
/// The client nonce (a UUID).
constexpr size_t MAX_DM_NONCE = 64;
/// LoginRequest.device_id (the client sends 32 hex characters).
constexpr size_t MAX_DEVICE_ID = 64;
/// A call candidate's address: the longest IPv6 literal is 45 bytes.
constexpr size_t MAX_CALL_CANDIDATE_IP = 45;
/// Identity-bundle field sizes (X25519 / Ed25519 public keys, Ed25519
/// signature) and the passphrase-wrapped backup blob cap.
constexpr size_t E2EE_PUB_LEN = 32;
constexpr size_t E2EE_SIG_LEN = 64;
constexpr size_t MAX_E2EE_BACKUP = 8 * 1024;

// --- Custom status messages (docs/superpowers/specs/2026-10-12-status-messages-design.md) ---
constexpr size_t STATUS_TEXT_MAX_CHARS = 128;
constexpr size_t STATUS_EMOJI_MAX_BYTES = 32;
constexpr int64_t STATUS_CLEAR_AFTER_MAX = 7 * 24 * 3600;

static int64_t unix_now() {
    return std::chrono::duration_cast<std::chrono::seconds>(
        std::chrono::system_clock::now().time_since_epoch()).count();
}

/// One line of display text: control characters and line / paragraph
/// separators become spaces, bidi embeddings / overrides / isolates and
/// malformed UTF-8 are dropped, the ends are trimmed, and at most
/// `max_chars` code points are kept. The client applies the same rules to
/// its own copy (presenceStore normalizeStatusText).
static std::string normalize_status_line(const std::string& in, size_t max_chars) {
    std::string out;
    out.reserve(std::min<size_t>(in.size(), max_chars * 4));
    size_t chars = 0;
    size_t i = 0;
    while (i < in.size() && chars < max_chars) {
        const unsigned char c = static_cast<unsigned char>(in[i]);
        size_t len = c < 0x80 ? 1 : (c >> 5) == 0x6 ? 2 : (c >> 4) == 0xE ? 3 : (c >> 3) == 0x1E ? 4 : 0;
        if (len == 0 || i + len > in.size()) { ++i; continue; }
        uint32_t cp = len == 1 ? c : (c & (0xFF >> (len + 1)));
        bool ok = true;
        for (size_t k = 1; k < len; ++k) {
            const unsigned char cc = static_cast<unsigned char>(in[i + k]);
            if ((cc & 0xC0) != 0x80) { ok = false; break; }
            cp = (cp << 6) | (cc & 0x3F);
        }
        if (!ok) { ++i; continue; }
        const bool space = cp <= 0x20 || cp == 0x7F || (cp >= 0x80 && cp < 0xA0) ||
                           cp == 0x2028 || cp == 0x2029;
        const bool drop = (cp >= 0x202A && cp <= 0x202E) || (cp >= 0x2066 && cp <= 0x2069);
        if (space) {
            if (!out.empty()) { out += ' '; ++chars; }
        } else if (!drop) {
            out.append(in, i, len);
            ++chars;
        }
        i += len;
    }
    while (!out.empty() && out.back() == ' ') out.pop_back();
    return out;
}

/// What a login reads (on the CPU pool for a password sign-in, inline for
/// a resume).
struct LoginOutcome {
    AuthManager::LoginResult login;
    AuthManager::PresenceProfile profile;
    std::vector<chatproj::CommunityServerInfo> memberships;
    std::string resume_token;  // '' if signing one failed (the client then signs in by password)
};

/// Lowercased, for keying failed sign-ins (usernames are lowercase).
static std::string account_key(const std::string& name) {
    std::string key = name.substr(0, 64);
    std::transform(key.begin(), key.end(), key.begin(),
                   [](unsigned char c) { return static_cast<char>(std::tolower(c)); });
    return key;
}

/// What a DM history request asks for (DM_HISTORY_REQ): an older page or
/// the latest (rows newest first), a newer page (oldest first), or a window
/// around one message (oldest first).
enum class DmPage { Older, Newer, Around };

/// Which rows of a DM history page go out, as [first, last) (review A1):
/// those the request asked for, cut where they would pass
/// MAX_RESPONSE_BYTES. `sizes` are the rows' encoded sizes in page order;
/// `anchor` is the target row of an Around window. A page keeps its first
/// row (the target) even alone, which always fits: the caps bound a row to
/// ~200 KiB (a 64 KiB body, two envelopes, a preview). Rows cut away set
/// has_more (older ones) or has_more_after (newer ones), so the client
/// pages on from the edge it got.
static std::pair<size_t, size_t> dm_page_slice(const std::vector<size_t>& sizes, DmPage mode, size_t anchor,
                                               bool& has_more, bool& has_more_after) {
    const size_t n = sizes.size();
    if (n == 0) return {0, 0};
    if (mode != DmPage::Around) {
        // The newest rows of an older page, the oldest of a newer one.
        size_t bytes = sizes[0], last = 1;
        while (last < n && bytes + sizes[last] <= MAX_RESPONSE_BYTES) bytes += sizes[last++];
        if (last < n) (mode == DmPage::Older ? has_more : has_more_after) = true;
        return {0, last};
    }
    // A window: from the target outwards, a row newer then a row older,
    // while they fit. A side stops at its first row that doesn't.
    size_t first = anchor, last = anchor + 1, bytes = sizes[anchor];
    bool newer = true, older = true;
    while (newer || older) {
        if (newer) {
            newer = last < n && bytes + sizes[last] <= MAX_RESPONSE_BYTES;
            if (newer) bytes += sizes[last++];
        }
        if (older) {
            older = first > 0 && bytes + sizes[first - 1] <= MAX_RESPONSE_BYTES;
            if (older) bytes += sizes[--first];
        }
    }
    if (first > 0) has_more = true;
    if (last < n) has_more_after = true;
    return {first, last};
}

static std::chrono::steady_clock::duration seconds_to_duration(double seconds) {
    return std::chrono::duration_cast<std::chrono::steady_clock::duration>(
        std::chrono::duration<double>(seconds));
}

class Session : public std::enable_shared_from_this<Session> {
public:
    /// `socket` comes from the acceptor on a fresh strand (docs/superpowers/
    /// specs/2026-10-15-central-scaling-design.md): every read and write
    /// of this session runs there, while other sessions run on the other io
    /// threads. Packet handlers run one at a time: database work on
    /// `db_pool` (on_packet), password hashing on `cpu_pool` (run_blocking).
    ///
    /// The acceptor admitted the connection against `limits` (one slot for
    /// `ip`); the session gives the slot back when it is destroyed.
    Session(tcp::socket socket, SessionManager& manager, ssl::context& context, AuthManager& auth_manager,
            boost::asio::thread_pool& cpu_pool, boost::asio::thread_pool& db_pool, AbuseLimits& limits,
            std::string ip)
        : socket_(std::move(socket), context), manager_(manager), auth_manager_(auth_manager),
          cpu_pool_(cpu_pool), db_pool_(db_pool), limits_(limits), ip_(std::move(ip)),
          pace_timer_(socket_.get_executor()) {
        touch();
        // Tighten TCP keepalive: 15s idle, 5s interval, 3 retries (~30s detection)
        boost::system::error_code ec;
        socket_.lowest_layer().set_option(boost::asio::socket_base::keep_alive(true), ec);
#ifdef __linux__
        int fd = socket_.lowest_layer().native_handle();
        int idle = 15, interval = 5, count = 3;
        setsockopt(fd, IPPROTO_TCP, TCP_KEEPIDLE, &idle, sizeof(idle));
        setsockopt(fd, IPPROTO_TCP, TCP_KEEPINTVL, &interval, sizeof(interval));
        setsockopt(fd, IPPROTO_TCP, TCP_KEEPCNT, &count, sizeof(count));
#endif
    }

    ~Session() { limits_.release_connection(ip_); }

    /// When the last packet arrived. Read by the stale sweep, any thread.
    std::chrono::steady_clock::time_point last_activity() const {
        return std::chrono::steady_clock::time_point(
            std::chrono::steady_clock::duration(last_activity_.load(std::memory_order_relaxed)));
    }

    void start() {
        auto self = shared_from_this();
        socket_.async_handshake(ssl::stream_base::server,
            [this, self](const boost::system::error_code& error) {
                if (!error) {
                    do_read_header();
                } else {
                    std::cerr << "[Session] TLS Handshake failed: " << error.message() << "\n";
                    manager_.leave(self);
                }
            });
    }

    /// Queue `frame` for this connection. Any thread: posts to the strand.
    void deliver(Frame frame) {
        auto self = shared_from_this();
        boost::asio::post(socket_.get_executor(), [this, self, frame = std::move(frame)]() mutable {
            enqueue(std::move(frame));
        });
    }

    /// Cancel in-flight I/O and close the socket, on the strand. Only
    /// SessionManager calls this (it forgets the session first, so a closed
    /// session never receives fan-out). Idempotent.
    void close() {
        auto self = shared_from_this();
        boost::asio::post(socket_.get_executor(), [this, self]() { shut(); });
    }

    /// Stop reading and close once what is queued has been written (a
    /// SESSION_CLOSED notice), or after 5 s if the peer doesn't read. Only
    /// SessionManager::retire calls this, after forgetting the session.
    void close_after_flush() {
        auto self = shared_from_this();
        boost::asio::post(socket_.get_executor(), [this, self]() {
            if (closed_ || closing_) return;
            if (write_queue_.empty()) {
                shut();
                return;
            }
            closing_ = true;
            // The pacing timer has nothing left to pace: a held request is
            // cancelled (its wait sees operation_aborted) and the timer
            // becomes the deadline.
            pace_timer_.expires_after(std::chrono::seconds(5));
            pace_timer_.async_wait([this, self](const boost::system::error_code& ec) {
                if (!ec) shut();
            });
        });
    }

private:
    /// Strand only: close now. The write queue stays until the session is
    /// destroyed: an in-flight write still references its front buffer.
    void shut() {
        if (closed_) return;
        closed_ = true;
        pace_timer_.cancel();
        boost::system::error_code ec;
        socket_.lowest_layer().cancel(ec);
        socket_.lowest_layer().close(ec);
    }

    void touch() {
        last_activity_.store(std::chrono::steady_clock::now().time_since_epoch().count(),
                             std::memory_order_relaxed);
    }

    void deliver(const chatproj::Packet& packet) { deliver(make_frame(packet)); }

    // --- Strand only below ---

    void enqueue(Frame frame) {
        if (closed_ || backlog_full_) return;
        // The client drops its connection over any frame past its cap and
        // asks again after reconnecting: an answer that big would lock the
        // user out. Every response is sized to fit (wire_limits.hpp); one
        // that doesn't is not sent, which leaves that one request unanswered
        // rather than the connection looping.
        if (frame->size() > 4 + MAX_CLIENT_FRAME) {
            std::cerr << "[Session] Not sending a " << frame->size() << "-byte frame (over the client's cap).\n";
            return;
        }
        // Cap the per-session backlog, in frames and in bytes. A client that
        // stops reading (or reads slowly) would otherwise let fan-out pile
        // up in memory without bound — and a few large replies (an avatar is
        // up to 1 MiB) reach gigabytes long before 1024 frames. Over either
        // cap, drop the connection (once: leave() closes it asynchronously).
        constexpr size_t MAX_QUEUED_MESSAGES = 1024;
        constexpr size_t MAX_QUEUED_BYTES = 16 * 1024 * 1024;
        if (write_queue_.size() >= MAX_QUEUED_MESSAGES ||
            queued_bytes_ + frame->size() > MAX_QUEUED_BYTES) {
            backlog_full_ = true;
            std::cout << "[Session] Write backlog full; closing connection.\n";
            manager_.leave(shared_from_this());
            return;
        }
        queued_bytes_ += frame->size();
        write_queue_.push_back(std::move(frame));
        if (write_queue_.size() == 1) do_write();
    }

    void do_write() {
        auto self = shared_from_this();
        boost::asio::async_write(socket_, boost::asio::buffer(*write_queue_.front()),
            [this, self](boost::system::error_code ec, std::size_t) {
                if (ec) {
                    manager_.leave(self);
                    return;
                }
                if (closed_) return;
                queued_bytes_ -= write_queue_.front()->size();
                write_queue_.pop_front();
                if (!write_queue_.empty()) do_write();
                else if (closing_) shut();  // retired: the notice is out
            });
    }

    void do_read_header() {
        if (closed_ || closing_) return;
        auto self = shared_from_this();
        boost::asio::async_read(socket_,
            boost::asio::buffer(inbound_header_, 4),
            [this, self](boost::system::error_code ec, std::size_t /*length*/) {
                if (ec) {
                    if (!closed_) std::cout << "[Session] Client disconnected: " << username_ << "\n";
                    manager_.leave(self);
                    return;
                }
                // memcpy rather than reinterpret_cast: reading a uint32_t
                // through a char[4] violates strict aliasing.
                uint32_t net_len;
                std::memcpy(&net_len, inbound_header_, 4);
                const uint32_t body_length = ntohl(net_len);
                if (body_length > 2 * 1024 * 1024) {
                    std::cout << "[Session] Oversized frame (" << body_length
                              << " bytes); closing connection.\n";
                    manager_.leave(self);
                    return;
                }
                inbound_body_.resize(body_length);
                do_read_body(body_length);
            });
    }

    void do_read_body(uint32_t length) {
        auto self = shared_from_this();
        boost::asio::async_read(socket_,
            boost::asio::buffer(inbound_body_.data(), length),
            [this, self](boost::system::error_code ec, std::size_t /*length*/) {
                if (ec) {
                    if (!closed_) std::cout << "[Session] Error in body read: " << username_ << "\n";
                    manager_.leave(self);
                    return;
                }
                touch();  // for stale session detection
                chatproj::Packet packet;
                const bool parsed = packet.ParseFromArray(inbound_body_.data(), static_cast<int>(inbound_body_.size()));
                // One avatar upload (up to 2 MB) must not stay allocated for
                // the rest of the connection.
                if (inbound_body_.capacity() > 64 * 1024) std::vector<uint8_t>().swap(inbound_body_);
                if (!parsed) {
                    do_read_header();
                    return;
                }
                on_packet(std::move(packet));
            });
    }

    /// Requests that draw on a per-connection rate: over it, the request
    /// waits (and reading pauses) until the bucket refills, so a burst is
    /// delayed, never dropped, and a flooding client is slowed to the rate.
    RateBucket* pace_bucket(chatproj::Packet::Type type) {
        using P = chatproj::Packet;
        switch (type) {
            case P::DIRECT_MSG:
            case P::DM_EDIT_REQ:
            case P::DM_DELETE_REQ:
                return &dm_bucket_;
            case P::DM_HISTORY_REQ:
            case P::DM_CONVERSATIONS_REQ:
            case P::DM_MARK_READ_REQ:
            case P::FETCH_AVATAR_REQ:
            case P::FETCH_SERVER_PICTURE_REQ:
            case P::FRIEND_LIST_REQ:
            case P::FRIENDS_IN_COMMUNITY_REQ:
            case P::SERVER_LIST_REQ:
            case P::INVITE_RESOLVE_REQ:
                return &read_bucket_;
            case P::FRIEND_ACTION_REQ:
            case P::UPDATE_AVATAR_REQ:
                return &write_bucket_;
            case P::LOGIN_REQ:
            case P::REGISTER_REQ:
                return &login_bucket_;
            // These used to be dropped over the rate (review B13): a lost
            // status / idle / privacy change left the account showing the
            // wrong state, a lost key fetch failed an MLS leaf check (a
            // quarantine) or a channel key fill, a lost call signal broke
            // the handshake, a lost subscription left stale presence.
            case P::SET_STATUS_REQ:
            case P::SET_IDLE_REQ:
            case P::SET_STATUS_MESSAGE_REQ:
            case P::DM_PRIVACY:
                return &status_bucket_;
            case P::E2EE_PUBLISH_KEYS_REQ:
            case P::E2EE_FETCH_KEYS_REQ:
            case P::E2EE_FETCH_BACKUP_REQ:
                return &key_bucket_;
            case P::CALL_SIGNAL:
                return &call_bucket_;
            case P::PRESENCE_SUBSCRIBE:
                return &watch_bucket_;
            default:
                return nullptr;
        }
    }

    /// Handlers that stay on the strand: the ones that hand work to the CPU
    /// pool (run_blocking, which resumes reading from its continuation) and
    /// the keepalive. Every other handler does database work and runs on the
    /// database pool. A handler added here must not be slow; one that calls
    /// run_blocking must be added here.
    static bool runs_on_strand(const chatproj::Packet& packet) {
        using P = chatproj::Packet;
        switch (packet.type()) {
            case P::CLIENT_PING:
            case P::REGISTER_REQ:
            case P::RENAME_REQ:
                return true;
            case P::LOGIN_REQ:
                return packet.login_req().resume_token().empty();  // a password sign-in
            default:
                return false;
        }
    }

    /// Handle `packet` once its rate bucket has a token (reading stays
    /// paused while it waits). Database work runs on the database pool
    /// (docs/superpowers/specs/2026-10-15-central-scaling-design.md): up to
    /// DECIBELL_DB_THREADS requests, from all sessions, wait on Postgres at
    /// once, so their commits share disk flushes, while the io threads keep
    /// serving sockets. Reading stays paused until the handler is done, so
    /// this connection's packets keep their order and its handlers never
    /// overlap; a handler only touches the session's handler state (never
    /// its socket or write queue: deliver() posts back to the strand).
    void on_packet(chatproj::Packet&& packet) {
        if (closing_) return;  // retired: a read that was in flight
        if (RateBucket* bucket = pace_bucket(packet.type()); bucket && !bucket->take()) {
            read_paused_ = true;
            auto held = std::make_shared<chatproj::Packet>(std::move(packet));
            auto self = shared_from_this();
            pace_timer_.expires_after(seconds_to_duration(bucket->wait()));
            pace_timer_.async_wait([this, self, held](const boost::system::error_code& ec) {
                if (ec || closed_) return;
                read_paused_ = false;
                on_packet(std::move(*held));
            });
            return;
        }
        if (runs_on_strand(packet)) {
            process_packet(packet);
            if (!read_paused_) do_read_header();
            return;
        }
        read_paused_ = true;
        auto held = std::make_shared<chatproj::Packet>(std::move(packet));
        auto self = shared_from_this();
        boost::asio::post(db_pool_, [this, self, held]() {
            try {
                process_packet(*held);
            } catch (const std::exception& e) {
                std::cerr << "[Session] Handler threw: " << e.what() << "\n";
            }
            boost::asio::post(socket_.get_executor(), [this, self]() {
                read_paused_ = false;
                do_read_header();
            });
        });
    }

    /// Run `work` on the CPU pool (password hashing), then `then(result)`
    /// back on this session's strand. Reading pauses until then, so this
    /// session's packets stay in order while the io threads serve everyone
    /// else. `work` returns a type whose default value means failure (it
    /// is what `then` gets if `work` throws).
    template <class Work, class Then>
    void run_blocking(Work work, Then then) {
        read_paused_ = true;
        auto self = shared_from_this();
        boost::asio::post(cpu_pool_, [self, work = std::move(work), then = std::move(then)]() mutable {
            decltype(work()) result{};
            try {
                result = work();
            } catch (const std::exception& e) {
                std::cerr << "[Session] Background work failed: " << e.what() << "\n";
            }
            boost::asio::post(self->socket_.get_executor(),
                [self, then = std::move(then), result = std::move(result)]() mutable {
                    then(std::move(result));
                    self->read_paused_ = false;
                    self->do_read_header();
                });
        });
    }

    /// The token every packet carries, verified once per token: a match
    /// with the last verified one only needs its expiry checked.
    bool token_ok(const std::string& token) {
        if (!verified_token_.empty() && token == verified_token_ && unix_now() < verified_token_expiry_) {
            return true;
        }
        const auto claims = auth_manager_.verifyToken(token);
        if (!claims) return false;
        verified_token_ = token;
        verified_token_expiry_ = claims->expires_at;
        return true;
    }

    void process_packet(const chatproj::Packet& packet) {
        // Client keepalive — no processing needed, touch() already updated timestamp
        if (packet.type() == chatproj::Packet::CLIENT_PING) {
            return;
        }

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

            if (!token_ok(packet.auth_token())) {
                std::cout << "[Security] Dropped packet - Missing or invalid JWT.\n";
                manager_.leave(shared_from_this());
                return;
            }
        }

        // Another device of this account renamed it: messages from here go
        // out under the new name from now on (multi-device design).
        if (authenticated_) {
            if (const uint64_t epoch = manager_.rename_epoch(); epoch != name_epoch_) {
                name_epoch_ = epoch;
                username_ = manager_.account_name(uid_, username_);
            }
        }

        // --- REGISTRATION ---
        // bcrypt (cost 12, ~0.25 s of CPU): on the CPU pool.
        if (packet.type() == chatproj::Packet::REGISTER_REQ) {
            const auto& req = packet.register_req();
            if (!limits_.take_signup(ip_)) {
                send_response(chatproj::Packet::REGISTER_RES, false,
                              "Too many sign-ups from your network. Try again later.");
                return;
            }
            run_blocking(
                [this, username = req.username(), email = req.email(), password = req.password()]() {
                    return std::optional<std::string>(auth_manager_.registerUser(username, email, password));
                },
                [this](std::optional<std::string> error) {
                    if (!error) {
                        send_response(chatproj::Packet::REGISTER_RES, false, "Registration failed. Please try again.");
                        return;
                    }
                    const bool success = error->empty();
                    send_response(chatproj::Packet::REGISTER_RES, success,
                                  success ? "Registration successful." : *error);
                });
        }

        // --- LOGIN ---
        // A resume token (a dropped connection coming back) costs a
        // signature check and a few reads, done here. A password sign-in's
        // bcrypt and reads run on the CPU pool; the session signs in on its
        // strand (finish_login). Password sign-ins are limited per address
        // and per account before any hashing.
        else if (packet.type() == chatproj::Packet::LOGIN_REQ) {
            if (authenticated_) return;
            const auto& req = packet.login_req();
            if (req.device_id().size() > MAX_DEVICE_ID) {
                send_response(chatproj::Packet::LOGIN_RES, false, "Invalid request.");
                return;
            }
            device_id_ = req.device_id();
            if (!req.resume_token().empty()) {
                std::optional<LoginOutcome> outcome;
                if (auto login = auth_manager_.resumeSession(req.resume_token())) {
                    outcome.emplace();
                    outcome->login = std::move(*login);
                    outcome->profile = auth_manager_.getPresenceProfile(outcome->login.uid);
                    outcome->memberships = auth_manager_.getUserCommunities(outcome->login.uid);
                    // Still valid until the expiry its password sign-in set.
                    outcome->resume_token = req.resume_token();
                }
                finish_login(std::move(outcome), "Your session has expired. Please sign in again.");
                return;
            }
            const std::string account = account_key(req.username());
            if (!limits_.take_password_attempt(ip_)) {
                send_response(chatproj::Packet::LOGIN_RES, false,
                              "Too many sign-in attempts from your network. Try again in a minute.");
                return;
            }
            if (limits_.account_blocked(account, ip_)) {
                send_response(chatproj::Packet::LOGIN_RES, false,
                              "Too many failed sign-ins for this account. Try again in a few minutes.");
                return;
            }
            // Set on the CPU pool, read in the completion (posted after it).
            auto server_error = std::make_shared<bool>(false);
            run_blocking(
                [this, username = req.username(), password = req.password(),
                 server_error]() -> std::optional<LoginOutcome> {
                    auto login = auth_manager_.authenticateUser(username, password, server_error.get());
                    if (!login) return std::nullopt;
                    LoginOutcome out;
                    out.login = std::move(*login);
                    out.profile = auth_manager_.getPresenceProfile(out.login.uid);
                    out.memberships = auth_manager_.getUserCommunities(out.login.uid);
                    out.resume_token = auth_manager_.mintResumeToken(out.login.uid, unix_now()).value_or("");
                    return out;
                },
                [this, account, server_error](std::optional<LoginOutcome> outcome) {
                    if (outcome) {
                        limits_.record_login(account, ip_);
                    } else if (!*server_error) {
                        limits_.record_failed_login(account, ip_);
                    }
                    // Clients forget a saved password only on the second
                    // message (native net/central.rs is_definitive_refusal),
                    // so a database outage must never answer with it.
                    finish_login(std::move(outcome), *server_error
                        ? "The server couldn't check your sign-in right now. Please try again."
                        : "Invalid username or password.");
                });
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

            // Built field by field, never copied from the request: the
            // client's Packet carries its session token in auth_token (and
            // could carry anything else), and this one goes to the
            // recipient. Only what a sender may choose crosses over;
            // central stamps the rest (the reply preview below included).
            const auto& req = packet.direct_msg();
            if (req.nonce().size() > MAX_DM_NONCE) return;
            chatproj::Packet routed_packet;
            routed_packet.set_type(chatproj::Packet::DIRECT_MSG);
            auto* dmsg = routed_packet.mutable_direct_msg();
            dmsg->set_recipient(req.recipient());
            dmsg->set_recipient_uid(req.recipient_uid());
            dmsg->set_content(req.content());
            dmsg->set_reply_to(req.reply_to());
            dmsg->set_nonce(req.nonce());
            dmsg->set_envelope(req.envelope());
            dmsg->set_sender(username_);
            dmsg->set_sender_uid(uid_);
            dmsg->set_timestamp(current_time);

            // E2EE: the sealed body rides `envelope`; central only caps
            // its size and stores/relays it opaquely. Plaintext gets the
            // same 64 KiB as an edit.
            if (dmsg->content().empty() && dmsg->envelope().empty()) return;
            if (dmsg->content().size() > MAX_DM_CONTENT || dmsg->envelope().size() > MAX_DM_ENVELOPE) {
                send_dm_refusal(*dmsg, "Message too long.");
                return;
            }

            // The recipient must be an account: by recipient_uid, or by the
            // typed name. Otherwise a message would be stored for a name
            // nobody holds yet, waiting for whoever registers it. Refused
            // with the sender's nonce, like the friends-only refusal below.
            const auto recipient = auth_manager_.resolveUser(dmsg->recipient_uid(), dmsg->recipient());
            if (!recipient.found()) {
                send_dm_refusal(*dmsg, recipient.uid == 0
                    ? "This user doesn't exist."
                    : "The server couldn't deliver your message. Please try again.");
                return;
            }
            // Self-DM guard. The DB schema allows self-rows, but the
            // UX doesn't make sense; reject explicitly so persistence
            // doesn't silently accumulate them.
            if (recipient.uid == uid_) {
                send_dm_refusal(*dmsg, "You can't message yourself.");
                return;
            }
            dmsg->set_recipient(recipient.username);
            dmsg->set_recipient_uid(recipient.uid);

            if (!manager_.check_dm_allowed(uid_, recipient.uid, auth_manager_)) {
                send_dm_refusal(*dmsg, "This user only accepts direct messages from users in their friends list.");
                return;
            }

            // Validate + resolve the reply parent within THIS conversation
            // before persisting — a forged reply_to must neither persist nor
            // leak another conversation's content. Not found → not a reply.
            // When valid, embed the parent's author + content so both sides
            // render the quoted preview without needing the parent loaded.
            if (dmsg->reply_to() > 0) {
                auto parent = auth_manager_.fetchDmPreview(uid_, recipient.uid, dmsg->reply_to());
                if (!parent) {
                    dmsg->set_reply_to(0);
                } else {
                    dmsg->set_reply_to_sender(parent->sender);
                    dmsg->set_reply_to_sender_uid(parent->sender_uid);
                    dmsg->set_reply_to_content(parent->content);
                    if (!parent->envelope.empty()) {
                        dmsg->set_reply_to_envelope(parent->envelope);
                        dmsg->set_reply_to_sealed_sender(parent->sent_sender);
                        dmsg->set_reply_to_sealed_recipient(parent->sent_recipient);
                    }
                }
            }

            // Persist before delivery. On DB failure, surface to
            // sender as a generic "couldn't deliver" — the message
            // is genuinely lost in that branch (rare).
            int64_t new_id = auth_manager_.insertDm(
                uid_, username_, recipient.uid, recipient.username,
                dmsg->content(), current_time, dmsg->reply_to(), dmsg->envelope());
            if (new_id == 0) {
                send_dm_refusal(*dmsg, "The server couldn't deliver your message. Please try again.");
                return;
            }

            // Stamp the persisted id onto the routed packet so the
            // client can use it as `up_to_id` in DmMarkReadReq.
            dmsg->set_id(new_id);

            // Best-effort live delivery — return value is informational
            // only. Recipient gets it now if online (every device), on next
            // login via DM_CONVERSATIONS_REQ / DM_HISTORY_REQ otherwise.
            const Frame framed = make_frame(routed_packet);
            manager_.deliver_to_user(recipient.uid, framed);

            // Echo to every device of the sender: this one settles its
            // bubble by the nonce (and learns the id), the others show the
            // message as sent from here.
            manager_.deliver_to_user(uid_, framed);
        }

        // --- CALL SIGNAL (P2P DM calls) ---
        // Ephemeral relay for the DM-call handshake: INVITE / RINGING /
        // ACCEPT / REJECT / BUSY / CANCEL / HANGUP / STREAM_START /
        // STREAM_STOP. Nothing is persisted; the DB is touched only to
        // resolve a peer named by name and for the policy checks: an
        // INVITE takes check_dm_allowed (same policy as a DM: blocked → no,
        // friends-only recipient → friends only), every other kind only
        // the block. Central stamps `from` so the peer never trusts a
        // client-supplied sender, and answers PEER_OFFLINE / NOT_ALLOWED
        // itself from the recipient's POV — to an INVITE only. Signals route
        // through the call registry (multi-device design §Calls): an INVITE
        // rings every device of the callee, the first to answer takes the
        // call, and the rest of it stays between those two connections.
        else if (packet.type() == chatproj::Packet::CALL_SIGNAL) {
            if (!authenticated_ || !packet.has_call_signal()) return;

            const auto& req = packet.call_signal();
            // PEER_OFFLINE / NOT_ALLOWED / *_ELSEWHERE are central's own.
            if (!chatproj::CallSignal::Kind_IsValid(req.kind()) ||
                req.kind() == chatproj::CallSignal::PEER_OFFLINE ||
                req.kind() == chatproj::CallSignal::NOT_ALLOWED ||
                req.kind() == chatproj::CallSignal::ANSWERED_ELSEWHERE ||
                req.kind() == chatproj::CallSignal::DECLINED_ELSEWHERE) {
                return;
            }
            // The peer by uid; only an INVITE may name someone typed.
            const bool invite = req.kind() == chatproj::CallSignal::INVITE;
            if (req.to_uid() <= 0 && (!invite || req.to().empty())) return;
            if (req.to().size() > 64 || req.call_id().size() > 64 ||
                req.pub_key().size() > 64 || req.pub_key_sig().size() > 64 ||
                req.candidates_size() > 16) {
                return;
            }
            for (const auto& c : req.candidates()) {
                if (c.ip().size() > MAX_CALL_CANDIDATE_IP) return;
            }
            const auto peer = auth_manager_.resolveUser(req.to_uid(), invite ? req.to() : std::string());
            if (!peer.found()) {
                // No such account: offline to the caller, as before.
                if (invite) {
                    send_call_reply(chatproj::CallSignal::PEER_OFFLINE, req.call_id(),
                                    req.to(), req.to_uid() > 0 ? req.to_uid() : 0);
                }
                return;
            }
            if (peer.uid == uid_) return;

            if (invite) {
                if (!manager_.check_dm_allowed(uid_, peer.uid, auth_manager_)) {
                    send_call_reply(chatproj::CallSignal::NOT_ALLOWED, req.call_id(), peer.username, peer.uid);
                    return;
                }
                // An invisible user looks offline to callers too (owner
                // decision). Only the INVITE: an invisible user can still
                // place calls, and the rest of that handshake must reach them.
                if (!manager_.is_user_visible(peer.uid)) {
                    send_call_reply(chatproj::CallSignal::PEER_OFFLINE, req.call_id(), peer.username, peer.uid);
                    return;
                }
            } else if (auth_manager_.isBlocked(uid_, peer.uid)) {
                // A block stops the whole handshake, not just the INVITE
                // (HANGUPs or stream notices at someone who blocked you).
                // Friends-only governs who may start a call; the rest goes
                // both ways, so it doesn't apply here.
                return;
            }

            // Built field by field, never copied from the request: the
            // client's Packet carries its session token in auth_token (and
            // could carry anything else), and this one goes to the peer.
            chatproj::Packet routed;
            routed.set_type(chatproj::Packet::CALL_SIGNAL);
            auto* sig = routed.mutable_call_signal();
            sig->set_kind(req.kind());
            sig->set_call_id(req.call_id());
            sig->set_pub_key(req.pub_key());
            sig->set_pub_key_sig(req.pub_key_sig());
            sig->set_key_id(req.key_id());
            for (const auto& c : req.candidates()) {
                auto* out = sig->add_candidates();
                out->set_socket(c.socket());
                out->set_kind(c.kind());
                out->set_ip(c.ip());
                out->set_port(c.port());
            }
            if (req.has_stream()) {
                auto* stream = sig->mutable_stream();
                stream->set_codec(req.stream().codec());
                stream->set_width(req.stream().width());
                stream->set_height(req.stream().height());
                stream->set_fps(req.stream().fps());
                stream->set_has_audio(req.stream().has_audio());
            }
            sig->set_to(peer.username);
            sig->set_to_uid(peer.uid);
            sig->set_from(username_);
            sig->set_from_uid(uid_);
            sig->set_timestamp(std::chrono::system_clock::to_time_t(
                std::chrono::system_clock::now()));

            // Only an undeliverable INVITE is answered. Answering the rest
            // would let anyone probe with a made-up call_id: an offline
            // user would answer PEER_OFFLINE, an invisible one silence. A
            // signal for no call between the two (or from a device not in
            // it) is dropped. A peer that drops mid-handshake is caught by
            // the call's own timeouts instead.
            const auto route = manager_.route_call(shared_from_this(), uid_, peer.uid, routed);
            if (route == SessionManager::CallRoute::offline && invite) {
                send_call_reply(chatproj::CallSignal::PEER_OFFLINE, req.call_id(), peer.username, peer.uid);
            }
        }

        // --- DM CONVERSATIONS REQ ---
        // Conversation previews + unread counts for the local user, one
        // page at a time (newest first; the client asks for the next page
        // with the last one's oldest last_message_id). Fired on login to
        // populate the DmSidebar cards.
        //
        // A page is at most DM_CONVERSATIONS_PAGE conversations and
        // MAX_RESPONSE_BYTES (review A1). A plaintext preview is cut to
        // DM_PREVIEW_MAX_BYTES; an encrypted last message's envelope can't
        // be cut (the client opens it for the preview line), so it goes
        // whole and counts toward the budget: a page of large encrypted
        // previews just holds fewer conversations. The caps bound one
        // preview to ~65 KiB, so a page always holds at least one.
        else if (packet.type() == chatproj::Packet::DM_CONVERSATIONS_REQ) {
            if (!authenticated_) return;
            const int64_t before_id = std::max<int64_t>(0, packet.dm_conversations_req().before_id());

            bool has_more = false;
            auto convs = auth_manager_.fetchDmConversations(uid_, before_id, has_more);

            chatproj::Packet response;
            response.set_type(chatproj::Packet::DM_CONVERSATIONS_RES);
            auto* res = response.mutable_dm_conversations_res();
            res->set_before_id(before_id);
            size_t bytes = 0;
            for (const auto& c : convs) {
                chatproj::DmConversationPreview preview;
                preview.set_peer(c.peer);
                preview.set_peer_uid(c.peer_uid);
                preview.set_last_message_content(c.last_message_content);
                preview.set_last_message_sender(c.last_message_sender);
                preview.set_last_message_sender_uid(c.last_message_sender_uid);
                preview.set_last_message_id(c.last_message_id);
                preview.set_last_timestamp(c.last_timestamp);
                preview.set_unread_count(c.unread_count);
                if (!c.last_message_envelope.empty()) {
                    preview.set_last_message_envelope(c.last_message_envelope);
                    preview.set_sealed_sender(c.last_sent_sender);
                    preview.set_sealed_recipient(c.last_sent_recipient);
                }
                const size_t size = preview.ByteSizeLong() + 4;  // + the field's tag and length
                if (res->conversations_size() > 0 && bytes + size > MAX_RESPONSE_BYTES) {
                    has_more = true;
                    break;
                }
                bytes += size;
                *res->add_conversations() = std::move(preview);
            }
            res->set_has_more(has_more);

            std::string s;
            response.SerializeToString(&s);
            deliver(std::make_shared<std::vector<uint8_t>>(
                chatproj::create_framed_packet(s)));
        }

        // --- DM HISTORY REQ ---
        // Paginated fetch of messages between the local user and the peer
        // (by peer_uid, or by name). before_id=0 returns the latest page;
        // client paginates upward by passing the oldest seen id.
        else if (packet.type() == chatproj::Packet::DM_HISTORY_REQ) {
            if (!authenticated_) return;
            const auto& req = packet.dm_history_req();
            if (req.peer_uid() <= 0) return;
            const auto peer = auth_manager_.resolveUser(req.peer_uid(), "");

            int32_t limit = req.limit();
            if (limit <= 0) limit = 50;
            if (limit > 200) limit = 200;

            // Route by request mode, mirroring the community channel-history
            // handler: around_id>0 → context window (replace on the client);
            // after_id>0 → newer page (append); else → older/most-recent page
            // (prepend). The DB helpers clamp their own limits. An unknown
            // peer gets an empty page.
            bool has_more = false;
            bool has_more_after = false;
            std::vector<AuthManager::DmHistoryRow> rows;
            if (peer.found() && req.around_id() > 0) {
                rows = auth_manager_.fetchDmHistoryAround(
                    uid_, peer.uid, req.around_id(), req.limit(),
                    has_more, has_more_after);
            } else if (peer.found() && req.after_id() > 0) {
                rows = auth_manager_.fetchDmHistoryAfter(
                    uid_, peer.uid, req.after_id(), req.limit(), has_more_after);
            } else if (peer.found()) {
                rows = auth_manager_.fetchDmHistory(
                    uid_, peer.uid, req.before_id(), limit, has_more);
            }

            std::vector<chatproj::DmHistoryMessage> messages;
            messages.reserve(rows.size());
            for (const auto& r : rows) {
                auto* msg = &messages.emplace_back();
                msg->set_id(r.id);
                msg->set_sender(r.sender);
                msg->set_sender_uid(r.sender_uid);
                msg->set_content(r.content);
                msg->set_timestamp(r.timestamp);
                msg->set_edited_at(r.edited_at);
                msg->set_reply_to(r.reply_to);
                msg->set_reply_to_sender(r.reply_to_sender);
                msg->set_reply_to_sender_uid(r.reply_to_sender_uid);
                msg->set_reply_to_content(r.reply_to_content);
                if (!r.envelope.empty()) {
                    msg->set_envelope(r.envelope);
                    msg->set_sealed_sender(r.sent_sender);
                    msg->set_sealed_recipient(r.sent_recipient);
                }
                if (!r.reply_to_envelope.empty()) {
                    msg->set_reply_to_envelope(r.reply_to_envelope);
                    msg->set_reply_to_sealed_sender(r.reply_to_sent_sender);
                    msg->set_reply_to_sealed_recipient(r.reply_to_sent_recipient);
                }
            }

            // The page within its byte budget (dm_page_slice).
            const DmPage mode = req.around_id() > 0 ? DmPage::Around
                              : req.after_id() > 0  ? DmPage::Newer
                                                    : DmPage::Older;
            std::vector<size_t> sizes;
            sizes.reserve(messages.size());
            size_t anchor = 0;
            for (size_t i = 0; i < messages.size(); ++i) {
                sizes.push_back(messages[i].ByteSizeLong() + 4);  // + the field's tag and length
                if (mode == DmPage::Around && messages[i].id() <= req.around_id()) anchor = i;
            }
            const auto [first, last] = dm_page_slice(sizes, mode, anchor, has_more, has_more_after);

            chatproj::Packet response;
            response.set_type(chatproj::Packet::DM_HISTORY_RES);
            auto* res = response.mutable_dm_history_res();
            res->set_peer(peer.found() ? peer.username : req.peer());
            res->set_peer_uid(peer.found() ? peer.uid : req.peer_uid());
            res->set_has_more(has_more);
            res->set_has_more_after(has_more_after);
            res->set_around_id(req.around_id());
            res->set_after_id(req.after_id());
            res->set_before_id(req.before_id());
            for (size_t i = first; i < last; ++i) *res->add_messages() = std::move(messages[i]);

            std::string s;
            response.SerializeToString(&s);
            deliver(std::make_shared<std::vector<uint8_t>>(
                chatproj::create_framed_packet(s)));
        }

        // --- DM MARK READ REQ ---
        // Fire-and-forget: update the read cursor so the next
        // DM_CONVERSATIONS_REQ surfaces the correct unread count. No
        // response — TCP delivery is the implicit ack. The account's other
        // devices clear their unread badge (DM_READ_SYNC).
        else if (packet.type() == chatproj::Packet::DM_MARK_READ_REQ) {
            if (!authenticated_) return;
            const auto& req = packet.dm_mark_read_req();
            if (req.peer_uid() <= 0) return;
            const auto peer = auth_manager_.resolveUser(req.peer_uid(), "");
            if (!peer.found()) return;
            auth_manager_.markDmRead(uid_, peer.uid, req.up_to_id());
            chatproj::Packet sync;
            sync.set_type(chatproj::Packet::DM_READ_SYNC);
            sync.mutable_dm_read_sync()->set_peer_uid(peer.uid);
            sync.mutable_dm_read_sync()->set_up_to_id(req.up_to_id());
            manager_.deliver_to_user_except(uid_, this, sync);
        }

        // --- DM_DELETE_REQ ---
        // Sender-only delete. The auth check happens inside the SQL
        // WHERE clause (sender_uid = uid_), so a forged packet with
        // someone else's message id is a no-op.
        else if (packet.type() == chatproj::Packet::DM_DELETE_REQ) {
            if (!authenticated_) return;
            const auto& req = packet.dm_delete_req();
            const auto peer = auth_manager_.resolveUser(req.peer_uid(), "");

            // Build RES skeleton early so failure branches can reuse.
            auto build_res = [&](bool success, const std::string& msg) {
                chatproj::Packet rsp;
                rsp.set_type(chatproj::Packet::DM_DELETE_RES);
                auto* res = rsp.mutable_dm_delete_res();
                res->set_success(success);
                res->set_message(msg);
                res->set_peer(peer.found() ? peer.username : req.peer());
                res->set_peer_uid(peer.found() ? peer.uid : req.peer_uid());
                res->set_message_id(req.message_id());
                std::string serialized;
                rsp.SerializeToString(&serialized);
                auto framed = std::make_shared<std::vector<uint8_t>>(
                    chatproj::create_framed_packet(serialized));
                deliver(framed);
            };

            if (!peer.found() || req.message_id() == 0) {
                build_res(false, "Invalid request.");
                return;
            }

            bool ok = auth_manager_.deleteDmMessage(uid_, peer.uid, req.message_id());

            build_res(ok, ok ? "" : "Message not found or not deletable.");

            if (!ok) return;

            // On success: broadcast DM_MESSAGE_DELETED to BOTH sessions.
            // peer field is rewritten per recipient so it's always "the
            // other user" from the receiving session's perspective.
            int64_t now_ts = static_cast<int64_t>(std::time(nullptr));

            // To every device of the sender (the requester's drives the
            // broadcast-handler dedupe with the optimistic remove).
            chatproj::Packet sender_bcast;
            sender_bcast.set_type(chatproj::Packet::DM_MESSAGE_DELETED);
            auto* sb = sender_bcast.mutable_dm_message_deleted();
            sb->set_peer(peer.username);   // from sender's POV, peer = recipient
            sb->set_peer_uid(peer.uid);
            sb->set_message_id(req.message_id());
            sb->set_deleted_at(now_ts);
            manager_.deliver_to_user(uid_, sender_bcast);

            // To the recipient (if online): peer = the sender.
            chatproj::Packet recv_bcast;
            recv_bcast.set_type(chatproj::Packet::DM_MESSAGE_DELETED);
            auto* rb = recv_bcast.mutable_dm_message_deleted();
            rb->set_peer(username_);       // from recipient's POV, peer = sender
            rb->set_peer_uid(uid_);
            rb->set_message_id(req.message_id());
            rb->set_deleted_at(now_ts);
            manager_.deliver_to_user(peer.uid, recv_bcast);
        }

        // --- DM_EDIT_REQ ---
        // Sender-only edit; the SQL WHERE clause is the authorization check.
        else if (packet.type() == chatproj::Packet::DM_EDIT_REQ) {
            if (!authenticated_) return;
            const auto& req = packet.dm_edit_req();
            const auto peer = auth_manager_.resolveUser(req.peer_uid(), "");

            auto build_res = [&](bool success, const std::string& msg) {
                chatproj::Packet rsp;
                rsp.set_type(chatproj::Packet::DM_EDIT_RES);
                auto* res = rsp.mutable_dm_edit_res();
                res->set_success(success);
                res->set_message(msg);
                res->set_peer(peer.found() ? peer.username : req.peer());
                res->set_peer_uid(peer.found() ? peer.uid : req.peer_uid());
                res->set_message_id(req.message_id());
                std::string serialized;
                rsp.SerializeToString(&serialized);
                deliver(std::make_shared<std::vector<uint8_t>>(
                    chatproj::create_framed_packet(serialized)));
            };

            if (!peer.found() || req.message_id() == 0) {
                build_res(false, "Invalid request.");
                return;
            }
            const std::string& content = req.content();
            const std::string& envelope = req.envelope();
            // An encrypted edit carries the placeholder in `content` and
            // the real body sealed in `envelope`; a plaintext edit has
            // no envelope. Either way something must be there.
            if (content.empty() && envelope.empty()) { build_res(false, "Message can't be empty."); return; }
            if (content.size() > MAX_DM_CONTENT) { build_res(false, "Message too long."); return; }
            if (envelope.size() > MAX_DM_ENVELOPE) { build_res(false, "Message too long."); return; }

            const int64_t now_ts = static_cast<int64_t>(std::time(nullptr));
            bool ok = auth_manager_.editDmMessage(
                uid_, peer.uid, req.message_id(), content, now_ts, envelope);
            build_res(ok, ok ? "" : "Message not found or not editable.");
            if (!ok) return;

            // Broadcast DM_MESSAGE_EDITED to both sessions, peer rewritten
            // per recipient (same convention as DM_MESSAGE_DELETED).
            chatproj::Packet sender_bcast;
            sender_bcast.set_type(chatproj::Packet::DM_MESSAGE_EDITED);
            auto* sb = sender_bcast.mutable_dm_message_edited();
            sb->set_peer(peer.username);   // sender's POV: peer = recipient
            sb->set_peer_uid(peer.uid);
            sb->set_message_id(req.message_id());
            sb->set_content(content);
            sb->set_edited_at(now_ts);
            sb->set_sender(username_);
            sb->set_sender_uid(uid_);
            if (!envelope.empty()) sb->set_envelope(envelope);
            manager_.deliver_to_user(uid_, sender_bcast);  // every device of the editor

            chatproj::Packet recv_bcast;
            recv_bcast.set_type(chatproj::Packet::DM_MESSAGE_EDITED);
            auto* rb = recv_bcast.mutable_dm_message_edited();
            rb->set_peer(username_);       // recipient's POV: peer = sender
            rb->set_peer_uid(uid_);
            rb->set_message_id(req.message_id());
            rb->set_content(content);
            rb->set_edited_at(now_ts);
            rb->set_sender(username_);
            rb->set_sender_uid(uid_);
            if (!envelope.empty()) rb->set_envelope(envelope);
            manager_.deliver_to_user(peer.uid, recv_bcast);
        }

        // --- E2EE: PUBLISH KEYS ---
        // Store the caller's identity bundle (central assigns key_id)
        // and/or replace their passphrase-wrapped backup. Central never
        // interprets the bytes beyond size checks. A new bundle is
        // announced to every session like an avatar change.
        else if (packet.type() == chatproj::Packet::E2EE_PUBLISH_KEYS_REQ) {
            if (!authenticated_) return;
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
            if (has_bundle) {
                const auto& b = req.bundle();
                if (b.dh_pub().size() != E2EE_PUB_LEN || b.sign_pub().size() != E2EE_PUB_LEN ||
                    b.signature().size() != E2EE_SIG_LEN || b.binding() > 2) {
                    reply(false, "Malformed key bundle.", 0);
                    return;
                }
            }
            // What the signature binds: 1 = the name (0 from older
            // clients), 2 = the uid (uid design, stage 4).
            const int binding = has_bundle && req.bundle().binding() == 2 ? 2 : 1;

            // Rebind: the current bundle re-signed over the uid. Same keys,
            // same key_id; only the signature and binding change.
            if (req.rebind()) {
                const auto& b = req.bundle();
                if (!has_bundle || !backup.empty() || b.key_id() == 0) {
                    reply(false, "Malformed rebind.", 0);
                    return;
                }
                if (!auth_manager_.rebindE2eeKeys(uid_, b.key_id(), b.dh_pub(), b.sign_pub(),
                                                  b.signature(), binding, username_)) {
                    auto cur = auth_manager_.getE2eeKeys(uid_, 0);
                    reply(false, "Those aren't your current keys.", cur ? cur->key_id : 0);
                    return;
                }
                reply(true, "", b.key_id());
                manager_.push_e2ee_keys_changed(uid_, b.key_id(), this);
                return;
            }

            const int64_t now_ts = static_cast<int64_t>(std::time(nullptr));
            uint32_t key_id = 0;
            if (has_bundle) {
                const auto& b = req.bundle();
                key_id = auth_manager_.publishE2eeKeys(
                    uid_, username_, b.dh_pub(), b.sign_pub(), b.signature(), binding, now_ts);
                if (key_id == 0) { reply(false, "Storage error.", 0); return; }
            } else {
                // Backup-only update (passphrase change): it belongs to
                // the current bundle.
                auto cur = auth_manager_.getE2eeKeys(uid_, 0);
                if (!cur) { reply(false, "No published keys to back up.", 0); return; }
                key_id = cur->key_id;
            }
            if (!backup.empty() &&
                !auth_manager_.setE2eeBackup(uid_, key_id, backup, now_ts)) {
                reply(false, "Storage error.", key_id);
                return;
            }
            reply(true, "", key_id);
            if (has_bundle) {
                manager_.push_e2ee_keys_changed(uid_, key_id, this);
            }
        }

        // --- RENAME (uid design, stage 5) ---
        // Password-confirmed. The client hands the fresh token to each
        // community (SESSION_TOKEN_UPDATE), which renames the live session in
        // place. The account's other devices hear USER_RENAMED (and ask for
        // a fresh token of their own); their sessions here pick the name up
        // through the rename epoch.
        else if (packet.type() == chatproj::Packet::RENAME_REQ) {
            if (!authenticated_) return;
            if (!rename_bucket_.take()) {
                send_rename_res(false, "Too many attempts. Try again in a minute.", "");
                return;
            }
            const auto& req = packet.rename_req();
            if (req.new_username().size() > 64 || req.password().size() > 1024) {
                send_rename_res(false, "Invalid request.", "");
                return;
            }
            // The password check (bcrypt) runs on the CPU pool.
            run_blocking(
                [this, uid = uid_, name = req.new_username(), password = req.password()]() {
                    return std::optional<AuthManager::RenameResult>(auth_manager_.renameUser(uid, name, password));
                },
                [this](std::optional<AuthManager::RenameResult> r) {
                    if (!r) {
                        send_rename_res(false, "Couldn't change your username. Please try again.", "");
                        return;
                    }
                    if (!r->error.empty()) {
                        send_rename_res(false, r->error, "");
                        return;
                    }
                    username_ = r->username;
                    std::cout << "[Rename] uid " << uid_ << ": " << r->old_username << " -> " << username_ << "\n";
                    send_rename_res(true, r->token.empty() ? "Renamed; sign in again." : "", r->token);
                    // Invisible users' too (owner, 2026-10-15): profile pushes
                    // (renames, avatars, E2EE keys) may reveal activity;
                    // clients need them.
                    manager_.push_user_renamed(uid_, username_, this);
                });
        }

        // --- TOKEN REFRESH ---
        // A fresh 24 h token for this session's account. Clients renew
        // before expiry (every packet's token is checked) and pass the new
        // one to their communities.
        else if (packet.type() == chatproj::Packet::TOKEN_REFRESH_REQ) {
            if (!authenticated_) return;
            chatproj::Packet rsp;
            rsp.set_type(chatproj::Packet::TOKEN_REFRESH_RES);
            auto* res = rsp.mutable_token_refresh_res();
            // Over the rate: answered, but with no token (review B13). Not
            // paced: a wait of up to a minute would outlast the stale sweep.
            const bool allowed = refresh_bucket_.take();
            auto token = allowed ? auth_manager_.mintToken(uid_, username_) : std::nullopt;
            res->set_success(token.has_value());
            if (token) res->set_jwt_token(*token);
            res->set_username(username_);
            std::string serialized;
            rsp.SerializeToString(&serialized);
            deliver(std::make_shared<std::vector<uint8_t>>(
                chatproj::create_framed_packet(serialized)));
        }

        // --- E2EE: FETCH KEYS ---
        // Any authenticated user may read anyone's public bundle (that is
        // the point of a public key); key_id 0 = current. Paced (key_bucket_):
        // each lookup is a database round trip.
        else if (packet.type() == chatproj::Packet::E2EE_FETCH_KEYS_REQ) {
            if (!authenticated_) return;
            const auto& req = packet.e2ee_fetch_keys_req();
            if (req.uid() <= 0 && (req.username().empty() || req.username().size() > 64)) return;
            const auto user = auth_manager_.resolveUser(req.uid(), req.username());

            chatproj::Packet rsp;
            rsp.set_type(chatproj::Packet::E2EE_FETCH_KEYS_RES);
            auto* res = rsp.mutable_e2ee_fetch_keys_res();
            res->set_username(user.found() ? user.username : req.username());
            res->set_uid(user.found() ? user.uid : req.uid());
            res->set_key_id(req.key_id());
            std::optional<AuthManager::E2eeKeyRow> row;
            if (user.found()) row = auth_manager_.getE2eeKeys(user.uid, req.key_id());
            res->set_found(row.has_value());
            if (row) {
                auto* b = res->mutable_bundle();
                b->set_username(user.username);
                b->set_uid(user.uid);
                b->set_signed_name(row->signed_name);
                b->set_binding(static_cast<uint32_t>(row->binding));
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

            chatproj::Packet rsp;
            rsp.set_type(chatproj::Packet::E2EE_FETCH_BACKUP_RES);
            auto* res = rsp.mutable_e2ee_fetch_backup_res();
            auto row = auth_manager_.getE2eeBackup(uid_);
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
            // The target by uid; only ADD takes a typed name.
            const auto target = auth_manager_.resolveUser(
                req.target_uid(), req.action() == chatproj::ADD ? req.target_username() : std::string());

            std::string error_msg = target.found()
                ? auth_manager_.handleFriendAction(uid_, req.action(), target.uid)
                : target.uid == 0 ? "User not found." : "Database error.";
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
                // Send updated list to the requester, on every device
                auto my_friends = auth_manager_.getFriends(uid_);
                chatproj::Packet my_list_pkt;
                my_list_pkt.set_type(chatproj::Packet::FRIEND_LIST_RES);
                auto* my_list = my_list_pkt.mutable_friend_list_res();
                for (auto& f : my_friends) {
                    if (f.status() == chatproj::FriendInfo::OFFLINE && manager_.is_user_visible(f.uid())) {
                        f.set_status(chatproj::FriendInfo::ONLINE);
                    }
                    *my_list->add_friends() = f;
                }
                manager_.deliver_to_user(uid_, my_list_pkt);

                // Send updated list to the target user (if online)
                auto target_friends = auth_manager_.getFriends(target.uid);
                chatproj::Packet target_list_pkt;
                target_list_pkt.set_type(chatproj::Packet::FRIEND_LIST_RES);
                auto* target_list = target_list_pkt.mutable_friend_list_res();
                for (auto& f : target_friends) {
                    if (f.status() == chatproj::FriendInfo::OFFLINE && manager_.is_user_visible(f.uid())) {
                        f.set_status(chatproj::FriendInfo::ONLINE);
                    }
                    *target_list->add_friends() = f;
                }
                manager_.deliver_to_user(target.uid, target_list_pkt);
            }
        }
        else if (packet.type() == chatproj::Packet::FRIEND_LIST_REQ) {
            if (!authenticated_) return;

            auto friends = auth_manager_.getFriends(uid_);

            chatproj::Packet res_packet;
            res_packet.set_type(chatproj::Packet::FRIEND_LIST_RES);
            auto* res = res_packet.mutable_friend_list_res();

            for (auto& f : friends) {
                // Determine online presence if they are ACCEPTED
                if (f.status() == chatproj::FriendInfo::OFFLINE) {
                    if (manager_.is_user_visible(f.uid())) {
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

        // "Invite friends": which of my friends are already in this community.
        // Central answers from its membership table, so the friend list never
        // reaches the community (owner rule).
        else if (packet.type() == chatproj::Packet::FRIENDS_IN_COMMUNITY_REQ) {
            if (!authenticated_) return;
            const int64_t server_id = packet.friends_in_community_req().server_id();
            chatproj::Packet res_packet;
            res_packet.set_type(chatproj::Packet::FRIENDS_IN_COMMUNITY_RES);
            auto* res = res_packet.mutable_friends_in_community_res();
            res->set_server_id(server_id);
            if (auto uids = auth_manager_.friendsInCommunity(uid_, server_id)) {
                res->set_success(true);
                for (int64_t u : *uids) res->add_member_uids(u);
            }
            std::string serialized;
            res_packet.SerializeToString(&serialized);
            deliver(std::make_shared<std::vector<uint8_t>>(chatproj::create_framed_packet(serialized)));
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
                version = auth_manager_.setAvatar(uid_, data);
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

            res->set_success(true);
            res->set_version(version);
            std::string s;
            response.SerializeToString(&s);
            deliver(std::make_shared<std::vector<uint8_t>>(
                chatproj::create_framed_packet(s)));

            manager_.push_avatar_changed(uid_, version, this);
        }

        // --- AVATAR FETCH ---
        // Authenticated callers can fetch anyone's avatar. Missing users
        // or missing avatars both surface as empty version + empty data.
        else if (packet.type() == chatproj::Packet::FETCH_AVATAR_REQ) {
            if (!authenticated_) return;
            const auto& req = packet.fetch_avatar_req();
            const auto target = auth_manager_.resolveUser(req.uid(), "");

            std::string version, data;
            if (target.found()) std::tie(version, data) = auth_manager_.getAvatar(target.uid);

            chatproj::Packet response;
            response.set_type(chatproj::Packet::FETCH_AVATAR_RES);
            auto* res = response.mutable_fetch_avatar_res();
            res->set_username(target.found() ? target.username : req.username());
            res->set_uid(target.found() ? target.uid : req.uid());
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
            const bool friends_only = packet.dm_privacy().friends_only();
            bool changed = false;
            manager_.update_presence(
                uid_,
                [&](Presence& p) {
                    changed = p.dm_friends_only != friends_only;
                    p.dm_friends_only = friends_only;
                },
                [&](const Presence& p) {
                    if (changed) auth_manager_.setDmFriendsOnly(uid_, p.dm_friends_only);
                });
        }

        // --- USER STATUS ---
        // The chosen status (online / away / dnd / invisible): saved on the
        // account and applied to this session. Others hear about it only
        // when what they can see changes.
        else if (packet.type() == chatproj::Packet::SET_STATUS_REQ) {
            if (!authenticated_ || !packet.has_set_status_req()) return;
            const int raw = static_cast<int>(packet.set_status_req().status());
            if (!chatproj::PresenceStatus_IsValid(raw)) return;
            const auto status = static_cast<chatproj::PresenceStatus>(raw);
            bool changed = false;
            manager_.update_presence(
                uid_,
                [&](Presence& p) {
                    changed = p.status != status;
                    p.status = status;
                },
                [&](const Presence& p) {
                    if (changed) auth_manager_.setPresenceStatus(uid_, static_cast<int>(p.status));
                });
        }

        // --- AUTO-AWAY ---
        // No input for the client's idle threshold, a locked screen or
        // sleep. Per connection, not saved: an ONLINE user shows AWAY while
        // every device of theirs is idle.
        else if (packet.type() == chatproj::Packet::SET_IDLE_REQ) {
            if (!authenticated_ || !packet.has_set_idle_req()) return;
            const bool idle = packet.set_idle_req().idle();
            manager_.set_idle(shared_from_this(), idle);
        }

        // --- CUSTOM STATUS MESSAGE ---
        // Text + emoji shown under the user's name, saved on the account
        // and cleared by the sweep after the chosen time. Others see it
        // with the presence, i.e. only while the user is visible.
        else if (packet.type() == chatproj::Packet::SET_STATUS_MESSAGE_REQ) {
            if (!authenticated_ || !packet.has_set_status_message_req()) return;
            const auto& req = packet.set_status_message_req();
            std::string text = normalize_status_line(req.text(), STATUS_TEXT_MAX_CHARS);
            std::string emoji = normalize_status_line(req.emoji(), STATUS_EMOJI_MAX_BYTES);
            if (emoji.size() > STATUS_EMOJI_MAX_BYTES) emoji.clear();
            const int64_t clear_after =
                std::clamp<int64_t>(req.clear_after(), 0, STATUS_CLEAR_AFTER_MAX);
            const bool empty = text.empty() && emoji.empty();
            const int64_t expires_at = empty || clear_after == 0 ? 0 : unix_now() + clear_after;
            bool changed = false;
            manager_.update_presence(
                uid_,
                [&](Presence& p) {
                    if (p.status_text == text && p.status_emoji == emoji && p.status_expires_at == expires_at) return;
                    p.status_text = text;
                    p.status_emoji = emoji;
                    p.status_expires_at = expires_at;
                    changed = true;
                },
                [&](const Presence& p) {
                    if (changed) auth_manager_.setStatusMessage(uid_, p.status_text, p.status_emoji, p.status_expires_at);
                });
        }

        // --- PRESENCE SUBSCRIPTIONS ---
        // The accounts this client is showing (central-scaling-design.md):
        // presence and profile pushes go to watchers only. Each request
        // replaces the set; the newly watched are answered at once.
        else if (packet.type() == chatproj::Packet::PRESENCE_SUBSCRIBE) {
            if (!authenticated_ || !packet.has_presence_subscribe()) return;
            const auto& uids = packet.presence_subscribe().uids();
            const auto n = std::min<size_t>(static_cast<size_t>(uids.size()), SessionManager::MAX_WATCH);
            manager_.watch(shared_from_this(), uid_, std::vector<int64_t>(uids.begin(), uids.begin() + n),
                           auth_manager_);
        }

        // --- COMMUNITY SERVER HEARTBEAT ---
        else if (packet.type() == chatproj::Packet::SERVER_HEARTBEAT) {
            if (!auth_manager_.verifySharedSecret(packet.auth_token())) {
                std::cout << "[Security] Dropped heartbeat - invalid shared secret.\n";
                return;
            }
            auto& hb = packet.server_heartbeat();
            // Clamped as the community clamps them: they reach clients in the
            // directory, every member's LOGIN_RES and invite previews (review
            // A1), and a name over the column's 64 characters used to fail
            // the whole heartbeat.
            const std::string name = chatproj::clamp_display(hb.name(), chatproj::MAX_COMMUNITY_NAME);
            const std::string description =
                chatproj::clamp_display(hb.description(), chatproj::MAX_COMMUNITY_DESCRIPTION);
            std::cout << "[Server] Heartbeat from community server: " << name << " at " << hb.host_ip() << ":" << hb.port() << "\n";
            int server_id = auth_manager_.upsertCommunityServer(
                name, description, hb.host_ip(), hb.port(), hb.member_count(),
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
            if (req.uid() <= 0 || req.server_id() == 0) return;
            // A membership the account didn't have: one of its devices just
            // joined, and the others connect too (multi-device design).
            if (auto joined = auth_manager_.registerMembership(req.uid(), req.server_id())) {
                chatproj::Packet added;
                added.set_type(chatproj::Packet::MEMBERSHIP_ADDED);
                *added.mutable_membership_added()->mutable_server() = std::move(*joined);
                manager_.deliver_to_user(req.uid(), added);
            }
        }

        // --- AUTO-REJOIN: REVOKE A MEMBERSHIP (dual-origin) ---
        // Two callers can revoke: community server (shared-secret,
        // kick/ban/leave path) or JWT-authed client (stale-membership
        // cleanup). Community can revoke any user (by uid); client can
        // only revoke its own (the session's uid).
        else if (packet.type() == chatproj::Packet::MEMBERSHIP_REVOKE_REQ) {
            const auto& req = packet.membership_revoke_req();
            if (req.server_id() == 0) return;

            int64_t target_uid = 0;
            if (auth_manager_.verifySharedSecret(packet.auth_token())) {
                target_uid = req.uid();
                if (target_uid <= 0) return;
            } else if (authenticated_) {
                target_uid = uid_;
            } else {
                std::cout << "[Security] Dropped membership_revoke - no valid auth.\n";
                return;
            }
            auth_manager_.revokeMembership(target_uid, req.server_id());
        }

        // --- SYNC_SERVER_PICTURE_REQ ---
        // Community proxies an owner-uploaded picture to central.
        // Owner verification happened on the community side; central
        // verifies the shared secret and writes to community_servers,
        // then broadcasts SERVER_PICTURE_CHANGED to every online
        // session of a member of this server.
        else if (packet.type() == chatproj::Packet::SYNC_SERVER_PICTURE_REQ) {
            if (!auth_manager_.verifySharedSecret(packet.auth_token())) {
                std::cout << "[Security] Dropped sync_server_picture - invalid shared secret.\n";
                return;
            }
            const auto& req = packet.sync_server_picture_req();
            // The community's own upload cap; a version is a sha256 hex
            // digest. Both go out to every member (review A1).
            if (req.data().size() > AuthManager::MAX_SERVER_PICTURE || req.version().size() > 64) {
                std::cout << "[Server] Dropped sync_server_picture - picture or version too large.\n";
                return;
            }
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
            manager_.deliver_to_users(members, bcast);

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
            // Codes are stored as VARCHAR(32): a longer one matches nothing.
            auto resolved = req.code().size() <= 32 ? auth_manager_.resolveCommunityInvite(req.code())
                                                    : std::nullopt;

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

    void send_rename_res(bool success, const std::string& msg, const std::string& token) {
        chatproj::Packet rsp;
        rsp.set_type(chatproj::Packet::RENAME_RES);
        auto* res = rsp.mutable_rename_res();
        res->set_success(success);
        res->set_message(msg);
        res->set_username(username_);
        res->set_jwt_token(token);
        deliver(rsp);
    }

    /// Central-originated CallSignal back to this session (PEER_OFFLINE /
    /// NOT_ALLOWED). Written from the recipient's point of view — `from`
    /// is the peer the request was addressed to, `to` is us — the same
    /// convention DM_MESSAGE_DELETED uses, so the client's call state
    /// machine keys every signal on `from` regardless of origin.
    void send_call_reply(chatproj::CallSignal::Kind kind, const std::string& call_id,
                         const std::string& peer, int64_t peer_uid) {
        chatproj::Packet pkt;
        pkt.set_type(chatproj::Packet::CALL_SIGNAL);
        auto* sig = pkt.mutable_call_signal();
        sig->set_kind(kind);
        sig->set_call_id(call_id);
        sig->set_from(peer);
        sig->set_from_uid(peer_uid);
        sig->set_to(username_);
        sig->set_to_uid(uid_);
        sig->set_timestamp(std::chrono::system_clock::to_time_t(
            std::chrono::system_clock::now()));
        std::string serialized;
        pkt.SerializeToString(&serialized);
        auto framed = std::make_shared<std::vector<uint8_t>>(
            chatproj::create_framed_packet(serialized));
        deliver(framed);
    }

    /// A DM central refused, back to its sender: a self-addressed
    /// DIRECT_MSG with no id, `text` as the content and the request's
    /// nonce, so the client withdraws exactly that pending bubble and
    /// shows the text.
    void send_dm_refusal(const chatproj::DirectMessage& req, const std::string& text) {
        chatproj::Packet error_packet;
        error_packet.set_type(chatproj::Packet::DIRECT_MSG);
        auto* err_msg = error_packet.mutable_direct_msg();
        err_msg->set_sender(username_);
        err_msg->set_sender_uid(uid_);
        err_msg->set_recipient(req.recipient());
        err_msg->set_recipient_uid(req.recipient_uid());
        err_msg->set_content(text);
        err_msg->set_timestamp(unix_now());
        err_msg->set_nonce(req.nonce());

        std::string serialized;
        error_packet.SerializeToString(&serialized);
        deliver(std::make_shared<std::vector<uint8_t>>(chatproj::create_framed_packet(serialized)));
    }

    /// Sign in with what a password check or a resume found, or refuse with
    /// `refusal`.
    void finish_login(std::optional<LoginOutcome> outcome, const char* refusal) {
        if (!outcome) {
            send_response(chatproj::Packet::LOGIN_RES, false, refusal);
            return;
        }
        const auto& p = outcome->profile;
        Presence presence;
        presence.username = outcome->login.username;
        presence.avatar_version = p.avatar_version;
        presence.status = p.status;
        presence.status_text = p.status_text;
        presence.status_emoji = p.status_emoji;
        presence.status_expires_at = p.status_expires_at;
        presence.dm_friends_only = p.dm_friends_only;
        // Sign in only once the account is proven, so an unauthenticated
        // client can't evict anyone. Other devices stay signed in
        // (docs/superpowers/specs/2026-10-18-multi-device-design.md); the
        // same device's previous connection (a dead one, or a copy of the
        // install) is replaced. The saved status applies before anyone hears
        // of the login, so an invisible user never flashes online.
        // Read before bind: a rename landing after it bumps the epoch past
        // this, and the next packet picks the name up.
        const uint64_t epoch = manager_.rename_epoch();
        // The reply goes out from inside bind, under the directory's lock, so
        // nothing another device changes reaches this connection before it.
        // Built here from the login's own read; bind rebuilds it only when
        // the account is signed in elsewhere with a different state.
        const Frame reply = login_frame(*outcome, presence);
        const LoginOutcome& login = *outcome;
        auto bound = manager_.bind(shared_from_this(), outcome->login.uid, device_id_, std::move(presence), reply,
                                   [&login](const Presence& live) { return login_frame(login, live); });
        if (bound.full) {
            // Never by evicting another device: that is the ping-pong this
            // replaced (review B3).
            chatproj::Packet resp;
            resp.set_type(chatproj::Packet::LOGIN_RES);
            auto* res = resp.mutable_login_res();
            res->set_success(false);
            res->set_device_limit(true);
            res->set_message("This account is signed in on too many devices (" +
                             std::to_string(SessionManager::MAX_DEVICES) + "). Sign out on one of them first.");
            deliver(resp);
            return;
        }
        if (!bound.ok) return;  // the connection went away meanwhile
        authenticated_ = true;
        uid_ = outcome->login.uid;
        username_ = bound.presence.username.empty() ? outcome->login.username : bound.presence.username;
        name_epoch_ = epoch;
        if (bound.replaced) {
            std::cout << "[Manager] Replacing the previous connection of uid " << uid_ << " (same device)\n";
            manager_.retire(bound.replaced, chatproj::SessionClosed::REPLACED);
        }
    }

    /// REGISTER_RES, or a refused LOGIN_RES.
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
        deliver(resp_packet);
    }

    /// A successful LOGIN_RES with the account's presence `p` (the login's
    /// read, or the live one: SessionManager::bind sends it).
    static Frame login_frame(const LoginOutcome& outcome, const Presence& p) {
        chatproj::Packet resp_packet;
        resp_packet.set_type(chatproj::Packet::LOGIN_RES);
        auto* resp = resp_packet.mutable_login_res();
        resp->set_success(true);
        resp->set_message("Login successful!");
        resp->set_jwt_token(outcome.login.token);
        resp->set_resume_token(outcome.resume_token);
        resp->set_uid(outcome.login.uid);
        resp->set_username(p.username.empty() ? outcome.login.username : p.username);
        // Auto-rejoin: the user's community memberships inline, so the
        // client can start connecting without an extra round-trip.
        for (const auto& info : outcome.memberships) {
            *resp->add_memberships() = info;
        }
        // P2P DM calls: advertise that this central relays CALL_SIGNAL and
        // hand out the operator's STUN list (may be empty → the client falls
        // back to its built-in default).
        for (const auto& stun : g_stun_servers) {
            resp->add_stun_servers(stun);
        }
        resp->set_call_signaling(true);
        // E2EE DMs: this build serves the key endpoints.
        resp->set_e2ee_keys(true);
        resp->set_e2ee_uid_binding(true);
        // Username changes (uid design, stage 5).
        resp->set_renames(true);
        // User statuses: this build understands SET_STATUS_REQ /
        // SET_IDLE_REQ; echo the account's choice.
        resp->set_presence_statuses(true);
        resp->set_status(p.status);
        // Custom status message, with the time left rather than the
        // deadline so the client's timer ignores clock skew.
        resp->set_status_text(p.status_text);
        resp->set_status_emoji(p.status_emoji);
        if (p.status_expires_at > 0) {
            resp->set_status_clear_in(std::max<int64_t>(1, p.status_expires_at - unix_now()));
        }
        resp->set_dm_friends_only(p.dm_friends_only);
        return make_frame(resp_packet);
    }

    ssl::stream<tcp::socket> socket_;
    SessionManager& manager_;
    AuthManager& auth_manager_;
    boost::asio::thread_pool& cpu_pool_;
    boost::asio::thread_pool& db_pool_;
    AbuseLimits& limits_;
    const std::string ip_;
    /// Holds a request over its rate until the bucket refills (on_packet).
    boost::asio::steady_timer pace_timer_;
    char inbound_header_[4];
    std::vector<uint8_t> inbound_body_;

    // Strand only. Others' view of this account (presence, avatar, DM
    // privacy) lives in the SessionManager.
    bool authenticated_ = false;
    int64_t uid_ = 0;
    /// The name as stored, for labels and the names messages are sent under.
    std::string username_;
    /// SessionManager::rename_epoch() when username_ was last read.
    uint64_t name_epoch_ = 0;
    /// LoginRequest.device_id: which install this connection is.
    std::string device_id_;
    /// A handler is running on the database pool, waiting on the CPU pool or
    /// waiting for its rate bucket; whatever finishes it resumes reading.
    bool read_paused_ = false;
    /// close() ran: no more writes or reads.
    bool closed_ = false;
    /// close_after_flush() ran: no more reads; closes when the queue drains.
    bool closing_ = false;
    std::deque<Frame> write_queue_;
    /// Bytes in write_queue_, and whether it overflowed (enqueue).
    size_t queued_bytes_ = 0;
    bool backlog_full_ = false;
    /// steady_clock ticks of the last packet (any thread reads it).
    std::atomic<std::chrono::steady_clock::rep> last_activity_{0};
    /// The last token that verified, and when it expires (token_ok).
    std::string verified_token_;
    int64_t verified_token_expiry_ = 0;

    // Per-connection buckets. Paced (on_packet: an over-rate request
    // waits, never dropped): DMs and their edits / deletes at the
    // community's message rate (10 burst / 3 per second); reads that each
    // cost a database round trip (history, conversations, read cursors,
    // avatars and server pictures, friend and server lists, invite lookups)
    // at 60 / 20 per second, which a client loading a screenful never
    // reaches; friend actions and avatar uploads at 20 / 1 per second; and
    // sign-in / sign-up attempts at 5 / 1 per 2 s (address and account
    // limits sit on top, AbuseLimits).
    RateBucket dm_bucket_{10.0, 3.0};
    RateBucket read_bucket_{60.0, 20.0};
    RateBucket write_bucket_{20.0, 1.0};
    RateBucket login_bucket_{5.0, 0.5};
    // Paced as well (review B13; each was dropped over the rate before):
    // CALL_SIGNAL (20 burst / 5 per second: far above a real handshake, ~6
    // signals per call, and keeps the INVITE-only DB touch bounded), the key
    // endpoints, status changes and presence subscriptions. Renames and
    // token renewals are refused over the rate, with an answer.
    RateBucket call_bucket_{20.0, 5.0};
    /// Same shape for the E2EE key endpoints (publish / fetch / backup):
    /// each is a database round-trip. A channel key fill or an MLS leaf
    /// check fetches one bundle per member; past the burst they wait.
    RateBucket key_bucket_{20.0, 5.0};
    /// Renames: each verifies the password (bcrypt), so a stolen token
    /// can't use this to guess it quickly. 10 burst (a few taken names in
    /// a row) / 1 per 30 s.
    RateBucket rename_bucket_{10.0, 1.0 / 30.0};
    /// Token renewal: clients ask every few hours; 3 burst / 1 per minute.
    RateBucket refresh_bucket_{3.0, 1.0 / 60.0};
    /// Status / idle / DM-privacy / status-text changes: each is a DB write
    /// or a fan-out to the account's watchers. A person picks a status a few
    /// times a day and auto-away flips at most every few minutes; 10 burst /
    /// 1 per 2 s never holds back a real client, and a burst past it still
    /// ends on its last value.
    RateBucket status_bucket_{10.0, 0.5};
    /// Presence subscriptions: the client sends its whole watch set at most
    /// every 600 ms (throttled); 10 burst / 2 per second never holds it back.
    RateBucket watch_bucket_{10.0, 2.0};
};


// --- SessionManager Implementations ---

Frame make_frame(const chatproj::Packet& packet) {
    std::string serialized;
    packet.SerializeToString(&serialized);
    return std::make_shared<const std::vector<uint8_t>>(chatproj::create_framed_packet(serialized));
}

namespace {
/// What others can see of an account; a change is what warrants a delta.
/// Status and message only count while visible.
struct PresenceView {
    bool visible;
    chatproj::PresenceStatus status;
    std::string text;
    std::string emoji;
    explicit PresenceView(const Presence& p)
        : visible(p.visible()), status(p.effective()), text(p.status_text), emoji(p.status_emoji) {}
    bool operator!=(const PresenceView& o) const {
        return visible != o.visible ||
               (visible && (status != o.status || text != o.text || emoji != o.emoji));
    }
};

/// What the account's own devices show of it (OWN_PRESENCE).
bool own_view_differs(const Presence& a, const Presence& b) {
    return a.status != b.status || a.status_text != b.status_text || a.status_emoji != b.status_emoji ||
           a.status_expires_at != b.status_expires_at || a.dm_friends_only != b.dm_friends_only ||
           a.idle != b.idle;
}

void deliver_all(const std::vector<std::shared_ptr<Session>>& targets, const Frame& frame) {
    for (const auto& session : targets) session->deliver(frame);
}

/// How long an unanswered call is kept (clients give up at 45 s), and how
/// long an answered one waits for a side that left to come back.
constexpr auto RING_KEEP = std::chrono::seconds(90);
constexpr auto DETACHED_KEEP = std::chrono::seconds(120);
/// Calls one connection may be placing at once (a client places one; glare
/// adds a second for a moment).
constexpr size_t MAX_CALLS_PER_CALLER = 4;

bool contains(const std::vector<std::shared_ptr<Session>>& list, const Session* s) {
    for (const auto& x : list) if (x.get() == s) return true;
    return false;
}

void remove_from(std::vector<std::shared_ptr<Session>>& list, const Session* s) {
    list.erase(std::remove_if(list.begin(), list.end(), [s](const auto& x) { return x.get() == s; }), list.end());
}

/// A central-made CALL_SIGNAL about call `id`, from `from` to `to`.
Frame call_frame(chatproj::CallSignal::Kind kind, const std::string& id, const std::string& from, int64_t from_uid,
                 const std::string& to, int64_t to_uid) {
    chatproj::Packet pkt;
    pkt.set_type(chatproj::Packet::CALL_SIGNAL);
    auto* sig = pkt.mutable_call_signal();
    sig->set_kind(kind);
    sig->set_call_id(id);
    sig->set_from(from);
    sig->set_from_uid(from_uid);
    sig->set_to(to);
    sig->set_to_uid(to_uid);
    sig->set_timestamp(unix_now());
    return make_frame(pkt);
}
}  // namespace

void SessionManager::Fanout::add(std::vector<std::shared_ptr<Session>> to, Frame frame) {
    if (!to.empty() && frame) sends.emplace_back(std::move(to), std::move(frame));
}

void SessionManager::Fanout::run() {
    for (const auto& [to, frame] : sends) deliver_all(to, frame);
}

void SessionManager::fill_presence(chatproj::UserPresence* entry, int64_t uid, const Presence& p) {
    entry->set_uid(uid);
    entry->set_username(p.username);
    entry->set_avatar_version(p.avatar_version);
    entry->set_status(p.effective());
    entry->set_status_text(p.status_text);
    entry->set_status_emoji(p.status_emoji);
}

chatproj::Packet SessionManager::presence_delta(int64_t uid, const std::string& username, const Presence* p) {
    chatproj::Packet packet;
    packet.set_type(chatproj::Packet::PRESENCE_DELTA);
    auto* d = packet.mutable_presence_delta();
    if (p && p->visible()) {
        d->set_online(true);
        fill_presence(d->mutable_user(), uid, *p);
    } else {
        d->mutable_user()->set_uid(uid);
        d->mutable_user()->set_username(username);
    }
    return packet;
}

chatproj::Packet SessionManager::own_presence(const Presence& p) {
    chatproj::Packet packet;
    packet.set_type(chatproj::Packet::OWN_PRESENCE);
    auto* o = packet.mutable_own_presence();
    o->set_status(p.status);
    o->set_status_text(p.status_text);
    o->set_status_emoji(p.status_emoji);
    // The time left rather than the deadline, as in LOGIN_RES: the client's
    // timer then ignores clock skew.
    if (p.status_expires_at > 0) o->set_status_clear_in(std::max<int64_t>(1, p.status_expires_at - unix_now()));
    o->set_dm_friends_only(p.dm_friends_only);
    o->set_idle(p.idle);
    return packet;
}

Frame SessionManager::session_closed(chatproj::SessionClosed::Reason reason) {
    chatproj::Packet packet;
    packet.set_type(chatproj::Packet::SESSION_CLOSED);
    packet.mutable_session_closed()->set_reason(reason);
    return make_frame(packet);
}

std::vector<std::shared_ptr<Session>> SessionManager::watcher_sessions_locked(int64_t uid) const {
    std::vector<std::shared_ptr<Session>> out;
    auto it = watchers_.find(uid);
    if (it == watchers_.end()) return out;
    out.reserve(it->second.size());
    // Alive: a watcher is in sessions_ (which holds it) for as long as it is here.
    for (Session* watcher : it->second) out.push_back(watcher->shared_from_this());
    return out;
}

void SessionManager::unwatch_all_locked(Session* session, Connection& conn) {
    for (int64_t target : conn.watching) {
        auto it = watchers_.find(target);
        if (it == watchers_.end()) continue;
        it->second.erase(session);
        if (it->second.empty()) watchers_.erase(it);
    }
    conn.watching.clear();
}

void SessionManager::recompute_idle_locked(Account& account) {
    bool all = !account.sessions.empty();
    for (const auto& s : account.sessions) {
        auto it = sessions_.find(s);
        if (it == sessions_.end() || !it->second.idle) {
            all = false;
            break;
        }
    }
    account.presence.idle = all;
}

void SessionManager::presence_changed_locked(int64_t uid, const Account& account, const Presence& before,
                                             const Session* except, Fanout& out) {
    const Presence& p = account.presence;
    if (PresenceView(p) != PresenceView(before)) {
        out.add(watcher_sessions_locked(uid), make_frame(presence_delta(uid, p.username, &p)));
    }
    if (own_view_differs(p, before)) {
        std::vector<std::shared_ptr<Session>> own;
        for (const auto& s : account.sessions) {
            if (s.get() != except) own.push_back(s);
        }
        out.add(std::move(own), make_frame(own_presence(p)));
    }
}

std::shared_ptr<Session> SessionManager::device_session_locked(int64_t uid, const std::string& device,
                                                               const Session* except) const {
    if (device.empty()) return nullptr;
    auto acc = accounts_.find(uid);
    if (acc == accounts_.end()) return nullptr;
    for (const auto& s : acc->second.sessions) {
        if (s.get() == except) continue;
        auto it = sessions_.find(s);
        if (it != sessions_.end() && it->second.device_id == device) return s;
    }
    return nullptr;
}

void SessionManager::sign_out_locked(const std::shared_ptr<Session>& session, Connection& conn, Fanout& out) {
    const int64_t uid = conn.uid;
    unwatch_all_locked(session.get(), conn);
    call_leave_locked(session.get(), conn.device_id, out);
    conn.uid = 0;
    if (uid <= 0) return;
    auto acc = accounts_.find(uid);
    if (acc == accounts_.end()) return;
    Account& account = acc->second;
    const size_t before_count = account.sessions.size();
    remove_from(account.sessions, session.get());
    if (account.sessions.size() == before_count) return;  // wasn't one of its devices
    if (account.sessions.empty()) {
        // The last device: the account goes offline. An invisible user's
        // disconnect says nothing.
        if (account.presence.visible()) {
            out.add(watcher_sessions_locked(uid),
                    make_frame(presence_delta(uid, account.presence.username, nullptr)));
        }
        accounts_.erase(acc);
        return;
    }
    // Others remain: the account stays online, and may now be idle
    // everywhere it still is.
    const Presence before = account.presence;
    recompute_idle_locked(account);
    presence_changed_locked(uid, account, before, nullptr, out);
}

void SessionManager::join(const std::shared_ptr<Session>& session) {
    size_t total;
    {
        std::lock_guard<std::mutex> lock(mutex_);
        sessions_.emplace(session, Connection{});
        total = sessions_.size();
    }
    std::cout << "[Manager] Session joined. Total: " << total << "\n";
}

void SessionManager::leave(const std::shared_ptr<Session>& session) {
    size_t total = 0;
    Fanout out;
    {
        std::lock_guard<std::mutex> lock(mutex_);
        auto it = sessions_.find(session);
        if (it == sessions_.end()) return;  // already gone
        sign_out_locked(session, it->second, out);
        sessions_.erase(it);
        total = sessions_.size();
    }
    session->close();
    std::cout << "[Manager] Session left. Total: " << total << "\n";
    out.run();
}

void SessionManager::retire(const std::shared_ptr<Session>& session, chatproj::SessionClosed::Reason reason) {
    Fanout out;
    {
        std::lock_guard<std::mutex> lock(mutex_);
        auto it = sessions_.find(session);
        if (it == sessions_.end()) return;
        sign_out_locked(session, it->second, out);
        sessions_.erase(it);
    }
    // Both post to its strand, in this order: the notice is queued before
    // the close waits for the queue to drain.
    session->deliver(session_closed(reason));
    session->close_after_flush();
    out.run();
}

size_t SessionManager::close_other_sessions(int64_t uid, const std::shared_ptr<Session>& keep) {
    std::vector<std::shared_ptr<Session>> others;
    {
        std::lock_guard<std::mutex> lock(mutex_);
        auto acc = accounts_.find(uid);
        if (acc == accounts_.end()) return 0;
        for (const auto& s : acc->second.sessions) {
            if (s != keep) others.push_back(s);
        }
    }
    for (const auto& s : others) retire(s, chatproj::SessionClosed::SIGNED_OUT);
    return others.size();
}

namespace {
/// What a LOGIN_RES says of the account's own state.
bool login_view_differs(const Presence& a, const Presence& b) {
    return a.username != b.username || a.status != b.status || a.status_text != b.status_text ||
           a.status_emoji != b.status_emoji || a.status_expires_at != b.status_expires_at ||
           a.dm_friends_only != b.dm_friends_only;
}
}  // namespace

SessionManager::Bound SessionManager::bind(const std::shared_ptr<Session>& session, int64_t uid,
                                           const std::string& device_id, Presence presence, const Frame& reply,
                                           const std::function<Frame(const Presence&)>& rebuild) {
    Bound result;
    Fanout out;
    {
        std::lock_guard<std::mutex> lock(mutex_);
        auto it = sessions_.find(session);
        if (it == sessions_.end()) return result;  // left while the login ran
        Connection& conn = it->second;
        // Signed in on this connection already (as anyone): that sign-in
        // ends first.
        if (conn.uid > 0) sign_out_locked(session, conn, out);

        auto acc = accounts_.find(uid);
        const bool fresh = acc == accounts_.end();
        if (!fresh) {
            Account& account = acc->second;
            // The same device signing in again: its old connection is dead
            // (or a copy of the install). It gives up its place; the account
            // stays (and says nothing) even when that was its only one. Its
            // calls stay too: the new connection has its device id and takes
            // its place in them once retire() detaches it.
            if (auto old = device_session_locked(uid, device_id)) {
                result.replaced = old;
                auto old_it = sessions_.find(old);
                if (old_it != sessions_.end()) {
                    unwatch_all_locked(old.get(), old_it->second);
                    old_it->second.uid = 0;
                }
                remove_from(account.sessions, old.get());
            } else if (account.sessions.size() >= MAX_DEVICES) {
                result.full = true;
                return result;
            }
        }
        if (fresh) acc = accounts_.emplace(uid, Account{}).first;
        Account& account = acc->second;
        conn.uid = uid;
        conn.device_id = device_id;
        conn.idle = false;
        account.sessions.push_back(session);
        Frame answer = reply;
        if (fresh) {
            // The saved status applies before anyone hears of the login, so
            // an invisible user never flashes online.
            account.presence = std::move(presence);
            account.presence.idle = false;
            if (account.presence.visible()) {
                out.add(watcher_sessions_locked(uid),
                        make_frame(presence_delta(uid, account.presence.username, &account.presence)));
            }
        } else {
            // Already online elsewhere (or swapping its only device): the
            // live presence stands. An active device ends auto-away.
            if (login_view_differs(account.presence, presence)) answer = rebuild(account.presence);
            const Presence before = account.presence;
            recompute_idle_locked(account);
            presence_changed_locked(uid, account, before, session.get(), out);
        }
        // Posted under the lock: a change from another device takes the lock
        // to find this connection, so it reaches it after the reply.
        session->deliver(answer);
        result.presence = account.presence;
        result.ok = true;
    }
    out.run();
    return result;
}

bool SessionManager::deliver_to_user(int64_t uid, const Frame& frame) {
    if (uid <= 0) return false;
    std::vector<std::shared_ptr<Session>> targets;
    {
        std::lock_guard<std::mutex> lock(mutex_);
        auto it = accounts_.find(uid);
        if (it == accounts_.end()) return false;
        targets = it->second.sessions;
    }
    deliver_all(targets, frame);
    return !targets.empty();
}

bool SessionManager::deliver_to_user(int64_t uid, const chatproj::Packet& packet) {
    // A packet bound for another account never carries credentials. The
    // relays build theirs fresh; this is the backstop should one ever copy
    // a client's request again (DMs and call signals once leaked the
    // sender's session token this way).
    if (!packet.auth_token().empty()) {
        chatproj::Packet clean = packet;
        clean.clear_auth_token();
        return deliver_to_user(uid, make_frame(clean));
    }
    return deliver_to_user(uid, make_frame(packet));
}

void SessionManager::deliver_to_user_except(int64_t uid, const Session* except, const chatproj::Packet& packet) {
    if (uid <= 0) return;
    std::vector<std::shared_ptr<Session>> targets;
    {
        std::lock_guard<std::mutex> lock(mutex_);
        auto it = accounts_.find(uid);
        if (it == accounts_.end()) return;
        for (const auto& s : it->second.sessions) {
            if (s.get() != except) targets.push_back(s);
        }
    }
    if (targets.empty()) return;
    if (!packet.auth_token().empty()) {
        chatproj::Packet clean = packet;
        clean.clear_auth_token();
        deliver_all(targets, make_frame(clean));
        return;
    }
    deliver_all(targets, make_frame(packet));
}

void SessionManager::deliver_to_users(const std::vector<int64_t>& uids, const chatproj::Packet& packet) {
    if (uids.empty()) return;
    std::vector<std::shared_ptr<Session>> targets;
    {
        std::lock_guard<std::mutex> lock(mutex_);
        std::unordered_set<int64_t> seen;
        for (int64_t uid : uids) {
            if (!seen.insert(uid).second) continue;
            auto it = accounts_.find(uid);
            if (it == accounts_.end()) continue;
            targets.insert(targets.end(), it->second.sessions.begin(), it->second.sessions.end());
        }
    }
    if (!targets.empty()) deliver_all(targets, make_frame(packet));
}

void SessionManager::deliver_to_watchers(int64_t uid, const chatproj::Packet& packet) {
    std::vector<std::shared_ptr<Session>> targets;
    {
        std::lock_guard<std::mutex> lock(mutex_);
        targets = watcher_sessions_locked(uid);
    }
    if (!targets.empty()) deliver_all(targets, make_frame(packet));
}

bool SessionManager::is_user_visible(int64_t uid) {
    std::lock_guard<std::mutex> lock(mutex_);
    auto it = accounts_.find(uid);
    return it != accounts_.end() && it->second.presence.visible();
}

bool SessionManager::update_presence(int64_t uid, const std::function<void(Presence&)>& change,
                                     const std::function<void(const Presence&)>& save) {
    std::shared_ptr<std::mutex> saving;
    if (save) {
        std::lock_guard<std::mutex> lock(mutex_);
        auto it = accounts_.find(uid);
        if (it == accounts_.end()) return false;
        saving = it->second.saving;
    }
    // One change and its save at a time for this account, taken before the
    // directory's lock and held across the database write (never mutex_).
    std::unique_lock<std::mutex> in_order;
    if (saving) in_order = std::unique_lock<std::mutex>(*saving);
    Fanout out;
    Presence after;
    {
        std::lock_guard<std::mutex> lock(mutex_);
        auto it = accounts_.find(uid);
        if (it == accounts_.end()) return false;
        const Presence before = it->second.presence;
        change(it->second.presence);
        presence_changed_locked(uid, it->second, before, nullptr, out);
        if (save) after = it->second.presence;
    }
    out.run();
    if (save) save(after);
    return true;
}

void SessionManager::set_idle(const std::shared_ptr<Session>& session, bool idle) {
    Fanout out;
    {
        std::lock_guard<std::mutex> lock(mutex_);
        auto it = sessions_.find(session);
        if (it == sessions_.end() || it->second.uid <= 0 || it->second.idle == idle) return;
        it->second.idle = idle;
        auto acc = accounts_.find(it->second.uid);
        if (acc == accounts_.end()) return;
        const Presence before = acc->second.presence;
        recompute_idle_locked(acc->second);
        presence_changed_locked(it->second.uid, acc->second, before, nullptr, out);
    }
    out.run();
}

void SessionManager::watch(const std::shared_ptr<Session>& session, int64_t uid, const std::vector<int64_t>& uids,
                           AuthManager& auth_manager) {
    std::vector<int64_t> added;
    {
        std::lock_guard<std::mutex> lock(mutex_);
        auto it = sessions_.find(session);
        if (it == sessions_.end() || it->second.uid != uid) return;  // replaced or gone meanwhile
        Connection& conn = it->second;
        std::unordered_set<int64_t> next;
        next.reserve(std::min(uids.size(), MAX_WATCH));
        for (int64_t target : uids) {
            if (next.size() >= MAX_WATCH) break;
            if (target > 0) next.insert(target);
        }
        for (int64_t target : conn.watching) {
            if (next.count(target)) continue;
            auto w = watchers_.find(target);
            if (w == watchers_.end()) continue;
            w->second.erase(session.get());
            if (w->second.empty()) watchers_.erase(w);
        }
        for (int64_t target : next) {
            if (conn.watching.count(target)) continue;
            watchers_[target].insert(session.get());
            added.push_back(target);
        }
        conn.watching = std::move(next);
    }
    if (added.empty()) return;

    // Pictures of accounts that aren't signed in come from the database,
    // read without the lock (one query for the whole set). A signed-in
    // account's version is the directory's, kept current by
    // push_avatar_changed; the two agree, so an invisible account answers
    // exactly like an offline one.
    const auto stored = auth_manager.getAvatarVersions(added);

    // Built and posted under the lock: any push about these accounts that
    // takes the lock after this is delivered after the answer, and one
    // delivered before it is already reflected in it.
    //
    // In parts of at most MAX_RESPONSE_BYTES (review A1): 5,000 visible
    // accounts with long status messages come to over 3 MB. Each part is a
    // whole answer for the accounts it covers, and clients apply each
    // PRESENCE_UPDATE on its own.
    std::lock_guard<std::mutex> lock(mutex_);
    auto it = sessions_.find(session);
    if (it == sessions_.end() || it->second.uid != uid) return;
    const Connection& conn = it->second;
    chatproj::Packet answer;
    answer.set_type(chatproj::Packet::PRESENCE_UPDATE);
    auto* update = answer.mutable_presence_update();
    size_t bytes = 0;
    for (int64_t target : added) {
        if (!conn.watching.count(target)) continue;
        if (bytes >= MAX_RESPONSE_BYTES) {
            session->deliver(make_frame(answer));
            update->Clear();
            bytes = 0;
        }
        update->add_covered_uids(target);
        bytes += 10;  // at most, as a varint
        const std::string* version = nullptr;
        auto other = accounts_.find(target);
        if (other != accounts_.end()) {
            if (other->second.presence.visible()) {
                auto* entry = update->add_users();
                fill_presence(entry, target, other->second.presence);
                bytes += entry->ByteSizeLong() + 3;
                continue;
            }
            version = &other->second.presence.avatar_version;
        } else if (auto s = stored.find(target); s != stored.end()) {
            version = &s->second;
        }
        if (version && !version->empty()) {
            auto* avatar = update->add_avatars();
            avatar->set_uid(target);
            avatar->set_version(*version);
            bytes += avatar->ByteSizeLong() + 2;
        }
    }
    if (update->covered_uids_size() > 0) session->deliver(make_frame(answer));
}

namespace {
/// The account's watchers plus its own connections but `from`, once each.
std::vector<std::shared_ptr<Session>> with_own(std::vector<std::shared_ptr<Session>> watchers,
                                               const std::vector<std::shared_ptr<Session>>& own,
                                               const Session* from) {
    for (const auto& s : own) {
        if (s.get() == from) continue;
        if (!contains(watchers, s.get())) watchers.push_back(s);
    }
    return watchers;
}
}  // namespace

void SessionManager::push_user_renamed(int64_t uid, const std::string& username, const Session* from) {
    std::vector<std::shared_ptr<Session>> targets;
    {
        std::lock_guard<std::mutex> lock(mutex_);
        auto it = accounts_.find(uid);
        targets = watcher_sessions_locked(uid);
        if (it != accounts_.end()) {
            it->second.presence.username = username;
            targets = with_own(std::move(targets), it->second.sessions, from);
        }
        rename_epoch_.fetch_add(1, std::memory_order_acq_rel);
    }
    if (targets.empty()) return;
    chatproj::Packet packet;
    packet.set_type(chatproj::Packet::USER_RENAMED);
    auto* payload = packet.mutable_user_renamed();
    payload->set_uid(uid);
    payload->set_username(username);
    deliver_all(targets, make_frame(packet));
}

std::string SessionManager::account_name(int64_t uid, const std::string& fallback) {
    std::lock_guard<std::mutex> lock(mutex_);
    auto it = accounts_.find(uid);
    return it != accounts_.end() && !it->second.presence.username.empty() ? it->second.presence.username : fallback;
}

void SessionManager::push_avatar_changed(int64_t uid, const std::string& version, const Session* from) {
    std::vector<std::shared_ptr<Session>> targets;
    std::string username;
    {
        std::lock_guard<std::mutex> lock(mutex_);
        auto it = accounts_.find(uid);
        targets = watcher_sessions_locked(uid);
        if (it != accounts_.end()) {
            it->second.presence.avatar_version = version;
            username = it->second.presence.username;
            targets = with_own(std::move(targets), it->second.sessions, from);
        }
    }
    if (targets.empty()) return;
    chatproj::Packet packet;
    packet.set_type(chatproj::Packet::AVATAR_CHANGED);
    auto* payload = packet.mutable_avatar_changed();
    payload->set_uid(uid);
    payload->set_username(username);
    payload->set_version(version);
    deliver_all(targets, make_frame(packet));
}

void SessionManager::push_e2ee_keys_changed(int64_t uid, uint32_t key_id, const Session* from) {
    std::vector<std::shared_ptr<Session>> targets;
    std::string username;
    {
        std::lock_guard<std::mutex> lock(mutex_);
        auto it = accounts_.find(uid);
        targets = watcher_sessions_locked(uid);
        if (it != accounts_.end()) {
            username = it->second.presence.username;
            // The account's other devices: a new generation means theirs are
            // stale (they lock and re-sync from the backup).
            targets = with_own(std::move(targets), it->second.sessions, from);
        }
    }
    if (targets.empty()) return;
    chatproj::Packet packet;
    packet.set_type(chatproj::Packet::E2EE_KEYS_CHANGED);
    auto* payload = packet.mutable_e2ee_keys_changed();
    payload->set_uid(uid);
    payload->set_username(username);
    payload->set_key_id(key_id);
    deliver_all(targets, make_frame(packet));
}

// --- Calls ---

void SessionManager::index_call_locked(Session* session, const std::string& id) {
    if (!session) return;
    auto& ids = call_index_[session];
    if (std::find(ids.begin(), ids.end(), id) == ids.end()) ids.push_back(id);
}

void SessionManager::unindex_call_locked(Session* session, const std::string& id) {
    if (!session) return;
    auto it = call_index_.find(session);
    if (it == call_index_.end()) return;
    auto& ids = it->second;
    ids.erase(std::remove(ids.begin(), ids.end(), id), ids.end());
    if (ids.empty()) call_index_.erase(it);
}

void SessionManager::erase_call_locked(const std::string& id) {
    auto it = calls_.find(id);
    if (it == calls_.end()) return;
    // Copied: unindexing may not touch the call's own strings.
    const std::string key = id;
    Call& c = it->second;
    unindex_call_locked(c.caller.get(), key);
    unindex_call_locked(c.callee.get(), key);
    for (const auto& s : c.ringing) unindex_call_locked(s.get(), key);
    calls_.erase(it);
}

void SessionManager::reattach_locked(Call& c, const std::string& id, const Session* except) {
    if (!c.caller) {
        c.caller = device_session_locked(c.caller_uid, c.caller_device, except);
        index_call_locked(c.caller.get(), id);
    }
    if (c.answered && !c.callee) {
        c.callee = device_session_locked(c.callee_uid, c.callee_device, except);
        index_call_locked(c.callee.get(), id);
    }
    if (c.caller && (c.callee || !c.answered)) {
        c.detached = {};
    } else if (c.answered && c.detached == std::chrono::steady_clock::time_point{}) {
        c.detached = std::chrono::steady_clock::now();
    }
}

void SessionManager::call_leave_locked(Session* session, const std::string& device, Fanout& out) {
    auto idx = call_index_.find(session);
    if (idx == call_index_.end()) return;
    const std::vector<std::string> ids = std::move(idx->second);
    call_index_.erase(idx);
    for (const auto& id : ids) {
        auto it = calls_.find(id);
        if (it == calls_.end()) continue;
        Call& c = it->second;
        // A side whose device has another connection already (the one that
        // replaced this) moves there; otherwise it waits for that device.
        if (c.caller.get() == session) c.caller.reset();
        if (c.callee.get() == session) c.callee.reset();
        reattach_locked(c, id, session);
        if (contains(c.ringing, session)) {
            remove_from(c.ringing, session);
            // A ringing device that signed in again keeps ringing there: the
            // call may still be showing, and its answer comes from the new
            // connection.
            auto back = device_session_locked(c.callee_uid, device, session);
            if (back && !contains(c.ringing, back.get())) {
                c.ringing.push_back(back);
                index_call_locked(back.get(), id);
            }
            if (!c.answered && c.ringing.empty()) {
                // No device of the callee can ring any more. Busy if one of
                // them said so; otherwise the caller's own timeout ends it.
                if (c.busy && c.caller) {
                    out.add({c.caller}, call_frame(chatproj::CallSignal::BUSY, id, c.callee_name, c.callee_uid,
                                                   c.caller_name, c.caller_uid));
                }
                erase_call_locked(id);
                continue;
            }
        }
        if (c.answered && !c.caller && !c.callee) erase_call_locked(id);
    }
}

SessionManager::CallRoute SessionManager::route_call(const std::shared_ptr<Session>& from, int64_t from_uid,
                                                     int64_t to_uid, const chatproj::Packet& routed) {
    using K = chatproj::CallSignal;
    const auto& sig = routed.call_signal();
    const std::string& id = sig.call_id();
    const auto kind = sig.kind();
    Fanout out;
    {
        std::lock_guard<std::mutex> lock(mutex_);
        auto conn = sessions_.find(from);
        if (conn == sessions_.end() || conn->second.uid != from_uid) return CallRoute::dropped;
        const std::string& device = conn->second.device_id;

        if (kind == K::INVITE) {
            if (id.empty() || calls_.count(id)) return CallRoute::dropped;
            if (auto idx = call_index_.find(from.get()); idx != call_index_.end()) {
                size_t placing = 0;
                for (const auto& other : idx->second) {
                    auto c = calls_.find(other);
                    if (c != calls_.end() && c->second.caller == from) ++placing;
                }
                if (placing >= MAX_CALLS_PER_CALLER) return CallRoute::dropped;
            }
            auto acc = accounts_.find(to_uid);
            if (acc == accounts_.end() || acc->second.sessions.empty()) return CallRoute::offline;
            Call c;
            c.caller_uid = from_uid;
            c.callee_uid = to_uid;
            c.caller_name = sig.from();
            c.callee_name = sig.to();
            c.caller = from;
            c.caller_device = device;
            c.ringing = acc->second.sessions;
            c.started = std::chrono::steady_clock::now();
            index_call_locked(from.get(), id);
            for (const auto& s : c.ringing) index_call_locked(s.get(), id);
            out.add(c.ringing, make_frame(routed));
            calls_.emplace(id, std::move(c));
        } else {
            auto it = calls_.find(id);
            if (it == calls_.end()) return CallRoute::dropped;
            Call& c = it->second;
            const bool by_caller = from_uid == c.caller_uid && to_uid == c.callee_uid;
            const bool by_callee = from_uid == c.callee_uid && to_uid == c.caller_uid;
            if (!by_caller && !by_callee) return CallRoute::dropped;
            const Frame frame = make_frame(routed);
            // The other side's connection, or the one its device signed back
            // in on (which takes its place).
            auto target = [&](const std::shared_ptr<Session>& s, int64_t, const std::string&) {
                std::vector<std::shared_ptr<Session>> to;
                if (!s) reattach_locked(c, id);
                if (s) to.push_back(s);
                return to;
            };
            auto elsewhere = [&](K::Kind k) {
                return call_frame(k, id, c.caller_name, c.caller_uid, c.callee_name, c.callee_uid);
            };
            if (by_caller) {
                if (c.caller != from) {
                    // Only the device that placed the call, signing back in.
                    if (c.caller || device.empty() || device != c.caller_device) return CallRoute::dropped;
                    c.caller = from;
                    index_call_locked(from.get(), id);
                }
                switch (kind) {
                    case K::CANCEL:
                    case K::HANGUP:
                        out.add(c.answered ? target(c.callee, c.callee_uid, c.callee_device) : c.ringing, frame);
                        erase_call_locked(id);
                        break;
                    case K::STREAM_START:
                    case K::STREAM_STOP:
                        if (!c.answered) return CallRoute::dropped;
                        out.add(target(c.callee, c.callee_uid, c.callee_device), frame);
                        break;
                    default:
                        return CallRoute::dropped;
                }
            } else if (!c.answered) {
                if (!contains(c.ringing, from.get())) return CallRoute::dropped;
                std::vector<std::shared_ptr<Session>> others;
                for (const auto& s : c.ringing) {
                    if (s != from) others.push_back(s);
                }
                switch (kind) {
                    case K::RINGING:
                        if (c.rang) break;
                        c.rang = true;
                        out.add(target(c.caller, c.caller_uid, c.caller_device), frame);
                        break;
                    case K::BUSY:
                        // Heard only once no device of the callee can ring.
                        c.busy = true;
                        remove_from(c.ringing, from.get());
                        unindex_call_locked(from.get(), id);
                        if (c.ringing.empty()) {
                            out.add(target(c.caller, c.caller_uid, c.caller_device), frame);
                            erase_call_locked(id);
                        }
                        break;
                    case K::ACCEPT:
                        // The first to answer takes the call. A caller whose
                        // connection is gone is waited for from now
                        // (reattach_locked starts the clock).
                        c.answered = true;
                        c.callee = from;
                        c.callee_device = device;
                        reattach_locked(c, id);
                        for (const auto& s : others) unindex_call_locked(s.get(), id);
                        c.ringing.clear();
                        out.add(std::move(others), elsewhere(K::ANSWERED_ELSEWHERE));
                        out.add(target(c.caller, c.caller_uid, c.caller_device), frame);
                        break;
                    case K::REJECT:
                    case K::HANGUP:
                        out.add(target(c.caller, c.caller_uid, c.caller_device), frame);
                        out.add(std::move(others), elsewhere(K::DECLINED_ELSEWHERE));
                        erase_call_locked(id);
                        break;
                    default:
                        return CallRoute::dropped;
                }
            } else if (c.callee != from && (c.callee || device.empty() || device != c.callee_device)) {
                // Another device of the callee. One whose ACCEPT lost the
                // race to answer hears who won; anything else is dropped.
                if (kind != K::ACCEPT) return CallRoute::dropped;
                out.add({from}, elsewhere(K::ANSWERED_ELSEWHERE));
            } else {
                if (c.callee != from) {
                    c.callee = from;  // the answering device, signed back in
                    index_call_locked(from.get(), id);
                }
                switch (kind) {
                    case K::HANGUP:
                    case K::REJECT:
                        out.add(target(c.caller, c.caller_uid, c.caller_device), frame);
                        erase_call_locked(id);
                        break;
                    case K::STREAM_START:
                    case K::STREAM_STOP:
                        out.add(target(c.caller, c.caller_uid, c.caller_device), frame);
                        break;
                    default:
                        return CallRoute::dropped;
                }
            }
            if (auto again = calls_.find(id); again != calls_.end() && again->second.caller && again->second.callee) {
                again->second.detached = {};
            }
        }
    }
    out.run();
    return CallRoute::sent;
}

void SessionManager::sweep_calls() {
    const auto now = std::chrono::steady_clock::now();
    std::lock_guard<std::mutex> lock(mutex_);
    std::vector<std::string> stale;
    for (auto& [id, c] : calls_) {
        // A side's device may have signed back in without sending anything.
        if (c.answered && c.detached != std::chrono::steady_clock::time_point{}) reattach_locked(c, id);
        if (!c.answered ? now - c.started > RING_KEEP
                        : (c.detached != std::chrono::steady_clock::time_point{} && now - c.detached > DETACHED_KEEP)) {
            stale.push_back(id);
        }
    }
    for (const auto& id : stale) erase_call_locked(id);
}

void SessionManager::sweep_stale(std::chrono::seconds timeout) {
    const auto now = std::chrono::steady_clock::now();
    std::vector<std::shared_ptr<Session>> stale;
    {
        std::lock_guard<std::mutex> lock(mutex_);
        for (const auto& [session, conn] : sessions_) {
            if (now - session->last_activity() > timeout) stale.push_back(session);
        }
    }
    for (const auto& session : stale) {
        std::cout << "[Manager] Sweeping a stale session\n";
        leave(session);
    }
}

void SessionManager::expire_status_messages(AuthManager& auth_manager) {
    const int64_t now = unix_now();
    struct Expired {
        int64_t uid;
        int64_t expires_at;
    };
    std::vector<Expired> expired;
    Fanout out;
    {
        std::lock_guard<std::mutex> lock(mutex_);
        for (auto& [uid, account] : accounts_) {
            Presence& p = account.presence;
            if (p.status_expires_at <= 0 || p.status_expires_at > now) continue;
            expired.push_back({uid, p.status_expires_at});
            const Presence before = p;
            p.status_text.clear();
            p.status_emoji.clear();
            p.status_expires_at = 0;
            presence_changed_locked(uid, account, before, nullptr, out);
        }
    }
    // Outside the lock: a database write each, then the announcements.
    for (const auto& e : expired) auth_manager.clearExpiredStatusMessage(e.uid, e.expires_at);
    out.run();
}

bool SessionManager::check_dm_allowed(int64_t sender, int64_t recipient, AuthManager& auth_manager) {
    // Blocking applies regardless of the recipient's online state or
    // friends-only setting — a blocked user must not DM the person who
    // blocked them, even into the offline queue.
    if (auth_manager.isBlocked(sender, recipient)) {
        return false;
    }

    // Friends-only is saved on the account, so an online recipient (the
    // directory's copy) and an offline or invisible one (read from the DB)
    // get the same answer — the result must never reveal whether the
    // recipient is connected.
    std::optional<bool> friends_only;
    {
        std::lock_guard<std::mutex> lock(mutex_);
        auto it = accounts_.find(recipient);
        if (it != accounts_.end()) friends_only = it->second.presence.dm_friends_only;
    }
    if (!friends_only) friends_only = auth_manager.getDmFriendsOnly(recipient);
    if (!*friends_only) {
        return true;
    }

    // Recipient only allows DMs from friends (a DB round trip, outside the lock).
    return auth_manager.areFriends(sender, recipient);
}


// --- Server & Entry Point ---

class Server {
public:
    Server(boost::asio::io_context& io_context, short port, SessionManager& manager, AuthManager& auth_manager,
           boost::asio::thread_pool& cpu_pool, boost::asio::thread_pool& db_pool, AbuseLimits& limits)
        : io_context_(io_context),
          acceptor_(io_context, tcp::endpoint(tcp::v4(), port)),
          ssl_context_(ssl::context::tlsv12),
          manager_(manager),
          auth_manager_(auth_manager),
          cpu_pool_(cpu_pool),
          db_pool_(db_pool),
          limits_(limits),
          retry_timer_(io_context) {

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
    /// Out of file descriptors or memory: accept() fails again at once, so
    /// retrying immediately spins a core while nothing changes.
    static bool out_of_resources(const boost::system::error_code& ec) {
        if (ec.category() != boost::system::system_category()) return false;
        const int v = ec.value();
        return v == EMFILE || v == ENFILE || v == ENOBUFS || v == ENOMEM;
    }

    /// At most one log line per kind every 10 s: a flood must not become a
    /// flood of log lines too.
    bool should_log(std::chrono::steady_clock::time_point& last) {
        const auto now = std::chrono::steady_clock::now();
        if (now - last < std::chrono::seconds(10)) return false;
        last = now;
        return true;
    }

    void do_accept() {
        // Each connection gets its own strand: its handlers never run
        // concurrently, different connections run on different threads.
        // One accept is outstanding at a time, so these handlers (and the
        // members they touch) never run concurrently either.
        acceptor_.async_accept(boost::asio::make_strand(io_context_),
            [this](boost::system::error_code ec, tcp::socket socket) {
                if (ec == boost::asio::error::operation_aborted) return;  // shutting down
                if (ec) {
                    if (out_of_resources(ec)) {
                        if (should_log(last_resource_log_)) {
                            std::cerr << "[Server] accept failed (" << ec.message()
                                      << "); pausing new connections briefly\n";
                        }
                        retry_timer_.expires_after(std::chrono::milliseconds(100));
                        retry_timer_.async_wait([this](const boost::system::error_code& wait_ec) {
                            if (!wait_ec) do_accept();
                        });
                        return;
                    }
                    do_accept();  // e.g. the client reset before we accepted
                    return;
                }
                boost::system::error_code ep_ec;
                const auto endpoint = socket.remote_endpoint(ep_ec);
                if (ep_ec) {
                    do_accept();  // gone already; `socket` closes here
                    return;
                }
                std::string ip = endpoint.address().to_string();
                const auto admit = limits_.admit_connection(ip);
                if (admit != AbuseLimits::Admit::ok) {
                    ++refused_;
                    if (should_log(last_refusal_log_)) {
                        const char* why = admit == AbuseLimits::Admit::server_full ? "server full"
                                        : admit == AbuseLimits::Admit::address_full ? "too many from one address"
                                        : admit == AbuseLimits::Admit::address_rate ? "connecting too fast"
                                        : "address table full";
                        std::cout << "[Server] Refused " << refused_ << " connection(s) in the last 10 s (latest: "
                                  << ip << ", " << why << ")\n";
                        refused_ = 0;
                    }
                    boost::system::error_code close_ec;
                    socket.close(close_ec);
                    do_accept();
                    return;
                }
                std::shared_ptr<Session> session;
                try {
                    session = std::make_shared<Session>(std::move(socket), manager_, ssl_context_,
                                                        auth_manager_, cpu_pool_, db_pool_, limits_, ip);
                } catch (const std::exception& e) {
                    // Never constructed, so its destructor won't return the slot.
                    limits_.release_connection(ip);
                    std::cerr << "[Server] Couldn't set up a connection: " << e.what() << "\n";
                    do_accept();
                    return;
                }
                manager_.join(session);
                session->start();
                do_accept();
            });
    }
    boost::asio::io_context& io_context_;
    tcp::acceptor acceptor_;
    ssl::context ssl_context_;
    SessionManager& manager_;
    AuthManager& auth_manager_;
    boost::asio::thread_pool& cpu_pool_;
    boost::asio::thread_pool& db_pool_;
    AbuseLimits& limits_;
    boost::asio::steady_timer retry_timer_;
    std::chrono::steady_clock::time_point last_resource_log_{};
    std::chrono::steady_clock::time_point last_refusal_log_{};
    std::size_t refused_ = 0;
};

/// Raise the open-file soft limit to the hard one (every connection is a
/// descriptor; shells and systemd start services at a soft limit of 1024,
/// and systemd's LimitNOFILE= sets the hard one). Returns the limit now in
/// force.
static std::size_t raise_open_file_limit() {
#ifndef _WIN32
    rlimit rl{};
    if (getrlimit(RLIMIT_NOFILE, &rl) != 0) return 1024;
    if (rl.rlim_cur != RLIM_INFINITY && (rl.rlim_max == RLIM_INFINITY || rl.rlim_cur < rl.rlim_max)) {
        rlimit want = rl;
        // Linux refuses RLIM_INFINITY for this resource: cap at nr_open's default.
        want.rlim_cur = rl.rlim_max == RLIM_INFINITY ? 1048576 : rl.rlim_max;
        if (setrlimit(RLIMIT_NOFILE, &want) == 0) rl = want;
    }
    return rl.rlim_cur == RLIM_INFINITY ? 1048576 : static_cast<std::size_t>(rl.rlim_cur);
#else
    return 8192;
#endif
}

/// A thread count from the environment: `fallback` when unset or invalid.
static unsigned env_threads(const char* name, unsigned fallback) {
    if (const char* v = std::getenv(name)) {
        try {
            const long n = std::stol(v);
            if (n > 0 && n <= 256) return static_cast<unsigned>(n);
        } catch (...) {}
        std::cerr << "[Server] Ignoring " << name << "=" << v << " (expected 1..256)\n";
    }
    return fallback;
}

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

        // Threads (central-scaling-design.md): io threads serve the
        // connections (each on its own strand); the database pool runs the
        // handlers that query Postgres, many at once, so commits share disk
        // flushes (the DM rate is bound by flushes, not CPU); the CPU pool
        // hashes passwords, so a login burst queues there instead of stalling
        // everyone. Each thread may hold a Postgres connection: max_connections
        // must exceed their sum.
        const unsigned cores = std::max(2u, std::thread::hardware_concurrency());
        const unsigned io_threads = env_threads("DECIBELL_IO_THREADS", cores);
        const unsigned cpu_threads = env_threads("DECIBELL_CPU_THREADS", std::max(1u, cores / 2));
        // 64 by measurement (load.py, NVMe): 32 kept up with ~4,000 DMs/s,
        // 64 with ~7,600.
        unsigned db_threads = env_threads("DECIBELL_DB_THREADS", 64);
        // Every thread may hold a connection at once (plus the sweep's): fit
        // under PostgreSQL's limit, leaving room for psql and backups, rather
        // than failing queries at peak.
        if (const int available = auth_manager.availableConnections(); available > 0) {
            const long room = static_cast<long>(available) - 8 - io_threads - cpu_threads - 2;
            if (room < static_cast<long>(db_threads)) {
                db_threads = static_cast<unsigned>(std::max(4L, room));
                std::cerr << "[Server] PostgreSQL allows " << available << " connections: using " << db_threads
                          << " database threads (raise max_connections for more)\n";
            }
        }
        auth_manager.set_pool_idle(io_threads + cpu_threads + db_threads + 2);
        std::cout << "[Server] " << io_threads << " io threads, " << db_threads << " database threads, "
                  << cpu_threads << " CPU threads\n";

        // Connections stop short of the open-file limit, leaving room for
        // the database pool (a connection per thread), the log and the
        // listener: a server out of descriptors fails its queries too.
        const std::size_t fd_limit = raise_open_file_limit();
        const std::size_t reserve = io_threads + cpu_threads + db_threads + 2 + 64;
        std::size_t max_connections = fd_limit > reserve + 16 ? fd_limit - reserve : 16;
        if (const char* v = std::getenv("DECIBELL_MAX_CONNECTIONS")) {
            try {
                const long long n = std::stoll(v);
                if (n > 0) {
                    if (static_cast<std::size_t>(n) > max_connections) {
                        std::cerr << "[Server] DECIBELL_MAX_CONNECTIONS=" << n << " is above what the open-file limit ("
                                  << fd_limit << ") leaves room for (" << max_connections << ")\n";
                    }
                    max_connections = static_cast<std::size_t>(n);
                }
            } catch (...) {
                std::cerr << "[Server] Ignoring DECIBELL_MAX_CONNECTIONS=" << v << "\n";
            }
        }
        std::cout << "[Server] Open-file limit " << fd_limit << ": up to " << max_connections << " connections\n";
        AbuseLimits limits(max_connections);

        boost::asio::io_context io_context(static_cast<int>(io_threads));
        boost::asio::thread_pool cpu_pool(cpu_threads);
        boost::asio::thread_pool db_pool(db_threads);

        SessionManager manager;
        Server s(io_context, 8080, manager, auth_manager, cpu_pool, db_pool, limits);

        // Periodic sweep of stale sessions (no activity for 60s).
        // Catches dead connections where TCP FIN was never sent (e.g. client crash).
        // Hourly (from the first tick): forget renames past the reservation.
        boost::asio::steady_timer sweep_timer(io_context);
        std::function<void(const boost::system::error_code&)> sweep_fn;
        int sweeps_until_prune = 0;
        sweep_fn = [&](const boost::system::error_code& ec) {
            if (ec) return;
            manager.sweep_stale(std::chrono::seconds(60));
            manager.expire_status_messages(auth_manager);
            manager.sweep_calls();
            limits.sweep();
            if (sweeps_until_prune-- == 0) {
                auth_manager.pruneNameChanges();
                sweeps_until_prune = 120;
            }
            sweep_timer.expires_after(std::chrono::seconds(30));
            sweep_timer.async_wait(sweep_fn);
        };
        sweep_timer.expires_after(std::chrono::seconds(30));
        sweep_timer.async_wait(sweep_fn);

        // SIGINT / SIGTERM (systemd stop): stop serving and exit cleanly —
        // threads joined, connections and the database pool released.
        boost::asio::signal_set signals(io_context, SIGINT, SIGTERM);
        signals.async_wait([&io_context](const boost::system::error_code& ec, int signo) {
            if (ec) return;
            std::cout << "[Server] Signal " << signo << ": shutting down\n";
            io_context.stop();
        });

        std::cout << "Decibell Central Server running on port 8080...\n";

        // A handler that throws must not take its thread (or the server)
        // down: log it and keep serving.
        auto run_io = [&io_context]() {
            for (;;) {
                try {
                    io_context.run();
                    return;
                } catch (const std::exception& e) {
                    std::cerr << "[Server] Handler threw: " << e.what() << "\n";
                }
            }
        };
        std::vector<std::thread> threads;
        threads.reserve(io_threads - 1);
        for (unsigned i = 1; i < io_threads; ++i) threads.emplace_back(run_io);
        run_io();
        for (auto& t : threads) t.join();
        // Handlers and password checks still running finish (their
        // continuations land in the stopped io_context and are dropped
        // with it).
        db_pool.join();
        cpu_pool.join();
        std::cout << "[Server] Stopped.\n";
    } catch (std::exception& e) {
        std::cerr << "Exception: " << e.what() << "\n";
    }
    return 0;
}