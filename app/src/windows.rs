//! Windows: the pill never takes focus, and the main window reports its own
//! keys, since Windows withholds them from the global listener while
//! Transcribe is in front.
use crate::shell::{Shell, UiEvent};
use futures::channel::mpsc::UnboundedSender;
use gpui::{Context, Task, Window};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};
use transcribe_core::{
    hotkey,
    rules::{RecorderMode, WindowShortcut},
    session::{App as Core, Event},
};
use windows_sys::Win32::{
    Foundation::{HWND, LPARAM, LRESULT, WPARAM},
    UI::{
        Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass},
        WindowsAndMessaging::{
            GWL_EXSTYLE, GetWindowLongPtrW, HWND_TOPMOST, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE,
            SWP_SHOWWINDOW, SetWindowLongPtrW, SetWindowPos, WM_KEYDOWN, WM_KEYUP, WM_KILLFOCUS,
            WM_NCDESTROY, WM_SYSKEYDOWN, WM_SYSKEYUP, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
            WS_EX_TOPMOST,
        },
    },
};

fn hwnd(window: &Window) -> Option<HWND> {
    match HasWindowHandle::window_handle(window).ok()?.as_raw() {
        RawWindowHandle::Win32(handle) => Some(handle.hwnd.get() as HWND),
        _ => None,
    }
}

/// Shows the pill above other windows without activating it.
pub fn float(window: &Window) {
    let Some(hwnd) = hwnd(window) else { return };
    unsafe {
        let style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
        SetWindowLongPtrW(
            hwnd,
            GWL_EXSTYLE,
            style | (WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW | WS_EX_TOPMOST) as isize,
        );
        SetWindowPos(
            hwnd,
            HWND_TOPMOST,
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_SHOWWINDOW,
        );
    }
}

/// Puts a live recording's pill back after something hid it, such as a
/// fullscreen app or a shell that drops topmost windows.
pub fn reshow(cx: &mut Context<Shell>) -> Task<()> {
    cx.spawn(async move |this, cx| {
        loop {
            cx.background_executor()
                .timer(Duration::from_millis(750))
                .await;
            if this
                .update(cx, |shell, cx| shell.reassert_pill(cx))
                .is_err()
            {
                return;
            }
        }
    })
}

struct Keys {
    shortcut: WindowShortcut,
    binding: Vec<Vec<u32>>,
    mode: RecorderMode,
    token: Option<u64>,
}

pub struct KeyRouter {
    app: Core,
    events: UnboundedSender<UiEvent>,
    keys: Mutex<Keys>,
}

impl KeyRouter {
    pub fn new(app: Core, events: UnboundedSender<UiEvent>) -> Self {
        let binding = app.shortcut_binding();
        Self {
            app,
            events,
            keys: Mutex::new(Keys {
                shortcut: WindowShortcut::default(),
                binding,
                mode: RecorderMode::Idle,
                token: None,
            }),
        }
    }

    pub fn set_capture(&self, mode: RecorderMode, token: Option<u64>) {
        let mut keys = self.keys.lock().unwrap();
        keys.mode = mode;
        keys.token = token;
    }

    pub fn set_binding(&self, binding: Vec<Vec<u32>>) {
        self.keys.lock().unwrap().binding = binding;
    }

    fn blur(&self) {
        self.keys.lock().unwrap().shortcut.blur();
    }

    /// Keys are reported here, in order, as they happen. True when the window
    /// must not see the key.
    fn key(&self, key: u32, scan: u32, extended: bool, down: bool, repeat: bool) -> bool {
        let pressed = hotkey::windows_key(key, scan, extended);
        let mut keys = self.keys.lock().unwrap();
        if keys.mode != RecorderMode::Idle {
            // Keys being recorded as a shortcut must not also operate the window.
            let (listening, token) = (keys.mode == RecorderMode::Listening, keys.token);
            drop(keys);
            let Some(token) = token.filter(|_| listening && !repeat) else {
                return true;
            };
            match self.app.capture_shortcut_key(token, pressed, down) {
                Ok(events) => {
                    for event in events {
                        let _ = self
                            .events
                            .unbounded_send(UiEvent::Core(Event::Capture(event)));
                    }
                }
                Err(error) => {
                    let _ = self
                        .events
                        .unbounded_send(UiEvent::CaptureFailed(error.to_string()));
                }
            }
            return true;
        }
        let binding = keys.binding.clone();
        let suppress = keys.shortcut.key(pressed.code, down, &binding);
        drop(keys);
        if !repeat {
            // A key the listener cannot name is not part of any shortcut.
            let _ = self.app.window_shortcut_key(pressed, down);
        }
        suppress
    }
}

unsafe extern "system" fn subclass(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _: usize,
    data: usize,
) -> LRESULT {
    let router = unsafe { &*(data as *const KeyRouter) };
    match message {
        WM_KEYDOWN | WM_SYSKEYDOWN | WM_KEYUP | WM_SYSKEYUP => {
            let down = matches!(message, WM_KEYDOWN | WM_SYSKEYDOWN);
            let scan = ((lparam >> 16) & 0xff) as u32;
            let extended = (lparam >> 24) & 1 == 1;
            let repeat = down && (lparam >> 30) & 1 == 1;
            if router.key(wparam as u32, scan, extended, down, repeat) {
                return 0;
            }
        }
        WM_KILLFOCUS => router.blur(),
        WM_NCDESTROY => unsafe {
            RemoveWindowSubclass(hwnd, Some(subclass), 1);
            drop(Arc::from_raw(data as *const KeyRouter));
        },
        _ => {}
    }
    unsafe { DefSubclassProc(hwnd, message, wparam, lparam) }
}

pub fn route_keys(window: &Window, router: Arc<KeyRouter>) {
    let Some(hwnd) = hwnd(window) else { return };
    unsafe {
        SetWindowSubclass(hwnd, Some(subclass), 1, Arc::into_raw(router) as usize);
    }
}
