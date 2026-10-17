#include "pill.hpp"

#include "shell.hpp"

#include <adwaita.h>
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
    : app_(std::move(app)), session_box_(Gtk::Orientation::HORIZONTAL, 8),
      error_box_(Gtk::Orientation::HORIZONTAL, 8), card_box_(Gtk::Orientation::HORIZONTAL, 6) {
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
        gtk_layer_set_margin(window, GTK_LAYER_SHELL_EDGE_BOTTOM, 16);
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

    pill_.add_css_class("card");
    pill_.add_css_class("transcribe-pill");
    pill_.add_css_class("closing");
    pill_.set_halign(Gtk::Align::CENTER);
    pill_.set_valign(Gtk::Align::END);
    // Room for the card's shadow.
    pill_.set_margin(12);
    pill_.set_overflow(Gtk::Overflow::HIDDEN);
    for (auto* box : {&session_box_, &error_box_, &card_box_}) {
        box->set_hexpand(true);
        pill_.append(*box);
    }

    // While recording: the microphone's level, the time, Stop and Cancel. Then a spinner with
    // what is happening, and a check once the text is in.
    session_box_.set_margin_start(6);
    microphone_ = shell::icon("audio-input-microphone-symbolic");
    spinner_ = Gtk::manage(Glib::wrap(adw_spinner_new()));
    done_ = shell::icon("object-select-symbolic");
    done_->add_css_class("success");
    // A plain meter: the default offsets would colour quiet speech as a warning.
    for (const char* offset :
         {GTK_LEVEL_BAR_OFFSET_LOW, GTK_LEVEL_BAR_OFFSET_HIGH, GTK_LEVEL_BAR_OFFSET_FULL})
        level_bar_.remove_offset_value(offset);
    level_bar_.set_size_request(72, -1);
    level_bar_.set_valign(Gtk::Align::CENTER);
    status_.add_css_class("numeric");
    words_.set_ellipsize(Pango::EllipsizeMode::START);
    words_.set_width_chars(32);
    words_.set_max_width_chars(32);
    words_.set_hexpand(true);
    words_.set_xalign(0);
    icon_button(stop_, "media-playback-stop-symbolic", "Stop");
    stop_.add_css_class("circular");
    stop_.add_css_class("destructive-action");
    stop_.signal_clicked().connect([this] { command([app = app_] { app->toggle(false); }); });
    icon_button(cancel_, "window-close-symbolic", "Cancel");
    cancel_.add_css_class("circular");
    cancel_.add_css_class("flat");
    cancel_.signal_clicked().connect([this] { command([app = app_] { app->cancel(); }); });
    for (auto* widget : std::initializer_list<Gtk::Widget*>{
             microphone_, spinner_, done_, &level_bar_, &status_, &words_, &stop_, &cancel_})
        session_box_.append(*widget);

    error_box_.set_margin_start(6);
    auto* alert = shell::icon("dialog-error-symbolic");
    alert->add_css_class("error");
    shell::accessible_label(alert->gobj(), "Error");
    failure_.set_ellipsize(Pango::EllipsizeMode::END);
    failure_.set_max_width_chars(40);
    failure_.set_hexpand(true);
    failure_.set_xalign(0);
    history_.set_label("History");
    history_.set_valign(Gtk::Align::CENTER);
    history_.signal_clicked().connect([this] { command([app = app_] { app->open_history(); }); });
    icon_button(dismiss_error_, "window-close-symbolic", "Dismiss");
    dismiss_error_.add_css_class("circular");
    dismiss_error_.add_css_class("flat");
    dismiss_error_.signal_clicked().connect([this] { command([app = app_] { app->cancel(); }); });
    error_box_.append(*alert);
    error_box_.append(failure_);
    error_box_.append(history_);
    error_box_.append(dismiss_error_);

    // A transcript that could not be pasted is offered for dragging instead.
    auto* grip = shell::icon("list-drag-handle-symbolic");
    grip->add_css_class("dim-label");
    transcript_.set_wrap(true);
    transcript_.set_wrap_mode(Pango::WrapMode::WORD_CHAR);
    transcript_.set_lines(2);
    transcript_.set_ellipsize(Pango::EllipsizeMode::END);
    transcript_.set_width_chars(36);
    transcript_.set_max_width_chars(36);
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
    dismiss_card_.add_css_class("circular");
    dismiss_card_.add_css_class("flat");
    dismiss_card_.signal_clicked().connect([this] { command([app = app_] { app->cancel(); }); });
    card_box_.append(*draggable);
    card_box_.append(dismiss_card_);

    level_bar_.add_tick_callback(sigc::mem_fun(*this, &Pill::tick));
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

    if (mode == WidgetMode::Error) {
        failure_.set_text(session.error.value_or("Error"));
        failure_.set_tooltip_text(session.error.value_or(""));
    } else if (mode == WidgetMode::Card) {
        transcript_.set_text(*session.transcript);
        transcript_.set_tooltip_text(session.error.value_or(""));
    } else {
        bool recording = session.phase == Phase::Recording;
        bool done = finished(session);
        auto words = spoken(session.words);
        microphone_->set_visible(recording);
        level_bar_.set_visible(recording);
        spinner_->set_visible(!recording && !done);
        done_->set_visible(done);
        words_.set_visible(words.has_value());
        if (words)
            words_.set_markup(Glib::Markup::escape_text(words->text) + "<span alpha=\"55%\">" +
                              Glib::Markup::escape_text(words->interim) + "</span>");
        status_.set_visible(!words);
        if (!recording) status_.set_text(status_label(session).value_or("Starting"));
        stop_.set_visible(recording);
        cancel_.set_visible(!done);
        if (recording && !clock_timer_.connected()) {
            auto update = [this] {
                status_.set_text(transcribe::app::clock(shown_.started_at, transcribe::now_ms()));
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
    int width = gdk_surface_get_width(surface);
    int height = gdk_surface_get_height(surface);
    XMoveWindow(gdk_x11_display_get_xdisplay(display), gdk_x11_surface_get_xid(surface),
                (area.x + (area.width - width) / 2) * scale,
                (area.y + area.height - 64 - height) * scale);
}

bool Pill::tick(const Glib::RefPtr<Gdk::FrameClock>& clock) {
    auto now = clock->get_frame_time();
    double dt = last_frame_ ? std::min(0.1, static_cast<double>(now - *last_frame_) / 1e6) : 0;
    last_frame_ = now;
    bool listening = !closing_ && shown_.phase == Phase::Recording;
    // Every recording learns its room afresh.
    if (listening && !listening_) loudness_ = Loudness();
    listening_ = listening;
    level_bar_.set_value(listening ? loudness_(level, dt) : 0);
    return true;
}
