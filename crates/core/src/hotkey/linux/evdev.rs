use super::*;
use crate::linux_input;
use ::evdev::{Device, EventType, KeyCode};
use std::{collections::HashMap, path::PathBuf, str::FromStr};

struct Keyboard {
    device: Device,
    held: BTreeSet<u16>,
}

#[derive(Default)]
struct Held(HashMap<u16, usize>);
impl Held {
    fn input(&mut self, device: &mut BTreeSet<u16>, code: u16, down: bool) -> bool {
        if down {
            if !device.insert(code) {
                return false;
            }
            let count = self.0.entry(code).or_default();
            *count += 1;
            *count == 1
        } else {
            if !device.remove(&code) {
                return false;
            }
            let count = self.0.entry(code).or_default();
            *count = count.saturating_sub(1);
            *count == 0
        }
    }
}

fn key_label(code: u16) -> String {
    match code {
        29 => "Left Ctrl".into(),
        97 => "Right Ctrl".into(),
        42 => "Left Shift".into(),
        54 => "Right Shift".into(),
        56 => "Left Alt".into(),
        100 => "Right Alt".into(),
        125 => "Left Super".into(),
        126 => "Right Super".into(),
        _ => format!("{:?}", KeyCode(code))
            .trim_start_matches("KEY_")
            .replace('_', " "),
    }
}

fn input(service: &Service, code: u16, down: bool) -> bool {
    let (_, events) = service.engine.lock().unwrap().input(
        Key {
            code: code as u32,
            label: key_label(code),
        },
        down,
        matches!(code, 29 | 97 | 42 | 54 | 56 | 100 | 125 | 126),
    );
    let mut activate = false;
    for event in events {
        if matches!(event, Event::Activate) {
            activate = true;
        } else {
            let _ = service.sender.send(event);
        }
    }
    activate
}

fn discover(
    devices: &mut HashMap<PathBuf, Keyboard>,
    held: &mut Held,
    service: &Service,
) -> Result<()> {
    for (path, device) in linux_input::keyboards() {
        if devices.contains_key(&path) {
            continue;
        }
        // Hotplug discovery can race a removal or a session permission change.
        if device.set_nonblocking(true).is_err() {
            continue;
        }
        let Ok(state) = device.get_key_state() else {
            continue;
        };
        let mut keys = BTreeSet::new();
        for key in state.iter().filter(|key| linux_input::is_key(key.code())) {
            if held.input(&mut keys, key.code(), true) {
                input(service, key.code(), true);
            }
        }
        devices.insert(path, Keyboard { device, held: keys });
    }
    Ok(())
}

pub(super) fn start(service: Arc<Service>) -> Result<()> {
    let mut devices = HashMap::new();
    let mut held = Held::default();
    discover(&mut devices, &mut held, &service)?;
    anyhow::ensure!(
        !devices.is_empty(),
        "Keyboard access required: cannot read /dev/input/event* key devices"
    );
    std::thread::Builder::new()
        .name("hotkey-evdev".into())
        .spawn(move || {
            let result = (|| -> Result<()> {
                let mut refresh = Instant::now();
                let mut pending = Vec::new();
                loop {
                    let mut removed = Vec::new();
                    for (path, keyboard) in &mut devices {
                        let fetched = keyboard
                            .device
                            .fetch_events()
                            .map(|events| events.collect::<Vec<_>>());
                        match fetched {
                            Ok(events) => {
                                // evdev synthesizes releases when a device disappears.
                                // Those releases are cancellation, not a shortcut tap.
                                if !events.is_empty() && keyboard.device.get_key_state().is_err() {
                                    removed.push(path.clone());
                                    continue;
                                }
                                for event in events {
                                    if event.event_type() == EventType::KEY
                                        && linux_input::is_key(event.code())
                                        && matches!(event.value(), 0 | 1)
                                        && held.input(
                                            &mut keyboard.held,
                                            event.code(),
                                            event.value() == 1,
                                        )
                                    {
                                        if input(&service, event.code(), event.value() == 1) {
                                            // The kernel releases held keys just before removing
                                            // a device. Confirm it survived before activating.
                                            pending.push((path.clone(), Instant::now()));
                                        }
                                    }
                                }
                            }
                            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => (),
                            Err(_) => removed.push(path.clone()),
                        }
                    }
                    if !removed.is_empty() {
                        // Unplugging a held key must cancel the stroke, never activate it.
                        service.reset_input();
                        held = Held::default();
                        for path in removed {
                            devices.remove(&path);
                            pending.retain(|(source, _)| source != &path);
                        }
                        for keyboard in devices.values_mut() {
                            for code in &keyboard.held {
                                let count = held.0.entry(*code).or_default();
                                *count += 1;
                                if *count == 1 {
                                    input(&service, *code, true);
                                }
                            }
                        }
                        service.engine.lock().unwrap().reset();
                    }
                    pending.retain(|(path, released)| {
                        if released.elapsed() < Duration::from_millis(40) {
                            return true;
                        }
                        if devices
                            .get(path)
                            .is_some_and(|keyboard| keyboard.device.get_key_state().is_ok())
                        {
                            service.activate();
                        }
                        false
                    });
                    if refresh.elapsed() >= Duration::from_millis(500) {
                        discover(&mut devices, &mut held, &service)?;
                        refresh = Instant::now();
                    }
                    std::thread::sleep(Duration::from_millis(5));
                }
            })();
            if let Err(error) = result {
                service.failed(format!("Keyboard listener stopped: {error}"));
            }
        })?;
    Ok(())
}

fn dom_key(name: &str) -> Result<u32> {
    let key = match name {
        "ControlLeft" => "LEFTCTRL",
        "ControlRight" => "RIGHTCTRL",
        "ShiftLeft" => "LEFTSHIFT",
        "ShiftRight" => "RIGHTSHIFT",
        "AltLeft" => "LEFTALT",
        "AltRight" => "RIGHTALT",
        "MetaLeft" => "LEFTMETA",
        "MetaRight" => "RIGHTMETA",
        "Enter" => "ENTER",
        "Escape" => "ESC",
        "Space" => "SPACE",
        "Backspace" => "BACKSPACE",
        "CapsLock" => "CAPSLOCK",
        "PageUp" => "PAGEUP",
        "PageDown" => "PAGEDOWN",
        "ArrowLeft" => "LEFT",
        "ArrowRight" => "RIGHT",
        "ArrowUp" => "UP",
        "ArrowDown" => "DOWN",
        "Backquote" => "GRAVE",
        "Equal" => "EQUAL",
        "Minus" => "MINUS",
        "BracketLeft" => "LEFTBRACE",
        "BracketRight" => "RIGHTBRACE",
        "Backslash" => "BACKSLASH",
        "Semicolon" => "SEMICOLON",
        "Quote" => "APOSTROPHE",
        "Comma" => "COMMA",
        "Period" => "DOT",
        "Slash" => "SLASH",
        "NumpadEnter" => "KPENTER",
        "NumpadAdd" => "KPPLUS",
        "NumpadSubtract" => "KPMINUS",
        "NumpadMultiply" => "KPASTERISK",
        "NumpadDivide" => "KPSLASH",
        "NumpadDecimal" => "KPDOT",
        "PrintScreen" => "SYSRQ",
        "ScrollLock" => "SCROLLLOCK",
        "NumLock" => "NUMLOCK",
        "AudioVolumeUp" => "VOLUMEUP",
        "AudioVolumeDown" => "VOLUMEDOWN",
        "AudioVolumeMute" => "MUTE",
        "MediaPlayPause" => "PLAYPAUSE",
        "MediaStop" => "STOPCD",
        "MediaTrackNext" => "NEXTSONG",
        "MediaTrackPrevious" => "PREVIOUSSONG",
        "ContextMenu" => "COMPOSE",
        "IntlBackslash" => "102ND",
        "IntlRo" => "RO",
        "IntlYen" => "YEN",
        "Convert" => "HENKAN",
        "NonConvert" => "MUHENKAN",
        "KanaMode" => "KATAKANAHIRAGANA",
        "Lang1" => "HANGEUL",
        "Lang2" => "HANJA",
        "Lang3" => "KATAKANA",
        "Lang4" => "HIRAGANA",
        "Lang5" => "ZENKAKUHANKAKU",
        n if n.starts_with("Key") && n.len() == 4 => &n[3..],
        n if n.starts_with("Digit") && n.len() == 6 => &n[5..],
        n if n.starts_with("Numpad") && n.len() == 7 => {
            return named_key(&format!("KP{}", &n[6..]))
        }
        n => n,
    };
    named_key(key)
}

fn named_key(name: &str) -> Result<u32> {
    KeyCode::from_str(&format!("KEY_{}", name.to_uppercase()))
        .map(|key| key.code() as u32)
        .context("Shortcut needs to be recorded again")
}

pub(crate) fn binding(text: &str) -> Result<Vec<Vec<u32>>> {
    let keys = if text.starts_with('{') {
        let binding: Binding = serde_json::from_str(text).context("Invalid shortcut")?;
        binding
            .keys
            .into_iter()
            .map(|key| {
                Ok(vec![match binding.platform.as_str() {
                    "linux-evdev" => key.code,
                    "wayland" => dom_key(&key.label)?,
                    "linux" => key.code.checked_sub(8).context("Invalid X11 key")?,
                    _ => bail!("Shortcut belongs to another platform"),
                }])
            })
            .collect::<Result<Vec<_>>>()?
    } else {
        text.split('+')
            .map(|name| {
                Ok(match name {
                    "CommandOrControl" | "Control" | "Ctrl" => vec![29, 97],
                    "Shift" => vec![42, 54],
                    "Alt" => vec![56, 100],
                    "Super" | "Meta" | "Command" => vec![125, 126],
                    n => vec![dom_key(n)?],
                })
            })
            .collect::<Result<Vec<_>>>()?
    };
    anyhow::ensure!(!keys.is_empty() && keys.len() <= 512, "Invalid shortcut");
    let mut seen = BTreeSet::new();
    for code in keys.iter().flatten() {
        anyhow::ensure!(
            *code <= u16::MAX as u32 && linux_input::is_key(*code as u16),
            "Unsupported Linux key"
        );
        anyhow::ensure!(seen.insert(*code), "Duplicate shortcut key");
    }
    Ok(keys)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn migrates_right_alt_without_losing_its_side() {
        assert_eq!(
            binding(r#"{"platform":"wayland","keys":[{"code":0,"label":"AltRight"}]}"#).unwrap(),
            vec![vec![100]]
        );
    }
    #[test]
    fn supports_single_keys_media_and_arbitrary_chords() {
        for codes in [
            vec![30],
            vec![100],
            vec![183],
            vec![464],
            vec![30, 48, 100],
            vec![29, 97],
        ] {
            let text = serde_json::to_string(&Binding {
                platform: "linux-evdev".into(),
                keys: codes
                    .iter()
                    .map(|code| Key {
                        code: *code,
                        label: "".into(),
                    })
                    .collect(),
            })
            .unwrap();
            assert_eq!(
                binding(&text).unwrap(),
                codes.into_iter().map(|code| vec![code]).collect::<Vec<_>>()
            );
        }
        assert_eq!(
            binding("CommandOrControl+Shift+Space").unwrap(),
            vec![vec![29, 97], vec![42, 54], vec![57]]
        );
    }
    #[test]
    fn shared_key_on_two_keyboards_releases_only_after_both() {
        let mut held = Held::default();
        let (mut a, mut b) = (BTreeSet::new(), BTreeSet::new());
        assert!(held.input(&mut a, 100, true));
        assert!(!held.input(&mut a, 100, true));
        assert!(!held.input(&mut b, 100, true));
        assert!(!held.input(&mut a, 100, false));
        assert!(held.input(&mut b, 100, false));
    }
}
