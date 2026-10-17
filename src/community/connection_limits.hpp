#pragma once
// Connection admission for the community server's two TLS listeners (chat
// on 8082, attachments on 8085): a server-wide cap derived from the
// open-file limit, and per IP address a cap on open connections and on new
// connections per second. Every connection is a descriptor, and SQLite,
// uploads and downloads need descriptors too; before these, one host
// connecting ~35 times a second (no sign-in needed) ran the process out
// of them. Simplified from central's AbuseLimits (src/server/
// abuse_limits.hpp): no sign-in tracking, one table per listener.
#include <algorithm>
#include <cstddef>
#include <iterator>
#include <mutex>
#include <string>
#include <unordered_map>

#include "rate_limit.hpp"

namespace chatproj {

class ConnectionLimits {
public:
    /// Concurrent connections from one address: room for a household or an
    /// office behind one NAT (a member holds one chat connection per signed-
    /// in device and up to ~6 attachment connections), not for one machine
    /// holding hundreds.
    static constexpr std::size_t MAX_CONNECTIONS_PER_IP = 128;
    /// Addresses tracked. Past it (after a sweep), new addresses are
    /// refused until entries age out.
    static constexpr std::size_t MAX_TRACKED = 100000;

    enum class Admit { ok, server_full, address_full, address_rate, table_full };

    explicit ConnectionLimits(std::size_t max_connections) : max_connections_(max_connections) {}

    std::size_t max_connections() const { return max_connections_; }

    /// A new connection from `ip`. On `ok` the caller owns one slot and
    /// returns it with release().
    Admit admit(const std::string& ip) {
        std::lock_guard<std::mutex> lock(mutex_);
        if (open_ >= max_connections_) return Admit::server_full;
        auto it = ips_.find(ip);
        if (it == ips_.end()) {
            if (ips_.size() >= next_sweep_) {
                sweep_locked();
                // Amortised: the next sweep waits until the table doubles.
                next_sweep_ = std::max<std::size_t>(kFirstSweep, ips_.size() * 2);
            }
            if (ips_.size() >= MAX_TRACKED) return Admit::table_full;
            it = ips_.emplace(ip, Address{}).first;
        }
        Address& a = it->second;
        if (a.open >= MAX_CONNECTIONS_PER_IP) return Admit::address_full;
        if (!a.connects.try_take()) return Admit::address_rate;
        ++a.open;
        ++open_;
        return Admit::ok;
    }

    void release(const std::string& ip) {
        std::lock_guard<std::mutex> lock(mutex_);
        auto it = ips_.find(ip);
        if (it == ips_.end() || it->second.open == 0) return;
        --it->second.open;
        --open_;
    }

    static const char* describe(Admit a) {
        switch (a) {
            case Admit::ok:           return "ok";
            case Admit::server_full:  return "server full";
            case Admit::address_full: return "too many from one address";
            case Admit::address_rate: return "connecting too fast";
            case Admit::table_full:   return "address table full";
        }
        return "?";
    }

private:
    static constexpr std::size_t kFirstSweep = 1024;

    /// Drop addresses with no open connection whose bucket is back at full
    /// (nothing left to remember).
    void sweep_locked() {
        for (auto it = ips_.begin(); it != ips_.end();) {
            it = (it->second.open == 0 && it->second.connects.full()) ? ips_.erase(it) : std::next(it);
        }
    }

    struct Address {
        std::size_t open = 0;
        /// New connections: a whole NAT reconnecting after a restart, with
        /// the clients' jittered backoff, fits in 60 + 10 a second (as on
        /// central).
        TokenBucket connects{60.0, 10.0};
    };

    std::size_t max_connections_;
    std::size_t open_ = 0;
    std::size_t next_sweep_ = kFirstSweep;
    std::unordered_map<std::string, Address> ips_;
    std::mutex mutex_;
};

/// One connection's slot in a ConnectionLimits table, returned when the
/// connection object is destroyed (it outlives its socket only briefly:
/// every error path closes the socket and cancels the timers holding it).
class ConnectionSlot {
public:
    ConnectionSlot(ConnectionLimits* limits, std::string ip) : limits_(limits), ip_(std::move(ip)) {}
    ~ConnectionSlot() { if (limits_) limits_->release(ip_); }
    ConnectionSlot(const ConnectionSlot&) = delete;
    ConnectionSlot& operator=(const ConnectionSlot&) = delete;
    const std::string& ip() const { return ip_; }

private:
    ConnectionLimits* limits_;
    std::string ip_;
};

} // namespace chatproj
