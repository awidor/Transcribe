#pragma once

#include "transcribe/util.hpp"

#include <nlohmann/json.hpp>

#include <chrono>
#include <cstdint>
#include <filesystem>
#include <functional>
#include <memory>
#include <optional>
#include <stop_token>
#include <string>

// S1-mini by Superwhisper, run locally through llama.cpp's server. The model and the
// llama.cpp build for this machine's hardware download on first use.
namespace transcribe::s1 {

enum class Styling { Casual, SemiCasual, SemiFormal, Formal };

// S1-mini needs two downloads: llama.cpp built for this machine, and the model.
enum class Part { Engine, Model };

struct Download {
    bool ready = false;
    // Bytes still to download.
    std::uint64_t size = 0;
    // From 0 to 1 while downloading.
    std::optional<double> progress;
    std::optional<std::string> error;
    bool operator==(const Download&) const = default;
};

struct Status {
    Download engine;
    Download model;
    bool operator==(const Status&) const = default;
};

class Engine {
  public:
    static std::shared_ptr<Engine> create(std::filesystem::path dir);
    ~Engine();
    Engine(const Engine&) = delete;
    Engine& operator=(const Engine&) = delete;

    // Called on a background thread whenever the status changes.
    void on_status(std::function<void(const Status&)> listener);
    Status status() const;
    // Throws when a download is still missing.
    void check() const;
    // Blocks until the part is downloaded; failures are reported in the status as well.
    void download(Part part);
    void cancel_download(Part part);
    void stop();
    // How long an unused model stays loaded; nothing keeps it loaded.
    void set_unload(std::optional<std::chrono::seconds> unload);
    // Loads the model ahead of a cleanup.
    void warm();
    // `stop` abandons the cleanup, including a server still starting for it.
    std::string clean(const std::string& transcript, Styling styling, std::stop_token stop = {});

    struct State;

  private:
    explicit Engine(std::shared_ptr<State> state);
    std::shared_ptr<State> state_;
};

// Settings store these as "casual", "semi-casual", "semi-formal" and "formal".
inline void to_json(nlohmann::json& json, Styling styling) {
    static const char* names[] = {"casual", "semi-casual", "semi-formal", "formal"};
    json = names[static_cast<int>(styling)];
}
inline void from_json(const nlohmann::json& json, Styling& styling) {
    auto name = json.get<std::string>();
    if (name == "casual")
        styling = Styling::Casual;
    else if (name == "semi-casual")
        styling = Styling::SemiCasual;
    else if (name == "semi-formal")
        styling = Styling::SemiFormal;
    else if (name == "formal")
        styling = Styling::Formal;
    else
        throw Error("Unknown styling");
}

} // namespace transcribe::s1
