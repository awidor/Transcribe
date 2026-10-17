#include "transcribe/util.hpp"

#include <array>
#include <fstream>
#include <random>
#include <thread>

#ifdef _WIN32
#include <bcrypt.h>
#include <windows.h>
#else
#include <fcntl.h>
#include <openssl/evp.h>
#include <sys/stat.h>
#include <unistd.h>
#endif

namespace transcribe {

std::int64_t now_ms() {
    using namespace std::chrono;
    return duration_cast<milliseconds>(system_clock::now().time_since_epoch()).count();
}

namespace {

std::array<std::uint8_t, 16> random_uuid() {
    static thread_local std::random_device device;
    std::array<std::uint8_t, 16> bytes{};
    for (std::size_t i = 0; i < bytes.size(); i += 4) {
        auto value = device();
        for (std::size_t j = 0; j < 4; ++j)
            bytes[i + j] = static_cast<std::uint8_t>(value >> (8 * j));
    }
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    return bytes;
}

std::string hex(const std::uint8_t* data, std::size_t size) {
    static constexpr char digits[] = "0123456789abcdef";
    std::string text;
    text.reserve(size * 2);
    for (std::size_t i = 0; i < size; ++i) {
        text += digits[data[i] >> 4];
        text += digits[data[i] & 0xf];
    }
    return text;
}

} // namespace

std::string uuid_simple() {
    auto bytes = random_uuid();
    return hex(bytes.data(), bytes.size());
}

std::string uuid_v4() {
    auto text = uuid_simple();
    return text.substr(0, 8) + "-" + text.substr(8, 4) + "-" + text.substr(12, 4) + "-" +
           text.substr(16, 4) + "-" + text.substr(20);
}

bool is_uuid(std::string_view text) {
    auto is_hex = [](char c) {
        return (c >= '0' && c <= '9') || (c >= 'a' && c <= 'f') || (c >= 'A' && c <= 'F');
    };
    if (text.size() == 32) {
        for (char c : text)
            if (!is_hex(c)) return false;
        return true;
    }
    if (text.size() == 38 && text.front() == '{' && text.back() == '}') text = text.substr(1, 36);
    if (text.size() != 36) return false;
    for (std::size_t i = 0; i < text.size(); ++i) {
        bool dash = i == 8 || i == 13 || i == 18 || i == 23;
        if (dash ? text[i] != '-' : !is_hex(text[i])) return false;
    }
    return true;
}

bool is_whitespace(char32_t c) {
    return (c >= 0x09 && c <= 0x0d) || c == 0x20 || c == 0x85 || c == 0xa0 || c == 0x1680 ||
           (c >= 0x2000 && c <= 0x200a) || c == 0x2028 || c == 0x2029 || c == 0x202f ||
           c == 0x205f || c == 0x3000;
}

bool is_control(char32_t c) {
    return c <= 0x1f || (c >= 0x7f && c <= 0x9f);
}

std::u32string decode_utf8(std::string_view text) {
    std::u32string out;
    out.reserve(text.size());
    std::size_t i = 0;
    while (i < text.size()) {
        auto byte = static_cast<unsigned char>(text[i]);
        std::size_t length = byte < 0x80         ? 1
                             : (byte >> 5) == 6  ? 2
                             : (byte >> 4) == 14 ? 3
                             : (byte >> 3) == 30 ? 4
                                                 : 0;
        char32_t c = length == 1   ? byte
                     : length == 2 ? byte & 0x1f
                     : length == 3 ? byte & 0x0f
                                   : byte & 0x07;
        bool valid = length != 0 && i + length <= text.size();
        for (std::size_t j = 1; valid && j < length; ++j) {
            auto next = static_cast<unsigned char>(text[i + j]);
            valid = (next >> 6) == 2;
            c = (c << 6) | (next & 0x3f);
        }
        static constexpr char32_t smallest[] = {0, 0, 0x80, 0x800, 0x10000};
        if (valid && (c < smallest[length] || c > 0x10ffff || (c >= 0xd800 && c <= 0xdfff)))
            valid = false;
        if (!valid) {
            out += U'�';
            ++i;
            continue;
        }
        out += c;
        i += length;
    }
    return out;
}

std::string encode_utf8(std::u32string_view text) {
    std::string out;
    out.reserve(text.size());
    for (char32_t c : text) {
        if (c < 0x80) {
            out += static_cast<char>(c);
        } else if (c < 0x800) {
            out += static_cast<char>(0xc0 | (c >> 6));
            out += static_cast<char>(0x80 | (c & 0x3f));
        } else if (c < 0x10000) {
            out += static_cast<char>(0xe0 | (c >> 12));
            out += static_cast<char>(0x80 | ((c >> 6) & 0x3f));
            out += static_cast<char>(0x80 | (c & 0x3f));
        } else {
            out += static_cast<char>(0xf0 | (c >> 18));
            out += static_cast<char>(0x80 | ((c >> 12) & 0x3f));
            out += static_cast<char>(0x80 | ((c >> 6) & 0x3f));
            out += static_cast<char>(0x80 | (c & 0x3f));
        }
    }
    return out;
}

namespace {

// The byte length of the code point starting at `i`, or 1 for an invalid byte.
std::size_t code_point(std::string_view text, std::size_t i, char32_t& c) {
    auto rest = text.substr(i, std::min<std::size_t>(4, text.size() - i));
    auto byte = static_cast<unsigned char>(rest[0]);
    std::size_t length = byte < 0x80         ? 1
                         : (byte >> 5) == 6  ? 2
                         : (byte >> 4) == 14 ? 3
                         : (byte >> 3) == 30 ? 4
                                             : 1;
    if (length > rest.size()) length = 1;
    auto decoded = decode_utf8(rest.substr(0, length));
    c = decoded.size() == 1 ? decoded[0] : U'�';
    return decoded.size() == 1 ? length : 1;
}

} // namespace

std::string trim(std::string_view text) {
    std::size_t start = 0;
    while (start < text.size()) {
        char32_t c;
        auto length = code_point(text, start, c);
        if (!is_whitespace(c)) break;
        start += length;
    }
    std::size_t end = start;
    std::size_t i = start;
    while (i < text.size()) {
        char32_t c;
        auto length = code_point(text, i, c);
        i += length;
        if (!is_whitespace(c)) end = i;
    }
    return std::string(text.substr(start, end - start));
}

std::vector<std::string_view> split_whitespace(std::string_view text) {
    std::vector<std::string_view> words;
    std::size_t i = 0, start = std::string_view::npos;
    while (i < text.size()) {
        char32_t c;
        auto length = code_point(text, i, c);
        if (is_whitespace(c)) {
            if (start != std::string_view::npos) words.push_back(text.substr(start, i - start));
            start = std::string_view::npos;
        } else if (start == std::string_view::npos) {
            start = i;
        }
        i += length;
    }
    if (start != std::string_view::npos) words.push_back(text.substr(start));
    return words;
}

std::string to_lower_ascii(std::string_view text) {
    std::string out(text);
    for (char& c : out)
        if (c >= 'A' && c <= 'Z') c = static_cast<char>(c - 'A' + 'a');
    return out;
}

#ifdef _WIN32

struct HashState {
    BCRYPT_ALG_HANDLE algorithm = nullptr;
    BCRYPT_HASH_HANDLE hash = nullptr;
};

Sha256::Sha256() : state_(new HashState) {
    auto* state = static_cast<HashState*>(state_);
    if (BCryptOpenAlgorithmProvider(&state->algorithm, BCRYPT_SHA256_ALGORITHM, nullptr, 0) != 0 ||
        BCryptCreateHash(state->algorithm, &state->hash, nullptr, 0, nullptr, 0, 0) != 0) {
        if (state->algorithm) BCryptCloseAlgorithmProvider(state->algorithm, 0);
        delete state;
        throw Error("SHA-256 unavailable");
    }
}

Sha256::~Sha256() {
    auto* state = static_cast<HashState*>(state_);
    BCryptDestroyHash(state->hash);
    BCryptCloseAlgorithmProvider(state->algorithm, 0);
    delete state;
}

void Sha256::update(const void* data, std::size_t size) {
    auto* state = static_cast<HashState*>(state_);
    auto* bytes = static_cast<PUCHAR>(const_cast<void*>(data));
    while (size > 0) {
        auto chunk = static_cast<ULONG>(std::min<std::size_t>(size, 1u << 30));
        BCryptHashData(state->hash, bytes, chunk, 0);
        bytes += chunk;
        size -= chunk;
    }
}

std::string Sha256::hex() {
    auto* state = static_cast<HashState*>(state_);
    std::uint8_t digest[32];
    BCryptFinishHash(state->hash, digest, sizeof digest, 0);
    return transcribe::hex(digest, sizeof digest);
}

#else

Sha256::Sha256() : state_(EVP_MD_CTX_new()) {
    if (!state_ || EVP_DigestInit_ex(static_cast<EVP_MD_CTX*>(state_), EVP_sha256(), nullptr) != 1)
        throw Error("SHA-256 unavailable");
}

Sha256::~Sha256() {
    EVP_MD_CTX_free(static_cast<EVP_MD_CTX*>(state_));
}

void Sha256::update(const void* data, std::size_t size) {
    EVP_DigestUpdate(static_cast<EVP_MD_CTX*>(state_), data, size);
}

std::string Sha256::hex() {
    unsigned char digest[EVP_MAX_MD_SIZE];
    unsigned int size = 0;
    EVP_DigestFinal_ex(static_cast<EVP_MD_CTX*>(state_), digest, &size);
    return transcribe::hex(digest, size);
}

#endif

void write_atomic(const std::filesystem::path& path, std::string_view bytes, bool owner_only) {
    auto parent = path.parent_path();
    if (parent.empty()) throw Error("Path has no parent");
    auto temporary = parent / ("." + path.filename().string() + "." + uuid_simple() + ".tmp");
#ifdef _WIN32
    (void)owner_only; // Files under the user's profile are private to them already.
    {
        std::ofstream file(temporary, std::ios::binary | std::ios::trunc);
        file.write(bytes.data(), static_cast<std::streamsize>(bytes.size()));
        file.flush();
        if (!file) {
            std::error_code ignored;
            std::filesystem::remove(temporary, ignored);
            throw Error("Could not write " + path.filename().string());
        }
    }
    if (!MoveFileExW(temporary.c_str(), path.c_str(),
                     MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH)) {
        std::error_code ignored;
        std::filesystem::remove(temporary, ignored);
        throw Error("Could not write " + path.filename().string());
    }
#else
    int fd = ::open(temporary.c_str(), O_WRONLY | O_CREAT | O_EXCL | O_CLOEXEC,
                    owner_only ? 0600 : 0644);
    if (fd < 0) throw Error("Could not write " + path.filename().string());
    bool ok = true;
    std::size_t written = 0;
    while (ok && written < bytes.size()) {
        auto n = ::write(fd, bytes.data() + written, bytes.size() - written);
        if (n < 0)
            ok = errno == EINTR;
        else
            written += static_cast<std::size_t>(n);
    }
    ok = ok && ::fsync(fd) == 0;
    ok = ::close(fd) == 0 && ok;
    ok = ok && ::rename(temporary.c_str(), path.c_str()) == 0;
    if (!ok) {
        ::unlink(temporary.c_str());
        throw Error("Could not write " + path.filename().string());
    }
#endif
}

bool sleep_for(std::stop_token stop, std::chrono::steady_clock::duration duration) {
    std::mutex mutex;
    std::condition_variable_any wake;
    std::unique_lock lock(mutex);
    return !wake.wait_for(lock, stop, duration, [] { return false; }) && !stop.stop_requested();
}

} // namespace transcribe
