use anyhow::{bail, Context, Result};
use clipboard_rs::{Clipboard as ClipboardApi, ClipboardContent, ClipboardContext};
use std::{
    collections::VecDeque,
    sync::{Mutex, OnceLock},
    time::Duration,
};
use x11rb::{
    connection::Connection,
    protocol::{xproto::ConnectionExt, xtest::ConnectionExt as _},
};

mod kwin;
mod wayland;

pub fn is_wayland() -> bool {
    std::env::var_os("WAYLAND_DISPLAY").is_some()
}
#[derive(Clone, PartialEq)]
struct Accessible {
    bus: String,
    path: zbus::zvariant::OwnedObjectPath,
    terminal: bool,
}
#[derive(Clone)]
pub struct Target {
    x11: Option<(u32, u32)>,
    accessible: Option<Accessible>,
    terminal: bool,
    kwin: Option<kwin::Window>,
}
async fn a11y_connection() -> Result<zbus::Connection> {
    let session = zbus::Connection::session().await?;
    let bus = zbus::Proxy::new(&session, "org.a11y.Bus", "/org/a11y/bus", "org.a11y.Bus").await?;
    let address: String = bus.call("GetAddress", &()).await?;
    Ok(zbus::connection::Builder::address(address.as_str())?
        .build()
        .await?)
}
async fn focused() -> Result<Accessible> {
    tokio::time::timeout(Duration::from_secs(3), async {
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
            // Only descend into the active top-level window. Inactive applications retain stale focused children.
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
                return Ok(Accessible {
                    bus,
                    path,
                    terminal,
                });
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
        bail!("Destination unavailable")
    })
    .await
    .context("Destination unavailable")?
}
fn x_target() -> Result<(u32, u32, bool)> {
    let (c, screen) = x11rb::connect(None)?;
    let root = c.setup().roots[screen].root;
    let atom = c.intern_atom(false, b"_NET_ACTIVE_WINDOW")?.reply()?.atom;
    let window = c
        .get_property(
            false,
            root,
            atom,
            x11rb::protocol::xproto::AtomEnum::WINDOW,
            0,
            1,
        )?
        .reply()?
        .value32()
        .and_then(|mut i| i.next())
        .context("Destination unavailable")?;
    let focus = c.get_input_focus()?.reply()?.focus;
    let class = c
        .get_property(
            false,
            window,
            x11rb::protocol::xproto::AtomEnum::WM_CLASS,
            x11rb::protocol::xproto::AtomEnum::STRING,
            0,
            1024,
        )?
        .reply()?
        .value;
    let class = String::from_utf8_lossy(&class).to_lowercase();
    let terminal = is_terminal(&class);
    Ok((window, focus, terminal))
}
fn is_terminal(class: &str) -> bool {
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
    ]
    .iter()
    .any(|s| class.to_lowercase().contains(s))
}
pub async fn capture() -> Result<Target> {
    if is_wayland() {
        tokio::task::spawn_blocking(crate::linux_input::prepare_paste).await??;
        let kwin = if kwin::available() {
            Some(kwin::focused().await?)
        } else {
            None
        };
        let accessible = if kwin.is_some() {
            tokio::time::timeout(Duration::from_millis(250), focused())
                .await
                .ok()
                .and_then(Result::ok)
        } else {
            Some(focused().await?)
        };
        let terminal = kwin.as_ref().is_some_and(|w| is_terminal(&w.class))
            || accessible.as_ref().is_some_and(|a| a.terminal);
        Ok(Target {
            x11: None,
            accessible,
            terminal,
            kwin,
        })
    } else {
        let (w, f, t) = tokio::task::spawn_blocking(x_target).await??;
        let accessible = focused().await.ok();
        let terminal = t || accessible.as_ref().is_some_and(|a| a.terminal);
        Ok(Target {
            x11: Some((w, f)),
            accessible,
            terminal,
            kwin: None,
        })
    }
}
async fn valid(target: &Target) -> bool {
    if let Some(expected) = &target.kwin {
        if kwin::focused().await.ok().as_ref() != Some(expected) {
            return false;
        }
    }
    if let Some(expected) = &target.accessible {
        if focused().await.ok().as_ref() != Some(expected) {
            return false;
        }
    }
    if let Some((w, f)) = target.x11 {
        return tokio::task::spawn_blocking(x_target)
            .await
            .ok()
            .and_then(Result::ok)
            .is_some_and(|(a, b, _)| a == w && b == f);
    }
    target.accessible.is_some() || target.kwin.is_some()
}
static X_CLIPBOARD: OnceLock<Mutex<Option<ClipboardContext>>> = OnceLock::new();
fn x_clip<T>(f: impl FnOnce(&ClipboardContext) -> Result<T>) -> Result<T> {
    let mut guard = X_CLIPBOARD
        .get_or_init(|| Mutex::new(None))
        .lock()
        .map_err(|_| anyhow::anyhow!("Clipboard unavailable"))?;
    if guard.is_none() {
        *guard = Some(ClipboardContext::new().map_err(|e| anyhow::anyhow!(e.to_string()))?);
    }
    f(guard.as_ref().unwrap())
}
fn clipboard_error(e: Box<dyn std::error::Error + Send + Sync>) -> anyhow::Error {
    anyhow::anyhow!(e.to_string())
}
fn x_paste(target: Target, text: String) -> Result<()> {
    x_clip(|clipboard| {
        let (c, screen) = x11rb::connect(None)?;
        let atom = c.intern_atom(false, b"CLIPBOARD")?.reply()?.atom;
        let initial = c.get_selection_owner(atom)?.reply()?.owner;
        let mut old = Vec::new();
        let mut size = 0;
        if initial != 0 {
            for format in clipboard.available_formats().map_err(clipboard_error)? {
                if ["TARGETS", "MULTIPLE", "TIMESTAMP", "SAVE_TARGETS"].contains(&format.as_str()) {
                    continue;
                }
                let data = clipboard.get_buffer(&format).map_err(clipboard_error)?;
                size += data.len();
                anyhow::ensure!(size <= 64 * 1024 * 1024, "Clipboard too large");
                old.push(ClipboardContent::Other(format, data));
            }
        }
        anyhow::ensure!(
            c.get_selection_owner(atom)?.reply()?.owner == initial,
            "Clipboard changed"
        );
        let (w, f, _) = x_target()?;
        anyhow::ensure!(target.x11 == Some((w, f)), "Destination changed");
        let setup = c.setup();
        let keys = c
            .get_keyboard_mapping(setup.min_keycode, setup.max_keycode - setup.min_keycode + 1)?
            .reply()?;
        let key = |sym: u32| -> Result<u8> {
            keys.keysyms
                .chunks(keys.keysyms_per_keycode as usize)
                .position(|s| s.contains(&sym))
                .map(|i| setup.min_keycode + i as u8)
                .context("Paste shortcut unavailable")
        };
        let control = key(0xffe3)?;
        let shift = key(0xffe1)?;
        let v = key(0x76)?;
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        loop {
            let state = c.query_keymap()?.reply()?.keys;
            let held = [
                0xffe1, 0xffe2, 0xffe3, 0xffe4, 0xffe9, 0xffea, 0xffeb, 0xffec,
            ]
            .iter()
            .filter_map(|s| key(*s).ok())
            .any(|k| state[(k / 8) as usize] & (1 << (k % 8)) != 0);
            if !held {
                break;
            }
            anyhow::ensure!(std::time::Instant::now() < deadline, "Shortcut still held");
            std::thread::sleep(Duration::from_millis(20));
        }
        clipboard.set_text(text).map_err(clipboard_error)?;
        let owner = c.get_selection_owner(atom)?.reply()?.owner;
        let dispatch = (|| -> Result<()> {
            let (w, f, _) = x_target()?;
            anyhow::ensure!(target.x11 == Some((w, f)), "Destination changed");
            let root = c.setup().roots[screen].root;
            let mut pressed = Vec::new();
            let result = (|| -> Result<()> {
                for code in [Some(control), target.terminal.then_some(shift), Some(v)]
                    .into_iter()
                    .flatten()
                {
                    c.xtest_fake_input(2, code, 0, root, 0, 0, 0)?.check()?;
                    pressed.push(code);
                }
                Ok(())
            })();
            for code in pressed.into_iter().rev() {
                let _ = c.xtest_fake_input(3, code, 0, root, 0, 0, 0)?.check();
            }
            c.flush()?;
            result?;
            std::thread::sleep(Duration::from_millis(1200));
            Ok(())
        })();
        if c.get_selection_owner(atom)?.reply()?.owner == owner {
            if old.is_empty() {
                clipboard.clear().map_err(clipboard_error)?;
            } else {
                clipboard.set(old).map_err(clipboard_error)?;
            }
        }
        dispatch
    })
}

pub async fn paste(target: Target, text: String) -> Result<()> {
    anyhow::ensure!(valid(&target).await, "Destination changed");
    let text = super::prepare_text(&text, target.terminal);
    if is_wayland() {
        wayland::paste(target, text).await
    } else {
        tokio::task::spawn_blocking(move || x_paste(target, text)).await?
    }
}
pub async fn copy(text: String) -> Result<()> {
    if is_wayland() {
        wayland::copy(text).await
    } else {
        tokio::task::spawn_blocking(move || x_clip(|c| c.set_text(text).map_err(clipboard_error)))
            .await?
    }
}
