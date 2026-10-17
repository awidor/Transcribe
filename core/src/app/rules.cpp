#include "transcribe/app.hpp"

#include <algorithm>
#include <cmath>
#include <cstdio>
#include <regex>

#ifdef _WIN32
#include <windows.h>
#else
#include <glib.h>
#endif

namespace transcribe::app {

namespace {

std::string trim_start(std::string_view text) {
    auto code_points = decode_utf8(text);
    std::size_t start = 0;
    while (start < code_points.size() && is_whitespace(code_points[start]))
        ++start;
    if (start == 0) return std::string(text);
    return encode_utf8(std::u32string_view(code_points).substr(start));
}

std::string lower(std::string_view text) {
#ifdef _WIN32
    if (text.empty()) return {};
    int size =
        MultiByteToWideChar(CP_UTF8, 0, text.data(), static_cast<int>(text.size()), nullptr, 0);
    std::wstring wide(static_cast<std::size_t>(size), L'\0');
    MultiByteToWideChar(CP_UTF8, 0, text.data(), static_cast<int>(text.size()), wide.data(), size);
    int length = LCMapStringEx(LOCALE_NAME_USER_DEFAULT, LCMAP_LOWERCASE, wide.data(), size,
                               nullptr, 0, nullptr, nullptr, 0);
    std::wstring lowered(static_cast<std::size_t>(length), L'\0');
    LCMapStringEx(LOCALE_NAME_USER_DEFAULT, LCMAP_LOWERCASE, wide.data(), size, lowered.data(),
                  length, nullptr, nullptr, 0);
    int bytes =
        WideCharToMultiByte(CP_UTF8, 0, lowered.data(), length, nullptr, 0, nullptr, nullptr);
    std::string out(static_cast<std::size_t>(bytes), '\0');
    WideCharToMultiByte(CP_UTF8, 0, lowered.data(), length, out.data(), bytes, nullptr, nullptr);
    return out;
#else
    auto valid = encode_utf8(decode_utf8(text));
    char* lowered = g_utf8_strdown(valid.data(), static_cast<gssize>(valid.size()));
    std::string out(lowered);
    g_free(lowered);
    return out;
#endif
}

// Labels saved by the old Wayland recorder are DOM key codes such as "AltRight".
std::string key_label(const std::string& label, const std::string& platform) {
    if (platform != "wayland") return label;
    auto first = std::regex_constants::format_first_only;
    auto text = std::regex_replace(label, std::regex("Control(Left|Right)"), "$1 Ctrl", first);
    text = std::regex_replace(text, std::regex("Shift(Left|Right)"), "$1 Shift", first);
    text = std::regex_replace(text, std::regex("Alt(Left|Right)"), "$1 Alt", first);
    text = std::regex_replace(text, std::regex("Meta(Left|Right)"), "$1 Super", first);
    text = std::regex_replace(text, std::regex("^Key"), "", first);
    text = std::regex_replace(text, std::regex("^Digit"), "", first);
    return std::regex_replace(text, std::regex("([a-z])([A-Z])"), "$1 $2");
}

double round_half_up(double value) {
    return std::floor(value + 0.5);
}

} // namespace

const char* name(Phase phase) {
    static const char* names[] = {"idle",     "starting",  "recording", "transcribing",
                                  "cleaning", "inserting", "done",      "error"};
    return names[static_cast<int>(phase)];
}

bool busy(Phase phase) {
    return phase == Phase::Starting || phase == Phase::Recording || phase == Phase::Transcribing ||
           phase == Phase::Cleaning || phase == Phase::Inserting;
}

bool working(Phase phase) {
    return busy(phase) && phase != Phase::Recording;
}

bool finished(const Session& session) {
    return session.phase == Phase::Inserting || session.phase == Phase::Done;
}

bool processing(const Session& session) {
    return session.phase == Phase::Transcribing || session.phase == Phase::Cleaning ||
           finished(session);
}

std::optional<std::string> status_label(const Session& session) {
    if (session.phase == Phase::Starting) return "Starting";
    if (finished(session)) return "Done";
    if (session.phase != Phase::Transcribing && session.phase != Phase::Cleaning)
        return std::nullopt;
    if (session.retrying) return "Retrying";
    return session.phase == Phase::Transcribing ? "Transcribing" : "Cleaning up";
}

std::optional<azure::Words> spoken(const azure::Words& words) {
    auto text = trim_start(words.text);
    auto interim = text.empty() ? trim_start(words.interim) : words.interim;
    if (text.empty() && trim(interim).empty()) return std::nullopt;
    return azure::Words{text, interim};
}

WidgetMode widget_mode(const Session& session) {
    if (session.phase != Phase::Error) return WidgetMode::Session;
    return session.transcript ? WidgetMode::Card : WidgetMode::Error;
}

std::string clock(std::optional<std::int64_t> started_at, std::int64_t now) {
    std::int64_t seconds = started_at ? std::max<std::int64_t>(0, (now - *started_at) / 1000) : 0;
    char text[32];
    std::snprintf(text, sizeof text, "%02lld:%02lld", static_cast<long long>(seconds / 60),
                  static_cast<long long>(seconds % 60));
    return text;
}

std::string size_label(std::uint64_t bytes) {
    char text[32];
    if (bytes >= 1'000'000'000)
        std::snprintf(text, sizeof text, "%.1f GB", static_cast<double>(bytes) / 1e9);
    else
        std::snprintf(text, sizeof text, "%.0f MB",
                      round_half_up(static_cast<double>(bytes) / 1e6));
    return text;
}

std::size_t word_count(std::string_view text) {
    return split_whitespace(text).size();
}

bool matches(std::string_view text, std::string_view search) {
    return lower(text).find(lower(search)) != std::string::npos;
}

std::string entry_preview(const storage::Entry& entry, const Session& session) {
    if (!entry.text.empty()) return entry.text;
    if (entry.error && !entry.error->empty()) return *entry.error;
    if (entry.status == "transcribing")
        if (auto status = status_label(session)) return *status;
    return "Processing";
}

bool not_pasted(const storage::Entry& entry) {
    return entry.status == "saved" && entry.error && !entry.error->empty();
}

double toward(double value, double goal, double rate, double dt) {
    return value + (goal - value) * (1 - std::exp(-rate * dt));
}

// Decibels a sound has to clear the room's noise by before it counts.
constexpr double GATE = 8;
// Decibels above the noise that full height takes, at the least.
constexpr double RANGE = 24;
// Seconds of sound the room's noise is judged from.
constexpr double WINDOW = 2;

double Loudness::operator()(double level, double dt) {
    now_ += dt;
    double target = 0;
    if (level > 0) {
        double db = 20 * std::log10(std::max(level, 1e-6));
        recent_.push_back({now_, db});
        while (recent_.front().at < now_ - WINDOW)
            recent_.erase(recent_.begin());
        // A quiet moment of the last two seconds is the room's noise; pauses between words
        // keep it there even while someone talks.
        std::vector<double> sorted;
        for (const auto& sample : recent_)
            sorted.push_back(sample.db);
        std::sort(sorted.begin(), sorted.end());
        double quiet = sorted[static_cast<std::size_t>(std::floor((sorted.size() - 1) * 0.2))];
        if (!floor_) {
            floor_ = db;
            peak_ = db + RANGE;
        }
        floor_ = toward(*floor_, quiet, 4, dt);
        double ceiling = std::max(db, *floor_ + RANGE);
        peak_ = toward(peak_, ceiling, ceiling > peak_ ? 14 : 0.8, dt);
        double clear = (db - *floor_ - GATE) / (peak_ - *floor_ - GATE);
        target = std::pow(std::min(1.0, std::max(0.0, clear)), 0.7);
    }
    loud_ = toward(loud_, target, target > loud_ ? 12 : 8, dt);
    return loud_;
}

const std::vector<Choice>& unload_choices() {
    static const std::vector<Choice> choices = {
        {"Never", std::nullopt},  {"Immediately", 0},        {"After 2 minutes", 120},
        {"After 5 minutes", 300}, {"After 10 minutes", 600}, {"After 15 minutes", 900},
        {"After 1 hour", 3600},
    };
    return choices;
}

const char* reasoning_label(provider::ReasoningEffort effort) {
    static const char* labels[] = {"None", "Minimal", "Low", "Medium", "High", "Extra high", "Max"};
    return labels[static_cast<int>(effort)];
}

const char* styling_label(s1::Styling styling) {
    static const char* labels[] = {"Casual", "Semi-casual", "Semi-formal", "Formal"};
    return labels[static_cast<int>(styling)];
}

const std::vector<provider::ReasoningEffort>& reasoning_efforts() {
    using provider::ReasoningEffort;
    static const std::vector<ReasoningEffort> efforts = {
        ReasoningEffort::None,   ReasoningEffort::Minimal, ReasoningEffort::Low,
        ReasoningEffort::Medium, ReasoningEffort::High,    ReasoningEffort::Xhigh,
        ReasoningEffort::Max};
    return efforts;
}

std::vector<provider::ReasoningEffort>
available_efforts(const std::vector<provider::CleanupModel>& models, const std::string& model) {
    for (const auto& candidate : models)
        if (candidate.id == model) return candidate.reasoning_efforts;
    return reasoning_efforts();
}

std::optional<provider::ReasoningEffort>
effective_effort(const storage::Settings& settings,
                 const std::vector<provider::CleanupModel>& models) {
    if (!settings.cleanup_reasoning_effort) return std::nullopt;
    auto available = available_efforts(models, settings.cleanup_model);
    if (std::find(available.begin(), available.end(), *settings.cleanup_reasoning_effort) ==
        available.end())
        return std::nullopt;
    return settings.cleanup_reasoning_effort;
}

std::vector<std::string> shortcut_labels(const std::string& shortcut) {
    try {
        auto binding = nlohmann::json::parse(shortcut);
        auto platform = binding.at("platform").get<std::string>();
        std::vector<std::string> labels;
        for (const auto& key : binding.at("keys"))
            labels.push_back(key_label(key.at("label").get<std::string>(), platform));
        return labels;
    } catch (const std::exception&) {
        auto text = shortcut;
        if (auto at = text.find("CommandOrControl"); at != std::string::npos)
            text.replace(at, 16, "Ctrl");
        std::vector<std::string> labels;
        std::size_t start = 0;
        for (;;) {
            auto plus = text.find('+', start);
            labels.push_back(text.substr(start, plus - start));
            if (plus == std::string::npos) break;
            start = plus + 1;
        }
        return labels;
    }
}

void ShortcutRecorder::starting() {
    mode_ = Mode::Starting;
    token_.reset();
    platform_.clear();
    buffered_.clear();
    keys_.clear();
}

ShortcutRecorder::Result ShortcutRecorder::begin(std::uint64_t token, std::string platform) {
    if (mode_ != Mode::Starting) return {true, std::nullopt, token};
    token_ = token;
    platform_ = std::move(platform);
    mode_ = Mode::Listening;
    auto buffered = std::move(buffered_);
    buffered_.clear();
    for (const auto& event : buffered) {
        auto result = receive(event);
        if (result.stopped) return result;
    }
    return {};
}

ShortcutRecorder::Result ShortcutRecorder::receive(const hotkey::Event& event) {
    if (mode_ == Mode::Idle) return {};
    if (!token_) {
        buffered_.push_back(event);
        return {};
    }
    if (auto* cancelled = std::get_if<hotkey::Cancelled>(&event)) {
        if (cancelled->token != *token_) return {};
        return {true, std::nullopt, stop()};
    }
    auto* capture = std::get_if<hotkey::Capture>(&event);
    if (!capture || capture->token != *token_) return {};
    keys_.clear();
    for (const auto& key : capture->keys)
        keys_.push_back(key_label(key.label, platform_));
    if (!capture->shortcut) return {};
    auto shortcut = capture->shortcut;
    return {true, shortcut, stop()};
}

std::optional<std::uint64_t> ShortcutRecorder::stop() {
    auto token = token_;
    mode_ = Mode::Idle;
    token_.reset();
    buffered_.clear();
    return token;
}

bool WindowShortcut::key(std::uint32_t code, bool down, const hotkey::Chord& binding) {
    static const std::set<std::uint32_t> modifiers = {160, 161, 162, 163, 164, 165, 91, 92};
    auto chord_matches = [&] {
        return binding.size() == held_.size() &&
               std::all_of(binding.begin(), binding.end(), [&](const auto& options) {
                   return std::any_of(options.begin(), options.end(),
                                      [&](std::uint32_t key) { return held_.contains(key); });
               });
    };
    if (down && !held_.contains(code)) {
        held_.insert(code);
        if (!modifiers.contains(code) && chord_matches()) swallowed_.insert(code);
    }
    if (!down) held_.erase(code);
    bool alt = (code == 164 || code == 165) &&
               std::any_of(binding.begin(), binding.end(), [&](const auto& options) {
                   return std::find(options.begin(), options.end(), code) != options.end();
               });
    bool suppress = swallowed_.contains(code) || alt;
    if (!down) swallowed_.erase(code);
    return suppress;
}

void WindowShortcut::blur() {
    held_.clear();
    swallowed_.clear();
}

std::optional<std::string> update_status(const UpdateView& update) {
    if (update.phase == "available") return "Version " + update.version.value_or("") + " available";
    if (update.phase == "checking") return "Checking";
    if (update.phase == "current") return "Up to date";
    if (update.phase == "installing") return "Installing";
    if (update.phase == "error") return "Update failed";
    return std::nullopt;
}

std::string update_action(const UpdateView& update) {
    return update.phase == "available" || update.phase == "installing" ? "Install and restart"
                                                                       : "Check for updates";
}

bool update_busy(const UpdateView& update) {
    return update.phase == "checking" || update.phase == "installing";
}

} // namespace transcribe::app
