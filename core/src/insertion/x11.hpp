#pragma once

#include "transcribe/util.hpp"

#include <xcb/xcb.h>

#include <cstdint>
#include <cstdlib>
#include <memory>
#include <string>
#include <string_view>

// Throwing helpers over xcb, in place of x11rb's checked requests and replies.
namespace transcribe::insertion::x11 {

struct Free {
    void operator()(void* pointer) const { std::free(pointer); }
};
template <class T> using Reply = std::unique_ptr<T, Free>;

struct Disconnect {
    void operator()(xcb_connection_t* connection) const { xcb_disconnect(connection); }
};
using Connection = std::unique_ptr<xcb_connection_t, Disconnect>;

[[noreturn]] inline void fail(xcb_generic_error_t* error) {
    if (!error) throw Error("X11 connection error");
    auto code = error->error_code;
    std::free(error);
    throw Error("X11 error " + std::to_string(code));
}

inline Connection connect(int& screen) {
    Connection c(xcb_connect(nullptr, &screen));
    if (xcb_connection_has_error(c.get())) fail(nullptr);
    return c;
}

inline xcb_screen_t* screen(xcb_connection_t* c, int number) {
    auto it = xcb_setup_roots_iterator(xcb_get_setup(c));
    for (; number > 0; --number)
        xcb_screen_next(&it);
    return it.data;
}

template <class R, class C>
Reply<R> get(R* (*fetch)(xcb_connection_t*, C, xcb_generic_error_t**), xcb_connection_t* c,
             C cookie) {
    xcb_generic_error_t* error = nullptr;
    Reply<R> reply(fetch(c, cookie, &error));
    if (!reply) fail(error);
    return reply;
}

inline void check(xcb_connection_t* c, xcb_void_cookie_t cookie) {
    if (auto* error = xcb_request_check(c, cookie)) fail(error);
    if (xcb_connection_has_error(c)) fail(nullptr);
}

inline xcb_atom_t atom(xcb_connection_t* c, std::string_view name) {
    return get(xcb_intern_atom_reply, c,
               xcb_intern_atom(c, 0, std::uint16_t(name.size()), name.data()))
        ->atom;
}

inline xcb_window_t window(xcb_connection_t* c, int number, std::uint16_t window_class,
                           xcb_visualid_t visual, std::uint32_t events) {
    auto id = xcb_generate_id(c);
    check(c, xcb_create_window_checked(c, XCB_COPY_FROM_PARENT, id, screen(c, number)->root, 0, 0,
                                       1, 1, 0, window_class, visual, XCB_CW_EVENT_MASK, &events));
    return id;
}

inline std::string value(const xcb_get_property_reply_t* reply) {
    return {static_cast<const char*>(xcb_get_property_value(reply)),
            std::size_t(xcb_get_property_value_length(reply))};
}

inline xcb_window_t owner(xcb_connection_t* c, xcb_atom_t selection) {
    return get(xcb_get_selection_owner_reply, c, xcb_get_selection_owner(c, selection))->owner;
}

// The requestor of a selection hears back once its target is written, or refused.
inline void notify(xcb_connection_t* c, const xcb_selection_request_event_t& request,
                   xcb_atom_t property) {
    xcb_selection_notify_event_t event{};
    event.response_type = XCB_SELECTION_NOTIFY;
    event.time = request.time;
    event.requestor = request.requestor;
    event.selection = request.selection;
    event.target = request.target;
    event.property = property;
    xcb_send_event(c, 0, request.requestor, XCB_EVENT_MASK_NO_EVENT,
                   reinterpret_cast<const char*>(&event));
    xcb_flush(c);
}

// x11rb refuses a request over the server's limit; xcb would drop the connection instead.
inline bool fits(xcb_connection_t* c, std::size_t bytes) {
    return bytes + 28 <= std::size_t(xcb_get_maximum_request_length(c)) * 4;
}

} // namespace transcribe::insertion::x11
