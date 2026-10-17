#include "transcribe/app.hpp"

#include <Velopack.hpp>

#include <mutex>

namespace transcribe::app {

void startup() {
    Velopack::VelopackApp::Build().Run();
}

std::optional<Updater> updater() {
    struct Releases {
        Velopack::UpdateManager manager{
            std::make_unique<Velopack::GithubSource>("https://github.com/awidor/Transcribe")};
        std::mutex mutex;
        std::optional<Velopack::UpdateInfo> found;
    };
    std::shared_ptr<Releases> releases;
    try {
        releases = std::make_shared<Releases>();
    } catch (const std::exception&) {
        return std::nullopt;
    }
    return Updater{[releases] {
                       auto found = releases->manager.CheckForUpdates();
                       std::lock_guard lock(releases->mutex);
                       releases->found = found;
                       return found ? std::optional(found->TargetFullRelease.Version)
                                    : std::nullopt;
                   },
                   [releases] {
                       std::optional<Velopack::UpdateInfo> found;
                       {
                           std::lock_guard lock(releases->mutex);
                           found = releases->found;
                       }
                       if (!found) throw Error("Update unavailable");
                       releases->manager.DownloadUpdates(*found);
                       releases->manager.WaitExitThenApplyUpdates(*found);
                   }};
}

} // namespace transcribe::app
