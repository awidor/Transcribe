#pragma once

#include "transcribe/app.hpp"

#include <adwaita.h>
#include <gtkmm.h>

#include <atomic>
#include <memory>

// The main window: History and Settings.
class Window {
  public:
    explicit Window(const Glib::RefPtr<Gtk::Application>& application);
    void attach(std::shared_ptr<transcribe::app::App> app);

    // Shows the window on History.
    void show_history();
    void session(const transcribe::app::Session& session);
    void refresh();
    void s1(const transcribe::s1::Status& status);
    void capture(const transcribe::hotkey::Event& event);
    void update(const transcribe::app::UpdateView& update);

    // Read from background threads.
    std::atomic<bool> focused = false;

  private:
    template <class T> T* widget(const char* id) { return builder_->get_widget<T>(id); }
    GObject* object(const char* id) { return gtk_builder_get_object(builder_->gobj(), id); }
    template <class Work, class Done> void run(Work work, Done done);
    template <class Work> void run(Work work);
    void fail(const std::string& message);
    void show_toast();

    void bootstrapped(transcribe::app::Bootstrap bootstrap);
    void show_entries();
    void select(const std::optional<std::string>& id);
    void save_text();
    void show_counts();
    void load_settings(const transcribe::storage::Settings& settings);
    void show_cleanup();
    void show_efforts();
    void load_models();
    void show_models();
    void show_download(AdwActionRow* row, Gtk::Box& box, transcribe::s1::Part part,
                       const transcribe::s1::Download& download);
    void show_shortcut();
    void begin_capture();
    void stop_capture();
    void captured(const transcribe::app::ShortcutRecorder::Result& result);
    void shortcut_error(const std::optional<std::string>& error);
    void grant(std::function<void(std::optional<std::string>)> done);
    void save();

    Glib::RefPtr<Gtk::Builder> builder_;
    Gtk::ApplicationWindow* window_;
    std::shared_ptr<transcribe::app::App> app_;
    bool loaded_ = false;

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
    sigc::connection clock_timer_, progress_timer_;

    // Errors from commands, and the session's own error, share one toast.
    AdwToast* toast_ = nullptr;
    std::optional<std::string> toast_message_;
    std::optional<std::string> command_error_;

    // The settings as edited, saved by Save.
    transcribe::storage::Settings draft_;
    std::vector<transcribe::provider::CleanupModel> models_;
    std::vector<transcribe::provider::ReasoningEffort> efforts_;
    bool loading_models_ = false;
    bool models_failed_ = false;
    bool saving_ = false;
    bool filling_ = false;
    Gtk::Box s1_engine_box_, s1_model_box_;
    transcribe::app::ShortcutRecorder recorder_;
    sigc::connection capture_timer_;

    transcribe::app::UpdateView update_;
};
