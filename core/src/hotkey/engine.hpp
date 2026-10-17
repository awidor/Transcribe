#pragma once

#include "transcribe/hotkey.hpp"
#include "transcribe/util.hpp"

#include <chrono>
#include <cstdint>
#include <future>
#include <memory>
#include <mutex>
#include <optional>
#include <set>
#include <string>
#include <string_view>
#include <thread>
#include <utility>
#include <vector>

namespace transcribe::hotkey {

using Clock = std::chrono::steady_clock;

struct Engine {
    struct Capture {
        std::uint64_t token = 0;
        Clock::time_point deadline;
        std::vector<Key> keys;
        bool armed = false;
    };

    Chord binding;
    std::set<std::uint32_t> held;
    std::set<std::uint32_t> stroke;
    std::set<std::uint32_t> swallowed;
    bool releasing = false;
    bool invalid = false;
    std::optional<Capture> capture;
    std::uint64_t next_token = 0;
    bool paused = false;
    std::optional<Clock::time_point> activation_inhibit_until;
    std::set<std::uint32_t> escape;
    bool dismissible = false;
    // Keys that act on their own when tapped, such as Alt opening a menu.
    std::set<std::uint32_t> menu_keys;
    // Menu keys held when the shortcut was pressed; their releases are masked.
    std::set<std::uint32_t> masked;
    // A menu key release held back, for the hook to replay behind a neutral key.
    std::optional<std::uint32_t> mask_release;

    bool dismisses(std::uint32_t key) const;
    bool matches() const;
    void reset();
#ifdef _WIN32
    void forget(const std::set<std::uint32_t>& released);
#endif
    std::optional<Event> cancel();
    // Whether to suppress the key, and the events it caused.
    std::pair<bool, std::vector<Event>> input(Key key, bool down, bool modifier);
};

#ifdef _WIN32
// A capture request, answered with the listener's error, if any.
using Reply = std::promise<std::optional<std::string>>;
struct Listener {
    unsigned long thread = 0;
    std::shared_ptr<Channel<Reply>> requests;
};
#endif

struct Service::State {
    explicit State(std::function<void(const Event&)> handler);

    void send(Event event);
    bool input(std::uint32_t code, std::string label, bool down, bool modifier);
    void failed(const std::string& message);
    void activate();
#ifdef __linux__
    void reset_input();
#endif
#ifdef _WIN32
    // A menu key release to replay behind a neutral key, instead of the real one.
    std::optional<std::uint32_t> take_mask();
    void window_input(Key key, bool down, bool (*pressed)(std::uint32_t));
    // Drops held keys that are no longer pressed, before `pressing` goes down.
    void forget_released(std::uint32_t pressing, bool (*pressed)(std::uint32_t));
#endif

    // Guards engine.
    std::mutex mutex;
    Engine engine;
    std::mutex start_mutex;
    bool started = false;
    std::shared_ptr<Channel<Event>> sender;
#ifdef _WIN32
    std::mutex listener_mutex;
    std::optional<Listener> listener_info;
#endif
    // The native listener's thread, stopped with the service.
    std::jthread listener;
    std::jthread handler;
};

inline bool is_ascii_digit(char c) {
    return c >= '0' && c <= '9';
}
inline bool is_ascii_alphanumeric(char c) {
    return is_ascii_digit(c) || ((c | 0x20) >= 'a' && (c | 0x20) <= 'z');
}
// Rust's `str::split`: empty pieces included.
std::vector<std::string> split(std::string_view text, char separator);
// Rust's `u32::from_str`: an optional `+`, then decimal digits only.
std::optional<std::uint32_t> parse_u32(std::string_view text);
// A JSON binding; anything malformed is "Invalid shortcut".
Binding parse(const std::string& text);

// The platform listener, as Rust's `native` module.
namespace native {
void start(Service::State& state);
void validate(const std::set<std::uint32_t>& keys);
Chord legacy(const std::string& text);
#ifdef _WIN32
bool modifier(std::uint32_t key);
bool pressed(std::uint32_t key);
void prepare_capture(Service::State& state);
#endif
} // namespace native

#ifdef __linux__
namespace evdev {
void start(Service::State& state);
Chord binding(const std::string& text);
} // namespace evdev
#endif

} // namespace transcribe::hotkey
