#pragma once

#include "transcribe/s1.hpp"

#include <nlohmann/json.hpp>

#include <chrono>
#include <functional>
#include <memory>
#include <optional>
#include <stop_token>
#include <string>
#include <variant>
#include <vector>

namespace transcribe::provider {

inline constexpr const char* DEFAULT_CLEANUP_MODEL = "google/gemini-3.8-flash";

enum class ReasoningEffort { None, Minimal, Low, Medium, High, Xhigh, Max };
enum class CleanupEngine { OpenRouter, S1Mini, Off };

struct CleanupModel {
    std::string id;
    std::string name;
    std::vector<ReasoningEffort> reasoning_efforts;
};

struct OpenRouterCleanup {
    std::string model;
    std::optional<ReasoningEffort> reasoning_effort;
    std::string key;
};
struct LocalCleanup {
    std::shared_ptr<s1::Engine> engine;
    s1::Styling styling;
};
using CleanupConfig = std::variant<OpenRouterCleanup, LocalCleanup>;

class OpenRouter {
  public:
    OpenRouter();
    // For tests: another cleanup endpoint and retry delays.
    OpenRouter(std::string cleanup_endpoint, std::vector<std::chrono::milliseconds> retry_delays);

    std::vector<CleanupModel> cleanup_models() const;
    // Cleans a transcript with the chosen engine. Cleanup that leaves nothing, such as filler
    // alone, throws audio::NoSpeech. `retrying` runs before each retry.
    std::string clean(const std::string& transcript, const CleanupConfig& cleanup,
                      const std::function<void()>& retrying, std::stop_token stop) const;

  private:
    std::string cleanup_endpoint_;
    std::vector<std::chrono::milliseconds> retry_delays_;
};

// Stored and sent lowercase: "none", "minimal", "low", "medium", "high", "xhigh", "max".
inline const char* name(ReasoningEffort effort) {
    static const char* names[] = {"none", "minimal", "low", "medium", "high", "xhigh", "max"};
    return names[static_cast<int>(effort)];
}
inline std::optional<ReasoningEffort> reasoning_effort(std::string_view text) {
    for (int i = 0; i <= static_cast<int>(ReasoningEffort::Max); ++i)
        if (text == name(static_cast<ReasoningEffort>(i))) return static_cast<ReasoningEffort>(i);
    return std::nullopt;
}
inline void to_json(nlohmann::json& json, ReasoningEffort effort) { json = name(effort); }
inline void from_json(const nlohmann::json& json, ReasoningEffort& effort) {
    auto parsed = reasoning_effort(json.get<std::string>());
    if (!parsed) throw Error("Unknown reasoning effort");
    effort = *parsed;
}

// Stored as "openrouter", "s1-mini" and "off".
inline void to_json(nlohmann::json& json, CleanupEngine engine) {
    static const char* names[] = {"openrouter", "s1-mini", "off"};
    json = names[static_cast<int>(engine)];
}
inline void from_json(const nlohmann::json& json, CleanupEngine& engine) {
    auto name = json.get<std::string>();
    if (name == "openrouter") engine = CleanupEngine::OpenRouter;
    else if (name == "s1-mini") engine = CleanupEngine::S1Mini;
    else if (name == "off") engine = CleanupEngine::Off;
    else throw Error("Unknown cleanup engine");
}

} // namespace transcribe::provider
