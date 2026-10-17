#pragma once

#include <cstdint>
#include <memory>
#include <optional>
#include <string>

// The focused window through wlr-foreign-toplevel-management.
namespace transcribe::insertion::focus {

struct Tracker;

struct Target {
    std::shared_ptr<Tracker> tracker;
    std::uint64_t id = 0;
    std::string app_id;
    bool valid() const;
};

// Nothing when the compositor has no foreign-toplevel management.
std::optional<Target> capture();

} // namespace transcribe::insertion::focus
