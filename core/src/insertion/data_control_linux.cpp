#include "insertion/data_control.hpp"

#include "ext-data-control-v1-client-protocol.h"

#include <fcntl.h>
#include <pthread.h>
#include <signal.h>
#include <unistd.h>
#include <wayland-client.h>

#include <algorithm>
#include <cerrno>
#include <cstring>
#include <future>
#include <list>
#include <memory>
#include <string_view>
#include <thread>
#include <utility>
#include <vector>

namespace transcribe::insertion::data_control {

namespace {

struct Session;

struct Seat {
    Session* session;
    wl_seat* seat;
    ext_data_control_device_v1* device = nullptr;
    ext_data_control_offer_v1* offer = nullptr;
    ext_data_control_offer_v1* primary_offer = nullptr;
};

struct Global {
    std::uint32_t name;
    std::string interface;
    std::uint32_t version;
};

// One connection, as each wl-clipboard-rs call makes its own.
struct Session {
    wl_display* display = nullptr;
    wl_registry* registry = nullptr;
    std::vector<Global> globals;
    ext_data_control_manager_v1* manager = nullptr;
    std::list<Seat> seats;
    bool got_primary_selection = false;
    // Reading collects every offer's MIME types; copying refuses offers instead.
    bool copying = false;
    std::map<ext_data_control_offer_v1*, std::vector<std::string>> offers;
    std::map<std::string, std::string> data;
    std::set<ext_data_control_source_v1*> sources;
    bool should_quit = false;

    ~Session() {
        if (!display) return;
        for (auto& [offer, types] : offers)
            wl_proxy_destroy((wl_proxy*)offer);
        for (auto* source : sources)
            wl_proxy_destroy((wl_proxy*)source);
        for (auto& seat : seats) {
            if (seat.device) wl_proxy_destroy((wl_proxy*)seat.device);
            wl_proxy_destroy((wl_proxy*)seat.seat);
        }
        if (manager) wl_proxy_destroy((wl_proxy*)manager);
        if (registry) wl_proxy_destroy((wl_proxy*)registry);
        wl_display_disconnect(display);
    }

    void roundtrip() {
        if (wl_display_roundtrip(display) < 0)
            throw Failure(Kind::Other, "Wayland compositor communication error");
    }

    void replace(ext_data_control_offer_v1*& slot, ext_data_control_offer_v1* offer) {
        auto* old = std::exchange(slot, offer);
        if (old && offers.erase(old)) ext_data_control_offer_v1_destroy(old);
    }

    void destroy(ext_data_control_source_v1* source) {
        if (sources.erase(source)) ext_data_control_source_v1_destroy(source);
    }
};

const wl_registry_listener REGISTRY = {
    .global =
        [](void* data, wl_registry*, std::uint32_t name, const char* interface,
           std::uint32_t version) {
            static_cast<Session*>(data)->globals.push_back({name, interface, version});
        },
    .global_remove = [](void*, wl_registry*, std::uint32_t) {},
};

const wl_seat_listener SEAT = {
    .capabilities = [](void*, wl_seat*, std::uint32_t) {},
    .name = [](void*, wl_seat*, const char*) {},
};

const ext_data_control_offer_v1_listener OFFER = {
    .offer =
        [](void* data, ext_data_control_offer_v1* offer, const char* mime_type) {
            auto& offers = static_cast<Session*>(data)->offers;
            if (auto it = offers.find(offer); it != offers.end()) it->second.push_back(mime_type);
        },
};

const ext_data_control_device_v1_listener DEVICE = {
    .data_offer =
        [](void* data, ext_data_control_device_v1*, ext_data_control_offer_v1* offer) {
            auto* session = static_cast<Seat*>(data)->session;
            if (session->copying) return ext_data_control_offer_v1_destroy(offer);
            session->offers[offer];
            ext_data_control_offer_v1_add_listener(offer, &OFFER, session);
        },
    .selection =
        [](void* data, ext_data_control_device_v1*, ext_data_control_offer_v1* offer) {
            auto* seat = static_cast<Seat*>(data);
            if (!seat->session->copying) seat->session->replace(seat->offer, offer);
        },
    .finished =
        [](void* data, ext_data_control_device_v1*) {
            auto* seat = static_cast<Seat*>(data);
            ext_data_control_device_v1_destroy(std::exchange(seat->device, nullptr));
        },
    .primary_selection =
        [](void* data, ext_data_control_device_v1*, ext_data_control_offer_v1* offer) {
            auto* seat = static_cast<Seat*>(data);
            seat->session->got_primary_selection = true;
            if (!seat->session->copying) seat->session->replace(seat->primary_offer, offer);
        },
};

bool write_all(int fd, std::string_view bytes) {
    while (!bytes.empty()) {
        auto written = ::write(fd, bytes.data(), bytes.size());
        if (written < 0 && errno == EINTR) continue;
        if (written < 0) return false;
        bytes.remove_prefix(std::size_t(written));
    }
    return true;
}

const ext_data_control_source_v1_listener SOURCE = {
    .send =
        [](void* data, ext_data_control_source_v1* source, const char* mime_type, std::int32_t fd) {
            auto* session = static_cast<Session*>(data);
            // Another source already failed to serve a request.
            if (session->should_quit) {
                close(fd);
                return session->destroy(source);
            }
            auto it = session->data.find(mime_type);
            // Clear O_NONBLOCK, otherwise the copy stops halfway.
            bool failed = it != session->data.end() &&
                          (fcntl(fd, F_SETFL, 0) < 0 || !write_all(fd, it->second));
            close(fd);
            if (failed) {
                session->should_quit = true;
                session->destroy(source);
            }
        },
    .cancelled =
        [](void* data, ext_data_control_source_v1* source) {
            static_cast<Session*>(data)->destroy(source);
        },
};

// Connects, binds data control and every seat's device, and receives the current offers.
std::unique_ptr<Session> open(bool primary, bool copying) {
    auto session = std::make_unique<Session>();
    auto& s = *session;
    s.copying = copying;
    s.display = wl_display_connect(nullptr);
    if (!s.display) throw Failure(Kind::Other, "Couldn't connect to the Wayland compositor");
    s.registry = wl_display_get_registry(s.display);
    wl_registry_add_listener(s.registry, &REGISTRY, &s);
    s.roundtrip();
    auto find = [&](std::string_view interface) {
        auto it = std::ranges::find(s.globals, interface, &Global::interface);
        return it == s.globals.end() ? nullptr : &*it;
    };
    std::uint32_t wlr_version = primary ? 2 : 1;
    if (auto* ext = find(ext_data_control_manager_v1_interface.name)) {
        s.manager = static_cast<ext_data_control_manager_v1*>(
            wl_registry_bind(s.registry, ext->name, &ext_data_control_manager_v1_interface, 1));
    } else if (auto* wlr = find("zwlr_data_control_manager_v1");
               wlr && wlr->version >= wlr_version) {
        // ext-data-control-v1 is wlr-data-control v2 renamed, request for request and event
        // for event, so the wlr global is bound by its own name and driven like ext.
        s.manager = reinterpret_cast<ext_data_control_manager_v1*>(wl_proxy_marshal_flags(
            (wl_proxy*)s.registry, WL_REGISTRY_BIND, &ext_data_control_manager_v1_interface,
            wlr_version, 0, wlr->name, "zwlr_data_control_manager_v1", wlr_version, nullptr));
    }
    if (!s.manager)
        throw Failure(
            Kind::MissingProtocol,
            "A required Wayland protocol (ext-data-control, or wlr-data-control version " +
                std::to_string(wlr_version) + ") is not supported by the compositor");
    for (auto& global : s.globals) {
        if (global.interface != wl_seat_interface.name || global.version < 2) continue;
        auto* seat =
            static_cast<wl_seat*>(wl_registry_bind(s.registry, global.name, &wl_seat_interface, 2));
        wl_seat_add_listener(seat, &SEAT, nullptr);
        s.seats.push_back({&s, seat});
    }
    if (s.seats.empty()) throw Failure(Kind::Other, "There are no seats");
    for (auto& seat : s.seats) {
        seat.device = ext_data_control_manager_v1_get_data_device(s.manager, seat.seat);
        ext_data_control_device_v1_add_listener(seat.device, &DEVICE, &seat);
    }
    s.roundtrip();
    if (primary && !s.got_primary_selection)
        throw Failure(Kind::PrimarySelectionUnsupported,
                      "The compositor does not support primary selection");
    return session;
}

std::pair<std::unique_ptr<Session>, ext_data_control_offer_v1*> offer(Clipboard clipboard) {
    bool primary = clipboard == Clipboard::Primary;
    auto session = open(primary, false);
    auto& seat = session->seats.front();
    auto* offer = primary ? seat.primary_offer : seat.offer;
    if (!offer) throw Failure(Kind::ClipboardEmpty, "The clipboard of the requested seat is empty");
    return {std::move(session), offer};
}

std::unique_ptr<Session> devices(Clipboard clipboard) {
    auto session = open(clipboard != Clipboard::Regular, true);
    if (std::ranges::none_of(session->seats, &Seat::device))
        throw Failure(Kind::Other, "The requested seat was not found");
    return session;
}

bool is_text(std::string_view mime_type) {
    return mime_type == "TEXT" || mime_type == "STRING" || mime_type == "UTF8_STRING" ||
           mime_type.starts_with("text/") || mime_type.find("json") != std::string_view::npos ||
           mime_type.ends_with("script") || mime_type.ends_with("xml") ||
           mime_type.ends_with("yaml") || mime_type.ends_with("csv") || mime_type.ends_with("ini");
}

} // namespace

std::set<std::string> mime_types(Clipboard clipboard) {
    auto [session, offer] = data_control::offer(clipboard);
    auto& types = session->offers[offer];
    return {types.begin(), types.end()};
}

int contents(Clipboard clipboard, const std::string& mime_type) {
    auto [session, offer] = data_control::offer(clipboard);
    if (std::ranges::find(session->offers[offer], mime_type) == session->offers[offer].end())
        throw Failure(Kind::Other, "No suitable type of content copied");
    int pipe[2];
    if (pipe2(pipe, O_CLOEXEC) < 0)
        throw Failure(Kind::Other, "Couldn't create a pipe for content transfer");
    ext_data_control_offer_v1_receive(offer, mime_type.c_str(), pipe[1]);
    close(pipe[1]);
    // A flush is not enough: the compositor may see the disconnect before the request.
    try {
        session->roundtrip();
    } catch (...) {
        close(pipe[0]);
        throw;
    }
    return pipe[0];
}

void copy(Clipboard clipboard, const std::map<std::string, std::string>& data) {
    std::promise<void> prepared;
    auto ready = prepared.get_future();
    std::thread([clipboard, data = data, prepared = std::move(prepared)]() mutable {
        // Rust ignores SIGPIPE; a paste target closing early must not end the process.
        sigset_t pipe;
        sigemptyset(&pipe);
        sigaddset(&pipe, SIGPIPE);
        pthread_sigmask(SIG_BLOCK, &pipe, nullptr);
        std::unique_ptr<Session> session;
        try {
            session = devices(clipboard);
            auto& s = *session;
            s.data = std::move(data);
            auto text =
                std::ranges::find_if(s.data, [](auto& entry) { return is_text(entry.first); });
            if (text != s.data.end()) {
                auto bytes = text->second;
                for (auto* type :
                     {"text/plain;charset=utf-8", "text/plain", "STRING", "UTF8_STRING", "TEXT"})
                    s.data.emplace(type, bytes);
            }
            for (auto& seat : s.seats) {
                if (!seat.device) continue;
                for (bool primary : {false, true}) {
                    if (primary ? clipboard == Clipboard::Regular : clipboard == Clipboard::Primary)
                        continue;
                    auto* source = ext_data_control_manager_v1_create_data_source(s.manager);
                    ext_data_control_source_v1_add_listener(source, &SOURCE, &s);
                    s.sources.insert(source);
                    for (auto& [type, bytes] : s.data)
                        ext_data_control_source_v1_offer(source, type.c_str());
                    if (primary)
                        ext_data_control_device_v1_set_primary_selection(seat.device, source);
                    else
                        ext_data_control_device_v1_set_selection(seat.device, source);
                }
            }
            wl_display_flush(s.display);
            prepared.set_value();
        } catch (...) {
            prepared.set_exception(std::current_exception());
            return;
        }
        while (!session->should_quit && !session->sources.empty())
            if (wl_display_dispatch(session->display) < 0) break;
    }).detach();
    ready.get();
}

void clear(Clipboard clipboard) {
    auto session = devices(clipboard);
    for (auto& seat : session->seats) {
        if (!seat.device) continue;
        if (clipboard != Clipboard::Regular)
            ext_data_control_device_v1_set_primary_selection(seat.device, nullptr);
        if (clipboard != Clipboard::Primary)
            ext_data_control_device_v1_set_selection(seat.device, nullptr);
    }
    session->roundtrip();
}

} // namespace transcribe::insertion::data_control
