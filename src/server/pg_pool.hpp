#pragma once
#include <cstddef>
#include <memory>
#include <mutex>
#include <string>
#include <utility>
#include <vector>
#include <pqxx/pqxx>

/// Postgres connections kept for reuse (docs/superpowers/specs/2026-10-15-
/// central-scaling-design.md). `acquire()` hands out an idle connection or
/// opens a new one — never waits, so a nested acquisition can't deadlock;
/// the thread count bounds how many are open at once. A connection returns
/// to the idle list only while it is still open (a broken one is dropped and
/// the next acquire opens a fresh one), and at most `max_idle` are kept.
/// Thread-safe.
class PgPool {
public:
    /// Exclusive use of one connection until destroyed. Any transaction on
    /// it must end first (declare the transaction after the lease, so it is
    /// destroyed — and rolled back if uncommitted — before the lease).
    class Lease {
    public:
        Lease(PgPool* pool, std::unique_ptr<pqxx::connection> conn) : pool_(pool), conn_(std::move(conn)) {}
        Lease(Lease&& other) noexcept : pool_(other.pool_), conn_(std::move(other.conn_)) {}
        Lease(const Lease&) = delete;
        Lease& operator=(const Lease&) = delete;
        Lease& operator=(Lease&&) = delete;
        ~Lease() {
            if (conn_) pool_->release(std::move(conn_));
        }
        pqxx::connection& operator*() const { return *conn_; }
        pqxx::connection* operator->() const { return conn_.get(); }

    private:
        PgPool* pool_;
        std::unique_ptr<pqxx::connection> conn_;
    };

    PgPool(std::string conn_str, std::size_t max_idle) : conn_str_(std::move(conn_str)), max_idle_(max_idle) {
        idle_.reserve(max_idle_);
    }
    PgPool(const PgPool&) = delete;
    PgPool& operator=(const PgPool&) = delete;

    void set_max_idle(std::size_t max_idle) {
        std::vector<std::unique_ptr<pqxx::connection>> surplus;
        {
            std::lock_guard<std::mutex> lock(mutex_);
            max_idle_ = max_idle;
            idle_.reserve(max_idle_);
            while (idle_.size() > max_idle_) {
                surplus.push_back(std::move(idle_.back()));
                idle_.pop_back();
            }
        }
        // `surplus` closes its connections here, outside the lock.
    }

    /// Throws what pqxx::connection throws when a new connection can't be
    /// opened; callers already catch database errors.
    Lease acquire() {
        for (;;) {
            std::unique_ptr<pqxx::connection> conn;
            {
                std::lock_guard<std::mutex> lock(mutex_);
                if (idle_.empty()) break;
                conn = std::move(idle_.back());
                idle_.pop_back();
            }
            if (is_open(*conn)) return Lease(this, std::move(conn));
            // Closed while idle: drop it (outside the lock) and try the next.
        }
        return Lease(this, std::make_unique<pqxx::connection>(conn_str_));
    }

private:
    static bool is_open(const pqxx::connection& conn) noexcept {
        try {
            return conn.is_open();
        } catch (...) {
            return false;
        }
    }

    void release(std::unique_ptr<pqxx::connection> conn) noexcept {
        if (!is_open(*conn)) return;  // broken: closed by the unique_ptr
        {
            std::lock_guard<std::mutex> lock(mutex_);
            if (idle_.size() < max_idle_) {
                try {
                    idle_.push_back(std::move(conn));
                } catch (...) {
                    // Out of memory growing the list: `conn` still owns the
                    // connection and closes it below.
                }
            }
        }
        // Not kept (over the cap, or the push failed): `conn` closes here,
        // outside the lock.
    }

    const std::string conn_str_;
    std::mutex mutex_;
    std::size_t max_idle_;
    std::vector<std::unique_ptr<pqxx::connection>> idle_;
};
