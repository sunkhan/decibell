#pragma once
// Message search worker (docs/superpowers/specs/2026-10-14-message-search-design.md).
//
// Searches run on ONE worker thread with its own read-only SQLite
// connection: WAL lets it read while the main connection writes, it never
// takes CommunityDb's mutex, and the io thread (which also relays voice)
// only validates a request, resolves which channels it may cover, and
// later hydrates at most one page of results by primary key.
//
// Every query is driven from an index in id order and stops after the
// page (plus counting, on the first page), under a wall-clock budget
// enforced by a progress handler. One query runs at a time; a session has
// at most one queued or running, and a new one supersedes it.
#include <atomic>
#include <chrono>
#include <condition_variable>
#include <cstdint>
#include <deque>
#include <functional>
#include <mutex>
#include <string>
#include <thread>
#include <vector>

struct sqlite3;

namespace chatproj {

// Mirror of MessageSearchRequest.Has (kept here so the worker doesn't
// depend on the generated protobuf headers).
namespace search_has {
constexpr uint32_t kFile     = 1;
constexpr uint32_t kImage    = 2;
constexpr uint32_t kVideo    = 4;
constexpr uint32_t kAudio    = 8;
constexpr uint32_t kDocument = 16;
constexpr uint32_t kLink     = 32;
constexpr uint32_t kKnown    = 63;
} // namespace search_has

enum class SearchSort { Newest = 0, Oldest = 1, Relevance = 2 };

// A validated search, resolved on the io thread.
struct SearchSpec {
    uint64_t owner = 0;                     // requesting session (supersede / cancel key)
    // Text channels to search: readable ones, and only plaintext ones when
    // `match` is set (the index holds plaintext rows only).
    std::vector<std::string> channels;
    std::string match;                      // FTS5 expression; "" = no words / link filter
    std::vector<int64_t> author_uids;       // members: every row their account sent
    std::vector<std::string> author_names;  // non-members: rows sent under this name
    int64_t after = 0;                      // unix seconds, inclusive; 0 = unbounded
    int64_t before = 0;                     // unix seconds, exclusive; 0 = unbounded
    uint32_t has = 0;                       // search_has bits, each must hold
    SearchSort sort = SearchSort::Newest;   // Relevance only with `match`
    int64_t cursor = 0;                     // Newest / Oldest: previous page's last id
    int32_t offset = 0;                     // Relevance: results already shown
    int32_t limit = 25;
    bool count = false;                     // first page: count matches
};

struct SearchOutcome {
    bool ok = false;
    std::string error;                      // user-facing when !ok
    std::vector<int64_t> ids;               // the page, in sort order
    bool has_more = false;
    int64_t total = -1;                     // -1 = not counted
    bool total_capped = false;              // more than `total` match
    bool partial = false;                   // budget ran out before the page filled
};

// Free text → FTS5 expression. Words are AND-ed; a word with 3+ letters
// matches as a prefix; "quoted phrases" match exactly; -word / -"phrase"
// excludes. Everything is quoted, so FTS5 syntax in the input is inert.
// Returns "" when no searchable word is left; `only_excluded` is then set
// if the text had excluded words only.
std::string build_fts_query(const std::string& text, bool* only_excluded = nullptr);

class SearchWorker {
public:
    using Done = std::function<void(SearchOutcome)>;   // invoked on the worker thread

    static constexpr size_t kMaxQueued = 32;
    static constexpr int kBudgetMs = 3000;        // per query, page + count
    // Counting past a full page: exact up to 10,000 matches unless it takes
    // longer than this, then "N+". Finding the page is cheap; an exact count
    // of a common word combined with a narrow filter is what costs.
    static constexpr int kCountExtraMs = 250;
    static constexpr int64_t kCountCap = 10000;
    // Relevance scores the newest this-many matches (and pages within them).
    static constexpr int32_t kRelevancePool = 2000;

    explicit SearchWorker(std::string db_path);
    ~SearchWorker();
    SearchWorker(const SearchWorker&) = delete;
    SearchWorker& operator=(const SearchWorker&) = delete;

    // Opens the read connection and starts the thread. False if the DB
    // can't be opened read-only.
    bool start();
    void stop();

    enum class Submit { Queued, Busy };
    // Queues a search. A queued search of the same owner is replaced (its
    // answer never comes); a running one is cancelled. Busy = queue full.
    Submit submit(SearchSpec spec, Done done);
    // Drops the owner's queued search and cancels its running one.
    void cancel(uint64_t owner);

private:
    struct Job {
        SearchSpec spec;
        Done done;
    };
    void run_();
    SearchOutcome execute_(const SearchSpec& spec);
    // Smallest message id whose timestamp is >= ts, assuming timestamps
    // grow with ids (they are server-assigned at insert). -1 = none.
    int64_t first_id_at_or_after_(int64_t ts);
    static int progress_(void* self);

    std::string path_;
    sqlite3* db_ = nullptr;
    std::thread thread_;
    std::mutex mutex_;
    std::condition_variable cv_;
    std::deque<Job> queue_;
    bool stop_ = false;
    uint64_t running_owner_ = 0;
    std::atomic<bool> cancel_running_{false};
    // Worker thread only (set before stepping, read by progress_).
    std::chrono::steady_clock::time_point deadline_{};
};

} // namespace chatproj
