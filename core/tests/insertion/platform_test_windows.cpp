#include "insertion/windows.hpp"

#include <gtest/gtest.h>

using namespace transcribe;
using namespace std::chrono_literals;
using insertion::platform::ClipboardFailure;
using insertion::platform::retry_snapshot;

TEST(Clipboard, SnapshotRetriesTransientReadsButNotUnsupportedFormats) {
    int reads = 0;
    auto saved = retry_snapshot(
        [&] {
            if (++reads == 1) throw ClipboardFailure("read", 49373, 0);
            return std::string("preserved");
        },
        1s);
    EXPECT_EQ(reads, 2);
    EXPECT_EQ(saved, "preserved");

    int attempts = 0;
    EXPECT_THROW(retry_snapshot(
                     [&]() -> int {
                         ++attempts;
                         throw Error("Clipboard format 3 cannot be preserved");
                     },
                     1s),
                 Error);
    EXPECT_EQ(attempts, 1);

    try {
        retry_snapshot([]() -> int { throw ClipboardFailure("read", 49373, 5); }, 0s);
        FAIL();
    } catch (const Error& error) {
        EXPECT_STREQ(error.what(), "Clipboard read failed (format 49373): Windows error 5");
    }
}
