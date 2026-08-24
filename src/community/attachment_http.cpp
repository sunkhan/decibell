#include "attachment_http.hpp"

#include <jwt-cpp/traits/nlohmann-json/defaults.h>
#include <nlohmann/json.hpp>

#include <algorithm>
#include <cctype>
#include <chrono>
#include <cerrno>
#include <cstring>
#include <cstdio>
#include <filesystem>
#include <fstream>
#include <iostream>
#include <memory>
#include <optional>
#include <string>
#include <system_error>
#include <unordered_map>
#include <utility>
#include <vector>

#include "db.hpp"
#include "authz.hpp"
#include "rate_limit.hpp"

namespace ssl = boost::asio::ssl;
using boost::asio::ip::tcp;
using json = nlohmann::json;

namespace {

// -------- small helpers shared across connection states --------

std::string lower(std::string s) {
    std::transform(s.begin(), s.end(), s.begin(),
                   [](unsigned char c) { return std::tolower(c); });
    return s;
}

bool starts_with(const std::string& s, const char* prefix) {
    const size_t n = std::strlen(prefix);
    return s.size() >= n && std::memcmp(s.data(), prefix, n) == 0;
}

// Classify a mime type into the enum the rest of the system uses. Matches
// chatproj::Attachment::Kind values (IMAGE=0, VIDEO=1, DOCUMENT=2, AUDIO=3).
int kind_from_mime(const std::string& mime) {
    const std::string m = lower(mime);
    if (starts_with(m, "image/")) return 0;
    if (starts_with(m, "video/")) return 1;
    if (starts_with(m, "audio/")) return 3;
    return 2; // document / everything else
}

// Replace or strip anything that isn't a safe filename char. Preserves
// extension. We don't actually need the original name to be recoverable;
// storage_path includes the numeric id so filesystem uniqueness is trivial.
std::string sanitize_filename(const std::string& raw) {
    std::string out;
    out.reserve(raw.size());
    for (char c : raw) {
        if (c == '/' || c == '\\' || c == '\0') continue;
        // Control chars and spaces → underscore; keep alnum, dot, dash, underscore.
        unsigned char uc = static_cast<unsigned char>(c);
        if (uc < 0x20 || uc == 0x7f) { out.push_back('_'); continue; }
        if (std::isalnum(uc) || c == '.' || c == '-' || c == '_') {
            out.push_back(c);
        } else {
            out.push_back('_');
        }
    }
    if (out.empty()) out = "file";
    if (out.size() > 128) out.resize(128);
    return out;
}

// Validate an Ed25519 JWT with central's public key (same check as the
// TCP layer). On success returns the `sub` claim (username).
std::optional<std::string> verify_jwt(const std::string& token, const std::string& public_pem) {
    try {
        auto decoded = jwt::decode(token);
        jwt::verify()
            .allow_algorithm(jwt::algorithm::ed25519{public_pem, ""})
            .with_issuer("decibell_central_auth")
            .verify(decoded);
        if (decoded.get_subject().empty()) return std::nullopt;
        return decoded.get_subject();
    } catch (const std::exception&) {
        return std::nullopt;
    }
}

// -------- parsed request model --------

struct HttpRequest {
    std::string method;
    std::string version;                                 // "HTTP/1.1" / "HTTP/1.0"
    std::string path;                                    // path only, no query
    std::unordered_map<std::string, std::string> query;
    std::unordered_map<std::string, std::string> headers; // lowercased keys
    int64_t content_length = 0;
    std::string authorization_token; // bearer token, empty if missing

    // Parse the raw request head (start-line + headers, terminated by CRLF
    // CRLF). Returns false on malformed input. Does NOT read the body.
    bool parse_head(const std::string& head);
    std::string header(const std::string& key_lower) const {
        auto it = headers.find(key_lower);
        return it == headers.end() ? std::string() : it->second;
    }
};

bool HttpRequest::parse_head(const std::string& head) {
    auto pos = head.find("\r\n");
    if (pos == std::string::npos) return false;
    // Request line: METHOD SP PATH SP HTTP/1.1
    const std::string line = head.substr(0, pos);
    auto sp1 = line.find(' ');
    if (sp1 == std::string::npos) return false;
    auto sp2 = line.find(' ', sp1 + 1);
    if (sp2 == std::string::npos) return false;
    method = line.substr(0, sp1);
    version = line.substr(sp2 + 1);
    std::string raw_path = line.substr(sp1 + 1, sp2 - sp1 - 1);

    // Split query string.
    auto q = raw_path.find('?');
    if (q == std::string::npos) {
        path = std::move(raw_path);
    } else {
        path = raw_path.substr(0, q);
        std::string qs = raw_path.substr(q + 1);
        size_t i = 0;
        while (i < qs.size()) {
            auto amp = qs.find('&', i);
            std::string pair = qs.substr(i, amp == std::string::npos ? std::string::npos : amp - i);
            auto eq = pair.find('=');
            if (eq != std::string::npos) {
                query.emplace(pair.substr(0, eq), pair.substr(eq + 1));
            } else if (!pair.empty()) {
                query.emplace(pair, std::string());
            }
            if (amp == std::string::npos) break;
            i = amp + 1;
        }
    }

    // Headers.
    size_t cursor = pos + 2;
    while (cursor < head.size()) {
        auto eol = head.find("\r\n", cursor);
        if (eol == std::string::npos) break;
        if (eol == cursor) break; // blank line = end
        std::string hline = head.substr(cursor, eol - cursor);
        cursor = eol + 2;
        auto colon = hline.find(':');
        if (colon == std::string::npos) continue;
        std::string name = lower(hline.substr(0, colon));
        std::string value = hline.substr(colon + 1);
        while (!value.empty() && (value.front() == ' ' || value.front() == '\t')) value.erase(value.begin());
        while (!value.empty() && (value.back() == ' ' || value.back() == '\t' || value.back() == '\r')) value.pop_back();
        headers[name] = std::move(value);
    }

    if (auto it = headers.find("content-length"); it != headers.end()) {
        try { content_length = std::stoll(it->second); } catch (...) { content_length = 0; }
    }
    if (auto it = headers.find("authorization"); it != headers.end()) {
        const std::string& v = it->second;
        if (v.size() > 7 && lower(v.substr(0, 7)) == "bearer ") {
            authorization_token = v.substr(7);
        }
    }
    return true;
}

// Validate a stored MIME type before reflecting it into a response header.
// `mime` is client-controlled at upload time and stored verbatim, so a
// value containing CR/LF would split the HTTP response (header injection),
// and a `text/html`/`image/svg+xml` value would let an uploaded blob run
// script in the community server's own TLS origin. Reject anything that
// isn't a well-formed `type/subtype` over a safe charset, falling back to
// a benign default. Paired with X-Content-Type-Options: nosniff and
// Content-Disposition: attachment on the serve path.
static std::string safe_content_type(const std::string& mime) {
    if (mime.empty() || mime.size() > 128) return "application/octet-stream";
    auto ok = [](char c) {
        return (c >= 'a' && c <= 'z') || (c >= 'A' && c <= 'Z') ||
               (c >= '0' && c <= '9') || c == '/' || c == '.' ||
               c == '+' || c == '-';
    };
    int slashes = 0;
    for (char c : mime) {
        if (!ok(c)) return "application/octet-stream";
        if (c == '/') slashes++;
    }
    if (slashes != 1 || mime.front() == '/' || mime.back() == '/')
        return "application/octet-stream";
    return mime;
}

// -------- connection handler --------

// Each incoming TCP/TLS connection lives inside one AttachmentConnection.
// Lifetime is extended via shared_ptr so the async handler chain keeps the
// object alive until the last completion callback fires.
// Per-username bucket for POST /attachments/init (one DB row + one
// directory + one file per call). Shared across connections; the server
// is single-threaded so a plain map is fine.
class InitRateLimiter {
public:
    bool allow(const std::string& username) {
        auto it = buckets_.find(username);
        if (it == buckets_.end()) {
            if (buckets_.size() >= 4096) buckets_.clear();
            it = buckets_.emplace(username, chatproj::TokenBucket(20, 0.5)).first;
        }
        return it->second.try_take();
    }
private:
    std::unordered_map<std::string, chatproj::TokenBucket> buckets_;
};
InitRateLimiter& init_rate_limiter() {
    static InitRateLimiter l;
    return l;
}

class AttachmentConnection : public std::enable_shared_from_this<AttachmentConnection> {
public:
    AttachmentConnection(tcp::socket socket,
                         ssl::context& ssl_ctx,
                         chatproj::CommunityDb& db,
                         const chatproj::Authorizer* authz,
                         const std::string& jwt_secret,
                         const std::string& storage_root,
                         int64_t max_attachment_bytes)
        : socket_(std::move(socket), ssl_ctx),
          shutdown_timer_(socket_.lowest_layer().get_executor()),
          deadline_timer_(socket_.lowest_layer().get_executor()),
          db_(db),
          authz_(authz),
          jwt_secret_(jwt_secret),
          storage_root_(storage_root),
          max_attachment_bytes_(max_attachment_bytes) {}

    void start() {
        auto self = shared_from_this();
        arm_deadline();
        socket_.async_handshake(ssl::stream_base::server,
            [this, self](const boost::system::error_code& ec) {
                if (ec) return;
                read_head();
            });
    }

private:
    // Inactivity deadline, re-armed on every read/write progress. A peer
    // that handshakes and trickles (or never sends) the request head,
    // or stalls mid-PATCH, used to hold this connection — and in PATCH
    // an open FILE* — for as long as it liked.
    static constexpr auto kInactivity = std::chrono::seconds(30);
    void arm_deadline() {
        auto self = shared_from_this();
        // Generation guard: expires_after() cannot cancel a wait whose
        // completion is already queued — it fires with ec == success even
        // though we just re-armed. Stamping each arming and checking it here
        // makes such a stale completion a no-op instead of a spurious close.
        const uint64_t gen = ++deadline_gen_;
        deadline_timer_.expires_after(kInactivity);
        deadline_timer_.async_wait([this, self, gen](const boost::system::error_code& ec) {
            if (ec || gen != deadline_gen_) return;
            if (patch_fp_ && *patch_fp_) { std::fclose(*patch_fp_); *patch_fp_ = nullptr; }
            boost::system::error_code ignore;
            socket_.lowest_layer().close(ignore);
        });
    }

    // ---- reading request head ----

    void read_head() {
        auto self = shared_from_this();
        // Read until CRLFCRLF. Bounded: we allow up to 16KB of headers.
        boost::asio::async_read_until(socket_, head_buf_, "\r\n\r\n",
            [this, self](const boost::system::error_code& ec, std::size_t n) {
                if (ec) return;
                arm_deadline();
                if (n > 16 * 1024) { send_error(431, "Request Header Fields Too Large"); return; }
                std::string head{
                    boost::asio::buffers_begin(head_buf_.data()),
                    boost::asio::buffers_begin(head_buf_.data()) + n
                };
                head_buf_.consume(n);
                if (!req_.parse_head(head)) { send_error(400, "Bad Request"); return; }
                on_head_ready();
            });
    }

    void on_head_ready() {
        // HTTP/1.1 keep-alive (P8): one TLS handshake per *connection*
        // instead of per request — a 100 MB upload in 256 KB PATCHes used
        // to pay ~400 handshakes. HTTP/1.1 defaults to persistent unless
        // `Connection: close`; HTTP/1.0 only with an explicit keep-alive.
        // Any error response or a request whose body we don't consume
        // forces a close (we can't resync the stream), see force_close_.
        {
            const std::string conn = lower(req_.header("connection"));
            keep_alive_ = req_.version == "HTTP/1.0" ? conn == "keep-alive"
                                                     : conn != "close";
            force_close_ = false;
            // Bodies on GET/HEAD/DELETE are never read — don't reuse then.
            if ((req_.method == "GET" || req_.method == "HEAD" || req_.method == "DELETE") &&
                req_.content_length > 0) {
                force_close_ = true;
            }
        }
        // All endpoints require an Authorization header. Single failure path
        // keeps the dispatch below clean.
        if (req_.authorization_token.empty()) { send_error(401, "Unauthorized"); return; }
        auto who = verify_jwt(req_.authorization_token, jwt_secret_);
        if (!who) { send_error(401, "Unauthorized"); return; }
        username_ = *who;

        // Route: match method + path. Paths carry a numeric id in the second
        // segment, so we parse it out where needed.
        if (req_.method == "POST" && req_.path == "/attachments/init") {
            return handle_init();
        }
        int64_t id = 0;
        std::string tail;
        if (parse_attachment_path(req_.path, id, tail)) {
            if (req_.method == "PATCH" && tail.empty())              return handle_patch(id);
            if (req_.method == "HEAD"  && tail.empty())              return handle_head_status(id);
            if (req_.method == "GET"   && tail.empty())              return handle_get(id);
            if (req_.method == "DELETE" && tail.empty())             return handle_delete(id);
            if (req_.method == "POST"  && tail == "/complete")       return handle_complete(id);
            if (req_.method == "POST"  && tail == "/thumbnail")      return handle_thumbnail_upload(id);
        }
        send_error(404, "Not Found");
    }

    // Parses "/attachments/<id>" or "/attachments/<id>/suffix". Returns true
    // when the path matches, writing the id and trailing suffix (empty if
    // none) to the outputs.
    static bool parse_attachment_path(const std::string& path, int64_t& id_out, std::string& tail_out) {
        constexpr const char* prefix = "/attachments/";
        constexpr size_t prefix_len = 13;
        if (path.size() < prefix_len + 1 || std::memcmp(path.data(), prefix, prefix_len) != 0) return false;
        size_t i = prefix_len;
        while (i < path.size() && std::isdigit(static_cast<unsigned char>(path[i]))) ++i;
        if (i == prefix_len) return false;
        try { id_out = std::stoll(path.substr(prefix_len, i - prefix_len)); }
        catch (...) { return false; }
        tail_out = path.substr(i);
        return true;
    }

    // ---- endpoint: POST /attachments/init ----

    void handle_init() {
        if (req_.content_length <= 0 || req_.content_length > 64 * 1024) {
            send_error(400, "Bad Request");
            return;
        }
        body_.resize(static_cast<size_t>(req_.content_length));
        // We may have some body bytes already in head_buf_ if the TCP read
        // overshot the CRLFCRLF boundary. Pull those out first, then read the
        // rest off the wire.
        size_t already = std::min(head_buf_.size(), body_.size());
        if (already > 0) {
            std::memcpy(body_.data(),
                        boost::asio::buffers_begin(head_buf_.data()).operator->(),
                        already);
            head_buf_.consume(already);
        }
        if (already == body_.size()) return do_init_with_body();
        auto self = shared_from_this();
        boost::asio::async_read(socket_,
            boost::asio::buffer(body_.data() + already, body_.size() - already),
            [this, self](const boost::system::error_code& ec, std::size_t) {
                if (ec) return;
                do_init_with_body();
            });
    }

    void do_init_with_body() {
        std::string filename, mime, channel_id, placeholder;
        int64_t size = 0;
        int32_t width = 0, height = 0, duration_ms = 0;
        try {
            auto j = json::parse(std::string(body_.begin(), body_.end()));
            channel_id = j.value("channelId", "");
            filename   = j.value("filename",  "");
            mime       = j.value("mime",      "application/octet-stream");
            size       = j.value("size",      (int64_t)0);
            // Optional: uploader client reads image dimensions and forwards
            // them so downstream viewers can reserve the right placeholder
            // size before the image data URL loads. Zero = unknown.
            width      = j.value("width",     0);
            height     = j.value("height",    0);
            // Audio + video duration in ms, read at upload time client-side.
            // Drives "0:00 / 3:45" labels before the file is downloaded.
            duration_ms = j.value("durationMs", 0);
            // Opaque base64 ThumbHash the uploader computed from the
            // same bitmap it derives thumbnails from. Never decoded
            // here — stored and echoed so viewers can paint a blurred
            // preview with no extra request. Bounded because it's
            // client-supplied; a real hash is ~34 chars.
            placeholder = j.value("placeholder", std::string());
            if (placeholder.size() > 128) placeholder.clear();
        } catch (...) { send_error(400, "Bad Request"); return; }

        if (channel_id.empty() || filename.empty() || size <= 0) {
            send_error(400, "Bad Request"); return;
        }
        if (max_attachment_bytes_ > 0 && size > max_attachment_bytes_) {
            send_error(413, "Payload Too Large"); return;
        }
        if (!db_.is_member(username_))      { send_error(403, "Forbidden"); return; }
        if (!init_rate_limiter().allow(username_)) { send_error(429, "Too Many Requests"); return; }
        // Category headers carry no messages — nothing can bind an
        // attachment uploaded against one, so refuse at init.
        auto target_channel = db_.get_channel(channel_id);
        if (!target_channel || target_channel->type == 2) {
            send_error(404, "Not Found"); return;
        }
        // Permissions v2: uploading is "sending a message with a file".
        if (authz_) {
            const chatproj::AuthCtx ctx{username_, channel_id, ""};
            if (!authz_->check(chatproj::Action::SendMessage, ctx) ||
                !authz_->check(chatproj::Action::AttachFiles, ctx)) {
                send_error(403, "Forbidden"); return;
            }
        }

        // Disk headroom: refuse an upload that would leave the store's
        // volume with less than the configured minimum free space (H1).
        // Cross-platform via std::filesystem::space (statvfs /
        // GetDiskFreeSpaceEx); the ec overload keeps a stat failure off the
        // throwing path. On query failure we allow (log) rather than break
        // uploads. `available` is what's usable by this process.
        {
            std::error_code space_ec;
            const auto info = std::filesystem::space(storage_root_, space_ec);
            if (!space_ec) {
                const int64_t min_free = db_.min_free_bytes();
                const int64_t avail = static_cast<int64_t>(info.available);
                if (avail - size < min_free) {
                    std::cerr << "[AttachmentHttp] init: refusing " << size
                              << "B upload from " << username_ << " — only " << avail
                              << "B free, keep >= " << min_free << "B\n";
                    send_error(507, "Insufficient Storage"); return;
                }
            } else {
                std::cerr << "[AttachmentHttp] init: filesystem::space('" << storage_root_
                          << "') failed: " << space_ec.message() << " — allowing upload\n";
            }
        }

        const std::string safe_name = sanitize_filename(filename);
        const int kind = kind_from_mime(mime);

        // Make the channel directory, then insert with an empty placeholder
        // storage_path (we need the autoincrement id to build the final one).
        std::filesystem::path dir = std::filesystem::path(storage_root_) / channel_id;
        std::error_code mkdir_ec;
        std::filesystem::create_directories(dir, mkdir_ec);
        if (mkdir_ec) {
            std::cerr << "[AttachmentHttp] init: mkdir '" << dir.string()
                      << "' failed: " << mkdir_ec.message() << "\n";
            send_error(500, "Internal Server Error"); return;
        }

        const int64_t new_id = db_.insert_pending_attachment(
            channel_id, kind, filename, mime, size, /*storage_path*/ "", username_,
            /*position*/ 0, width, height, duration_ms, placeholder);
        if (new_id == 0) {
            std::cerr << "[AttachmentHttp] init: insert_pending_attachment "
                         "returned 0 (see [DB] log for SQLite error)\n";
            send_error(500, "Internal Server Error"); return;
        }

        // Final path: <root>/<channel>/<id>_<safe_name>. .partial variant used
        // during upload so completion is a rename-only atomic flip.
        const std::string final_path =
            (dir / (std::to_string(new_id) + "_" + safe_name)).string();
        if (!db_.update_attachment_storage_path(new_id, final_path)) {
            std::cerr << "[AttachmentHttp] init: update_attachment_storage_path "
                         "for id=" << new_id << " failed\n";
            std::error_code cleanup_ec;
            std::filesystem::remove(final_path + ".partial", cleanup_ec);
            send_error(500, "Internal Server Error"); return;
        }

        // Create an empty .partial file so HEAD can return offset=0 without
        // stat() races.
        {
            std::ofstream f(final_path + ".partial", std::ios::binary | std::ios::trunc);
            if (!f.good()) {
                std::cerr << "[AttachmentHttp] init: cannot create '"
                          << (final_path + ".partial")
                          << "' (errno=" << errno << ": " << std::strerror(errno)
                          << ")\n";
                send_error(500, "Internal Server Error"); return;
            }
        }

        json resp = { {"id", new_id}, {"uploadOffset", 0} };
        send_json(201, "Created", resp.dump());
    }

    // ---- endpoint: PATCH /attachments/<id> ----

    void handle_patch(int64_t id) {
        auto att = db_.get_attachment(id);
        if (!att)                                { send_error(404, "Not Found"); return; }
        if (att->upload_status != "uploading")   { send_error(409, "Conflict"); return; }
        if (att->uploader != username_)          { send_error(403, "Forbidden"); return; }

        int64_t offset = 0;
        try { offset = std::stoll(req_.header("upload-offset")); }
        catch (...) { send_error(400, "Bad Request"); return; }
        if (offset < 0) { send_error(400, "Bad Request"); return; }

        const std::string partial_path = att->storage_path + ".partial";
        std::error_code ec;
        const int64_t cur_size = static_cast<int64_t>(
            std::filesystem::exists(partial_path, ec)
                ? std::filesystem::file_size(partial_path, ec) : 0);
        if (ec) { send_error(500, "Internal Server Error"); return; }
        if (offset != cur_size) {
            // Client's offset disagrees with ours. The tus-style contract is
            // 409 so the client knows to HEAD and realign.
            send_error(409, "Conflict"); return;
        }
        if (req_.content_length < 0) { send_error(411, "Length Required"); return; }
        // Overflow-safe caps. `offset + content_length` is signed 64-bit;
        // a Content-Length near INT64_MAX would wrap negative and pass both
        // `>` checks, letting the upload blow past expected_size AND
        // max_attachment_bytes_ (disk-fill DoS). offset == cur_size here
        // (checked above) so `limit - offset` is a non-negative headroom.
        if (att->expected_size > 0 &&
            req_.content_length > att->expected_size - offset) {
            send_error(413, "Payload Too Large"); return;
        }
        if (max_attachment_bytes_ > 0 &&
            req_.content_length > max_attachment_bytes_ - offset) {
            send_error(413, "Payload Too Large"); return;
        }

        auto self = shared_from_this();
        auto fp = std::make_shared<std::FILE*>(
            std::fopen(partial_path.c_str(), "r+b"));
        if (!*fp) {
            if (std::FILE* f = std::fopen(partial_path.c_str(), "w+b")) {
                *fp = f;
            }
        }
        if (!*fp) { send_error(500, "Internal Server Error"); return; }
        if (std::fseek(*fp, offset, SEEK_SET) != 0) {
            std::fclose(*fp); *fp = nullptr;
            send_error(500, "Internal Server Error"); return;
        }

        patch_id_     = id;
        patch_final_  = offset + req_.content_length;
        patch_remain_ = req_.content_length;
        patch_fp_     = fp;

        // Consume any body bytes already buffered behind the head.
        if (head_buf_.size() > 0) {
            auto data = head_buf_.data();
            const size_t have = std::min<size_t>(head_buf_.size(),
                                                 static_cast<size_t>(patch_remain_));
            const char* src = boost::asio::buffers_begin(data).operator->();
            if (std::fwrite(src, 1, have, *patch_fp_) != have) {
                std::fclose(*patch_fp_); *patch_fp_ = nullptr;
                send_error(500, "Internal Server Error"); return;
            }
            head_buf_.consume(have);
            patch_remain_ -= static_cast<int64_t>(have);
        }
        if (patch_remain_ == 0) return finish_patch();
        read_patch_chunk();
    }

    void read_patch_chunk() {
        auto self = shared_from_this();
        const size_t want = static_cast<size_t>(std::min<int64_t>(patch_remain_,
                                                                   PATCH_BUF_SIZE));
        patch_chunk_.resize(want);
        boost::asio::async_read(socket_, boost::asio::buffer(patch_chunk_),
            [this, self](const boost::system::error_code& ec, std::size_t n) {
                if (ec) {
                    if (patch_fp_ && *patch_fp_) { std::fclose(*patch_fp_); *patch_fp_ = nullptr; }
                    return;
                }
                // The inactivity deadline can fire and close us (nulling the
                // FILE*) between this read being queued and running — the
                // timer handler and this completion may sit in the same
                // reactor batch. Bail before touching *patch_fp_; writing to
                // the nulled stream used to SIGSEGV the whole server (A1).
                if (!patch_fp_ || !*patch_fp_) return;
                arm_deadline();
                if (std::fwrite(patch_chunk_.data(), 1, n, *patch_fp_) != n) {
                    std::fclose(*patch_fp_); *patch_fp_ = nullptr;
                    send_error(500, "Internal Server Error"); return;
                }
                patch_remain_ -= static_cast<int64_t>(n);
                if (patch_remain_ == 0) return finish_patch();
                read_patch_chunk();
            });
    }

    void finish_patch() {
        // fwrite() only buffers; a full disk (ENOSPC) surfaces at fflush /
        // fclose, not at the earlier fwrite. Ignoring those return values
        // made the 204 lie — it reported patch_final_ bytes stored when
        // fewer actually reached disk, so the client happily advanced and
        // later "completed" a short file (X8). Check both and fail with 500
        // instead; the partial file is left intact so the client can re-HEAD
        // and resume from the real on-disk offset.
        bool write_ok = true;
        if (patch_fp_ && *patch_fp_) {
            if (std::fflush(*patch_fp_) != 0) write_ok = false;
            if (std::fclose(*patch_fp_) != 0) write_ok = false;
            *patch_fp_ = nullptr;
        }
        if (!write_ok) {
            std::cerr << "[AttachmentHttp] patch: flush/close failed (disk full?) for id "
                      << patch_id_ << "\n";
            send_error(500, "Internal Server Error");
            return;
        }
        // Respond 204 with Upload-Offset so the client knows where we are.
        std::string resp =
            "HTTP/1.1 204 No Content\r\n"
            "Upload-Offset: " + std::to_string(patch_final_) + "\r\n" +
            conn_header() +
            "Content-Length: 0\r\n\r\n";
        send_response(std::move(resp));
    }

    // ---- endpoint: HEAD /attachments/<id> ----

    void handle_head_status(int64_t id) {
        auto att = db_.get_attachment(id);
        if (!att) { send_error(404, "Not Found"); return; }
        if (att->upload_status == "uploading" && att->uploader != username_) {
            send_error(403, "Forbidden"); return;
        }
        if (att->upload_status == "ready" && !db_.is_member(username_)) {
            send_error(403, "Forbidden"); return;
        }
        int64_t offset = 0;
        if (att->upload_status == "uploading") {
            const std::string partial = att->storage_path + ".partial";
            std::error_code ec;
            if (std::filesystem::exists(partial, ec))
                offset = static_cast<int64_t>(std::filesystem::file_size(partial, ec));
        } else {
            offset = att->size_bytes;
        }
        std::string resp =
            "HTTP/1.1 200 OK\r\n"
            "Upload-Offset: " + std::to_string(offset) + "\r\n"
            "Upload-Length: " + std::to_string(att->expected_size) + "\r\n"
            "Upload-Status: " + att->upload_status + "\r\n" +
            conn_header() +
            "Content-Length: 0\r\n\r\n";
        send_response(std::move(resp));
    }

    // ---- endpoint: POST /attachments/<id>/complete ----

    void handle_complete(int64_t id) {
        auto att = db_.get_attachment(id);
        if (!att)                              { send_error(404, "Not Found"); return; }
        if (att->upload_status != "uploading") { send_error(409, "Conflict"); return; }
        if (att->uploader != username_)        { send_error(403, "Forbidden"); return; }

        const std::string partial = att->storage_path + ".partial";
        std::error_code ec;
        if (!std::filesystem::exists(partial, ec)) { send_error(409, "Conflict"); return; }
        const int64_t actual = static_cast<int64_t>(std::filesystem::file_size(partial, ec));
        if (ec) { send_error(500, "Internal Server Error"); return; }
        if (att->expected_size > 0 && actual != att->expected_size) {
            send_error(409, "Conflict"); return;
        }

        std::filesystem::rename(partial, att->storage_path, ec);
        if (ec) { send_error(500, "Internal Server Error"); return; }
        if (!db_.complete_attachment(id, actual)) {
            // Try to revert the rename so a retry can succeed.
            std::error_code revert_ec;
            std::filesystem::rename(att->storage_path, partial, revert_ec);
            send_error(500, "Internal Server Error"); return;
        }

        // Echo the ready attachment back so the client can construct the
        // ChannelMessage.attachments entry without a second round trip.
        json resp = {
            {"id",         att->id},
            {"kind",       att->kind},
            {"filename",   att->filename},
            {"mime",       att->mime},
            {"sizeBytes",  actual},
            {"uploadStatus", "ready"},
        };
        send_json(200, "OK", resp.dump());
    }

    // ---- endpoint: GET /attachments/<id> (with Range: bytes=) ----

    void handle_get(int64_t id) {
        auto att = db_.get_attachment(id);
        if (!att)                               { send_error(404, "Not Found"); return; }
        if (att->upload_status != "ready")      { send_error(404, "Not Found"); return; }
        if (att->purged_at != 0)                { send_error(410, "Gone"); return; }
        if (!db_.is_member(username_))          { send_error(403, "Forbidden"); return; }
        // Permissions v2: a private channel's files are private too.
        // Legacy rows (channel_id '') fall back to the membership check.
        if (authz_ && !att->channel_id.empty() &&
            !authz_->check(chatproj::Action::ViewChannel, {username_, att->channel_id, ""})) {
            send_error(403, "Forbidden"); return;
        }

        // ?variant=thumb[&size=N] diverts to a JPEG thumbnail file.
        // Reuses the same GET endpoint and auth check rather than
        // introducing a parallel route. Range/partial isn't worth
        // supporting for tiny thumbs.
        //
        // Size resolution: if `size` is one of 320/640/1280, look for
        // <storage_path>.thumb-Npx.jpg. If `size` is missing, pick the
        // largest available. Legacy single-size uploads fall back to
        // <storage_path>.thumb.jpg when size=320 or no size given.
        const auto var_it = req_.query.find("variant");
        if (var_it != req_.query.end() && var_it->second == "thumb") {
            if (att->thumbnail_size_bytes <= 0) { send_error(404, "Not Found"); return; }

            // Parse requested size (optional). 0 = "pick best".
            int requested = 0;
            if (const auto sz_it = req_.query.find("size"); sz_it != req_.query.end()) {
                try { requested = std::stoi(sz_it->second); } catch (...) { requested = 0; }
                if (requested != 320 && requested != 640 && requested != 1280) {
                    send_error(400, "Bad Request"); return;
                }
            }

            // Pick the served size: requested if available, else nearest
            // smaller that exists, else largest available, else legacy.
            const int mask = att->thumbnail_sizes_mask;
            const auto bit_for = [](int sz) -> int {
                if (sz == 320) return 1;
                if (sz == 640) return 2;
                if (sz == 1280) return 4;
                return 0;
            };
            int chosen = 0;
            if (requested != 0 && (mask & bit_for(requested))) {
                chosen = requested;
            } else {
                // Pick the smallest size >= requested. If requested = 0
                // (no preference), pick the largest available.
                const int prefs[3] = { 320, 640, 1280 };
                if (requested == 0) {
                    for (int i = 2; i >= 0; --i) {
                        if (mask & bit_for(prefs[i])) { chosen = prefs[i]; break; }
                    }
                } else {
                    for (int i = 0; i < 3; ++i) {
                        if (prefs[i] >= requested && (mask & bit_for(prefs[i]))) {
                            chosen = prefs[i]; break;
                        }
                    }
                    // Fallback to largest available if none >= requested.
                    if (chosen == 0) {
                        for (int i = 2; i >= 0; --i) {
                            if (mask & bit_for(prefs[i])) { chosen = prefs[i]; break; }
                        }
                    }
                }
            }

            std::string thumb_path;
            if (chosen != 0) {
                thumb_path = att->storage_path + ".thumb-" + std::to_string(chosen) + "px.jpg";
            } else {
                // Legacy upload: mask=0 but bytes>0 means a single
                // .thumb.jpg lives next to the file (size 320).
                thumb_path = att->storage_path + ".thumb.jpg";
            }

            std::error_code thumb_ec;
            if (!std::filesystem::exists(thumb_path, thumb_ec)) {
                // Final fallback: legacy file when the size-named one
                // doesn't exist. Covers a partially-migrated row.
                thumb_path = att->storage_path + ".thumb.jpg";
                if (!std::filesystem::exists(thumb_path, thumb_ec)) {
                    send_error(404, "Not Found"); return;
                }
            }
            const int64_t thumb_total = static_cast<int64_t>(
                std::filesystem::file_size(thumb_path, thumb_ec));
            if (thumb_ec) { send_error(500, "Internal Server Error"); return; }
            auto thumb_fp = std::make_shared<std::FILE*>(
                std::fopen(thumb_path.c_str(), "rb"));
            if (!*thumb_fp) { send_error(500, "Internal Server Error"); return; }
            std::string thumb_headers =
                "HTTP/1.1 200 OK\r\n"
                "Content-Type: image/jpeg\r\n"
                "Content-Length: " + std::to_string(thumb_total) + "\r\n" +
                conn_header() + "\r\n";
            auto self_t = shared_from_this();
            auto hdr_t = std::make_shared<std::string>(std::move(thumb_headers));
            boost::asio::async_write(socket_, boost::asio::buffer(*hdr_t),
                [this, self_t, hdr_t, thumb_fp, thumb_total](
                    const boost::system::error_code& ec, std::size_t) {
                    if (ec) { std::fclose(*thumb_fp); return; }
                    send_file_body(thumb_fp, thumb_total);
                });
            return;
        }

        std::error_code ec;
        if (!std::filesystem::exists(att->storage_path, ec)) {
            send_error(410, "Gone"); return;
        }
        const int64_t total = static_cast<int64_t>(std::filesystem::file_size(att->storage_path, ec));
        if (ec) { send_error(500, "Internal Server Error"); return; }

        int64_t start = 0, end = total - 1;
        bool partial = false;
        const std::string range_hdr = req_.header("range");
        if (!range_hdr.empty() && starts_with(range_hdr, "bytes=")) {
            const std::string r = range_hdr.substr(6);
            auto dash = r.find('-');
            if (dash == std::string::npos) { send_error(416, "Range Not Satisfiable"); return; }
            try {
                if (dash > 0) start = std::stoll(r.substr(0, dash));
                if (dash + 1 < r.size()) end = std::stoll(r.substr(dash + 1));
            } catch (...) { send_error(416, "Range Not Satisfiable"); return; }
            if (start < 0 || start >= total || end < start || end >= total) {
                send_error(416, "Range Not Satisfiable"); return;
            }
            partial = true;
        }

        auto fp = std::make_shared<std::FILE*>(std::fopen(att->storage_path.c_str(), "rb"));
        if (!*fp) { send_error(500, "Internal Server Error"); return; }
        if (start > 0 && std::fseek(*fp, start, SEEK_SET) != 0) {
            std::fclose(*fp); send_error(500, "Internal Server Error"); return;
        }

        std::string headers;
        const int64_t body_len = end - start + 1;
        if (partial) {
            headers = "HTTP/1.1 206 Partial Content\r\n";
            headers += "Content-Range: bytes " + std::to_string(start) + "-" +
                       std::to_string(end) + "/" + std::to_string(total) + "\r\n";
        } else {
            headers = "HTTP/1.1 200 OK\r\n";
        }
        headers += "Content-Type: "   + safe_content_type(att->mime) + "\r\n";
        // nosniff: don't let the browser MIME-sniff an octet-stream back
        // into executable HTML. attachment: force download rather than
        // inline top-level rendering, so an uploaded HTML/SVG blob can't
        // execute in this origin. (Subresource loads — <img>/<video> in
        // the client — are unaffected by Content-Disposition.)
        headers += "X-Content-Type-Options: nosniff\r\n";
        headers += "Content-Disposition: attachment\r\n";
        headers += "Content-Length: " + std::to_string(body_len) + "\r\n";
        headers += "Accept-Ranges: bytes\r\n";
        headers += conn_header() + "\r\n";

        auto self = shared_from_this();
        auto hdr = std::make_shared<std::string>(std::move(headers));
        boost::asio::async_write(socket_, boost::asio::buffer(*hdr),
            [this, self, hdr, fp, body_len](const boost::system::error_code& ec, std::size_t) {
                if (ec) { std::fclose(*fp); return; }
                send_file_body(fp, body_len);
            });
    }

    // ---- endpoint: POST /attachments/<id>/thumbnail ----
    //
    // Uploader-only, called once after /complete. Body is the raw JPEG bytes
    // of the thumbnail; cap small (256 KB) so a misbehaving client can't
    // chew up disk. Writes to "<storage_path>.thumb.jpg" and stamps the
    // size onto the attachment row so downstream consumers know it exists.

    static constexpr int64_t MAX_THUMB_BYTES = 512 * 1024;

    void handle_thumbnail_upload(int64_t id) {
        auto att = db_.get_attachment(id);
        if (!att)                            { send_error(404, "Not Found"); return; }
        if (att->upload_status != "ready")   { send_error(409, "Conflict"); return; }
        if (att->uploader != username_)      { send_error(403, "Forbidden"); return; }
        if (req_.content_length <= 0 ||
            req_.content_length > MAX_THUMB_BYTES) {
            send_error(413, "Payload Too Large"); return;
        }

        // ?size=N selects which pre-generated size this upload targets.
        // Missing / invalid → legacy single-file behaviour, which writes
        // <storage_path>.thumb.jpg and uses the legacy DB path.
        thumb_size_px_ = 0;
        if (const auto sz_it = req_.query.find("size"); sz_it != req_.query.end()) {
            int requested = 0;
            try { requested = std::stoi(sz_it->second); } catch (...) { requested = 0; }
            if (requested != 320 && requested != 640 && requested != 1280) {
                send_error(400, "Bad Request"); return;
            }
            thumb_size_px_ = requested;
        }

        thumb_id_       = id;
        thumb_path_     = thumb_size_px_ != 0
            ? att->storage_path + ".thumb-" + std::to_string(thumb_size_px_) + "px.jpg"
            : att->storage_path + ".thumb.jpg";
        thumb_remain_   = req_.content_length;
        thumb_buf_.clear();
        thumb_buf_.reserve(static_cast<size_t>(req_.content_length));

        // Drain whatever body bytes piggy-backed on the head read.
        if (head_buf_.size() > 0) {
            const size_t have = std::min<size_t>(
                head_buf_.size(), static_cast<size_t>(thumb_remain_));
            const char* src = boost::asio::buffers_begin(head_buf_.data()).operator->();
            thumb_buf_.insert(thumb_buf_.end(), src, src + have);
            head_buf_.consume(have);
            thumb_remain_ -= static_cast<int64_t>(have);
        }
        if (thumb_remain_ == 0) return finish_thumbnail();
        read_thumbnail_chunk();
    }

    void read_thumbnail_chunk() {
        auto self = shared_from_this();
        const size_t want = static_cast<size_t>(
            std::min<int64_t>(thumb_remain_, 64 * 1024));
        const size_t base = thumb_buf_.size();
        thumb_buf_.resize(base + want);
        boost::asio::async_read(socket_,
            boost::asio::buffer(thumb_buf_.data() + base, want),
            [this, self](const boost::system::error_code& ec, std::size_t n) {
                if (ec) return;
                arm_deadline();
                thumb_remain_ -= static_cast<int64_t>(n);
                if (thumb_remain_ == 0) return finish_thumbnail();
                read_thumbnail_chunk();
            });
    }

    void finish_thumbnail() {
        // Write atomically via a .tmp sibling so a crash mid-write doesn't
        // leave a half-thumbnail that fools the next GET.
        const std::string tmp_path = thumb_path_ + ".tmp";
        {
            std::ofstream f(tmp_path, std::ios::binary | std::ios::trunc);
            if (!f.good()) {
                send_error(500, "Internal Server Error"); return;
            }
            f.write(thumb_buf_.data(), static_cast<std::streamsize>(thumb_buf_.size()));
            if (!f.good()) {
                std::error_code rm_ec;
                std::filesystem::remove(tmp_path, rm_ec);
                send_error(500, "Internal Server Error"); return;
            }
        }
        std::error_code rn_ec;
        std::filesystem::rename(tmp_path, thumb_path_, rn_ec);
        if (rn_ec) {
            std::error_code rm_ec;
            std::filesystem::remove(tmp_path, rm_ec);
            send_error(500, "Internal Server Error"); return;
        }
        const int64_t bytes = static_cast<int64_t>(thumb_buf_.size());
        bool ok = false;
        if (thumb_size_px_ == 0) {
            // Legacy single-thumb path. Stamps the row's bytes-only field.
            ok = db_.set_attachment_thumbnail_size(thumb_id_, bytes);
        } else {
            const int bit =
                thumb_size_px_ == 320  ? 1 :
                thumb_size_px_ == 640  ? 2 :
                thumb_size_px_ == 1280 ? 4 : 0;
            ok = db_.add_attachment_thumbnail_size(thumb_id_, bit, bytes);
        }
        if (!ok) {
            send_error(500, "Internal Server Error"); return;
        }
        send_response("HTTP/1.1 204 No Content\r\nContent-Length: 0\r\n" + conn_header() + "\r\n");
    }

    void send_file_body(std::shared_ptr<std::FILE*> fp, int64_t remaining) {
        if (remaining == 0) {
            if (fp && *fp) { std::fclose(*fp); *fp = nullptr; }
            finish_request();
            return;
        }
        const size_t want = static_cast<size_t>(std::min<int64_t>(remaining, GET_BUF_SIZE));
        auto buf = std::make_shared<std::vector<char>>(want);
        const size_t got = std::fread(buf->data(), 1, want, *fp);
        if (got == 0) {
            // Short read mid-body: the declared Content-Length can't be
            // honoured, so the connection must not be reused.
            if (fp && *fp) { std::fclose(*fp); *fp = nullptr; }
            graceful_close();
            return;
        }
        buf->resize(got);
        auto self = shared_from_this();
        boost::asio::async_write(socket_, boost::asio::buffer(*buf),
            [this, self, fp, buf, remaining, got](const boost::system::error_code& ec, std::size_t) {
                if (ec) { std::fclose(*fp); *fp = nullptr; return; }
                arm_deadline();
                send_file_body(fp, remaining - static_cast<int64_t>(got));
            });
    }

    // ---- endpoint: DELETE /attachments/<id> ----

    void handle_delete(int64_t id) {
        auto att = db_.get_attachment(id);
        if (!att)                              { send_error(404, "Not Found"); return; }
        if (att->upload_status != "uploading") { send_error(409, "Conflict"); return; }
        if (att->uploader != username_)        { send_error(403, "Forbidden"); return; }

        auto path = db_.abort_pending_attachment(id);
        if (!path) { send_error(500, "Internal Server Error"); return; }
        std::error_code ec;
        std::filesystem::remove(*path + ".partial", ec);
        std::filesystem::remove(*path + ".thumb.jpg", ec);
        std::filesystem::remove(*path + ".thumb-320px.jpg", ec);
        std::filesystem::remove(*path + ".thumb-640px.jpg", ec);
        std::filesystem::remove(*path + ".thumb-1280px.jpg", ec);
        send_response("HTTP/1.1 204 No Content\r\nContent-Length: 0\r\n" + conn_header() + "\r\n");
    }

    // ---- response helpers ----

    bool will_keep_alive() const { return keep_alive_ && !force_close_; }
    std::string conn_header() const {
        return will_keep_alive() ? "Connection: keep-alive\r\n" : "Connection: close\r\n";
    }

    void send_json(int status, const std::string& reason, const std::string& body) {
        std::string resp =
            "HTTP/1.1 " + std::to_string(status) + " " + reason + "\r\n"
            "Content-Type: application/json\r\n"
            "Content-Length: " + std::to_string(body.size()) + "\r\n" +
            conn_header() + "\r\n" + body;
        send_response(std::move(resp));
    }

    // Errors may be sent before a request body was consumed, so the
    // stream can't be reused afterwards.
    void send_error(int status, const std::string& reason) {
        force_close_ = true;
        const std::string body = "{\"error\":\"" + reason + "\"}";
        send_json(status, reason, body);
    }

    // Write a complete response, then either serve the next request on
    // this connection (keep-alive) or close.
    void send_response(std::string data) {
        auto self = shared_from_this();
        auto buf = std::make_shared<std::string>(std::move(data));
        boost::asio::async_write(socket_, boost::asio::buffer(*buf),
            [this, self, buf](const boost::system::error_code& ec, std::size_t) {
                if (ec) { graceful_close(); return; }
                finish_request();
            });
    }

    void finish_request() {
        if (!will_keep_alive()) { graceful_close(); return; }
        // Reset per-request state; head_buf_ may already hold the start
        // of a pipelined next request, which async_read_until consumes.
        req_ = HttpRequest{};
        username_.clear();
        body_.clear();
        patch_id_ = 0; patch_final_ = 0; patch_remain_ = 0; patch_fp_.reset(); patch_chunk_.clear();
        thumb_id_ = 0; thumb_remain_ = 0; thumb_size_px_ = 0; thumb_path_.clear(); thumb_buf_.clear();
        arm_deadline();
        read_head();
    }

    // TLS close_notify, asynchronously and with a deadline. This used to
    // be the SYNCHRONOUS ssl::stream::shutdown(), which sends close_notify
    // and then blocks reading the peer's close_notify — on the one io
    // thread shared with chat, auth and both UDP relays. Any peer that
    // completed a TLS handshake (no credentials needed), received its
    // response and simply went silent froze the whole server until it
    // hung up. Every response carries a Content-Length, so a peer that
    // doesn't answer the close_notify within the deadline loses nothing
    // when we just drop the socket.
    void graceful_close() {
        auto self = shared_from_this();
        deadline_timer_.cancel();
        shutdown_timer_.expires_after(std::chrono::seconds(2));
        shutdown_timer_.async_wait([this, self](const boost::system::error_code& ec) {
            if (ec) return;  // cancelled: shutdown completed in time
            boost::system::error_code ignore;
            socket_.lowest_layer().close(ignore);
        });
        socket_.async_shutdown([this, self](const boost::system::error_code&) {
            shutdown_timer_.cancel();
            boost::system::error_code ignore;
            socket_.lowest_layer().close(ignore);
        });
    }

    // ---- state ----

    static constexpr int64_t PATCH_BUF_SIZE = 256 * 1024;  // 256 KB
    static constexpr int64_t GET_BUF_SIZE   = 256 * 1024;

    ssl::stream<tcp::socket> socket_;
    boost::asio::steady_timer shutdown_timer_;
    boost::asio::steady_timer deadline_timer_;
    uint64_t deadline_gen_ = 0;  // stamps each arm_deadline() (stale-completion guard)
    chatproj::CommunityDb& db_;
    const chatproj::Authorizer* authz_;
    std::string jwt_secret_;
    std::string storage_root_;
    int64_t max_attachment_bytes_;

    // Bounded so a client that never sends the CRLFCRLF head terminator
    // can't grow this streambuf without limit (its default max_size is
    // SIZE_MAX). async_read_until errors out at the cap; the 16 KB guard
    // in read_head() still applies once the delimiter is found.
    boost::asio::streambuf head_buf_{16 * 1024};
    HttpRequest req_;
    std::string username_;
    bool keep_alive_ = false;
    bool force_close_ = false;
    std::vector<char> body_; // small-body JSON endpoints only

    // PATCH streaming state
    int64_t patch_id_ = 0;
    int64_t patch_final_ = 0;
    int64_t patch_remain_ = 0;
    std::shared_ptr<std::FILE*> patch_fp_;
    std::vector<char> patch_chunk_;

    // Thumbnail upload state
    int64_t thumb_id_ = 0;
    int64_t thumb_remain_ = 0;
    int thumb_size_px_ = 0; // 0 = legacy single-file path
    std::string thumb_path_;
    std::vector<char> thumb_buf_;
};

} // namespace

// -------- AttachmentHttpServer impl --------

AttachmentHttpServer::AttachmentHttpServer(boost::asio::io_context& ioc,
                                           unsigned short port,
                                           chatproj::CommunityDb& db,
                                           const std::string& jwt_secret,
                                           const std::string& storage_root,
                                           int64_t max_attachment_bytes)
    : ioc_(ioc),
      ssl_ctx_(ssl::context::tlsv12),
      acceptor_(ioc, tcp::endpoint(tcp::v4(), port)),
      accept_backoff_(ioc),
      db_(db),
      jwt_secret_(jwt_secret),
      storage_root_(storage_root),
      max_attachment_bytes_(max_attachment_bytes),
      port_(port) {
    ssl_ctx_.set_options(
        ssl::context::default_workarounds |
        ssl::context::no_sslv2 |
        ssl::context::no_sslv3 |
        ssl::context::no_tlsv1 |
        ssl::context::no_tlsv1_1);
    ssl_ctx_.use_certificate_chain_file("server.crt");
    ssl_ctx_.use_private_key_file("server.key", ssl::context::pem);

    std::error_code ec;
    std::filesystem::create_directories(storage_root_, ec);
    if (ec) {
        std::cerr << "[AttachmentHttp] Failed to create storage root '"
                  << storage_root_ << "': " << ec.message() << "\n";
    }

    std::cout << "[AttachmentHttp] Listening on port " << port
              << " (max_attachment_bytes=" << max_attachment_bytes_ << ")\n";
    do_accept();
}

void AttachmentHttpServer::do_accept() {
    acceptor_.async_accept(
        [this](const boost::system::error_code& ec, tcp::socket socket) {
            if (!ec) {
                auto conn = std::make_shared<AttachmentConnection>(
                    std::move(socket), ssl_ctx_, db_, authz_, jwt_secret_,
                    storage_root_, max_attachment_bytes_);
                conn->start();
                do_accept();
                return;
            }
            if (ec == boost::asio::error::operation_aborted) return;
            // See CommunityServer::do_accept — EMFILE must not become a
            // busy loop on the shared io thread.
            std::cerr << "[AttachmentHttp] accept failed: " << ec.message()
                      << " — retrying in 500 ms\n";
            accept_backoff_.expires_after(std::chrono::milliseconds(500));
            accept_backoff_.async_wait([this](const boost::system::error_code& tec) {
                if (!tec) do_accept();
            });
        });
}
