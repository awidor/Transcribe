#include "transcribe/app.hpp"

#include <gtest/gtest.h>

#include <algorithm>
#include <cmath>

using namespace transcribe::app;
using transcribe::hotkey::Cancelled;
using transcribe::hotkey::Capture;
using transcribe::provider::CleanupEngine;
using transcribe::provider::TranscriptionModel;

namespace {

// Feeds `level` at 60 frames per second and returns the highest reading after the first second.
template <class F> double highest(Loudness& meter, F level, double seconds) {
    double high = 0;
    for (int frame = 0; frame < seconds * 60; ++frame) {
        double reading = meter(level(frame), 1.0 / 60);
        if (frame >= 60) high = std::max(high, reading);
    }
    return high;
}
double decibels(double db) {
    return std::pow(10, db / 20);
}
// Deterministic ±3 dB jitter, like a room's steady noise.
double jitter(int frame) {
    return std::sin(frame * 12.9898) * 3;
}

} // namespace

TEST(Loudness, FillsTheRangeForQuietMicrophonesButIgnoresTheRoom) {
    Loudness quiet;
    highest(quiet, [](int frame) { return decibels(-80 + jitter(frame)); }, 2);
    double voice = 0;
    for (int frame = 0; frame < 24; ++frame)
        voice = quiet(decibels(-58), 1.0 / 60);
    EXPECT_GT(voice, 0.7);

    Loudness noise;
    EXPECT_LT(highest(noise, [](int frame) { return decibels(-65 + jitter(frame)); }, 4), 0.05);
}

TEST(WindowShortcut, WithholdsOnlyTheKeyThatCompletesTheShortcut) {
    WindowShortcut window;
    transcribe::hotkey::Chord chord = {{162, 163}, {160, 161}, {32}};
    EXPECT_FALSE(window.key(32, true, chord));
    EXPECT_FALSE(window.key(32, false, chord));
    EXPECT_FALSE(window.key(162, true, chord));
    EXPECT_FALSE(window.key(160, true, chord));
    EXPECT_TRUE(window.key(32, true, chord));
    EXPECT_TRUE(window.key(32, false, chord));
    EXPECT_FALSE(window.key(160, false, chord));
    // Keys still down when the window loses focus are not part of the next chord.
    window.blur();
    EXPECT_FALSE(window.key(32, true, chord));
    // A bare Alt shortcut never reaches the window menu.
    EXPECT_TRUE(window.key(165, true, {{165}}));
}

TEST(ShortcutRecorder, ReplaysEventsThatArriveBeforeTheTokenAndIgnoresOlderCaptures) {
    ShortcutRecorder recorder;
    recorder.starting();
    EXPECT_FALSE(recorder.receive(Capture{6, {}, "stale"}).stopped);
    EXPECT_FALSE(recorder.receive(Capture{7, {{100, "Right Alt"}}, "binding"}).stopped);
    auto result = recorder.begin(7, "linux");
    EXPECT_TRUE(result.stopped);
    EXPECT_EQ(result.shortcut, "binding");
    EXPECT_EQ(result.ended, 7u);
    EXPECT_EQ(recorder.mode(), ShortcutRecorder::Mode::Idle);

    recorder.starting();
    recorder.begin(8, "linux");
    EXPECT_FALSE(recorder.receive(Cancelled{7}).stopped);
    EXPECT_TRUE(recorder.receive(Cancelled{8}).stopped);
}

TEST(StreamingChoice, StreamingOffPicksCleanTranscriptionAndOnRestoresTheCleanup) {
    transcribe::storage::Settings settings;
    StreamingChoice choice;
    choice.cleanup(settings, CleanupEngine::S1Mini);
    choice.streaming(settings, false);
    EXPECT_EQ(settings.cleanup_engine, CleanupEngine::MaiTranscribe);
    choice.streaming(settings, true);
    EXPECT_EQ(settings.cleanup_engine, CleanupEngine::S1Mini);
    choice.cleanup(settings, CleanupEngine::MaiTranscribe);
    EXPECT_FALSE(settings.streaming);
    // Voxtral has no clean style.
    choice.model(settings, TranscriptionModel::Voxtral);
    EXPECT_EQ(settings.cleanup_engine, CleanupEngine::S1Mini);
    choice.streaming(settings, true);
    choice.streaming(settings, false);
    EXPECT_EQ(settings.cleanup_engine, CleanupEngine::S1Mini);
    choice.cleanup(settings, CleanupEngine::MaiTranscribe);
    EXPECT_EQ(settings.transcription_model, TranscriptionModel::MaiTranscribe);
}
