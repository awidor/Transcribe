#include "insertion/data_control.hpp"
#include "insertion/linux.hpp"
#include "transcribe/linux_input.hpp"

#include <gtest/gtest.h>
#include <sys/socket.h>
#include <wayland-server.h>

#include <cstdlib>
#include <initializer_list>
#include <string>
#include <thread>
#include <vector>

extern "C" {
extern const wl_interface ext_data_control_manager_v1_interface;
extern const wl_interface ext_data_control_device_v1_interface;
}

using namespace transcribe::insertion;
using namespace std::literals;
using transcribe::linux_input::paste_commands;
using Commands = std::vector<std::vector<std::string>>;

namespace {

// ext_data_control_manager_v1's requests in protocol order, as a compositor implements them.
struct ManagerRequests {
    void (*create_data_source)(wl_client*, wl_resource*, std::uint32_t);
    void (*get_data_device)(wl_client*, wl_resource*, std::uint32_t, wl_resource*);
    void (*destroy)(wl_client*, wl_resource*);
};
const ManagerRequests MANAGER{
    [](wl_client*, wl_resource*, std::uint32_t) {},
    [](wl_client* client, wl_resource* manager, std::uint32_t id, wl_resource*) {
        wl_resource_create(client, &ext_data_control_device_v1_interface,
                           wl_resource_get_version(manager), id);
    },
    [](wl_client*, wl_resource* manager) { wl_resource_destroy(manager); },
};

// A compositor in this process with `globals`, serving the next Wayland connection.
class Compositor {
  public:
    explicit Compositor(std::initializer_list<const wl_interface*> globals) {
        for (auto* interface : globals)
            wl_global_create(display_, interface, interface->version, (void*)interface, bind);
        int sockets[2];
        socketpair(AF_UNIX, SOCK_STREAM | SOCK_CLOEXEC, 0, sockets);
        wl_client_create(display_, sockets[0]);
        // WAYLAND_SOCKET takes precedence over WAYLAND_DISPLAY, so the desktop is never touched.
        setenv("WAYLAND_SOCKET", std::to_string(sockets[1]).c_str(), 1);
        loop_ = std::jthread([this](std::stop_token stop) {
            while (!stop.stop_requested()) {
                wl_event_loop_dispatch(wl_display_get_event_loop(display_), 10);
                wl_display_flush_clients(display_);
            }
        });
    }
    ~Compositor() {
        loop_.request_stop();
        loop_.join();
        // The loop may stop before it sees the client hang up.
        wl_display_destroy_clients(display_);
        wl_display_destroy(display_);
    }

  private:
    static void bind(wl_client* client, void* data, std::uint32_t version, std::uint32_t id) {
        auto* interface = static_cast<const wl_interface*>(data);
        auto* resource = wl_resource_create(client, interface, int(version), id);
        if (interface != &wl_seat_interface)
            wl_resource_set_implementation(resource, &MANAGER, nullptr, nullptr);
    }

    wl_display* display_ = wl_display_create();
    std::jthread loop_;
};

data_control::Kind failure(std::initializer_list<const wl_interface*> globals) {
    Compositor compositor(globals);
    try {
        data_control::mime_types(data_control::Clipboard::Regular);
    } catch (const data_control::Failure& failure) {
        return failure.kind;
    }
    ADD_FAILURE() << "An empty clipboard must not read";
    return data_control::Kind::Other;
}

} // namespace

TEST(Backend, UsesXWaylandSelectionsOnlyWithoutDataControl) {
    // GNOME offers no data control, so its XWayland selections stand in.
    EXPECT_EQ(failure({&wl_seat_interface}), data_control::Kind::MissingProtocol);
    {
        Compositor gnome({&wl_seat_interface});
        EXPECT_TRUE(platform::lacks_data_control());
    }
    // ext-data-control, and wlr-data-control bound under its own name, answer with the
    // current selection: none here.
    EXPECT_EQ(failure({&wl_seat_interface, &ext_data_control_manager_v1_interface}),
              data_control::Kind::ClipboardEmpty);
    auto wlr = ext_data_control_manager_v1_interface;
    wlr.name = "zwlr_data_control_manager_v1";
    wlr.version = 2;
    EXPECT_EQ(failure({&wl_seat_interface, &wlr}), data_control::Kind::ClipboardEmpty);
}

TEST(Terminal, RecognizesTerminalAppIdsAndX11Classes) {
    for (auto app : {"Alacritty"sv, "org.kde.konsole"sv, "xterm\0XTerm\0"sv, "st\0St\0"sv, "foot"sv,
                     "footclient"sv, "org.gnome.Ptyxis"sv, "org.gnome.Console"sv,
                     "com.system76.CosmicTerm"sv, "com.raphaelamorim.rio"sv})
        EXPECT_TRUE(platform::is_terminal(app)) << app;
    for (auto app : {"firefox"sv, "org.kde.kate"sv, "transcribe"sv, "org.example.Postman"sv,
                     "stellarium\0Stellarium\0"sv})
        EXPECT_FALSE(platform::is_terminal(app)) << app;
}

TEST(Paste, ToolsFollowTheSessionAndReceiver) {
    EXPECT_EQ(paste_commands(true, false, false, false),
              (Commands{{"wtype", "-M", "ctrl", "-k", "v", "-m", "ctrl"},
                        {"sh", "-c", "echo key ctrl+v | dotool"},
                        {"ydotool", "key", "29:1", "47:1", "47:0", "29:0"}}));
    // wtype is skipped on KDE and for terminals.
    EXPECT_EQ(paste_commands(true, true, true, false),
              (Commands{{"sh", "-c", "echo key ctrl+shift+v | dotool"},
                        {"ydotool", "key", "29:1", "42:1", "47:1", "47:0", "42:0", "29:0"}}));
    EXPECT_EQ(paste_commands(false, false, true, true),
              (Commands{{"xdotool", "key", "--clearmodifiers", "ctrl+shift+v"},
                        {"ydotool", "key", "ctrl+shift+v"}}));
}
