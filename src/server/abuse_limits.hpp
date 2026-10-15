#pragma once
#include <algorithm>
#include <chrono>
#include <cstddef>
#include <iterator>
#include <mutex>
#include <string>
#include <unordered_map>

/// A token bucket: `capacity` burst, refilled at `per_second`. Not
/// thread-safe on its own (a session's live on its strand; AbuseLimits
/// guards its own).
struct RateBucket {
    RateBucket(double capacity, double per_second)
        : capacity(capacity), per_second(per_second), tokens(capacity) {}
    double capacity;
    double per_second;
    double tokens;
    std::chrono::steady_clock::time_point last = std::chrono::steady_clock::now();

    bool take() {
        refill();
        if (tokens < 1.0) return false;
        tokens -= 1.0;
        return true;
    }
    /// Seconds until take() succeeds (0: it would now).
    double wait() {
        refill();
        return tokens >= 1.0 ? 0.0 : (1.0 - tokens) / per_second;
    }
    bool full() {
        refill();
        return tokens >= capacity;
    }

private:
    void refill() {
        const auto now = std::chrono::steady_clock::now();
        tokens = std::min(capacity, tokens + std::chrono::duration<double>(now - last).count() * per_second);
        last = now;
    }
};

/// Limits that hold across connections (docs/superpowers/specs/
/// 2026-10-15-central-hardening-design.md): per IP address (open
/// connections, new connections, password sign-ins, sign-ups) and per
/// account (failed sign-ins), plus the server-wide connection cap. Each
/// password check costs ~0.25 s of CPU and a connection per attempt gets
/// round any per-connection limit, so these are what keep one machine from
/// tying up central. Thread-safe. Entries back at full that hold no
/// connection are swept; the tables are capped so a flood of addresses or
/// names can't grow them without bound.
class AbuseLimits {
public:
    /// Concurrent connections from one address: room for a household or an
    /// office behind one NAT, not for one machine holding thousands.
    static constexpr std::size_t MAX_CONNECTIONS_PER_IP = 256;
    /// Entries per table. Past it, new addresses are refused a connection
    /// and new account names go untracked (the address limits still hold).
    static constexpr std::size_t MAX_TRACKED = 500000;

    enum class Admit { ok, server_full, address_full, address_rate, table_full };

    explicit AbuseLimits(std::size_t max_connections) : max_connections_(max_connections) {}

    std::size_t max_connections() const { return max_connections_; }

    /// A new connection from `ip`. On `ok` the caller owns one slot and
    /// returns it with release_connection.
    Admit admit_connection(const std::string& ip) {
        std::lock_guard<std::mutex> lock(mutex_);
        if (open_ >= max_connections_) return Admit::server_full;
        auto it = ips_.find(ip);
        if (it == ips_.end()) {
            if (ips_.size() >= MAX_TRACKED) return Admit::table_full;
            it = ips_.emplace(ip, Address{}).first;
        }
        Address& a = it->second;
        if (a.open >= MAX_CONNECTIONS_PER_IP) return Admit::address_full;
        if (!a.connects.take()) return Admit::address_rate;
        ++a.open;
        ++open_;
        return Admit::ok;
    }

    void release_connection(const std::string& ip) {
        std::lock_guard<std::mutex> lock(mutex_);
        auto it = ips_.find(ip);
        if (it == ips_.end() || it->second.open == 0) return;
        --it->second.open;
        --open_;
    }

    /// A password sign-in from `ip` (the address has a connection, so it is
    /// tracked).
    bool take_password_attempt(const std::string& ip) {
        std::lock_guard<std::mutex> lock(mutex_);
        auto it = ips_.find(ip);
        return it != ips_.end() && it->second.passwords.take();
    }

    bool take_signup(const std::string& ip) {
        std::lock_guard<std::mutex> lock(mutex_);
        auto it = ips_.find(ip);
        return it != ips_.end() && it->second.signups.take();
    }

    /// Too many recent failed sign-ins for `account` (a lowercased name):
    /// refuse before checking the password.
    bool account_blocked(const std::string& account) {
        std::lock_guard<std::mutex> lock(mutex_);
        auto it = accounts_.find(account);
        return it != accounts_.end() && it->second.wait() > 0.0;
    }

    void record_failed_login(const std::string& account) {
        std::lock_guard<std::mutex> lock(mutex_);
        auto it = accounts_.find(account);
        if (it == accounts_.end()) {
            if (accounts_.size() >= MAX_TRACKED) return;
            it = accounts_.emplace(account, RateBucket(FAILED_LOGIN_BURST, FAILED_LOGIN_PER_SECOND)).first;
        }
        it->second.take();
    }

    /// A successful sign-in forgets the account's failures.
    void record_login(const std::string& account) {
        std::lock_guard<std::mutex> lock(mutex_);
        accounts_.erase(account);
    }

    /// Drop entries with nothing left to remember (the sweep timer).
    void sweep() {
        std::lock_guard<std::mutex> lock(mutex_);
        for (auto it = ips_.begin(); it != ips_.end();) {
            Address& a = it->second;
            if (a.open == 0 && a.connects.full() && a.passwords.full() && a.signups.full()) {
                it = ips_.erase(it);
            } else {
                ++it;
            }
        }
        for (auto it = accounts_.begin(); it != accounts_.end();) {
            it = it->second.full() ? accounts_.erase(it) : std::next(it);
        }
    }

private:
    /// 10 failed sign-ins in a row, then one more a minute.
    static constexpr double FAILED_LOGIN_BURST = 10.0;
    static constexpr double FAILED_LOGIN_PER_SECOND = 1.0 / 60.0;

    struct Address {
        std::size_t open = 0;
        /// New connections: a whole NAT reconnecting after a restart, with
        /// the clients' jittered backoff, fits in 60 + 10 a second.
        RateBucket connects{60.0, 10.0};
        /// Password sign-ins (a resume costs no password check and isn't
        /// counted): 30 in a row, then one every 2 s.
        RateBucket passwords{30.0, 0.5};
        /// Sign-ups: 10 in a row, then one every 2 minutes.
        RateBucket signups{10.0, 1.0 / 120.0};
    };

    std::size_t max_connections_;
    std::size_t open_ = 0;
    std::unordered_map<std::string, Address> ips_;
    std::unordered_map<std::string, RateBucket> accounts_;
    std::mutex mutex_;
};
