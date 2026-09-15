use anyhow::{bail, Context, Result};
use ashpd::desktop::{
    clipboard::{Clipboard, SetSelectionOptions},
    remote_desktop::{DeviceType, KeyState, RemoteDesktop, SelectDevicesOptions},
    Session,
};
use clipboard_rs::{Clipboard as ClipboardApi, ClipboardContent, ClipboardContext};
use futures_util::StreamExt;
use std::{
    collections::{HashMap, VecDeque},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex, OnceLock,
    },
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    sync::Mutex as AsyncMutex,
};
use x11rb::{
    connection::Connection,
    protocol::{xproto::ConnectionExt, xtest::ConnectionExt as _},
};

mod identity;

pub fn is_wayland() -> bool {
    std::env::var_os("WAYLAND_DISPLAY").is_some()
}
fn same_session(a: &impl serde::Serialize, b: &impl serde::Serialize) -> bool {
    serde_json::to_value(a)
        .ok()
        .zip(serde_json::to_value(b).ok())
        .is_some_and(|(a, b)| a == b)
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
    let terminal = [
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
    .any(|s| class.contains(s));
    Ok((window, focus, terminal))
}
pub async fn capture() -> Result<Target> {
    if is_wayland() {
        portal().await?;
        let accessible = focused().await?;
        Ok(Target {
            x11: None,
            terminal: accessible.terminal,
            accessible: Some(accessible),
        })
    } else {
        let (w, f, t) = tokio::task::spawn_blocking(x_target).await??;
        let accessible = focused().await.ok();
        let terminal = t || accessible.as_ref().is_some_and(|a| a.terminal);
        Ok(Target {
            x11: Some((w, f)),
            accessible,
            terminal,
        })
    }
}
async fn valid(target: &Target) -> bool {
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
    target.accessible.is_some()
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

struct ClipboardState {
    formats: Vec<String>,
    known: bool,
    ours: bool,
    generation: u64,
}
struct Portal {
    remote: RemoteDesktop,
    clipboard: Clipboard,
    session: Session<RemoteDesktop>,
    state: AsyncMutex<ClipboardState>,
    data: AsyncMutex<HashMap<String, Vec<u8>>>,
    transfers: AtomicU64,
}
static PORTAL: OnceLock<AsyncMutex<Option<Arc<Portal>>>> = OnceLock::new();
async fn portal() -> Result<Arc<Portal>> {
    identity::register().await?;
    let mut global = PORTAL.get_or_init(|| AsyncMutex::new(None)).lock().await;
    if let Some(p) = global.as_ref() {
        return Ok(p.clone());
    }
    let remote = RemoteDesktop::new().await?;
    let clipboard = Clipboard::new().await?;
    let session = remote.create_session(Default::default()).await?;
    remote
        .select_devices(
            &session,
            SelectDevicesOptions::default().set_devices(Some(DeviceType::Keyboard.into())),
        )
        .await?
        .response()?;
    clipboard.request(&session, Default::default()).await?;
    let p = Arc::new(Portal {
        remote,
        clipboard,
        session,
        state: AsyncMutex::new(ClipboardState {
            formats: vec![],
            known: false,
            ours: false,
            generation: 0,
        }),
        data: AsyncMutex::new(HashMap::new()),
        transfers: AtomicU64::new(0),
    });
    let events = p.clone();
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        let streams = async {
            Ok::<_, ashpd::Error>((
                events
                    .clipboard
                    .receive_selection_owner_changed::<RemoteDesktop>()
                    .await?,
                events
                    .clipboard
                    .receive_selection_transfer::<RemoteDesktop>()
                    .await?,
            ))
        }
        .await;
        let Ok((owners, transfers)) = streams else {
            let _ = ready_tx.send(false);
            return;
        };
        futures_util::pin_mut!(owners, transfers);
        let _ = ready_tx.send(true);
        loop {
            tokio::select! {
                Some((session,change))=owners.next()=>{if same_session(&session,&events.session){let mut s=events.state.lock().await;s.formats=change.mime_types().to_vec();s.known=true;s.ours=change.session_is_owner()==Some(true);s.generation+=1;}},
                Some((session,mime,serial))=transfers.next()=>{if same_session(&session,&events.session){
                    let data=events.data.lock().await.get(&mime).cloned();
                    let written=async {let data=data.context("Clipboard format unavailable")?;let fd=events.clipboard.selection_write(&events.session,serial).await?;
                        let owned:std::os::fd::OwnedFd=fd.into();let mut file=tokio::fs::File::from_std(std::fs::File::from(owned));file.write_all(&data).await?;file.flush().await?;Ok::<_,anyhow::Error>(())}.await;
                    let _=events.clipboard.selection_write_done(&events.session,serial,written.is_ok()).await;
                    if written.is_ok(){events.transfers.fetch_add(1,Ordering::SeqCst);}
                }}, else=>break,
            }
        }
    });
    anyhow::ensure!(
        ready_rx.await.unwrap_or(false),
        "Clipboard portal unavailable"
    );
    let response = p
        .remote
        .start(&p.session, None, Default::default())
        .await?
        .response()?;
    anyhow::ensure!(
        response.devices().contains(DeviceType::Keyboard),
        "Input permission required"
    );
    anyhow::ensure!(
        response.is_clipboard_enabled(),
        "Clipboard permission required"
    );
    *global = Some(p.clone());
    Ok(p)
}
async fn portal_write(p: &Portal, data: HashMap<String, Vec<u8>>) -> Result<()> {
    let formats: Vec<String> = data.keys().cloned().collect();
    *p.data.lock().await = data;
    let names: Vec<&str> = formats.iter().map(String::as_str).collect();
    p.clipboard
        .set_selection(
            &p.session,
            SetSelectionOptions::default().set_mime_types(&names),
        )
        .await?;
    Ok(())
}
static SHORTCUT_HELD: AtomicBool = AtomicBool::new(false);
async fn wayland_paste(target: Target, text: String) -> Result<()> {
    let p = portal().await?;
    anyhow::ensure!(valid(&target).await, "Destination changed");
    let (formats, generation) = {
        let s = p.state.lock().await;
        anyhow::ensure!(s.known, "Clipboard unavailable");
        (s.formats.clone(), s.generation)
    };
    let mut saved = HashMap::new();
    let mut total = 0;
    for format in formats {
        let fd = p.clipboard.selection_read(&p.session, &format).await?;
        let owned: std::os::fd::OwnedFd = fd.into();
        let file = tokio::fs::File::from_std(std::fs::File::from(owned));
        let mut bytes = Vec::new();
        tokio::time::timeout(
            Duration::from_secs(2),
            file.take(64 * 1024 * 1024 + 1).read_to_end(&mut bytes),
        )
        .await??;
        total += bytes.len();
        anyhow::ensure!(total <= 64 * 1024 * 1024, "Clipboard too large");
        saved.insert(format, bytes);
    }
    anyhow::ensure!(
        p.state.lock().await.generation == generation,
        "Clipboard changed"
    );
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    while SHORTCUT_HELD.load(Ordering::SeqCst) {
        anyhow::ensure!(
            tokio::time::Instant::now() < deadline,
            "Shortcut still held"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let temp = HashMap::from([
        ("text/plain;charset=utf-8".into(), text.as_bytes().to_vec()),
        ("text/plain".into(), text.into_bytes()),
    ]);
    portal_write(&p, temp).await?;
    let before = p.transfers.load(Ordering::SeqCst);
    let dispatch = async {
        anyhow::ensure!(valid(&target).await, "Destination changed");
        let keys = if target.terminal {
            vec![0xffe3, 0xffe1, 0x76]
        } else {
            vec![0xffe3, 0x76]
        };
        let mut pressed = Vec::new();
        let mut error = None;
        for key in keys {
            match p
                .remote
                .notify_keyboard_keysym(&p.session, key, KeyState::Pressed, Default::default())
                .await
            {
                Ok(()) => pressed.push(key),
                Err(e) => {
                    error = Some(e);
                    break;
                }
            }
        }
        for key in pressed.into_iter().rev() {
            p.remote
                .notify_keyboard_keysym(&p.session, key, KeyState::Released, Default::default())
                .await?;
        }
        if let Some(e) = error {
            return Err(e.into());
        }
        tokio::time::sleep(Duration::from_millis(1200)).await;
        // A transfer acknowledges clipboard consumption, not the final editor state.
        anyhow::ensure!(
            p.transfers.load(Ordering::SeqCst) > before,
            "Paste could not be confirmed"
        );
        Ok::<_, anyhow::Error>(())
    }
    .await;
    if p.state.lock().await.ours {
        portal_write(&p, saved).await?;
    }
    dispatch
}
pub async fn paste(target: Target, text: String) -> Result<()> {
    anyhow::ensure!(valid(&target).await, "Destination changed");
    let text = super::prepare_text(&text, target.terminal);
    if is_wayland() {
        wayland_paste(target, text).await
    } else {
        tokio::task::spawn_blocking(move || x_paste(target, text)).await?
    }
}
pub async fn copy(text: String) -> Result<()> {
    if is_wayland() {
        let p = portal().await?;
        portal_write(
            &p,
            HashMap::from([("text/plain;charset=utf-8".into(), text.into_bytes())]),
        )
        .await
    } else {
        tokio::task::spawn_blocking(move || x_clip(|c| c.set_text(text).map_err(clipboard_error)))
            .await?
    }
}
type ShortcutTask = (
    tokio_util::sync::CancellationToken,
    tokio::task::JoinHandle<()>,
);
static SHORTCUT: OnceLock<AsyncMutex<Option<ShortcutTask>>> = OnceLock::new();
pub async fn bind_shortcut(shortcut: &str, handler: Arc<dyn Fn() + Send + Sync>) -> Result<String> {
    use ashpd::desktop::global_shortcuts::{GlobalShortcuts, NewShortcut};
    identity::register().await?;
    let mut lock = SHORTCUT.get_or_init(|| AsyncMutex::new(None)).lock().await;
    let proxy = GlobalShortcuts::new().await?;
    let session = proxy.create_session(Default::default()).await?;
    let activated = proxy.receive_activated().await?;
    let deactivated = proxy.receive_deactivated().await?;
    let response = async {
        proxy
            .bind_shortcuts(
                &session,
                &[NewShortcut::new("record", "Transcribe").preferred_trigger(Some(shortcut))],
                None,
                Default::default(),
            )
            .await?
            .response()
            .map_err(anyhow::Error::from)
    }
    .await;
    let response = match response {
        Ok(response) => response,
        Err(error) => {
            let _ = session.close().await;
            return Err(error);
        }
    };
    let label = response
        .shortcuts()
        .iter()
        .find(|s| s.id() == "record")
        .map(|s| s.trigger_description().to_string())
        .filter(|s| !s.is_empty());
    let Some(label) = label else {
        session.close().await?;
        bail!("Desktop did not accept the shortcut");
    };
    if let Some((cancel, old)) = lock.take() {
        cancel.cancel();
        let _ = old.await;
    }
    let cancel = tokio_util::sync::CancellationToken::new();
    let stopping = cancel.clone();
    *lock = Some((
        cancel,
        tokio::spawn(async move {
            futures_util::pin_mut!(activated, deactivated);
            loop {
                tokio::select! {
                    _=stopping.cancelled()=>break,
                    Some(e)=activated.next()=>{if e.shortcut_id()=="record" && same_session(&e.session_handle(),&session){SHORTCUT_HELD.store(true,Ordering::SeqCst);}},
                    Some(e)=deactivated.next()=>{if e.shortcut_id()=="record" && same_session(&e.session_handle(),&session) && SHORTCUT_HELD.swap(false,Ordering::SeqCst){handler();}},else=>break,
                }
            }
            SHORTCUT_HELD.store(false, Ordering::SeqCst);
            let _ = session.close().await;
        }),
    ));
    Ok(label)
}
