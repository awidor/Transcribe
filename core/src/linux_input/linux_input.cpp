#include "transcribe/linux_input.hpp"

#include "transcribe/util.hpp"

#include <libevdev/libevdev-uinput.h>
#include <libevdev/libevdev.h>

#include <cerrno>
#include <cstdlib>
#include <cstring>
#include <fcntl.h>
#include <filesystem>
#include <linux/input.h>
#include <mutex>
#include <optional>
#include <spawn.h>
#include <string_view>
#include <sys/wait.h>
#include <thread>
#include <unistd.h>
#include <utility>

extern char** environ;

namespace transcribe::linux_input {

namespace {

// Runs `argv` from PATH with no input. Its exit code and combined output; nothing when it
// cannot be started or a signal ended it.
std::optional<std::pair<int, std::string>> run(const std::vector<std::string>& argv) {
    int out[2];
    if (::pipe2(out, O_CLOEXEC) != 0) return std::nullopt;
    posix_spawn_file_actions_t actions;
    posix_spawn_file_actions_init(&actions);
    posix_spawn_file_actions_addopen(&actions, 0, "/dev/null", O_RDONLY, 0);
    posix_spawn_file_actions_adddup2(&actions, out[1], 1);
    posix_spawn_file_actions_adddup2(&actions, out[1], 2);
    std::vector<char*> args;
    for (const auto& arg : argv)
        args.push_back(const_cast<char*>(arg.c_str()));
    args.push_back(nullptr);
    pid_t pid;
    bool started = posix_spawnp(&pid, args[0], &actions, nullptr, args.data(), environ) == 0;
    posix_spawn_file_actions_destroy(&actions);
    ::close(out[1]);
    std::string output;
    char buffer[4096];
    for (ssize_t n; started && (n = ::read(out[0], buffer, sizeof buffer)) != 0;)
        if (n > 0)
            output.append(buffer, std::size_t(n));
        else if (errno != EINTR)
            break;
    ::close(out[0]);
    if (!started) return std::nullopt;
    int status;
    while (waitpid(pid, &status, 0) < 0)
        if (errno != EINTR) return std::nullopt;
    if (!WIFEXITED(status)) return std::nullopt;
    return std::pair(WEXITSTATUS(status), std::move(output));
}

// Each key event and its report go out 25 ms apart. `write` returns 0 or a negative errno.
template <class Write> void send(Write write, bool terminal) {
    std::vector<unsigned> keys{KEY_LEFTCTRL};
    if (terminal) keys.push_back(KEY_LEFTSHIFT);
    keys.push_back(KEY_V);
    std::vector<std::pair<unsigned, int>> events;
    for (unsigned key : keys)
        events.emplace_back(key, 1);
    for (auto it = keys.rbegin(); it != keys.rend(); ++it)
        events.emplace_back(*it, 0);
    // Always attempt every release, even when a press failed.
    int error = 0;
    for (auto [code, value] : events) {
        int result = write(EV_KEY, code, value);
        if (!result) result = write(EV_SYN, SYN_REPORT, 0);
        if (!error) error = result;
        std::this_thread::sleep_for(std::chrono::milliseconds(25));
    }
    if (error) throw Error(std::strerror(-error));
}

struct VirtualKeyboard {
    libevdev* device = nullptr;
    libevdev_uinput* uinput = nullptr;
};

std::mutex keyboard_mutex;
VirtualKeyboard keyboard;

// Sends the events and one report, like evdev's emit.
void emit(const std::vector<std::pair<unsigned, int>>& keys) {
    int error = 0;
    for (auto [code, value] : keys)
        if (!error) error = libevdev_uinput_write_event(keyboard.uinput, EV_KEY, code, value);
    if (!error) error = libevdev_uinput_write_event(keyboard.uinput, EV_SYN, SYN_REPORT, 0);
    if (error) throw Error(std::strerror(-error));
}

} // namespace

Keyboard::Keyboard(Keyboard&& other) noexcept
    : path(std::move(other.path)), device(std::exchange(other.device, nullptr)),
      fd(std::exchange(other.fd, -1)) {}

Keyboard& Keyboard::operator=(Keyboard&& other) noexcept {
    if (this != &other) {
        this->~Keyboard();
        path = std::move(other.path);
        device = std::exchange(other.device, nullptr);
        fd = std::exchange(other.fd, -1);
    }
    return *this;
}

Keyboard::~Keyboard() {
    if (device) libevdev_free(static_cast<libevdev*>(device));
    if (fd >= 0) ::close(fd);
    device = nullptr;
    fd = -1;
}

std::vector<Keyboard> keyboards() {
    std::vector<Keyboard> found;
    std::error_code error;
    for (const auto& entry : std::filesystem::directory_iterator("/dev/input", error)) {
        auto name = entry.path().filename().string();
        if (!name.starts_with("event")) continue;
        // Read and write when allowed, as evdev opens devices, else read only.
        int fd = ::open(entry.path().c_str(), O_RDWR | O_CLOEXEC);
        if (fd < 0) fd = ::open(entry.path().c_str(), O_RDONLY | O_CLOEXEC);
        if (fd < 0) continue;
        Keyboard keyboard;
        keyboard.path = entry.path().string();
        keyboard.fd = fd;
        libevdev* device = nullptr;
        if (libevdev_new_from_fd(fd, &device) != 0) continue;
        keyboard.device = device;
        const char* device_name = libevdev_get_name(device);
        if (device_name && std::strcmp(device_name, VIRTUAL_KEYBOARD) == 0) continue;
        bool has_key = false;
        for (unsigned code = 0; code <= KEY_MAX && !has_key; ++code)
            has_key = is_key(code) && libevdev_has_event_code(device, EV_KEY, code);
        if (has_key) found.push_back(std::move(keyboard));
    }
    return found;
}

void prepare_keyboard() {
    std::lock_guard lock(keyboard_mutex);
    if (keyboard.uinput) return;
    int fd = ::open("/dev/uinput", O_RDWR | O_CLOEXEC);
    if (fd < 0) throw Error("Automatic paste requires write access to /dev/uinput");
    libevdev* device = libevdev_new();
    libevdev_set_name(device, VIRTUAL_KEYBOARD);
    libevdev_enable_event_type(device, EV_KEY);
    for (unsigned code : {KEY_LEFTCTRL, KEY_LEFTSHIFT, KEY_V})
        libevdev_enable_event_code(device, EV_KEY, code, nullptr);
    libevdev_uinput* uinput = nullptr;
    int error = libevdev_uinput_create_from_device(device, fd, &uinput);
    if (error) {
        libevdev_free(device);
        ::close(fd);
        throw Error(std::strerror(-error));
    }
    // Wait for the input node, then let the compositor discover the device.
    for (int i = 0; i < 100 && !libevdev_uinput_get_devnode(uinput); ++i)
        std::this_thread::sleep_for(std::chrono::milliseconds(10));
    std::this_thread::sleep_for(std::chrono::milliseconds(250));
    keyboard = {device, uinput};
}

void mask() {
    std::lock_guard lock(keyboard_mutex);
    if (!keyboard.uinput) throw Error("Paste keyboard unavailable");
    emit({{KEY_LEFTCTRL, 1}});
    emit({{KEY_LEFTCTRL, 0}});
}

std::vector<std::vector<std::string>> paste_commands(bool wayland, bool kde, bool terminal,
                                                     bool legacy_ydotool) {
    std::string keys = terminal ? "ctrl+shift+v" : "ctrl+v";
    std::vector<std::vector<std::string>> commands;
    if (wayland) {
        // KWin advertises wtype's protocol but ignores its keys.
        if (!terminal && !kde) commands.push_back({"wtype", "-M", "ctrl", "-k", "v", "-m", "ctrl"});
        commands.push_back({"sh", "-c", "echo key " + keys + " | dotool"});
    } else {
        commands.push_back({"xdotool", "key", "--clearmodifiers", keys});
    }
    std::vector<std::string> ydotool{"ydotool", "key"};
    if (legacy_ydotool)
        ydotool.push_back(keys);
    else if (terminal)
        ydotool.insert(ydotool.end(), {"29:1", "42:1", "47:1", "47:0", "42:0", "29:0"});
    else
        ydotool.insert(ydotool.end(), {"29:1", "47:1", "47:0", "29:0"});
    commands.push_back(std::move(ydotool));
    return commands;
}

void paste(bool terminal) {
    // ydotool 0.1 takes key names; later versions take raw keycodes.
    static const bool legacy_ydotool = [] {
        auto help = run({"ydotool", "key", "--help"});
        return help && help->second.find("separated by plus") != std::string::npos;
    }();
    const char* desktop = std::getenv("XDG_CURRENT_DESKTOP");
    bool kde = desktop && std::string_view(desktop).find("KDE") != std::string_view::npos;
    for (const auto& command : paste_commands(is_wayland(), kde, terminal, legacy_ydotool))
        if (auto result = run(command); result && result->first == 0) return;
    try {
        prepare_keyboard();
        std::lock_guard lock(keyboard_mutex);
        send(
            [](unsigned type, unsigned code, int value) {
                return libevdev_uinput_write_event(keyboard.uinput, type, code, value);
            },
            terminal);
        return;
    } catch (const std::exception&) {
    }
    for (auto& device : keyboards()) {
        auto* evdev = static_cast<libevdev*>(device.device);
        if ((::fcntl(device.fd, F_GETFL) & O_ACCMODE) != O_RDWR ||
            !libevdev_has_event_code(evdev, EV_KEY, KEY_V))
            continue;
        send(
            [fd = device.fd](unsigned type, unsigned code, int value) {
                input_event event{};
                event.type = std::uint16_t(type);
                event.code = std::uint16_t(code);
                event.value = value;
                return ::write(fd, &event, sizeof event) == sizeof event ? 0 : -errno;
            },
            terminal);
        return;
    }
    throw Error(ACCESS_REQUIRED);
}

} // namespace transcribe::linux_input
