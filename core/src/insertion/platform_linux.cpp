#include "insertion/data_control.hpp"
#include "insertion/destination.hpp"
#include "insertion/linux.hpp"
#include "insertion/x11.hpp"
#include "insertion/x11_clipboard.hpp"
#include "insertion/xselection.hpp"
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
#include <tuple>
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

// Nothing when the focused element cannot be found within `timeout`.
std::optional<Accessible> focused(Clock::duration timeout) {
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
    if (!session) return std::nullopt;
    auto address = call(session.get(), "org.a11y.Bus", "/org/a11y/bus", "org.a11y.Bus",
                        "GetAddress", nullptr, "(s)");
    if (!address) return std::nullopt;
    Owned<GDBusConnection, close_connection> c(g_dbus_connection_new_for_address_sync(
        text(address.get()).c_str(),
        GDBusConnectionFlags(G_DBUS_CONNECTION_FLAGS_AUTHENTICATION_CLIENT |
                             G_DBUS_CONNECTION_FLAGS_MESSAGE_BUS_CONNECTION),
        nullptr, cancel.get(), nullptr));
    if (!c) return std::nullopt;

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
    if (!apps) return std::nullopt;
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
        if (state & (1 << 12))
            return Accessible{bus, path, terminal, role, (state & (1 << 7)) != 0};
        if (depth < 30)
            if (auto children = node(ACCESSIBLE, "GetChildren", nullptr, "(a(so))"))
                extend(children.get(), depth + 1, terminal);
    }
    return std::nullopt;
}

std::tuple<std::uint32_t, std::uint32_t, bool> x_target() {
    int number = 0;
    auto conn = x11::connect(number);
    auto* c = conn.get();
    auto active =
        x11::get(xcb_get_property_reply, c,
                 xcb_get_property(c, 0, x11::screen(c, number)->root,
                                  x11::atom(c, "_NET_ACTIVE_WINDOW"), XCB_ATOM_WINDOW, 0, 1));
    if (active->format != 32 || xcb_get_property_value_length(active.get()) < 4)
        throw Error("Destination unavailable");
    auto window = *static_cast<const std::uint32_t*>(xcb_get_property_value(active.get()));
    auto focus = x11::get(xcb_get_input_focus_reply, c, xcb_get_input_focus(c))->focus;
    auto wm_class =
        x11::get(xcb_get_property_reply, c,
                 xcb_get_property(c, 0, window, XCB_ATOM_WM_CLASS, XCB_ATOM_STRING, 0, 1024));
    return {window, focus, is_terminal(x11::value(wm_class.get()))};
}

bool valid(const Target& target) {
    if (target.window && !target.window->valid()) return false;
    if (target.accessible && focused(3s) != target.accessible) return false;
    if (target.x11) {
        try {
            auto [window, focus, terminal] = x_target();
            return target.x11 == std::pair(window, focus);
        } catch (const Error&) {
            return false;
        }
    }
    // The compositor routes synthetic keys to current focus when neither
    // shared protocol exposes a destination. Never require a desktop adapter.
    return true;
}

void x_paste(const Target& target, const std::string& text) {
    int number = 0;
    auto conn = x11::connect(number);
    auto* c = conn.get();
    auto atom = x11::atom(c, "CLIPBOARD");
    auto initial = x11::owner(c, atom);
    Data old;
    std::size_t size = 0;
    if (initial != XCB_NONE) {
        for (auto& format : x11_clipboard::available_formats()) {
            auto data = x11_clipboard::get_buffer(format);
            size += data.size();
            if (size > LIMIT) throw Error("Clipboard too large");
            old.emplace(format, std::move(data));
        }
    }
    if (x11::owner(c, atom) != initial) throw Error("Clipboard changed");
    auto unchanged = [&] {
        auto [window, focus, terminal] = x_target();
        return target.x11 == std::pair(window, focus);
    };
    if (!unchanged()) throw Error("Destination changed");
    auto* setup = xcb_get_setup(c);
    auto keys = x11::get(xcb_get_keyboard_mapping_reply, c,
                         xcb_get_keyboard_mapping(c, setup->min_keycode,
                                                  setup->max_keycode - setup->min_keycode + 1));
    auto* syms = xcb_get_keyboard_mapping_keysyms(keys.get());
    int count = xcb_get_keyboard_mapping_keysyms_length(keys.get());
    auto key = [&](xcb_keysym_t sym) -> std::optional<std::uint8_t> {
        for (int i = 0; keys->keysyms_per_keycode && i < count; ++i)
            if (syms[i] == sym)
                return std::uint8_t(setup->min_keycode + i / keys->keysyms_per_keycode);
        return std::nullopt;
    };
    auto code = [&](xcb_keysym_t sym) {
        if (auto found = key(sym)) return *found;
        throw Error("Paste shortcut unavailable");
    };
    auto control = code(0xffe3), shift = code(0xffe1), v = code(0x76);
    auto deadline = Clock::now() + 2s;
    for (;;) {
        auto state = x11::get(xcb_query_keymap_reply, c, xcb_query_keymap(c));
        bool held = false;
        for (xcb_keysym_t modifier :
             {0xffe1, 0xffe2, 0xffe3, 0xffe4, 0xffe9, 0xffea, 0xffeb, 0xffec})
            if (auto k = key(modifier)) held = held || (state->keys[*k / 8] & (1 << (*k % 8)));
        if (!held) break;
        if (Clock::now() >= deadline) throw Error("Shortcut still held");
        std::this_thread::sleep_for(20ms);
    }
    x11_clipboard::set({{"UTF8_STRING", text}});
    auto owner = x11::owner(c, atom);
    std::exception_ptr dispatch;
    try {
        if (!unchanged()) throw Error("Destination changed");
        auto root = x11::screen(c, number)->root;
        std::vector<std::uint8_t> pressed;
        std::exception_ptr result;
        try {
            for (auto k :
                 target.terminal ? std::vector{control, shift, v} : std::vector{control, v}) {
                x11::check(c, xcb_test_fake_input_checked(c, XCB_KEY_PRESS, k, 0, root, 0, 0, 0));
                pressed.push_back(k);
            }
        } catch (...) {
            result = std::current_exception();
        }
        for (auto k = pressed.rbegin(); k != pressed.rend(); ++k)
            xcb_test_fake_input(c, XCB_KEY_RELEASE, *k, 0, root, 0, 0, 0);
        if (xcb_flush(c) <= 0) x11::fail(nullptr);
        if (result) std::rethrow_exception(result);
        std::this_thread::sleep_for(1200ms);
    } catch (...) {
        dispatch = std::current_exception();
    }
    if (x11::owner(c, atom) == owner) x11_clipboard::set(old);
    if (dispatch) std::rethrow_exception(dispatch);
}

// GNOME has no data-control protocol; its XWayland selections are shared instead.
bool x11() {
    static const bool x11 = lacks_data_control();
    return x11;
}

std::set<std::string> formats(Clipboard clipboard) {
    if (x11()) return xselection::formats(clipboard == Clipboard::Primary);
    try {
        return data_control::mime_types(clipboard);
    } catch (const data_control::Failure& failure) {
        if (failure.kind != Kind::ClipboardEmpty) throw;
        return {};
    }
}

std::string read(Clipboard clipboard, const std::string& format) {
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
    if (x11()) {
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

void wayland_paste(const Target& target, const std::string& text) {
    auto saved = snapshot(Clipboard::Regular);
    // Shift+Insert is also supported by traditional terminals that pass
    // Ctrl+Shift+V through to the running program. Some paste PRIMARY, others
    // CLIPBOARD, so offer the same text on both without changing user settings.
    std::optional<Data> primary;
    if (target.terminal) {
        try {
            primary = snapshot(Clipboard::Primary);
        } catch (const data_control::Failure& failure) {
            if (failure.kind != Kind::PrimarySelectionUnsupported) throw;
        }
    }
    linux_input::wait_released();
    if (!valid(target)) throw Error("Destination changed");
    if (snapshot(Clipboard::Regular) != saved) throw Error("Clipboard changed");
    if (primary && snapshot(Clipboard::Primary) != *primary) throw Error("Selection changed");
    // A unique MIME marker identifies this selection, even if the user copies
    // identical text while delivery is in progress. Never overwrite a new copy.
    auto marker = "application/x-transcribe-" + uuid_v4();
    auto data = plain_text(text);
    data.emplace(marker, "");
    write(primary ? Clipboard::Both : Clipboard::Regular, data);
    std::exception_ptr dispatch;
    try {
        if (!valid(target)) throw Error("Destination changed");
        linux_input::paste(shortcut(target, primary.has_value()));
        // Keep the selection alive while native and XWayland editors request it.
        std::this_thread::sleep_for(1200ms);
    } catch (...) {
        dispatch = std::current_exception();
    }
    // Restore each selection independently; a new copy or mouse selection must
    // survive even if the other selection is still ours. Attempt both cleanups.
    auto restore = [&](Clipboard clipboard, const Data& saved) -> std::exception_ptr {
        try {
            if (formats(clipboard).contains(marker)) write(clipboard, saved);
        } catch (...) {
            return std::current_exception();
        }
        return nullptr;
    };
    auto regular = restore(Clipboard::Regular, saved);
    auto selection = primary ? restore(Clipboard::Primary, *primary) : nullptr;
    for (auto& error : {regular, selection, dispatch})
        if (error) std::rethrow_exception(error);
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

Target wayland_target(std::optional<focus::Target> window, std::optional<Accessible> accessible) {
    // Without receiver metadata, use the terminal-safe path: Shift+Insert
    // and single-line text. Focus metadata is optional on Wayland.
    bool terminal = (window && is_terminal(window->app_id)) ||
                    (accessible && accessible->terminal) || (!window && !accessible);
    return {std::nullopt, std::move(accessible), terminal, std::move(window)};
}

linux_input::PasteShortcut shortcut(const Target& target, bool primary) {
    bool unknown_receiver = !target.window && !target.accessible;
    if (primary || unknown_receiver) return linux_input::PasteShortcut::TerminalSelection;
    return target.terminal ? linux_input::PasteShortcut::TerminalClipboard
                           : linux_input::PasteShortcut::Clipboard;
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
    Target target;
    if (is_wayland()) {
        linux_input::prepare_keyboard();
        auto window = focus::capture();
        target = wayland_target(std::move(window), focused(250ms));
    } else {
        auto [window, focus, terminal] = x_target();
        auto accessible = focused(3s);
        target = {std::pair(window, focus), accessible,
                  terminal || (accessible && accessible->terminal), std::nullopt};
    }
    auto& accessible = target.accessible;
    auto accepts =
        target.terminal ||
        !(accessible && destination::linux_rejects(accessible->role, accessible->editable));
    // Nothing is sent and the clipboard is untouched; the widget offers the text.
    if (!accepts) throw Error(destination::NO_TEXT_FIELD);
    return std::make_shared<Target>(std::move(target));
}

void paste(const Target& target, std::string text) {
    if (!valid(target)) throw Error("Destination changed");
    text = prepare_text(text, target.terminal);
    if (is_wayland())
        wayland_paste(target, text);
    else
        x_paste(target, text);
}

void copy(const std::string& text) {
    if (!is_wayland()) return x11_clipboard::set({{"UTF8_STRING", text}});
    write(Clipboard::Regular, plain_text(text));
}

void prepare() {}

} // namespace transcribe::insertion::platform
