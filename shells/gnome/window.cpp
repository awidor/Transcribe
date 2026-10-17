#include "window.hpp"

#include "shell.hpp"
#include "transcribe/linux_input.hpp"

#include <algorithm>
#include <cmath>

using namespace transcribe;
using namespace transcribe::app;
using provider::CleanupEngine;

namespace {

// Combo positions of the cleanup engines: Off, OpenRouter, S1-mini.
constexpr CleanupEngine ENGINES[] = {CleanupEngine::Off, CleanupEngine::OpenRouter,
                                     CleanupEngine::S1Mini};

std::string date(std::int64_t ms) {
    return Glib::DateTime::create_now_local(ms / 1000).format("%b %-d, %H:%M");
}

const char* text_of(GObject* editable) {
    return gtk_editable_get_text(GTK_EDITABLE(editable));
}

void set_strings(GObject* combo, const std::vector<std::string>& items) {
    std::vector<Glib::ustring> strings(items.begin(), items.end());
    auto list = Gtk::StringList::create(strings);
    adw_combo_row_set_model(ADW_COMBO_ROW(combo), G_LIST_MODEL(list->gobj()));
}

guint selected(GObject* combo) {
    return adw_combo_row_get_selected(ADW_COMBO_ROW(combo));
}

void clear(Gtk::Box& box) {
    while (auto* child = box.get_first_child())
        box.remove(*child);
}

} // namespace

Window::Window(const Glib::RefPtr<Gtk::Application>& application)
    : builder_(Gtk::Builder::create_from_resource("/app/transcribe/desktop/window.ui")) {
    window_ = widget<Gtk::ApplicationWindow>("window");
    application->add_window(*window_);
    window_->signal_close_request().connect(
        [this] {
            stop_capture();
            window_->set_visible(false);
            return true;
        },
        false);
    window_->property_is_active().signal_changed().connect([this] {
        focused = window_->is_active();
        if (!focused) stop_capture();
    });
    // Keys being recorded as a shortcut must not also operate the window.
    auto keys = Gtk::EventControllerKey::create();
    keys->set_propagation_phase(Gtk::PropagationPhase::CAPTURE);
    keys->signal_key_pressed().connect(
        [this](guint, guint, Gdk::ModifierType) {
            return recorder_.mode() != ShortcutRecorder::Mode::Idle;
        },
        false);
    window_->add_controller(keys);

    widget<Gtk::Button>("import")->signal_clicked().connect([this] {
        auto filter = Gtk::FileFilter::create();
        filter->set_name("Audio");
        for (const char* suffix : {"wav", "mp3", "flac"})
            filter->add_suffix(suffix);
        auto dialog = Gtk::FileDialog::create();
        dialog->set_default_filter(filter);
        dialog->open(*window_, [this, dialog](const Glib::RefPtr<Gio::AsyncResult>& result) {
            Glib::RefPtr<Gio::File> file;
            try {
                file = dialog->open_finish(result);
            } catch (const Glib::Error&) {
                return;
            }
            run([app = app_, path = std::filesystem::path(file->get_path())] {
                app->import_audio(path);
            });
        });
    });
    widget<Gtk::Button>("record")->signal_clicked().connect(
        [this] { run([app = app_] { app->toggle(false); }); });
    widget<Gtk::Button>("cancel")->signal_clicked().connect(
        [this] { run([app = app_] { app->cancel(); }); });

    // History
    widget<Gtk::SearchEntry>("search")->signal_search_changed().connect([this] { show_entries(); });
    widget<Gtk::ListBox>("entries")->signal_row_selected().connect([this](Gtk::ListBoxRow* row) {
        if (filling_ || !row) return;
        save_text();
        select(shown_ids_.at(static_cast<std::size_t>(row->get_index())));
    });
    widget<Gtk::Button>("empty_action")->signal_clicked().connect([this] {
        if (has_azure_key_)
            widget<Gtk::Button>("import")->activate();
        else
            adw_view_stack_set_visible_child_name(ADW_VIEW_STACK(object("stack")), "settings");
    });
    auto* text = widget<Gtk::TextView>("text");
    text->get_buffer()->signal_changed().connect([this] { show_counts(); });
    auto focus = Gtk::EventControllerFocus::create();
    focus->signal_leave().connect([this] { save_text(); });
    text->add_controller(focus);
    widget<Gtk::Button>("copy")->signal_clicked().connect([this] {
        auto id = *selected_;
        auto value = widget<Gtk::TextView>("text")->get_buffer()->get_text().raw();
        auto it = std::ranges::find(entries_, id, &storage::Entry::id);
        bool edited = it != entries_.end() && it->text != value;
        run(
            [app = app_, id, value, edited] {
                if (edited) app->edit_entry(id, value);
                app->copy_entry(id);
            },
            [this] {
                auto* copy = widget<Gtk::Button>("copy");
                copy->set_icon_name("object-select-symbolic");
                copy->set_tooltip_text("Copied");
                Glib::signal_timeout().connect_once(
                    [copy] {
                        copy->set_icon_name("edit-copy-symbolic");
                        copy->set_tooltip_text("Copy");
                    },
                    1500);
                refresh();
            });
    });
    widget<Gtk::Button>("delete")->signal_clicked().connect([this] {
        run([app = app_, id = *selected_] { app->delete_entry(id); }, [this] { refresh(); });
    });

    // Settings
    shell::on<GObject*, GParamSpec*>(object("microphone"), "notify::selected",
                                     [this](GObject*, GParamSpec*) {
                                         if (filling_) return;
                                         auto index = selected(object("microphone"));
                                         draft_.microphone.reset();
                                         if (index > 0 && index <= microphone_names_.size())
                                             draft_.microphone = microphone_names_[index - 1];
                                     });
    shell::on<GObject*, GParamSpec*>(
        object("engine"), "notify::selected", [this](GObject*, GParamSpec*) {
            if (filling_) return;
            draft_.cleanup_engine = ENGINES[selected(object("engine"))];
            show_cleanup();
        });
    shell::on<GtkEditable*>(object("model"), "changed", [this](GtkEditable*) {
        if (filling_) return;
        std::string model = text_of(object("model"));
        if (model != draft_.cleanup_model) draft_.cleanup_reasoning_effort.reset();
        draft_.cleanup_model = model;
        show_efforts();
    });
    shell::on<GObject*, GParamSpec*>(object("effort"), "notify::selected",
                                     [this](GObject*, GParamSpec*) {
                                         if (filling_) return;
                                         auto index = selected(object("effort"));
                                         draft_.cleanup_reasoning_effort.reset();
                                         if (index > 0 && index <= efforts_.size())
                                             draft_.cleanup_reasoning_effort = efforts_[index - 1];
                                     });
    shell::on<GObject*, GParamSpec*>(
        object("styling"), "notify::selected", [this](GObject*, GParamSpec*) {
            if (filling_) return;
            draft_.cleanup_styling = static_cast<s1::Styling>(selected(object("styling")));
        });
    shell::on<GObject*, GParamSpec*>(
        object("unload"), "notify::selected", [this](GObject*, GParamSpec*) {
            if (filling_) return;
            draft_.cleanup_unload_seconds = unload_choices().at(selected(object("unload"))).seconds;
        });
    std::vector<std::string> stylings, unloads;
    for (int i = 0; i <= static_cast<int>(s1::Styling::Formal); ++i)
        stylings.emplace_back(styling_label(static_cast<s1::Styling>(i)));
    for (const auto& choice : unload_choices())
        unloads.push_back(choice.label);
    set_strings(object("styling"), stylings);
    set_strings(object("unload"), unloads);

    widget<Gtk::Button>("models_retry")->signal_clicked().connect([this] { load_models(); });
    widget<Gtk::SearchEntry>("models_search")->signal_search_changed().connect([this] {
        show_models();
    });
    widget<Gtk::ListBox>("models_list")
        ->signal_row_activated()
        .connect([this](Gtk::ListBoxRow* row) {
            auto search = widget<Gtk::SearchEntry>("models_search")->get_text();
            std::vector<const provider::CleanupModel*> shown;
            for (const auto& model : models_)
                if (matches(model.id + " " + model.name, search.raw())) shown.push_back(&model);
            gtk_editable_set_text(GTK_EDITABLE(object("model")),
                                  shown.at(static_cast<std::size_t>(row->get_index()))->id.c_str());
            widget<Gtk::Popover>("models_popover")->popdown();
        });

    for (auto [id, box] : {std::pair{"s1_engine", &s1_engine_box_}, {"s1_model", &s1_model_box_}}) {
        box->set_spacing(6);
        box->set_valign(Gtk::Align::CENTER);
        adw_action_row_add_suffix(ADW_ACTION_ROW(object(id)), GTK_WIDGET(box->gobj()));
    }

    shell::on<AdwActionRow*>(object("shortcut"), "activated",
                             [this](AdwActionRow*) { begin_capture(); });
    widget<Gtk::Button>("shortcut_cancel")->signal_clicked().connect([this] { stop_capture(); });
    widget<Gtk::Button>("grant")->signal_clicked().connect(
        [this] { grant([this](std::optional<std::string> error) { shortcut_error(error); }); });
    widget<Gtk::Button>("save")->signal_clicked().connect([this] { save(); });

    widget<Gtk::Button>("update_action")->signal_clicked().connect([this] {
        if (update_.phase == "available")
            run([app = app_] { app->install_update(); });
        else
            run([app = app_] { return app->check_update(); },
                [this](UpdateView view) { update(view); });
    });
}

void Window::attach(std::shared_ptr<App> app) {
    app_ = std::move(app);
    run([app = app_] { return app->bootstrap(); },
        [this](Bootstrap bootstrap) { bootstrapped(std::move(bootstrap)); });
    run([app = app_] { return app->update_state(); }, [this](UpdateView view) { update(view); });
    run([app = app_] { return app->s1_status(); }, [this](s1::Status status) { s1(status); });
    load_models();
}

template <class Work, class Done> void Window::run(Work work, Done done) {
    command_error_.reset();
    show_toast();
    shell::background(std::move(work), std::move(done),
                      [this](const std::string& message) { fail(message); });
}

template <class Work> void Window::run(Work work) {
    run(std::move(work), [] {});
}

void Window::fail(const std::string& message) {
    command_error_ = message;
    show_toast();
}

// A failed paste is offered in the widget and kept in history, not reported here.
void Window::show_toast() {
    auto message = command_error_        ? command_error_
                   : session_.transcript ? std::nullopt
                                         : session_.error;
    if (message == toast_message_) return;
    if (auto* old = std::exchange(toast_, nullptr)) adw_toast_dismiss(old);
    toast_message_ = message;
    if (!message) return;
    toast_ = adw_toast_new("");
    adw_toast_set_use_markup(toast_, FALSE);
    adw_toast_set_title(toast_, message->c_str());
    adw_toast_set_timeout(toast_, 0);
    if (*message == linux_input::ACCESS_REQUIRED) {
        adw_toast_set_button_label(toast_, "Grant access");
        shell::on<AdwToast*>(toast_, "button-clicked", [this](AdwToast* toast) {
            if (toast != toast_) return;
            toast_ = nullptr;
            toast_message_.reset();
            grant([this](std::optional<std::string> error) {
                command_error_ = error;
                show_toast();
            });
        });
    }
    bool from_session = !command_error_;
    shell::on<AdwToast*>(toast_, "dismissed", [this, from_session](AdwToast* toast) {
        if (toast != toast_) return;
        toast_ = nullptr;
        toast_message_.reset();
        if (from_session)
            run([app = app_] { app->cancel(); });
        else
            command_error_.reset();
    });
    adw_toast_overlay_add_toast(ADW_TOAST_OVERLAY(object("toasts")), toast_);
}

void Window::show_history() {
    adw_view_stack_set_visible_child_name(ADW_VIEW_STACK(object("stack")), "history");
    window_->present();
}

void Window::bootstrapped(Bootstrap bootstrap) {
    entries_ = std::move(bootstrap.entries);
    has_azure_key_ = bootstrap.has_azure_key;
    has_openrouter_key_ = bootstrap.has_open_router_key;
    microphone_names_ = std::move(bootstrap.microphones);
    load_settings(bootstrap.settings);
    session(bootstrap.session);
    show_entries();
    if (!has_azure_key_ && !loaded_)
        adw_view_stack_set_visible_child_name(ADW_VIEW_STACK(object("stack")), "settings");
    loaded_ = true;
}

void Window::session(const Session& session) {
    session_ = session;
    bool recording = session.phase == Phase::Recording;
    bool busy = working(session.phase);
    bool done = finished(session);
    auto status = status_label(session);

    auto* record = widget<Gtk::Button>("record");
    widget<Gtk::Stack>("record_icon")
        ->set_visible_child(busy && done ? "done"
                            : busy       ? "busy"
                            : recording  ? "stop"
                                         : "record");
    Glib::ustring label = recording ? "Stop" : busy ? status.value_or("") : "Record";
    widget<Gtk::Label>("record_label")->set_text(label);
    shell::accessible_label(record->gobj(), recording ? "Stop recording" : label.c_str());
    record->set_sensitive(!busy && has_azure_key_);
    widget<Gtk::Button>("import")->set_sensitive(!busy && !recording && has_azure_key_);
    widget<Gtk::Button>("cancel")->set_visible(recording || (busy && !done));
    widget<Gtk::Button>("empty_action")->set_sensitive(!has_azure_key_ || (!busy && !recording));

    auto* clock = widget<Gtk::Label>("clock");
    clock->set_visible(recording);
    if (recording && !clock_timer_.connected()) {
        auto tick = [this, clock] {
            clock->set_text(app::clock(session_.started_at, now_ms()));
            return true;
        };
        tick();
        clock_timer_ = Glib::signal_timeout().connect(tick, 250);
    } else if (!recording) {
        clock_timer_.disconnect();
    }

    auto* progress = widget<Gtk::ProgressBar>("progress");
    bool working = processing(session) && !session.error;
    progress->set_visible(working);
    if (working && done) {
        progress_timer_.disconnect();
        progress->set_fraction(1);
    } else if (working && !progress_timer_.connected()) {
        progress_timer_ = Glib::signal_timeout().connect(
            [progress] {
                progress->pulse();
                return true;
            },
            100);
    } else if (!working) {
        progress_timer_.disconnect();
    }

    // History rows still being transcribed show the current stage.
    if (status != entry_status_ &&
        std::ranges::any_of(entries_, [](const auto& e) { return e.status == "transcribing"; }))
        show_entries();
    entry_status_ = status;
    show_toast();
    update(update_);
}

void Window::refresh() {
    shell::background([app = app_] { return app->history(); },
                      [this](std::vector<storage::Entry> entries) {
                          entries_ = std::move(entries);
                          show_entries();
                      },
                      [](const std::string&) {});
}

void Window::show_entries() {
    auto* list = widget<Gtk::ListBox>("entries");
    auto search = widget<Gtk::SearchEntry>("search")->get_text().raw();
    filling_ = true;
    list->remove_all();
    shown_ids_.clear();
    for (const auto& entry : entries_) {
        if (!matches(entry.text, search)) continue;
        shown_ids_.push_back(entry.id);
        auto* box = Gtk::make_managed<Gtk::Box>(Gtk::Orientation::VERTICAL, 2);
        box->set_margin(6);
        auto* top = Gtk::make_managed<Gtk::Box>(Gtk::Orientation::HORIZONTAL, 6);
        auto* time = Gtk::make_managed<Gtk::Label>(date(entry.created_at));
        time->set_hexpand(true);
        time->set_xalign(0);
        time->add_css_class("caption");
        time->add_css_class("dim-label");
        top->append(*time);
        if (entry.status == "failed") {
            auto* dot = shell::icon("dialog-error-symbolic");
            dot->add_css_class("error");
            top->append(*dot);
        } else {
            auto* seconds =
                Gtk::make_managed<Gtk::Label>(std::to_string(std::lround(entry.seconds)) + "s");
            seconds->add_css_class("caption");
            seconds->add_css_class("dim-label");
            seconds->add_css_class("numeric");
            top->append(*seconds);
        }
        auto* preview = Gtk::make_managed<Gtk::Label>(entry_preview(entry, session_));
        preview->set_xalign(0);
        preview->set_wrap(true);
        preview->set_wrap_mode(Pango::WrapMode::WORD_CHAR);
        preview->set_lines(2);
        preview->set_ellipsize(Pango::EllipsizeMode::END);
        preview->set_max_width_chars(1);
        preview->set_hexpand(true);
        box->append(*top);
        box->append(*preview);
        list->append(*box);
    }
    auto* placeholder =
        Gtk::make_managed<Gtk::Label>(search.empty() ? "No transcripts" : "No matches");
    placeholder->add_css_class("dim-label");
    placeholder->set_margin(18);
    list->set_placeholder(*placeholder);
    widget<Gtk::Label>("list_heading")
        ->set_text(search.empty() ? "All transcripts" : "Search results");
    widget<Gtk::Label>("list_count")->set_text(std::to_string(shown_ids_.size()));

    if (!std::ranges::any_of(entries_, [&](const auto& e) { return e.id == selected_; }))
        selected_ = entries_.empty() ? std::nullopt : std::optional(entries_.front().id);
    if (auto it = std::ranges::find(shown_ids_, selected_); it != shown_ids_.end())
        list->select_row(*list->get_row_at_index(static_cast<int>(it - shown_ids_.begin())));
    filling_ = false;
    select(selected_);
}

void Window::select(const std::optional<std::string>& id) {
    auto it = std::ranges::find(entries_, id, &storage::Entry::id);
    if (it == entries_.end()) {
        selected_.reset();
        auto* action = widget<Gtk::Button>("empty_action");
        action->set_label(has_azure_key_ ? "Import audio" : "Open Settings");
        widget<Gtk::Stack>("detail")->set_visible_child("empty");
        return;
    }
    bool switched = selected_ != id || shown_text_ != it->text;
    selected_ = id;
    const auto& entry = *it;
    widget<Gtk::Stack>("detail")->set_visible_child("editor");
    widget<Gtk::Label>("entry_time")->set_text(date(entry.created_at));
    auto* unpasted = widget<Gtk::Label>("not_pasted");
    unpasted->set_visible(not_pasted(entry));
    unpasted->set_tooltip_text(entry.error.value_or(""));
    auto* error = widget<Gtk::Label>("entry_error");
    error->set_visible(entry.error && !entry.error->empty() && !not_pasted(entry));
    error->set_text(entry.error.value_or(""));
    widget<Gtk::Label>("seconds")->set_text(std::to_string(std::lround(entry.seconds)) +
                                            " seconds");
    if (switched) {
        shown_text_ = entry.text;
        widget<Gtk::TextView>("text")->get_buffer()->set_text(entry.text);
    }
    show_counts();
}

void Window::show_counts() {
    auto value = widget<Gtk::TextView>("text")->get_buffer()->get_text().raw();
    widget<Gtk::Label>("words")->set_text(std::to_string(word_count(value)) + " words");
    widget<Gtk::Button>("copy")->set_sensitive(!value.empty());
}

void Window::save_text() {
    if (!selected_) return;
    auto it = std::ranges::find(entries_, selected_, &storage::Entry::id);
    auto value = widget<Gtk::TextView>("text")->get_buffer()->get_text().raw();
    if (it == entries_.end() || it->text == value) return;
    it->text = value;
    shown_text_ = value;
    run([app = app_, id = *selected_, value] { app->edit_entry(id, value); },
        [this] { refresh(); });
}

void Window::load_settings(const storage::Settings& settings) {
    draft_ = settings;
    filling_ = true;
    gtk_editable_set_text(GTK_EDITABLE(object("azure_key")), "");
    gtk_editable_set_text(GTK_EDITABLE(object("openrouter_key")), "");
    widget<Gtk::Image>("azure_saved")->set_visible(has_azure_key_);
    widget<Gtk::Image>("openrouter_saved")->set_visible(has_openrouter_key_);
    // A saved microphone that is not connected stays selected.
    if (settings.microphone &&
        std::ranges::find(microphone_names_, *settings.microphone) == microphone_names_.end())
        microphone_names_.push_back(*settings.microphone);
    std::vector<std::string> microphones{"System default"};
    microphones.insert(microphones.end(), microphone_names_.begin(), microphone_names_.end());
    set_strings(object("microphone"), microphones);
    auto microphone = std::ranges::find(microphone_names_, settings.microphone.value_or(""));
    adw_combo_row_set_selected(
        ADW_COMBO_ROW(object("microphone")),
        settings.microphone ? static_cast<guint>(microphone - microphone_names_.begin()) + 1 : 0);
    adw_combo_row_set_selected(
        ADW_COMBO_ROW(object("engine")),
        static_cast<guint>(std::ranges::find(ENGINES, settings.cleanup_engine) - ENGINES));
    gtk_editable_set_text(GTK_EDITABLE(object("model")), settings.cleanup_model.c_str());
    adw_combo_row_set_selected(ADW_COMBO_ROW(object("styling")),
                               static_cast<guint>(settings.cleanup_styling));
    const auto& unloads = unload_choices();
    auto unload = std::ranges::find(unloads, settings.cleanup_unload_seconds, &Choice::seconds);
    adw_combo_row_set_selected(
        ADW_COMBO_ROW(object("unload")),
        unload == unloads.end() ? 0 : static_cast<guint>(unload - unloads.begin()));
    filling_ = false;
    show_efforts();
    show_cleanup();
    show_shortcut();
    session(session_);
}

void Window::show_cleanup() {
    bool openrouter = draft_.cleanup_engine == CleanupEngine::OpenRouter;
    bool local = draft_.cleanup_engine == CleanupEngine::S1Mini;
    for (const char* id : {"openrouter_key", "model", "effort"})
        gtk_widget_set_visible(GTK_WIDGET(object(id)), openrouter);
    for (const char* id : {"s1_engine", "s1_model", "styling", "unload"})
        gtk_widget_set_visible(GTK_WIDGET(object(id)), local);
}

void Window::show_efforts() {
    efforts_ = available_efforts(models_, draft_.cleanup_model);
    std::vector<std::string> labels{"Model default"};
    for (auto effort : efforts_)
        labels.emplace_back(reasoning_label(effort));
    auto effective = effective_effort(draft_, models_);
    bool filling = std::exchange(filling_, true);
    set_strings(object("effort"), labels);
    adw_combo_row_set_selected(
        ADW_COMBO_ROW(object("effort")),
        effective
            ? static_cast<guint>(std::ranges::find(efforts_, *effective) - efforts_.begin()) + 1
            : 0);
    filling_ = filling;
    gtk_widget_set_sensitive(GTK_WIDGET(object("effort")), !saving_ && !efforts_.empty());
}

void Window::load_models() {
    loading_models_ = true;
    models_failed_ = false;
    show_models();
    shell::background([app = app_] { return app->cleanup_models(); },
                      [this](std::vector<provider::CleanupModel> models) {
                          models_ = std::move(models);
                          loading_models_ = false;
                          show_models();
                          show_efforts();
                      },
                      [this](const std::string&) {
                          loading_models_ = false;
                          models_failed_ = true;
                          show_models();
                      });
}

void Window::show_models() {
    gtk_widget_set_visible(GTK_WIDGET(object("models_loading")), loading_models_);
    widget<Gtk::Label>("models_error")->set_visible(models_failed_);
    widget<Gtk::Button>("models_retry")->set_visible(models_failed_);
    widget<Gtk::MenuButton>("models_menu")->set_visible(!models_.empty());
    auto* list = widget<Gtk::ListBox>("models_list");
    auto search = widget<Gtk::SearchEntry>("models_search")->get_text().raw();
    list->remove_all();
    for (const auto& model : models_) {
        if (!matches(model.id + " " + model.name, search)) continue;
        auto* box = Gtk::make_managed<Gtk::Box>(Gtk::Orientation::VERTICAL, 2);
        box->set_margin(4);
        auto* name = Gtk::make_managed<Gtk::Label>(model.name);
        name->set_xalign(0);
        auto* id = Gtk::make_managed<Gtk::Label>(model.id);
        id->set_xalign(0);
        id->add_css_class("caption");
        id->add_css_class("dim-label");
        box->append(*name);
        box->append(*id);
        list->append(*box);
    }
}

void Window::s1(const s1::Status& status) {
    show_download(ADW_ACTION_ROW(object("s1_engine")), s1_engine_box_, s1::Part::Engine,
                  status.engine);
    show_download(ADW_ACTION_ROW(object("s1_model")), s1_model_box_, s1::Part::Model, status.model);
}

void Window::show_download(AdwActionRow* row, Gtk::Box& box, s1::Part part,
                           const s1::Download& download) {
    clear(box);
    adw_action_row_set_subtitle(row, download.error.value_or("").c_str());
    if (download.progress) {
        auto* bar = Gtk::make_managed<Gtk::ProgressBar>();
        bar->set_fraction(*download.progress);
        bar->set_valign(Gtk::Align::CENTER);
        bar->set_size_request(120, -1);
        auto* percent = Gtk::make_managed<Gtk::Label>(
            std::to_string(static_cast<int>(std::floor(*download.progress * 100))) + "%");
        percent->add_css_class("numeric");
        auto* cancel = Gtk::make_managed<Gtk::Button>();
        Glib::ustring label =
            part == s1::Part::Engine ? "Cancel engine download" : "Cancel model download";
        cancel->set_icon_name("window-close-symbolic");
        cancel->set_tooltip_text(label);
        shell::accessible_label(cancel->gobj(), label.c_str());
        cancel->add_css_class("flat");
        cancel->signal_clicked().connect(
            [this, part] { run([app = app_, part] { app->cancel_s1_download(part); }); });
        box.append(*bar);
        box.append(*percent);
        box.append(*cancel);
    } else if (download.ready) {
        box.append(*shell::icon("object-select-symbolic"));
        box.append(*Gtk::make_managed<Gtk::Label>("Downloaded"));
    } else {
        auto* start = Gtk::make_managed<Gtk::Button>("Download " + size_label(download.size));
        start->signal_clicked().connect(
            [this, part] { run([app = app_, part] { app->download_s1(part); }); });
        box.append(*start);
    }
}

void Window::show_shortcut() {
    auto mode = recorder_.mode();
    auto& keys = *widget<Gtk::Box>("shortcut_keys");
    clear(keys);
    auto labels = mode == ShortcutRecorder::Mode::Listening ? recorder_.keys()
                  : draft_.shortcut_label                   ? std::vector{*draft_.shortcut_label}
                                                            : shortcut_labels(draft_.shortcut);
    if (mode != ShortcutRecorder::Mode::Starting) {
        for (const auto& label : labels) {
            auto* key = Gtk::make_managed<Gtk::Label>(label);
            key->add_css_class("keycap");
            keys.append(*key);
        }
        if (labels.empty()) keys.append(*Gtk::make_managed<Gtk::Label>("Press keys"));
    }
    gtk_widget_set_visible(GTK_WIDGET(object("shortcut_spinner")),
                           mode == ShortcutRecorder::Mode::Starting);
    widget<Gtk::Button>("shortcut_cancel")->set_visible(mode != ShortcutRecorder::Mode::Idle);
    gtk_widget_set_sensitive(GTK_WIDGET(object("shortcut")), !saving_);
    widget<Gtk::Button>("save")->set_sensitive(!saving_ && mode == ShortcutRecorder::Mode::Idle);
}

void Window::begin_capture() {
    if (recorder_.mode() != ShortcutRecorder::Mode::Idle || saving_) return;
    shortcut_error(std::nullopt);
    if (!window_->is_active()) {
        shortcut_error(FOCUS_SETTINGS);
        return;
    }
    recorder_.starting();
    show_shortcut();
    capture_timer_ = Glib::signal_timeout().connect(
        [this] {
            stop_capture();
            return false;
        },
        15000);
    shell::background([app = app_] { return app->begin_shortcut_capture(); },
                      [this](CaptureSession capture) {
                          if (recorder_.mode() == ShortcutRecorder::Mode::Starting &&
                              !window_->is_active()) {
                              stop_capture();
                              shortcut_error(FOCUS_SETTINGS);
                          }
                          captured(recorder_.begin(capture.token, capture.platform));
                      },
                      [this](const std::string& message) {
                          if (recorder_.mode() == ShortcutRecorder::Mode::Starting) stop_capture();
                          shortcut_error(message);
                      });
}

void Window::capture(const hotkey::Event& event) {
    captured(recorder_.receive(event));
}

void Window::captured(const ShortcutRecorder::Result& result) {
    if (result.stopped) {
        capture_timer_.disconnect();
        if (result.ended)
            shell::background(
                [app = app_, token = *result.ended] { app->end_shortcut_capture(token); }, [] {},
                [](const std::string&) {});
        if (result.shortcut) {
            draft_.shortcut = *result.shortcut;
            draft_.shortcut_label.reset();
        }
    }
    show_shortcut();
}

void Window::stop_capture() {
    if (recorder_.mode() == ShortcutRecorder::Mode::Idle) return;
    // A capture still starting ends when its token arrives.
    captured({true, std::nullopt, recorder_.stop()});
}

void Window::shortcut_error(const std::optional<std::string>& error) {
    adw_action_row_set_subtitle(ADW_ACTION_ROW(object("shortcut")), error.value_or("").c_str());
    widget<Gtk::Button>("grant")->set_visible(error == linux_input::ACCESS_REQUIRED);
}

// Reports nothing once access is granted; a dismissed prompt reports nothing either.
void Window::grant(std::function<void(std::optional<std::string>)> done) {
    auto* button = widget<Gtk::Button>("grant");
    button->set_sensitive(false);
    shell::background([app = app_] { return app->grant_keyboard_access(); },
                      [button, done](bool granted) {
                          button->set_sensitive(true);
                          if (granted) done(std::nullopt);
                      },
                      [button, done](const std::string& message) {
                          button->set_sensitive(true);
                          done(message);
                      });
}

void Window::save() {
    if (saving_ || recorder_.mode() != ShortcutRecorder::Mode::Idle) return;
    auto settings = draft_;
    settings.cleanup_reasoning_effort = effective_effort(draft_, models_);
    auto entered = [&](const char* id) -> std::optional<std::string> {
        auto value = trim(text_of(object(id)));
        if (value.empty()) return std::nullopt;
        return value;
    };
    auto key = entered("openrouter_key");
    auto azure_key = entered("azure_key");
    saving_ = true;
    show_shortcut();
    show_efforts();
    command_error_.reset();
    show_toast();
    shell::background([app = app_, settings, key,
                       azure_key] { return app->save_settings(settings, key, azure_key); },
                      [this, key, azure_key](storage::Settings saved) {
                          saving_ = false;
                          has_azure_key_ = has_azure_key_ || azure_key;
                          has_openrouter_key_ = has_openrouter_key_ || key;
                          load_settings(saved);
                          adw_toast_overlay_add_toast(ADW_TOAST_OVERLAY(object("toasts")),
                                                      adw_toast_new("Saved"));
                      },
                      [this](const std::string& message) {
                          saving_ = false;
                          show_shortcut();
                          show_efforts();
                          fail(message);
                      });
}

void Window::update(const UpdateView& update) {
    update_ = update;
    widget<Gtk::Widget>("updates")->set_visible(update.phase != "unsupported");
    adw_preferences_row_set_title(ADW_PREFERENCES_ROW(object("version")),
                                  ("Version " + update.current).c_str());
    auto* status = widget<Gtk::Label>("update_status");
    status->set_text(update_status(update).value_or(""));
    status->set_tooltip_text(update.phase == "error" ? update.error.value_or("") : "");
    widget<Gtk::Label>("update_label")->set_text(update_action(update));
    gtk_widget_set_visible(GTK_WIDGET(object("update_spinner")), update_busy(update));
    bool recording = busy(session_.phase);
    widget<Gtk::Button>("update_action")
        ->set_sensitive(!update_busy(update) && !(update.phase == "available" && recording));
    adw_view_stack_page_set_needs_attention(ADW_VIEW_STACK_PAGE(object("settings_page")),
                                            update.phase == "available");
}
