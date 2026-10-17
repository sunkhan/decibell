#pragma once
// Size limits on what central sends to clients (review A1, 2026-10-17).
//
// The client treats any frame over 2 MiB as a broken connection
// (native net/framing.rs), reconnects and asks again, so a response that
// outgrows it locks the user out until the data changes. Every list or
// page response is cut at MAX_RESPONSE_BYTES and says so (has_more), and
// every stored string a response repeats has a byte cap, applied when it is
// written and again when it is read (rows stored before a cap existed).

#include <cstddef>
#include <string>

namespace chatproj {

/// The client's frame cap (native net/framing.rs MAX_FRAME_SIZE).
constexpr size_t MAX_CLIENT_FRAME = 2 * 1024 * 1024;
/// The byte budget of one list or page response: what its rows may add up
/// to. The rest of the frame cap is slack for the envelope and per-row
/// framing. Same figure as the community's search pages.
constexpr size_t MAX_RESPONSE_BYTES = 1536 * 1024;

/// Plaintext DM body cap (the edit cap). Rows stored before DIRECT_MSG
/// checked it are cut to it when read.
constexpr size_t MAX_DM_CONTENT = 64 * 1024;
/// A DM quoted as a reply parent, or shown as a conversation's last
/// message: one line in the client, so 512 bytes is plenty.
constexpr size_t DM_PREVIEW_MAX_BYTES = 512;
/// A community's name and description in the directory, at logins and in
/// invite previews: the community's own caps (SERVER_UPDATE_REQ).
constexpr size_t MAX_COMMUNITY_NAME = 64;
constexpr size_t MAX_COMMUNITY_DESCRIPTION = 512;

/// The longest prefix of `in` that fits in `max_bytes` without cutting a
/// UTF-8 sequence in two. The client's protobuf decoder rejects a whole
/// packet over one malformed string, so a plain resize() is never enough.
/// Input is valid UTF-8 (protobuf and PostgreSQL both check it), so a cut
/// can only land inside a sequence by splitting it.
inline std::string utf8_prefix(const std::string& in, size_t max_bytes) {
    if (in.size() <= max_bytes) return in;
    size_t cut = max_bytes;
    while (cut > 0 && (static_cast<unsigned char>(in[cut]) & 0xC0) == 0x80) --cut;
    return in.substr(0, cut);
}

/// `in` cut to at most `max_bytes`, ending in "…" when it was cut, for text
/// that stands in for a longer body (previews, old oversized messages).
inline std::string utf8_preview(const std::string& in, size_t max_bytes) {
    if (in.size() <= max_bytes) return in;
    static const std::string ellipsis = "\xE2\x80\xA6";  // U+2026
    return utf8_prefix(in, max_bytes - ellipsis.size()) + ellipsis;
}

/// A display string from a community (its name or description): ASCII
/// control characters dropped, then cut to `max_bytes`. The community
/// applies the same rule before it sends them (community db.hpp clamp_utf8).
inline std::string clamp_display(const std::string& in, size_t max_bytes) {
    std::string out;
    out.reserve(in.size() < max_bytes ? in.size() : max_bytes);
    for (unsigned char c : in) {
        if (c < 0x20 || c == 0x7F) continue;
        out.push_back(static_cast<char>(c));
    }
    return utf8_prefix(out, max_bytes);
}

}  // namespace chatproj
