#include "pch.h"

#include "PillWindow.xaml.h"
#if __has_include("PillWindow.g.cpp")
#include "PillWindow.g.cpp"
#endif

#include "shell.h"
#include "transcribe/util.hpp"

#include <algorithm>
#include <cmath>
#include <limits>
#include <system_error>

using namespace winrt;
using namespace winrt::Microsoft::UI::Xaml;
using namespace winrt::Microsoft::UI::Xaml::Controls;
using namespace transcribe::app;
using namespace std::chrono_literals;
using winrt::Windows::ApplicationModel::DataTransfer::DataPackageOperation;
using winrt::Windows::Foundation::IInspectable;
namespace backdrops = winrt::Microsoft::UI::Composition::SystemBackdrops;
namespace windowing = winrt::Microsoft::UI::Windowing;

namespace {

Visibility shown(bool visible) {
    return visible ? Visibility::Visible : Visibility::Collapsed;
}

void describe(const FrameworkElement& element, const std::string& value) {
    Automation::AutomationProperties::SetName(element, shell::text(value));
    ToolTipService::SetToolTip(element, value.empty() ? IInspectable{nullptr}
                                                      : box_value(shell::text(value)));
}

// Commands from the pill report nothing back: the session they change is what it shows.
template <class Work> void command(Work work) {
    shell::background(std::move(work), [] {}, [](const std::string&) {});
}

[[noreturn]] void failed(const char* operation) {
    throw transcribe::Error(std::string("Widget ") + operation +
                            " failed: " + std::system_category().message(GetLastError()));
}

} // namespace

namespace winrt::Transcribe::implementation {

void PillWindow::InitializeComponent() {
    PillWindowT::InitializeComponent();
    auto presenter = windowing::OverlappedPresenter::Create();
    presenter.IsResizable(false);
    presenter.IsMaximizable(false);
    presenter.IsMinimizable(false);
    presenter.IsAlwaysOnTop(true);
    presenter.SetBorderAndTitleBar(true, false);
    AppWindow().SetPresenter(presenter);
    auto hwnd = shell::hwnd(AppWindow());
    // Never activated, so the text being dictated into keeps focus; never listed either.
    SetWindowLongPtrW(hwnd, GWL_EXSTYLE,
                      GetWindowLongPtrW(hwnd, GWL_EXSTYLE) | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE);
    DWM_WINDOW_CORNER_PREFERENCE corner = DWMWCP_ROUND;
    DwmSetWindowAttribute(hwnd, DWMWA_WINDOW_CORNER_PREFERENCE, &corner, sizeof corner);

    // Acrylic only shows for the active window by default, and the pill never is.
    backdrop_config_ = backdrops::SystemBackdropConfiguration();
    backdrop_config_.IsInputActive(true);
    auto theme = [this] {
        backdrop_config_.Theme(Root().ActualTheme() == ElementTheme::Dark
                                   ? backdrops::SystemBackdropTheme::Dark
                                   : backdrops::SystemBackdropTheme::Light);
    };
    theme();
    Root().ActualThemeChanged([theme](auto&&, auto&&) { theme(); });
    backdrop_ = backdrops::DesktopAcrylicController();
    backdrop_.SetSystemBackdropConfiguration(backdrop_config_);
    Transcribe::PillWindow self = *this;
    backdrop_.AddSystemBackdropTarget(
        self.as<Microsoft::UI::Composition::ICompositionSupportsSystemBackdrop>());

    clock_timer_ = DispatcherQueue().CreateTimer();
    clock_timer_.Interval(250ms);
    clock_timer_.Tick([this](auto&&, auto&&) {
        Status().Text(shell::text(transcribe::app::clock(shown_.started_at, transcribe::now_ms())));
    });
    CancelButton().Click([this](auto&&, auto&&) { command([app = app_] { app->cancel(); }); });
    StopButton().Click([this](auto&&, auto&&) { command([app = app_] { app->toggle(false); }); });
    HistoryButton().Click(
        [this](auto&&, auto&&) { command([app = app_] { app->open_history(); }); });
    DismissError().Click([this](auto&&, auto&&) { command([app = app_] { app->cancel(); }); });
    DismissCard().Click([this](auto&&, auto&&) { command([app = app_] { app->cancel(); }); });
    Card().DragStarting([this](auto&&, const DragStartingEventArgs& args) {
        args.Data().SetText(shell::text(shown_.transcript.value_or("")));
        args.Data().RequestedOperation(DataPackageOperation::Copy);
        args.AllowedOperations(DataPackageOperation::Copy);
    });
    Card().DropCompleted([this](auto&&, const DropCompletedEventArgs& args) {
        if (args.DropResult() != DataPackageOperation::None)
            command([app = app_] { app->cancel(); });
    });
}

void PillWindow::attach(std::shared_ptr<transcribe::app::App> app) {
    app_ = std::move(app);
}

void PillWindow::show(const Session& session) {
    closing_ = false;
    render(session);
    place();
    if (!rendering_)
        rendering_ = Media::CompositionTarget::Rendering([this](auto&&, auto&&) { tick(); });
}

void PillWindow::session(const Session& current) {
    if (!IsWindowVisible(shell::hwnd(AppWindow()))) return;
    if (current.phase == Phase::Idle) {
        closing_ = true;
        clock_timer_.Stop();
        return;
    }
    closing_ = false;
    render(current);
    try {
        // Words widen the pill as they arrive.
        place();
    } catch (const std::exception&) {
        // The next show reports it.
    }
}

void PillWindow::hide() {
    AppWindow().Hide();
    closing_ = true;
    clock_timer_.Stop();
    if (rendering_) Media::CompositionTarget::Rendering(std::exchange(rendering_, {}));
    last_frame_.reset();
}

void PillWindow::render(const Session& session) {
    shown_ = session;
    auto mode = widget_mode(session);
    SessionView().Visibility(shown(mode == WidgetMode::Session));
    ErrorView().Visibility(shown(mode == WidgetMode::Error));
    CardView().Visibility(shown(mode == WidgetMode::Card));

    if (mode == WidgetMode::Error) {
        Failure().Text(shell::text(session.error.value_or("Error")));
        describe(Failure(), session.error.value_or(""));
    } else if (mode == WidgetMode::Card) {
        TranscriptText().Text(shell::text(*session.transcript));
        describe(TranscriptText(), session.error.value_or(""));
    } else {
        bool recording = session.phase == Phase::Recording;
        bool done = finished(session);
        auto words = spoken(session.words);
        Microphone().Visibility(shown(recording));
        Level().Visibility(shown(recording));
        Busy().Visibility(shown(!recording && !done));
        Busy().IsActive(!recording && !done);
        DoneIcon().Visibility(shown(done));
        WordsScroll().Visibility(shown(words.has_value()));
        if (words) {
            Said().Text(shell::text(words->text));
            Guess().Text(shell::text(words->interim));
            // The newest words stay in view.
            WordsScroll().UpdateLayout();
            WordsScroll().ChangeView(WordsScroll().ScrollableWidth(), nullptr, nullptr, true);
        }
        Status().Visibility(shown(!words));
        if (!recording) Status().Text(shell::text(status_label(session).value_or("Starting")));
        StopButton().Visibility(shown(recording));
        CancelButton().Visibility(shown(!done));
        if (recording && !clock_timer_.IsRunning()) {
            Status().Text(
                shell::text(transcribe::app::clock(shown_.started_at, transcribe::now_ms())));
            clock_timer_.Start();
        } else if (!recording) {
            clock_timer_.Stop();
        }
    }
}

// Centred above the taskbar of the primary display, like the system's own flyouts, at the size
// its content asks for.
void PillWindow::place() {
    auto hwnd = shell::hwnd(AppWindow());
    HMONITOR monitor = MonitorFromWindow(nullptr, MONITOR_DEFAULTTOPRIMARY);
    MONITORINFO info{sizeof(MONITORINFO)};
    if (!GetMonitorInfoW(monitor, &info)) failed("monitor lookup");
    UINT dpi = 96;
    UINT unused = 96;
    GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &dpi, &unused);
    double scale = dpi / 96.0;
    float unbounded = std::numeric_limits<float>::infinity();
    Root().Measure({unbounded, unbounded});
    auto size = Root().DesiredSize();
    const RECT& work = info.rcWork;
    // The content gets the client area, inside the window's frame.
    RECT outer{};
    RECT client{};
    GetWindowRect(hwnd, &outer);
    GetClientRect(hwnd, &client);
    int frame_width = (outer.right - outer.left) - (client.right - client.left);
    int frame_height = (outer.bottom - outer.top) - (client.bottom - client.top);
    int width = std::min(static_cast<int>(std::ceil(size.Width * scale)) + frame_width,
                         static_cast<int>(work.right - work.left));
    int height = std::min(static_cast<int>(std::ceil(size.Height * scale)) + frame_height,
                          static_cast<int>(work.bottom - work.top));
    int left = static_cast<int>(work.left + (work.right - work.left - width) / 2);
    int top =
        std::max(static_cast<int>(work.bottom) - height - static_cast<int>(std::lround(24 * scale)),
                 static_cast<int>(work.top));
    // Also repairs native hiding, such as Show desktop.
    if (IsIconic(hwnd)) ShowWindow(hwnd, SW_SHOWNOACTIVATE);
    if (!SetWindowPos(hwnd, HWND_TOPMOST, left, top, width, height,
                      SWP_NOACTIVATE | SWP_SHOWWINDOW))
        failed("position/show");
    if (!IsWindowVisible(hwnd)) throw transcribe::Error("Widget remained hidden after showing");
}

void PillWindow::tick() {
    auto now = std::chrono::steady_clock::now();
    double dt =
        last_frame_ ? std::min(0.1, std::chrono::duration<double>(now - *last_frame_).count()) : 0;
    last_frame_ = now;
    bool listening = !closing_ && shown_.phase == Phase::Recording;
    // Every recording learns its room afresh.
    if (listening && !listening_) loudness_ = Loudness();
    listening_ = listening;
    Level().Value(listening ? loudness_(level, dt) : 0);
}

} // namespace winrt::Transcribe::implementation
