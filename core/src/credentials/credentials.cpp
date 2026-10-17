#include "transcribe/credentials.hpp"

#include "transcribe/util.hpp"

#include <fstream>

namespace transcribe::credentials {

std::string read(const std::filesystem::path& path) {
    std::ifstream file(path, std::ios::binary);
    if (!file) throw Error("API key unavailable");
    std::string key((std::istreambuf_iterator<char>(file)), {});
    key = trim(key);
    if (key.empty()) throw Error("API key required");
    return key;
}

void save(const std::filesystem::path& path, const std::string& key) {
    auto trimmed = trim(key);
    if (trimmed.empty()) throw Error("API key required");
    if (path.parent_path().empty()) throw Error("API key path has no parent");
    // The replacement is owner-only and atomic, so a failed write leaves the previous key.
    write_atomic(path, trimmed, true);
}

} // namespace transcribe::credentials
