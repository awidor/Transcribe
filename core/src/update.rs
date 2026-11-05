//! Updates for builds Velopack installed: the Windows setup and the AppImage.
use anyhow::{Context, Result};
use std::sync::Mutex;
use velopack::{
    sources::FileSource, sources::GithubSource, UpdateCheck, UpdateInfo, UpdateManager, VelopackApp,
};

/// The releases to update from: the GitHub repository, or a folder of
/// releases for testing, chosen when building.
const FEED: &str = match option_env!("TRANSCRIBE_UPDATES") {
    Some(feed) => feed,
    None => "https://github.com/awidor/Transcribe",
};

/// Runs the installer's hooks; call it first in main, as it may end the process.
pub fn startup() {
    VelopackApp::build().run();
}

pub struct Updater {
    manager: UpdateManager,
    found: Mutex<Option<UpdateInfo>>,
}

impl Updater {
    /// Nothing for builds Velopack did not install.
    pub fn new() -> Option<Self> {
        let manager = if FEED.starts_with("https://github.com/") {
            UpdateManager::new(GithubSource::new(FEED, None, false), None, None)
        } else {
            UpdateManager::new(FileSource::new(FEED), None, None)
        };
        Some(Self {
            manager: manager.ok()?,
            found: Mutex::new(None),
        })
    }

    /// The newer version on offer, or nothing when this one is current.
    pub fn check(&self) -> Result<Option<String>> {
        let found = match self.manager.check_for_updates()? {
            UpdateCheck::UpdateAvailable(info) => Some(*info),
            UpdateCheck::NoUpdateAvailable | UpdateCheck::RemoteIsEmpty => None,
        };
        let version = found
            .as_ref()
            .map(|info| info.TargetFullRelease.Version.clone());
        *self.found.lock().unwrap() = found;
        Ok(version)
    }

    /// Downloads the version `check` found and hands it to the installer,
    /// which applies it and restarts the app once the app exits.
    pub fn install(&self) -> Result<()> {
        let found = self
            .found
            .lock()
            .unwrap()
            .clone()
            .context("Update unavailable")?;
        self.manager.download_updates(&found, None)?;
        self.manager
            .wait_exit_then_apply_updates(&found, false, true, Vec::<String>::new())?;
        Ok(())
    }
}
