#pragma once

#include "transcribe/util.hpp"

#include <map>
#include <set>
#include <string>

// What delivery uses of wl-clipboard-rs: the Wayland selections through ext-data-control,
// or wlr-data-control on compositors without it. Messages match wl-clipboard-rs.
namespace transcribe::insertion::data_control {

enum class Clipboard { Regular, Primary, Both };

// The failures callers tell apart; everything else is Other.
enum class Kind { Other, ClipboardEmpty, MissingProtocol, PrimarySelectionUnsupported };
struct Failure : Error {
    Failure(Kind kind, const std::string& message) : Error(message), kind(kind) {}
    Kind kind;
};

// Regular or Primary only.
std::set<std::string> mime_types(Clipboard clipboard);
// A pipe receiving the selection in `mime_type`; the caller closes it.
int contents(Clipboard clipboard, const std::string& mime_type);
// Offers `data` from a background thread until another copy replaces it. Text is also
// offered as text/plain, STRING and TEXT, as wl-clipboard-rs does.
void copy(Clipboard clipboard, const std::map<std::string, std::string>& data);
void clear(Clipboard clipboard);

} // namespace transcribe::insertion::data_control
