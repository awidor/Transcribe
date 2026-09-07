#![cfg(windows)]

#[allow(dead_code)]
#[path = "../src/focus.rs"]
mod focus;

use std::ptr::null_mut;
use windows_sys::Win32::{
    Foundation::HWND,
    UI::WindowsAndMessaging::{CreateWindowExW, DestroyWindow, WS_CHILD, WS_OVERLAPPED},
};

struct HiddenWindow(HWND);
impl HiddenWindow {
    fn new(parent: HWND, child: bool) -> Self {
        let class: Vec<u16> = "STATIC\0".encode_utf16().collect();
        // Invisible fixtures never activate or receive injected keyboard input.
        let hwnd = unsafe {
            CreateWindowExW(
                0,
                class.as_ptr(),
                null_mut(),
                if child { WS_CHILD } else { WS_OVERLAPPED },
                0,
                0,
                10,
                10,
                parent,
                null_mut(),
                null_mut(),
                null_mut(),
            )
        };
        assert!(!hwnd.is_null(), "{}", std::io::Error::last_os_error());
        Self(hwnd)
    }
}
impl Drop for HiddenWindow {
    fn drop(&mut self) {
        unsafe {
            DestroyWindow(self.0);
        }
    }
}

#[test]
fn recognizes_webview_children_without_accepting_other_windows() {
    let settings = HiddenWindow::new(null_mut(), false);
    let webview = HiddenWindow::new(settings.0, true);
    let input = HiddenWindow::new(webview.0, true);
    let other_app = HiddenWindow::new(null_mut(), false);
    let widget = HiddenWindow::new(settings.0, false);

    assert!(focus::same_window(settings.0, settings.0));
    assert!(focus::same_window(settings.0, webview.0));
    assert!(focus::same_window(settings.0, input.0));
    assert!(!focus::same_window(settings.0, other_app.0));
    assert!(!focus::same_window(settings.0, widget.0));
    assert!(!focus::same_window(settings.0, null_mut()));
    assert!(!focus::same_window(null_mut(), settings.0));
    let destroyed = HiddenWindow::new(null_mut(), false);
    let handle = destroyed.0;
    drop(destroyed);
    assert!(!focus::same_window(handle, handle));
}
