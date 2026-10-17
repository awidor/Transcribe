#include "insertion/focus.hpp"

#include "transcribe/util.hpp"
#include "wlr-foreign-toplevel-management-unstable-v1-client-protocol.h"

#include <poll.h>
#include <wayland-client.h>

#include <algorithm>
#include <cerrno>
#include <chrono>
#include <cstring>
#include <map>
#include <mutex>
#include <set>
#include <vector>

namespace transcribe::insertion::focus {

// Keep one connection throughout delivery: protocol object IDs are meaningful
// only within that connection, and distinguish even two windows of one app.
struct Tracker {
    struct Window {
        std::string app_id;
        bool active = false;
    };
    struct Toplevel {
        // Proxies are reused once destroyed, so each window gets its own id.
        std::uint64_t id;
        Window committed;
        Window pending;
    };

    explicit Tracker(wl_display* display) : display(display) {}
    ~Tracker() {
        for (auto& [handle, toplevel] : windows)
            wl_proxy_destroy((wl_proxy*)handle);
        for (auto* manager : managers)
            wl_proxy_destroy((wl_proxy*)manager);
        for (auto* callback : callbacks)
            wl_callback_destroy(callback);
        if (registry) wl_registry_destroy(registry);
        wl_display_disconnect(display);
    }

    const Toplevel* focused() const {
        if (!manager) return nullptr;
        const Toplevel* found = nullptr;
        for (auto& [handle, toplevel] : windows) {
            if (!toplevel.committed.active) continue;
            // Ambiguous focus must not choose a receiver arbitrarily.
            if (found) return nullptr;
            found = &toplevel;
        }
        return found;
    }

    void sync();

    wl_display* display;
    wl_registry* registry = nullptr;
    std::vector<zwlr_foreign_toplevel_manager_v1*> managers;
    std::set<wl_callback*> callbacks;
    std::optional<std::uint32_t> manager;
    bool synced = false;
    std::map<zwlr_foreign_toplevel_handle_v1*, Toplevel> windows;
    std::uint64_t next_id = 0;
    std::mutex mutex;
};

namespace {

Tracker::Toplevel* toplevel(void* data, zwlr_foreign_toplevel_handle_v1* handle) {
    auto& windows = static_cast<Tracker*>(data)->windows;
    auto it = windows.find(handle);
    return it == windows.end() ? nullptr : &it->second;
}

const zwlr_foreign_toplevel_handle_v1_listener HANDLE = {
    .title = [](void*, zwlr_foreign_toplevel_handle_v1*, const char*) {},
    .app_id =
        [](void* data, zwlr_foreign_toplevel_handle_v1* handle, const char* app_id) {
            if (auto* window = toplevel(data, handle)) window->pending.app_id = app_id;
        },
    .output_enter = [](void*, zwlr_foreign_toplevel_handle_v1*, wl_output*) {},
    .output_leave = [](void*, zwlr_foreign_toplevel_handle_v1*, wl_output*) {},
    .state =
        [](void* data, zwlr_foreign_toplevel_handle_v1* handle, wl_array* state) {
            auto* window = toplevel(data, handle);
            if (!window) return;
            auto* states = static_cast<const std::uint32_t*>(state->data);
            window->pending.active = std::find(states, states + state->size / 4,
                                               ZWLR_FOREIGN_TOPLEVEL_HANDLE_V1_STATE_ACTIVATED) !=
                                     states + state->size / 4;
        },
    .done =
        [](void* data, zwlr_foreign_toplevel_handle_v1* handle) {
            if (auto* window = toplevel(data, handle)) window->committed = window->pending;
        },
    .closed =
        [](void* data, zwlr_foreign_toplevel_handle_v1* handle) {
            static_cast<Tracker*>(data)->windows.erase(handle);
            zwlr_foreign_toplevel_handle_v1_destroy(handle);
        },
    .parent = [](void*, zwlr_foreign_toplevel_handle_v1*, zwlr_foreign_toplevel_handle_v1*) {},
};

const zwlr_foreign_toplevel_manager_v1_listener MANAGER = {
    .toplevel =
        [](void* data, zwlr_foreign_toplevel_manager_v1*, zwlr_foreign_toplevel_handle_v1* handle) {
            auto* tracker = static_cast<Tracker*>(data);
            tracker->windows.emplace(handle, Tracker::Toplevel{tracker->next_id++, {}, {}});
            zwlr_foreign_toplevel_handle_v1_add_listener(handle, &HANDLE, data);
        },
    .finished =
        [](void* data, zwlr_foreign_toplevel_manager_v1*) {
            static_cast<Tracker*>(data)->manager.reset();
        },
};

const wl_registry_listener REGISTRY = {
    .global =
        [](void* data, wl_registry* registry, std::uint32_t name, const char* interface,
           std::uint32_t version) {
            if (std::strcmp(interface, zwlr_foreign_toplevel_manager_v1_interface.name)) return;
            auto* tracker = static_cast<Tracker*>(data);
            auto* manager = static_cast<zwlr_foreign_toplevel_manager_v1*>(
                wl_registry_bind(registry, name, &zwlr_foreign_toplevel_manager_v1_interface,
                                 std::min(version, 3u)));
            zwlr_foreign_toplevel_manager_v1_add_listener(manager, &MANAGER, data);
            tracker->managers.push_back(manager);
            tracker->manager = name;
        },
    .global_remove =
        [](void* data, wl_registry*, std::uint32_t name) {
            auto* tracker = static_cast<Tracker*>(data);
            if (tracker->manager == name) tracker->manager.reset();
        },
};

const wl_callback_listener CALLBACK = {
    .done =
        [](void* data, wl_callback* callback, std::uint32_t) {
            auto* tracker = static_cast<Tracker*>(data);
            tracker->synced = true;
            tracker->callbacks.erase(callback);
            wl_callback_destroy(callback);
        },
};

} // namespace

void Tracker::sync() {
    auto deadline = std::chrono::steady_clock::now() + std::chrono::milliseconds(250);
    synced = false;
    auto* callback = wl_display_sync(display);
    wl_callback_add_listener(callback, &CALLBACK, this);
    callbacks.insert(callback);
    if (wl_display_flush(display) < 0 && errno != EAGAIN) throw Error(std::strerror(errno));
    for (;;) {
        if (wl_display_dispatch_pending(display) < 0) throw Error(std::strerror(errno));
        if (synced) return;
        if (wl_display_prepare_read(display) != 0) continue;
        auto left = std::chrono::ceil<std::chrono::milliseconds>(deadline -
                                                                 std::chrono::steady_clock::now());
        pollfd ready{wl_display_get_fd(display), POLLIN, 0};
        int result = left.count() > 0 ? poll(&ready, 1, int(left.count())) : 0;
        if (result <= 0) {
            wl_display_cancel_read(display);
            if (result < 0 && errno == EINTR) continue;
            throw Error("Wayland focus lookup timed out");
        }
        if (wl_display_read_events(display) < 0 && errno != EAGAIN)
            throw Error(std::strerror(errno));
    }
}

bool Target::valid() const {
    std::lock_guard lock(tracker->mutex);
    try {
        tracker->sync();
    } catch (const Error&) {
        return false;
    }
    auto* window = tracker->focused();
    return window && window->id == id && window->committed.app_id == app_id;
}

std::optional<Target> capture() {
    auto* display = wl_display_connect(nullptr);
    if (!display) throw Error("Could not find wayland compositor");
    auto tracker = std::make_shared<Tracker>(display);
    tracker->registry = wl_display_get_registry(display);
    wl_registry_add_listener(tracker->registry, &REGISTRY, tracker.get());
    tracker->sync();
    if (!tracker->manager) return std::nullopt;
    // The registry callback binds the manager; a second sync receives windows.
    tracker->sync();
    auto* window = tracker->focused();
    if (!window) throw Error("Focus a destination before pasting");
    return Target{tracker, window->id, window->committed.app_id};
}

} // namespace transcribe::insertion::focus
