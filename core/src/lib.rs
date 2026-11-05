pub mod audio;
pub mod azure;
pub mod credentials;
pub mod hotkey;
pub mod insertion;
#[cfg(target_os = "linux")]
pub mod linux_input;
pub mod provider;
pub mod rules;
pub mod s1;
pub mod session;
pub mod storage;
pub mod update;

/// True when Linux runs a Wayland session.
pub fn is_wayland() -> bool {
    cfg!(target_os = "linux") && std::env::var_os("WAYLAND_DISPLAY").is_some()
}
