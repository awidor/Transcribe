#include "hotkey/engine.hpp"

#include <gtest/gtest.h>

using namespace transcribe::hotkey;

namespace {

std::vector<Event> input(Engine& e, std::uint32_t key, bool down) {
    return e.input(Key{key, std::to_string(key)}, down, key < 10).second;
}

Engine engine(const std::vector<std::uint32_t>& binding) {
    Engine e;
    for (auto key : binding)
        e.binding.push_back({key});
    return e;
}

bool fires(const std::vector<Event>& events) {
    return std::ranges::any_of(events,
                               [](const auto& e) { return std::holds_alternative<Activate>(e); });
}

const Capture* finished(const std::vector<Event>& events) {
    for (const auto& event : events)
        if (auto capture = std::get_if<Capture>(&event); capture && capture->shortcut)
            return capture;
    return nullptr;
}

void arm(Engine& e, bool armed) {
    e.capture = Engine::Capture{1, Clock::now() + std::chrono::seconds(5), {}, armed};
}

} // namespace

TEST(Engine, ChordFiresOnceOnReleaseInAnyOrderAndNeverAsPartOfAnotherShortcut) {
    auto tap = engine({1});
    EXPECT_FALSE(fires(input(tap, 1, true)));
    EXPECT_FALSE(fires(input(tap, 1, true)));
    EXPECT_TRUE(fires(input(tap, 1, false)));
    EXPECT_FALSE(fires(input(tap, 1, false)));
    for (auto [k, d] : {std::pair{1, true}, {20, true}, {20, false}, {1, false}})
        EXPECT_FALSE(fires(input(tap, k, d)));

    auto chord = engine({1, 20, 30});
    for (auto [k, d] : {std::pair{30, true}, {1, true}, {20, true}, {1, false}, {30, false}})
        EXPECT_FALSE(fires(input(chord, k, d)));
    EXPECT_TRUE(fires(input(chord, 20, false)));

    // Sequential keys are not a chord, and extra keys invalidate it.
    for (auto sequence :
         {std::vector<std::pair<int, bool>>{
              {1, true}, {20, true}, {20, false}, {30, true}, {30, false}, {1, false}},
          std::vector<std::pair<int, bool>>{{1, true},
                                            {20, true},
                                            {30, true},
                                            {40, true},
                                            {40, false},
                                            {30, false},
                                            {20, false},
                                            {1, false}}}) {
        auto e = engine({1, 20, 30});
        for (auto [k, d] : sequence)
            EXPECT_FALSE(fires(input(e, k, d)));
    }
}

TEST(Engine, ReconfigurationOrPauseCannotFireAHeldKey) {
    auto e = engine({1});
    input(e, 1, true);
    e.reset();
    EXPECT_FALSE(fires(input(e, 1, false)));
    e.paused = true;
    input(e, 1, true);
    EXPECT_FALSE(fires(input(e, 1, false)));
}

TEST(Engine, AMenuKeyReleasedAfterItsShortcutIsHeldBackForMasking) {
    std::uint32_t alt = 1;
    auto run = [&](std::vector<std::uint32_t> binding, std::vector<std::uint32_t> keys,
                   bool paused) {
        auto e = engine(binding);
        e.menu_keys = {alt};
        e.paused = paused;
        for (auto key : keys) {
            input(e, key, true);
            input(e, key, true);
        }
        std::vector<std::uint32_t> held_back;
        bool fired = false;
        for (auto key = keys.rbegin(); key != keys.rend(); ++key) {
            auto [suppress, events] = e.input(Key{*key, std::to_string(*key)}, false, true);
            fired |= fires(events);
            if (auto release = std::exchange(e.mask_release, std::nullopt)) {
                EXPECT_TRUE(suppress);
                held_back.push_back(*release);
            }
        }
        return std::pair{held_back, fired};
    };
    using Result = std::pair<std::vector<std::uint32_t>, bool>;
    EXPECT_EQ(run({alt}, {alt}, false), (Result{{alt}, true}));
    EXPECT_EQ(run({alt, 30}, {alt, 30}, false), (Result{{alt}, true}));
    EXPECT_EQ(run({2, 30}, {2, 30}, false), (Result{{}, true}));
    // A dedicated menu key stays quiet even while the shortcut is ignored.
    EXPECT_EQ(run({alt}, {alt}, true), (Result{{alt}, false}));
}

TEST(Engine, EscapeDismissesASessionUnlessTheShortcutUsesIt) {
    auto e = engine({1});
    e.escape = {27};
    e.dismissible = true;
    auto [suppress, events] = e.input(Key{27, "Esc"}, true, false);
    EXPECT_TRUE(suppress);
    ASSERT_EQ(events.size(), 1u);
    EXPECT_TRUE(std::holds_alternative<Dismiss>(events[0]));
    EXPECT_TRUE(e.input(Key{27, "Esc"}, false, false).first);
    EXPECT_TRUE(e.swallowed.empty() && e.held.empty());

    auto own = engine({27});
    own.escape = {27};
    own.dismissible = true;
    input(own, 27, true);
    EXPECT_TRUE(fires(input(own, 27, false)));
}

TEST(Capture, RecordsTheWholeChordAsSerdeJsonWithoutActivating) {
    auto e = engine({1});
    arm(e, true);
    input(e, 1, true);
    input(e, 2, true);
    input(e, 1, false);
    auto events = input(e, 2, false);
    EXPECT_FALSE(fires(events));
    auto capture = finished(events);
    ASSERT_TRUE(capture);
    EXPECT_EQ(*capture->shortcut,
              R"({"platform":")" + platform() +
                  R"(","keys":[{"code":1,"label":"1"},{"code":2,"label":"2"}]})");
}

TEST(Capture, DrainsTheKeyThatOpenedTheRecorder) {
    auto e = engine({1});
    input(e, 20, true);
    arm(e, false);
    e.reset();
    EXPECT_TRUE(input(e, 20, false).empty());
    EXPECT_TRUE(e.capture->armed);
    input(e, 1, true);
    auto events = input(e, 1, false);
    auto capture = finished(events);
    ASSERT_TRUE(capture);
    EXPECT_EQ(capture->keys, (std::vector<Key>{{1, "1"}}));
    EXPECT_FALSE(fires(events));
}

TEST(Capture, CancelOrExpiryReleasesKeysWithoutActivation) {
    auto e = engine({1});
    arm(e, true);
    input(e, 1, true);
    e.cancel();
    auto [suppress, events] = e.input(Key{1, "Ctrl"}, false, true);
    EXPECT_TRUE(suppress);
    EXPECT_FALSE(fires(events));
    EXPECT_TRUE(e.swallowed.empty());

    auto expired = engine({1});
    input(expired, 1, true);
    expired.capture = Engine::Capture{1, Clock::now() - std::chrono::seconds(1), {}, true};
    events = input(expired, 1, false);
    ASSERT_FALSE(events.empty());
    EXPECT_EQ(std::get<Cancelled>(events[0]).token, 1u);
    EXPECT_FALSE(fires(events));
}

TEST(Binding, JsonMatchesSerde) {
    Binding binding =
        nlohmann::json::parse(R"({"platform":"linux","keys":[{"code":37,"label":"Ctrl"}]})");
    EXPECT_EQ(binding.platform, "linux");
    EXPECT_EQ(binding.keys, (std::vector<Key>{{37, "Ctrl"}}));
    EXPECT_THROW(parse(R"({"platform":"linux","keys":[{"code":-1,"label":""}]})"),
                 transcribe::Error);
    EXPECT_THROW(parse(R"({"platform":"linux","keys":[{"code":1.5,"label":""}]})"),
                 transcribe::Error);
}
