#include "insertion/data_control.hpp"
#include "insertion/focus.hpp"
#include "insertion/linux.hpp"
#include "insertion/x11.hpp"
#include "insertion/x11_clipboard.hpp"
#include "insertion/xselection.hpp"
#include "transcribe/linux_input.hpp"
#include "transcribe/util.hpp"

#include <gio/gio.h>
#include <poll.h>
#include <unistd.h>
#include <xcb/xtest.h>

#include <algorithm>
#include <array>
#include <cerrno>
#include <chrono>
#include <cstring>
#include <deque>
#include <exception>
#include <map>
#include <thread>
#include <vector>

namespace transcribe::insertion::platform {

namespace {

using namespace std::chrono_literals;
using Clock = std::chrono::steady_clock;
using Data = std::map<std::string, std::string>;
using data_control::Clipboard;
using data_control::Kind;

constexpr std::size_t LIMIT = 64 * 1024 * 1024;
constexpr const char* ACCESSIBLE = "org.a11y.atspi.Accessible";

template <class T, auto Free> using Owned = std::unique_ptr<T, decltype([](T* p) { Free(p); })>;
using Variant = Owned<GVariant, g_variant_unref>;

void close_connection(GDBusConnection* c) {
    g_dbus_connection_close(c, nullptr, nullptr, nullptr);
    g_object_unref(c);
}

std::string text(GVariant* reply) {
    const char* value = "";
    g_variant_get(reply, "(&s)", &value);
    return value;
}

// True when the focused element, found within `timeout`, is in a terminal.
bool in_terminal(Clock::duration timeout) {
    Owned<GCancellable, g_object_unref> cancel(g_cancellable_new());
    // Rust drops the whole lookup at its timeout; cancelling every pending call does the same.
    std::jthread watchdog([&](std::stop_token stop) {
        if (sleep_for(stop, timeout)) g_cancellable_cancel(cancel.get());
    });
    auto call = [&](GDBusConnection* c, const char* bus, const char* path, const char* interface,
                    const char* method, GVariant* args, const char* type) {
        return Variant(g_dbus_connection_call_sync(c, bus, path, interface, method, args,
                                                   G_VARIANT_TYPE(type), G_DBUS_CALL_FLAGS_NONE, -1,
                                                   cancel.get(), nullptr));
    };
    Owned<GDBusConnection, g_object_unref> session(
        g_bus_get_sync(G_BUS_TYPE_SESSION, cancel.get(), nullptr));
    if (!session) return false;
    auto address = call(session.get(), "org.a11y.Bus", "/org/a11y/bus", "org.a11y.Bus",
                        "GetAddress", nullptr, "(s)");
    if (!address) return false;
    Owned<GDBusConnection, close_connection> c(g_dbus_connection_new_for_address_sync(
        text(address.get()).c_str(),
        GDBusConnectionFlags(G_DBUS_CONNECTION_FLAGS_AUTHENTICATION_CLIENT |
                             G_DBUS_CONNECTION_FLAGS_MESSAGE_BUS_CONNECTION),
        nullptr, cancel.get(), nullptr));
    if (!c) return false;

    struct Node {
        std::string bus, path;
        int depth;
        bool terminal;
    };
    std::deque<Node> queue;
    auto extend = [&](GVariant* children, int depth, bool terminal) {
        GVariantIter* it = nullptr;
        const char *bus, *path;
        g_variant_get(children, "(a(so))", &it);
        while (g_variant_iter_next(it, "(&s&o)", &bus, &path))
            queue.push_back({bus, path, depth, terminal});
        g_variant_iter_free(it);
    };
    auto apps = call(c.get(), "org.a11y.atspi.Registry", "/org/a11y/atspi/accessible/root",
                     ACCESSIBLE, "GetChildren", nullptr, "(a(so))");
    if (!apps) return false;
    extend(apps.get(), 0, false);
    for (int count = 1; !queue.empty(); ++count) {
        auto [bus, path, depth, terminal] = std::move(queue.front());
        queue.pop_front();
        if (count > 3000 || g_cancellable_is_cancelled(cancel.get())) break;
        if (!g_dbus_is_name(bus.c_str())) continue;
        auto node = [&](const char* interface, const char* method, GVariant* args,
                        const char* type) {
            return call(c.get(), bus.c_str(), path.c_str(), interface, method, args, type);
        };
        std::uint32_t state = 0;
        if (auto states = node(ACCESSIBLE, "GetState", nullptr, "(au)")) {
            Variant array(g_variant_get_child_value(states.get(), 0));
            gsize length = 0;
            auto* values = static_cast<const std::uint32_t*>(
                g_variant_get_fixed_array(array.get(), &length, sizeof(std::uint32_t)));
            if (length) state = values[0];
        }
        // Only descend into the active top-level window. Inactive applications retain stale focused
        // children.
        if (depth == 1 && !(state & (1 << 1))) continue;
        std::string role;
        if (auto reply = node(ACCESSIBLE, "GetRoleName", nullptr, "(s)")) role = text(reply.get());
        auto named_terminal = [&] {
            auto reply = node("org.freedesktop.DBus.Properties", "Get",
                              g_variant_new("(ss)", ACCESSIBLE, "Name"), "(v)");
            if (!reply) return false;
            Variant name(g_variant_get_child_value(reply.get(), 0));
            Variant value(g_variant_get_variant(name.get()));
            return g_variant_is_of_type(value.get(), G_VARIANT_TYPE_STRING) &&
                   to_lower_ascii(g_variant_get_string(value.get(), nullptr)) == "terminal";
        };
        terminal = terminal || role == "terminal" || named_terminal();
        if (state & (1 << 12)) return terminal;
        if (depth < 30)
            if (auto children = node(ACCESSIBLE, "GetChildren", nullptr, "(a(so))"))
                extend(children.get(), depth + 1, terminal);
    }
    return false;
}

bool x_terminal() {
    int number = 0;
    auto conn = x11::connect(number);
    auto* c = conn.get();
    auto active =
        x11::get(xcb_get_property_reply, c,
                 xcb_get_property(c, 0, x11::screen(c, number)->root,
                                  x11::atom(c, "_NET_ACTIVE_WINDOW"), XCB_ATOM_WINDOW, 0, 1));
    if (active->format != 32 || xcb_get_property_value_length(active.get()) < 4) return false;
    auto window = *static_cast<const std::uint32_t*>(xcb_get_property_value(active.get()));
    auto wm_class =
        x11::get(xcb_get_property_reply, c,
                 xcb_get_property(c, 0, window, XCB_ATOM_WM_CLASS, XCB_ATOM_STRING, 0, 1024));
    return is_terminal(x11::value(wm_class.get()));
}

// The last resort on X11, as enigo sends it.
void x_press(bool terminal) {
    int number = 0;
    auto conn = x11::connect(number);
    auto* c = conn.get();
    auto* setup = xcb_get_setup(c);
    auto keys = x11::get(xcb_get_keyboard_mapping_reply, c,
                         xcb_get_keyboard_mapping(c, setup->min_keycode,
                                                  setup->max_keycode - setup->min_keycode + 1));
    auto* syms = xcb_get_keyboard_mapping_keysyms(keys.get());
    int count = xcb_get_keyboard_mapping_keysyms_length(keys.get());
    auto code = [&](xcb_keysym_t sym) {
        for (int i = 0; keys->keysyms_per_keycode && i < count; ++i)
            if (syms[i] == sym)
                return std::uint8_t(setup->min_keycode + i / keys->keysyms_per_keycode);
        throw Error("Paste shortcut unavailable");
    };
    auto control = code(0xffe3), shift = code(0xffe1), v = code(0x76);
    auto root = x11::screen(c, number)->root;
    std::vector<std::uint8_t> pressed;
    std::exception_ptr result;
    try {
        for (auto k : terminal ? std::vector{control, shift, v} : std::vector{control, v}) {
            x11::check(c, xcb_test_fake_input_checked(c, XCB_KEY_PRESS, k, 0, root, 0, 0, 0));
            pressed.push_back(k);
        }
        xcb_flush(c);
        std::this_thread::sleep_for(100ms);
    } catch (...) {
        result = std::current_exception();
    }
    for (auto k = pressed.rbegin(); k != pressed.rend(); ++k)
        xcb_test_fake_input(c, XCB_KEY_RELEASE, *k, 0, root, 0, 0, 0);
    if (xcb_flush(c) <= 0) x11::fail(nullptr);
    if (result) std::rethrow_exception(result);
}

// GNOME has no data-control protocol; its XWayland selections are shared instead.
bool x11() {
    static const bool x11 = lacks_data_control();
    return x11;
}

std::set<std::string> formats(Clipboard clipboard) {
    if (!is_wayland()) {
        auto formats = x11_clipboard::available_formats();
        return {formats.begin(), formats.end()};
    }
    if (x11()) return xselection::formats(clipboard == Clipboard::Primary);
    try {
        return data_control::mime_types(clipboard);
    } catch (const data_control::Failure& failure) {
        if (failure.kind != Kind::ClipboardEmpty) throw;
        return {};
    }
}

std::string read(Clipboard clipboard, const std::string& format) {
    if (!is_wayland()) return x11_clipboard::get_buffer(format);
    if (x11()) return xselection::read(clipboard == Clipboard::Primary, format);
    struct Pipe {
        int fd;
        ~Pipe() { close(fd); }
    } pipe{data_control::contents(clipboard, format)};
    std::string bytes;
    char buffer[65536];
    auto deadline = Clock::now() + 2s;
    while (bytes.size() <= LIMIT) {
        auto left = std::chrono::ceil<std::chrono::milliseconds>(deadline - Clock::now()).count();
        pollfd ready{pipe.fd, POLLIN, 0};
        int result = left > 0 ? poll(&ready, 1, int(left)) : 0;
        if (result < 0 && errno == EINTR) continue;
        if (result == 0) throw Error("Clipboard read timed out");
        auto n = result < 0
                     ? -1
                     : ::read(pipe.fd, buffer, std::min(sizeof buffer, LIMIT + 1 - bytes.size()));
        if (n < 0 && errno == EINTR) continue;
        if (n < 0) throw Error(std::strerror(errno));
        if (n == 0) return bytes;
        bytes.append(buffer, std::size_t(n));
    }
    throw Error("Clipboard too large");
}

Data snapshot(Clipboard clipboard) {
    auto initial = formats(clipboard);
    Data data;
    std::size_t total = 0;
    for (auto& format : initial) {
        auto bytes = read(clipboard, format);
        total += bytes.size();
        if (total > LIMIT) throw Error("Clipboard too large");
        data.emplace(format, std::move(bytes));
    }
    if (formats(clipboard) != initial) throw Error("Clipboard changed");
    return data;
}

void write(Clipboard clipboard, const Data& data) {
    if (!is_wayland()) {
        x11_clipboard::set(data);
    } else if (x11()) {
        if (clipboard != Clipboard::Primary) xselection::write(false, data);
        if (clipboard != Clipboard::Regular) xselection::write(true, data);
    } else if (data.empty()) {
        data_control::clear(clipboard);
    } else {
        data_control::copy(clipboard, data);
    }
}

// X11 clients, and GNOME's bridge from the X11 clipboard, ask for UTF8_STRING.
Data plain_text(const std::string& text) {
    return {{"text/plain;charset=utf-8", text}, {"UTF8_STRING", text}};
}

} // namespace

bool is_terminal(std::string_view app) {
    auto lower = to_lower_ascii(app);
    for (std::size_t start = 0; start <= lower.size();) {
        auto end = std::min(lower.find('\0', start), lower.size());
        auto part = std::string_view(lower).substr(start, end - start);
        if (part == "st" || part == "st-256color") return true;
        start = end + 1;
    }
    constexpr std::array<std::string_view, 26> TERMINALS{
        "terminal",
        "konsole",
        "kitty",
        "wezterm",
        "alacritty",
        "xterm",
        "ghostty",
        "tilix",
        "terminator",
        "urxvt",
        "rxvt",
        "foot",
        "ptyxis",
        "org.gnome.console",
        "kgx",
        "yakuake",
        "guake",
        "tilda",
        "terminology",
        "termite",
        "termit",
        "contour",
        "com.raggesilver.blackbox",
        "com.system76.cosmicterm",
        "com.mitchellh.ghostty",
        "com.raphaelamorim.rio",
    };
    return std::ranges::any_of(TERMINALS,
                               [&](auto name) { return lower.find(name) != std::string::npos; });
}

bool lacks_data_control() {
    try {
        data_control::mime_types(Clipboard::Regular);
    } catch (const data_control::Failure& failure) {
        return failure.kind == Kind::MissingProtocol;
    } catch (const std::exception&) {
    }
    return false;
}

std::shared_ptr<Target> capture() {
    bool terminal;
    if (is_wayland()) {
        auto app = focus::capture();
        terminal = (app && is_terminal(*app)) || in_terminal(250ms);
    } else {
        terminal = x_terminal() || in_terminal(3s);
    }
    return std::make_shared<Target>(Target{terminal});
}

void paste(const Target& target, std::string text) {
    text = prepare_text(text, target.terminal);
    // An unreadable clipboard keeps the transcript rather than being restored empty.
    std::optional<Data> saved;
    try {
        saved = snapshot(Clipboard::Regular);
    } catch (const std::exception&) {
    }
    // Marks this selection, so a copy made while pasting, even of identical text, is kept.
    constexpr const char* MARKER = "application/x-transcribe";
    auto data = plain_text(text);
    data.emplace(MARKER, "");
    write(Clipboard::Regular, data);
    std::exception_ptr dispatch;
    try {
        try {
            linux_input::paste(target.terminal);
        } catch (const std::exception&) {
            if (is_wayland()) throw;
            x_press(target.terminal);
        }
        // Keep the selection alive while the destination requests it.
        std::this_thread::sleep_for(500ms);
    } catch (...) {
        dispatch = std::current_exception();
    }
    std::exception_ptr restore;
    try {
        if (saved && formats(Clipboard::Regular).contains(MARKER))
            write(Clipboard::Regular, *saved);
    } catch (...) {
        restore = std::current_exception();
    }
    for (auto& error : {dispatch, restore})
        if (error) std::rethrow_exception(error);
}

void copy(const std::string& text) {
    if (!is_wayland()) return x11_clipboard::set({{"UTF8_STRING", text}});
    write(Clipboard::Regular, plain_text(text));
}

void prepare() {}

} // namespace transcribe::insertion::platform
