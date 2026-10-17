#include "insertion/xselection.hpp"

#include "insertion/x11.hpp"

#include <algorithm>
#include <chrono>
#include <condition_variable>
#include <cstring>
#include <mutex>
#include <optional>
#include <thread>
#include <utility>
#include <vector>

namespace transcribe::insertion::xselection {

namespace {

using namespace std::chrono_literals;
using Clock = std::chrono::steady_clock;

constexpr std::size_t LIMIT = 64 * 1024 * 1024;

struct Offer {
    std::vector<std::pair<xcb_atom_t, std::string>> formats;
    // GNOME reads TARGETS when it takes over an X11 selection for Wayland apps.
    bool adopted = false;
};

struct Owner {
    x11::Connection conn;
    xcb_window_t window;
    xcb_atom_t targets;
    std::mutex mutex;
    std::map<xcb_atom_t, Offer> offers;
    std::condition_variable adopted;

    void serve() {
        auto* c = conn.get();
        while (x11::Reply<xcb_generic_event_t> event{xcb_wait_for_event(c)}) {
            auto type = event->response_type & 0x7f;
            if (type == XCB_SELECTION_REQUEST) {
                answer(*reinterpret_cast<xcb_selection_request_event_t*>(event.get()));
            } else if (type == XCB_SELECTION_CLEAR) {
                // A clear can arrive after this window took the selection back.
                auto selection =
                    reinterpret_cast<xcb_selection_clear_event_t*>(event.get())->selection;
                x11::Reply<xcb_get_selection_owner_reply_t> owner{xcb_get_selection_owner_reply(
                    c, xcb_get_selection_owner(c, selection), nullptr)};
                if (!owner || owner->owner != window) {
                    std::lock_guard lock(mutex);
                    offers.erase(selection);
                }
            }
        }
    }

    void answer(const xcb_selection_request_event_t& request) {
        auto* c = conn.get();
        // Obsolete clients name no property and expect the target's.
        auto property = request.property == XCB_NONE ? request.target : request.property;
        bool sent = false;
        {
            std::lock_guard lock(mutex);
            auto offer = offers.find(request.selection);
            if (offer != offers.end() && request.target == targets) {
                std::vector<xcb_atom_t> atoms{targets};
                for (auto& [format, bytes] : offer->second.formats)
                    atoms.push_back(format);
                xcb_change_property(c, XCB_PROP_MODE_REPLACE, request.requestor, property,
                                    XCB_ATOM_ATOM, 32, atoms.size(), atoms.data());
                offer->second.adopted = sent = true;
                adopted.notify_all();
            } else if (offer != offers.end()) {
                auto& formats = offer->second.formats;
                auto format = std::ranges::find_if(
                    formats, [&](auto& entry) { return entry.first == request.target; });
                if (format != formats.end()) {
                    if (!x11::fits(c, format->second.size())) return;
                    xcb_change_property(c, XCB_PROP_MODE_REPLACE, request.requestor, property,
                                        format->first, 8, format->second.size(),
                                        format->second.data());
                    sent = true;
                }
            }
        }
        x11::notify(c, request, sent ? property : XCB_NONE);
    }
};

struct Selections {
    std::shared_ptr<Owner> owner;
    x11::Connection conn;
    xcb_window_t window;
    xcb_atom_t property, incr, targets, clipboard;

    Selections() {
        int owner_screen = 0, screen = 0;
        auto owner_conn = x11::connect(owner_screen);
        auto owner_window = x11::window(owner_conn.get(), owner_screen, XCB_WINDOW_CLASS_INPUT_ONLY,
                                        XCB_COPY_FROM_PARENT, XCB_EVENT_MASK_NO_EVENT);
        auto owner_targets = x11::atom(owner_conn.get(), "TARGETS");
        owner = std::make_shared<Owner>(std::move(owner_conn), owner_window, owner_targets);
        conn = x11::connect(screen);
        window = x11::window(conn.get(), screen, XCB_WINDOW_CLASS_INPUT_ONLY, XCB_COPY_FROM_PARENT,
                             XCB_EVENT_MASK_PROPERTY_CHANGE);
        property = x11::atom(conn.get(), "TRANSCRIBE_SELECTION");
        incr = x11::atom(conn.get(), "INCR");
        targets = x11::atom(conn.get(), "TARGETS");
        clipboard = x11::atom(conn.get(), "CLIPBOARD");
        std::thread([owner = owner] { owner->serve(); }).detach();
    }

    xcb_atom_t atom(bool primary) const {
        return primary ? xcb_atom_t(XCB_ATOM_PRIMARY) : clipboard;
    }

    x11::Reply<xcb_generic_event_t> event(Clock::time_point deadline) {
        for (;;) {
            if (auto* event = xcb_poll_for_event(conn.get()))
                return x11::Reply<xcb_generic_event_t>(event);
            if (xcb_connection_has_error(conn.get())) x11::fail(nullptr);
            if (Clock::now() >= deadline) throw Error("Clipboard read timed out");
            std::this_thread::sleep_for(5ms);
        }
    }

    std::string convert(xcb_atom_t selection, xcb_atom_t target) {
        auto* c = conn.get();
        // Drop replies to earlier reads that timed out.
        while (x11::Reply<xcb_generic_event_t>(xcb_poll_for_event(c))) {
        }
        xcb_convert_selection(c, window, selection, target, property, XCB_CURRENT_TIME);
        xcb_flush(c);
        auto deadline = Clock::now() + 2s;
        for (;;) {
            auto event = this->event(deadline);
            auto* notify = reinterpret_cast<xcb_selection_notify_event_t*>(event.get());
            if ((event->response_type & 0x7f) != XCB_SELECTION_NOTIFY ||
                notify->selection != selection || notify->target != target)
                continue;
            if (notify->property == XCB_NONE) throw Error("Clipboard format unavailable");
            break;
        }
        auto take = [&] {
            return x11::get(
                xcb_get_property_reply, c,
                xcb_get_property(c, 1, window, property, XCB_GET_PROPERTY_TYPE_ANY, 0, UINT32_MAX));
        };
        auto reply = take();
        if (reply->type != incr) return x11::value(reply.get());
        // Large data arrives in chunks, each after the previous one is deleted.
        std::string data;
        for (;;) {
            auto event = this->event(deadline);
            auto* notify = reinterpret_cast<xcb_property_notify_event_t*>(event.get());
            if ((event->response_type & 0x7f) != XCB_PROPERTY_NOTIFY || notify->atom != property ||
                notify->state != XCB_PROPERTY_NEW_VALUE)
                continue;
            auto chunk = take();
            if (xcb_get_property_value_length(chunk.get()) == 0) return data;
            data += x11::value(chunk.get());
            if (data.size() > LIMIT) throw Error("Clipboard too large");
            deadline = Clock::now() + 2s;
        }
    }
};

// Created once and kept, as its owner window must outlive every offer.
Selections& selections() {
    static Selections selections;
    return selections;
}

} // namespace

std::set<std::string> formats(bool primary) {
    auto& s = selections();
    auto* c = s.conn.get();
    auto selection = s.atom(primary);
    std::set<std::string> formats;
    if (x11::owner(c, selection) == XCB_NONE) return formats;
    auto targets = s.convert(selection, s.targets);
    for (std::size_t i = 0; i + 4 <= targets.size(); i += 4) {
        xcb_atom_t target;
        std::memcpy(&target, targets.data() + i, 4);
        auto reply = x11::get(xcb_get_atom_name_reply, c, xcb_get_atom_name(c, target));
        auto name =
            encode_utf8(decode_utf8({xcb_get_atom_name_name(reply.get()),
                                     std::size_t(xcb_get_atom_name_name_length(reply.get()))}));
        if (name != "TARGETS" && name != "MULTIPLE" && name != "TIMESTAMP" &&
            name != "SAVE_TARGETS")
            formats.insert(name);
    }
    return formats;
}

std::string read(bool primary, const std::string& format) {
    auto& s = selections();
    return s.convert(s.atom(primary), x11::atom(s.conn.get(), format));
}

void write(bool primary, const std::map<std::string, std::string>& data) {
    auto& s = selections();
    auto& owner = *s.owner;
    auto* c = owner.conn.get();
    auto selection = s.atom(primary);
    if (data.empty()) {
        {
            std::lock_guard lock(owner.mutex);
            owner.offers.erase(selection);
        }
        if (x11::owner(c, selection) == owner.window)
            x11::check(c,
                       xcb_set_selection_owner_checked(c, XCB_NONE, selection, XCB_CURRENT_TIME));
        return;
    }
    Offer offer;
    for (auto& [format, bytes] : data)
        offer.formats.emplace_back(x11::atom(c, format), bytes);
    {
        std::lock_guard lock(owner.mutex);
        owner.offers.insert_or_assign(selection, std::move(offer));
    }
    x11::check(c, xcb_set_selection_owner_checked(c, owner.window, selection, XCB_CURRENT_TIME));
    if (x11::owner(c, selection) != owner.window) throw Error("Clipboard unavailable");
    std::unique_lock lock(owner.mutex);
    auto adopted = [&] {
        auto it = owner.offers.find(selection);
        return it == owner.offers.end() ? std::optional<bool>() : it->second.adopted;
    };
    owner.adopted.wait_for(lock, 1s, [&] { return adopted() != false; });
    if (adopted() != true) throw Error("Clipboard unavailable");
}

} // namespace transcribe::insertion::xselection
