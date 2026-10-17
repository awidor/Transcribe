#include "transcribe/app.hpp"

#include "transcribe/credentials.hpp"
#include "transcribe/insertion.hpp"
#ifdef __linux__
#include "transcribe/linux_input.hpp"
#endif

#include <iostream>
#include <mutex>
#include <thread>

#ifdef _WIN32
#include <shlobj.h>
#include <windows.h>
#endif

namespace transcribe::app {

namespace {

struct SessionState {
    Session view;
    std::string id;
    std::unique_ptr<audio::Recorder> recorder;
    bool automatic = false;
    bool widget_requested = false;
    std::stop_source cancel;
};

std::optional<std::chrono::seconds> unload(const storage::Settings& settings) {
    if (!settings.cleanup_unload_seconds) return std::nullopt;
    return std::chrono::seconds(*settings.cleanup_unload_seconds);
}

// Runs `work` in the background, like a spawned task: failures there only end the task.
template <class F> void spawn(F work) {
    std::thread([work = std::move(work)]() mutable {
        try {
            work();
        } catch (const std::exception& e) {
            std::cerr << e.what() << '\n';
        }
    }).detach();
}

struct Job {
    std::string id;
    std::string text;
    double seconds;
    bool automatic;
    std::stop_token cancel;
};

} // namespace

struct App::State : std::enable_shared_from_this<App::State> {
    Listener listener;
    std::mutex store_mutex;
    std::unique_ptr<storage::Store> store;
    std::mutex session_mutex;
    SessionState session;
    provider::OpenRouter openrouter;
    // The region found for the saved Azure key, with the key it belongs to.
    std::mutex azure_region_mutex;
    std::optional<std::pair<std::string, std::string>> azure_region;
    std::filesystem::path api_key;
    std::shared_ptr<hotkey::Service> hotkeys;
    std::mutex settings_mutex;
    std::shared_ptr<s1::Engine> s1;
    std::mutex update_mutex;
    UpdateView update;
    std::optional<Updater> updater;

    std::filesystem::path azure_key_path() const { return api_key.parent_path() / "azure-api-key"; }

    storage::Settings settings() {
        std::lock_guard lock(store_mutex);
        return store->settings();
    }

    void history() {
        if (listener.history) listener.history();
    }

    // Called with the session locked.
    void emit(const SessionState& s) {
        auto phase = s.view.phase;
        hotkeys->dismissible(s.widget_requested &&
                             (phase == Phase::Starting || phase == Phase::Recording ||
                              phase == Phase::Transcribing || phase == Phase::Cleaning));
        if (listener.session) listener.session(s.view);
    }

    void show_widget() {
        if (listener.show_widget) listener.show_widget();
    }

    // Hides the widget once the pill has played its closing animation, unless a new session
    // has opened it again in the meantime.
    void close_widget() {
        spawn([self = shared_from_this()] {
            std::this_thread::sleep_for(std::chrono::milliseconds(260));
            {
                std::lock_guard lock(self->session_mutex);
                if (self->session.view.phase != Phase::Idle) return;
            }
            if (self->listener.hide_widget) self->listener.hide_widget();
        });
    }

    void error(const std::string& id, const std::string& message) {
        std::lock_guard lock(session_mutex);
        auto& s = session;
        if (s.id != id || s.cancel.stop_requested()) return;
        if (s.recorder) {
            s.recorder.reset();
            audio::chime(false);
        }
        s.view.phase = Phase::Error;
        s.view.error = message;
        emit(s);
    }

    // Ends a session that finished with nothing to show, such as silence.
    void dismiss(const std::string& id) {
        {
            std::lock_guard lock(session_mutex);
            if (session.id != id) return;
            session = SessionState();
            emit(session);
        }
        close_widget();
    }

    std::string azure_key() {
        try {
            return credentials::read(azure_key_path());
        } catch (const std::exception&) {
            throw Error("Azure API key required");
        }
    }

    std::string region_for(const std::string& key) {
        std::lock_guard lock(azure_region_mutex);
        if (azure_region && azure_region->first == key) return azure_region->second;
        auto region = azure::region(key);
        azure_region = {key, region};
        return region;
    }

    std::optional<provider::CleanupConfig> cleanup_config(const storage::Settings& settings) {
        switch (settings.cleanup_engine) {
        case provider::CleanupEngine::Off:
            return std::nullopt;
        case provider::CleanupEngine::S1Mini:
            return provider::LocalCleanup{s1, settings.cleanup_styling};
        case provider::CleanupEngine::OpenRouter:
            std::string key;
            try {
                key = credentials::read(api_key);
            } catch (const std::exception&) {
                throw Error("OpenRouter API key required");
            }
            return provider::OpenRouterCleanup{settings.cleanup_model,
                                               settings.cleanup_reasoning_effort, key};
        }
        return std::nullopt;
    }

    void register_shortcut(const std::string& shortcut) {
        auto binding = hotkeys->validate(shortcut);
        hotkeys->start();
        hotkeys->configure(std::move(binding));
    }

    void toggle(bool automatic);
    void stop_recording(const std::optional<std::string>& expected);
    void cancel();
    void transcribe(std::string id, std::string key, audio::Receiver audio, bool automatic,
                    std::stop_token cancel);
    void process(Job job);
    UpdateView change_update(const std::function<void(UpdateView&)>& change);
    UpdateView check_update(bool manual);
};

void App::State::toggle(bool automatic) {
    std::string id;
    {
        std::unique_lock lock(session_mutex);
        if (session.view.phase == Phase::Recording) {
            lock.unlock();
            stop_recording(std::nullopt);
            return;
        }
        if (busy(session.view.phase)) return;
        // A shortcut started in our own window has the same save-only intent as the Record
        // button. External shortcuts still resolve their destination at delivery; no
        // application or field is captured here.
        automatic = automatic && !(listener.main_focused && listener.main_focused());
        id = uuid_v4();
        session = SessionState();
        session.id = id;
        session.widget_requested = true;
        session.view.phase = Phase::Starting;
        emit(session);
    }
    show_widget();
    if (automatic) spawn([] { insertion::prepare(); });
    try {
        auto key = azure_key();
        auto settings = this->settings();
        // A missing cleanup key fails now, not after the user has spoken.
        cleanup_config(settings);
        if (settings.cleanup_engine == provider::CleanupEngine::S1Mini) {
            s1->check();
            // The model loads while the user is still speaking.
            spawn([engine = s1] { engine->warm(); });
        }
        auto [recorder, audio] =
            audio::Recorder::start(settings.microphone, [self = weak_from_this()](float level) {
                if (auto state = self.lock(); state && state->listener.level)
                    state->listener.level(level);
            });
        std::stop_token cancel;
        {
            std::lock_guard lock(session_mutex);
            if (session.id != id || session.cancel.stop_requested()) {
                audio->close();
                return;
            }
            session.recorder = std::move(recorder);
            session.automatic = automatic;
            session.view.phase = Phase::Recording;
            session.view.started_at = now_ms();
            cancel = session.cancel.get_token();
            emit(session);
        }
        audio::chime(true);
        show_widget();
        spawn([self = shared_from_this(), id, key, audio, automatic, cancel] {
            self->transcribe(id, key, audio, automatic, cancel);
        });
        spawn([self = shared_from_this(), id, cancel] {
            if (sleep_for(cancel, std::chrono::seconds(audio::MAX_SECONDS)))
                self->stop_recording(id);
        });
    } catch (const std::exception& e) {
        error(id, e.what());
        show_widget();
        throw Error(e.what());
    }
}

void App::State::stop_recording(const std::optional<std::string>& expected) {
    {
        std::lock_guard lock(session_mutex);
        if (session.view.phase != Phase::Recording || (expected && *expected != session.id)) return;
        // Ending capture closes the audio stream; Azure then finalizes the transcript.
        session.recorder.reset();
        session.view.phase = Phase::Transcribing;
        emit(session);
    }
    audio::chime(false);
}

void App::State::cancel() {
    {
        std::lock_guard lock(session_mutex);
        if (session.view.phase == Phase::Inserting) return;
        session.cancel.request_stop();
        if (session.recorder) {
            session.recorder.reset();
            audio::chime(false);
        }
        session = SessionState();
        emit(session);
    }
    close_widget();
}

// Streams a recording or an import to Azure, showing words as they arrive, then hands the
// transcript on for cleanup and delivery.
void App::State::transcribe(std::string id, std::string key, audio::Receiver audio, bool automatic,
                            std::stop_token cancel) {
    std::mutex words_mutex;
    azure::Words latest;
    auto words = [&](const azure::Words& next) {
        {
            std::lock_guard lock(words_mutex);
            latest = next;
        }
        bool appeared;
        {
            std::lock_guard lock(session_mutex);
            if (session.id != id || session.cancel.stop_requested()) return;
            // The widget grows to show words, and only resizes when they appear.
            appeared = session.view.words.empty() && !next.empty();
            session.view.words = next;
            emit(session);
        }
        if (appeared) show_widget();
    };
    std::optional<std::pair<std::string, double>> result;
    std::exception_ptr failure;
    try {
        // The microphone is already buffering while the region is looked up.
        auto region = region_for(key);
        auto stream = azure::Stream::connect({region, key}, cancel);
        result = stream.run(audio, words, cancel);
    } catch (...) {
        failure = std::current_exception();
    }
    // Nothing reads the audio any more; a decoding import stops here.
    audio->close();
    std::string confirmed;
    {
        std::lock_guard lock(words_mutex);
        confirmed = trim(latest.text);
    }
    if (result) {
        process({id, result->first, result->second, automatic, cancel});
        return;
    }
    if (cancel.stop_requested()) return;
    try {
        std::rethrow_exception(failure);
    } catch (const audio::NoSpeech&) {
        dismiss(id);
    } catch (const std::exception& e) {
        std::string message = e.what();
        // Words Azure already confirmed are kept with the failure.
        try {
            std::lock_guard lock(store_mutex);
            store->insert({id, now_ms(), confirmed, 0., "failed", message});
        } catch (const std::exception&) {
        }
        history();
        error(id, message);
        show_widget();
    }
}

void App::State::process(Job job) {
    auto& [id, text, seconds, automatic, cancel] = job;
    auto operation = [&] {
        if (cancel.stop_requested()) throw Error("Cancelled");
        // The raw transcript is saved first, so a failed cleanup never loses it.
        {
            std::lock_guard lock(store_mutex);
            store->insert({id, now_ms(), text, seconds, "transcribing", std::nullopt});
        }
        history();
        auto settings = this->settings();
        auto cleanup = cleanup_config(settings);
        if (cleanup) {
            {
                std::lock_guard lock(session_mutex);
                if (session.id == id && !session.cancel.stop_requested()) {
                    session.view.phase = Phase::Cleaning;
                    session.view.retrying = false;
                    emit(session);
                }
            }
            auto retrying = [&] {
                std::lock_guard lock(session_mutex);
                if (session.id != id || session.cancel.stop_requested()) return;
                if (session.view.phase == Phase::Cleaning && !session.view.retrying) {
                    session.view.retrying = true;
                    emit(session);
                }
            };
            try {
                text = openrouter.clean(text, *cleanup, retrying, cancel);
            } catch (const audio::NoSpeech&) {
                // Filler alone is silence, and leaves nothing to keep.
                {
                    std::lock_guard lock(store_mutex);
                    store->remove(id);
                }
                history();
                dismiss(id);
                return;
            }
        }
        {
            std::lock_guard lock(store_mutex);
            store->edit(id, text);
            store->status(id, "pending", std::nullopt);
        }
        history();
        // Persist first: insertion failures must never lose a successful transcript.
        {
            std::unique_lock lock(session_mutex);
            if (cancel.stop_requested()) throw Error("Cancelled");
            if (session.id != id) {
                lock.unlock();
                {
                    std::lock_guard store_lock(store_mutex);
                    store->status(id, "saved", std::nullopt);
                }
                history();
                return;
            }
            session.view.phase = Phase::Inserting;
            session.view.retrying = false;
            emit(session);
        }
        std::optional<std::string> insertion_error;
        {
            auto pause = hotkeys->pause();
            if (automatic) {
                try {
                    insertion::insert(id, text);
                } catch (const std::exception& e) {
                    insertion_error = e.what();
                }
            }
        }
        auto status = !insertion_error && automatic ? "unverified" : "saved";
        {
            std::lock_guard lock(store_mutex);
            store->status(id, status, insertion_error);
        }
        history();
        bool unpasted = false;
        {
            std::lock_guard lock(session_mutex);
            if (session.id == id) {
                session.view.phase = insertion_error ? Phase::Error : Phase::Done;
                session.view.transcript =
                    insertion_error ? std::optional<std::string>(text) : std::nullopt;
                session.view.error = insertion_error;
                emit(session);
                unpasted = session.view.transcript.has_value();
            }
        }
        // The widget grows into the transcript card.
        if (unpasted) show_widget();
    };
    try {
        operation();
    } catch (const std::exception& e) {
        if (cancel.stop_requested()) {
            // A cancelled transcription leaves nothing behind.
            try {
                std::lock_guard lock(store_mutex);
                store->remove(id);
            } catch (const std::exception&) {
            }
            history();
        } else {
            std::string message = e.what();
            try {
                std::lock_guard lock(store_mutex);
                store->status(id, "failed", message);
            } catch (const std::exception&) {
            }
            history();
            error(id, message);
        }
    }
    // Long enough for the pill's check to land before it closes.
    std::this_thread::sleep_for(std::chrono::milliseconds(300));
    bool closing = false;
    {
        std::lock_guard lock(session_mutex);
        if (session.id == id && session.view.phase == Phase::Done) {
            session = SessionState();
            emit(session);
            closing = true;
        }
    }
    if (closing) close_widget();
}

UpdateView App::State::change_update(const std::function<void(UpdateView&)>& change) {
    std::lock_guard lock(update_mutex);
    change(update);
    if (listener.update) listener.update(update);
    return update;
}

UpdateView App::State::check_update(bool manual) {
    if (!updater) throw Error("Updates unavailable");
    UpdateView previous;
    {
        std::lock_guard lock(update_mutex);
        if (update.phase == "checking" || update.phase == "installing") return update;
        previous = update;
        update.phase = "checking";
        update.error.reset();
        if (listener.update) listener.update(update);
    }
    std::optional<std::string> version;
    std::optional<std::string> failure;
    try {
        version = updater->check();
    } catch (const std::exception& e) {
        failure = e.what();
    }
    return change_update([&](UpdateView& view) {
        if (failure) {
            // Background checks stay quiet: offline machines and releases without an update
            // feed are not actionable.
            if (!manual)
                view = previous;
            else {
                view.phase = "error";
                view.error = failure;
            }
        } else if (version) {
            view.phase = "available";
            view.version = version;
        } else {
            view.phase = "current";
            view.version.reset();
        }
    });
}

App::App(std::shared_ptr<State> state) : state_(std::move(state)) {}

App::~App() = default;

std::shared_ptr<App> App::create(const std::filesystem::path& data, Listener listener,
                                 std::optional<Updater> updater, std::string version) {
    std::filesystem::create_directories(data);
    storage::remove_legacy_audio(data);
    auto state = std::make_shared<State>();
    state->listener = std::move(listener);
    state->store = std::make_unique<storage::Store>(data / "history.sqlite");
    state->api_key = data / "api-key";
    state->s1 = s1::Engine::create(data / "s1");
    state->s1->set_unload(unload(state->store->settings()));
    state->s1->on_status([weak = std::weak_ptr<State>(state)](const s1::Status& status) {
        if (auto state = weak.lock(); state && state->listener.s1) state->listener.s1(status);
    });
    state->updater = std::move(updater);
    state->update = {state->updater ? "idle" : "unsupported", std::move(version), std::nullopt,
                     std::nullopt};
    std::weak_ptr<State> weak = state;
    state->hotkeys = hotkey::Service::create([weak](const hotkey::Event& event) {
        auto state = weak.lock();
        if (!state) return;
        if (std::holds_alternative<hotkey::Activate>(event)) {
            spawn([state] { state->toggle(true); });
        } else if (std::holds_alternative<hotkey::Dismiss>(event)) {
            spawn([state] { state->cancel(); });
        } else if (auto* failure = std::get_if<hotkey::Failure>(&event)) {
            std::lock_guard lock(state->session_mutex);
            state->session.view.error = failure->message;
            state->emit(state->session);
        } else if (state->listener.capture) {
            state->listener.capture(event);
        }
    });
    spawn([state] {
        std::lock_guard settings_lock(state->settings_mutex);
        auto shortcut = state->settings().shortcut;
        try {
            state->register_shortcut(shortcut);
        } catch (const std::exception& e) {
            std::lock_guard lock(state->session_mutex);
            state->session.view.error = e.what();
            state->emit(state->session);
        }
    });
#ifdef NDEBUG
    if (state->updater) {
        spawn([weak] {
            std::this_thread::sleep_for(std::chrono::seconds(10));
            for (;;) {
                auto state = weak.lock();
                if (!state) return;
                try {
                    state->check_update(false);
                } catch (const std::exception&) {
                }
                state.reset();
                std::this_thread::sleep_for(std::chrono::hours(6));
            }
        });
    }
#endif
    return std::shared_ptr<App>(new App(state));
}

Bootstrap App::bootstrap() {
    Bootstrap bootstrap;
    {
        std::lock_guard lock(state_->store_mutex);
        bootstrap.entries = state_->store->list();
        bootstrap.settings = state_->store->settings();
    }
    if (hotkey::platform() == "linux-evdev") bootstrap.settings.shortcut_label.reset();
    try {
        bootstrap.microphones = audio::devices();
    } catch (const std::exception&) {
    }
    auto has = [](const std::filesystem::path& path) {
        try {
            credentials::read(path);
            return true;
        } catch (const std::exception&) {
            return false;
        }
    };
    bootstrap.has_azure_key = has(state_->azure_key_path());
    bootstrap.has_open_router_key = has(state_->api_key);
    std::lock_guard lock(state_->session_mutex);
    bootstrap.session = state_->session.view;
    return bootstrap;
}

std::vector<storage::Entry> App::history() {
    std::lock_guard lock(state_->store_mutex);
    return state_->store->list();
}

void App::copy_entry(const std::string& id) {
    std::string text;
    {
        std::lock_guard lock(state_->store_mutex);
        bool found = false;
        for (auto& entry : state_->store->list())
            if (entry.id == id) {
                text = std::move(entry.text);
                found = true;
                break;
            }
        if (!found) throw Error("Transcript unavailable");
    }
    insertion::copy(text);
}

void App::edit_entry(const std::string& id, const std::string& text) {
    if (text.size() > 4 * 1024 * 1024) throw Error("Transcript too large");
    std::lock_guard lock(state_->store_mutex);
    state_->store->edit(id, text);
}

void App::delete_entry(const std::string& id) {
    {
        std::lock_guard lock(state_->session_mutex);
        if (state_->session.id == id && busy(state_->session.view.phase))
            throw Error("Transcription in progress");
    }
    std::lock_guard lock(state_->store_mutex);
    state_->store->remove(id);
}

storage::Settings App::save_settings(storage::Settings settings, std::optional<std::string> key,
                                     std::optional<std::string> azure_key) {
    settings.cleanup_model = trim(settings.cleanup_model);
    if (settings.cleanup_model.empty()) {
        if (settings.cleanup_engine == provider::CleanupEngine::OpenRouter)
            throw Error("Cleanup model required");
        settings.cleanup_model = provider::DEFAULT_CLEANUP_MODEL;
    }
    std::lock_guard guard(state_->settings_mutex);
    auto old = state_->settings();
    state_->hotkeys->validate(settings.shortcut);
    // A key Azure does not accept is never saved.
    if (azure_key) {
        auto trimmed = trim(*azure_key);
        auto region = azure::region(trimmed);
        std::lock_guard lock(state_->azure_region_mutex);
        state_->azure_region = {trimmed, region};
    }
    if (key) credentials::save(state_->api_key, *key);
    if (azure_key) credentials::save(state_->azure_key_path(), *azure_key);
    state_->register_shortcut(settings.shortcut);
    settings.shortcut_label.reset();
    try {
        std::lock_guard lock(state_->store_mutex);
        state_->store->save_settings(settings);
    } catch (const std::exception& e) {
        std::string message = e.what();
        if (old.shortcut != settings.shortcut) {
            try {
                state_->register_shortcut(old.shortcut);
            } catch (const std::exception& rollback) {
                throw Error(message + "; shortcut restore failed: " + rollback.what());
            }
        }
        throw Error(message);
    }
    state_->s1->set_unload(unload(settings));
    if (settings.cleanup_engine != provider::CleanupEngine::S1Mini) state_->s1->stop();
    return settings;
}

std::vector<provider::CleanupModel> App::cleanup_models() {
    return state_->openrouter.cleanup_models();
}

s1::Status App::s1_status() {
    return state_->s1->status();
}

void App::download_s1(s1::Part part) {
    // Progress and failures reach Settings as status changes.
    spawn([engine = state_->s1, part] { engine->download(part); });
}

void App::cancel_s1_download(s1::Part part) {
    state_->s1->cancel_download(part);
}

CaptureSession App::begin_shortcut_capture() {
    {
        std::lock_guard lock(state_->session_mutex);
        if (busy(state_->session.view.phase))
            throw Error("Stop recording before changing the shortcut");
    }
    auto token = state_->hotkeys->begin_capture();
    spawn([service = state_->hotkeys, token] {
        std::this_thread::sleep_for(std::chrono::seconds(15));
        service->cancel_capture(token);
    });
    return {token, hotkey::platform()};
}

std::vector<hotkey::Event> App::capture_shortcut_key(std::uint64_t token, const hotkey::Key& key,
                                                     bool down) {
    return state_->hotkeys->capture_key(token, key, down);
}

void App::window_shortcut_key(const hotkey::Key& key, bool down) {
    state_->hotkeys->window_key(key, down);
}

hotkey::Chord App::shortcut_binding() {
    return state_->hotkeys->binding();
}

void App::end_shortcut_capture(std::optional<std::uint64_t> token) {
    state_->hotkeys->cancel_capture(token);
}

bool App::grant_keyboard_access() {
#ifdef __linux__
    bool granted = linux_input::grant_access();
    if (granted) {
        std::lock_guard guard(state_->settings_mutex);
        state_->register_shortcut(state_->settings().shortcut);
        std::lock_guard lock(state_->session_mutex);
        if (state_->session.view.error == linux_input::ACCESS_REQUIRED) {
            state_->session.view.error.reset();
            state_->emit(state_->session);
        }
    }
    return granted;
#else
    throw Error("Unsupported");
#endif
}

void App::open_history() {
    {
        std::lock_guard lock(state_->session_mutex);
        state_->session.widget_requested = false;
    }
    if (state_->listener.open_history) state_->listener.open_history();
    if (state_->listener.hide_widget) state_->listener.hide_widget();
}

void App::toggle(bool automatic) {
    state_->toggle(automatic);
}

void App::cancel() {
    state_->cancel();
}

void App::import_audio(const std::filesystem::path& path) {
    auto format = to_lower_ascii(path.extension().string());
    if (format != ".wav" && format != ".mp3" && format != ".flac")
        throw Error("Audio format unsupported");
    auto key = state_->azure_key();
    state_->cleanup_config(state_->settings());
    auto audio = audio::decode(path);
    std::string id;
    std::stop_token cancel;
    {
        std::lock_guard lock(state_->session_mutex);
        if (busy(state_->session.view.phase)) {
            audio->close();
            throw Error("Recording in progress");
        }
        id = uuid_v4();
        state_->session = SessionState();
        state_->session.id = id;
        state_->session.view.phase = Phase::Transcribing;
        cancel = state_->session.cancel.get_token();
        state_->emit(state_->session);
    }
    spawn([state = state_, id, key, audio, cancel] {
        state->transcribe(id, key, audio, false, cancel);
    });
}

std::optional<Session> App::widget_to_show(bool active_only) {
    // Recheck on the UI thread so a queued show cannot undo cancellation.
    std::lock_guard lock(state_->session_mutex);
    const auto& s = state_->session;
    if (!s.widget_requested) return std::nullopt;
    if (!busy(s.view.phase) &&
        (active_only || (s.view.phase != Phase::Error && s.view.phase != Phase::Done)))
        return std::nullopt;
    return s.view;
}

void App::widget_shown(std::optional<std::string> error) {
    std::lock_guard lock(state_->session_mutex);
    auto& view = state_->session.view;
    if (error) {
        auto message = "Recording widget: " + *error;
        std::cerr << message << '\n';
        if (!view.error) {
            view.error = message;
            state_->emit(state_->session);
        }
    } else if (view.error && view.error->starts_with("Recording widget: ")) {
        view.error.reset();
        state_->emit(state_->session);
    }
}

UpdateView App::update_state() {
    std::lock_guard lock(state_->update_mutex);
    return state_->update;
}

UpdateView App::check_update() {
    return state_->check_update(true);
}

void App::install_update() {
    static constexpr const char* BUSY = "Stop recording before updating";
    auto recording = [&] {
        std::lock_guard lock(state_->session_mutex);
        return busy(state_->session.view.phase);
    };
    if (recording()) throw Error(BUSY);
    {
        std::lock_guard lock(state_->update_mutex);
        if (state_->update.phase != "available" || !state_->updater)
            throw Error("Update unavailable");
        state_->update.phase = "installing";
        if (state_->listener.update) state_->listener.update(state_->update);
    }
    try {
        // A recording started meanwhile would be lost on restart.
        if (recording()) throw Error(BUSY);
        state_->updater->install();
    } catch (const std::exception& e) {
        std::string message = e.what();
        state_->change_update([&](UpdateView& view) {
            if (message == BUSY) {
                view.phase = "available";
            } else {
                view.phase = "error";
                view.error = message;
            }
        });
        throw Error(message);
    }
}

void App::shutdown() {
    state_->s1->stop();
}

std::filesystem::path data_dir() {
    static constexpr const char* IDENTIFIER = "app.transcribe.desktop";
#ifdef _WIN32
    PWSTR roaming = nullptr;
    if (FAILED(SHGetKnownFolderPath(FOLDERID_RoamingAppData, 0, nullptr, &roaming)))
        throw Error("App data folder unavailable");
    std::filesystem::path path(roaming);
    CoTaskMemFree(roaming);
    return path / IDENTIFIER;
#else
    if (const char* data = std::getenv("XDG_DATA_HOME"); data && *data == '/')
        return std::filesystem::path(data) / IDENTIFIER;
    const char* home = std::getenv("HOME");
    if (!home || !*home) throw Error("App data folder unavailable");
    return std::filesystem::path(home) / ".local" / "share" / IDENTIFIER;
#endif
}

} // namespace transcribe::app
