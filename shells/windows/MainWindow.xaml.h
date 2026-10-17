#pragma once

#include "MainWindow.g.h"

#include "transcribe/app.hpp"

#include <atomic>
#include <functional>
#include <memory>

namespace winrt::Transcribe::implementation {

// The main window: History and Settings.
struct MainWindow : MainWindowT<MainWindow> {
    void InitializeComponent();
    // `quit` ends the app, which lets an installed update apply.
    void attach(std::shared_ptr<transcribe::app::App> app, std::function<void()> quit);

    // Shows the window on History.
    void show_history();
    void session(const transcribe::app::Session& session);
    void refresh();
    void s1(const transcribe::s1::Status& status);
    void capture(const transcribe::hotkey::Event& event);
    void update(const transcribe::app::UpdateView& update);

    // Read from background threads.
    std::atomic<bool> focused = false;
    // Whether an Alt in the shortcut must keep the window menu closed.
    bool holds_alt() const;

  private:
    template <class Work, class Done> void run(Work work, Done done);
    template <class Work> void run(Work work);
    void fail(const std::string& message);
    void show_notice();
    void show_page(bool settings);
    void key(const Microsoft::UI::Xaml::Input::KeyRoutedEventArgs& args, bool down);
    fire_and_forget pick_audio();

    void bootstrapped(transcribe::app::Bootstrap bootstrap);
    void show_entries();
    void select_entry(const std::optional<std::string>& id);
    void save_text();
    void show_counts();
    void load_settings(const transcribe::storage::Settings& settings);
    void show_cleanup();
    void show_efforts();
    void load_models();
    void show_models();
    void show_download(const Microsoft::UI::Xaml::Controls::StackPanel& box,
                       const Microsoft::UI::Xaml::Controls::TextBlock& error,
                       transcribe::s1::Part part, const transcribe::s1::Download& download);
    void show_shortcut();
    void begin_capture();
    void stop_capture();
    void captured(const transcribe::app::ShortcutRecorder::Result& result);
    void shortcut_error(const std::optional<std::string>& error);
    void save();

    std::shared_ptr<transcribe::app::App> app_;
    std::function<void()> quit_;
    bool loaded_ = false;
    bool settings_shown_ = false;

    transcribe::app::Session session_;
    std::vector<transcribe::storage::Entry> entries_;
    // The ids of the rows History lists, in order.
    std::vector<std::string> shown_ids_;
    std::optional<std::string> selected_;
    std::string shown_text_;
    std::optional<std::string> entry_status_;
    bool has_azure_key_ = false;
    bool has_openrouter_key_ = false;
    std::vector<std::string> microphone_names_;
    Microsoft::UI::Dispatching::DispatcherQueueTimer clock_timer_{nullptr};
    Microsoft::UI::Dispatching::DispatcherQueueTimer copied_timer_{nullptr};
    Microsoft::UI::Dispatching::DispatcherQueueTimer saved_timer_{nullptr};

    // Errors from commands, and the session's own error, share one notice.
    std::optional<std::string> notice_message_;
    bool notice_from_session_ = false;
    std::optional<std::string> command_error_;

    // The settings as edited, saved by Save.
    transcribe::storage::Settings draft_;
    std::vector<transcribe::provider::CleanupModel> models_;
    std::vector<transcribe::provider::ReasoningEffort> efforts_;
    bool loading_models_ = false;
    bool models_failed_ = false;
    bool saving_ = false;
    bool filling_ = false;
    transcribe::app::ShortcutRecorder recorder_;
    Microsoft::UI::Dispatching::DispatcherQueueTimer capture_timer_{nullptr};
    transcribe::app::WindowShortcut window_shortcut_;
    transcribe::hotkey::Chord binding_;

    transcribe::app::UpdateView update_;
};

} // namespace winrt::Transcribe::implementation

namespace winrt::Transcribe::factory_implementation {

struct MainWindow : MainWindowT<MainWindow, implementation::MainWindow> {};

} // namespace winrt::Transcribe::factory_implementation
