#pragma once

#include <gtkmm/image.h>

#include <exception>
#include <functional>
#include <string>
#include <thread>
#include <type_traits>
#include <utility>

namespace shell {

// Runs `work` on the GTK main thread; safe to call from any thread.
inline void ui(std::function<void()> work) {
    g_idle_add_full(
        G_PRIORITY_DEFAULT,
        [](gpointer data) {
            (*static_cast<std::function<void()>*>(data))();
            return G_SOURCE_REMOVE;
        },
        new std::function<void()>(std::move(work)),
        [](gpointer data) { delete static_cast<std::function<void()>*>(data); });
}

// Runs a blocking core command away from the UI, then hands its result to `done` (or the
// failure's message to `failed`) on the UI thread.
template <class Work, class Done, class Failed>
void background(Work work, Done done, Failed failed) {
    std::thread([work = std::move(work), done = std::move(done),
                 failed = std::move(failed)]() mutable {
        try {
            if constexpr (std::is_void_v<std::invoke_result_t<Work&>>) {
                work();
                ui([done = std::move(done)]() mutable { done(); });
            } else {
                ui([done = std::move(done), result = work()]() mutable {
                    done(std::move(result));
                });
            }
        } catch (const std::exception& e) {
            ui([failed = std::move(failed), message = std::string(e.what())]() mutable {
                failed(message);
            });
        }
    }).detach();
}

// A themed icon; gtkmm's string constructor reads a file instead.
inline Gtk::Image* icon(const char* name) {
    auto* image = Gtk::make_managed<Gtk::Image>();
    image->set_from_icon_name(name);
    return image;
}

// What a screen reader announces for a widget without text of its own.
inline void accessible_label(gpointer widget, const char* label) {
    gtk_accessible_update_property(GTK_ACCESSIBLE(widget), GTK_ACCESSIBLE_PROPERTY_LABEL, label,
                                   -1);
}

// Connects a GObject signal of a libadwaita object, which gtkmm does not wrap.
template <class... Args, class F> void on(gpointer instance, const char* signal, F handler) {
    g_signal_connect_data(
        instance, signal,
        G_CALLBACK(+[](Args... args, gpointer data) { (*static_cast<F*>(data))(args...); }),
        new F(std::move(handler)), [](gpointer data, GClosure*) { delete static_cast<F*>(data); },
        GConnectFlags{});
}

} // namespace shell
