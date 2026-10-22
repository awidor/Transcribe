#include "insertion/destination.hpp"
#include "insertion/platform.hpp"

#include <gtest/gtest.h>

using namespace transcribe::insertion;

TEST(PrepareText, TerminalInputCannotSubmitOrInjectEscapeSequences) {
    EXPECT_EQ(prepare_text("echo hi\r\nrm file\n\x1b\x03", true), "echo hi rm file ");
    EXPECT_EQ(prepare_text("Grüße\n世界 👋", false), "Grüße\n世界 👋");
}

TEST(Destination, RejectsOnlyClearNonTextFocus) {
    using destination::windows_rejects;
    // Measured from Chromium: page body, fields, read-only input, bare contenteditable.
    EXPECT_TRUE(windows_rejects(50030, true));
    EXPECT_FALSE(windows_rejects(50004, false));
    EXPECT_TRUE(windows_rejects(50004, true));
    EXPECT_FALSE(windows_rejects(50026, std::nullopt));
    EXPECT_TRUE(windows_rejects(50000, std::nullopt));
    // A writable spreadsheet cell, editors without a Value pattern, and unknown focus.
    EXPECT_FALSE(windows_rejects(50029, false));
    EXPECT_FALSE(windows_rejects(50030, std::nullopt));
    EXPECT_FALSE(windows_rejects(std::nullopt, std::nullopt));
}
