#pragma once

#include <filesystem>
#include <string>

namespace transcribe::credentials {

// The trimmed key. Throws when the file is missing or empty.
std::string read(const std::filesystem::path& path);
// Replaces the key atomically in an owner-only file. An empty key throws and keeps the old one.
void save(const std::filesystem::path& path, const std::string& key);

} // namespace transcribe::credentials
