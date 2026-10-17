#include "hotkey/engine.hpp"
#include "transcribe/linux_input.hpp"

#include <algorithm>
#include <cerrno>
#include <map>
#include <unordered_map>

#include <fcntl.h>
#include <libevdev/libevdev.h>
#include <sys/ioctl.h>

namespace transcribe::hotkey::evdev {

namespace {

using namespace std::chrono_literals;

struct Keyboard {
    linux_input::Keyboard device;
    std::set<std::uint16_t> held;
};

struct Held {
    std::unordered_map<std::uint16_t, std::size_t> counts;
    bool input(std::set<std::uint16_t>& device, std::uint16_t code, bool down) {
        if (down) {
            if (!device.insert(code).second) return false;
            return ++counts[code] == 1;
        }
        if (!device.erase(code)) return false;
        auto& count = counts[code];
        if (count > 0) --count;
        return count == 0;
    }
};

// The evdev crate's names where libevdev's newer ones differ, so labels and saved
// bindings read the same.
constexpr std::pair<std::uint16_t, std::string_view> RENAMED[] = {
    {153, "KEY_DIRECTION"}, {204, "KEY_DASHBOARD"}, {372, "KEY_ZOOM"}, {375, "KEY_SCREEN"}};

std::string key_label(std::uint16_t code) {
    switch (code) {
    case 29:
        return "Left Ctrl";
    case 97:
        return "Right Ctrl";
    case 42:
        return "Left Shift";
    case 54:
        return "Right Shift";
    case 56:
        return "Left Alt";
    case 100:
        return "Right Alt";
    case 125:
        return "Left Super";
    case 126:
        return "Right Super";
    }
    auto renamed =
        std::ranges::find(RENAMED, code, &std::pair<std::uint16_t, std::string_view>::first);
    const char* name = renamed != std::end(RENAMED) ? renamed->second.data()
                                                    : libevdev_event_code_get_name(EV_KEY, code);
    if (!name) return "unknown key: " + std::to_string(code);
    std::string label = name;
    if (label.starts_with("KEY_")) label.erase(0, 4);
    std::ranges::replace(label, '_', ' ');
    return label;
}

// The keys the kernel reports held now; nothing once the device is gone.
std::optional<std::vector<std::uint16_t>> key_state(int fd) {
    unsigned char bits[KEY_MAX / 8 + 1] = {};
    if (::ioctl(fd, EVIOCGKEY(sizeof bits), bits) < 0) return std::nullopt;
    std::vector<std::uint16_t> keys;
    for (unsigned code = 0; code <= KEY_MAX; ++code)
        if (bits[code / 8] & (1 << (code % 8))) keys.push_back(code);
    return keys;
}

// Reads what the device has, as the evdev crate does: a SYN_DROPPED gap comes back as the
// key changes it hid. The result is -EAGAIN once drained, else the read error.
int fetch(libevdev* device, std::vector<input_event>& events) {
    unsigned flag = LIBEVDEV_READ_FLAG_NORMAL;
    for (;;) {
        input_event event;
        int status = libevdev_next_event(device, flag, &event);
        if (status == -EAGAIN && flag == LIBEVDEV_READ_FLAG_SYNC) {
            flag = LIBEVDEV_READ_FLAG_NORMAL;
            continue;
        }
        if (status == LIBEVDEV_READ_STATUS_SYNC)
            flag = LIBEVDEV_READ_FLAG_SYNC;
        else if (status != LIBEVDEV_READ_STATUS_SUCCESS)
            return status;
        events.push_back(event);
    }
}

bool input(Service::State& state, std::uint16_t code, bool down) {
    std::unique_lock lock(state.mutex);
    auto masked = state.engine.masked.size();
    auto events = state.engine
                      .input(Key{code, key_label(code)}, down,
                             code == 29 || code == 97 || code == 42 || code == 54 || code == 56 ||
                                 code == 100 || code == 125 || code == 126)
                      .second;
    // Reading evdev cannot hold a menu key's release back as Windows does, so
    // mask it now, while it is still held.
    bool mask = state.engine.masked.size() > masked;
    lock.unlock();
    if (mask) {
        try {
            linux_input::mask();
        } catch (const std::exception&) {
        }
    }
    bool activate = false;
    for (auto& event : events) {
        if (std::holds_alternative<Activate>(event))
            activate = true;
        else
            state.send(std::move(event));
    }
    return activate;
}

void discover(std::map<std::string, Keyboard>& devices, Held& held, Service::State& state) {
    for (auto& device : linux_input::keyboards()) {
        if (devices.contains(device.path)) continue;
        // Hotplug discovery can race a removal or a session permission change.
        int flags = ::fcntl(device.fd, F_GETFL);
        if (flags < 0 || ::fcntl(device.fd, F_SETFL, flags | O_NONBLOCK) < 0) continue;
        auto pressed = key_state(device.fd);
        if (!pressed) continue;
        std::set<std::uint16_t> keys;
        for (auto code : *pressed)
            if (linux_input::is_key(code) && held.input(keys, code, true)) input(state, code, true);
        auto path = device.path;
        devices.emplace(std::move(path), Keyboard{std::move(device), std::move(keys)});
    }
}

void listen(std::stop_token stop, Service::State& state, std::map<std::string, Keyboard> devices,
            Held held) {
    auto refresh = Clock::now();
    std::vector<std::pair<std::string, Clock::time_point>> pending;
    do {
        std::vector<std::string> removed;
        for (auto& [path, keyboard] : devices) {
            std::vector<input_event> events;
            int status = fetch(static_cast<libevdev*>(keyboard.device.device), events);
            if (events.empty()) {
                if (status != -EAGAIN) removed.push_back(path);
                continue;
            }
            // evdev synthesizes releases when a device disappears.
            // Those releases are cancellation, not a shortcut tap.
            if (!key_state(keyboard.device.fd)) {
                removed.push_back(path);
                continue;
            }
            for (const auto& event : events) {
                bool down = event.value == 1;
                if (event.type == EV_KEY && linux_input::is_key(event.code) &&
                    (event.value == 0 || down) && held.input(keyboard.held, event.code, down) &&
                    input(state, event.code, down)) {
                    // The kernel releases held keys just before removing
                    // a device. Confirm it survived before activating.
                    pending.emplace_back(path, Clock::now());
                }
            }
        }
        if (!removed.empty()) {
            // Unplugging a held key must cancel the stroke, never activate it.
            state.reset_input();
            held = {};
            for (const auto& path : removed) {
                devices.erase(path);
                std::erase_if(pending, [&](const auto& entry) { return entry.first == path; });
            }
            for (auto& [path, keyboard] : devices)
                for (auto code : keyboard.held)
                    if (++held.counts[code] == 1) input(state, code, true);
            std::lock_guard lock(state.mutex);
            state.engine.reset();
        }
        std::erase_if(pending, [&](const auto& entry) {
            if (Clock::now() - entry.second < 40ms) return false;
            auto device = devices.find(entry.first);
            if (device != devices.end() && key_state(device->second.device.fd)) state.activate();
            return true;
        });
        if (Clock::now() - refresh >= 500ms) {
            discover(devices, held, state);
            refresh = Clock::now();
        }
    } while (sleep_for(stop, 5ms));
}

std::uint32_t named_key(std::string_view name) {
    std::string full = "KEY_";
    for (char c : name)
        full += c >= 'a' && c <= 'z' ? static_cast<char>(c - 32) : c;
    auto renamed =
        std::ranges::find(RENAMED, full, &std::pair<std::uint16_t, std::string_view>::second);
    if (renamed != std::end(RENAMED)) return renamed->first;
    int code = libevdev_event_code_from_name(EV_KEY, full.c_str());
    if (code < 0) throw Error("Shortcut needs to be recorded again");
    return code;
}

std::uint32_t dom_key(std::string_view name) {
    static constexpr std::pair<std::string_view, std::string_view> keys[] = {
        {"ControlLeft", "LEFTCTRL"},
        {"ControlRight", "RIGHTCTRL"},
        {"ShiftLeft", "LEFTSHIFT"},
        {"ShiftRight", "RIGHTSHIFT"},
        {"AltLeft", "LEFTALT"},
        {"AltRight", "RIGHTALT"},
        {"MetaLeft", "LEFTMETA"},
        {"MetaRight", "RIGHTMETA"},
        {"Enter", "ENTER"},
        {"Escape", "ESC"},
        {"Space", "SPACE"},
        {"Backspace", "BACKSPACE"},
        {"CapsLock", "CAPSLOCK"},
        {"PageUp", "PAGEUP"},
        {"PageDown", "PAGEDOWN"},
        {"ArrowLeft", "LEFT"},
        {"ArrowRight", "RIGHT"},
        {"ArrowUp", "UP"},
        {"ArrowDown", "DOWN"},
        {"Backquote", "GRAVE"},
        {"Equal", "EQUAL"},
        {"Minus", "MINUS"},
        {"BracketLeft", "LEFTBRACE"},
        {"BracketRight", "RIGHTBRACE"},
        {"Backslash", "BACKSLASH"},
        {"Semicolon", "SEMICOLON"},
        {"Quote", "APOSTROPHE"},
        {"Comma", "COMMA"},
        {"Period", "DOT"},
        {"Slash", "SLASH"},
        {"NumpadEnter", "KPENTER"},
        {"NumpadAdd", "KPPLUS"},
        {"NumpadSubtract", "KPMINUS"},
        {"NumpadMultiply", "KPASTERISK"},
        {"NumpadDivide", "KPSLASH"},
        {"NumpadDecimal", "KPDOT"},
        {"PrintScreen", "SYSRQ"},
        {"ScrollLock", "SCROLLLOCK"},
        {"NumLock", "NUMLOCK"},
        {"AudioVolumeUp", "VOLUMEUP"},
        {"AudioVolumeDown", "VOLUMEDOWN"},
        {"AudioVolumeMute", "MUTE"},
        {"MediaPlayPause", "PLAYPAUSE"},
        {"MediaStop", "STOPCD"},
        {"MediaTrackNext", "NEXTSONG"},
        {"MediaTrackPrevious", "PREVIOUSSONG"},
        {"ContextMenu", "COMPOSE"},
        {"IntlBackslash", "102ND"},
        {"IntlRo", "RO"},
        {"IntlYen", "YEN"},
        {"Convert", "HENKAN"},
        {"NonConvert", "MUHENKAN"},
        {"KanaMode", "KATAKANAHIRAGANA"},
        {"Lang1", "HANGEUL"},
        {"Lang2", "HANJA"},
        {"Lang3", "KATAKANA"},
        {"Lang4", "HIRAGANA"},
        {"Lang5", "ZENKAKUHANKAKU"},
    };
    auto known =
        std::ranges::find(keys, name, &std::pair<std::string_view, std::string_view>::first);
    if (known != std::end(keys)) return named_key(known->second);
    if (name.starts_with("Key") && name.size() == 4) return named_key(name.substr(3));
    if (name.starts_with("Digit") && name.size() == 6) return named_key(name.substr(5));
    if (name.starts_with("Numpad") && name.size() == 7)
        return named_key("KP" + std::string(name.substr(6)));
    return named_key(name);
}

} // namespace

void start(Service::State& state) {
    {
        std::lock_guard lock(state.mutex);
        state.engine.escape = {KEY_ESC};
        // Alt and Super, which open menu bars and the overview when tapped alone.
        state.engine.menu_keys = {56, 100, 125, 126};
    }
    std::map<std::string, Keyboard> devices;
    Held held;
    discover(devices, held, state);
    if (devices.empty()) throw Error(linux_input::ACCESS_REQUIRED);
    state.listener = std::jthread([&state, devices = std::move(devices),
                                   held = std::move(held)](std::stop_token stop) mutable {
        // Masking must not wait for the virtual keyboard on the first press.
        // Without uinput access, paste reports the error when it is used.
        try {
            linux_input::prepare_keyboard();
        } catch (const std::exception&) {
        }
        try {
            listen(stop, state, std::move(devices), std::move(held));
        } catch (const std::exception& error) {
            state.failed(std::string("Keyboard listener stopped: ") + error.what());
        }
    });
}

Chord binding(const std::string& text) {
    Chord keys;
    if (text.starts_with('{')) {
        auto binding = parse(text);
        for (const auto& key : binding.keys) {
            if (binding.platform == "linux-evdev")
                keys.push_back({key.code});
            else if (binding.platform == "wayland")
                keys.push_back({dom_key(key.label)});
            else if (binding.platform == "linux") {
                if (key.code < 8) throw Error("Invalid X11 key");
                keys.push_back({key.code - 8});
            } else
                throw Error("Shortcut belongs to another platform");
        }
    } else {
        for (const auto& name : split(text, '+')) {
            if (name == "CommandOrControl" || name == "Control" || name == "Ctrl")
                keys.push_back({29, 97});
            else if (name == "Shift")
                keys.push_back({42, 54});
            else if (name == "Alt")
                keys.push_back({56, 100});
            else if (name == "Super" || name == "Meta" || name == "Command")
                keys.push_back({125, 126});
            else
                keys.push_back({dom_key(name)});
        }
    }
    if (keys.empty() || keys.size() > 512) throw Error("Invalid shortcut");
    std::set<std::uint32_t> seen;
    for (const auto& options : keys) {
        for (auto code : options) {
            if (!linux_input::is_key(code)) throw Error("Unsupported Linux key");
            if (!seen.insert(code).second) throw Error("Duplicate shortcut key");
        }
    }
    return keys;
}

} // namespace transcribe::hotkey::evdev
