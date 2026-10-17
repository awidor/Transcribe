#pragma once

#include <exception>
#include <functional>
#include <string>
#include <string_view>
#include <thread>
#include <type_traits>
#include <utility>

namespace shell {

// The UI thread's queue, set before any window exists.
inline winrt::Microsoft::UI::Dispatching::DispatcherQueue queue{nullptr};

// Runs `work` on the UI thread; safe to call from any thread.
inline void ui(std::function<void()> work) {
    queue.TryEnqueue([work = std::move(work)] { work(); });
}

// Runs a blocking core command away from the UI, then hands its result to `done` (or the
// failure's message to `failed`) on the UI thread.
template <class Work, class Done, class Failed>
void background(Work work, Done done, Failed failed) {
    std::thread([work = std::move(work), done = std::move(done),
                 failed = std::move(failed)]() mutable {
        try {
            if constexpr (std::is_void_v<std::invoke_result_t<Work&>>) {
                work();
                ui([done = std::move(done)]() mutable { done(); });
            } else {
                ui([done = std::move(done), result = work()]() mutable {
                    done(std::move(result));
                });
            }
        } catch (const std::exception& e) {
            ui([failed = std::move(failed), message = std::string(e.what())]() mutable {
                failed(message);
            });
        }
    }).detach();
}

inline winrt::hstring text(std::string_view value) {
    return winrt::to_hstring(value);
}

inline HWND hwnd(const winrt::Microsoft::UI::Windowing::AppWindow& window) {
    return winrt::Microsoft::UI::GetWindowFromWindowId(window.Id());
}

// A style from the window's resources, for controls created in code.
inline winrt::Microsoft::UI::Xaml::Style
style(const winrt::Microsoft::UI::Xaml::FrameworkElement& root, const wchar_t* key) {
    return root.Resources()
        .Lookup(winrt::box_value(winrt::hstring(key)))
        .as<winrt::Microsoft::UI::Xaml::Style>();
}

} // namespace shell
