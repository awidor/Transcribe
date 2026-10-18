#pragma once

#include "transcribe/provider.hpp"
#include "transcribe/s1.hpp"

#include <nlohmann/json.hpp>

#include <cstdint>
#include <filesystem>
#include <memory>
#include <optional>
#include <string>
#include <vector>

namespace transcribe::storage {

struct Settings {
    std::optional<std::string> microphone;
    std::string shortcut = "CommandOrControl+Shift+Space";
    std::optional<std::string> shortcut_label;
    std::string cleanup_model = provider::DEFAULT_CLEANUP_MODEL;
    std::optional<provider::ReasoningEffort> cleanup_reasoning_effort =
        provider::ReasoningEffort::Low;
    provider::CleanupEngine cleanup_engine = provider::CleanupEngine::OpenRouter;
    s1::Styling cleanup_styling = s1::Styling::SemiFormal;
    // How long an unused local model stays loaded; nothing keeps it loaded.
    std::optional<std::uint64_t> cleanup_unload_seconds = 5 * 60;
    bool streaming = true;
};

// The JSON the settings table stores, with camelCase keys. Missing optional fields take their
// defaults; a missing shortcut or a malformed value fails.
void to_json(nlohmann::json& json, const Settings& settings);
void from_json(const nlohmann::json& json, Settings& settings);

struct Entry {
    std::string id;
    std::int64_t created_at = 0;
    std::string text;
    double seconds = 0;
    std::string status;
    std::optional<std::string> error;
};

class Store {
  public:
    explicit Store(const std::filesystem::path& path);
    ~Store();
    Store(const Store&) = delete;
    Store& operator=(const Store&) = delete;

    std::vector<Entry> list() const;
    void insert(const Entry& entry);
    void status(const std::string& id, const std::string& status,
                const std::optional<std::string>& error);
    void edit(const std::string& id, const std::string& text);
    void remove(const std::string& id);
    // The saved settings, or the defaults when none are saved or they cannot be read.
    Settings settings() const;
    void save_settings(const Settings& settings);

  private:
    void* connection_;
};

void remove_legacy_audio(const std::filesystem::path& data);

} // namespace transcribe::storage
