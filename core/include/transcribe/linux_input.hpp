#pragma once

#include <cstdint>
#include <string>
#include <vector>

// Keyboard devices and the virtual paste keyboard on Linux, through evdev and uinput.
namespace transcribe::linux_input {

inline constexpr const char* VIRTUAL_KEYBOARD = "Transcribe paste keyboard";
inline constexpr const char* ACCESS_REQUIRED = "Keyboard access required";

struct Keyboard {
    std::string path;
    // An open libevdev device; closed by the destructor.
    void* device = nullptr;
    int fd = -1;
    Keyboard() = default;
    Keyboard(Keyboard&& other) noexcept;
    Keyboard& operator=(Keyboard&& other) noexcept;
    ~Keyboard();
};

// Every readable keyboard except the paste keyboard.
std::vector<Keyboard> keyboards();

// EV_KEY also contains mouse and gamepad buttons.
inline bool is_key(std::uint32_t code) {
    return (code >= 1 && code < 0x100) || (code >= 0x160 && code <= 0x2ff);
}

void prepare_keyboard();

// Taps Ctrl while the shortcut's Alt or Super is held, as keyd does. The desktop then sees a
// combination instead of a lone tap, which would open a menu bar or the overview on release.
void mask();

// The paste tools tried in order, each an argv; the first that exits with 0 wins.
std::vector<std::vector<std::string>> paste_commands(bool wayland, bool kde, bool terminal,
                                                     bool legacy_ydotool);
// Ctrl+V, or Ctrl+Shift+V in a terminal, through the paste tools, then the paste keyboard,
// then a physical keyboard. Throws ACCESS_REQUIRED when none can send it.
void paste(bool terminal);

} // namespace transcribe::linux_input
