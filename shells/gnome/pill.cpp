#include "pill.hpp"

#include "shell.hpp"

#include <gdk/wayland/gdkwayland.h>
#include <gdk/x11/gdkx.h>
#include <gtk4-layer-shell.h>

#include <cstdlib>
#include <iostream>

using namespace transcribe::app;

namespace {

void icon_button(Gtk::Button& button, const char* icon, const char* label) {
    button.set_icon_name(icon);
    button.set_tooltip_text(label);
    shell::accessible_label(button.gobj(), label);
    button.set_valign(Gtk::Align::CENTER);
}

// Commands from the pill report nothing back: the session they change is what it shows.
template <class Work> void command(Work work) {
    shell::background(
        std::move(work), [] {}, [](const std::string& message) { std::cerr << message << '\n'; });
}

} // namespace

Pill::Pill(std::shared_ptr<App> app)
    : app_(std::move(app)), session_box_(Gtk::Orientation::HORIZONTAL, 6),
      error_box_(Gtk::Orientation::HORIZONTAL, 8), card_box_(Gtk::Orientation::HORIZONTAL, 4) {
    auto* display = gdk_display_get_default();
    bool wayland = GDK_IS_WAYLAND_DISPLAY(display);
    if (wayland && gtk_layer_is_supported()) {
        placement_ = Placement::Layer;
    } else if (!wayland) {
        placement_ = Placement::X11;
    } else {
        // Wayland lets only layer-shell surfaces choose their position, so without it
        // (GNOME) the pill opens on XWayland, which honours its position and stacking.
        placement_ = Placement::Plain;
        const char* name = std::getenv("DISPLAY");
        if (auto* x11 = name && *name ? gdk_display_open(name) : nullptr) {
            if (GDK_IS_X11_DISPLAY(x11)) {
                gtk_window_set_display(window_.gobj(), x11);
                placement_ = Placement::X11;
            } else {
                gdk_display_close(x11);
            }
        }
    }

    window_.set_title("Transcribe");
    window_.set_decorated(false);
    window_.set_resizable(false);
    window_.add_css_class("transcribe-widget");
    window_.set_child(pill_);
    if (placement_ == Placement::Layer) {
        auto* window = window_.gobj();
        gtk_layer_init_for_window(window);
        gtk_layer_set_namespace(window, "transcribe-widget");
        gtk_layer_set_layer(window, GTK_LAYER_SHELL_LAYER_OVERLAY);
        gtk_layer_set_anchor(window, GTK_LAYER_SHELL_EDGE_BOTTOM, TRUE);
        gtk_layer_set_margin(window, GTK_LAYER_SHELL_EDGE_BOTTOM, 24);
        gtk_layer_set_keyboard_mode(window, GTK_LAYER_SHELL_KEYBOARD_MODE_NONE);
    } else if (placement_ == Placement::X11) {
        window_.signal_realize().connect([this] {
            // Unmanaged by the window manager: never focused or listed, and above windows.
            auto* surface = window_.get_surface()->gobj();
            XSetWindowAttributes attributes{};
            attributes.override_redirect = True;
            XChangeWindowAttributes(gdk_x11_display_get_xdisplay(gdk_surface_get_display(surface)),
                                    gdk_x11_surface_get_xid(surface), CWOverrideRedirect,
                                    &attributes);
            window_.get_surface()->signal_layout().connect([this](int, int) { place(); });
        });
    }

    pill_.add_css_class("transcribe-pill");
    pill_.add_css_class("closing");
    pill_.set_halign(Gtk::Align::CENTER);
    pill_.set_valign(Gtk::Align::END);
    pill_.set_margin(4);
    pill_.set_overflow(Gtk::Overflow::HIDDEN);
    for (auto* box : {&session_box_, &error_box_, &card_box_}) {
        box->set_hexpand(true);
        pill_.append(*box);
    }

    // One layout from the first word to the finished paste: the bars follow the voice, then
    // settle into a wave while the transcript is prepared, and the stop button turns into a
    // spinner and then a check.
    session_box_.set_margin_start(6);
    session_box_.set_margin_end(6);
    icon_button(cancel_, "window-close-symbolic", "Cancel");
    cancel_.signal_clicked().connect([this] { command([app = app_] { app->cancel(); }); });
    wave_.set_content_width(53);
    wave_.set_content_height(24);
    wave_.set_valign(Gtk::Align::CENTER);
    wave_.set_draw_func([this](const Cairo::RefPtr<Cairo::Context>& cr, int width, int height) {
        cr->set_source_rgb(1, 1, 1);
        cr->set_line_width(2.5);
        cr->set_line_cap(Cairo::Context::LineCap::ROUND);
        double x = (width - 52.5) / 2 + 1.25;
        for (double bar : heights_) {
            double half = std::max(0.0, bar / 2 - 1.25);
            cr->move_to(x, height / 2.0 - half);
            cr->line_to(x, height / 2.0 + half);
            x += 5;
        }
        cr->stroke();
    });
    words_.add_css_class("words");
    words_.set_ellipsize(Pango::EllipsizeMode::START);
    words_.set_max_width_chars(1);
    words_.set_hexpand(true);
    words_.set_xalign(0);
    clock_.add_css_class("clock");
    action_icon_.add(*shell::icon("media-playback-stop-symbolic"), "stop");
    action_icon_.add(*Gtk::make_managed<Gtk::Spinner>(), "busy");
    action_icon_.add(*shell::icon("object-select-symbolic"), "done");
    static_cast<Gtk::Spinner*>(action_icon_.get_child_by_name("busy"))->start();
    action_.set_child(action_icon_);
    action_.add_css_class("action");
    action_.set_valign(Gtk::Align::CENTER);
    action_.signal_clicked().connect([this] { command([app = app_] { app->toggle(false); }); });
    for (auto* widget :
         std::initializer_list<Gtk::Widget*>{&cancel_, &wave_, &words_, &clock_, &action_})
        session_box_.append(*widget);

    error_box_.set_margin_start(14);
    error_box_.set_margin_end(6);
    auto* failure = Gtk::make_managed<Gtk::Box>(Gtk::Orientation::HORIZONTAL, 6);
    auto* alert = shell::icon("dialog-error-symbolic");
    alert->add_css_class("failure-icon");
    failure_.set_text("Error");
    failure_.add_css_class("failure");
    failure->append(*alert);
    failure->append(failure_);
    failure->set_hexpand(true);
    history_.set_label("History");
    history_.add_css_class("history");
    history_.set_valign(Gtk::Align::CENTER);
    history_.signal_clicked().connect([this] { command([app = app_] { app->open_history(); }); });
    icon_button(dismiss_error_, "window-close-symbolic", "Dismiss");
    dismiss_error_.signal_clicked().connect([this] { command([app = app_] { app->cancel(); }); });
    error_box_.append(*failure);
    error_box_.append(history_);
    error_box_.append(dismiss_error_);

    // A transcript that could not be pasted is offered for dragging instead.
    card_box_.set_margin_start(4);
    card_box_.set_margin_end(8);
    auto* grip = shell::icon("list-drag-handle-symbolic");
    grip->add_css_class("grip");
    transcript_.add_css_class("transcript");
    transcript_.set_wrap(true);
    transcript_.set_wrap_mode(Pango::WrapMode::WORD_CHAR);
    transcript_.set_lines(2);
    transcript_.set_ellipsize(Pango::EllipsizeMode::END);
    transcript_.set_max_width_chars(1);
    transcript_.set_hexpand(true);
    transcript_.set_xalign(0);
    auto* draggable = Gtk::make_managed<Gtk::Box>(Gtk::Orientation::HORIZONTAL, 4);
    draggable->set_hexpand(true);
    draggable->set_cursor("grab");
    draggable->append(*grip);
    draggable->append(transcript_);
    auto drag = Gtk::DragSource::create();
    drag->set_actions(Gdk::DragAction::COPY);
    drag->signal_prepare().connect(
        [this](double, double) {
            Glib::Value<Glib::ustring> text;
            text.init(text.value_type());
            text.set(shown_.transcript.value_or(""));
            return Gdk::ContentProvider::create(text);
        },
        false);
    drag->signal_drag_begin().connect([this, drag = drag.get()](const Glib::RefPtr<Gdk::Drag>&) {
        dropped_ = true;
        // The whole card stays legible over any destination.
        drag->set_icon(Gtk::WidgetPaintable::create(pill_), 0, 0);
    });
    drag->signal_drag_cancel().connect(
        [this](const Glib::RefPtr<Gdk::Drag>&, Gdk::DragCancelReason) {
            dropped_ = false;
            return false;
        },
        false);
    drag->signal_drag_end().connect([this](const Glib::RefPtr<Gdk::Drag>&, bool) {
        if (dropped_) command([app = app_] { app->cancel(); });
    });
    draggable->add_controller(drag);
    icon_button(dismiss_card_, "window-close-symbolic", "Dismiss");
    dismiss_card_.signal_clicked().connect([this] { command([app = app_] { app->cancel(); }); });
    card_box_.append(*draggable);
    card_box_.append(dismiss_card_);

    heights_.fill(4.9);
    wave_.add_tick_callback(sigc::mem_fun(*this, &Pill::tick));
}

void Pill::show(const Session& session) {
    if (placement_ == Placement::Layer) {
        // Wayland does not always expose a primary output. Pin the widget to the first output
        // instead of following focus.
        auto monitors = window_.get_display()->get_monitors();
        auto monitor = std::dynamic_pointer_cast<Gdk::Monitor>(monitors->get_object(0));
        if (!monitor) throw transcribe::Error("Widget monitor unavailable");
        gtk_layer_set_monitor(window_.gobj(), monitor->gobj());
    }
    if (closing_) {
        closing_ = false;
        pill_.remove_css_class("closing");
    }
    render(session);
    window_.set_visible(true);
    place();
}

void Pill::session(const Session& session) {
    if (!window_.get_visible()) return;
    if (session.phase == Phase::Idle) {
        // The pill keeps showing its last session while it closes.
        closing_ = true;
        pill_.add_css_class("closing");
        clock_timer_.disconnect();
        return;
    }
    if (closing_) {
        closing_ = false;
        pill_.remove_css_class("closing");
    }
    render(session);
}

void Pill::hide() {
    window_.set_visible(false);
    // The next session opens afresh, replaying the opening animation.
    closing_ = true;
    pill_.add_css_class("closing");
    clock_timer_.disconnect();
    last_frame_.reset();
}

void Pill::render(const Session& session) {
    shown_ = session;
    auto mode = widget_mode(session);
    session_box_.set_visible(mode == WidgetMode::Session);
    error_box_.set_visible(mode == WidgetMode::Error);
    card_box_.set_visible(mode == WidgetMode::Card);
    auto size = widget_size(session);
    pill_.set_size_request(size.width - 8, size.height - 8);
    if (mode == WidgetMode::Card)
        pill_.add_css_class("card");
    else
        pill_.remove_css_class("card");

    if (mode == WidgetMode::Error) {
        failure_.set_tooltip_text(session.error.value_or("Error"));
        shell::accessible_label(failure_.gobj(),
                                (session.error ? "Error. " + *session.error : "Error").c_str());
    } else if (mode == WidgetMode::Card) {
        transcript_.set_text(*session.transcript);
        transcript_.set_tooltip_text(session.error.value_or(""));
    } else {
        bool recording = session.phase == Phase::Recording;
        bool done = finished(session);
        auto words = spoken(session.words);
        cancel_.set_opacity(done ? 0 : 1);
        cancel_.set_sensitive(!done);
        words_.set_visible(words.has_value());
        if (words)
            words_.set_markup(Glib::Markup::escape_text(words->text) +
                              "<span foreground=\"#8a8a8a\">" +
                              Glib::Markup::escape_text(words->interim) + "</span>");
        wave_.set_hexpand(!words);
        clock_.set_visible(recording && !words);
        auto status = status_label(session);
        const char* label = recording ? "Stop" : nullptr;
        Glib::ustring name =
            label ? Glib::ustring(label) : Glib::ustring(status.value_or("Starting"));
        action_.set_tooltip_text(name);
        shell::accessible_label(action_.gobj(), name.c_str());
        action_.set_sensitive(recording);
        action_icon_.set_visible_child(done ? "done" : recording ? "stop" : "busy");
        for (const char* state : {"stop", "busy", "done"})
            action_.remove_css_class(state);
        action_.add_css_class(done ? "done" : recording ? "stop" : "busy");
        if (recording && !clock_timer_.connected()) {
            auto update = [this] {
                clock_.set_text(transcribe::app::clock(shown_.started_at, transcribe::now_ms()));
                return true;
            };
            update();
            clock_timer_ = Glib::signal_timeout().connect(update, 250);
        } else if (!recording) {
            clock_timer_.disconnect();
        }
    }
}

void Pill::place() {
    if (placement_ != Placement::X11 || !window_.get_visible()) return;
    auto* surface = window_.get_surface()->gobj();
    auto* display = gdk_surface_get_display(surface);
    GdkMonitor* monitor = gdk_x11_display_get_primary_monitor(display);
    if (!monitor) {
        auto monitors = window_.get_display()->get_monitors();
        auto first = std::dynamic_pointer_cast<Gdk::Monitor>(monitors->get_object(0));
        if (!first) return;
        monitor = first->gobj();
    }
    GdkRectangle area;
    gdk_monitor_get_geometry(monitor, &area);
    int scale = gdk_monitor_get_scale_factor(monitor);
    auto size = widget_size(shown_);
    XMoveWindow(gdk_x11_display_get_xdisplay(display), gdk_x11_surface_get_xid(surface),
                (area.x + (area.width - size.width) / 2) * scale,
                (area.y + area.height - 72 - size.height) * scale);
}

bool Pill::tick(const Glib::RefPtr<Gdk::FrameClock>& clock) {
    auto now = clock->get_frame_time();
    double dt = last_frame_ ? std::min(0.1, static_cast<double>(now - *last_frame_) / 1e6) : 0;
    last_frame_ = now;
    auto motion = closing_ || finished(shown_)       ? Bars::Motion::Resting
                  : shown_.phase == Phase::Recording ? Bars::Motion::Listening
                                                     : Bars::Motion::Working;
    heights_ = bars_.update(level, motion, static_cast<double>(now) / 1e6, dt);
    wave_.queue_draw();
    return true;
}
