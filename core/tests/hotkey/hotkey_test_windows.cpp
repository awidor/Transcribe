#include "transcribe/hotkey.hpp"
#include "transcribe/util.hpp"

#include <gtest/gtest.h>

using namespace transcribe::hotkey;

TEST(Windows, PreservesLegacyDefaultsAndRejectsReservedOrForeignBindings) {
    auto service = Service::create([](const Event&) {});
    EXPECT_EQ(service->validate("CommandOrControl+Shift+Space"),
              (Chord{{162, 163}, {160, 161}, {32}}));
    EXPECT_THROW(service->validate("Control+Alt+Delete"), transcribe::Error);
    EXPECT_THROW(service->validate("Super+L"), transcribe::Error);
    EXPECT_THROW(service->validate("Ctrl+Ctrl"), transcribe::Error);
    EXPECT_THROW(service->validate(R"({"platform":"macos","keys":[{"code":59,"label":"Ctrl"}]})"),
                 transcribe::Error);
    EXPECT_THROW(service->validate(R"({"platform":"windows","keys":[]})"), transcribe::Error);
    EXPECT_EQ(service->validate("NumpadEnter"), (Chord{{269}}));
}
