#pragma once

#include <map>
#include <set>
#include <string>

// X11 CLIPBOARD and PRIMARY, which GNOME shares with Wayland apps. GNOME
// offers no data-control protocol, so they are its only selection access.
// Callers hold the insertion CLIPBOARD lock, so calls never overlap.
namespace transcribe::insertion::xselection {

std::set<std::string> formats(bool primary);
std::string read(bool primary, const std::string& format);
// Offers `data` on the selection, or releases it when empty, and waits
// until GNOME has taken the new selection over for Wayland apps.
void write(bool primary, const std::map<std::string, std::string>& data);

} // namespace transcribe::insertion::xselection
