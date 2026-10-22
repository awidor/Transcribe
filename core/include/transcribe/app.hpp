#pragma once

#include "transcribe/audio.hpp"
#include "transcribe/azure.hpp"
#include "transcribe/hotkey.hpp"
#include "transcribe/provider.hpp"
#include "transcribe/s1.hpp"
#include "transcribe/storage.hpp"

#include <cstdint>
#include <filesystem>
#include <functional>
#include <memory>
#include <optional>
#include <set>
#include <string>
#include <vector>

// Everything the windows show, decided once for every shell: the session, history, settings
// and updates, plus the rules each control follows. Shells only draw it.
namespace transcribe::app {

enum class Phase { Idle, Starting, Recording, Transcribing, Cleaning, Inserting, Done, Error };
const char* name(Phase phase);

// A session holds the microphone or the pipeline in these phases.
bool busy(Phase phase);
// The phases that disable recording controls; recording itself stays stoppable.
bool working(Phase phase);

struct Session {
    Phase phase = Phase::Idle;
    std::optional<std::int64_t> started_at;
    std::optional<std::string> error;
    bool retrying = false;
    // The saved transcript the widget offers for dragging after paste fails.
    std::optional<std::string> transcript;
    azure::Words words;
};

// Text appears as soon as insertion starts; the rest of that phase only keeps the clipboard
// available to the destination, so it already counts as done.
bool finished(const Session& session);
bool processing(const Session& session);
// "Starting", "Transcribing", "Cleaning up", "Retrying" or "Done"; nothing while idle or
// recording.
std::optional<std::string> status_label(const Session& session);

// Words as spoken: confirmed text, then the dimmed guess that follows it.
std::optional<azure::Words> spoken(const azure::Words& words);

// What the widget shows: the session pill, an error, or the unpasted transcript card.
enum class WidgetMode { Session, Error, Card };
WidgetMode widget_mode(const Session& session);

// "mm:ss" since `started_at`, both in milliseconds since the epoch.
std::string clock(std::optional<std::int64_t> started_at, std::int64_t now);
// "1.2 GB" or "350 MB".
std::string size_label(std::uint64_t bytes);
std::size_t word_count(std::string_view text);
// Case-insensitive search through transcripts.
bool matches(std::string_view text, std::string_view search);

// The text a history row shows.
std::string entry_preview(const storage::Entry& entry, const Session& session);
// A transcript that was saved but could not be pasted.
bool not_pasted(const storage::Entry& entry);

// Moves `value` toward `goal` at `rate` per second, independent of frame rate.
double toward(double value, double goal, double rate, double dt);

// Loudness from 0 to 1, judged against this microphone's own room: quiet and loud microphones
// both reach the top, while background noise stays near zero. Readings of zero (before the
// microphone reports) are ignored.
class Loudness {
  public:
    double operator()(double level, double dt);

  private:
    struct Sample {
        double at;
        double db;
    };
    std::vector<Sample> recent_;
    double now_ = 0;
    std::optional<double> floor_;
    double peak_ = 0;
    double loud_ = 0;
};

// Labels for the settings controls.
struct Choice {
    std::string label;
    std::optional<std::uint64_t> seconds;
};
const std::vector<Choice>& unload_choices();
const char* reasoning_label(provider::ReasoningEffort effort);
const char* styling_label(s1::Styling styling);
const std::vector<provider::ReasoningEffort>& reasoning_efforts();
// The thinking levels the chosen model offers; every level for a model outside the catalog.
std::vector<provider::ReasoningEffort>
available_efforts(const std::vector<provider::CleanupModel>& models, const std::string& model);
// The saved level when the model offers it, otherwise the model's default.
std::optional<provider::ReasoningEffort>
effective_effort(const storage::Settings& settings,
                 const std::vector<provider::CleanupModel>& models);

// The Streaming switch, the Transcription choice and the Cleanup choice. MAI-Transcribe-2's
// clean style needs the whole recording and MAI-Transcribe-2 itself: turning streaming off or
// choosing MAI-Transcribe-2 picks it, choosing it turns streaming off and picks
// MAI-Transcribe-2, and turning streaming on or choosing Voxtral returns to the cleanup chosen
// before.
class StreamingChoice {
  public:
    void streaming(storage::Settings& settings, bool on);
    void model(storage::Settings& settings, provider::TranscriptionModel model);
    // Shells also call this with the saved cleanup when they load settings.
    void cleanup(storage::Settings& settings, provider::CleanupEngine engine);

  private:
    provider::CleanupEngine previous_ = provider::CleanupEngine::OpenRouter;
};

// The keys a saved shortcut shows: a native binding's labels, or a legacy accelerator split
// at "+".
std::vector<std::string> shortcut_labels(const std::string& shortcut);

// Records a shortcut from capture events, which can arrive before the capture's token does.
class ShortcutRecorder {
  public:
    enum class Mode { Idle, Starting, Listening };
    struct Result {
        bool stopped = false;
        std::optional<std::string> shortcut;
        // The capture to end with App::end_shortcut_capture once stopped.
        std::optional<std::uint64_t> ended;
    };
    void starting();
    // Returns what the events buffered before the token amounted to.
    Result begin(std::uint64_t token, std::string platform);
    Result receive(const hotkey::Event& event);
    // Returns the capture to end, if it had begun.
    std::optional<std::uint64_t> stop();
    Mode mode() const { return mode_; }
    std::optional<std::uint64_t> token() const { return token_; }
    const std::string& platform() const { return platform_; }
    const std::vector<std::string>& keys() const { return keys_; }

  private:
    Mode mode_ = Mode::Idle;
    std::optional<std::uint64_t> token_;
    std::string platform_;
    std::vector<hotkey::Event> buffered_;
    std::vector<std::string> keys_;
};

// Windows withholds input from the global shortcut listener while Transcribe's window is in
// front, so the window reports its own keys. Like the listener, the key completing the
// shortcut never reaches the window, and an Alt in the shortcut must not open the window menu,
// which would take the next keys.
class WindowShortcut {
  public:
    // True when the window should not handle the key.
    bool key(std::uint32_t code, bool down, const hotkey::Chord& binding);
    void blur();

  private:
    std::set<std::uint32_t> held_;
    std::set<std::uint32_t> swallowed_;
};

struct Bootstrap {
    std::vector<storage::Entry> entries;
    storage::Settings settings;
    std::vector<std::string> microphones;
    bool has_azure_key = false;
    bool has_open_router_key = false;
    Session session;
};

struct CaptureSession {
    std::uint64_t token;
    std::string platform;
};

struct UpdateView {
    // "unsupported", "idle", "checking", "current", "available", "installing" or "error".
    std::string phase;
    std::string current;
    std::optional<std::string> version;
    std::optional<std::string> error;
};
// Update status text and the button it offers.
std::optional<std::string> update_status(const UpdateView& update);
std::string update_action(const UpdateView& update);
bool update_busy(const UpdateView& update);

// What installs updates for the build that is running.
struct Updater {
    // The newer version on offer, or nothing when this one is current.
    std::function<std::optional<std::string>()> check;
    // Downloads the version `check` found and hands it to the installer, which applies it and
    // restarts the app once the app exits.
    std::function<void()> install;
};

// Runs the installer's hooks; call it first in main, as it may end the process.
void startup();
// Updates from the GitHub releases for builds Velopack installed; nothing for other builds.
std::optional<Updater> updater();

inline constexpr const char* FOCUS_SETTINGS = "Focus Settings to record a shortcut";

// The callbacks run on background threads, some while the app holds its own locks: never call
// back into the app from them; hand the values to the UI thread instead.
struct Listener {
    std::function<void(const Session&)> session;
    // About 30 times a second while recording.
    std::function<void(float)> level;
    std::function<void()> history;
    // Show the main window on History.
    std::function<void()> open_history;
    std::function<void(const s1::Status&)> s1;
    // Capture and Cancelled events for the shortcut recorder.
    std::function<void(const hotkey::Event&)> capture;
    std::function<void(const UpdateView&)> update;
    // Re-evaluate the widget with widget_to_show(false) on the UI thread.
    std::function<void()> show_widget;
    std::function<void()> hide_widget;
    // Whether the main window is in front; read from background threads.
    std::function<bool()> main_focused;
};

class App {
  public:
    // Opens the data in `data` and registers the saved shortcut.
    static std::shared_ptr<App> create(const std::filesystem::path& data, Listener listener,
                                       std::optional<Updater> updater);
    ~App();
    App(const App&) = delete;
    App& operator=(const App&) = delete;

    // The commands below block; shells run them away from the UI thread. Failures throw
    // transcribe::Error with the message to show.
    Bootstrap bootstrap();
    std::vector<storage::Entry> history();
    void copy_entry(const std::string& id);
    void edit_entry(const std::string& id, const std::string& text);
    void delete_entry(const std::string& id);
    // `key` and `azure_key` replace the saved keys when present.
    storage::Settings save_settings(storage::Settings settings, std::optional<std::string> key,
                                    std::optional<std::string> azure_key);
    std::vector<provider::CleanupModel> cleanup_models();
    s1::Status s1_status();
    // Progress and failures arrive through Listener::s1.
    void download_s1(s1::Part part);
    void cancel_s1_download(s1::Part part);

    // Shells check that Settings has focus before and after beginning a capture.
    CaptureSession begin_shortcut_capture();
    std::vector<hotkey::Event> capture_shortcut_key(std::uint64_t token, const hotkey::Key& key,
                                                    bool down);
    void window_shortcut_key(const hotkey::Key& key, bool down);
    hotkey::Chord shortcut_binding();
    void end_shortcut_capture(std::optional<std::uint64_t> token);

    void open_history();
    // `automatic` sessions paste where the user is typing; others only save.
    void toggle(bool automatic);
    void cancel();
    void import_audio(const std::filesystem::path& path);

    // Called on the UI thread. The session the widget should show now, or nothing to leave it.
    // `active_only` re-shows a live recording that something else hid.
    std::optional<Session> widget_to_show(bool active_only);
    // After showing the widget: the failure, or nothing.
    void widget_shown(std::optional<std::string> error);

    UpdateView update_state();
    UpdateView check_update();
    // The shell exits afterwards, so the update can be applied.
    void install_update();

    void shutdown();

    struct State;

  private:
    explicit App(std::shared_ptr<State> state);
    std::shared_ptr<State> state_;
};

// Where the Tauri app kept its data, so history, keys and settings carry over.
std::filesystem::path data_dir();

} // namespace transcribe::app
