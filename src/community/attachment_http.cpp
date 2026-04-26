#include "attachment_http.hpp"

#include <jwt-cpp/traits/nlohmann-json/defaults.h>
#include <nlohmann/json.hpp>

#include <algorithm>
#include <cctype>
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

// Validate an HS256 JWT with the same issuer the TCP layer uses. On success
// returns the `sub` claim (username). On failure returns nullopt.
std::optional<std::string> verify_jwt(const std::string& token, const std::string& secret) {
    try {
        auto decoded = jwt::decode(token);
        jwt::verify()
            .allow_algorithm(jwt::algorithm::hs256{secret})
            .with_issuer("decibell_central_auth")
            .verify(decoded);
        return decoded.get_subject();
    } catch (const std::exception&) {
        return std::nullopt;
    }
}

// -------- parsed request model --------

struct HttpRequest {
    std::string method;
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

// -------- connection handler --------

// Each incoming TCP/TLS connection lives inside one AttachmentConnection.
// Lifetime is extended via shared_ptr so the async handler chain keeps the
// object alive until the last completion callback fires.
class AttachmentConnection : public std::enable_shared_from_this<AttachmentConnection> {
public:
    AttachmentConnection(tcp::socket socket,
                         ssl::context& ssl_ctx,
                         chatproj::CommunityDb& db,
                         const std::string& jwt_secret,
                         const std::string& storage_root,
                         int64_t max_attachment_bytes)
        : socket_(std::move(socket), ssl_ctx),
          db_(db),
          jwt_secret_(jwt_secret),
          storage_root_(storage_root),
          max_attachment_bytes_(max_attachment_bytes) {}

    void start() {
        auto self = shared_from_this();
        socket_.async_handshake(ssl::stream_base::server,
            [this, self](const boost::system::error_code& ec) {
                if (ec) return;
                read_head();
            });
    }

private:
    // ---- reading request head ----

    void read_head() {
        auto self = shared_from_this();
        // Read until CRLFCRLF. Bounded: we allow up to 16KB of headers.
        boost::asio::async_read_until(socket_, head_buf_, "\r\n\r\n",
            [this, self](const boost::system::error_code& ec, std::size_t n) {
                if (ec) return;
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
        std::string filename, mime, channel_id;
        int64_t size = 0;
        int32_t width = 0, height = 0;
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
        } catch (...) { send_error(400, "Bad Request"); return; }

        if (channel_id.empty() || filename.empty() || size <= 0) {
            send_error(400, "Bad Request"); return;
        }
        if (max_attachment_bytes_ > 0 && size > max_attachment_bytes_) {
            send_error(413, "Payload Too Large"); return;
        }
        if (!db_.is_member(username_))      { send_error(403, "Forbidden"); return; }
        if (!db_.get_channel(channel_id))   { send_error(404, "Not Found"); return; }

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
            /*position*/ 0, width, height);
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
            std::filesystem::exists(partial_path)
                ? std::filesystem::file_size(partial_path, ec) : 0);
        if (ec) { send_error(500, "Internal Server Error"); return; }
        if (offset != cur_size) {
            // Client's offset disagrees with ours. The tus-style contract is
            // 409 so the client knows to HEAD and realign.
            send_error(409, "Conflict"); return;
        }
        if (req_.content_length < 0) { send_error(411, "Length Required"); return; }
        if (att->expected_size > 0 &&
            offset + req_.content_length > att->expected_size) {
            send_error(413, "Payload Too Large"); return;
        }
        if (max_attachment_bytes_ > 0 &&
            offset + req_.content_length > max_attachment_bytes_) {
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
        if (patch_fp_ && *patch_fp_) {
            std::fflush(*patch_fp_);
            std::fclose(*patch_fp_);
            *patch_fp_ = nullptr;
        }
        // Respond 204 with Upload-Offset so the client knows where we are.
        std::string resp =
            "HTTP/1.1 204 No Content\r\n"
            "Upload-Offset: " + std::to_string(patch_final_) + "\r\n"
            "Connection: close\r\n"
            "Content-Length: 0\r\n\r\n";
        send_raw_and_close(std::move(resp));
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
            if (std::filesystem::exists(partial))
                offset = static_cast<int64_t>(std::filesystem::file_size(partial, ec));
        } else {
            offset = att->size_bytes;
        }
        std::string resp =
            "HTTP/1.1 200 OK\r\n"
            "Upload-Offset: " + std::to_string(offset) + "\r\n"
            "Upload-Length: " + std::to_string(att->expected_size) + "\r\n"
            "Upload-Status: " + att->upload_status + "\r\n"
            "Connection: close\r\n"
            "Content-Length: 0\r\n\r\n";
        send_raw_and_close(std::move(resp));
    }

    // ---- endpoint: POST /attachments/<id>/complete ----

    void handle_complete(int64_t id) {
        auto att = db_.get_attachment(id);
        if (!att)                              { send_error(404, "Not Found"); return; }
        if (att->upload_status != "uploading") { send_error(409, "Conflict"); return; }
        if (att->uploader != username_)        { send_error(403, "Forbidden"); return; }

        const std::string partial = att->storage_path + ".partial";
        std::error_code ec;
        if (!std::filesystem::exists(partial)) { send_error(409, "Conflict"); return; }
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

        // ?variant=thumb diverts to the JPEG thumbnail file. Reuses the same
        // GET endpoint and auth check rather than introducing a parallel
        // route. Range/partial isn't worth supporting for tiny thumbs.
        const auto var_it = req_.query.find("variant");
        if (var_it != req_.query.end() && var_it->second == "thumb") {
            if (att->thumbnail_size_bytes <= 0) { send_error(404, "Not Found"); return; }
            const std::string thumb_path = att->storage_path + ".thumb.jpg";
            std::error_code thumb_ec;
            if (!std::filesystem::exists(thumb_path, thumb_ec)) {
                send_error(404, "Not Found"); return;
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
                "Content-Length: " + std::to_string(thumb_total) + "\r\n"
                "Connection: close\r\n\r\n";
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
        headers += "Content-Type: "   + att->mime + "\r\n";
        headers += "Content-Length: " + std::to_string(body_len) + "\r\n";
        headers += "Accept-Ranges: bytes\r\n";
        headers += "Connection: close\r\n\r\n";

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

    static constexpr int64_t MAX_THUMB_BYTES = 256 * 1024;

    void handle_thumbnail_upload(int64_t id) {
        auto att = db_.get_attachment(id);
        if (!att)                            { send_error(404, "Not Found"); return; }
        if (att->upload_status != "ready")   { send_error(409, "Conflict"); return; }
        if (att->uploader != username_)      { send_error(403, "Forbidden"); return; }
        if (req_.content_length <= 0 ||
            req_.content_length > MAX_THUMB_BYTES) {
            send_error(413, "Payload Too Large"); return;
        }

        thumb_id_       = id;
        thumb_path_     = att->storage_path + ".thumb.jpg";
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
        if (!db_.set_attachment_thumbnail_size(
                thumb_id_, static_cast<int64_t>(thumb_buf_.size()))) {
            send_error(500, "Internal Server Error"); return;
        }
        send_raw_and_close(
            "HTTP/1.1 204 No Content\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
    }

    void send_file_body(std::shared_ptr<std::FILE*> fp, int64_t remaining) {
        if (remaining == 0) {
            if (fp && *fp) { std::fclose(*fp); *fp = nullptr; }
            return;
        }
        const size_t want = static_cast<size_t>(std::min<int64_t>(remaining, GET_BUF_SIZE));
        auto buf = std::make_shared<std::vector<char>>(want);
        const size_t got = std::fread(buf->data(), 1, want, *fp);
        if (got == 0) {
            if (fp && *fp) { std::fclose(*fp); *fp = nullptr; }
            return;
        }
        buf->resize(got);
        auto self = shared_from_this();
        boost::asio::async_write(socket_, boost::asio::buffer(*buf),
            [this, self, fp, buf, remaining, got](const boost::system::error_code& ec, std::size_t) {
                if (ec) { std::fclose(*fp); *fp = nullptr; return; }
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
        send_raw_and_close("HTTP/1.1 204 No Content\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
    }

    // ---- response helpers ----

    void send_json(int status, const std::string& reason, const std::string& body) {
        std::string resp =
            "HTTP/1.1 " + std::to_string(status) + " " + reason + "\r\n"
            "Content-Type: application/json\r\n"
            "Content-Length: " + std::to_string(body.size()) + "\r\n"
            "Connection: close\r\n\r\n" + body;
        send_raw_and_close(std::move(resp));
    }

    void send_error(int status, const std::string& reason) {
        const std::string body = "{\"error\":\"" + reason + "\"}";
        send_json(status, reason, body);
    }

    void send_raw_and_close(std::string data) {
        auto self = shared_from_this();
        auto buf = std::make_shared<std::string>(std::move(data));
        boost::asio::async_write(socket_, boost::asio::buffer(*buf),
            [this, self, buf](const boost::system::error_code&, std::size_t) {
                boost::system::error_code ignore;
                socket_.shutdown(ignore);
            });
    }

    // ---- state ----

    static constexpr int64_t PATCH_BUF_SIZE = 256 * 1024;  // 256 KB
    static constexpr int64_t GET_BUF_SIZE   = 256 * 1024;

    ssl::stream<tcp::socket> socket_;
    chatproj::CommunityDb& db_;
    std::string jwt_secret_;
    std::string storage_root_;
    int64_t max_attachment_bytes_;

    boost::asio::streambuf head_buf_;
    HttpRequest req_;
    std::string username_;
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
                    std::move(socket), ssl_ctx_, db_, jwt_secret_,
                    storage_root_, max_attachment_bytes_);
                conn->start();
            }
            do_accept();
        });
}
