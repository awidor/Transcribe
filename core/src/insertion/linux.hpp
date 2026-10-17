#pragma once

#include "insertion/focus.hpp"
#include "insertion/platform.hpp"
#include "transcribe/linux_input.hpp"

#include <cstdint>
#include <optional>
#include <string>
#include <string_view>
#include <utility>

namespace transcribe::insertion::platform {

struct Accessible {
    std::string bus;
    std::string path;
    bool terminal = false;
    std::string role;
    bool editable = false;
    bool operator==(const Accessible&) const = default;
};

struct Target {
    std::optional<std::pair<std::uint32_t, std::uint32_t>> x11;
    std::optional<Accessible> accessible;
    bool terminal = false;
    std::optional<focus::Target> window;
};

bool is_terminal(std::string_view app);
Target wayland_target(std::optional<focus::Target> window, std::optional<Accessible> accessible);
linux_input::PasteShortcut shortcut(const Target& target, bool primary);
// True when the compositor lacks data control, as GNOME does; XWayland selections then stand in.
bool lacks_data_control();

} // namespace transcribe::insertion::platform
