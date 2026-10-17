#include "hotkey/engine.hpp"

#include <algorithm>
#include <cwctype>
#include <format>
#include <limits>

#include <windows.h>

namespace transcribe::hotkey::native {

namespace {

thread_local Service::State* service = nullptr;

constexpr UINT PREPARE_CAPTURE = WM_APP + 1;

std::string utf8(const wchar_t* text, int size) {
    int length = WideCharToMultiByte(CP_UTF8, 0, text, size, nullptr, 0, nullptr, nullptr);
    std::string result(length, '\0');
    WideCharToMultiByte(CP_UTF8, 0, text, size, result.data(), length, nullptr, nullptr);
    return result;
}

// Rust's wording for the last OS error.
std::string last_os_error() {
    auto code = GetLastError();
    wchar_t buffer[2048];
    DWORD size = FormatMessageW(FORMAT_MESSAGE_FROM_SYSTEM | FORMAT_MESSAGE_IGNORE_INSERTS, nullptr,
                                code, 0, buffer, 2048, nullptr);
    while (size > 0 && iswspace(buffer[size - 1]))
        --size;
    return std::format("{} (os error {})", utf8(buffer, static_cast<int>(size)), code);
}

LRESULT CALLBACK hook(int code, WPARAM w, LPARAM l);

HHOOK install_hook() {
    return SetWindowsHookExW(WH_KEYBOARD_LL, hook, GetModuleHandleW(nullptr), 0);
}

void reconcile_released_keys(Engine& engine, const std::set<std::uint32_t>& down) {
    std::erase_if(engine.held, [&](auto key) { return !down.contains(key); });
    std::erase_if(engine.swallowed, [&](auto key) { return !engine.held.contains(key); });
    std::erase_if(engine.masked, [&](auto key) { return !engine.held.contains(key); });
    engine.reset();
}

// Replays a menu key's release behind a tap of an unassigned key (0xE8, as
// AutoHotkey does). The app then sees the Alt or Windows press as part of a
// combination and opens no menu or Start. This hook ignores injected events.
bool mask_release(std::uint32_t key) {
    DWORD extended = key == 0xA5 || key == 0x5B || key == 0x5C ? KEYEVENTF_EXTENDEDKEY : 0;
    std::pair<WORD, DWORD> keys[] = {
        {0xE8, 0}, {0xE8, KEYEVENTF_KEYUP}, {static_cast<WORD>(key), KEYEVENTF_KEYUP | extended}};
    INPUT events[3] = {};
    for (int i = 0; i < 3; ++i) {
        events[i].type = INPUT_KEYBOARD;
        events[i].ki.wVk = keys[i].first;
        events[i].ki.wScan = static_cast<WORD>(MapVirtualKeyW(keys[i].first, MAPVK_VK_TO_VSC));
        events[i].ki.dwFlags = keys[i].second;
    }
    return SendInput(3, events, sizeof(INPUT)) == 3;
}

std::string label(std::uint32_t key, DWORD scan, bool extended) {
    switch (key) {
    case 0xA0:
        return "Left Shift";
    case 0xA1:
        return "Right Shift";
    case 0xA2:
        return "Left Ctrl";
    case 0xA3:
        return "Right Ctrl";
    case 0xA4:
        return "Left Alt";
    case 0xA5:
        return "Right Alt";
    case 0x5B:
        return "Left Win";
    case 0x5C:
        return "Right Win";
    case 0x10D:
        return "Numpad Enter";
    case 0x20:
        return "Space";
    case 0xAD:
        return "Mute";
    case 0xAE:
        return "Volume Down";
    case 0xAF:
        return "Volume Up";
    case 0xB0:
        return "Next Track";
    case 0xB1:
        return "Previous Track";
    case 0xB2:
        return "Media Stop";
    case 0xB3:
        return "Play / Pause";
    case 0xA6:
        return "Browser Back";
    case 0xA7:
        return "Browser Forward";
    case 0xA8:
        return "Browser Refresh";
    case 0xA9:
        return "Browser Stop";
    case 0xAA:
        return "Browser Search";
    case 0xAB:
        return "Browser Favorites";
    case 0xAC:
        return "Browser Home";
    }
    wchar_t name[128];
    int count =
        GetKeyNameTextW(static_cast<LONG>((scan << 16) | (DWORD(extended) << 24)), name, 128);
    return count > 0 ? utf8(name, count) : std::format("Key {:X}", key);
}

LRESULT CALLBACK hook(int code, WPARAM w, LPARAM l) {
    const auto* event = reinterpret_cast<const KBDLLHOOKSTRUCT*>(l);
    if (code == HC_ACTION && !(event->flags & LLKHF_INJECTED) && service) {
        bool down = w == WM_KEYDOWN || w == WM_SYSKEYDOWN;
        bool up = w == WM_KEYUP || w == WM_SYSKEYUP;
        if (down || up) {
            bool extended = event->flags & LLKHF_EXTENDED;
            std::uint32_t key = event->vkCode | (event->vkCode == 13 && extended ? 0x100 : 0);
            // Releases can happen out of this hook's sight, such as while
            // Transcribe's own window is in front.
            if (down) service->forget_released(key, pressed);
            bool suppress =
                service->input(key, label(key, event->scanCode, extended), down, modifier(key));
            // If Windows refuses the replay, the real release goes through.
            if (auto mask = service->take_mask()) suppress = mask_release(*mask);
            if (suppress) return 1;
        }
    }
    return CallNextHookEx(nullptr, code, w, l);
}

void run(std::stop_token stop, Service::State& state, std::promise<DWORD>& ready,
         Channel<Reply>& pending) {
    service = &state;
    MSG message;
    PeekMessageW(&message, nullptr, 0, 0, PM_NOREMOVE);
    HHOOK handle = install_hook();
    if (!handle) {
        ready.set_exception(std::make_exception_ptr(Error(last_os_error())));
        return;
    }
    ready.set_value(GetCurrentThreadId());
    std::stop_callback quit(
        stop, [thread = GetCurrentThreadId()] { PostThreadMessageW(thread, WM_QUIT, 0, 0); });
    while (GetMessageW(&message, nullptr, 0, 0) > 0) {
        if (message.message == PREPARE_CAPTURE) {
            while (auto reply = pending.try_recv()) {
                // Windows can silently remove a timed-out hook. Reinstall on
                // its owning thread before acknowledging a new capture.
                std::optional<std::string> result;
                if (HHOOK replacement = install_hook()) {
                    UnhookWindowsHookEx(handle);
                    handle = replacement;
                    // Query before locking: Win32 calls may deliver hooks.
                    std::set<std::uint32_t> down;
                    for (std::uint32_t key = 8; key <= 254; ++key)
                        if (GetAsyncKeyState(static_cast<int>(key)) < 0) down.insert(key);
                    if (down.contains(13)) down.insert(269);
                    std::lock_guard lock(state.mutex);
                    reconcile_released_keys(state.engine, down);
                } else {
                    result = last_os_error();
                }
                reply->set_value(result);
            }
            continue;
        }
        TranslateMessage(&message);
        DispatchMessageW(&message);
    }
    UnhookWindowsHookEx(handle);
    state.failed("Keyboard listener stopped");
}

} // namespace

void prepare_capture(Service::State& state) {
    Reply reply;
    auto result = reply.get_future();
    {
        std::lock_guard lock(state.listener_mutex);
        if (!state.listener_info) throw Error("Keyboard listener unavailable");
        state.listener_info->requests->send(std::move(reply));
        if (!PostThreadMessageW(state.listener_info->thread, PREPARE_CAPTURE, 0, 0))
            throw Error("Keyboard listener unavailable: " + last_os_error());
    }
    if (result.wait_for(std::chrono::seconds(2)) != std::future_status::ready)
        throw Error("Keyboard listener did not respond");
    if (auto error = result.get()) throw Error(*error);
}

bool modifier(std::uint32_t key) {
    return (key >= 0xA0 && key <= 0xA5) || key == 0x5B || key == 0x5C;
}

bool pressed(std::uint32_t key) {
    if (key == 269) key = 13;
    return GetAsyncKeyState(static_cast<int>(key)) < 0;
}

void start(Service::State& state) {
    {
        std::lock_guard lock(state.mutex);
        state.engine.escape = {0x1B};
        state.engine.menu_keys = {0xA4, 0xA5, 0x5B, 0x5C};
    }
    auto requests = std::make_shared<Channel<Reply>>(std::numeric_limits<std::size_t>::max());
    std::promise<DWORD> ready;
    auto thread = ready.get_future();
    state.listener =
        std::jthread([&state, ready = std::move(ready), requests](std::stop_token stop) mutable {
            run(stop, state, ready, *requests);
        });
    auto id = thread.get();
    std::lock_guard lock(state.listener_mutex);
    state.listener_info = Listener{id, requests};
}

void validate(const std::set<std::uint32_t>& keys) {
    for (auto k : keys)
        if (k != 3 && (k < 8 || k > 254) && k != 269) throw Error("Unsupported key");
    if (((keys.contains(0x5B) || keys.contains(0x5C)) && keys.contains(0x4C)) ||
        ((keys.contains(0xA2) || keys.contains(0xA3)) &&
         (keys.contains(0xA4) || keys.contains(0xA5)) && keys.contains(0x2E)))
        throw Error("Shortcut is reserved by Windows");
}

Chord legacy(const std::string& text) {
    static constexpr std::pair<std::string_view, std::uint32_t> named[] = {
        {"Space", 32},
        {"Enter", 13},
        {"Tab", 9},
        {"Escape", 27},
        {"Backspace", 8},
        {"Delete", 46},
        {"Insert", 45},
        {"Home", 36},
        {"End", 35},
        {"PageUp", 33},
        {"PageDown", 34},
        {"ArrowLeft", 37},
        {"Left", 37},
        {"ArrowUp", 38},
        {"Up", 38},
        {"ArrowRight", 39},
        {"Right", 39},
        {"ArrowDown", 40},
        {"Down", 40},
        {"Backquote", 192},
        {"Minus", 189},
        {"Equal", 187},
        {"BracketLeft", 219},
        {"BracketRight", 221},
        {"Backslash", 220},
        {"Semicolon", 186},
        {"Quote", 222},
        {"Comma", 188},
        {"Period", 190},
        {"Slash", 191},
        {"CapsLock", 20},
        {"NumLock", 144},
        {"ScrollLock", 145},
        {"Pause", 19},
        {"PrintScreen", 44},
        {"NumpadEnter", 269},
        {"NumpadAdd", 107},
        {"NumpadSubtract", 109},
        {"NumpadMultiply", 106},
        {"NumpadDivide", 111},
        {"NumpadDecimal", 110},
    };
    Chord binding;
    for (const auto& name : split(text, '+')) {
        auto known =
            std::ranges::find(named, name, &std::pair<std::string_view, std::uint32_t>::first);
        if (name == "CommandOrControl" || name == "Control" || name == "Ctrl")
            binding.push_back({0xA2, 0xA3});
        else if (name == "Alt")
            binding.push_back({0xA4, 0xA5});
        else if (name == "Shift")
            binding.push_back({0xA0, 0xA1});
        else if (name == "Super" || name == "Meta" || name == "Command")
            binding.push_back({0x5B, 0x5C});
        else if (known != std::end(named))
            binding.push_back({known->second});
        else if (name.starts_with("Numpad") && name.size() == 7 && is_ascii_digit(name[6]))
            binding.push_back({96u + (name[6] - '0')});
        else if (name.size() == 1 && is_ascii_alphanumeric(name[0]))
            binding.push_back(
                {static_cast<std::uint32_t>(name[0] >= 'a' ? name[0] - 32 : name[0])});
        else if (auto number = name.starts_with('F') ? parse_u32(name.substr(1)) : std::nullopt;
                 number && *number >= 1 && *number <= 24)
            binding.push_back({111 + *number});
        else
            throw Error("Shortcut needs to be recorded again");
    }
    return binding;
}

} // namespace transcribe::hotkey::native
