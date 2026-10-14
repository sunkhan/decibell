#include "search.hpp"

#include <sqlite3.h>

#include <algorithm>
#include <iostream>
#include <utility>
#include <variant>

namespace chatproj {

namespace {

// One prepared statement on the worker's own connection. Not the
// CommunityDb statement cache: that one is keyed by SQL text only and
// belongs to the main connection.
struct Statement {
    sqlite3_stmt* s = nullptr;
    Statement(sqlite3* db, const std::string& sql) {
        if (sqlite3_prepare_v2(db, sql.c_str(), -1, &s, nullptr) != SQLITE_OK) {
            std::cerr << "[Search] prepare failed: " << sqlite3_errmsg(db) << "\n";
            s = nullptr;
        }
    }
    ~Statement() { if (s) sqlite3_finalize(s); }
    Statement(const Statement&) = delete;
    Statement& operator=(const Statement&) = delete;
};

bool is_ascii_alnum(unsigned char c) {
    return (c >= '0' && c <= '9') || (c >= 'a' && c <= 'z') || (c >= 'A' && c <= 'Z');
}

// Letters/digits in a token, counting each non-ASCII code point as one
// (unicode61 treats them as token characters; punctuation splits).
size_t word_chars(const std::string& s) {
    size_t n = 0;
    for (unsigned char c : s) {
        if (is_ascii_alnum(c) || c >= 0xC0) ++n;   // ASCII alnum, or a UTF-8 lead byte
    }
    return n;
}

bool is_space(char c) { return c == ' ' || c == '\t' || c == '\n' || c == '\r'; }

std::string quote_fts(const std::string& token) {
    std::string out = "\"";
    for (char c : token) {
        out.push_back(c);
        if (c == '"') out.push_back('"');
    }
    out.push_back('"');
    return out;
}

std::string placeholders(size_t n) {
    std::string out;
    out.reserve(n * 2);
    for (size_t i = 0; i < n; ++i) {
        if (i > 0) out.push_back(',');
        out.push_back('?');
    }
    return out;
}

// Attachment.Kind of each has-bit.
struct KindBit { uint32_t bit; int kind; };
constexpr KindBit kKindBits[] = {
    { search_has::kImage, 0 },
    { search_has::kVideo, 1 },
    { search_has::kDocument, 2 },
    { search_has::kAudio, 3 },
};

// Folded code point of U+00C0..U+017F (Latin-1 Supplement + Latin
// Extended-A): lower case, accents removed (NFD base letter), and the
// Turkish dotless ı (U+0131) as i. Generated from Unicode data; letters
// without a decomposition (æ, ø, ß, ł, …) only lower-case.
constexpr uint16_t kLatinFold[0x180 - 0xC0] = {
    0x0061, 0x0061, 0x0061, 0x0061, 0x0061, 0x0061, 0x00E6, 0x0063,  // U+00C0
    0x0065, 0x0065, 0x0065, 0x0065, 0x0069, 0x0069, 0x0069, 0x0069,  // U+00C8
    0x00F0, 0x006E, 0x006F, 0x006F, 0x006F, 0x006F, 0x006F, 0x00D7,  // U+00D0
    0x00F8, 0x0075, 0x0075, 0x0075, 0x0075, 0x0079, 0x00FE, 0x00DF,  // U+00D8
    0x0061, 0x0061, 0x0061, 0x0061, 0x0061, 0x0061, 0x00E6, 0x0063,  // U+00E0
    0x0065, 0x0065, 0x0065, 0x0065, 0x0069, 0x0069, 0x0069, 0x0069,  // U+00E8
    0x00F0, 0x006E, 0x006F, 0x006F, 0x006F, 0x006F, 0x006F, 0x00F7,  // U+00F0
    0x00F8, 0x0075, 0x0075, 0x0075, 0x0075, 0x0079, 0x00FE, 0x0079,  // U+00F8
    0x0061, 0x0061, 0x0061, 0x0061, 0x0061, 0x0061, 0x0063, 0x0063,  // U+0100
    0x0063, 0x0063, 0x0063, 0x0063, 0x0063, 0x0063, 0x0064, 0x0064,  // U+0108
    0x0111, 0x0111, 0x0065, 0x0065, 0x0065, 0x0065, 0x0065, 0x0065,  // U+0110
    0x0065, 0x0065, 0x0065, 0x0065, 0x0067, 0x0067, 0x0067, 0x0067,  // U+0118
    0x0067, 0x0067, 0x0067, 0x0067, 0x0068, 0x0068, 0x0127, 0x0127,  // U+0120
    0x0069, 0x0069, 0x0069, 0x0069, 0x0069, 0x0069, 0x0069, 0x0069,  // U+0128
    0x0069, 0x0069, 0x0133, 0x0133, 0x006A, 0x006A, 0x006B, 0x006B,  // U+0130
    0x0138, 0x006C, 0x006C, 0x006C, 0x006C, 0x006C, 0x006C, 0x0140,  // U+0138
    0x0140, 0x0142, 0x0142, 0x006E, 0x006E, 0x006E, 0x006E, 0x006E,  // U+0140
    0x006E, 0x0149, 0x014B, 0x014B, 0x006F, 0x006F, 0x006F, 0x006F,  // U+0148
    0x006F, 0x006F, 0x0153, 0x0153, 0x0072, 0x0072, 0x0072, 0x0072,  // U+0150
    0x0072, 0x0072, 0x0073, 0x0073, 0x0073, 0x0073, 0x0073, 0x0073,  // U+0158
    0x0073, 0x0073, 0x0074, 0x0074, 0x0074, 0x0074, 0x0167, 0x0167,  // U+0160
    0x0075, 0x0075, 0x0075, 0x0075, 0x0075, 0x0075, 0x0075, 0x0075,  // U+0168
    0x0075, 0x0075, 0x0075, 0x0075, 0x0077, 0x0077, 0x0079, 0x0079,  // U+0170
    0x0079, 0x007A, 0x007A, 0x007A, 0x007A, 0x007A, 0x007A, 0x017F,  // U+0178
};

uint32_t fold_code_point(uint32_t c) {
    if (c < 0x80) return (c >= 'A' && c <= 'Z') ? c + 32 : c;
    if (c >= 0xC0 && c < 0x180) return kLatinFold[c - 0xC0];
    if (c >= 0x391 && c <= 0x3A9 && c != 0x3A2) return c + 0x20;   // Greek capitals
    if (c >= 0x410 && c <= 0x42F) return c + 0x20;                  // Cyrillic А..Я
    if (c >= 0x400 && c <= 0x40F) return c + 0x50;                  // Cyrillic Ѐ..Џ
    return c;
}

void append_utf8(std::string& out, uint32_t c) {
    if (c < 0x80) {
        out.push_back(static_cast<char>(c));
    } else if (c < 0x800) {
        out.push_back(static_cast<char>(0xC0 | (c >> 6)));
        out.push_back(static_cast<char>(0x80 | (c & 0x3F)));
    } else if (c < 0x10000) {
        out.push_back(static_cast<char>(0xE0 | (c >> 12)));
        out.push_back(static_cast<char>(0x80 | ((c >> 6) & 0x3F)));
        out.push_back(static_cast<char>(0x80 | (c & 0x3F)));
    } else {
        out.push_back(static_cast<char>(0xF0 | (c >> 18)));
        out.push_back(static_cast<char>(0x80 | ((c >> 12) & 0x3F)));
        out.push_back(static_cast<char>(0x80 | ((c >> 6) & 0x3F)));
        out.push_back(static_cast<char>(0x80 | (c & 0x3F)));
    }
}

std::string fold_bytes(const unsigned char* s, size_t n) {
    std::string out;
    out.reserve(n);
    size_t i = 0;
    while (i < n) {
        const unsigned char b = s[i];
        if (b < 0x80) {   // the common case
            out.push_back(static_cast<char>(b >= 'A' && b <= 'Z' ? b + 32 : b));
            ++i;
            continue;
        }
        uint32_t c;
        size_t len;
        if ((b & 0xE0) == 0xC0) { c = b & 0x1F; len = 2; }
        else if ((b & 0xF0) == 0xE0) { c = b & 0x0F; len = 3; }
        else if ((b & 0xF8) == 0xF0) { c = b & 0x07; len = 4; }
        else { out.push_back(static_cast<char>(b)); ++i; continue; }   // stray byte: keep
        if (i + len > n) { out.append(reinterpret_cast<const char*>(s + i), n - i); break; }
        for (size_t k = 1; k < len; ++k) c = (c << 6) | (s[i + k] & 0x3F);
        i += len;
        if (c >= 0x300 && c <= 0x36F) continue;   // combining marks: a decomposed accent
        append_utf8(out, fold_code_point(c));
    }
    return out;
}

// decibell_fold(text): search_fold as an SQL function, for inside-word
// matching on the worker's connection.
void fold_sql(sqlite3_context* ctx, int, sqlite3_value** argv) {
    const unsigned char* t = sqlite3_value_text(argv[0]);
    if (!t) {
        sqlite3_result_null(ctx);
        return;
    }
    const std::string out = fold_bytes(t, static_cast<size_t>(sqlite3_value_bytes(argv[0])));
    sqlite3_result_text(ctx, out.data(), static_cast<int>(out.size()), SQLITE_TRANSIENT);
}

// The words and "phrases" of a query, in order; -word / -"phrase" negated.
struct QueryTerm {
    std::string text;
    bool phrase = false;
    bool negate = false;
};

std::vector<QueryTerm> split_query(const std::string& text, size_t max_terms) {
    std::vector<QueryTerm> out;
    size_t i = 0;
    const size_t n = text.size();
    while (i < n && out.size() < max_terms) {
        while (i < n && is_space(text[i])) ++i;
        if (i >= n) break;
        QueryTerm t;
        if (text[i] == '-' && i + 1 < n && !is_space(text[i + 1])) {
            t.negate = true;
            ++i;
        }
        if (text[i] == '"') {
            const size_t close = text.find('"', i + 1);
            const size_t end = close == std::string::npos ? n : close;
            t.text = text.substr(i + 1, end - i - 1);
            i = close == std::string::npos ? n : close + 1;
            t.phrase = true;
        } else {
            size_t j = i;
            while (j < n && !is_space(text[j])) ++j;
            t.text = text.substr(i, j - i);
            i = j;
        }
        out.push_back(std::move(t));
    }
    return out;
}

// The index stores ı as i (dotless and dotted i match each other, for
// Turkish); queries get the same.
std::string dotless_i_as_i(std::string s) {
    for (size_t at = s.find("\xC4\xB1"); at != std::string::npos; at = s.find("\xC4\xB1", at)) {
        s.replace(at, 2, "i");
    }
    return s;
}

} // namespace

std::string search_fold(const std::string& text) {
    return fold_bytes(reinterpret_cast<const unsigned char*>(text.data()), text.size());
}

void split_fragments(const std::string& text, std::vector<std::string>* include,
                     std::vector<std::string>* exclude) {
    for (auto& t : split_query(text, 16)) {
        std::string f = search_fold(t.text);
        if (f.empty()) continue;
        (t.negate ? exclude : include)->push_back(std::move(f));
    }
}

std::string build_fts_query(const std::string& text, bool* only_excluded) {
    if (only_excluded) *only_excluded = false;
    std::vector<std::string> include, exclude;
    for (auto& t : split_query(text, 16)) {
        const std::string token = dotless_i_as_i(std::move(t.text));
        const size_t letters = word_chars(token);
        if (letters == 0) continue;   // punctuation only: tokenises to nothing
        // A word of 3+ letters matches as a prefix ("deploy" → "deployment",
        // and Turkish suffixes); shorter ones exactly, so "a" never expands
        // into half the index.
        std::string term = quote_fts(token);
        if (!t.phrase && letters >= 3) term += "*";
        (t.negate ? exclude : include).push_back(std::move(term));
    }
    if (include.empty()) {
        if (only_excluded) *only_excluded = !exclude.empty();
        return std::string();
    }
    std::string expr;
    for (size_t k = 0; k < include.size(); ++k) {
        if (k > 0) expr += " AND ";
        expr += include[k];
    }
    if (!exclude.empty()) {
        expr = "(" + expr + ")";
        for (const auto& e : exclude) expr += " NOT " + e;
    }
    return expr;
}

SearchWorker::SearchWorker(std::string db_path) : path_(std::move(db_path)) {}

SearchWorker::~SearchWorker() { stop(); }

bool SearchWorker::start() {
    if (thread_.joinable()) return true;
    if (sqlite3_open_v2(path_.c_str(), &db_, SQLITE_OPEN_READONLY | SQLITE_OPEN_NOMUTEX,
                        nullptr) != SQLITE_OK) {
        std::cerr << "[Search] can't open " << path_ << " read-only: "
                  << (db_ ? sqlite3_errmsg(db_) : "?") << "\n";
        if (db_) { sqlite3_close(db_); db_ = nullptr; }
        return false;
    }
    sqlite3_busy_timeout(db_, 2000);
    sqlite3_exec(db_, "PRAGMA query_only=1; PRAGMA temp_store=MEMORY; PRAGMA cache_size=-16384;",
                 nullptr, nullptr, nullptr);
    // Every ~1000 VM steps: give up once the job's budget is spent or a
    // newer search from the same session cancelled it.
    sqlite3_progress_handler(db_, 1000, &SearchWorker::progress_, this);
    sqlite3_create_function_v2(db_, "decibell_fold", 1, SQLITE_UTF8 | SQLITE_DETERMINISTIC,
                               nullptr, &fold_sql, nullptr, nullptr, nullptr);
    stop_ = false;
    thread_ = std::thread([this] { run_(); });
    return true;
}

void SearchWorker::stop() {
    {
        std::lock_guard<std::mutex> lock(mutex_);
        stop_ = true;
        queue_.clear();
        cancel_running_ = true;
    }
    cv_.notify_all();
    if (thread_.joinable()) thread_.join();
    if (db_) {
        sqlite3_close(db_);
        db_ = nullptr;
    }
}

SearchWorker::Submit SearchWorker::submit(SearchSpec spec, Done done) {
    {
        std::lock_guard<std::mutex> lock(mutex_);
        if (running_owner_ != 0 && running_owner_ == spec.owner) cancel_running_ = true;
        for (auto& job : queue_) {
            if (job.spec.owner == spec.owner) {
                // Superseded in place: keeps its turn, the old answer never comes.
                job = Job{std::move(spec), std::move(done)};
                return Submit::Queued;
            }
        }
        if (queue_.size() >= kMaxQueued) return Submit::Busy;
        queue_.push_back(Job{std::move(spec), std::move(done)});
    }
    cv_.notify_one();
    return Submit::Queued;
}

void SearchWorker::cancel(uint64_t owner) {
    std::lock_guard<std::mutex> lock(mutex_);
    for (auto it = queue_.begin(); it != queue_.end();) {
        if (it->spec.owner == owner) it = queue_.erase(it);
        else ++it;
    }
    if (running_owner_ != 0 && running_owner_ == owner) cancel_running_ = true;
}

int SearchWorker::progress_(void* p) {
    auto* self = static_cast<SearchWorker*>(p);
    if (self->cancel_running_.load(std::memory_order_relaxed)) return 1;
    return std::chrono::steady_clock::now() >= self->deadline_ ? 1 : 0;
}

void SearchWorker::run_() {
    for (;;) {
        Job job;
        {
            std::unique_lock<std::mutex> lock(mutex_);
            cv_.wait(lock, [this] { return stop_ || !queue_.empty(); });
            if (stop_) return;
            job = std::move(queue_.front());
            queue_.pop_front();
            running_owner_ = job.spec.owner;
            cancel_running_ = false;
        }
        SearchOutcome out = execute_(job.spec);
        bool cancelled;
        {
            std::lock_guard<std::mutex> lock(mutex_);
            cancelled = cancel_running_ || stop_;
            running_owner_ = 0;
            cancel_running_ = false;
        }
        if (!cancelled && job.done) job.done(std::move(out));
    }
}

int64_t SearchWorker::first_id_at_or_after_(int64_t ts) {
    int64_t lo = 0, max_id = 0;
    {
        // Two subqueries: SQLite answers a lone min() / max() from the
        // primary key, but "SELECT min(id), max(id)" scans the table.
        Statement q(db_, "SELECT (SELECT min(id) FROM messages), (SELECT max(id) FROM messages);");
        if (!q.s || sqlite3_step(q.s) != SQLITE_ROW ||
            sqlite3_column_type(q.s, 0) == SQLITE_NULL) return -1;   // no messages
        lo = sqlite3_column_int64(q.s, 0);
        max_id = sqlite3_column_int64(q.s, 1);
    }
    Statement probe(db_, "SELECT id, timestamp FROM messages WHERE id >= ? ORDER BY id LIMIT 1;");
    if (!probe.s) return -2;
    // Invariants: every row below `lo` is older than ts; the first row at or
    // above `hi` is not (hi = max + 1 stands for "past the end").
    int64_t hi = max_id + 1;
    while (lo < hi) {
        const int64_t mid = lo + (hi - lo) / 2;
        sqlite3_reset(probe.s);
        sqlite3_bind_int64(probe.s, 1, mid);
        const int rc = sqlite3_step(probe.s);
        if (rc == SQLITE_DONE) { hi = mid; continue; }
        if (rc != SQLITE_ROW) return -2;   // interrupted / error: no bound
        const int64_t id = sqlite3_column_int64(probe.s, 0);
        if (sqlite3_column_int64(probe.s, 1) >= ts) hi = mid;
        else lo = id + 1;
    }
    if (lo > max_id) return -1;
    sqlite3_reset(probe.s);
    sqlite3_bind_int64(probe.s, 1, lo);
    if (sqlite3_step(probe.s) != SQLITE_ROW) return -2;
    return sqlite3_column_int64(probe.s, 0);
}

SearchOutcome SearchWorker::execute_(const SearchSpec& spec) {
    using clock = std::chrono::steady_clock;
    deadline_ = clock::now() + std::chrono::milliseconds(kBudgetMs);
    SearchOutcome out;
    auto empty_result = [&] {
        out.ok = true;
        out.total = spec.count ? 0 : -1;
        return out;
    };
    if (spec.channels.empty()) return empty_result();

    const bool fts = !spec.match.empty();
    SearchSort sort = spec.sort;
    if (sort == SearchSort::Relevance && !fts) sort = SearchSort::Newest;
    const bool relevance = sort == SearchSort::Relevance;
    const bool oldest = sort == SearchSort::Oldest;
    if (relevance && spec.offset >= kRelevancePool) {
        out.ok = true;
        return out;
    }

    // Dates → id bounds (timestamps are server-assigned and grow with ids),
    // so every driver seeks straight to the window. The timestamp checks
    // stay in the query, so the bounds only ever speed things up.
    int64_t min_id = 0, max_id = 0;
    if (spec.after > 0) {
        const int64_t f = first_id_at_or_after_(spec.after);
        if (f == -1) return empty_result();   // nothing that recent
        if (f > 0) min_id = f;
    }
    if (spec.before > 0) {
        const int64_t f = first_id_at_or_after_(spec.before);
        if (f > 0) {
            if (f <= 1) return empty_result();
            max_id = f - 1;
        }
        // -1: every message is older — no upper bound needed.
    }
    if (min_id > 0 && max_id > 0 && min_id > max_id) return empty_result();

    // The driver: the index the walk follows, in id order.
    enum class Driver { Fts, Author, Files, Channel, Scan };
    const uint32_t file_bits = spec.has & (search_has::kFile | search_has::kImage | search_has::kVideo |
                                           search_has::kAudio | search_has::kDocument);
    const Driver driver =
        fts ? Driver::Fts
        : (!spec.author_uids.empty() && spec.author_names.empty()) ? Driver::Author
        : file_bits ? Driver::Files
        : spec.channels.size() == 1 ? Driver::Channel
        : Driver::Scan;

    using Bind = std::variant<int64_t, std::string>;
    std::string sql;
    std::vector<Bind> binds;
    std::string idcol = "m.id";
    int driver_kind = -1;   // Files: the attachment kind the walk filters on
    switch (driver) {
        case Driver::Fts:
            // Relevance ranks the newest kRelevancePool matches: scoring every
            // match of a common word (and sorting them) cost ~1 s at 1M rows.
            sql = relevance
                ? "SELECT m.id, bm25(messages_fts, 1.0, 0.4) FROM messages_fts "
                  "CROSS JOIN messages m ON m.id = messages_fts.rowid WHERE messages_fts MATCH ?"
                : "SELECT m.id FROM messages_fts CROSS JOIN messages m ON m.id = messages_fts.rowid "
                  "WHERE messages_fts MATCH ?";
            binds.emplace_back(spec.match);
            idcol = "messages_fts.rowid";
            break;
        case Driver::Author:
            sql = "SELECT m.id FROM messages m WHERE m.sender_uid IN (" +
                  placeholders(spec.author_uids.size()) + ")";
            for (int64_t uid : spec.author_uids) binds.emplace_back(uid);
            break;
        case Driver::Files:
            sql = "SELECT a.message_id FROM attachments a CROSS JOIN messages m ON m.id = a.message_id "
                  "WHERE a.message_id > 0";
            for (const auto& kb : kKindBits) {
                if (spec.has & kb.bit) {
                    driver_kind = kb.kind;
                    sql += " AND a.kind = " + std::to_string(kb.kind);
                    break;
                }
            }
            idcol = "a.message_id";
            break;
        case Driver::Channel:
            sql = "SELECT m.id FROM messages m WHERE m.channel_id = ?";
            binds.emplace_back(spec.channels.front());
            break;
        case Driver::Scan:
            sql = "SELECT m.id FROM messages m WHERE 1";
            break;
    }
    // The unary + keeps the planner on the driver's index.
    if (driver != Driver::Channel) {
        sql += " AND +m.channel_id IN (" + placeholders(spec.channels.size()) + ")";
        for (const auto& c : spec.channels) binds.emplace_back(c);
    }
    if (driver != Driver::Author && (!spec.author_uids.empty() || !spec.author_names.empty())) {
        sql += " AND (";
        if (!spec.author_uids.empty()) {
            sql += "+m.sender_uid IN (" + placeholders(spec.author_uids.size()) + ")";
            for (int64_t uid : spec.author_uids) binds.emplace_back(uid);
        }
        if (!spec.author_names.empty()) {
            if (!spec.author_uids.empty()) sql += " OR ";
            sql += "m.sender IN (" + placeholders(spec.author_names.size()) + ")";
            for (const auto& name : spec.author_names) binds.emplace_back(name);
        }
        sql += ")";
    }
    if (spec.after > 0) { sql += " AND +m.timestamp >= ?"; binds.emplace_back(spec.after); }
    if (spec.before > 0) { sql += " AND +m.timestamp < ?"; binds.emplace_back(spec.before); }
    if (min_id > 0) { sql += " AND " + idcol + " >= ?"; binds.emplace_back(min_id); }
    if (max_id > 0) { sql += " AND " + idcol + " <= ?"; binds.emplace_back(max_id); }
    if (!relevance && spec.cursor > 0) {
        sql += " AND " + idcol + (oldest ? " > ?" : " < ?");
        binds.emplace_back(spec.cursor);
    }
    if ((spec.has & search_has::kFile) && driver != Driver::Files) {
        sql += " AND EXISTS (SELECT 1 FROM attachments x WHERE x.message_id = m.id)";
    }
    for (const auto& kb : kKindBits) {
        if (!(spec.has & kb.bit) || kb.kind == driver_kind) continue;
        sql += " AND EXISTS (SELECT 1 FROM attachments x WHERE x.message_id = m.id AND x.kind = " +
               std::to_string(kb.kind) + ")";
    }
    // Inside-word matching: a scan of the folded text (and file names),
    // never an index — the trade-off in the spec. Plaintext rows only.
    if (!spec.contains.empty() || !spec.excludes.empty()) {
        sql += " AND m.envelope IS NULL";
        auto fragment = [&](const std::string& f, bool negate) {
            sql += negate ? " AND NOT (" : " AND (";
            sql += "instr(decibell_fold(m.content), ?) > 0 OR EXISTS (SELECT 1 FROM attachments x "
                   "WHERE x.message_id = m.id AND x.encrypted = 0 AND instr(decibell_fold(x.filename), ?) > 0))";
            binds.emplace_back(f);
            binds.emplace_back(f);
        };
        for (const auto& f : spec.contains) fragment(f, false);
        for (const auto& f : spec.excludes) fragment(f, true);
    }
    if (spec.has & search_has::kLink) {
        // The FTS side already narrowed to rows with an http/https token.
        sql += " AND (instr(m.content, 'http://') > 0 OR instr(m.content, 'https://') > 0)";
    }
    // Relevance walks newest first too and sorts its pool afterwards (file
    // names weigh less than the message text in the score).
    sql += " ORDER BY " + idcol + (oldest ? " ASC" : " DESC");
    sql += ";";

    Statement q(db_, sql);
    if (!q.s) {
        out.error = "Search failed.";
        return out;
    }
    for (size_t i = 0; i < binds.size(); ++i) {
        const int idx = static_cast<int>(i + 1);
        if (const auto* v = std::get_if<int64_t>(&binds[i])) {
            sqlite3_bind_int64(q.s, idx, *v);
        } else {
            const auto& str = std::get<std::string>(binds[i]);
            sqlite3_bind_text(q.s, idx, str.data(), static_cast<int>(str.size()), SQLITE_STATIC);
        }
    }

    const size_t limit = static_cast<size_t>(spec.limit > 0 ? spec.limit : 25);
    // Newest / Oldest: the page is the first `limit` rows of the walk.
    // Relevance: the first kRelevancePool rows are scored, sorted, and the
    // page cut from them.
    const size_t keep = relevance ? static_cast<size_t>(kRelevancePool) : limit;
    std::vector<std::pair<double, int64_t>> pool;   // relevance: (bm25, id)
    size_t kept = 0;
    int64_t seen = 0;       // distinct matches walked
    int64_t last = 0;       // Files: one row per attachment, consecutive per message
    bool counting = false;  // what's needed is in hand; still counting (first page)
    int rc;
    while ((rc = sqlite3_step(q.s)) == SQLITE_ROW) {
        const int64_t id = sqlite3_column_int64(q.s, 0);
        if (id == last) continue;
        last = id;
        ++seen;
        if (kept < keep) {
            ++kept;
            if (relevance) pool.emplace_back(sqlite3_column_double(q.s, 1), id);
            else out.ids.push_back(id);
            continue;
        }
        if (!relevance) out.has_more = true;
        if (!spec.count) break;
        if (!counting) {
            counting = true;
            const auto cap = clock::now() + std::chrono::milliseconds(kCountExtraMs);
            if (cap < deadline_) deadline_ = cap;
        }
        if (seen >= kCountCap) {
            out.total_capped = true;
            break;
        }
    }
    if (relevance) {
        // bm25: lower is better; ties newest first.
        std::sort(pool.begin(), pool.end(), [](const auto& x, const auto& y) {
            return x.first != y.first ? x.first < y.first : x.second > y.second;
        });
        const size_t from = std::min(pool.size(), static_cast<size_t>(spec.offset));
        const size_t to = std::min(pool.size(), from + limit);
        for (size_t i = from; i < to; ++i) out.ids.push_back(pool[i].second);
        out.has_more = to < pool.size();
    }
    if (rc == SQLITE_INTERRUPT) {
        if (cancel_running_) {
            out.error = "Cancelled.";
            return out;
        }
        if (counting) {
            out.total_capped = true;   // the page is whole; the count stopped early
        } else if (kept == 0) {
            out.error = "This search is too broad. Add a filter.";
            return out;
        } else {
            out.partial = true;
            out.has_more = true;
            out.total_capped = true;
        }
    } else if (rc != SQLITE_ROW && rc != SQLITE_DONE) {
        std::cerr << "[Search] step failed: " << sqlite3_errmsg(db_) << "\n";
        out.error = "Search failed.";
        return out;
    }
    if (spec.count) out.total = seen;
    out.ok = true;
    return out;
}

} // namespace chatproj
