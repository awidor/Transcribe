#include "hotkey/engine.hpp"

#include <cstdlib>
#include <map>
#include <memory>

#include <sys/socket.h>
#include <xcb/xcb.h>
#include <xcb/xinput.h>
#include <xcb/xtest.h>

namespace transcribe::hotkey::native {

namespace {

struct Free {
    void operator()(void* pointer) const { std::free(pointer); }
};
template <class T> using Reply = std::unique_ptr<T, Free>;
using Connection = std::unique_ptr<xcb_connection_t, decltype(&xcb_disconnect)>;

// The wording x11rb gives a broken connection.
std::string connection_error(xcb_connection_t* conn) {
    switch (xcb_connection_has_error(conn)) {
    case XCB_CONN_CLOSED_EXT_NOTSUPPORTED:
        return "Unsupported extension";
    case XCB_CONN_CLOSED_MEM_INSUFFICIENT:
        return "Insufficient memory";
    case XCB_CONN_CLOSED_REQ_LEN_EXCEED:
        return "Maximum request length exceeded";
    case XCB_CONN_CLOSED_FDPASSING_FAILED:
        return "FD passing failed";
    default:
        return "Unknown connection error";
    }
}

// A reply, or the request's error.
// Takes the error's address: it is only filled once the reply call has run.
template <class T> Reply<T> checked(xcb_connection_t* conn, T* reply, xcb_generic_error_t** error) {
    Reply<xcb_generic_error_t> failure(*error);
    if (failure) throw Error("X11 error " + std::to_string(failure->error_code));
    if (!reply) throw Error(connection_error(conn));
    return Reply<T>(reply);
}

Connection connect(int* screen) {
    Connection conn(xcb_connect(nullptr, screen), xcb_disconnect);
    if (xcb_connection_has_error(conn.get())) throw Error("X11 keyboard connection unavailable");
    return conn;
}

xcb_window_t root(xcb_connection_t* conn, int screen) {
    auto roots = xcb_setup_roots_iterator(xcb_get_setup(conn));
    for (int i = 0; i < screen; ++i)
        xcb_screen_next(&roots);
    return roots.data->root;
}

std::string names(std::uint32_t symbol) {
    switch (symbol) {
    case 0xffe1:
        return "Left Shift";
    case 0xffe2:
        return "Right Shift";
    case 0xffe3:
        return "Left Ctrl";
    case 0xffe4:
        return "Right Ctrl";
    case 0xffe9:
        return "Left Alt";
    case 0xffea:
        return "Right Alt";
    case 0xffeb:
        return "Left Super";
    case 0xffec:
        return "Right Super";
    case 0xfe03:
        return "AltGr";
    case 0xff0d:
        return "Enter";
    case 0xff8d:
        return "Numpad Enter";
    case 0xff1b:
        return "Escape";
    case 0xff09:
        return "Tab";
    case 0xff08:
        return "Backspace";
    case 0xffff:
        return "Delete";
    case 0xff63:
        return "Insert";
    case 0xff50:
        return "Home";
    case 0xff57:
        return "End";
    case 0xff51:
        return "Left";
    case 0xff52:
        return "Up";
    case 0xff53:
        return "Right";
    case 0xff54:
        return "Down";
    case 0xff55:
        return "Page Up";
    case 0xff56:
        return "Page Down";
    case 0xffe5:
        return "Caps Lock";
    case 0xff7f:
        return "Num Lock";
    case 0xff14:
        return "Scroll Lock";
    case 0xff13:
        return "Pause";
    case 0xff61:
        return "Print Screen";
    case 32:
        return "Space";
    case 0x1008ff11:
        return "Volume Down";
    case 0x1008ff12:
        return "Mute";
    case 0x1008ff13:
        return "Volume Up";
    case 0x1008ff14:
        return "Play / Pause";
    case 0x1008ff15:
        return "Media Stop";
    case 0x1008ff16:
        return "Previous Track";
    case 0x1008ff17:
        return "Next Track";
    default:
        return "";
    }
}

std::string label(std::uint32_t symbol, std::uint32_t code) {
    auto name = names(symbol);
    if (!name.empty()) return name;
    if (symbol >= 0xffbe && symbol <= 0xffe0) return "F" + std::to_string(symbol - 0xffbe + 1);
    if (symbol >= 33 && symbol <= 126)
        return std::string(
            1, static_cast<char>(symbol >= 'a' && symbol <= 'z' ? symbol - 32 : symbol));
    return "Key " + std::to_string(code);
}

std::map<std::uint32_t, std::uint32_t> mapping(xcb_connection_t* conn) {
    const auto* setup = xcb_get_setup(conn);
    auto min = setup->min_keycode;
    xcb_generic_error_t* error = nullptr;
    auto reply = checked(
        conn,
        xcb_get_keyboard_mapping_reply(
            conn, xcb_get_keyboard_mapping(conn, min, setup->max_keycode - min + 1), &error),
        &error);
    const auto* keysyms = xcb_get_keyboard_mapping_keysyms(reply.get());
    int length = xcb_get_keyboard_mapping_keysyms_length(reply.get());
    int per = reply->keysyms_per_keycode;
    std::map<std::uint32_t, std::uint32_t> keys;
    for (int i = 0; per > 0 && i * per < length; ++i)
        keys[min + i] = keysyms[i * per];
    return keys;
}

std::set<std::uint32_t> escape(const std::map<std::uint32_t, std::uint32_t>& keys) {
    std::set<std::uint32_t> codes;
    for (auto [code, symbol] : keys)
        if (symbol == 0xff1b) codes.insert(code);
    return codes;
}

// Meta, Alt and Super, which open menu bars and the overview when tapped alone.
std::set<std::uint32_t> menu(const std::map<std::uint32_t, std::uint32_t>& keys) {
    std::set<std::uint32_t> codes;
    for (auto [code, symbol] : keys)
        if (symbol >= 0xffe7 && symbol <= 0xffec) codes.insert(code);
    return codes;
}

void configure(Service::State& state, const std::map<std::uint32_t, std::uint32_t>& keys) {
    std::lock_guard lock(state.mutex);
    state.engine.escape = escape(keys);
    state.engine.menu_keys = menu(keys);
}

std::set<std::uint16_t> xtest_devices(xcb_connection_t* conn) {
    xcb_generic_error_t* error = nullptr;
    auto reply = checked(conn,
                         xcb_input_xi_query_device_reply(
                             conn, xcb_input_xi_query_device(conn, XCB_INPUT_DEVICE_ALL), &error),
                         &error);
    std::set<std::uint16_t> injected;
    for (auto infos = xcb_input_xi_query_device_infos_iterator(reply.get()); infos.rem;
         xcb_input_xi_device_info_next(&infos)) {
        std::string_view name(xcb_input_xi_device_info_name(infos.data), infos.data->name_len);
        if (name.find("XTEST") != std::string_view::npos) injected.insert(infos.data->deviceid);
    }
    return injected;
}

void listen(Service::State& state, xcb_connection_t* conn, xcb_window_t root, std::uint8_t opcode,
            std::map<std::uint32_t, std::uint32_t> keys) {
    std::set<std::uint16_t> injected;
    bool refresh_devices = true;
    for (;;) {
        if (refresh_devices) {
            injected = xtest_devices(conn);
            refresh_devices = false;
        }
        Reply<xcb_generic_event_t> event(xcb_wait_for_event(conn));
        if (!event) throw Error(connection_error(conn));
        auto type = event->response_type & 0x7f;
        if (type == XCB_MAPPING_NOTIFY) {
            keys = mapping(conn);
            configure(state, keys);
            continue;
        }
        const auto* generic = reinterpret_cast<const xcb_ge_generic_event_t*>(event.get());
        if (type != XCB_GE_GENERIC || generic->extension != opcode) continue;
        if (generic->event_type == XCB_INPUT_HIERARCHY) {
            refresh_devices = true;
            continue;
        }
        if (generic->event_type != XCB_INPUT_RAW_KEY_PRESS &&
            generic->event_type != XCB_INPUT_RAW_KEY_RELEASE)
            continue;
        const auto* e = reinterpret_cast<const xcb_input_raw_key_press_event_t*>(event.get());
        if (injected.contains(e->sourceid) || (e->flags & XCB_INPUT_KEY_EVENT_FLAGS_KEY_REPEAT))
            continue;
        auto found = keys.find(e->detail);
        std::uint32_t symbol = found == keys.end() ? 0 : found->second;
        std::size_t masked;
        {
            std::lock_guard lock(state.mutex);
            masked = state.engine.masked.size();
        }
        state.input(e->detail, label(symbol, e->detail), e->event_type == XCB_INPUT_RAW_KEY_PRESS,
                    (symbol >= 0xffe1 && symbol <= 0xffee) || symbol == 0xfe03);
        // Raw events cannot hold a menu key's release back as Windows does, so mask it now,
        // while it is still held, with a Ctrl tap (see linux_input::mask).
        bool mask;
        {
            std::lock_guard lock(state.mutex);
            mask = state.engine.masked.size() > masked;
        }
        auto control = std::ranges::find_if(keys, [](auto key) { return key.second == 0xffe3; });
        if (mask && control != keys.end()) {
            auto code = static_cast<std::uint8_t>(control->first);
            xcb_test_fake_input(conn, XCB_KEY_PRESS, code, 0, root, 0, 0, 0);
            xcb_test_fake_input(conn, XCB_KEY_RELEASE, code, 0, root, 0, 0, 0);
            xcb_flush(conn);
        }
    }
}

} // namespace

void start(Service::State& state) {
    if (is_wayland()) return evdev::start(state);
    int screen = 0;
    auto conn = connect(&screen);
    xcb_generic_error_t* error = nullptr;
    checked(conn.get(),
            xcb_input_xi_query_version_reply(conn.get(),
                                             xcb_input_xi_query_version(conn.get(), 2, 2), &error),
            &error);
    auto window = root(conn.get(), screen);
    struct Mask {
        xcb_input_event_mask_t head;
        std::uint32_t mask;
    };
    Mask masks[] = {
        {{XCB_INPUT_DEVICE_ALL_MASTER, 1},
         XCB_INPUT_XI_EVENT_MASK_RAW_KEY_PRESS | XCB_INPUT_XI_EVENT_MASK_RAW_KEY_RELEASE},
        {{XCB_INPUT_DEVICE_ALL, 1}, XCB_INPUT_XI_EVENT_MASK_HIERARCHY},
    };
    Reply<xcb_generic_error_t> failure(xcb_request_check(
        conn.get(), xcb_input_xi_select_events_checked(
                        conn.get(), window, 2, reinterpret_cast<xcb_input_event_mask_t*>(masks))));
    if (failure) throw Error("X11 error " + std::to_string(failure->error_code));
    auto keys = mapping(conn.get());
    configure(state, keys);
    xcb_flush(conn.get());
    auto opcode = xcb_get_extension_data(conn.get(), &xcb_input_id)->major_opcode;
    state.listener = std::jthread([&state, conn = std::move(conn), window, opcode,
                                   keys = std::move(keys)](std::stop_token stop) mutable {
        // Wakes the blocking wait for the next event; the X server never sees it.
        std::stop_callback wake(
            stop, [fd = xcb_get_file_descriptor(conn.get())] { ::shutdown(fd, SHUT_RD); });
        try {
            listen(state, conn.get(), window, opcode, std::move(keys));
        } catch (const std::exception& e) {
            state.failed(std::string("Keyboard listener stopped: ") + e.what());
        }
    });
}

void validate(const std::set<std::uint32_t>& keys) {
    for (auto key : keys)
        if (key < 8 || key > 255) throw Error("Unsupported X11 key");
}

Chord legacy(const std::string& text) {
    int screen = 0;
    auto conn = connect(&screen);
    auto map = mapping(conn.get());
    Chord binding;
    for (const auto& name : split(text, '+')) {
        std::vector<std::uint32_t> symbols;
        if (name == "CommandOrControl" || name == "Control" || name == "Ctrl")
            symbols = {0xffe3, 0xffe4};
        else if (name == "Shift")
            symbols = {0xffe1, 0xffe2};
        else if (name == "Alt")
            symbols = {0xffe9, 0xffea};
        else if (name == "Super" || name == "Meta" || name == "Command")
            symbols = {0xffeb, 0xffec};
        else if (name == "Space")
            symbols = {32};
        else if (name == "Enter")
            symbols = {0xff0d};
        else if (name == "Escape")
            symbols = {0xff1b};
        else if (name == "Tab")
            symbols = {0xff09};
        else if (name == "Backspace")
            symbols = {0xff08};
        else if (name == "Delete")
            symbols = {0xffff};
        else if (name == "Insert")
            symbols = {0xff63};
        else if (name == "Home")
            symbols = {0xff50};
        else if (name == "End")
            symbols = {0xff57};
        else if (name == "PageUp")
            symbols = {0xff55};
        else if (name == "PageDown")
            symbols = {0xff56};
        else if (name == "ArrowLeft" || name == "Left")
            symbols = {0xff51};
        else if (name == "ArrowUp" || name == "Up")
            symbols = {0xff52};
        else if (name == "ArrowRight" || name == "Right")
            symbols = {0xff53};
        else if (name == "ArrowDown" || name == "Down")
            symbols = {0xff54};
        else if (name == "Backquote")
            symbols = {96};
        else if (name == "Minus")
            symbols = {45};
        else if (name == "Equal")
            symbols = {61};
        else if (name == "BracketLeft")
            symbols = {91};
        else if (name == "BracketRight")
            symbols = {93};
        else if (name == "Backslash")
            symbols = {92};
        else if (name == "Semicolon")
            symbols = {59};
        else if (name == "Quote")
            symbols = {39};
        else if (name == "Comma")
            symbols = {44};
        else if (name == "Period")
            symbols = {46};
        else if (name == "Slash")
            symbols = {47};
        else if (name == "NumpadEnter")
            symbols = {0xff8d};
        else if (name == "NumpadAdd")
            symbols = {0xffab};
        else if (name == "NumpadSubtract")
            symbols = {0xffad};
        else if (name == "NumpadMultiply")
            symbols = {0xffaa};
        else if (name == "NumpadDivide")
            symbols = {0xffaf};
        else if (name == "NumpadDecimal")
            symbols = {0xffae, 0xff9f};
        else if (name.starts_with("Numpad") && name.size() == 7 && is_ascii_digit(name[6])) {
            static constexpr std::uint32_t keypad[] = {0xff9e, 0xff9c, 0xff99, 0xff9b, 0xff96,
                                                       0xff9d, 0xff98, 0xff95, 0xff97, 0xff9a};
            auto digit = name[6] - '0';
            symbols = {0xffb0u + digit, keypad[digit]};
        } else if (name.size() == 1 && is_ascii_alphanumeric(name[0]))
            symbols = {static_cast<std::uint32_t>(to_lower_ascii(name)[0])};
        else if (auto number = name.starts_with('F') ? parse_u32(name.substr(1)) : std::nullopt;
                 number && *number >= 1 && *number <= 24)
            symbols = {0xffbd + *number};
        else
            throw Error("Shortcut needs to be recorded again");
        std::vector<std::uint32_t> codes;
        for (auto [code, symbol] : map)
            if (std::ranges::find(symbols, symbol) != symbols.end()) codes.push_back(code);
        if (codes.empty()) throw Error("Shortcut key is unavailable on this keyboard");
        binding.push_back(std::move(codes));
    }
    return binding;
}

} // namespace transcribe::hotkey::native
