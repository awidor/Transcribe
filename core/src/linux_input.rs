//! Keyboard devices and the virtual paste keyboard on Linux, through evdev and uinput.
use anyhow::{bail, Context, Result};
use evdev::{
    uinput::VirtualDevice, AttributeSet, Device, EventType, InputEvent, KeyCode, KeyEvent,
};
use std::{
    path::PathBuf,
    process::{Command, Stdio},
    sync::{Mutex, OnceLock},
    time::Duration,
};

pub(crate) const VIRTUAL_KEYBOARD: &str = "Transcribe paste keyboard";
pub const ACCESS_REQUIRED: &str = "Keyboard access required";

/// Every readable keyboard except the paste keyboard.
pub(crate) fn keyboards() -> Vec<(PathBuf, Device)> {
    evdev::enumerate()
        .filter(|(_, device)| {
            device.name() != Some(VIRTUAL_KEYBOARD)
                && device
                    .supported_keys()
                    .is_some_and(|keys| keys.iter().any(|key| is_key(key.code())))
        })
        .collect()
}

pub(crate) fn is_key(code: u16) -> bool {
    // EV_KEY also contains mouse and gamepad buttons.
    (1..0x100).contains(&code) || (0x160..=0x2ff).contains(&code)
}

static KEYBOARD: OnceLock<Mutex<Option<VirtualDevice>>> = OnceLock::new();

fn keyboard() -> std::sync::MutexGuard<'static, Option<VirtualDevice>> {
    KEYBOARD
        .get_or_init(|| Mutex::new(None))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

pub(crate) fn prepare_keyboard() -> Result<()> {
    let mut keyboard = keyboard();
    if keyboard.is_none() {
        let keys = [
            KeyCode::KEY_LEFTCTRL,
            KeyCode::KEY_LEFTSHIFT,
            KeyCode::KEY_V,
        ]
        .into_iter()
        .collect::<AttributeSet<_>>();
        let mut device = VirtualDevice::builder()
            .context("Automatic paste requires write access to /dev/uinput")?
            .name(VIRTUAL_KEYBOARD)
            .with_keys(&keys)?
            .build()
            .context("Automatic paste requires write access to /dev/uinput")?;
        // Wait for the input node, then let the compositor discover the device.
        let _ = device.enumerate_dev_nodes_blocking()?.next().transpose()?;
        std::thread::sleep(Duration::from_millis(250));
        *keyboard = Some(device);
    }
    Ok(())
}

/// Taps Ctrl while the shortcut's Alt or Super is held, as keyd does. The
/// desktop then sees a combination instead of a lone tap, which would open a
/// menu bar or the overview on release.
pub(crate) fn mask() -> Result<()> {
    let mut keyboard = keyboard();
    let device = keyboard.as_mut().context("Paste keyboard unavailable")?;
    device.emit(&[*KeyEvent::new(KeyCode::KEY_LEFTCTRL, 1)])?;
    device.emit(&[*KeyEvent::new(KeyCode::KEY_LEFTCTRL, 0)])?;
    Ok(())
}

/// The paste tools tried in order, each an argv; the first that exits with 0 wins.
pub(crate) fn paste_commands(
    wayland: bool,
    kde: bool,
    terminal: bool,
    legacy_ydotool: bool,
) -> Vec<Vec<String>> {
    let keys = if terminal { "ctrl+shift+v" } else { "ctrl+v" };
    let owned = |argv: &[&str]| argv.iter().map(|arg| arg.to_string()).collect::<Vec<_>>();
    let mut commands = Vec::new();
    if wayland {
        // KWin advertises wtype's protocol but ignores its keys.
        if !terminal && !kde {
            commands.push(owned(&["wtype", "-M", "ctrl", "-k", "v", "-m", "ctrl"]));
        }
        commands.push(owned(&["sh", "-c", &format!("echo key {keys} | dotool")]));
    } else {
        commands.push(owned(&["xdotool", "key", "--clearmodifiers", keys]));
    }
    let mut ydotool = owned(&["ydotool", "key"]);
    if legacy_ydotool {
        ydotool.push(keys.into());
    } else if terminal {
        ydotool.extend(owned(&["29:1", "42:1", "47:1", "47:0", "42:0", "29:0"]));
    } else {
        ydotool.extend(owned(&["29:1", "47:1", "47:0", "29:0"]));
    }
    commands.push(ydotool);
    commands
}

/// Runs `argv` from PATH with no input; its exit code and combined output.
fn run(argv: &[String]) -> Option<(i32, String)> {
    let output = Command::new(&argv[0])
        .args(&argv[1..])
        .stdin(Stdio::null())
        .output()
        .ok()?;
    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&output.stderr));
    Some((output.status.code()?, text))
}

/// Each key event and its report go out 25 ms apart; every release is
/// attempted, even when a press failed.
fn send(mut write: impl FnMut(&[InputEvent]) -> std::io::Result<()>, terminal: bool) -> Result<()> {
    let mut keys = vec![KeyCode::KEY_LEFTCTRL];
    if terminal {
        keys.push(KeyCode::KEY_LEFTSHIFT);
    }
    keys.push(KeyCode::KEY_V);
    let events = keys
        .iter()
        .map(|key| (*key, 1))
        .chain(keys.iter().rev().map(|key| (*key, 0)));
    let mut failure = None;
    for (key, value) in events {
        let result = write(&[*KeyEvent::new(key, value)]);
        if failure.is_none() {
            failure = result.err();
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    match failure {
        Some(error) => Err(error.into()),
        None => Ok(()),
    }
}

/// Ctrl+V, or Ctrl+Shift+V in a terminal, through the paste tools, then the
/// paste keyboard, then a physical keyboard.
pub(crate) fn paste(terminal: bool) -> Result<()> {
    // ydotool 0.1 takes key names; later versions take raw keycodes.
    static LEGACY_YDOTOOL: OnceLock<bool> = OnceLock::new();
    let legacy_ydotool = *LEGACY_YDOTOOL.get_or_init(|| {
        run(&["ydotool".into(), "key".into(), "--help".into()])
            .is_some_and(|(_, help)| help.contains("separated by plus"))
    });
    let kde = std::env::var("XDG_CURRENT_DESKTOP").is_ok_and(|desktop| desktop.contains("KDE"));
    let wayland = crate::is_wayland();
    for command in paste_commands(wayland, kde, terminal, legacy_ydotool) {
        if run(&command).is_some_and(|(code, _)| code == 0) {
            return Ok(());
        }
    }
    if prepare_keyboard().is_ok() {
        let mut keyboard = keyboard();
        if let Some(device) = keyboard.as_mut() {
            return send(|events| device.emit(events), terminal);
        }
    }
    for (_, mut device) in keyboards() {
        if !device
            .supported_keys()
            .is_some_and(|keys| keys.contains(KeyCode::KEY_V))
        {
            continue;
        }
        let syn = InputEvent::new(EventType::SYNCHRONIZATION.0, 0, 0);
        // A device opened read-only refuses the first event.
        let mut probe = || device.send_events(&[syn]);
        if probe().is_err() {
            continue;
        }
        return send(
            |events| {
                device.send_events(events)?;
                device.send_events(&[syn])
            },
            terminal,
        );
    }
    bail!(ACCESS_REQUIRED)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paste_tools_follow_the_session_and_the_receiver() {
        let first =
            |wayland, kde, terminal| paste_commands(wayland, kde, terminal, false)[0].join(" ");
        assert_eq!(first(true, false, false), "wtype -M ctrl -k v -m ctrl");
        assert_eq!(first(true, true, false), "sh -c echo key ctrl+v | dotool");
        assert_eq!(
            first(true, false, true),
            "sh -c echo key ctrl+shift+v | dotool"
        );
        assert_eq!(
            first(false, false, true),
            "xdotool key --clearmodifiers ctrl+shift+v"
        );
        assert_eq!(
            paste_commands(false, false, false, false)
                .last()
                .unwrap()
                .join(" "),
            "ydotool key 29:1 47:1 47:0 29:0"
        );
        assert_eq!(
            paste_commands(false, false, true, true)
                .last()
                .unwrap()
                .join(" "),
            "ydotool key ctrl+shift+v"
        );
    }
}
