#pragma once

#include "transcribe/util.hpp"

#include <chrono>
#include <optional>
#include <string>
#include <thread>

namespace transcribe::insertion::platform {

// A native clipboard call that failed. Only these are retried while preserving the clipboard.
struct ClipboardFailure : Error {
    ClipboardFailure(const std::string& operation, std::optional<unsigned> format,
                     unsigned long code)
        : Error("Clipboard " + operation + " failed" +
                (format ? " (format " + std::to_string(*format) + ")" : "") + ": Windows error " +
                std::to_string(code)) {}
};

void pump_messages();

template <class Attempt>
auto retry_snapshot(Attempt attempt, std::chrono::steady_clock::duration budget) {
    auto deadline = std::chrono::steady_clock::now() + budget;
    for (;;) {
        try {
            return attempt();
        } catch (const ClipboardFailure&) {
            // Only retry native preparation failures. Unsupported formats
            // must remain intact; neither writes nor dispatch run here.
            if (std::chrono::steady_clock::now() >= deadline) throw;
            pump_messages();
            std::this_thread::sleep_for(std::chrono::milliseconds(50));
        }
    }
}

} // namespace transcribe::insertion::platform
