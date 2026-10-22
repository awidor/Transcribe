#pragma once

#include <optional>
#include <string>

// The focused window through wlr-foreign-toplevel-management.
namespace transcribe::insertion::focus {

// The focused window's app id; nothing when the compositor cannot tell.
std::optional<std::string> capture();

} // namespace transcribe::insertion::focus
