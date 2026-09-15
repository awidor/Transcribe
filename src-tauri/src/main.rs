#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
fn main() {
    // WebKitGTK's DMA-BUF renderer can terminate the Wayland connection on
    // NVIDIA ("explicit sync is used, but no acquire point is set"). Set this
    // before GTK or worker threads start, while respecting explicit overrides.
    #[cfg(target_os = "linux")]
    if std::env::var_os("WAYLAND_DISPLAY").is_some()
        && std::env::var_os("WEBKIT_DISABLE_DMABUF_RENDERER").is_none()
    {
        std::env::set_var("WEBKIT_DISABLE_DMABUF_RENDERER", "1");
    }
    transcribe::run();
}
