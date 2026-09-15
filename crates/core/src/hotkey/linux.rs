use super::*;
mod evdev;
pub(super) use evdev::binding as evdev_binding;
use std::collections::BTreeMap;
use x11rb::{
    connection::Connection,
    protocol::{
        xinput::{self, ConnectionExt as _},
        xproto::ConnectionExt as _,
        Event as XEvent,
    },
};

fn names(symbol: u32) -> String {
    match symbol {
        0xffe1 => "Left Shift",
        0xffe2 => "Right Shift",
        0xffe3 => "Left Ctrl",
        0xffe4 => "Right Ctrl",
        0xffe9 => "Left Alt",
        0xffea => "Right Alt",
        0xffeb => "Left Super",
        0xffec => "Right Super",
        0xfe03 => "AltGr",
        0xff0d => "Enter",
        0xff8d => "Numpad Enter",
        0xff1b => "Escape",
        0xff09 => "Tab",
        0xff08 => "Backspace",
        0xffff => "Delete",
        0xff63 => "Insert",
        0xff50 => "Home",
        0xff57 => "End",
        0xff51 => "Left",
        0xff52 => "Up",
        0xff53 => "Right",
        0xff54 => "Down",
        0xff55 => "Page Up",
        0xff56 => "Page Down",
        0xffe5 => "Caps Lock",
        0xff7f => "Num Lock",
        0xff14 => "Scroll Lock",
        0xff13 => "Pause",
        0xff61 => "Print Screen",
        32 => "Space",
        0x1008ff11 => "Volume Down",
        0x1008ff12 => "Mute",
        0x1008ff13 => "Volume Up",
        0x1008ff14 => "Play / Pause",
        0x1008ff15 => "Media Stop",
        0x1008ff16 => "Previous Track",
        0x1008ff17 => "Next Track",
        _ => "",
    }
    .to_string()
}
fn label(symbol: u32, code: u32) -> String {
    let name = names(symbol);
    if !name.is_empty() {
        return name;
    }
    if (0xffbe..=0xffe0).contains(&symbol) {
        return format!("F{}", symbol - 0xffbe + 1);
    }
    if (33..=126).contains(&symbol) {
        return char::from_u32(symbol).unwrap().to_uppercase().to_string();
    }
    format!("Key {code}")
}
fn mapping(conn: &x11rb::rust_connection::RustConnection) -> Result<BTreeMap<u32, u32>> {
    let min = conn.setup().min_keycode;
    let reply = conn
        .get_keyboard_mapping(min, conn.setup().max_keycode - min + 1)?
        .reply()?;
    Ok(reply
        .keysyms
        .chunks(reply.keysyms_per_keycode as usize)
        .enumerate()
        .map(|(i, syms)| (min as u32 + i as u32, syms[0]))
        .collect())
}
pub fn start(service: Arc<Service>) -> Result<()> {
    if crate::insertion::linux::is_wayland() {
        return evdev::start(service);
    }
    let (conn, screen) = x11rb::connect(None).context("X11 keyboard connection unavailable")?;
    conn.xinput_xi_query_version(2, 2)?.reply()?;
    conn.xinput_xi_select_events(
        conn.setup().roots[screen].root,
        &[
            xinput::EventMask {
                deviceid: xinput::Device::ALL_MASTER.into(),
                mask: vec![
                    xinput::XIEventMask::RAW_KEY_PRESS | xinput::XIEventMask::RAW_KEY_RELEASE,
                ],
            },
            xinput::EventMask {
                deviceid: xinput::Device::ALL.into(),
                mask: vec![xinput::XIEventMask::HIERARCHY],
            },
        ],
    )?
    .check()?;
    let mut keys = mapping(&conn)?;
    conn.flush()?;
    std::thread::Builder::new()
        .name("hotkey-x11".into())
        .spawn(move || {
            let result = (|| -> Result<()> {
                let mut injected = BTreeSet::new();
                let mut refresh_devices = true;
                loop {
                    if refresh_devices {
                        injected = conn
                            .xinput_xi_query_device(xinput::Device::ALL)?
                            .reply()?
                            .infos
                            .iter()
                            .filter(|d| String::from_utf8_lossy(&d.name).contains("XTEST"))
                            .map(|d| d.deviceid)
                            .collect();
                        refresh_devices = false;
                    }
                    match conn.wait_for_event()? {
                        XEvent::XinputRawKeyPress(e) | XEvent::XinputRawKeyRelease(e) => {
                            if injected.contains(&e.sourceid)
                                || e.flags.contains(xinput::KeyEventFlags::KEY_REPEAT)
                            {
                                continue;
                            }
                            let symbol = *keys.get(&e.detail).unwrap_or(&0);
                            service.input(
                                e.detail,
                                label(symbol, e.detail),
                                e.event_type == xinput::RAW_KEY_PRESS_EVENT,
                                matches!(symbol, 0xffe1..=0xffee | 0xfe03),
                            );
                        }
                        XEvent::MappingNotify(_) => {
                            keys = mapping(&conn)?;
                        }
                        XEvent::XinputHierarchy(_) => {
                            refresh_devices = true;
                        }
                        _ => (),
                    }
                }
            })();
            if let Err(e) = result {
                service.failed(format!("Keyboard listener stopped: {e}"));
            }
        })?;
    Ok(())
}
pub fn validate(keys: &BTreeSet<u32>) -> Result<()> {
    if keys.iter().any(|k| !(8..=255).contains(k)) {
        bail!("Unsupported X11 key");
    }
    Ok(())
}
pub fn legacy(text: &str) -> Result<Vec<Vec<u32>>> {
    let (conn, _) = x11rb::connect(None).context("X11 keyboard connection unavailable")?;
    let map = mapping(&conn)?;
    text.split('+')
        .map(|name| {
            let symbols = match name {
                "CommandOrControl" | "Control" | "Ctrl" => vec![0xffe3, 0xffe4],
                "Shift" => vec![0xffe1, 0xffe2],
                "Alt" => vec![0xffe9, 0xffea],
                "Super" | "Meta" | "Command" => vec![0xffeb, 0xffec],
                "Space" => vec![32],
                "Enter" => vec![0xff0d],
                "Escape" => vec![0xff1b],
                "Tab" => vec![0xff09],
                "Backspace" => vec![0xff08],
                "Delete" => vec![0xffff],
                "Insert" => vec![0xff63],
                "Home" => vec![0xff50],
                "End" => vec![0xff57],
                "PageUp" => vec![0xff55],
                "PageDown" => vec![0xff56],
                "ArrowLeft" | "Left" => vec![0xff51],
                "ArrowUp" | "Up" => vec![0xff52],
                "ArrowRight" | "Right" => vec![0xff53],
                "ArrowDown" | "Down" => vec![0xff54],
                "Backquote" => vec![96],
                "Minus" => vec![45],
                "Equal" => vec![61],
                "BracketLeft" => vec![91],
                "BracketRight" => vec![93],
                "Backslash" => vec![92],
                "Semicolon" => vec![59],
                "Quote" => vec![39],
                "Comma" => vec![44],
                "Period" => vec![46],
                "Slash" => vec![47],
                "NumpadEnter" => vec![0xff8d],
                "NumpadAdd" => vec![0xffab],
                "NumpadSubtract" => vec![0xffad],
                "NumpadMultiply" => vec![0xffaa],
                "NumpadDivide" => vec![0xffaf],
                "NumpadDecimal" => vec![0xffae, 0xff9f],
                n if n.starts_with("Numpad")
                    && n.len() == 7
                    && n.as_bytes()[6].is_ascii_digit() =>
                {
                    let digit = (n.as_bytes()[6] - b'0') as usize;
                    vec![
                        0xffb0 + digit as u32,
                        [
                            0xff9e, 0xff9c, 0xff99, 0xff9b, 0xff96, 0xff9d, 0xff98, 0xff95, 0xff97,
                            0xff9a,
                        ][digit],
                    ]
                }
                n if n.len() == 1 && n.as_bytes()[0].is_ascii_alphanumeric() => {
                    vec![n.as_bytes()[0].to_ascii_lowercase() as u32]
                }
                n if n.starts_with('F')
                    && n[1..].parse::<u32>().is_ok_and(|v| (1..=24).contains(&v)) =>
                {
                    vec![0xffbd + n[1..].parse::<u32>()?]
                }
                _ => bail!("Shortcut needs to be recorded again"),
            };
            let codes: Vec<_> = map
                .iter()
                .filter(|(_, v)| symbols.contains(v))
                .map(|(k, _)| *k)
                .collect();
            if codes.is_empty() {
                bail!("Shortcut key is unavailable on this keyboard");
            }
            Ok(codes)
        })
        .collect()
}
