#pragma once

// What the engine needs from its platform files.

#include "transcribe/util.hpp"

#include <cstdint>
#include <filesystem>
#include <functional>
#include <stop_token>
#include <string>
#include <string_view>
#include <utility>
#include <vector>

namespace transcribe::s1 {

struct Asset {
    std::string url;
    std::string sha256;
    std::uint64_t size;
    std::string file() const { return url.substr(url.rfind('/') + 1); }
};

inline Asset llama(std::string_view file, std::string sha256, std::uint64_t size) {
    return {"https://github.com/ggml-org/llama.cpp/releases/download/b11153/" + std::string(file),
            std::move(sha256), size};
}

struct Runtime {
    std::string name;
    std::vector<Asset> assets;
    bool gpu;
};

// The llama.cpp build for this machine. Throws where S1-mini is unavailable.
const Runtime& runtime();

#ifdef _WIN32
inline constexpr const char* SERVER = "llama-server.exe";
#else
inline constexpr const char* SERVER = "llama-server";
#endif

inline constexpr const char* START_FAILED = "S1-mini failed to start";

extern const Asset MODEL;

// Downloads `asset` to `destination`, resuming its .part file. `progress` gets byte counts.
void fetch(const Asset& asset, const std::filesystem::path& destination, std::stop_token cancel,
           const std::function<void(std::uint64_t)>& progress);
// Unpacks the runtime's archives into `target` by way of `staging`.
void install_runtime(const std::vector<std::filesystem::path>& archives,
                     const std::filesystem::path& staging, const std::filesystem::path& target);

// A loopback port nothing listens on.
std::uint16_t free_port();

// A child process with no input and its output in `log`, killed when destroyed.
class Process {
  public:
    // Throws when it cannot start.
    Process(const std::filesystem::path& program, const std::vector<std::string>& args,
            const std::filesystem::path& directory,
            const std::vector<std::pair<std::string, std::string>>& environment,
            const std::filesystem::path& log);
    ~Process();
    Process(const Process&) = delete;
    Process& operator=(const Process&) = delete;
    // Callers serialize this through the engine's server lock.
    bool alive();

  private:
#ifdef _WIN32
    void* handle_;
#else
    int pid_;
    bool exited_ = false;
#endif
};

} // namespace transcribe::s1
