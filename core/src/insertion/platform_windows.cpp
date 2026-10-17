#include "insertion/destination.hpp"
#include "insertion/platform.hpp"
#include "insertion/windows.hpp"

#include <windows.h>

#include <ole2.h>
#include <uiautomationclient.h>

#include <algorithm>
#include <array>
#include <cstdint>
#include <cstring>
#include <exception>
#include <memory>
#include <string_view>
#include <utility>
#include <vector>

namespace transcribe::insertion::platform {

struct Target {
    HWND hwnd;
    DWORD pid;
    HWND focus;
    std::optional<std::string> identity;
    bool terminal;
    bool no_text_field;
};

namespace {

using namespace std::chrono_literals;
using Clock = std::chrono::steady_clock;
using Snapshot = std::vector<std::pair<UINT, std::string>>;

constexpr std::size_t LIMIT = 64 * 1024 * 1024;

std::string narrow(std::wstring_view text) {
    if (text.empty()) return {};
    int size = WideCharToMultiByte(CP_UTF8, 0, text.data(), int(text.size()), nullptr, 0, nullptr,
                                   nullptr);
    std::string out(std::size_t(size), '\0');
    WideCharToMultiByte(CP_UTF8, 0, text.data(), int(text.size()), out.data(), size, nullptr,
                        nullptr);
    return out;
}

std::string take(BSTR text) {
    auto value = text ? narrow({text, SysStringLen(text)}) : std::string();
    SysFreeString(text);
    return value;
}

struct Release {
    void operator()(IUnknown* object) const { object->Release(); }
};
template <class T> using Com = std::unique_ptr<T, Release>;

struct Automation {
    std::string identity;
    int control_type;
    // Present when the element exposes a Value pattern.
    std::optional<bool> read_only;
};

std::optional<Automation> automation() {
    bool initialized = SUCCEEDED(CoInitializeEx(nullptr, COINIT_MULTITHREADED));
    auto result = []() -> std::optional<Automation> {
        IUIAutomation* created = nullptr;
        if (FAILED(CoCreateInstance(__uuidof(CUIAutomation), nullptr, CLSCTX_INPROC_SERVER,
                                    IID_PPV_ARGS(&created))))
            return std::nullopt;
        Com<IUIAutomation> automation(created);
        IUIAutomationElement* focused = nullptr;
        if (FAILED(automation->GetFocusedElement(&focused)) || !focused) return std::nullopt;
        Com<IUIAutomationElement> e(focused);
        // Element identity (not text contents) distinguishes editor panes, tabs and terminal
        // controls.
        SAFEARRAY* array = nullptr;
        if (FAILED(e->GetRuntimeId(&array))) return std::nullopt;
        std::optional<std::string> runtime = "[";
        LONG lower = 0, upper = -1;
        if (FAILED(SafeArrayGetLBound(array, 1, &lower)) ||
            FAILED(SafeArrayGetUBound(array, 1, &upper)))
            runtime.reset();
        for (LONG i = lower; runtime && i <= upper; ++i) {
            int value = 0;
            if (FAILED(SafeArrayGetElement(array, &i, &value)))
                runtime.reset();
            else
                *runtime += (i == lower ? "" : ", ") + std::to_string(value);
        }
        SafeArrayDestroy(array);
        CONTROLTYPEID control_type = 0;
        if (FAILED(e->get_CurrentControlType(&control_type))) return std::nullopt;
        std::optional<bool> read_only;
        IUIAutomationValuePattern* pattern = nullptr;
        if (SUCCEEDED(e->GetCurrentPatternAs(UIA_ValuePatternId, IID_PPV_ARGS(&pattern))) &&
            pattern) {
            Com<IUIAutomationValuePattern> value(pattern);
            BOOL flag = FALSE;
            if (SUCCEEDED(value->get_CurrentIsReadOnly(&flag))) read_only = flag != FALSE;
        }
        int pid = 0;
        BSTR id = nullptr, class_name = nullptr;
        if (FAILED(e->get_CurrentProcessId(&pid)) || FAILED(e->get_CurrentAutomationId(&id)))
            return std::nullopt;
        auto automation_id = take(id);
        if (FAILED(e->get_CurrentClassName(&class_name)) || !runtime) return std::nullopt;
        return Automation{std::to_string(pid) + "|" + automation_id + "|" + take(class_name) + "|" +
                              std::to_string(control_type) + "|" + *runtime + "]",
                          control_type, read_only};
    }();
    if (initialized) CoUninitialize();
    return result;
}

Target current() {
    HWND hwnd = GetForegroundWindow();
    if (!hwnd) throw Error("Destination unavailable");
    DWORD pid = 0;
    DWORD thread = GetWindowThreadProcessId(hwnd, &pid);
    if (pid == GetCurrentProcessId()) throw Error("Choose a destination");
    GUITHREADINFO info{};
    info.cbSize = sizeof info;
    if (!GetGUIThreadInfo(thread, &info)) throw Error("Destination unavailable");
    std::string exe;
    if (HANDLE process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, FALSE, pid)) {
        wchar_t name[1024];
        DWORD length = 1024;
        if (QueryFullProcessImageNameW(process, 0, name, &length))
            exe = to_lower_ascii(narrow({name, length}));
        CloseHandle(process);
    }
    auto automation = platform::automation();
    wchar_t class_name[256];
    std::wstring_view window_class(class_name,
                                   std::size_t(std::max(GetClassNameW(hwnd, class_name, 256), 0)));
    constexpr std::array<std::string_view, 8> TERMINALS{
        "windowsterminal.exe", "wezterm-gui.exe", "alacritty.exe",  "mintty.exe",
        "conhost.exe",         "cmd.exe",         "powershell.exe", "pwsh.exe",
    };
    bool terminal =
        std::ranges::any_of(TERMINALS, [&](auto name) { return exe.ends_with(name); }) ||
        window_class == L"ConsoleWindowClass" ||
        (automation && to_lower_ascii(automation->identity).find("terminal") != std::string::npos);
    bool no_text_field =
        !terminal && destination::windows_rejects(
                         automation ? std::optional(automation->control_type) : std::nullopt,
                         automation ? automation->read_only : std::nullopt);
    return {hwnd,           pid,
            info.hwndFocus, automation ? std::optional(automation->identity) : std::nullopt,
            terminal,       no_text_field};
}

bool valid(const Target& target) {
    try {
        auto now = current();
        return now.hwnd == target.hwnd && now.pid == target.pid && now.focus == target.focus &&
               now.identity == target.identity;
    } catch (const Error&) {
        return false;
    }
}

ClipboardFailure failure(const char* operation, std::optional<unsigned> format = std::nullopt) {
    return {operation, format, GetLastError()};
}

// A message-only clipboard owner.
struct Window {
    HWND hwnd = CreateWindowExW(0, L"STATIC", L"STATIC", 0, 0, 0, 0, 0, HWND_MESSAGE, nullptr,
                                nullptr, nullptr);
    Window() {
        if (!hwnd) throw failure("owner window creation");
    }
    ~Window() { DestroyWindow(hwnd); }
    Window(const Window&) = delete;
    Window& operator=(const Window&) = delete;
};

void wait_for_paste(Clock::duration duration) {
    auto until = Clock::now() + duration;
    while (Clock::now() < until) {
        pump_messages();
        std::this_thread::sleep_for(10ms);
    }
}

struct Open {
    explicit Open(HWND owner) {
        auto deadline = Clock::now() + 2s;
        while (!OpenClipboard(owner)) {
            if (Clock::now() > deadline) throw failure("open (busy for 2 seconds)");
            pump_messages();
            std::this_thread::sleep_for(15ms);
        }
    }
    ~Open() { CloseClipboard(); }
    Open(const Open&) = delete;
    Open& operator=(const Open&) = delete;
};

void write(UINT format, std::string_view bytes) {
    if (format == CF_ENHMETAFILE) {
        auto metafile =
            SetEnhMetaFileBits(UINT(bytes.size()), reinterpret_cast<const BYTE*>(bytes.data()));
        if (!metafile) throw failure("metafile allocation", format);
        if (!SetClipboardData(format, metafile)) {
            auto error = failure("write", format);
            DeleteEnhMetaFile(metafile);
            throw error;
        }
        return;
    }
    HGLOBAL memory = GlobalAlloc(GMEM_MOVEABLE, std::max<std::size_t>(bytes.size(), 1));
    if (!memory) throw failure("allocation", format);
    void* locked = GlobalLock(memory);
    if (!locked) {
        auto error = failure("write memory lock", format);
        GlobalFree(memory);
        throw error;
    }
    std::memcpy(locked, bytes.data(), bytes.size());
    GlobalUnlock(memory);
    if (!SetClipboardData(format, memory)) {
        auto error = failure("write", format);
        GlobalFree(memory);
        throw error;
    }
}

Snapshot snapshot() {
    Snapshot result;
    std::size_t total = 0;
    for (UINT f = 0;;) {
        SetLastError(0);
        f = EnumClipboardFormats(f);
        if (!f) {
            if (GetLastError()) throw failure("format enumeration");
            break;
        }
        // Windows regenerates CF_BITMAP from a restored DIB, including its color table.
        if (f == CF_BITMAP &&
            (IsClipboardFormatAvailable(CF_DIB) || IsClipboardFormatAvailable(CF_DIBV5)))
            continue;
        // Enumeration lists the placed DIB format before the one Windows synthesizes
        // from it; restoring the first regenerates the other without a second copy.
        if ((f == CF_DIB || f == CF_DIBV5) && std::ranges::any_of(result, [](auto& saved) {
                return saved.first == CF_DIB || saved.first == CF_DIBV5;
            }))
            continue;
        if (f == CF_ENHMETAFILE) {
            auto metafile = static_cast<HENHMETAFILE>(GetClipboardData(f));
            if (!metafile) throw failure("read", f);
            UINT size = GetEnhMetaFileBits(metafile, 0, nullptr);
            if (!size) throw failure("metafile size", f);
            total += size;
            if (total > LIMIT) throw Error("Clipboard too large");
            std::string bytes(size, '\0');
            if (GetEnhMetaFileBits(metafile, size, reinterpret_cast<BYTE*>(bytes.data())) != size)
                throw failure("metafile read", f);
            result.emplace_back(f, std::move(bytes));
            continue;
        }
        // Refuse remaining handle formats instead of destroying a clipboard we cannot restore.
        if (f == 2 || f == 3 || f == 9 || (f >= 0x80 && f <= 0x83) || f == 0x8e)
            throw Error("Clipboard format " + std::to_string(f) + " cannot be preserved");
        HANDLE data = GetClipboardData(f);
        if (!data) throw failure("read", f);
        SIZE_T size = GlobalSize(data);
        total += size;
        if (size == 0 || total > LIMIT)
            throw Error("Clipboard format " + std::to_string(f) + " cannot be preserved (size " +
                        std::to_string(size) + ", total " + std::to_string(total) + ")");
        void* locked = GlobalLock(data);
        if (!locked) throw failure("read memory lock", f);
        result.emplace_back(f, std::string(static_cast<const char*>(locked), size));
        GlobalUnlock(data);
    }
    return result;
}

std::pair<std::unique_ptr<Open>, Snapshot> open_snapshot(HWND owner) {
    return retry_snapshot(
        [&] {
            auto open = std::make_unique<Open>(owner);
            // A failed read closes the clipboard before retrying, allowing a delayed-rendering
            // owner to finish publishing. Every retry starts a fresh full snapshot.
            auto saved = snapshot();
            return std::pair(std::move(open), std::move(saved));
        },
        2s);
}

void clear() {
    if (!EmptyClipboard()) throw failure("clear");
}

void restore(const Snapshot& old) {
    clear();
    std::string errors;
    // Attempt every saved format even if restoring one fails.
    for (auto& [format, bytes] : old) {
        try {
            write(format, bytes);
        } catch (const std::exception& error) {
            errors += (errors.empty() ? "" : "; ") + std::string(error.what());
        }
    }
    if (!errors.empty()) throw Error("Clipboard restore failed: " + errors);
}

std::string text_bytes(const std::string& text) {
    int size = MultiByteToWideChar(CP_UTF8, 0, text.data(), int(text.size()), nullptr, 0);
    std::wstring wide(std::size_t(size), L'\0');
    MultiByteToWideChar(CP_UTF8, 0, text.data(), int(text.size()), wide.data(), size);
    wide.push_back(L'\0');
    return {reinterpret_cast<const char*>(wide.data()), wide.size() * sizeof(wchar_t)};
}

void suppress_history() {
    for (auto* name : {L"CanIncludeInClipboardHistory", L"CanUploadToCloudClipboard"}) {
        UINT format = RegisterClipboardFormatW(name);
        if (!format) throw failure("privacy format registration");
        std::uint32_t zero = 0;
        write(format, {reinterpret_cast<const char*>(&zero), sizeof zero});
    }
}

bool modifiers_released() {
    return std::ranges::all_of(std::array{VK_CONTROL, VK_SHIFT, VK_MENU, VK_LWIN, VK_RWIN},
                               [](int key) { return GetAsyncKeyState(key) >= 0; });
}

// Prefixes the message as anyhow's context does.
[[noreturn]] void rethrow(const std::string& context) {
    try {
        throw;
    } catch (const std::exception& error) {
        throw Error(context + ": " + error.what());
    }
}

void paste_sync(const Target& target, const std::string& text) {
    auto deadline = Clock::now() + 2s;
    while (!modifiers_released()) {
        if (Clock::now() > deadline) throw Error("Shortcut still held");
        std::this_thread::sleep_for(15ms);
    }
    if (!valid(target)) throw Error("Destination changed");
    Window window;
    Snapshot old;
    {
        std::unique_ptr<Open> open;
        try {
            std::tie(open, old) = open_snapshot(window.hwnd);
        } catch (...) {
            rethrow("Could not preserve clipboard before paste");
        }
        clear();
        try {
            write(CF_UNICODETEXT, text_bytes(prepare_text(text, target.terminal)));
            suppress_history();
        } catch (const std::exception& error) {
            try {
                restore(old);
            } catch (...) {
                rethrow(std::string(error.what()) + "; rollback also failed");
            }
            throw;
        }
    }
    // CloseClipboard can synthesize compatible formats and increment the sequence.
    DWORD sequence = GetClipboardSequenceNumber();
    std::exception_ptr dispatched;
    try {
        if (!valid(target) || !modifiers_released()) throw Error("Destination changed");
        if (GetClipboardOwner() != window.hwnd || GetClipboardSequenceNumber() != sequence)
            throw Error("Clipboard changed before paste; transcript saved");
        std::vector<WORD> keys{VK_CONTROL};
        if (target.terminal) keys.push_back(VK_SHIFT);
        keys.push_back('V');
        std::vector<INPUT> events;
        auto add = [&](WORD key, DWORD flags) {
            INPUT input{};
            input.type = INPUT_KEYBOARD;
            input.ki.wVk = key;
            input.ki.dwFlags = flags;
            events.push_back(input);
        };
        for (auto key : keys)
            add(key, 0);
        for (auto key = keys.rbegin(); key != keys.rend(); ++key)
            add(*key, KEYEVENTF_KEYUP);
        if (SendInput(UINT(events.size()), events.data(), sizeof(INPUT)) != events.size()) {
            // Always release our synthetic modifiers, even after a partial dispatch.
            std::vector<INPUT> releases(events.begin() + std::ptrdiff_t(keys.size()), events.end());
            SendInput(UINT(releases.size()), releases.data(), sizeof(INPUT));
            throw Error("Paste could not be confirmed");
        }
        // SendInput acknowledges dispatch, not insertion. Keep history status unverified.
        // Clipboard ownership messages must remain serviced while the destination reads.
        wait_for_paste(1200ms);
    } catch (...) {
        dispatched = std::current_exception();
    }
    {
        std::optional<Open> open;
        try {
            open.emplace(window.hwnd);
        } catch (...) {
            rethrow("Paste unverified; clipboard busy");
        }
        if (GetClipboardOwner() == window.hwnd && GetClipboardSequenceNumber() == sequence) {
            try {
                restore(old);
            } catch (...) {
                rethrow("Paste unverified; clipboard restore failed");
            }
        }
    }
    if (dispatched) std::rethrow_exception(dispatched);
}

} // namespace

void pump_messages() {
    MSG message;
    while (PeekMessageW(&message, nullptr, 0, 0, PM_REMOVE)) {
        TranslateMessage(&message);
        DispatchMessageW(&message);
    }
}

std::shared_ptr<Target> capture() {
    auto target = current();
    // Nothing is sent and the clipboard is untouched; the widget offers the text.
    if (target.no_text_field) throw Error(destination::NO_TEXT_FIELD);
    return std::make_shared<Target>(std::move(target));
}

void prepare() {
    automation();
}

void paste(const Target& target, std::string text) {
    paste_sync(target, text);
}

void copy(const std::string& text) {
    Window window;
    Open open(window.hwnd);
    clear();
    write(CF_UNICODETEXT, text_bytes(text));
}

} // namespace transcribe::insertion::platform
