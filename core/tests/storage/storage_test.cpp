#include "support/server.hpp"
#include "transcribe/storage.hpp"

#include <gtest/gtest.h>

#include <sqlite3.h>

#include <algorithm>
#include <fstream>

using namespace transcribe;
using provider::CleanupEngine;
using provider::ReasoningEffort;
using s1::Styling;
using storage::Entry;
using storage::Settings;
using storage::Store;

namespace {

void execute(const std::filesystem::path& path, const char* sql) {
    sqlite3* db = nullptr;
    ASSERT_EQ(sqlite3_open(path.string().c_str(), &db), SQLITE_OK);
    ASSERT_EQ(sqlite3_exec(db, sql, nullptr, nullptr, nullptr), SQLITE_OK);
    sqlite3_close(db);
}

} // namespace

TEST(Storage, LegacySettingsPreservePreferencesAndReloadCleanupChoices) {
    test::TempDir dir;
    auto path = dir.path() / "history.sqlite";
    {
        Store store(path);
        execute(
            path,
            R"(INSERT INTO settings VALUES (1,'{"microphone":"Studio USB","shortcut":"Alt+Space","shortcutLabel":"Alt + Space"}'))");
        auto settings = store.settings();
        EXPECT_EQ(settings.microphone, "Studio USB");
        EXPECT_EQ(settings.shortcut, "Alt+Space");
        EXPECT_EQ(settings.shortcut_label, "Alt + Space");
        EXPECT_EQ(settings.cleanup_model, provider::DEFAULT_CLEANUP_MODEL);
        EXPECT_EQ(settings.cleanup_reasoning_effort, ReasoningEffort::Low);
        EXPECT_EQ(settings.cleanup_engine, CleanupEngine::OpenRouter);
        EXPECT_EQ(settings.cleanup_styling, Styling::SemiFormal);
        EXPECT_EQ(settings.cleanup_unload_seconds, 300u);
        EXPECT_TRUE(settings.streaming);
        settings.cleanup_model = "custom/new-model";
        settings.cleanup_reasoning_effort.reset();
        settings.cleanup_engine = CleanupEngine::S1Mini;
        settings.cleanup_styling = Styling::SemiCasual;
        settings.cleanup_unload_seconds.reset();
        settings.streaming = false;
        store.save_settings(settings);
    }
    {
        Store store(path);
        auto settings = store.settings();
        EXPECT_EQ(settings.microphone, "Studio USB");
        EXPECT_EQ(settings.shortcut, "Alt+Space");
        EXPECT_EQ(settings.cleanup_model, "custom/new-model");
        EXPECT_EQ(settings.cleanup_reasoning_effort, std::nullopt);
        EXPECT_EQ(settings.cleanup_engine, CleanupEngine::S1Mini);
        EXPECT_EQ(settings.cleanup_styling, Styling::SemiCasual);
        EXPECT_EQ(settings.cleanup_unload_seconds, std::nullopt);
        EXPECT_FALSE(settings.streaming);
        settings.cleanup_reasoning_effort = ReasoningEffort::Xhigh;
        settings.cleanup_engine = CleanupEngine::MaiTranscribe;
        store.save_settings(settings);
    }
    EXPECT_EQ(Store(path).settings().cleanup_reasoning_effort, ReasoningEffort::Xhigh);
    EXPECT_EQ(Store(path).settings().cleanup_engine, CleanupEngine::MaiTranscribe);
}

TEST(Storage, ReadsSettingsTheRustAppSaved) {
    test::TempDir dir;
    auto path = dir.path() / "history.sqlite";
    Store store(path);
    execute(
        path,
        R"(INSERT INTO settings VALUES (1,'{"microphone":null,"shortcut":"{\"platform\":\"windows\",\"keys\":[]}","shortcutLabel":null,"cleanupModel":"m","cleanupReasoningEffort":null,"cleanupEngine":"off","cleanupStyling":"formal","cleanupUnloadSeconds":120}'))");
    auto settings = store.settings();
    EXPECT_EQ(settings.microphone, std::nullopt);
    EXPECT_EQ(settings.shortcut, R"({"platform":"windows","keys":[]})");
    EXPECT_EQ(settings.cleanup_reasoning_effort, std::nullopt);
    EXPECT_EQ(settings.cleanup_engine, CleanupEngine::Off);
    EXPECT_EQ(settings.cleanup_styling, Styling::Formal);
    EXPECT_EQ(settings.cleanup_unload_seconds, 120u);
    // A malformed value or a missing shortcut falls back to the defaults, as serde did.
    execute(path, R"(UPDATE settings SET data='{"shortcut":"A","cleanupEngine":"other"}')");
    EXPECT_EQ(store.settings().shortcut, Settings{}.shortcut);
    execute(path, R"(UPDATE settings SET data='{"microphone":"USB"}')");
    EXPECT_EQ(store.settings().microphone, std::nullopt);
}

TEST(Storage, RemovesOnlyLegacyAppAudioFiles) {
    test::TempDir dir;
    auto recordings = dir.path() / "recordings";
    std::filesystem::create_directory(recordings);
    auto audio = recordings / (uuid_v4() + ".audio");
    std::ofstream(audio) << "audio";
    auto other = recordings / "unrelated.txt";
    std::ofstream(other) << "keep";
    storage::remove_legacy_audio(dir.path());
    EXPECT_FALSE(std::filesystem::exists(audio));
    EXPECT_TRUE(std::filesystem::exists(other));
    storage::remove_legacy_audio(dir.path());
}

TEST(Storage, SilenceAndCancellationAreNotKeptAsFailures) {
    test::TempDir dir;
    auto path = dir.path() / "history.sqlite";
    {
        Store store(path);
        for (auto [id, text, error] : {std::tuple{"silent", "", "No speech detected"},
                                       {"cancelled", "", "Cancelled"},
                                       {"offline", "", "Transcription connection failed"},
                                       {"kept", "Hello.", "Cancelled"}})
            store.insert({id, 1, text, 1., "failed", std::string(error)});
    }
    std::vector<std::string> ids;
    for (const auto& entry : Store(path).list())
        ids.push_back(entry.id);
    std::sort(ids.begin(), ids.end());
    EXPECT_EQ(ids, (std::vector<std::string>{"kept", "offline"}));
}

TEST(Storage, UnicodeHistoryAndSettingsRoundtrip) {
    Store store(":memory:");
    Entry e{"one", 1, "Grüße 你好 👋", 1.5, "pending", std::nullopt};
    store.insert(e);
    store.status("one", "unverified", std::nullopt);
    EXPECT_EQ(store.list()[0].text, e.text);
    Settings settings;
    settings.microphone = "USB";
    store.save_settings(settings);
    EXPECT_EQ(store.settings().microphone, settings.microphone);
    store.remove("one");
    EXPECT_TRUE(store.list().empty());
}

TEST(Storage, InterruptedSessionsAreResolvedOnOpen) {
    test::TempDir dir;
    auto path = dir.path() / "history.sqlite";
    {
        Store store(path);
        store.insert({"pending", 1, "a", 1., "pending", std::nullopt});
        store.insert({"transcribing", 2, "b", 1., "transcribing", std::nullopt});
    }
    auto entries = Store(path).list();
    ASSERT_EQ(entries.size(), 2u);
    EXPECT_EQ(entries[0].status, "failed");
    EXPECT_EQ(entries[0].error, "Interrupted");
    EXPECT_EQ(entries[1].status, "unverified");
}
