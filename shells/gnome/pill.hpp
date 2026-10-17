#pragma once

#include "transcribe/app.hpp"

#include <gtkmm.h>

#include <atomic>
#include <memory>

// The recording widget: a floating bar at the bottom of the screen that never takes focus.
class Pill {
  public:
    explicit Pill(std::shared_ptr<transcribe::app::App> app);
    // Shows what App::widget_to_show decided.
    void show(const transcribe::app::Session& session);
    // Follows the session while shown; an idle session fades the bar out.
    void session(const transcribe::app::Session& session);
    void hide();
    // Written from the audio thread.
    std::atomic<float> level = 0;

  private:
    void render(const transcribe::app::Session& session);
    void place();
    bool tick(const Glib::RefPtr<Gdk::FrameClock>& clock);

    std::shared_ptr<transcribe::app::App> app_;
    enum class Placement { Layer, X11, Plain } placement_;
    Gtk::Window window_;
    Gtk::Box pill_;
    // The last session shown, kept on screen while the pill closes.
    transcribe::app::Session shown_;
    bool closing_ = true;

    Gtk::Box session_box_, error_box_, card_box_;
    Gtk::Button stop_, cancel_, history_, dismiss_error_, dismiss_card_;
    Gtk::Image* microphone_;
    Gtk::Widget* spinner_;
    Gtk::Image* done_;
    Gtk::LevelBar level_bar_;
    Gtk::Label status_, words_, failure_, transcript_;
    sigc::connection clock_timer_;

    transcribe::app::Loudness loudness_;
    bool listening_ = false;
    std::optional<std::int64_t> last_frame_;
    bool dropped_ = false;
};
