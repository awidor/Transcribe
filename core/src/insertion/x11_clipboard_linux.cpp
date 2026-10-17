#include "insertion/x11_clipboard.hpp"

#include "insertion/x11.hpp"

#include <algorithm>
#include <chrono>
#include <cstring>
#include <memory>
#include <mutex>
#include <thread>
#include <utility>

namespace transcribe::insertion::x11_clipboard {

namespace {

using namespace std::chrono_literals;

struct Server {
    x11::Connection conn;
    xcb_window_t window;
};

Server server() {
    int number = 0;
    auto conn = x11::connect(number);
    auto window = x11::window(conn.get(), number, XCB_WINDOW_CLASS_INPUT_OUTPUT,
                              x11::screen(conn.get(), number)->root_visual,
                              XCB_EVENT_MASK_STRUCTURE_NOTIFY | XCB_EVENT_MASK_PROPERTY_CHANGE);
    return {std::move(conn), window};
}

struct Inner {
    // Reads and the serving thread use separate connections, as clipboard-rs does.
    Server reader = server();
    Server writer = server();
    xcb_atom_t clipboard = atom("CLIPBOARD"), property = atom("PROPERTY"),
               targets = atom("TARGETS"), save_targets = atom("SAVE_TARGETS"), incr = atom("INCR"),
               timestamp = atom("TIMESTAMP"), multiple = atom("MULTIPLE");
    std::mutex mutex;
    std::vector<std::pair<xcb_atom_t, std::string>> data;

    xcb_atom_t atom(std::string_view name) { return x11::atom(reader.conn.get(), name); }

    void serve() {
        auto* c = writer.conn.get();
        while (x11::Reply<xcb_generic_event_t> event{xcb_wait_for_event(c)}) {
            auto type = event->response_type & 0x7f;
            if (type == XCB_DESTROY_NOTIFY) return;
            if (type == XCB_SELECTION_CLEAR &&
                reinterpret_cast<xcb_selection_clear_event_t*>(event.get())->selection ==
                    clipboard) {
                std::lock_guard lock(mutex);
                data.clear();
            }
            if (type == XCB_SELECTION_REQUEST)
                answer(*reinterpret_cast<xcb_selection_request_event_t*>(event.get()));
        }
    }

    void answer(const xcb_selection_request_event_t& request) {
        auto* c = writer.conn.get();
        bool sent = false;
        {
            std::lock_guard lock(mutex);
            if (request.target == targets) {
                std::vector<xcb_atom_t> atoms{targets, save_targets};
                for (auto& [format, bytes] : data)
                    atoms.push_back(format);
                xcb_change_property(c, XCB_PROP_MODE_REPLACE, request.requestor, request.property,
                                    XCB_ATOM_ATOM, 32, atoms.size(), atoms.data());
                sent = true;
            } else if (auto format = std::ranges::find_if(
                           data, [&](auto& entry) { return entry.first == request.target; });
                       format != data.end()) {
                if (!x11::fits(c, format->second.size())) return;
                xcb_change_property(c, XCB_PROP_MODE_REPLACE, request.requestor, request.property,
                                    request.target, 8, format->second.size(),
                                    format->second.data());
                sent = true;
            }
        }
        x11::notify(c, request, sent ? request.property : XCB_NONE);
    }

    std::string read(xcb_atom_t target) {
        auto* c = reader.conn.get();
        auto cookie = xcb_convert_selection_checked(c, reader.window, clipboard, target, property,
                                                    XCB_CURRENT_TIME);
        x11::check(c, cookie);
        auto get = [&](bool remove, xcb_atom_t type, std::uint32_t length) {
            return x11::get(xcb_get_property_reply, c,
                            xcb_get_property(c, remove, reader.window, property, type, 0, length));
        };
        std::string buffer;
        bool incremental = false;
        auto start = std::chrono::steady_clock::now();
        for (bool done = false; !done;) {
            if (std::chrono::steady_clock::now() - start >= 500ms)
                throw Error("Timeout while waiting for clipboard data");
            x11::Reply<xcb_generic_event_t> event{xcb_poll_for_event(c)};
            if (!event) {
                if (xcb_connection_has_error(c)) x11::fail(nullptr);
                std::this_thread::sleep_for(50ms);
                continue;
            }
            if (event->full_sequence < cookie.sequence) continue;
            auto type = event->response_type & 0x7f;
            if (type == XCB_SELECTION_NOTIFY) {
                auto* notify = reinterpret_cast<xcb_selection_notify_event_t*>(event.get());
                if (notify->selection != clipboard) continue;
                auto reply = x11::get(
                    xcb_get_property_reply, c,
                    xcb_get_property(c, 0, notify->requestor, notify->property,
                                     target == targets ? xcb_atom_t(XCB_ATOM_ATOM) : target, 0,
                                     UINT32_MAX));
                if (reply->type == incr) {
                    x11::check(c, xcb_delete_property_checked(c, reader.window, property));
                    incremental = true;
                    continue;
                }
                if (reply->type != target && reply->type != XCB_ATOM_ATOM)
                    throw Error("Clipboard data type mismatch");
                buffer += x11::value(reply.get());
                done = true;
            } else if (type == XCB_PROPERTY_NOTIFY && incremental &&
                       reinterpret_cast<xcb_property_notify_event_t*>(event.get())->state ==
                           XCB_PROPERTY_NEW_VALUE) {
                auto length = get(false, XCB_ATOM_ATOM, 0)->bytes_after;
                auto chunk = get(true, XCB_GET_PROPERTY_TYPE_ANY, length);
                if (chunk->type != target) continue;
                if (xcb_get_property_value_length(chunk.get()) == 0) done = true;
                buffer += x11::value(chunk.get());
            }
        }
        x11::check(c, xcb_delete_property_checked(c, reader.window, property));
        return buffer;
    }
};

// Never destroyed: its thread serves the clipboard for the whole process.
Inner& inner() {
    static Inner& inner = []() -> Inner& {
        auto* created = new Inner;
        std::thread([created] { created->serve(); }).detach();
        return *created;
    }();
    return inner;
}

} // namespace

std::vector<std::string> available_formats() {
    auto& i = inner();
    auto* c = i.reader.conn.get();
    auto data = i.read(i.targets);
    std::vector<std::string> formats;
    for (std::size_t k = 0; k + 4 <= data.size(); k += 4) {
        xcb_atom_t atom;
        std::memcpy(&atom, data.data() + k, 4);
        if (atom == i.timestamp || atom == i.multiple || atom == i.targets ||
            atom == i.save_targets)
            continue;
        x11::Reply<xcb_get_atom_name_reply_t> name{
            xcb_get_atom_name_reply(c, xcb_get_atom_name(c, atom), nullptr)};
        formats.push_back(name ? encode_utf8(decode_utf8(
                                     {xcb_get_atom_name_name(name.get()),
                                      std::size_t(xcb_get_atom_name_name_length(name.get()))}))
                               : "Unknown");
    }
    return formats;
}

std::string get_buffer(const std::string& format) {
    auto& i = inner();
    return i.read(i.atom(format));
}

void set(const std::map<std::string, std::string>& contents) {
    auto& i = inner();
    auto* c = i.writer.conn.get();
    std::vector<std::pair<xcb_atom_t, std::string>> data;
    for (auto& [format, bytes] : contents)
        data.emplace_back(x11::atom(c, format), bytes);
    {
        std::lock_guard lock(i.mutex);
        i.data = std::move(data);
    }
    x11::check(c,
               xcb_set_selection_owner_checked(c, i.writer.window, i.clipboard, XCB_CURRENT_TIME));
    x11::Reply<xcb_get_selection_owner_reply_t> owner{
        xcb_get_selection_owner_reply(c, xcb_get_selection_owner(c, i.clipboard), nullptr)};
    if (!owner || owner->owner != i.writer.window)
        throw Error("Failed to take ownership of the clipboard");
}

} // namespace transcribe::insertion::x11_clipboard
