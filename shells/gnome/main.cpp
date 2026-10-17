#include "pill.hpp"
#include "shell.hpp"
#include "window.hpp"

#include <ayatana-appindicator.h>

#include <csignal>
#include <filesystem>

using namespace transcribe::app;

namespace {
// These live until the process exits: core threads may still report to them.
Window* window = nullptr;
Pill* pill = nullptr;
std::shared_ptr<App>* app = nullptr;
} // namespace

int main(int argc, char** argv) {
    startup();
    // A clipboard reader that goes away mid-transfer must not end the app.
    std::signal(SIGPIPE, SIG_IGN);
    auto application = Gtk::Application::create("app.transcribe.desktop");

    application->signal_startup().connect([&] {
        adw_init();
        auto icons = std::filesystem::read_symlink("/proc/self/exe").parent_path() / "icons";
        Gtk::Window::set_default_icon_name("app.transcribe.desktop");
        window = new Window(application);

        Listener listener;
        listener.session = [&](const Session& session) {
            shell::ui([&, session] {
                // Levels only arrive while recording; an old one must not linger.
                if (session.phase != Phase::Recording) pill->level = 0;
                window->session(session);
                pill->session(session);
            });
        };
        listener.level = [&](float level) { pill->level = level; };
        listener.history = [&] { shell::ui([&] { window->refresh(); }); };
        listener.open_history = [&] { shell::ui([&] { window->show_history(); }); };
        listener.s1 = [&](const transcribe::s1::Status& status) {
            shell::ui([&, status] { window->s1(status); });
        };
        listener.capture = [&](const transcribe::hotkey::Event& event) {
            shell::ui([&, event] { window->capture(event); });
        };
        listener.update = [&](const UpdateView& update) {
            shell::ui([&, update] { window->update(update); });
        };
        listener.show_widget = [&] {
            shell::ui([&] {
                auto session = (*app)->widget_to_show(false);
                if (!session) return;
                try {
                    pill->show(*session);
                    (*app)->widget_shown(std::nullopt);
                } catch (const std::exception& e) {
                    (*app)->widget_shown(e.what());
                }
            });
        };
        listener.hide_widget = [&] { shell::ui([&] { pill->hide(); }); };
        listener.main_focused = [&] { return window->focused.load(); };
        app = new std::shared_ptr<App>(App::create(data_dir(), std::move(listener), updater()));
        pill = new Pill(*app);

        // The pill may sit on its own XWayland display.
        auto css = Gtk::CssProvider::create();
        css->load_from_resource("/app/transcribe/desktop/style.css");
        for (const auto& display : Gdk::DisplayManager::get()->list_displays()) {
            Gtk::StyleProvider::add_provider_for_display(display, css,
                                                         GTK_STYLE_PROVIDER_PRIORITY_APPLICATION);
            // Icons beside the binary, for builds that are not installed.
            if (std::filesystem::is_directory(icons))
                Gtk::IconTheme::get_for_display(display)->add_search_path(icons.string());
        }
        window->attach(*app);

        auto actions = Gio::SimpleActionGroup::create();
        actions->add_action("history", [&] { window->show_history(); });
        actions->add_action("quit", [&] { application->quit(); });
        auto menu = Gio::Menu::create();
        menu->append("History", "indicator.history");
        menu->append("Quit", "indicator.quit");
        auto* tray = app_indicator_new("app.transcribe.desktop", "app.transcribe.desktop",
                                       APP_INDICATOR_CATEGORY_APPLICATION_STATUS);
        if (std::filesystem::is_directory(icons))
            app_indicator_set_icon_theme_path(tray, icons.c_str());
        app_indicator_set_title(tray, "Transcribe");
        app_indicator_set_actions(tray, G_SIMPLE_ACTION_GROUP(g_object_ref(actions->gobj())));
        app_indicator_set_menu(tray, G_MENU(g_object_ref(menu->gobj())));
        app_indicator_set_status(tray, APP_INDICATOR_STATUS_ACTIVE);

        // Closing the window keeps the app running in the tray.
        application->hold();
    });
    // A second launch opens History in this instance.
    application->signal_activate().connect([&] { window->show_history(); });
    application->signal_shutdown().connect([&] { (*app)->shutdown(); });
    return application->run(argc, argv);
}
