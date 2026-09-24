use super::*;
use std::ffi::{c_char, c_void, CStr};
unsafe extern "C" {
    fn tc_hotkey_start(
        context: *mut c_void,
        callback: extern "C" fn(*mut c_void, u32, *const c_char, bool, bool) -> bool,
        ready: extern "C" fn(*mut c_void, i32),
    );
}
struct Context {
    service: Arc<Service>,
    ready: mpsc::SyncSender<i32>,
    started: bool,
}
extern "C" fn ready(context: *mut c_void, status: i32) {
    let context = unsafe { &mut *(context as *mut Context) };
    context.started = status == 0;
    let _ = context.ready.send(status);
}
extern "C" fn event(
    context: *mut c_void,
    code: u32,
    label: *const c_char,
    down: bool,
    modifier: bool,
) -> bool {
    let context = unsafe { &*(context as *const Context) };
    if code == u32::MAX {
        let mut engine = context.service.engine.lock().unwrap();
        engine.held.clear();
        engine.swallowed.clear();
        engine.reset();
        return false;
    }
    let label = unsafe { CStr::from_ptr(label) }
        .to_string_lossy()
        .into_owned();
    context.service.input(code, label, down, modifier)
}
pub fn start(service: Arc<Service>) -> Result<()> {
    service.engine.lock().unwrap().escape = BTreeSet::from([53]);
    let (tx, rx) = mpsc::sync_channel(1);
    std::thread::Builder::new()
        .name("hotkey-macos".into())
        .spawn(move || {
            let mut context = Context {
                service: service.clone(),
                ready: tx,
                started: false,
            };
            unsafe {
                tc_hotkey_start(&mut context as *mut Context as *mut c_void, event, ready);
            }
            if context.started {
                service.failed("Keyboard listener stopped".into());
            }
        })?;
    match rx.recv()? {
        0 => Ok(()),
        1 => bail!("Accessibility access unavailable. If Transcribe is already enabled, remove and re-add the installed app in System Settings, then reopen it."),
        2 => bail!("Input Monitoring access unavailable. Enable Transcribe in System Settings, then reopen it."),
        _ => bail!("Keyboard listener could not start. Quit and reopen Transcribe, then retry."),
    }
}
pub fn validate(keys: &BTreeSet<u32>) -> Result<()> {
    if keys.iter().any(|k| *k > 127 && !(256..=287).contains(k)) {
        bail!("Unsupported macOS key");
    }
    Ok(())
}
pub fn legacy(text: &str) -> Result<Vec<Vec<u32>>> {
    text.split('+')
        .map(|name| {
            Ok(match name {
                "CommandOrControl" | "Command" | "Meta" | "Super" => vec![55, 54],
                "Control" | "Ctrl" => vec![59, 62],
                "Alt" => vec![58, 61],
                "Shift" => vec![56, 60],
                "Space" => vec![49],
                "Enter" => vec![36],
                "Tab" => vec![48],
                "Escape" => vec![53],
                "Backspace" => vec![51],
                "Delete" => vec![117],
                "Home" => vec![115],
                "End" => vec![119],
                "PageUp" => vec![116],
                "PageDown" => vec![121],
                "ArrowLeft" | "Left" => vec![123],
                "ArrowRight" | "Right" => vec![124],
                "ArrowDown" | "Down" => vec![125],
                "ArrowUp" | "Up" => vec![126],
                "Backquote" => vec![50],
                "Minus" => vec![27],
                "Equal" => vec![24],
                "BracketLeft" => vec![33],
                "BracketRight" => vec![30],
                "Backslash" => vec![42],
                "Semicolon" => vec![41],
                "Quote" => vec![39],
                "Comma" => vec![43],
                "Period" => vec![47],
                "Slash" => vec![44],
                "J" => vec![38],
                "K" => vec![40],
                "L" => vec![37],
                "N" => vec![45],
                "M" => vec![46],
                "NumpadEnter" => vec![76],
                "NumpadAdd" => vec![69],
                "NumpadSubtract" => vec![78],
                "NumpadMultiply" => vec![67],
                "NumpadDivide" => vec![75],
                "NumpadDecimal" => vec![65],
                n if n.starts_with("Numpad")
                    && n.len() == 7
                    && n.as_bytes()[6].is_ascii_digit() =>
                {
                    vec![
                        [82, 83, 84, 85, 86, 87, 88, 89, 91, 92][(n.as_bytes()[6] - b'0') as usize],
                    ]
                }
                n => {
                    let names = [
                        "A",
                        "S",
                        "D",
                        "F",
                        "H",
                        "G",
                        "Z",
                        "X",
                        "C",
                        "V",
                        "IntlBackslash",
                        "B",
                        "Q",
                        "W",
                        "E",
                        "R",
                        "Y",
                        "T",
                        "1",
                        "2",
                        "3",
                        "4",
                        "6",
                        "5",
                        "Equal",
                        "9",
                        "7",
                        "Minus",
                        "8",
                        "0",
                        "BracketRight",
                        "O",
                        "U",
                        "BracketLeft",
                        "I",
                        "P",
                    ];
                    if let Some(i) = names.iter().position(|v| *v == n) {
                        vec![i as u32]
                    } else if let Some(i) = [
                        "F1", "F2", "F3", "F4", "F5", "F6", "F7", "F8", "F9", "F10", "F11", "F12",
                        "F13", "F14", "F15", "F16", "F17", "F18", "F19", "F20",
                    ]
                    .iter()
                    .position(|v| *v == n)
                    {
                        vec![
                            [
                                122, 120, 99, 118, 96, 97, 98, 100, 101, 109, 103, 111, 105, 107,
                                113, 106, 64, 79, 80, 90,
                            ][i],
                        ]
                    } else {
                        bail!("Shortcut needs to be recorded again");
                    }
                }
            })
        })
        .collect()
}
