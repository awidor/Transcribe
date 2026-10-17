#include "hotkey/engine.hpp"

#include <gtest/gtest.h>

using namespace transcribe::hotkey;

namespace {

std::string message(const std::string& text) {
    try {
        evdev::binding(text);
    } catch (const transcribe::Error& error) {
        return error.what();
    }
    return "";
}

} // namespace

TEST(Evdev, MigratesRightAltWithoutLosingItsSide) {
    EXPECT_EQ(evdev::binding(R"({"platform":"wayland","keys":[{"code":0,"label":"AltRight"}]})"),
              (Chord{{100}}));
    EXPECT_EQ(evdev::binding(R"({"platform":"linux","keys":[{"code":108,"label":"Right Alt"}]})"),
              (Chord{{100}}));
}

TEST(Evdev, SupportsSingleKeysMediaAndArbitraryChords) {
    for (const auto& codes : std::vector<std::vector<std::uint32_t>>{
             {30}, {100}, {183}, {464}, {30, 48, 100}, {29, 97}}) {
        Binding binding{"linux-evdev", {}};
        Chord expected;
        for (auto code : codes) {
            binding.keys.push_back({code, ""});
            expected.push_back({code});
        }
        EXPECT_EQ(evdev::binding(nlohmann::json(binding).dump()), expected);
    }
    EXPECT_EQ(evdev::binding("CommandOrControl+Shift+Space"), (Chord{{29, 97}, {42, 54}, {57}}));
    EXPECT_EQ(evdev::binding("KeyA+Digit1+Numpad5+F13"), (Chord{{30}, {2}, {76}, {183}}));
}

TEST(Evdev, RejectsInvalidBindingsWithTheRustMessages) {
    EXPECT_EQ(message("Ctrl+Control"), "Duplicate shortcut key");
    EXPECT_EQ(message("Nonsense"), "Shortcut needs to be recorded again");
    EXPECT_EQ(message(R"({"platform":"windows","keys":[{"code":1,"label":""}]})"),
              "Shortcut belongs to another platform");
    EXPECT_EQ(message(R"({"platform":"linux-evdev","keys":[]})"), "Invalid shortcut");
    EXPECT_EQ(message(R"({"platform":"linux-evdev","keys":[{"code":272,"label":""}]})"),
              "Unsupported Linux key");
    EXPECT_EQ(message("{"), "Invalid shortcut");
}
