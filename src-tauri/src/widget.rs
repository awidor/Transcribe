#[cfg(target_os = "macos")]
use tauri::Manager;
use tauri::WebviewWindow;

#[cfg(target_os = "linux")]
pub(crate) fn init_linux<R: tauri::Runtime>(window: &WebviewWindow<R>) -> tauri::Result<()> {
    use gtk::prelude::*;
    use gtk_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};
    if !gtk::is_initialized_main_thread() || !gtk_layer_shell::is_supported() {
        return Ok(());
    }
    let native = window.gtk_window()?;
    native.init_layer_shell();
    native.set_size_request(188, 48);
    native.resize(188, 48);
    native.set_namespace("transcribe-widget");
    native.set_layer(Layer::Overlay);
    native.set_anchor(Edge::Bottom, true);
    native.set_layer_shell_margin(Edge::Bottom, 24);
    // Zone zero respects panels without reserving space for the widget.
    native.set_exclusive_zone(0);
    native.set_keyboard_mode(KeyboardMode::None);
    native.set_accept_focus(false);
    native.set_focus_on_map(false);
    Ok(())
}

pub(crate) fn hide(window: &WebviewWindow) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        crate::queue_widget(window.app_handle(), false);
        Ok(())
    }
    #[cfg(not(target_os = "macos"))]
    window.hide().map_err(|e| e.to_string())
}

#[cfg(target_os = "macos")]
pub(crate) mod macos {
    use std::{
        ffi::{c_char, CString},
        sync::OnceLock,
    };
    use tauri::Manager;
    static APP: OnceLock<tauri::AppHandle> = OnceLock::new();
    unsafe extern "C" {
        fn tc_notch_init(action: extern "C" fn(i32));
        fn tc_notch_update(phase: *const c_char, started_at: i64, error: *const c_char);
        fn tc_notch_level(level: f32);
        fn tc_notch_hide();
    }
    extern "C" fn action(action: i32) {
        let Some(app) = APP.get().cloned() else {
            return;
        };
        let state = app
            .state::<std::sync::Arc<crate::AppState>>()
            .inner()
            .clone();
        tauri::async_runtime::spawn(async move {
            match action {
                0 => {
                    let _ = crate::stop_recording(app, state, None).await;
                }
                1 => {
                    let _ = crate::cancel_impl(app, state).await;
                }
                2 => {
                    crate::open_history_impl(app, state).await;
                }
                _ => {}
            }
        });
    }
    pub(crate) fn init(app: tauri::AppHandle) {
        let _ = APP.set(app);
        unsafe {
            tc_notch_init(action);
        }
    }
    pub(super) fn show(session: &crate::SessionView) -> Result<(), String> {
        let phase = CString::new(session.phase.as_str()).map_err(|e| e.to_string())?;
        let error = CString::new(session.error.as_deref().unwrap_or("").replace('\0', " "))
            .map_err(|e| e.to_string())?;
        unsafe {
            tc_notch_update(
                phase.as_ptr(),
                session.started_at.unwrap_or(0),
                error.as_ptr(),
            );
        }
        Ok(())
    }
    pub(crate) fn level(level: f32) {
        unsafe {
            tc_notch_level(level);
        }
    }
    pub(crate) fn hide() {
        unsafe {
            tc_notch_hide();
        }
    }
}

#[cfg(windows)]
pub(crate) fn watch_foreground(app: tauri::AppHandle) -> Result<(), String> {
    windows::watch_foreground(move || {
        let app = app.clone();
        // Leave the native callback immediately; showing can itself generate
        // window events. The queued update still checks the current session.
        tauri::async_runtime::spawn(async move { crate::queue_widget(&app, true) });
    })
}

/// Logical window size: the recording pill, or the card offering the
/// transcript after paste fails. The page lays itself out to match.
#[cfg(not(target_os = "macos"))]
fn size(session: &crate::SessionView) -> (f64, f64) {
    if session.transcript.is_some() {
        (308.0, 68.0)
    } else {
        (188.0, 48.0)
    }
}

/// Called on the event-loop thread. Showing the widget must never activate it.
pub(crate) fn show(_window: &WebviewWindow, _session: &crate::SessionView) -> Result<(), String> {
    #[cfg(windows)]
    return windows::show(_window, size(_session));

    #[cfg(target_os = "macos")]
    return macos::show(_session);
    #[cfg(target_os = "linux")]
    let window = _window;
    #[cfg(target_os = "linux")]
    let (width, height) = size(_session);
    #[cfg(target_os = "linux")]
    {
        use gtk_layer_shell::LayerShell;
        let native = window.gtk_window().map_err(|e| e.to_string())?;
        if native.is_layer_window() {
            let display = gtk::prelude::WidgetExt::display(&native);
            // Wayland does not always expose a primary output. Pin the widget
            // to the first output in that case instead of following focus.
            let monitor = display
                .primary_monitor()
                .or_else(|| display.monitor(0))
                .ok_or("Widget monitor unavailable")?;
            native.set_monitor(&monitor);
            gtk::prelude::WidgetExt::set_size_request(&native, width as i32, height as i32);
            gtk::prelude::GtkWindowExt::resize(&native, width as i32, height as i32);
            return window.show().map_err(|e| e.to_string());
        }
    }
    #[cfg(target_os = "linux")]
    window
        .set_size(tauri::LogicalSize::new(width, height))
        .map_err(|e| e.to_string())?;
    #[cfg(target_os = "linux")]
    if let Some(monitor) = window.primary_monitor().map_err(|e| e.to_string())? {
        let scale = monitor.scale_factor();
        let origin = monitor.position();
        let size = monitor.size();
        window
            .set_position(tauri::PhysicalPosition::new(
                origin.x + ((size.width as f64 - width * scale) / 2.0) as i32,
                origin.y + (size.height as f64 - (72.0 + height) * scale) as i32,
            ))
            .map_err(|e| e.to_string())?;
    }
    #[cfg(target_os = "linux")]
    window.show().map_err(|e| e.to_string())
}

#[cfg(windows)]
mod windows {
    use super::*;
    use std::{cell::RefCell, rc::Rc};
    use windows_sys::Win32::{
        Foundation::{HWND, RECT},
        Graphics::Gdi::{
            GetMonitorInfoW, MonitorFromWindow, MONITORINFO, MONITOR_DEFAULTTOPRIMARY,
        },
        UI::{
            Accessibility::{SetWinEventHook, UnhookWinEvent, HWINEVENTHOOK},
            WindowsAndMessaging::*,
        },
    };

    struct Watcher {
        hooks: Vec<HWINEVENTHOOK>,
        changed: Rc<dyn Fn()>,
    }
    impl Drop for Watcher {
        fn drop(&mut self) {
            for hook in &self.hooks {
                unsafe {
                    UnhookWinEvent(*hook);
                }
            }
        }
    }
    thread_local! {
        // Hooks and their callback live on Tauri's message-loop thread.
        static WATCHER: RefCell<Option<Watcher>> = const { RefCell::new(None) };
    }

    fn destination_event(
        event: u32,
        hwnd: HWND,
        object: i32,
        child: i32,
        foreground: HWND,
    ) -> bool {
        !hwnd.is_null()
            && hwnd == foreground
            && (event == EVENT_SYSTEM_FOREGROUND
                || (event == EVENT_OBJECT_LOCATIONCHANGE && object == OBJID_WINDOW && child == 0))
    }

    unsafe extern "system" fn changed(
        _: HWINEVENTHOOK,
        event: u32,
        hwnd: HWND,
        object: i32,
        child: i32,
        _: u32,
        _: u32,
    ) {
        if !destination_event(event, hwnd, object, child, unsafe { GetForegroundWindow() }) {
            return;
        }
        let callback = WATCHER.with(|watcher| watcher.borrow().as_ref().map(|w| w.changed.clone()));
        if let Some(callback) = callback {
            callback();
        }
    }

    pub(super) fn watch_foreground(callback: impl Fn() + 'static) -> Result<(), String> {
        let mut watcher = Watcher {
            hooks: Vec::new(),
            changed: Rc::new(callback),
        };
        for event in [EVENT_SYSTEM_FOREGROUND, EVENT_OBJECT_LOCATIONCHANGE] {
            let hook = unsafe {
                SetWinEventHook(
                    event,
                    event,
                    std::ptr::null_mut(),
                    Some(changed),
                    0,
                    0,
                    WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS,
                )
            };
            if hook.is_null() {
                return Err(failure("foreground tracking"));
            }
            watcher.hooks.push(hook);
        }
        WATCHER.with(|current| *current.borrow_mut() = Some(watcher));
        Ok(())
    }

    fn failure(operation: &str) -> String {
        format!(
            "Widget {operation} failed: {}",
            std::io::Error::last_os_error()
        )
    }

    fn bounds(work: RECT, scale: f64, (width, height): (f64, f64)) -> RECT {
        let width = ((width * scale).round() as i32).min(work.right - work.left);
        let height = ((height * scale).round() as i32).min(work.bottom - work.top);
        let left = work.left + (work.right - work.left - width) / 2;
        let top = (work.bottom - height - (24.0 * scale).round() as i32).max(work.top);
        RECT {
            left,
            top,
            right: left + width,
            bottom: top + height,
        }
    }

    pub(super) fn show(window: &WebviewWindow, size: (f64, f64)) -> Result<(), String> {
        unsafe {
            let monitor = MonitorFromWindow(std::ptr::null_mut(), MONITOR_DEFAULTTOPRIMARY);
            let mut info: MONITORINFO = std::mem::zeroed();
            info.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
            if GetMonitorInfoW(monitor, &mut info) == 0 {
                return Err(failure("monitor lookup"));
            }
            let work = info.rcWork;
            let display = window
                .monitor_from_point(
                    (work.left + (work.right - work.left) / 2) as f64,
                    (work.top + (work.bottom - work.top) / 2) as f64,
                )
                .map_err(|e| e.to_string())?
                .ok_or("Widget monitor unavailable")?;
            let rect = bounds(work, display.scale_factor(), size);
            let hwnd = window.hwnd().map_err(|e| e.to_string())?.0;
            // Keep Tao's visibility state in sync, but also repair native hiding
            // (for example Show Desktop) even if Tao still considers it visible.
            window.show().map_err(|e| e.to_string())?;
            ensure_visible(hwnd, rect)
        }
    }

    fn ensure_visible(hwnd: HWND, rect: RECT) -> Result<(), String> {
        unsafe {
            if IsIconic(hwnd) != 0 {
                ShowWindow(hwnd, SW_SHOWNOACTIVATE);
            }
            if SetWindowPos(
                hwnd,
                HWND_TOPMOST,
                rect.left,
                rect.top,
                rect.right - rect.left,
                rect.bottom - rect.top,
                SWP_NOACTIVATE | SWP_SHOWWINDOW,
            ) == 0
            {
                return Err(failure("position/show"));
            }
            if IsWindowVisible(hwnd) == 0 {
                return Err("Widget remained hidden after showing".into());
            }
            Ok(())
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn tracks_destination_changes_without_following_caret_or_background_events() {
            let active = 1usize as HWND;
            let other = 2usize as HWND;
            assert!(destination_event(
                EVENT_SYSTEM_FOREGROUND,
                active,
                0,
                0,
                active
            ));
            assert!(destination_event(
                EVENT_OBJECT_LOCATIONCHANGE,
                active,
                OBJID_WINDOW,
                0,
                active
            ));
            assert!(!destination_event(
                EVENT_OBJECT_LOCATIONCHANGE,
                active,
                OBJID_CARET,
                0,
                active
            ));
            assert!(!destination_event(
                EVENT_OBJECT_LOCATIONCHANGE,
                other,
                OBJID_WINDOW,
                0,
                active
            ));
            assert!(!destination_event(
                EVENT_OBJECT_LOCATIONCHANGE,
                active,
                OBJID_WINDOW,
                1,
                active
            ));
            assert!(!destination_event(
                EVENT_SYSTEM_FOREGROUND,
                std::ptr::null_mut(),
                0,
                0,
                std::ptr::null_mut()
            ));
        }

        #[test]
        fn placement_respects_destination_work_area_and_dpi() {
            for (work, scale) in [
                (
                    RECT {
                        left: 0,
                        top: 0,
                        right: 3440,
                        bottom: 1392,
                    },
                    1.0,
                ),
                (
                    RECT {
                        left: 3440,
                        top: 0,
                        right: 6000,
                        bottom: 1392,
                    },
                    1.5,
                ),
                (
                    RECT {
                        left: -2560,
                        top: -200,
                        right: 0,
                        bottom: 1192,
                    },
                    2.0,
                ),
            ] {
                for (width, height) in [(188.0, 48.0), (308.0, 68.0)] {
                    let rect = bounds(work, scale, (width, height));
                    assert_eq!(rect.right - rect.left, (width * scale) as i32);
                    assert_eq!(rect.bottom - rect.top, (height * scale) as i32);
                    assert!(rect.left >= work.left && rect.right <= work.right);
                    assert!(rect.top >= work.top && rect.bottom < work.bottom);
                    assert!(((rect.left + rect.right) - (work.left + work.right)).abs() <= 1);
                }
            }
        }

        #[test]
        #[ignore = "Briefly shows a native non-activating widget fixture; requires an unlocked desktop"]
        fn native_widget_recovers_visibility_without_stealing_focus() {
            unsafe {
                let foreground = GetForegroundWindow();
                let class: Vec<u16> = "STATIC\0".encode_utf16().collect();
                let hwnd = CreateWindowExW(
                    WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW,
                    class.as_ptr(),
                    std::ptr::null(),
                    WS_POPUP,
                    0,
                    0,
                    188,
                    48,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null(),
                );
                assert!(!hwnd.is_null());
                struct Fixture(HWND);
                impl Drop for Fixture {
                    fn drop(&mut self) {
                        unsafe {
                            DestroyWindow(self.0);
                        }
                    }
                }
                let _fixture = Fixture(hwnd);
                let rect = RECT {
                    left: 100,
                    top: 100,
                    right: 324,
                    bottom: 160,
                };
                for _ in 0..2 {
                    ShowWindow(hwnd, SW_HIDE);
                    assert_eq!(IsWindowVisible(hwnd), 0);
                    ensure_visible(hwnd, rect).unwrap();
                    assert_ne!(IsWindowVisible(hwnd), 0);
                    assert_eq!(GetForegroundWindow(), foreground);
                    let mut actual: RECT = std::mem::zeroed();
                    assert_ne!(GetWindowRect(hwnd, &mut actual), 0);
                    assert_eq!(
                        (actual.left, actual.top, actual.right, actual.bottom),
                        (rect.left, rect.top, rect.right, rect.bottom)
                    );
                }
            }
        }
    }
}
