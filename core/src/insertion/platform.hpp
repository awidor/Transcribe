#pragma once

#include <memory>
#include <string>
#include <string_view>

namespace transcribe::insertion {

std::string prepare_text(std::string_view text, bool terminal);

} // namespace transcribe::insertion

// Each platform resolves the app and field in front and pastes into it.
namespace transcribe::insertion::platform {

struct Target;
// Throws when nothing there can take text; the clipboard is then untouched.
std::shared_ptr<Target> capture();
void paste(const Target& target, std::string text);
void copy(const std::string& text);
void prepare();

} // namespace transcribe::insertion::platform
