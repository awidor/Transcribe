#pragma once

#include "App.xaml.g.h"

namespace winrt::Transcribe::implementation {

struct App : AppT<App> {
    void OnLaunched(const Microsoft::UI::Xaml::LaunchActivatedEventArgs&);
};

} // namespace winrt::Transcribe::implementation
