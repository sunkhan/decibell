#pragma once

// Some Boost.Asio versions (e.g. Debian 12's 1.74) use std::exchange in
// awaitable.hpp without including <utility> themselves. Pulling it in first
// keeps the community server buildable across the distro versions we target.
#include <utility>

#include <boost/asio.hpp>
#include <boost/asio/ssl.hpp>
#include <cstdint>
#include <string>

namespace chatproj {
class CommunityDb;
}

// HTTP/TLS endpoint for resumable attachment upload/download. Runs alongside
// the chat TCP/TLS listener on port+3 — reusing the same certificate. Speaks
// a minimal subset of HTTP/1.1 (no keepalive, no chunked transfer-encoding):
//
//   POST /attachments/init                — create pending attachment row
//   PATCH /attachments/<id>               — append chunk at Upload-Offset
//   HEAD /attachments/<id>                — report current Upload-Offset
//   POST /attachments/<id>/complete       — finalize (rename .partial)
//   GET /attachments/<id>                 — stream (honors Range: bytes=)
//   DELETE /attachments/<id>              — abort pending upload
//
// All requests authenticate via `Authorization: Bearer <JWT>` using the same
// HS256 secret as the chat TCP layer. Uploads stream to a .partial temp file
// next to the future final path; complete renames. This makes partial files
// obvious for operator cleanup and guarantees readers never see a half-written
// blob.
class AttachmentHttpServer {
public:
    AttachmentHttpServer(boost::asio::io_context& ioc,
                         unsigned short port,
                         chatproj::CommunityDb& db,
                         const std::string& jwt_secret,
                         const std::string& storage_root,
                         int64_t max_attachment_bytes);

    // So callers (e.g. SessionManager::broadcast_to_members on
    // CommunityAuthResponse) can report the right attachment port to clients.
    unsigned short port() const { return port_; }
    int64_t max_attachment_bytes() const { return max_attachment_bytes_; }
    const std::string& storage_root() const { return storage_root_; }

private:
    void do_accept();

    boost::asio::io_context& ioc_;
    boost::asio::ssl::context ssl_ctx_;
    boost::asio::ip::tcp::acceptor acceptor_;
    chatproj::CommunityDb& db_;
    std::string jwt_secret_;
    std::string storage_root_;
    int64_t max_attachment_bytes_;
    unsigned short port_;
};
