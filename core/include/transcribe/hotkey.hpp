#pragma once

#include <nlohmann/json.hpp>

#include <cstdint>
#include <functional>
#include <memory>
#include <optional>
#include <string>
#include <utility>
#include <variant>
#include <vector>

// Global input is reduced to a chord in memory. Only an activation or an explicitly requested
// capture is published; ordinary keyboard events never leave this module.
namespace transcribe::hotkey {

struct Key {
    std::uint32_t code = 0;
    std::string label;
    bool operator==(const Key&) const = default;
};
struct Binding {
    std::string platform;
    std::vector<Key> keys;
};
void to_json(nlohmann::json& json, const Key& key);
void from_json(const nlohmann::json& json, Key& key);
void to_json(nlohmann::json& json, const Binding& binding);
void from_json(const nlohmann::json& json, Binding& binding);

struct Activate {};
struct Dismiss {};
struct Capture {
    std::uint64_t token = 0;
    std::vector<Key> keys;
    std::optional<std::string> shortcut;
};
struct Cancelled {
    std::uint64_t token = 0;
};
struct Failure {
    std::string message;
};
using Event = std::variant<Activate, Dismiss, Capture, Cancelled, Failure>;

// "windows", "linux", or "linux-evdev" on Wayland.
std::string platform();

#ifdef _WIN32
// The key a window's key event stands for, named as the global listener names it. Modifiers keep
// their side, which windows report only through the scan code and the extended flag.
Key windows_key(std::uint32_t virtual_key, std::uint32_t scan, bool extended);
#endif

using Chord = std::vector<std::vector<std::uint32_t>>;

class Service {
  public:
    // `handler` runs on a dedicated thread, one event at a time.
    static std::shared_ptr<Service> create(std::function<void(const Event&)> handler);
    ~Service();
    Service(const Service&) = delete;
    Service& operator=(const Service&) = delete;

    void start();
    Chord validate(const std::string& text);
    void configure(Chord binding);
    std::uint64_t begin_capture();
    void cancel_capture(std::optional<std::uint64_t> token);
    // Focused-window input is scoped to an explicit capture (Windows only).
    std::vector<Event> capture_key(std::uint64_t token, const Key& key, bool down);
    Chord binding() const;
    // Windows withholds input from the global listener while Transcribe's own window is in
    // front, so that window reports its keys here instead (Windows only).
    void window_key(const Key& key, bool down);
    void paused(bool paused);
    // Pauses until the guard is destroyed.
    class Pause {
      public:
        explicit Pause(Service* service) : service_(service) {}
        Pause(Pause&& other) noexcept : service_(std::exchange(other.service_, nullptr)) {}
        ~Pause() {
            if (service_) service_->paused(false);
        }

      private:
        Service* service_;
    };
    [[nodiscard]] Pause pause();
    void dismissible(bool dismissible);
    void activate();

    struct State;

  private:
    explicit Service(std::shared_ptr<State> state);
    std::shared_ptr<State> state_;
};

} // namespace transcribe::hotkey
