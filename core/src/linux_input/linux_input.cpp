#include "transcribe/linux_input.hpp"

#include "transcribe/util.hpp"

#include <libevdev/libevdev-uinput.h>
#include <libevdev/libevdev.h>

#include <cerrno>
#include <cstring>
#include <fcntl.h>
#include <filesystem>
#include <fstream>
#include <linux/input.h>
#include <mutex>
#include <optional>
#include <spawn.h>
#include <sys/ioctl.h>
#include <sys/wait.h>
#include <thread>
#include <unistd.h>
#include <utility>

extern char** environ;

namespace transcribe::linux_input {

namespace {

// logind grants the active local session access to devices tagged uaccess;
// the rule has to sort before 73-seat-late.rules.
constexpr const char* GRANT = R"(set -e
cat > /etc/udev/rules.d/70-transcribe.rules <<'EOF'
SUBSYSTEM=="input", KERNEL=="event*", ENV{ID_INPUT_KEY}=="1", TAG+="uaccess"
KERNEL=="uinput", SUBSYSTEM=="misc", OPTIONS+="static_node=uinput", TAG+="uaccess"
EOF
udevadm control --reload-rules
udevadm trigger --action=change --subsystem-match=input
udevadm trigger --action=change --subsystem-match=misc --sysname-match=uinput
udevadm settle)";

struct Terminal {
    const char* program;
    std::vector<const char*> args;
};

// Programs that run their remaining arguments as a command.
const std::vector<Terminal> TERMINALS = {
    {"xdg-terminal-exec", {}},
    {"gnome-terminal", {"--wait", "--"}},
    {"konsole", {"--nofork", "-e"}},
    {"kitty", {}},
    {"foot", {}},
    {"wezterm", {"start", "--always-new-process", "--"}},
    {"ghostty", {"-e"}},
    {"alacritty", {"-e"}},
    {"xfce4-terminal", {"--disable-server", "-x"}},
    {"xterm", {"-e"}},
};

// Starts `argv` from PATH. Nothing when the program cannot be started.
std::optional<pid_t> spawn(const std::vector<const char*>& argv) {
    std::vector<char*> args;
    for (const char* arg : argv)
        args.push_back(const_cast<char*>(arg));
    args.push_back(nullptr);
    pid_t pid;
    if (posix_spawnp(&pid, args[0], nullptr, nullptr, args.data(), environ) != 0)
        return std::nullopt;
    return pid;
}

// The exit code, or nothing when a signal ended the process.
std::optional<int> wait(pid_t pid) {
    int status;
    while (waitpid(pid, &status, 0) < 0)
        if (errno != EINTR) return std::nullopt;
    if (WIFEXITED(status)) return WEXITSTATUS(status);
    return std::nullopt;
}

bool grant_in_terminal() {
    auto dir = std::filesystem::temp_directory_path() / ("transcribe-" + uuid_simple());
    std::filesystem::create_directory(dir);
    struct Remove {
        std::filesystem::path path;
        ~Remove() {
            std::error_code ignored;
            std::filesystem::remove_all(path, ignored);
        }
    } remove{dir};
    auto result = (dir / "status").string();
    // Closing the window or Ctrl+C kills pkexec but still records a status.
    const char* script = R"(trap : HUP INT; pkexec /bin/sh -c "$1"; echo $? > "$2")";
    std::optional<pid_t> terminal;
    for (const auto& candidate : TERMINALS) {
        std::vector<const char*> argv{candidate.program};
        argv.insert(argv.end(), candidate.args.begin(), candidate.args.end());
        argv.insert(argv.end(), {"/bin/sh", "-c", script, "sh", GRANT, result.c_str()});
        if ((terminal = spawn(argv))) break;
    }
    if (!terminal) throw Error("Keyboard access requires a terminal");
    bool exited = false;
    auto deadline = std::chrono::steady_clock::now() + std::chrono::seconds(300);
    while (std::chrono::steady_clock::now() < deadline) {
        std::this_thread::sleep_for(std::chrono::milliseconds(250));
        bool failed = false;
        if (!exited) {
            int status;
            if (waitpid(*terminal, &status, WNOHANG) == *terminal) {
                exited = true;
                failed = !WIFEXITED(status) || WEXITSTATUS(status) != 0;
            }
        }
        std::ifstream file(result);
        std::string text((std::istreambuf_iterator<char>(file)), {});
        auto code = trim(text);
        char* end = nullptr;
        long value = code.empty() ? 0 : std::strtol(code.c_str(), &end, 10);
        if (!code.empty() && end && *end == '\0') {
            if (value == 0) return true;
            if (value == 126 || value >= 129) return false;
            throw Error("Keyboard access not granted");
        }
        // A terminal may hand the command to a running instance and exit at once, so
        // only a failed launch ends the wait early.
        if (failed) throw Error("Terminal unavailable");
    }
    return false;
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

bool grant_access() {
    auto pid = spawn({"pkexec", "/bin/sh", "-c", GRANT});
    if (!pid) throw Error("Keyboard access requires pkexec");
    auto code = wait(*pid);
    bool granted;
    if (code == 0)
        granted = true;
    else if (code == 126)
        granted = false;
    // No polkit agent: pkexec authenticates on a terminal instead.
    else if (code == 127)
        granted = grant_in_terminal();
    else
        throw Error("Keyboard access not granted");
    if (granted && keyboards().empty()) throw Error("Keyboard access unavailable");
    return granted;
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

void wait_released() {
    auto devices = keyboards();
    if (devices.empty()) throw Error(ACCESS_REQUIRED);
    auto deadline = std::chrono::steady_clock::now() + std::chrono::seconds(2);
    for (;;) {
        bool held = false;
        for (const auto& device : devices) {
            unsigned char bits[KEY_MAX / 8 + 1] = {};
            if (::ioctl(device.fd, EVIOCGKEY(sizeof bits), bits) < 0)
                throw Error(std::strerror(errno));
            for (unsigned code = 0; code <= KEY_MAX && !held; ++code)
                held = is_key(code) && (bits[code / 8] & (1 << (code % 8)));
        }
        if (!held) return;
        if (std::chrono::steady_clock::now() >= deadline)
            throw Error("Release the keyboard before pasting");
        std::this_thread::sleep_for(std::chrono::milliseconds(10));
    }
}

void prepare_keyboard() {
    std::lock_guard lock(keyboard_mutex);
    if (keyboard.uinput) return;
    int fd = ::open("/dev/uinput", O_RDWR | O_CLOEXEC);
    if (fd < 0) throw Error("Automatic paste requires write access to /dev/uinput");
    libevdev* device = libevdev_new();
    libevdev_set_name(device, VIRTUAL_KEYBOARD);
    libevdev_enable_event_type(device, EV_KEY);
    for (unsigned code : {KEY_LEFTCTRL, KEY_LEFTSHIFT, KEY_V, KEY_INSERT})
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

void paste(PasteShortcut shortcut) {
    std::lock_guard lock(keyboard_mutex);
    if (!keyboard.uinput) throw Error("Paste keyboard unavailable");
    std::vector<unsigned> keys;
    switch (shortcut) {
    case PasteShortcut::Clipboard:
        keys = {KEY_LEFTCTRL, KEY_V};
        break;
    case PasteShortcut::TerminalClipboard:
        keys = {KEY_LEFTCTRL, KEY_LEFTSHIFT, KEY_V};
        break;
    case PasteShortcut::TerminalSelection:
        keys = {KEY_LEFTSHIFT, KEY_INSERT};
        break;
    }
    std::vector<std::pair<unsigned, int>> press, release;
    for (unsigned key : keys)
        press.emplace_back(key, 1);
    for (auto it = keys.rbegin(); it != keys.rend(); ++it)
        release.emplace_back(*it, 0);
    // Always attempt every release, even when the press failed partway through.
    std::optional<Error> failure;
    try {
        emit(press);
    } catch (const Error& e) {
        failure = e;
    }
    emit(release);
    if (failure) throw *failure;
}

} // namespace transcribe::linux_input
