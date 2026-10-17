#pragma once

#include <map>
#include <string>
#include <vector>

// The X11 CLIPBOARD as clipboard-rs offers it to X11 sessions: reads time out after 500 ms
// and a background thread serves what this process owns. Callers hold the insertion
// CLIPBOARD lock, so calls never overlap.
namespace transcribe::insertion::x11_clipboard {

std::vector<std::string> available_formats();
std::string get_buffer(const std::string& format);
// Owns CLIPBOARD with `contents`; empty contents own it with nothing, as clipboard-rs's clear.
void set(const std::map<std::string, std::string>& contents);

} // namespace transcribe::insertion::x11_clipboard
