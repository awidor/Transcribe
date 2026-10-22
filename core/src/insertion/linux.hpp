#pragma once

#include "insertion/platform.hpp"

#include <string_view>

namespace transcribe::insertion::platform {

struct Target {
    bool terminal = false;
};

bool is_terminal(std::string_view app);
// True when the compositor lacks data control, as GNOME does; XWayland selections then stand in.
bool lacks_data_control();

} // namespace transcribe::insertion::platform
