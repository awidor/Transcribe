#pragma once

#include <chrono>
#include <condition_variable>
#include <cstdint>
#include <cstdlib>
#include <deque>
#include <filesystem>
#include <mutex>
#include <optional>
#include <stdexcept>
#include <stop_token>
#include <string>
#include <string_view>
#include <vector>

namespace transcribe {

// Every failure the user can see. The message is shown as is.
struct Error : std::runtime_error {
    using std::runtime_error::runtime_error;
};

// Milliseconds since the Unix epoch.
std::int64_t now_ms();

// A random version 4 UUID: hyphenated, or the 32 hex digits alone.
std::string uuid_v4();
std::string uuid_simple();
bool is_uuid(std::string_view text);

// Unicode-aware text helpers matching Rust's str::trim, split_whitespace and char::is_control.
bool is_whitespace(char32_t c);
bool is_control(char32_t c);
std::string trim(std::string_view text);
std::vector<std::string_view> split_whitespace(std::string_view text);
// The code points of valid UTF-8; invalid bytes become U+FFFD.
std::u32string decode_utf8(std::string_view text);
std::string encode_utf8(std::u32string_view text);
std::string to_lower_ascii(std::string_view text);

// Incremental SHA-256 with a lowercase hex digest.
class Sha256 {
  public:
    Sha256();
    ~Sha256();
    Sha256(const Sha256&) = delete;
    Sha256& operator=(const Sha256&) = delete;
    void update(const void* data, std::size_t size);
    std::string hex();

  private:
    void* state_;
};

// Replaces `path` atomically with `bytes`. With `owner_only`, the file is private to the user.
void write_atomic(const std::filesystem::path& path, std::string_view bytes, bool owner_only);

// True when Linux runs a Wayland session.
inline bool is_wayland() {
#ifdef __linux__
    return std::getenv("WAYLAND_DISPLAY") != nullptr;
#else
    return false;
#endif
}

// Waits until `stop` is requested or `duration` passes. True when the full time passed.
bool sleep_for(std::stop_token stop, std::chrono::steady_clock::duration duration);

// A bounded queue between threads. Either side can close it: sends then fail, and the
// receiver drains what is left before recv returns nothing.
template <class T> class Channel {
  public:
    explicit Channel(std::size_t capacity) : capacity_(capacity) {}

    // Blocks while full. False once closed.
    bool send(T value) {
        std::unique_lock lock(mutex_);
        changed_.wait(lock, [&] { return closed_ || items_.size() < capacity_; });
        if (closed_) return false;
        items_.push_back(std::move(value));
        changed_.notify_all();
        return true;
    }
    // False when full or closed.
    bool try_send(T value) {
        std::lock_guard lock(mutex_);
        if (closed_ || items_.size() >= capacity_) return false;
        items_.push_back(std::move(value));
        changed_.notify_all();
        return true;
    }
    // Waits for a value. Nothing once closed and drained, or when `stop` is requested.
    std::optional<T> recv(std::stop_token stop = {}) {
        std::unique_lock lock(mutex_);
        changed_.wait(lock, stop, [&] { return closed_ || !items_.empty(); });
        return take();
    }
    // Like recv, but gives up after `timeout`.
    std::optional<T> recv_for(std::chrono::steady_clock::duration timeout, std::stop_token stop = {}) {
        std::unique_lock lock(mutex_);
        changed_.wait_for(lock, stop, timeout, [&] { return closed_ || !items_.empty(); });
        return take();
    }
    std::optional<T> try_recv() {
        std::lock_guard lock(mutex_);
        return take();
    }
    void close() {
        std::lock_guard lock(mutex_);
        closed_ = true;
        changed_.notify_all();
    }
    bool closed() const {
        std::lock_guard lock(mutex_);
        return closed_;
    }

  private:
    std::optional<T> take() {
        if (items_.empty()) return std::nullopt;
        T value = std::move(items_.front());
        items_.pop_front();
        changed_.notify_all();
        return value;
    }

    mutable std::mutex mutex_;
    std::condition_variable_any changed_;
    std::deque<T> items_;
    std::size_t capacity_;
    bool closed_ = false;
};

} // namespace transcribe
