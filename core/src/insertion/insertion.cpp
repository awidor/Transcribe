#include "transcribe/insertion.hpp"

#include "insertion/platform.hpp"
#include "transcribe/util.hpp"

#include <mutex>
#include <unordered_set>

namespace transcribe::insertion {

namespace {

std::mutex INSERTIONS_MUTEX;
std::unordered_set<std::string> INSERTIONS;
std::mutex CLIPBOARD;

} // namespace

void insert(const std::string& id, const std::string& text) {
    {
        std::lock_guard attempted(INSERTIONS_MUTEX);
        if (!INSERTIONS.insert(id).second) throw Error("Insertion already attempted");
    }
    std::lock_guard guard(CLIPBOARD);
    // Resolve the current app and field only when this paste can proceed. A
    // recording never owns a destination, even while waiting on the clipboard.
    auto target = platform::capture();
    platform::paste(*target, text);
}

void copy(const std::string& text) {
    std::lock_guard guard(CLIPBOARD);
    platform::copy(text);
}

void prepare() {
    platform::prepare();
}

std::string prepare_text(std::string_view text, bool terminal) {
    std::u32string kept;
    // Terminal line breaks can submit commands even without an Enter event.
    // Keep the original, unmodified transcript in history.
    if (terminal) {
        std::string joined;
        auto words = split_whitespace(text);
        for (std::size_t i = 0; i < words.size(); ++i) {
            if (i) joined += ' ';
            joined += words[i];
        }
        for (auto c : decode_utf8(joined))
            if (!is_control(c)) kept += c;
    } else {
        for (auto c : decode_utf8(text))
            if (!is_control(c) || c == U'\n' || c == U'\t') kept += c;
    }
    return encode_utf8(kept);
}

} // namespace transcribe::insertion
