#pragma once
// Per-session token buckets for the community server's TCP handlers.
//
// Nothing throttled any client before this: a member could loop 64 KB
// CHANNEL_MSGs at line rate (each persisted, FTS-indexed and fanned out to
// every session), or spam VOICE_STATE_NOTIFY (three all-session broadcasts
// each). Buckets are cheap, live on the Session, and double as the
// mechanism for per-channel slowmode later.
#include <algorithm>
#include <chrono>

namespace chatproj {

class TokenBucket {
public:
    // `capacity` = burst size, `refill_per_sec` = sustained rate.
    TokenBucket(double capacity, double refill_per_sec)
        : capacity_(capacity), refill_(refill_per_sec), tokens_(capacity),
          last_(std::chrono::steady_clock::now()) {}

    // Consume one token if available. Returns false (and consumes
    // nothing) when the caller should drop the action.
    bool try_take() {
        const auto now = std::chrono::steady_clock::now();
        const double elapsed =
            std::chrono::duration<double>(now - last_).count();
        last_ = now;
        tokens_ = std::min(capacity_, tokens_ + elapsed * refill_);
        if (tokens_ < 1.0) return false;
        tokens_ -= 1.0;
        return true;
    }

private:
    double capacity_;
    double refill_;
    double tokens_;
    std::chrono::steady_clock::time_point last_;
};

} // namespace chatproj
