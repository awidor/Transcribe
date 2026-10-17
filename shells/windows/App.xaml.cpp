#include "pch.h"

#include "App.xaml.h"
#include "MainWindow.xaml.h"
#include "PillWindow.xaml.h"
#include "shell.h"

#include <windowsx.h>

#include <chrono>

using namespace winrt;
using namespace winrt::Microsoft::UI::Xaml;
using namespace transcribe::app;
namespace lifecycle = winrt::Microsoft::Windows::AppLifecycle;
namespace implementation = winrt::Transcribe::implementation;

namespace {

// These live until the process exits: core threads may still report to them.
implementation::MainWindow* window = nullptr;
implementation::PillWindow* pill = nullptr;
std::shared_ptr<transcribe::app::App>* app = nullptr;

constexpr UINT TRAY_MESSAGE = WM_APP + 1;
constexpr UINT HISTORY = 1;
constexpr UINT QUIT = 2;
UINT taskbar_created = 0;
NOTIFYICONDATAW tray{};

void quit() {
    Shell_NotifyIconW(NIM_DELETE, &tray);
    (*app)->shutdown();
    Application::Current().Exit();
}

void show_widget(bool active_only) {
    auto session = (*app)->widget_to_show(active_only);
    if (!session) return;
    try {
        pill->show(*session);
        (*app)->widget_shown(std::nullopt);
    } catch (const std::exception& e) {
        (*app)->widget_shown(e.what());
    } catch (const hresult_error& e) {
        (*app)->widget_shown(to_string(e.message()));
    }
}

LRESULT CALLBACK tray_proc(HWND hwnd, UINT message, WPARAM wparam, LPARAM lparam) {
    if (message == taskbar_created) {
        // Explorer restarted without the icon.
        Shell_NotifyIconW(NIM_ADD, &tray);
        Shell_NotifyIconW(NIM_SETVERSION, &tray);
        return 0;
    }
    if (message != TRAY_MESSAGE) return DefWindowProcW(hwnd, message, wparam, lparam);
    auto event = LOWORD(lparam);
    if (event != NIN_SELECT && event != NIN_KEYSELECT && event != WM_CONTEXTMENU) return 0;
    HMENU menu = CreatePopupMenu();
    AppendMenuW(menu, MF_STRING, HISTORY, L"History");
    AppendMenuW(menu, MF_STRING, QUIT, L"Quit");
    // The menu only closes on a click elsewhere while its owner is in front.
    SetForegroundWindow(hwnd);
    UINT chosen = TrackPopupMenu(menu, TPM_RETURNCMD | TPM_NONOTIFY | TPM_RIGHTBUTTON,
                                 GET_X_LPARAM(wparam), GET_Y_LPARAM(wparam), 0, hwnd, nullptr);
    PostMessageW(hwnd, WM_NULL, 0, 0);
    DestroyMenu(menu);
    if (chosen == HISTORY) window->show_history();
    if (chosen == QUIT) quit();
    return 0;
}

void create_tray() {
    auto instance = GetModuleHandleW(nullptr);
    WNDCLASSW type{};
    type.lpfnWndProc = tray_proc;
    type.hInstance = instance;
    type.lpszClassName = L"TranscribeTray";
    RegisterClassW(&type);
    // Hidden, but not message-only: those never receive TaskbarCreated.
    HWND hwnd = CreateWindowExW(0, type.lpszClassName, L"Transcribe", WS_OVERLAPPED, 0, 0, 0, 0,
                                nullptr, nullptr, instance, nullptr);
    taskbar_created = RegisterWindowMessageW(L"TaskbarCreated");
    tray.cbSize = sizeof tray;
    tray.hWnd = hwnd;
    tray.uID = 1;
    tray.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP | NIF_SHOWTIP;
    tray.uCallbackMessage = TRAY_MESSAGE;
    auto dpi = GetDpiForSystem();
    tray.hIcon = static_cast<HICON>(LoadImageW(instance, MAKEINTRESOURCEW(1), IMAGE_ICON,
                                               GetSystemMetricsForDpi(SM_CXSMICON, dpi),
                                               GetSystemMetricsForDpi(SM_CYSMICON, dpi), 0));
    wcscpy_s(tray.szTip, L"Transcribe");
    tray.uVersion = NOTIFYICON_VERSION_4;
    Shell_NotifyIconW(NIM_ADD, &tray);
    Shell_NotifyIconW(NIM_SETVERSION, &tray);
}

void CALLBACK destination_changed(HWINEVENTHOOK, DWORD event, HWND hwnd, LONG object, LONG child,
                                  DWORD, DWORD) {
    if (!hwnd || hwnd != GetForegroundWindow()) return;
    if (event == EVENT_OBJECT_LOCATIONCHANGE && (object != OBJID_WINDOW || child != CHILDID_SELF))
        return;
    // Leave the hook at once: showing the widget raises window events of its own.
    shell::ui([] { show_widget(true); });
}

// A hidden widget or a display change must not leave a live recording invisible.
void watch_widget() {
    for (DWORD event : {EVENT_SYSTEM_FOREGROUND, EVENT_OBJECT_LOCATIONCHANGE})
        SetWinEventHook(event, event, nullptr, destination_changed, 0, 0,
                        WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS);
    auto* timer = new Microsoft::UI::Dispatching::DispatcherQueueTimer(shell::queue.CreateTimer());
    timer->Interval(std::chrono::milliseconds(750));
    timer->Tick([](auto&&, auto&&) { show_widget(true); });
    timer->Start();
}

// A second launch hands its activation to the running app, which shows History.
bool redirected() {
    auto instance = lifecycle::AppInstance::FindOrRegisterForKey(L"main");
    if (instance.IsCurrent()) return false;
    auto args = lifecycle::AppInstance::GetCurrent().GetActivatedEventArgs();
    handle done{CreateEventW(nullptr, TRUE, FALSE, nullptr)};
    // Redirecting blocks, so it runs elsewhere while this apartment keeps pumping.
    std::thread redirect([&] {
        try {
            instance.RedirectActivationToAsync(args).get();
        } catch (const hresult_error&) {
        }
        SetEvent(done.get());
    });
    HANDLE handles[] = {done.get()};
    DWORD index = 0;
    CoWaitForMultipleObjects(CWMO_DEFAULT, INFINITE, 1, handles, &index);
    redirect.join();
    return true;
}

} // namespace

namespace winrt::Transcribe::implementation {

void App::OnLaunched(const LaunchActivatedEventArgs&) {
    shell::queue = Microsoft::UI::Dispatching::DispatcherQueue::GetForCurrentThread();
    // Closing the window keeps the app running in the tray.
    DispatcherShutdownMode(Microsoft::UI::Xaml::DispatcherShutdownMode::OnExplicitShutdown);
    auto* main = new Transcribe::MainWindow(make<implementation::MainWindow>());
    window = get_self<implementation::MainWindow>(*main);
    auto* widget = new Transcribe::PillWindow(make<implementation::PillWindow>());
    pill = get_self<implementation::PillWindow>(*widget);

    Listener listener;
    listener.session = [](const Session& session) {
        shell::ui([session] {
            // Levels only arrive while recording; an old one must not linger.
            if (session.phase != Phase::Recording) pill->level = 0;
            window->session(session);
            pill->session(session);
        });
    };
    listener.level = [](float level) { pill->level = level; };
    listener.history = [] { shell::ui([] { window->refresh(); }); };
    listener.open_history = [] { shell::ui([] { window->show_history(); }); };
    listener.s1 = [](const transcribe::s1::Status& status) {
        shell::ui([status] { window->s1(status); });
    };
    listener.capture = [](const transcribe::hotkey::Event& event) {
        shell::ui([event] { window->capture(event); });
    };
    listener.update = [](const UpdateView& update) {
        shell::ui([update] { window->update(update); });
    };
    listener.show_widget = [] { shell::ui([] { show_widget(false); }); };
    listener.hide_widget = [] { shell::ui([] { pill->hide(); }); };
    listener.main_focused = [] { return window->focused.load(); };
    app = new std::shared_ptr<transcribe::app::App>(
        transcribe::app::App::create(data_dir(), std::move(listener), updater()));
    pill->attach(*app);
    window->attach(*app, quit);

    create_tray();
    watch_widget();
    lifecycle::AppInstance::GetCurrent().Activated(
        [](auto&&, auto&&) { shell::ui([] { window->show_history(); }); });
    window->show_history();
}

} // namespace winrt::Transcribe::implementation

int WINAPI wWinMain(HINSTANCE, HINSTANCE, PWSTR, int) {
    startup();
    init_apartment(apartment_type::single_threaded);
    if (redirected()) return 0;
    Application::Start([](auto&&) { make<implementation::App>(); });
    return 0;
}
