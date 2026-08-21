#pragma once
// Single worker thread + bounded queue for every community→central
// exchange (heartbeat, invite register/unregister, membership
// register/revoke, server-picture sync).
//
// Each of these used to spawn its own detached std::thread and open its
// own one-shot TLS connection. A restart-driven reconnect storm — every
// client re-authenticating at once, each auth firing a membership
// register — meant N threads and N simultaneous TLS handshakes against
// central, each allowed to live up to the 5 s timeout. Serialising them
// also restores ordering (a revoke can no longer overtake the register
// that preceded it) and gives shutdown a thread to join instead of a
// detached one that could outlive the objects it captured.
#include <condition_variable>
#include <cstdint>
#include <deque>
#include <functional>
#include <mutex>
#include <string>
#include <thread>
#include <utility>
#include <vector>

namespace chatproj {

class CentralSyncWorker {
public:
    // Performs one exchange. `framed` is the length-prefixed packet;
    // returns true on success and, when `response` is non-null, fills it
    // with the framed reply body (empty if none was requested/received).
    using Exchange = std::function<bool(const std::vector<uint8_t>& framed,
                                        bool read_response,
                                        std::vector<uint8_t>* response)>;
    // Invoked on the worker thread after the exchange (only when set).
    using Done = std::function<void(bool ok, const std::vector<uint8_t>& response)>;

    explicit CentralSyncWorker(Exchange exchange) : exchange_(std::move(exchange)) {}
    ~CentralSyncWorker() { stop(); }

    void start() {
        std::lock_guard<std::mutex> lock(mutex_);
        if (thread_.joinable()) return;
        stop_ = false;
        thread_ = std::thread([this] { run_(); });
    }

    void stop() {
        {
            std::lock_guard<std::mutex> lock(mutex_);
            stop_ = true;
        }
        cv_.notify_all();
        if (thread_.joinable()) thread_.join();
    }

    // Returns false (and drops the job) when the queue is full — better
    // than growing without bound while central is unreachable; the
    // callers are all idempotent and re-fire on the next occasion
    // (next auth, next heartbeat, startup re-registration).
    bool enqueue(std::vector<uint8_t> framed, bool read_response = false,
                 Done done = nullptr) {
        {
            std::lock_guard<std::mutex> lock(mutex_);
            if (queue_.size() >= kMaxQueued) return false;
            queue_.push_back(Job{std::move(framed), read_response, std::move(done)});
        }
        cv_.notify_one();
        return true;
    }

    size_t pending() {
        std::lock_guard<std::mutex> lock(mutex_);
        return queue_.size();
    }

private:
    struct Job {
        std::vector<uint8_t> framed;
        bool read_response;
        Done done;
    };
    static constexpr size_t kMaxQueued = 4096;

    void run_() {
        for (;;) {
            Job job;
            {
                std::unique_lock<std::mutex> lock(mutex_);
                cv_.wait(lock, [this] { return stop_ || !queue_.empty(); });
                if (stop_) return;
                job = std::move(queue_.front());
                queue_.pop_front();
            }
            std::vector<uint8_t> response;
            const bool ok = exchange_(job.framed, job.read_response,
                                      job.read_response ? &response : nullptr);
            if (job.done) job.done(ok, response);
        }
    }

    Exchange exchange_;
    std::deque<Job> queue_;
    std::mutex mutex_;
    std::condition_variable cv_;
    bool stop_ = false;
    std::thread thread_;
};

} // namespace chatproj
