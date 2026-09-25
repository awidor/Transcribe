use anyhow::{bail, ensure, Context, Result};
use evdev::{uinput::VirtualDevice, AttributeSet, Device, KeyCode, KeyEvent};
use std::{
    path::PathBuf,
    process::Command,
    sync::{Mutex, OnceLock},
    time::{Duration, Instant},
};

pub(crate) const VIRTUAL_KEYBOARD: &str = "Transcribe paste keyboard";
pub const ACCESS_REQUIRED: &str = "Keyboard access required";

// logind grants the active local session access to devices tagged uaccess;
// the rule has to sort before 73-seat-late.rules.
const GRANT: &str = r#"set -e
cat > /etc/udev/rules.d/70-transcribe.rules <<'EOF'
SUBSYSTEM=="input", KERNEL=="event*", ENV{ID_INPUT_KEY}=="1", TAG+="uaccess"
KERNEL=="uinput", SUBSYSTEM=="misc", OPTIONS+="static_node=uinput", TAG+="uaccess"
EOF
udevadm control --reload-rules
udevadm trigger --action=change --subsystem-match=input
udevadm trigger --action=change --subsystem-match=misc --sysname-match=uinput
udevadm settle"#;

// Programs that run their remaining arguments as a command.
const TERMINALS: &[(&str, &[&str])] = &[
    ("xdg-terminal-exec", &[]),
    ("gnome-terminal", &["--wait", "--"]),
    ("konsole", &["--nofork", "-e"]),
    ("kitty", &[]),
    ("foot", &[]),
    ("wezterm", &["start", "--always-new-process", "--"]),
    ("ghostty", &["-e"]),
    ("alacritty", &["-e"]),
    ("xfce4-terminal", &["--disable-server", "-x"]),
    ("xterm", &["-e"]),
];

/// Installs the udev rule as root. Returns false when authentication is dismissed.
pub fn grant_access() -> Result<bool> {
    let status = Command::new("pkexec")
        .args(["/bin/sh", "-c", GRANT])
        .status()
        .context("Keyboard access requires pkexec")?;
    let granted = match status.code() {
        Some(0) => true,
        Some(126) => false,
        // No polkit agent: pkexec authenticates on a terminal instead.
        Some(127) => grant_in_terminal()?,
        _ => bail!("Keyboard access not granted"),
    };
    ensure!(
        !granted || !keyboards().is_empty(),
        "Keyboard access unavailable"
    );
    Ok(granted)
}

fn grant_in_terminal() -> Result<bool> {
    let dir = tempfile::tempdir()?;
    let result = dir.path().join("status");
    // Closing the window or Ctrl+C kills pkexec but still records a status.
    let script = r#"trap : HUP INT; pkexec /bin/sh -c "$1"; echo $? > "$2""#;
    let command = [
        "/bin/sh".as_ref(),
        "-c".as_ref(),
        script.as_ref(),
        "sh".as_ref(),
        GRANT.as_ref(),
        result.as_os_str(),
    ];
    let mut terminal = TERMINALS
        .iter()
        .find_map(|(program, args)| Command::new(program).args(*args).args(command).spawn().ok())
        .context("Keyboard access requires a terminal")?;
    let deadline = Instant::now() + Duration::from_secs(300);
    while Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(250));
        let exited = terminal.try_wait()?;
        match std::fs::read_to_string(&result).map(|code| code.trim().parse::<i32>()) {
            Ok(Ok(0)) => return Ok(true),
            Ok(Ok(126 | 129..)) => return Ok(false),
            Ok(Ok(_)) => bail!("Keyboard access not granted"),
            // A terminal may hand the command to a running instance and exit
            // at once, so only a failed launch ends the wait early.
            _ if exited.is_some_and(|status| !status.success()) => {
                bail!("Terminal unavailable")
            }
            _ => (),
        }
    }
    Ok(false)
}

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
    ensure!(!devices.is_empty(), ACCESS_REQUIRED);
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
            KeyCode::KEY_INSERT,
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

#[derive(Clone, Copy)]
pub(crate) enum PasteShortcut {
    Clipboard,
    TerminalClipboard,
    TerminalSelection,
}

pub(crate) fn paste(shortcut: PasteShortcut) -> Result<()> {
    let mut keyboard = KEYBOARD.get_or_init(|| Mutex::new(None)).lock().unwrap();
    let device = keyboard.as_mut().context("Paste keyboard unavailable")?;
    let keys: &[KeyCode] = match shortcut {
        PasteShortcut::Clipboard => &[KeyCode::KEY_LEFTCTRL, KeyCode::KEY_V],
        PasteShortcut::TerminalClipboard => &[
            KeyCode::KEY_LEFTCTRL,
            KeyCode::KEY_LEFTSHIFT,
            KeyCode::KEY_V,
        ],
        PasteShortcut::TerminalSelection => &[KeyCode::KEY_LEFTSHIFT, KeyCode::KEY_INSERT],
    };
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
