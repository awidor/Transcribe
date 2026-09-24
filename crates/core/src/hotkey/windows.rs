use super::*;
use std::cell::RefCell;
use windows_sys::Win32::{
    Foundation::*,
    System::{LibraryLoader::GetModuleHandleW, Threading::GetCurrentThreadId},
    UI::{
        Input::KeyboardAndMouse::{GetAsyncKeyState, GetKeyNameTextW},
        WindowsAndMessaging::*,
    },
};
thread_local! { static SERVICE: RefCell<Option<Arc<Service>>> = const { RefCell::new(None) }; }

const PREPARE_CAPTURE: u32 = WM_APP + 1;
pub(super) struct Listener {
    thread: u32,
    requests: mpsc::Sender<mpsc::Sender<std::result::Result<(), String>>>,
}

pub fn prepare_capture(service: &Service) -> Result<()> {
    let (tx, rx) = mpsc::channel();
    {
        let listener = service.listener.lock().unwrap();
        let listener = listener.as_ref().context("Keyboard listener unavailable")?;
        listener.requests.send(tx)?;
        if unsafe { PostThreadMessageW(listener.thread, PREPARE_CAPTURE, 0, 0) } == 0 {
            bail!(
                "Keyboard listener unavailable: {}",
                std::io::Error::last_os_error()
            );
        }
    }
    rx.recv_timeout(Duration::from_secs(2))
        .context("Keyboard listener did not respond")?
        .map_err(anyhow::Error::msg)
}

unsafe fn install_hook() -> std::result::Result<HHOOK, String> {
    let handle = SetWindowsHookExW(
        WH_KEYBOARD_LL,
        Some(hook),
        GetModuleHandleW(std::ptr::null()),
        0,
    );
    if handle.is_null() {
        Err(std::io::Error::last_os_error().to_string())
    } else {
        Ok(handle)
    }
}

fn reconcile_released_keys(engine: &mut Engine, down: &BTreeSet<u32>) {
    engine.held.retain(|key| down.contains(key));
    engine.swallowed.retain(|key| engine.held.contains(key));
    engine.reset();
}

unsafe extern "system" fn hook(code: i32, w: WPARAM, l: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 {
        let event = &*(l as *const KBDLLHOOKSTRUCT);
        if event.flags & LLKHF_INJECTED == 0 {
            let down = w == WM_KEYDOWN as usize || w == WM_SYSKEYDOWN as usize;
            let up = w == WM_KEYUP as usize || w == WM_SYSKEYUP as usize;
            if down || up {
                let key = event.vkCode
                    | if event.vkCode == 13 && event.flags & LLKHF_EXTENDED != 0 {
                        0x100
                    } else {
                        0
                    };
                let modifier = matches!(key, 0xA0..=0xA5 | 0x5B..=0x5C);
                let suppress = SERVICE.with(|s| {
                    s.borrow().as_ref().is_some_and(|s| {
                        s.input(
                            key,
                            label(key, event.scanCode, event.flags & LLKHF_EXTENDED != 0),
                            down,
                            modifier,
                        )
                    })
                });
                if suppress {
                    return 1;
                }
            }
        }
    }
    CallNextHookEx(std::ptr::null_mut(), code, w, l)
}
fn label(key: u32, scan: u32, extended: bool) -> String {
    let special = match key {
        0xA0 => "Left Shift",
        0xA1 => "Right Shift",
        0xA2 => "Left Ctrl",
        0xA3 => "Right Ctrl",
        0xA4 => "Left Alt",
        0xA5 => "Right Alt",
        0x5B => "Left Win",
        0x5C => "Right Win",
        0x10D => "Numpad Enter",
        0x20 => "Space",
        0xAD => "Mute",
        0xAE => "Volume Down",
        0xAF => "Volume Up",
        0xB0 => "Next Track",
        0xB1 => "Previous Track",
        0xB2 => "Media Stop",
        0xB3 => "Play / Pause",
        0xA6 => "Browser Back",
        0xA7 => "Browser Forward",
        0xA8 => "Browser Refresh",
        0xA9 => "Browser Stop",
        0xAA => "Browser Search",
        0xAB => "Browser Favorites",
        0xAC => "Browser Home",
        _ => "",
    };
    if !special.is_empty() {
        return special.into();
    }
    let mut name = [0u16; 128];
    let count = unsafe {
        GetKeyNameTextW(
            ((scan << 16) | ((extended as u32) << 24)) as i32,
            name.as_mut_ptr(),
            name.len() as i32,
        )
    };
    if count > 0 {
        String::from_utf16_lossy(&name[..count as usize])
    } else {
        format!("Key {key:X}")
    }
}
pub fn start(service: Arc<Service>) -> Result<()> {
    service.engine.lock().unwrap().escape = BTreeSet::from([0x1B]);
    let (tx, rx) = mpsc::sync_channel(1);
    let (requests, pending) = mpsc::channel::<mpsc::Sender<std::result::Result<(), String>>>();
    let callback_service = service.clone();
    std::thread::Builder::new()
        .name("hotkey-windows".into())
        .spawn(move || unsafe {
            let service = callback_service;
            SERVICE.with(|s| *s.borrow_mut() = Some(service.clone()));
            let mut message = std::mem::zeroed();
            PeekMessageW(&mut message, std::ptr::null_mut(), 0, 0, PM_NOREMOVE);
            let mut handle = match install_hook() {
                Ok(handle) => handle,
                Err(error) => {
                    let _ = tx.send(Err(error));
                    return;
                }
            };
            let _ = tx.send(Ok(GetCurrentThreadId()));
            while GetMessageW(&mut message, std::ptr::null_mut(), 0, 0) > 0 {
                if message.message == PREPARE_CAPTURE {
                    for reply in pending.try_iter() {
                        // Windows can silently remove a timed-out hook. Reinstall on
                        // its owning thread before acknowledging a new capture.
                        let result = install_hook().map(|replacement| {
                            UnhookWindowsHookEx(handle);
                            handle = replacement;
                            // Query before locking: Win32 calls may deliver hooks.
                            let mut down: BTreeSet<u32> = (8..=254)
                                .filter(|key| GetAsyncKeyState(*key as i32) < 0)
                                .collect();
                            if down.contains(&13) {
                                down.insert(269);
                            }
                            reconcile_released_keys(&mut service.engine.lock().unwrap(), &down);
                        });
                        let _ = reply.send(result);
                    }
                    continue;
                }
                TranslateMessage(&message);
                DispatchMessageW(&message);
            }
            UnhookWindowsHookEx(handle);
            service.failed("Keyboard listener stopped".into());
        })?;
    let thread = rx.recv()?.map_err(anyhow::Error::msg)?;
    *service.listener.lock().unwrap() = Some(Listener { thread, requests });
    Ok(())
}
pub fn validate(keys: &BTreeSet<u32>) -> Result<()> {
    if keys.iter().any(|k| !matches!(k, 3 | 8..=254 | 269)) {
        bail!("Unsupported key");
    }
    if (keys.contains(&0x5B) || keys.contains(&0x5C)) && keys.contains(&0x4C)
        || (keys.contains(&0xA2) || keys.contains(&0xA3))
            && (keys.contains(&0xA4) || keys.contains(&0xA5))
            && keys.contains(&0x2E)
    {
        bail!("Shortcut is reserved by Windows");
    }
    Ok(())
}
pub fn legacy(text: &str) -> Result<Vec<Vec<u32>>> {
    text.split('+')
        .map(|name| {
            Ok(match name {
                "CommandOrControl" | "Control" | "Ctrl" => vec![0xA2, 0xA3],
                "Alt" => vec![0xA4, 0xA5],
                "Shift" => vec![0xA0, 0xA1],
                "Super" | "Meta" | "Command" => vec![0x5B, 0x5C],
                "Space" => vec![32],
                "Enter" => vec![13],
                "Tab" => vec![9],
                "Escape" => vec![27],
                "Backspace" => vec![8],
                "Delete" => vec![46],
                "Insert" => vec![45],
                "Home" => vec![36],
                "End" => vec![35],
                "PageUp" => vec![33],
                "PageDown" => vec![34],
                "ArrowLeft" | "Left" => vec![37],
                "ArrowUp" | "Up" => vec![38],
                "ArrowRight" | "Right" => vec![39],
                "ArrowDown" | "Down" => vec![40],
                "Backquote" => vec![192],
                "Minus" => vec![189],
                "Equal" => vec![187],
                "BracketLeft" => vec![219],
                "BracketRight" => vec![221],
                "Backslash" => vec![220],
                "Semicolon" => vec![186],
                "Quote" => vec![222],
                "Comma" => vec![188],
                "Period" => vec![190],
                "Slash" => vec![191],
                "CapsLock" => vec![20],
                "NumLock" => vec![144],
                "ScrollLock" => vec![145],
                "Pause" => vec![19],
                "PrintScreen" => vec![44],
                "NumpadEnter" => vec![269],
                "NumpadAdd" => vec![107],
                "NumpadSubtract" => vec![109],
                "NumpadMultiply" => vec![106],
                "NumpadDivide" => vec![111],
                "NumpadDecimal" => vec![110],
                n if n.starts_with("Numpad")
                    && n.len() == 7
                    && n.as_bytes()[6].is_ascii_digit() =>
                {
                    vec![96 + (n.as_bytes()[6] - b'0') as u32]
                }
                n if n.len() == 1 && n.as_bytes()[0].is_ascii_alphanumeric() => {
                    vec![n.as_bytes()[0].to_ascii_uppercase() as u32]
                }
                n if n.starts_with('F')
                    && n[1..].parse::<u32>().is_ok_and(|v| (1..=24).contains(&v)) =>
                {
                    vec![111 + n[1..].parse::<u32>()?]
                }
                _ => bail!("Shortcut needs to be recorded again"),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refresh_discards_stuck_keys_but_drains_genuinely_held_keys() {
        let mut engine = Engine::default();
        engine.held.extend([162, 13]);
        engine.swallowed.extend([162, 13]);
        reconcile_released_keys(&mut engine, &BTreeSet::from([13]));
        assert_eq!(engine.held, BTreeSet::from([13]));
        assert_eq!(engine.swallowed, BTreeSet::from([13]));
        assert!(engine.invalid);
        reconcile_released_keys(&mut engine, &BTreeSet::new());
        assert!(engine.held.is_empty());
        assert!(engine.swallowed.is_empty());
        assert!(!engine.invalid);
    }

    #[test]
    #[ignore = "Installs a native hook; requires a desktop session"]
    fn native_listener_refresh_acknowledges_repeated_capture_requests() {
        let service = Service::new(|_| {});
        for _ in 0..3 {
            let token = service.begin_capture().unwrap();
            service.cancel_capture(Some(token));
        }
    }
}
