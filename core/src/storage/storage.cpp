#include "transcribe/storage.hpp"

#include <sqlite3.h>

namespace transcribe::storage {

namespace {

template <class T>
void optional_field(const nlohmann::json& json, const char* key, std::optional<T>& value) {
    auto it = json.find(key);
    if (it == json.end()) return;
    if (it->is_null())
        value.reset();
    else
        value = it->get<T>();
}

std::string string_value(const nlohmann::json& json) {
    if (!json.is_string()) throw Error("Invalid settings");
    return json.get<std::string>();
}

std::uint64_t unsigned_value(const nlohmann::json& json) {
    if (!json.is_number_unsigned()) throw Error("Invalid settings");
    return json.get<std::uint64_t>();
}

sqlite3* db(void* connection) {
    return static_cast<sqlite3*>(connection);
}

[[noreturn]] void fail(void* connection) {
    throw Error(sqlite3_errmsg(db(connection)));
}

// A prepared statement, finalized when it goes out of scope.
class Statement {
  public:
    Statement(void* connection, const char* sql) : connection_(connection) {
        if (sqlite3_prepare_v2(db(connection), sql, -1, &statement_, nullptr) != SQLITE_OK)
            fail(connection);
    }
    ~Statement() { sqlite3_finalize(statement_); }
    Statement& bind(int index, const std::string& text) {
        check(sqlite3_bind_text(statement_, index, text.data(), static_cast<int>(text.size()),
                                SQLITE_TRANSIENT));
        return *this;
    }
    Statement& bind(int index, const std::optional<std::string>& text) {
        if (text) return bind(index, *text);
        check(sqlite3_bind_null(statement_, index));
        return *this;
    }
    Statement& bind(int index, std::int64_t value) {
        check(sqlite3_bind_int64(statement_, index, value));
        return *this;
    }
    Statement& bind(int index, double value) {
        check(sqlite3_bind_double(statement_, index, value));
        return *this;
    }
    // True while a row is available.
    bool step() {
        int result = sqlite3_step(statement_);
        if (result == SQLITE_ROW) return true;
        if (result == SQLITE_DONE) return false;
        fail(connection_);
    }
    std::string text(int column) const {
        auto* data = reinterpret_cast<const char*>(sqlite3_column_text(statement_, column));
        return data ? std::string(data, sqlite3_column_bytes(statement_, column)) : std::string();
    }
    std::optional<std::string> optional_text(int column) const {
        if (sqlite3_column_type(statement_, column) == SQLITE_NULL) return std::nullopt;
        return text(column);
    }
    std::int64_t integer(int column) const { return sqlite3_column_int64(statement_, column); }
    double real(int column) const { return sqlite3_column_double(statement_, column); }

  private:
    void check(int result) {
        if (result != SQLITE_OK) fail(connection_);
    }
    void* connection_;
    sqlite3_stmt* statement_ = nullptr;
};

void execute(void* connection, const char* sql) {
    char* message = nullptr;
    if (sqlite3_exec(db(connection), sql, nullptr, nullptr, &message) != SQLITE_OK) {
        std::string text = message ? message : "Database error";
        sqlite3_free(message);
        throw Error(text);
    }
}

} // namespace

void to_json(nlohmann::json& json, const Settings& s) {
    auto optional = [](const auto& value) {
        return value ? nlohmann::json(*value) : nlohmann::json(nullptr);
    };
    json = {
        {"microphone", optional(s.microphone)},
        {"shortcut", s.shortcut},
        {"shortcutLabel", optional(s.shortcut_label)},
        {"cleanupModel", s.cleanup_model},
        {"cleanupReasoningEffort", optional(s.cleanup_reasoning_effort)},
        {"cleanupEngine", s.cleanup_engine},
        {"cleanupStyling", s.cleanup_styling},
        {"cleanupUnloadSeconds", optional(s.cleanup_unload_seconds)},
        {"streaming", s.streaming},
    };
}

void from_json(const nlohmann::json& json, Settings& s) {
    if (!json.is_object()) throw Error("Invalid settings");
    Settings parsed;
    auto field = [&](const char* key) {
        auto it = json.find(key);
        return it == json.end() ? nullptr : &*it;
    };
    if (auto* v = field("microphone"))
        parsed.microphone = v->is_null() ? std::nullopt : std::optional(string_value(*v));
    // Unlike the other fields, the shortcut has no default.
    auto* shortcut = field("shortcut");
    if (!shortcut) throw Error("Invalid settings");
    parsed.shortcut = string_value(*shortcut);
    if (auto* v = field("shortcutLabel"))
        parsed.shortcut_label = v->is_null() ? std::nullopt : std::optional(string_value(*v));
    if (auto* v = field("cleanupModel")) parsed.cleanup_model = string_value(*v);
    optional_field(json, "cleanupReasoningEffort", parsed.cleanup_reasoning_effort);
    if (auto* v = field("cleanupEngine")) parsed.cleanup_engine = v->get<provider::CleanupEngine>();
    if (auto* v = field("cleanupStyling")) parsed.cleanup_styling = v->get<s1::Styling>();
    if (auto* v = field("cleanupUnloadSeconds"))
        parsed.cleanup_unload_seconds =
            v->is_null() ? std::nullopt : std::optional(unsigned_value(*v));
    if (auto* v = field("streaming")) {
        if (!v->is_boolean()) throw Error("Invalid settings");
        parsed.streaming = v->get<bool>();
    }
    s = std::move(parsed);
}

Store::Store(const std::filesystem::path& path) : connection_(nullptr) {
    sqlite3* connection = nullptr;
    auto name = path.u8string();
    if (sqlite3_open(reinterpret_cast<const char*>(name.c_str()), &connection) != SQLITE_OK) {
        std::string message = connection ? sqlite3_errmsg(connection) : "Database unavailable";
        sqlite3_close(connection);
        throw Error(message);
    }
    connection_ = connection;
    try {
        execute(connection_,
                "PRAGMA journal_mode=WAL; CREATE TABLE IF NOT EXISTS transcripts (id TEXT PRIMARY "
                "KEY, created_at INTEGER NOT NULL, text TEXT NOT NULL, seconds REAL NOT NULL, "
                "status TEXT NOT NULL, error TEXT); CREATE TABLE IF NOT EXISTS settings (id "
                "INTEGER PRIMARY KEY CHECK(id=1), data TEXT NOT NULL);");
        // A crash after dispatch cannot safely be retried on startup.
        execute(connection_, "UPDATE transcripts SET status='unverified' WHERE status='pending'");
        execute(connection_, "UPDATE transcripts SET status='failed',error='Interrupted' WHERE "
                             "status='transcribing'");
        // Silence and cancellation were once recorded as failures.
        execute(connection_, "DELETE FROM transcripts WHERE status='failed' AND text='' AND error "
                             "IN ('No speech detected','Cancelled')");
    } catch (...) {
        sqlite3_close(connection);
        throw;
    }
}

Store::~Store() {
    sqlite3_close(db(connection_));
}

std::vector<Entry> Store::list() const {
    Statement query(connection_, "SELECT id,created_at,text,seconds,status,error FROM "
                                 "transcripts ORDER BY created_at DESC");
    std::vector<Entry> entries;
    while (query.step())
        entries.push_back({query.text(0), query.integer(1), query.text(2), query.real(3),
                           query.text(4), query.optional_text(5)});
    return entries;
}

void Store::insert(const Entry& e) {
    Statement(connection_, "INSERT INTO transcripts VALUES (?1,?2,?3,?4,?5,?6)")
        .bind(1, e.id)
        .bind(2, e.created_at)
        .bind(3, e.text)
        .bind(4, e.seconds)
        .bind(5, e.status)
        .bind(6, e.error)
        .step();
}

void Store::status(const std::string& id, const std::string& status,
                   const std::optional<std::string>& error) {
    Statement(connection_, "UPDATE transcripts SET status=?2,error=?3 WHERE id=?1")
        .bind(1, id)
        .bind(2, status)
        .bind(3, error)
        .step();
}

void Store::edit(const std::string& id, const std::string& text) {
    Statement(connection_, "UPDATE transcripts SET text=?2 WHERE id=?1")
        .bind(1, id)
        .bind(2, text)
        .step();
}

void Store::remove(const std::string& id) {
    Statement(connection_, "DELETE FROM transcripts WHERE id=?1").bind(1, id).step();
}

Settings Store::settings() const {
    try {
        Statement query(connection_, "SELECT data FROM settings WHERE id=1");
        if (!query.step()) return {};
        return nlohmann::json::parse(query.text(0)).get<Settings>();
    } catch (const std::exception&) {
        return {};
    }
}

void Store::save_settings(const Settings& settings) {
    Statement(connection_, "INSERT INTO settings VALUES (1,?1) ON CONFLICT(id) DO UPDATE SET "
                           "data=excluded.data")
        .bind(1, nlohmann::json(settings).dump())
        .step();
}

void remove_legacy_audio(const std::filesystem::path& data) {
    auto path = data / "recordings";
    if (!std::filesystem::exists(path)) return;
    for (const auto& entry : std::filesystem::directory_iterator(path)) {
        const auto& file = entry.path();
        if (file.extension() == ".audio" && is_uuid(file.stem().string()))
            std::filesystem::remove(file);
    }
}

} // namespace transcribe::storage
