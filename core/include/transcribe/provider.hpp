#pragma once

#include "transcribe/audio.hpp"
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
// MaiTranscribe is Azure's clean transcription style rather than a cleanup step.
enum class CleanupEngine { OpenRouter, S1Mini, Off, MaiTranscribe };
// What transcribes the whole recording when streaming is off.
enum class TranscriptionModel { MaiTranscribe, Voxtral };

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
    // For tests: another API base URL and retry delays.
    OpenRouter(std::string api, std::vector<std::chrono::milliseconds> retry_delays);

    std::vector<CleanupModel> cleanup_models() const;
    // Cleans a transcript with the chosen engine. Cleanup that leaves nothing, such as filler
    // alone, throws audio::NoSpeech. `retrying` runs before each retry.
    std::string clean(const std::string& transcript, const CleanupConfig& cleanup,
                      const std::function<void()>& retrying, std::stop_token stop) const;
    // Sends the whole recording to Voxtral once the receiver closes, then returns the
    // transcript and the seconds of audio it covers. Throws audio::NoSpeech for silence.
    std::pair<std::string, double> transcribe(audio::Receiver audio, const std::string& key,
                                              const std::function<void()>& retrying,
                                              std::stop_token stop) const;

  private:
    std::string api_;
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
inline void to_json(nlohmann::json& json, ReasoningEffort effort) {
    json = name(effort);
}
inline void from_json(const nlohmann::json& json, ReasoningEffort& effort) {
    auto parsed = reasoning_effort(json.get<std::string>());
    if (!parsed) throw Error("Unknown reasoning effort");
    effort = *parsed;
}

// Stored as "openrouter", "s1-mini", "off" and "mai-transcribe".
inline void to_json(nlohmann::json& json, CleanupEngine engine) {
    static const char* names[] = {"openrouter", "s1-mini", "off", "mai-transcribe"};
    json = names[static_cast<int>(engine)];
}
inline void from_json(const nlohmann::json& json, CleanupEngine& engine) {
    auto name = json.get<std::string>();
    if (name == "openrouter")
        engine = CleanupEngine::OpenRouter;
    else if (name == "s1-mini")
        engine = CleanupEngine::S1Mini;
    else if (name == "off")
        engine = CleanupEngine::Off;
    else if (name == "mai-transcribe")
        engine = CleanupEngine::MaiTranscribe;
    else
        throw Error("Unknown cleanup engine");
}

// Stored as "mai-transcribe-2" and "voxtral-small-transcribe-3".
inline void to_json(nlohmann::json& json, TranscriptionModel model) {
    json = model == TranscriptionModel::Voxtral ? "voxtral-small-transcribe-3" : "mai-transcribe-2";
}
inline void from_json(const nlohmann::json& json, TranscriptionModel& model) {
    auto name = json.get<std::string>();
    if (name == "mai-transcribe-2")
        model = TranscriptionModel::MaiTranscribe;
    else if (name == "voxtral-small-transcribe-3")
        model = TranscriptionModel::Voxtral;
    else
        throw Error("Unknown transcription model");
}

} // namespace transcribe::provider
