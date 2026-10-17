#pragma once

#include "PillWindow.g.h"

#include "transcribe/app.hpp"

#include <atomic>
#include <chrono>
#include <memory>
#include <optional>

namespace winrt::Transcribe::implementation {

// The recording widget: a flyout above the taskbar that never takes focus.
struct PillWindow : PillWindowT<PillWindow> {
    void InitializeComponent();
    void attach(std::shared_ptr<transcribe::app::App> app);

    // Throws when the widget cannot be shown.
    void show(const transcribe::app::Session& session);
    void session(const transcribe::app::Session& current);
    void hide();

    // Written from the audio thread.
    std::atomic<float> level{0};

  private:
    void render(const transcribe::app::Session& session);
    void place();
    void tick();

    std::shared_ptr<transcribe::app::App> app_;
    transcribe::app::Session shown_;
    // The pill keeps showing its last session while it closes.
    bool closing_ = true;
    transcribe::app::Loudness loudness_;
    bool listening_ = false;
    std::optional<std::chrono::steady_clock::time_point> last_frame_;
    event_token rendering_{};
    Microsoft::UI::Dispatching::DispatcherQueueTimer clock_timer_{nullptr};
    Microsoft::UI::Composition::SystemBackdrops::DesktopAcrylicController backdrop_{nullptr};
    Microsoft::UI::Composition::SystemBackdrops::SystemBackdropConfiguration backdrop_config_{
        nullptr};
};

} // namespace winrt::Transcribe::implementation

namespace winrt::Transcribe::factory_implementation {

struct PillWindow : PillWindowT<PillWindow, implementation::PillWindow> {};

} // namespace winrt::Transcribe::factory_implementation
