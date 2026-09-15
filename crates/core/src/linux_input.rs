use anyhow::{ensure, Context, Result};
use evdev::{uinput::VirtualDevice, AttributeSet, Device, KeyCode, KeyEvent};
use std::{
    path::PathBuf,
    sync::{Mutex, OnceLock},
    time::{Duration, Instant},
};

pub(crate) const VIRTUAL_KEYBOARD: &str = "Transcribe paste keyboard";

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

pub(crate) fn wait_released() -> Result<()> {
    let devices = keyboards();
    ensure!(
        !devices.is_empty(),
        "Keyboard access required: cannot read /dev/input/event* key devices"
    );
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let held = devices.iter().try_fold(false, |held, (_, device)| {
            Ok::<_, std::io::Error>(
                held || device.get_key_state()?.iter().any(|key| is_key(key.code())),
            )
        })?;
        if !held {
            return Ok(());
        }
        ensure!(
            Instant::now() < deadline,
            "Release the keyboard before pasting"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

static KEYBOARD: OnceLock<Mutex<Option<VirtualDevice>>> = OnceLock::new();

pub(crate) fn prepare_paste() -> Result<()> {
    let mut keyboard = KEYBOARD.get_or_init(|| Mutex::new(None)).lock().unwrap();
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
            .build()?;
        // Wait for the input node, then let the compositor discover the device.
        let _ = device.enumerate_dev_nodes_blocking()?.next().transpose()?;
        std::thread::sleep(Duration::from_millis(250));
        *keyboard = Some(device);
    }
    Ok(())
}

pub(crate) fn paste(terminal: bool) -> Result<()> {
    let mut keyboard = KEYBOARD.get_or_init(|| Mutex::new(None)).lock().unwrap();
    let device = keyboard.as_mut().context("Paste keyboard unavailable")?;
    let keys = [
        Some(KeyCode::KEY_LEFTCTRL),
        terminal.then_some(KeyCode::KEY_LEFTSHIFT),
        Some(KeyCode::KEY_V),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>();
    let press = device.emit(
        &keys
            .iter()
            .map(|key| *KeyEvent::new(*key, 1))
            .collect::<Vec<_>>(),
    );
    // Always attempt every release, even when the press failed partway through.
    let release = device.emit(
        &keys
            .iter()
            .rev()
            .map(|key| *KeyEvent::new(*key, 0))
            .collect::<Vec<_>>(),
    );
    press?;
    release?;
    Ok(())
}
