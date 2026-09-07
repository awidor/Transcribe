/// WebView2 can own keyboard focus while Tao reports its parent as unfocused.
/// Shortcut capture depends on the foreground window, including its children.
pub(crate) fn is_active<R: tauri::Runtime>(window: &tauri::Window<R>) -> tauri::Result<bool> {
    #[cfg(windows)]
    {
        use windows_sys::Win32::UI::WindowsAndMessaging::GetForegroundWindow;
        let hwnd = window.hwnd()?.0;
        Ok(same_window(hwnd, unsafe { GetForegroundWindow() }))
    }
    #[cfg(not(windows))]
    window.is_focused()
}

#[cfg(windows)]
pub(crate) fn same_window(
    window: windows_sys::Win32::Foundation::HWND,
    foreground: windows_sys::Win32::Foundation::HWND,
) -> bool {
    use windows_sys::Win32::UI::WindowsAndMessaging::{GetAncestor, GA_ROOT};
    if window.is_null() || foreground.is_null() {
        return false;
    }
    // GA_ROOT deliberately excludes separately owned windows, such as the widget.
    let root = unsafe { GetAncestor(window, GA_ROOT) };
    !root.is_null() && root == unsafe { GetAncestor(foreground, GA_ROOT) }
}
