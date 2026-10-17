#include "pch.h"

#include "MainWindow.xaml.h"
#if __has_include("MainWindow.g.cpp")
#include "MainWindow.g.cpp"
#endif

#include "shell.h"
#include "transcribe/util.hpp"

#include <algorithm>
#include <chrono>
#include <cmath>
#include <ctime>
#include <filesystem>
#include <format>

using namespace winrt;
using namespace winrt::Microsoft::UI::Xaml;
using namespace winrt::Microsoft::UI::Xaml::Controls;
using namespace transcribe::app;
using namespace std::chrono_literals;
using transcribe::provider::CleanupEngine;
using winrt::Windows::Foundation::IInspectable;
namespace storage = transcribe::storage;
namespace windowing = winrt::Microsoft::UI::Windowing;

namespace {

// Combo positions of the cleanup engines: Off, OpenRouter, S1-mini.
constexpr CleanupEngine ENGINES[] = {CleanupEngine::Off, CleanupEngine::OpenRouter,
                                     CleanupEngine::S1Mini};

winrt::Transcribe::implementation::MainWindow* main_window = nullptr;
WNDPROC base_proc = nullptr;

LRESULT CALLBACK window_proc(HWND hwnd, UINT message, WPARAM wparam, LPARAM lparam) {
    if (message == WM_SYSCOMMAND && (wparam & 0xFFF0) == SC_KEYMENU && main_window->holds_alt())
        return 0;
    return CallWindowProcW(base_proc, hwnd, message, wparam, lparam);
}

Visibility shown(bool visible) {
    return visible ? Visibility::Visible : Visibility::Collapsed;
}

std::string utf8(const hstring& value) {
    return to_string(value);
}

std::string date(std::int64_t ms) {
    std::time_t seconds = ms / 1000;
    std::tm local{};
    localtime_s(&local, &seconds);
    char month[16];
    char time[16];
    std::strftime(month, sizeof month, "%b", &local);
    std::strftime(time, sizeof time, "%H:%M", &local);
    return std::format("{} {}, {}", month, local.tm_mday, time);
}

void set_items(const ComboBox& combo, const std::vector<std::string>& items) {
    combo.Items().Clear();
    for (const auto& item : items)
        combo.Items().Append(box_value(shell::text(item)));
}

FontIcon glyph(const wchar_t* code, double size = 16) {
    FontIcon icon;
    icon.Glyph(code);
    icon.FontSize(size);
    return icon;
}

TextBlock label(std::string_view value, const Style& style = nullptr) {
    TextBlock block;
    block.Text(shell::text(value));
    if (style) block.Style(style);
    return block;
}

void tooltip(const DependencyObject& element, std::string_view value) {
    ToolTipService::SetToolTip(element, value.empty() ? IInspectable{nullptr}
                                                      : box_value(shell::text(value)));
}

// Names a control without text of its own, for screen readers and as its tooltip.
void name(const FrameworkElement& element, std::string_view value) {
    Automation::AutomationProperties::SetName(element, shell::text(value));
    tooltip(element, value);
}

} // namespace

namespace winrt::Transcribe::implementation {

template <class Work, class Done> void MainWindow::run(Work work, Done done) {
    command_error_.reset();
    show_notice();
    shell::background(std::move(work), std::move(done),
                      [this](const std::string& message) { fail(message); });
}

template <class Work> void MainWindow::run(Work work) {
    run(std::move(work), [] {});
}

void MainWindow::InitializeComponent() {
    MainWindowT::InitializeComponent();
    auto hwnd = shell::hwnd(AppWindow());
    double scale = GetDpiForWindow(hwnd) / 96.0;
    AppWindow().Resize({static_cast<int32_t>(760 * scale), static_cast<int32_t>(520 * scale)});
    if (auto presenter = AppWindow().Presenter().try_as<windowing::OverlappedPresenter>()) {
        presenter.PreferredMinimumWidth(static_cast<int32_t>(560 * scale));
        presenter.PreferredMinimumHeight(static_cast<int32_t>(360 * scale));
    }
    AppWindow().SetIcon(Microsoft::UI::GetIconIdFromIcon(
        LoadIconW(GetModuleHandleW(nullptr), MAKEINTRESOURCEW(1))));
    // An Alt in the shortcut must not open the window menu, which would take the next keys.
    main_window = this;
    base_proc = reinterpret_cast<WNDPROC>(
        SetWindowLongPtrW(hwnd, GWLP_WNDPROC, reinterpret_cast<LONG_PTR>(window_proc)));

    AppWindow().Closing([this](auto&&, const windowing::AppWindowClosingEventArgs& args) {
        args.Cancel(true);
        stop_capture();
        AppWindow().Hide();
    });
    Activated([this](auto&&, const WindowActivatedEventArgs& args) {
        focused = args.WindowActivationState() != WindowActivationState::Deactivated;
        if (!focused) {
            stop_capture();
            window_shortcut_.blur();
        } else if (app_) {
            // The shortcut registers after startup, so activating the window reloads it.
            binding_ = app_->shortcut_binding();
        }
    });
    Root().PreviewKeyDown(
        [this](auto&&, const Input::KeyRoutedEventArgs& args) { key(args, true); });
    Root().PreviewKeyUp(
        [this](auto&&, const Input::KeyRoutedEventArgs& args) { key(args, false); });
    Navigation().SelectionChanged(
        [this](auto&&, const NavigationViewSelectionChangedEventArgs& args) {
            show_page(args.IsSettingsSelected());
        });
    // The Settings item exists once the navigation view has loaded.
    Navigation().Loaded([this](auto&&, auto&&) {
        show_page(settings_shown_);
        update(update_);
    });

    clock_timer_ = DispatcherQueue().CreateTimer();
    clock_timer_.Interval(250ms);
    clock_timer_.Tick([this](auto&&, auto&&) {
        Clock().Text(
            shell::text(transcribe::app::clock(session_.started_at, transcribe::now_ms())));
    });
    ImportButton().Click([this](auto&&, auto&&) { pick_audio(); });
    RecordButton().Click([this](auto&&, auto&&) { run([app = app_] { app->toggle(false); }); });
    CancelButton().Click([this](auto&&, auto&&) { run([app = app_] { app->cancel(); }); });
    Notice().CloseButtonClick([this](auto&&, auto&&) {
        notice_message_.reset();
        if (notice_from_session_)
            run([app = app_] { app->cancel(); });
        else
            command_error_.reset();
    });

    // History
    Search().TextChanged([this](auto&&, auto&&) { show_entries(); });
    Entries().SelectionChanged([this](auto&&, auto&&) {
        auto index = Entries().SelectedIndex();
        if (filling_ || index < 0) return;
        save_text();
        select_entry(shown_ids_.at(static_cast<std::size_t>(index)));
    });
    EmptyAction().Click([this](auto&&, auto&&) {
        if (has_azure_key_)
            pick_audio();
        else
            show_page(true);
    });
    Transcript().TextChanged([this](auto&&, auto&&) { show_counts(); });
    Transcript().LostFocus([this](auto&&, auto&&) { save_text(); });
    copied_timer_ = DispatcherQueue().CreateTimer();
    copied_timer_.Interval(1500ms);
    copied_timer_.IsRepeating(false);
    copied_timer_.Tick([this](auto&&, auto&&) {
        CopyIcon().Glyph(L"");
        tooltip(CopyButton(), "Copy");
    });
    CopyButton().Click([this](auto&&, auto&&) {
        if (!selected_) return;
        auto id = *selected_;
        auto value = utf8(Transcript().Text());
        auto it = std::ranges::find(entries_, id, &storage::Entry::id);
        bool edited = it != entries_.end() && it->text != value;
        run(
            [app = app_, id, value, edited] {
                if (edited) app->edit_entry(id, value);
                app->copy_entry(id);
            },
            [this] {
                CopyIcon().Glyph(L"");
                tooltip(CopyButton(), "Copied");
                copied_timer_.Start();
                refresh();
            });
    });
    DeleteButton().Click([this](auto&&, auto&&) {
        if (!selected_) return;
        run([app = app_, id = *selected_] { app->delete_entry(id); }, [this] { refresh(); });
    });

    // Settings
    filling_ = true;
    Microphone().SelectionChanged([this](auto&&, auto&&) {
        if (filling_) return;
        auto index = Microphone().SelectedIndex();
        draft_.microphone.reset();
        if (index > 0 && static_cast<std::size_t>(index) <= microphone_names_.size())
            draft_.microphone = microphone_names_[static_cast<std::size_t>(index) - 1];
    });
    Engine().SelectionChanged([this](auto&&, auto&&) {
        auto index = Engine().SelectedIndex();
        if (filling_ || index < 0) return;
        draft_.cleanup_engine = ENGINES[index];
        show_cleanup();
    });
    Model().TextChanged([this](auto&&, auto&&) {
        if (filling_) return;
        auto model = utf8(Model().Text());
        if (model == draft_.cleanup_model) return;
        draft_.cleanup_reasoning_effort.reset();
        draft_.cleanup_model = model;
        show_efforts();
    });
    Effort().SelectionChanged([this](auto&&, auto&&) {
        if (filling_) return;
        auto index = Effort().SelectedIndex();
        draft_.cleanup_reasoning_effort.reset();
        if (index > 0 && static_cast<std::size_t>(index) <= efforts_.size())
            draft_.cleanup_reasoning_effort = efforts_[static_cast<std::size_t>(index) - 1];
    });
    Styling().SelectionChanged([this](auto&&, auto&&) {
        auto index = Styling().SelectedIndex();
        if (filling_ || index < 0) return;
        draft_.cleanup_styling = static_cast<transcribe::s1::Styling>(index);
    });
    Unload().SelectionChanged([this](auto&&, auto&&) {
        auto index = Unload().SelectedIndex();
        if (filling_ || index < 0) return;
        draft_.cleanup_unload_seconds =
            unload_choices().at(static_cast<std::size_t>(index)).seconds;
    });
    std::vector<std::string> stylings;
    std::vector<std::string> unloads;
    for (int i = 0; i <= static_cast<int>(transcribe::s1::Styling::Formal); ++i)
        stylings.emplace_back(styling_label(static_cast<transcribe::s1::Styling>(i)));
    for (const auto& choice : unload_choices())
        unloads.push_back(choice.label);
    set_items(Styling(), stylings);
    set_items(Unload(), unloads);
    filling_ = false;

    ModelsRetry().Click([this](auto&&, auto&&) { load_models(); });
    ModelsSearch().TextChanged([this](auto&&, auto&&) { show_models(); });
    ModelsList().ItemClick([this](auto&&, const ItemClickEventArgs& args) {
        Model().Text(unbox_value<hstring>(args.ClickedItem().as<FrameworkElement>().Tag()));
        ModelsFlyout().Hide();
    });

    ShortcutButton().Click([this](auto&&, auto&&) { begin_capture(); });
    ShortcutCancel().Click([this](auto&&, auto&&) { stop_capture(); });
    capture_timer_ = DispatcherQueue().CreateTimer();
    capture_timer_.Interval(15s);
    capture_timer_.IsRepeating(false);
    capture_timer_.Tick([this](auto&&, auto&&) { stop_capture(); });
    SaveButton().Click([this](auto&&, auto&&) { save(); });
    saved_timer_ = DispatcherQueue().CreateTimer();
    saved_timer_.Interval(3s);
    saved_timer_.IsRepeating(false);
    saved_timer_.Tick([this](auto&&, auto&&) { Saved().Visibility(Visibility::Collapsed); });

    UpdateAction().Click([this](auto&&, auto&&) {
        if (update_.phase == "available")
            run([app = app_] { app->install_update(); }, [this] { quit_(); });
        else
            run([app = app_] { return app->check_update(); },
                [this](UpdateView view) { update(view); });
    });
}

void MainWindow::attach(std::shared_ptr<transcribe::app::App> app, std::function<void()> quit) {
    app_ = std::move(app);
    quit_ = std::move(quit);
    binding_ = app_->shortcut_binding();
    run([app = app_] { return app->bootstrap(); },
        [this](Bootstrap bootstrap) { bootstrapped(std::move(bootstrap)); });
    run([app = app_] { return app->update_state(); }, [this](UpdateView view) { update(view); });
    run([app = app_] { return app->s1_status(); },
        [this](transcribe::s1::Status status) { s1(status); });
    load_models();
}

bool MainWindow::holds_alt() const {
    return std::ranges::any_of(binding_, [](const auto& options) {
        return std::ranges::any_of(options,
                                   [](auto code) { return code == VK_LMENU || code == VK_RMENU; });
    });
}

void MainWindow::key(const Input::KeyRoutedEventArgs& args, bool down) {
    auto status = args.KeyStatus();
    auto pressed = transcribe::hotkey::windows_key(static_cast<std::uint32_t>(args.Key()),
                                                   status.ScanCode, status.IsExtendedKey);
    bool repeat = down && status.WasKeyDown;
    // Keys are reported here, in order, as they happen: the core only takes a lock.
    if (recorder_.mode() != ShortcutRecorder::Mode::Idle) {
        // Keys being recorded as a shortcut must not also operate the window.
        args.Handled(true);
        auto token = recorder_.token();
        if (recorder_.mode() != ShortcutRecorder::Mode::Listening || !token || repeat) return;
        try {
            for (const auto& event : app_->capture_shortcut_key(*token, pressed, down))
                captured(recorder_.receive(event));
        } catch (const std::exception& e) {
            stop_capture();
            shortcut_error(e.what());
        }
        return;
    }
    if (window_shortcut_.key(pressed.code, down, binding_)) args.Handled(true);
    if (repeat) return;
    try {
        app_->window_shortcut_key(pressed, down);
    } catch (const std::exception&) {
        // A key the listener cannot name is not part of any shortcut.
    }
}

fire_and_forget MainWindow::pick_audio() {
    auto lifetime = get_strong();
    Windows::Storage::Pickers::FileOpenPicker picker;
    picker.as<::IInitializeWithWindow>()->Initialize(shell::hwnd(AppWindow()));
    for (const wchar_t* suffix : {L".wav", L".mp3", L".flac"})
        picker.FileTypeFilter().Append(suffix);
    auto file = co_await picker.PickSingleFileAsync();
    if (!file) co_return;
    run([app = app_, path = std::filesystem::path(std::wstring_view(file.Path()))] {
        app->import_audio(path);
    });
}

void MainWindow::fail(const std::string& message) {
    command_error_ = message;
    show_notice();
}

// A failed paste is offered in the widget and kept in history, not reported here.
void MainWindow::show_notice() {
    auto message = command_error_        ? command_error_
                   : session_.transcript ? std::nullopt
                                         : session_.error;
    if (message == notice_message_) return;
    notice_message_ = message;
    notice_from_session_ = !command_error_;
    Notice().Message(shell::text(message.value_or("")));
    Notice().IsOpen(message.has_value());
}

void MainWindow::show_page(bool settings) {
    settings_shown_ = settings;
    HistoryPage().Visibility(shown(!settings));
    SettingsPage().Visibility(shown(settings));
    IInspectable item = settings ? Navigation().SettingsItem() : IInspectable(HistoryItem());
    if (item && Navigation().SelectedItem() != item) Navigation().SelectedItem(item);
}

void MainWindow::show_history() {
    show_page(false);
    if (auto presenter = AppWindow().Presenter().try_as<windowing::OverlappedPresenter>();
        presenter && presenter.State() == windowing::OverlappedPresenterState::Minimized)
        presenter.Restore();
    AppWindow().Show();
    Activate();
}

void MainWindow::bootstrapped(Bootstrap bootstrap) {
    entries_ = std::move(bootstrap.entries);
    has_azure_key_ = bootstrap.has_azure_key;
    has_openrouter_key_ = bootstrap.has_open_router_key;
    microphone_names_ = std::move(bootstrap.microphones);
    load_settings(bootstrap.settings);
    session(bootstrap.session);
    show_entries();
    if (!has_azure_key_ && !loaded_) show_page(true);
    loaded_ = true;
}

void MainWindow::session(const Session& current) {
    session_ = current;
    bool recording = current.phase == Phase::Recording;
    bool is_busy = working(current.phase);
    bool done = finished(current);
    auto status = status_label(current);

    bool spinning = is_busy && !done;
    RecordIcon().Visibility(shown(!spinning));
    RecordIcon().Glyph(done && is_busy ? L"" : recording ? L"" : L"");
    RecordBusy().Visibility(shown(spinning));
    RecordBusy().IsActive(spinning);
    std::string label = recording ? "Stop" : is_busy ? status.value_or("") : "Record";
    RecordLabel().Text(shell::text(label));
    Automation::AutomationProperties::SetName(RecordButton(),
                                              shell::text(recording ? "Stop recording" : label));
    RecordButton().IsEnabled(!is_busy && has_azure_key_);
    ImportButton().IsEnabled(!is_busy && !recording && has_azure_key_);
    CancelButton().Visibility(shown(recording || spinning));
    EmptyAction().IsEnabled(!has_azure_key_ || (!is_busy && !recording));

    Clock().Visibility(shown(recording));
    if (recording && !clock_timer_.IsRunning()) {
        Clock().Text(
            shell::text(transcribe::app::clock(session_.started_at, transcribe::now_ms())));
        clock_timer_.Start();
    } else if (!recording) {
        clock_timer_.Stop();
    }

    bool preparing = processing(current) && !current.error;
    Progress().Visibility(shown(preparing));
    Progress().IsIndeterminate(!done);
    if (done) Progress().Value(100);

    // History rows still being transcribed show the current stage.
    if (status != entry_status_ &&
        std::ranges::any_of(entries_, [](const auto& e) { return e.status == "transcribing"; }))
        show_entries();
    entry_status_ = status;
    show_notice();
    update(update_);
}

void MainWindow::refresh() {
    shell::background([app = app_] { return app->history(); },
                      [this](std::vector<storage::Entry> entries) {
                          entries_ = std::move(entries);
                          show_entries();
                      },
                      [](const std::string&) {});
}

void MainWindow::show_entries() {
    auto search = utf8(Search().Text());
    auto caption = shell::style(Root(), L"Caption");
    filling_ = true;
    Entries().Items().Clear();
    shown_ids_.clear();
    for (const auto& entry : entries_) {
        if (!matches(entry.text, search)) continue;
        shown_ids_.push_back(entry.id);
        StackPanel row;
        row.Spacing(2);
        row.Padding({0, 6, 0, 6});
        Grid top;
        top.Children().Append(label(date(entry.created_at), caption));
        if (entry.status == "failed") {
            auto dot = glyph(L"");
            dot.Style(shell::style(Root(), L"FailureIcon"));
            dot.HorizontalAlignment(HorizontalAlignment::Right);
            top.Children().Append(dot);
        } else {
            auto seconds = label(std::to_string(std::lround(entry.seconds)) + "s", caption);
            seconds.HorizontalAlignment(HorizontalAlignment::Right);
            top.Children().Append(seconds);
        }
        auto preview = label(entry_preview(entry, session_));
        preview.TextWrapping(TextWrapping::Wrap);
        preview.MaxLines(2);
        preview.TextTrimming(TextTrimming::CharacterEllipsis);
        row.Children().Append(top);
        row.Children().Append(preview);
        Entries().Items().Append(row);
    }
    NoEntries().Text(search.empty() ? L"No transcripts" : L"No matches");
    NoEntries().Visibility(shown(shown_ids_.empty()));
    ListHeading().Text(search.empty() ? L"All transcripts" : L"Search results");
    ListCount().Text(to_hstring(shown_ids_.size()));

    if (!std::ranges::any_of(entries_, [&](const auto& e) { return e.id == selected_; }))
        selected_ = entries_.empty() ? std::nullopt : std::optional(entries_.front().id);
    if (auto it = std::ranges::find(shown_ids_, selected_); it != shown_ids_.end())
        Entries().SelectedIndex(static_cast<int32_t>(it - shown_ids_.begin()));
    filling_ = false;
    select_entry(selected_);
}

void MainWindow::select_entry(const std::optional<std::string>& id) {
    auto it = std::ranges::find(entries_, id, &storage::Entry::id);
    if (it == entries_.end()) {
        selected_.reset();
        EmptyAction().Content(
            box_value(hstring(has_azure_key_ ? L"Import audio" : L"Open Settings")));
        Empty().Visibility(Visibility::Visible);
        Editor().Visibility(Visibility::Collapsed);
        return;
    }
    bool switched = selected_ != id || shown_text_ != it->text;
    selected_ = id;
    const auto& entry = *it;
    Empty().Visibility(Visibility::Collapsed);
    Editor().Visibility(Visibility::Visible);
    EntryTime().Text(shell::text(date(entry.created_at)));
    NotPasted().Visibility(shown(not_pasted(entry)));
    tooltip(NotPasted(), entry.error.value_or(""));
    EntryError().Visibility(shown(entry.error && !entry.error->empty() && !not_pasted(entry)));
    EntryError().Text(shell::text(entry.error.value_or("")));
    Seconds().Text(shell::text(std::to_string(std::lround(entry.seconds)) + " seconds"));
    if (switched) {
        shown_text_ = entry.text;
        Transcript().Text(shell::text(entry.text));
    }
    show_counts();
}

void MainWindow::show_counts() {
    auto value = utf8(Transcript().Text());
    Words().Text(shell::text(std::to_string(word_count(value)) + " words"));
    CopyButton().IsEnabled(!value.empty());
}

void MainWindow::save_text() {
    if (!selected_) return;
    auto it = std::ranges::find(entries_, selected_, &storage::Entry::id);
    auto value = utf8(Transcript().Text());
    if (it == entries_.end() || it->text == value) return;
    it->text = value;
    shown_text_ = value;
    run([app = app_, id = *selected_, value] { app->edit_entry(id, value); },
        [this] { refresh(); });
}

void MainWindow::load_settings(const storage::Settings& settings) {
    draft_ = settings;
    filling_ = true;
    AzureKey().Password(L"");
    OpenRouterKey().Password(L"");
    AzureSaved().Visibility(shown(has_azure_key_));
    OpenRouterSaved().Visibility(shown(has_openrouter_key_));
    // A saved microphone that is not connected stays selected.
    if (settings.microphone &&
        std::ranges::find(microphone_names_, *settings.microphone) == microphone_names_.end())
        microphone_names_.push_back(*settings.microphone);
    std::vector<std::string> microphones{"System default"};
    microphones.insert(microphones.end(), microphone_names_.begin(), microphone_names_.end());
    set_items(Microphone(), microphones);
    auto microphone = std::ranges::find(microphone_names_, settings.microphone.value_or(""));
    Microphone().SelectedIndex(
        settings.microphone ? static_cast<int32_t>(microphone - microphone_names_.begin()) + 1 : 0);
    Engine().SelectedIndex(
        static_cast<int32_t>(std::ranges::find(ENGINES, settings.cleanup_engine) - ENGINES));
    Model().Text(shell::text(settings.cleanup_model));
    Styling().SelectedIndex(static_cast<int32_t>(settings.cleanup_styling));
    const auto& unloads = unload_choices();
    auto unload = std::ranges::find(unloads, settings.cleanup_unload_seconds, &Choice::seconds);
    Unload().SelectedIndex(
        unload == unloads.end() ? 0 : static_cast<int32_t>(unload - unloads.begin()));
    filling_ = false;
    show_efforts();
    show_cleanup();
    show_shortcut();
    session(session_);
}

void MainWindow::show_cleanup() {
    bool openrouter = draft_.cleanup_engine == CleanupEngine::OpenRouter;
    bool local = draft_.cleanup_engine == CleanupEngine::S1Mini;
    for (const auto& row : {OpenRouterRow(), ModelRow(), EffortRow()})
        row.Visibility(shown(openrouter));
    for (const auto& row : {S1EngineRow(), S1ModelRow(), StylingRow(), UnloadRow()})
        row.Visibility(shown(local));
}

void MainWindow::show_efforts() {
    efforts_ = available_efforts(models_, draft_.cleanup_model);
    std::vector<std::string> labels{"Model default"};
    for (auto effort : efforts_)
        labels.emplace_back(reasoning_label(effort));
    auto effective = effective_effort(draft_, models_);
    bool filling = std::exchange(filling_, true);
    set_items(Effort(), labels);
    Effort().SelectedIndex(
        effective
            ? static_cast<int32_t>(std::ranges::find(efforts_, *effective) - efforts_.begin()) + 1
            : 0);
    filling_ = filling;
    Effort().IsEnabled(!saving_ && !efforts_.empty());
}

void MainWindow::load_models() {
    loading_models_ = true;
    models_failed_ = false;
    show_models();
    shell::background([app = app_] { return app->cleanup_models(); },
                      [this](std::vector<transcribe::provider::CleanupModel> models) {
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

void MainWindow::show_models() {
    ModelsLoading().Visibility(shown(loading_models_));
    ModelsLoading().IsActive(loading_models_);
    ModelsFailed().Visibility(shown(models_failed_));
    ModelsButton().Visibility(shown(!models_.empty()));
    auto search = utf8(ModelsSearch().Text());
    auto caption = shell::style(Root(), L"Caption");
    ModelsList().Items().Clear();
    for (const auto& model : models_) {
        if (!matches(model.id + " " + model.name, search)) continue;
        StackPanel item;
        item.Padding({0, 4, 0, 4});
        item.Tag(box_value(shell::text(model.id)));
        item.Children().Append(label(model.name));
        item.Children().Append(label(model.id, caption));
        ModelsList().Items().Append(item);
    }
}

void MainWindow::s1(const transcribe::s1::Status& status) {
    show_download(S1EngineStatus(), S1EngineError(), transcribe::s1::Part::Engine, status.engine);
    show_download(S1ModelStatus(), S1ModelError(), transcribe::s1::Part::Model, status.model);
}

void MainWindow::show_download(const StackPanel& box, const TextBlock& error,
                               transcribe::s1::Part part,
                               const transcribe::s1::Download& download) {
    box.Children().Clear();
    error.Text(shell::text(download.error.value_or("")));
    error.Visibility(shown(download.error && !download.error->empty()));
    if (download.progress) {
        ProgressBar bar;
        bar.Width(120);
        bar.Value(*download.progress * 100);
        bar.VerticalAlignment(VerticalAlignment::Center);
        auto percent =
            label(std::to_string(static_cast<int>(std::floor(*download.progress * 100))) + "%");
        percent.VerticalAlignment(VerticalAlignment::Center);
        Button cancel;
        cancel.Content(glyph(L"", 12));
        name(cancel, part == transcribe::s1::Part::Engine ? "Cancel engine download"
                                                          : "Cancel model download");
        cancel.Click([this, part](auto&&, auto&&) {
            run([app = app_, part] { app->cancel_s1_download(part); });
        });
        box.Children().Append(bar);
        box.Children().Append(percent);
        box.Children().Append(cancel);
    } else if (download.ready) {
        auto done = glyph(L"");
        done.VerticalAlignment(VerticalAlignment::Center);
        auto text = label("Downloaded");
        text.VerticalAlignment(VerticalAlignment::Center);
        box.Children().Append(done);
        box.Children().Append(text);
    } else {
        Button start;
        start.Content(box_value(shell::text("Download " + size_label(download.size))));
        start.Click(
            [this, part](auto&&, auto&&) { run([app = app_, part] { app->download_s1(part); }); });
        box.Children().Append(start);
    }
}

void MainWindow::show_shortcut() {
    auto mode = recorder_.mode();
    ShortcutKeys().Children().Clear();
    auto labels = mode == ShortcutRecorder::Mode::Listening ? recorder_.keys()
                  : draft_.shortcut_label                   ? std::vector{*draft_.shortcut_label}
                                                            : shortcut_labels(draft_.shortcut);
    if (mode != ShortcutRecorder::Mode::Starting) {
        auto keycap = shell::style(Root(), L"Keycap");
        for (const auto& text : labels) {
            Border cap;
            cap.Style(keycap);
            cap.Child(label(text));
            ShortcutKeys().Children().Append(cap);
        }
        if (labels.empty()) ShortcutKeys().Children().Append(label("Press keys"));
    }
    ShortcutSpinner().Visibility(shown(mode == ShortcutRecorder::Mode::Starting));
    ShortcutSpinner().IsActive(mode == ShortcutRecorder::Mode::Starting);
    ShortcutCancel().Visibility(shown(mode != ShortcutRecorder::Mode::Idle));
    ShortcutButton().IsEnabled(!saving_);
    SaveButton().IsEnabled(!saving_ && mode == ShortcutRecorder::Mode::Idle);
}

void MainWindow::begin_capture() {
    if (recorder_.mode() != ShortcutRecorder::Mode::Idle || saving_) return;
    shortcut_error(std::nullopt);
    if (!focused) {
        shortcut_error(FOCUS_SETTINGS);
        return;
    }
    recorder_.starting();
    show_shortcut();
    capture_timer_.Start();
    shell::background([app = app_] { return app->begin_shortcut_capture(); },
                      [this](CaptureSession started) {
                          if (recorder_.mode() == ShortcutRecorder::Mode::Starting && !focused) {
                              stop_capture();
                              shortcut_error(FOCUS_SETTINGS);
                          }
                          captured(recorder_.begin(started.token, started.platform));
                      },
                      [this](const std::string& message) {
                          if (recorder_.mode() == ShortcutRecorder::Mode::Starting) stop_capture();
                          shortcut_error(message);
                      });
}

void MainWindow::capture(const transcribe::hotkey::Event& event) {
    captured(recorder_.receive(event));
}

void MainWindow::captured(const ShortcutRecorder::Result& result) {
    if (result.stopped) {
        capture_timer_.Stop();
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

void MainWindow::stop_capture() {
    if (recorder_.mode() == ShortcutRecorder::Mode::Idle) return;
    // A capture still starting ends when its token arrives.
    captured({true, std::nullopt, recorder_.stop()});
}

void MainWindow::shortcut_error(const std::optional<std::string>& error) {
    ShortcutError().Text(shell::text(error.value_or("")));
    ShortcutError().Visibility(shown(error.has_value()));
}

void MainWindow::save() {
    if (saving_ || recorder_.mode() != ShortcutRecorder::Mode::Idle) return;
    auto settings = draft_;
    settings.cleanup_reasoning_effort = effective_effort(draft_, models_);
    auto entered = [](const hstring& value) -> std::optional<std::string> {
        auto trimmed = transcribe::trim(utf8(value));
        if (trimmed.empty()) return std::nullopt;
        return trimmed;
    };
    auto openrouter_key = entered(OpenRouterKey().Password());
    auto azure_key = entered(AzureKey().Password());
    saving_ = true;
    show_shortcut();
    show_efforts();
    command_error_.reset();
    show_notice();
    shell::background(
        [app = app_, settings, openrouter_key, azure_key] {
            return app->save_settings(settings, openrouter_key, azure_key);
        },
        [this, openrouter_key, azure_key](storage::Settings saved) {
            saving_ = false;
            has_azure_key_ = has_azure_key_ || azure_key;
            has_openrouter_key_ = has_openrouter_key_ || openrouter_key;
            load_settings(saved);
            binding_ = app_->shortcut_binding();
            Saved().Visibility(Visibility::Visible);
            saved_timer_.Start();
        },
        [this](const std::string& message) {
            saving_ = false;
            show_shortcut();
            show_efforts();
            fail(message);
        });
}

void MainWindow::update(const UpdateView& view) {
    update_ = view;
    Updates().Visibility(shown(view.phase != "unsupported"));
    Version().Text(shell::text("Version " + view.current));
    UpdateStatus().Text(shell::text(update_status(view).value_or("")));
    tooltip(UpdateStatus(), view.phase == "error" ? view.error.value_or("") : "");
    UpdateLabel().Text(shell::text(update_action(view)));
    UpdateSpinner().Visibility(shown(update_busy(view)));
    UpdateSpinner().IsActive(update_busy(view));
    UpdateAction().IsEnabled(!update_busy(view) &&
                             !(view.phase == "available" && busy(session_.phase)));
    if (auto item = Navigation().SettingsItem().try_as<NavigationViewItem>())
        item.InfoBadge(view.phase == "available" ? InfoBadge() : InfoBadge(nullptr));
}

} // namespace winrt::Transcribe::implementation
