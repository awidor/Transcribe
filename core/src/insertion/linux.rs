//! Pastes through the clipboard: the transcript replaces it, a paste
//! keystroke goes to the app in front, and the clipboard comes back after.
use anyhow::{Context, Result};
use std::{collections::VecDeque, time::Duration};
use wl_clipboard_rs::{copy, paste};
use x11rb::{
    connection::Connection,
    protocol::{
        xproto::{AtomEnum, ConnectionExt as _},
        xtest::ConnectionExt as _,
    },
};

mod clipboard;
mod focus;
mod xselection;

use crate::is_wayland;

pub struct Target {
    terminal: bool,
}

/// Marks this selection, so a copy made while pasting, even of identical
/// text, is kept.
const MARKER: &str = "application/x-transcribe";

pub(crate) fn is_terminal(app: &str) -> bool {
    let app = app.to_lowercase();
    if app
        .split('\0')
        .any(|part| part == "st" || part == "st-256color")
    {
        return true;
    }
    [
        "terminal",
        "konsole",
        "kitty",
        "wezterm",
        "alacritty",
        "xterm",
        "ghostty",
        "tilix",
        "terminator",
        "urxvt",
        "rxvt",
        "foot",
        "ptyxis",
        "org.gnome.console",
        "kgx",
        "yakuake",
        "guake",
        "tilda",
        "terminology",
        "termite",
        "termit",
        "contour",
        "com.raggesilver.blackbox",
        "com.system76.cosmicterm",
        "com.mitchellh.ghostty",
        "com.raphaelamorim.rio",
    ]
    .iter()
    .any(|name| app.contains(name))
}

async fn a11y_connection() -> Result<zbus::Connection> {
    let session = zbus::Connection::session().await?;
    let bus = zbus::Proxy::new(&session, "org.a11y.Bus", "/org/a11y/bus", "org.a11y.Bus").await?;
    let address: String = bus.call("GetAddress", &()).await?;
    Ok(zbus::connection::Builder::address(address.as_str())?
        .build()
        .await?)
}

/// True when the focused element, found within `timeout`, is in a terminal.
async fn in_terminal(timeout: Duration) -> bool {
    let search = async {
        let c = a11y_connection().await?;
        let root = zbus::Proxy::new(
            &c,
            "org.a11y.atspi.Registry",
            "/org/a11y/atspi/accessible/root",
            "org.a11y.atspi.Accessible",
        )
        .await?;
        let apps: Vec<(String, zbus::zvariant::OwnedObjectPath)> =
            root.call("GetChildren", &()).await?;
        let mut queue: VecDeque<_> = apps.into_iter().map(|(b, p)| (b, p, 0, false)).collect();
        let mut count = 0;
        while let Some((bus, path, depth, terminal)) = queue.pop_front() {
            count += 1;
            if count > 3000 {
                break;
            }
            let Ok(p) =
                zbus::Proxy::new(&c, bus.as_str(), path.as_str(), "org.a11y.atspi.Accessible")
                    .await
            else {
                continue;
            };
            let states: Vec<u32> = p.call("GetState", &()).await.unwrap_or_default();
            let state = states.first().copied().unwrap_or(0);
            // Only descend into the active top-level window. Inactive
            // applications retain stale focused children.
            if depth == 1 && state & (1 << 1) == 0 {
                continue;
            }
            let role: String = p.call("GetRoleName", &()).await.unwrap_or_default();
            let terminal = terminal
                || role == "terminal"
                || p.get_property::<String>("Name")
                    .await
                    .unwrap_or_default()
                    .eq_ignore_ascii_case("terminal");
            if state & (1 << 12) != 0 {
                return Ok(terminal);
            }
            if depth < 30 {
                let children: Vec<(String, zbus::zvariant::OwnedObjectPath)> =
                    p.call("GetChildren", &()).await.unwrap_or_default();
                queue.extend(
                    children
                        .into_iter()
                        .map(|(b, p)| (b, p, depth + 1, terminal)),
                );
            }
        }
        anyhow::Ok(false)
    };
    tokio::time::timeout(timeout, search)
        .await
        .ok()
        .and_then(Result::ok)
        .unwrap_or(false)
}

fn x_terminal() -> Result<bool> {
    let (c, screen) = x11rb::connect(None)?;
    let root = c.setup().roots[screen].root;
    let atom = c.intern_atom(false, b"_NET_ACTIVE_WINDOW")?.reply()?.atom;
    let Some(window) = c
        .get_property(false, root, atom, AtomEnum::WINDOW, 0, 1)?
        .reply()?
        .value32()
        .and_then(|mut values| values.next())
    else {
        return Ok(false);
    };
    let class = c
        .get_property(false, window, AtomEnum::WM_CLASS, AtomEnum::STRING, 0, 1024)?
        .reply()?
        .value;
    Ok(is_terminal(&String::from_utf8_lossy(&class)))
}

/// The last resort on X11, as enigo sends it.
fn x_press(terminal: bool) -> Result<()> {
    let (c, screen) = x11rb::connect(None)?;
    let setup = c.setup();
    let root = setup.roots[screen].root;
    let (min, max) = (setup.min_keycode, setup.max_keycode);
    let mapping = c.get_keyboard_mapping(min, max - min + 1)?.reply()?;
    let per = mapping.keysyms_per_keycode as usize;
    let code = |symbol: u32| {
        mapping
            .keysyms
            .iter()
            .position(|&s| s == symbol)
            .filter(|_| per > 0)
            .map(|index| min + (index / per) as u8)
            .context("Paste shortcut unavailable")
    };
    let (control, shift, v) = (code(0xffe3)?, code(0xffe1)?, code(0x76)?);
    let keys = if terminal {
        vec![control, shift, v]
    } else {
        vec![control, v]
    };
    let mut pressed = Vec::new();
    let mut result = Ok(());
    for key in keys {
        match c
            .xtest_fake_input(2, key, 0, root, 0, 0, 0)
            .map_err(anyhow::Error::from)
            .and_then(|cookie| cookie.check().map_err(anyhow::Error::from))
        {
            Ok(()) => pressed.push(key),
            Err(error) => {
                result = Err(error);
                break;
            }
        }
    }
    c.flush()?;
    if result.is_ok() {
        std::thread::sleep(Duration::from_millis(100));
    }
    for key in pressed.into_iter().rev() {
        c.xtest_fake_input(3, key, 0, root, 0, 0, 0)?;
    }
    c.flush()?;
    result
}

pub async fn capture() -> Result<Target> {
    let terminal = if is_wayland() {
        let app = focus::capture()
            .await
            .ok()
            .flatten()
            .map(|target| target.app_id);
        app.is_some_and(|app| is_terminal(&app)) || in_terminal(Duration::from_millis(250)).await
    } else {
        tokio::task::spawn_blocking(x_terminal)
            .await?
            .unwrap_or(false)
            || in_terminal(Duration::from_secs(3)).await
    };
    Ok(Target { terminal })
}

pub async fn paste(target: Target, text: String) -> Result<()> {
    let text = super::prepare_text(&text, target.terminal);
    // An unreadable clipboard keeps the transcript rather than being restored empty.
    let saved = clipboard::snapshot(paste::ClipboardType::Regular)
        .await
        .ok();
    let mut data = clipboard::plain_text(text);
    data.insert(MARKER.into(), Vec::new());
    clipboard::write(copy::ClipboardType::Regular, data).await?;
    let terminal = target.terminal;
    let dispatch = async {
        let sent = tokio::task::spawn_blocking(move || crate::linux_input::paste(terminal)).await?;
        if let Err(error) = sent {
            if is_wayland() {
                return Err(error);
            }
            tokio::task::spawn_blocking(move || x_press(terminal)).await??;
        }
        // Keep the selection alive while the destination requests it.
        tokio::time::sleep(Duration::from_millis(500)).await;
        Ok(())
    }
    .await;
    let restored = match saved {
        Some(saved) => clipboard::restore(paste::ClipboardType::Regular, saved, MARKER).await,
        None => Ok(()),
    };
    dispatch?;
    restored
}

pub async fn copy(text: String) -> Result<()> {
    clipboard::write(copy::ClipboardType::Regular, clipboard::plain_text(text)).await
}

pub async fn prepare() {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_terminal_app_ids_and_x11_classes() {
        for class in [
            "Alacritty",
            "org.kde.konsole",
            "xterm\0XTerm\0",
            "st\0St\0",
            "foot",
            "footclient",
            "org.gnome.Ptyxis",
            "org.gnome.Console",
            "com.system76.CosmicTerm",
            "com.raphaelamorim.rio",
        ] {
            assert!(is_terminal(class), "{class:?}");
        }
        for class in [
            "firefox",
            "org.kde.kate",
            "transcribe",
            "org.example.Postman",
        ] {
            assert!(!is_terminal(class), "{class:?}");
        }
    }
}
