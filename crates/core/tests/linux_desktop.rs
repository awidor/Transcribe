#![cfg(target_os = "linux")]

use evdev::{uinput::VirtualDevice, AttributeSet, KeyCode, KeyEvent};
use std::{
    io::{BufRead, BufReader, Read, Write},
    process::{Child, Command, Stdio},
    sync::mpsc,
    time::Duration,
};
use transcribe_core::{hotkey, insertion};
use wl_clipboard_rs::{copy, paste};

static DESKTOP_TEST: std::sync::Mutex<()> = std::sync::Mutex::new(());

struct Fixture {
    child: Child,
    bus: String,
}

struct RestoreClipboard(Vec<(copy::ClipboardType, Vec<copy::MimeSource>)>);
impl RestoreClipboard {
    fn new() -> Self {
        Self(
            [
                (paste::ClipboardType::Regular, copy::ClipboardType::Regular),
                (paste::ClipboardType::Primary, copy::ClipboardType::Primary),
            ]
            .into_iter()
            .map(|(read, write)| {
                let original = paste::get_mime_types(read, paste::Seat::Unspecified)
                    .unwrap_or_default()
                    .into_iter()
                    .map(|mime| copy::MimeSource {
                        source: copy::Source::Bytes(read_selection(read, &mime).into_boxed_slice()),
                        mime_type: copy::MimeType::Specific(mime),
                    })
                    .collect();
                (write, original)
            })
            .collect(),
        )
    }
}
impl Drop for RestoreClipboard {
    fn drop(&mut self) {
        for (clipboard, original) in self.0.drain(..) {
            if original.is_empty() {
                let _ = copy::clear(clipboard, copy::Seat::All);
            } else {
                let mut options = copy::Options::new();
                options.clipboard(clipboard);
                let _ = options.copy_multi(original);
            }
        }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
impl Fixture {
    fn new(mode: &str) -> Self {
        let mut child = Command::new("python")
            .arg(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../scripts/linux-input-fixture.py"
            ))
            .arg(mode)
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut bus = String::new();
        BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut bus)
            .unwrap();
        assert!(bus.starts_with(':'));
        Self {
            child,
            bus: bus.trim().into(),
        }
    }
    async fn proxy<'a>(&'a self, connection: &'a zbus::Connection) -> zbus::Proxy<'a> {
        zbus::Proxy::new(
            connection,
            self.bus.as_str(),
            "/app/transcribe/TestFixture",
            "app.transcribe.TestFixture",
        )
        .await
        .unwrap()
    }
    async fn focus(&self, connection: &zbus::Connection) {
        focus_window(connection, self.child.id()).await;
    }
    async fn state(&self, connection: &zbus::Connection) -> serde_json::Value {
        let json: String = self
            .proxy(connection)
            .await
            .call("state", &())
            .await
            .unwrap();
        serde_json::from_str(&json).unwrap()
    }
    async fn reset(&self, connection: &zbus::Connection) {
        self.proxy(connection)
            .await
            .call::<_, _, ()>("reset", &())
            .await
            .unwrap();
    }
}

async fn focus_window(connection: &zbus::Connection, pid: u32) {
    let mut script = tempfile::NamedTempFile::new().unwrap();
    write!(script, "for (const w of workspace.windowList()) {{ if (w.pid === {pid}) workspace.activeWindow = w; }}").unwrap();
    let scripting = zbus::Proxy::new(
        connection,
        "org.kde.KWin",
        "/Scripting",
        "org.kde.kwin.Scripting",
    )
    .await
    .unwrap();
    let name = format!("transcribe-test-{pid}");
    let id: i32 = scripting
        .call("loadScript", &(script.path().to_str().unwrap(), &name))
        .await
        .unwrap();
    let path = format!("/Scripting/Script{id}");
    let runner = zbus::Proxy::new(
        connection,
        "org.kde.KWin",
        path.as_str(),
        "org.kde.kwin.Script",
    )
    .await
    .unwrap();
    runner.call::<_, _, ()>("run", &()).await.unwrap();
    let _: bool = scripting.call("unloadScript", &(&name,)).await.unwrap();
    tokio::time::sleep(Duration::from_millis(150)).await;
}

fn keyboard() -> VirtualDevice {
    let keys = [
        KeyCode::KEY_A,
        KeyCode::KEY_B,
        KeyCode::KEY_LEFTALT,
        KeyCode::KEY_RIGHTALT,
        KeyCode::KEY_F24,
        KeyCode::KEY_LEFTCTRL,
        KeyCode::KEY_V,
    ]
    .into_iter()
    .collect::<AttributeSet<_>>();
    VirtualDevice::builder()
        .unwrap()
        .name("Transcribe input test fixture")
        .with_keys(&keys)
        .unwrap()
        .build()
        .unwrap()
}
fn stroke(device: &mut VirtualDevice, keys: &[KeyCode]) {
    device
        .emit(
            &keys
                .iter()
                .map(|key| *KeyEvent::new(*key, 1))
                .collect::<Vec<_>>(),
        )
        .unwrap();
    std::thread::sleep(Duration::from_millis(30));
    device
        .emit(
            &keys
                .iter()
                .rev()
                .map(|key| *KeyEvent::new(*key, 0))
                .collect::<Vec<_>>(),
        )
        .unwrap();
}
fn read_clipboard(mime: &str) -> Vec<u8> {
    read_selection(paste::ClipboardType::Regular, mime)
}
fn read_selection(clipboard: paste::ClipboardType, mime: &str) -> Vec<u8> {
    let (mut pipe, _) = paste::get_contents(
        clipboard,
        paste::Seat::Unspecified,
        paste::MimeType::Specific(mime),
    )
    .unwrap();
    let mut bytes = Vec::new();
    pipe.read_to_end(&mut bytes).unwrap();
    bytes
}

#[tokio::test]
#[ignore = "requires a KDE Wayland desktop, Alacritty, xterm, Python, and input/uinput access; opens safe terminal receivers"]
async fn real_terminal_paste() {
    let _desktop = DESKTOP_TEST.lock().unwrap();
    let _restore = RestoreClipboard::new();
    let connection = zbus::Connection::session().await.unwrap();
    for (program, bracketed, kitty) in [
        ("alacritty", false, false),
        ("alacritty", true, false),
        ("alacritty", true, true),
        ("xterm", false, false),
        ("xterm", true, false),
    ] {
        let directory = tempfile::tempdir().unwrap();
        let log_path = directory.path().join("events");
        let log = std::fs::File::create(&log_path).unwrap();
        let mut command = Command::new(program);
        if program == "alacritty" {
            command.args(["--print-events", "--title", "Transcribe terminal test"]);
        } else {
            command.args(["-title", "Transcribe terminal test"]);
        }
        command
            .args(["-e", "python"])
            .arg(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../scripts/linux-terminal-fixture.py"
            ))
            .arg(directory.path())
            .stdout(log.try_clone().unwrap())
            .stderr(log);
        if bracketed {
            command.arg("--bracketed");
        }
        if kitty {
            command.arg("--kitty");
        }
        let terminal = Fixture {
            child: command.spawn().unwrap(),
            bus: String::new(),
        };
        for _ in 0..100 {
            if directory.path().join("ready").exists() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert!(
            directory.path().join("ready").exists(),
            "Terminal did not start: {}",
            std::fs::read_to_string(&log_path).unwrap()
        );
        terminal.focus(&connection).await;
        for (clipboard, text) in [
            (copy::ClipboardType::Regular, "original clipboard"),
            (copy::ClipboardType::Primary, "original selection"),
        ] {
            let mut options = copy::Options::new();
            options.clipboard(clipboard);
            options
                .copy(
                    copy::Source::Bytes(text.as_bytes().into()),
                    copy::MimeType::Text,
                )
                .unwrap();
        }
        insertion::insert(&uuid::Uuid::new_v4().to_string(), "Grüße\n世界 👋\u{1b}")
            .await
            .unwrap();
        let expected = if bracketed {
            "\u{1b}[200~Grüße 世界 👋\u{1b}[201~"
        } else {
            "Grüße 世界 👋"
        };
        let received = std::fs::read(directory.path().join("received")).unwrap();
        // Enhanced keyboard reporting also forwards modifier events.
        let received = String::from_utf8_lossy(&received);
        assert_eq!(received.matches(expected).count(), 1,
            "{program} bracketed={bracketed} kitty={kitty}: expected {expected:?}, received {received:?}. Terminal events: {}",
            std::fs::read_to_string(&log_path).unwrap()
        );
        if !kitty {
            assert_eq!(received, expected, "No extra input may reach the terminal");
        }
        assert!(
            !received.contains(['\r', '\n']),
            "Paste must not submit a command"
        );
        assert_eq!(
            read_clipboard("text/plain;charset=utf-8"),
            b"original clipboard"
        );
        assert_eq!(
            read_selection(paste::ClipboardType::Primary, "text/plain;charset=utf-8"),
            b"original selection"
        );

        // A copy and a mouse selection made during delivery independently win
        // over our restoration, without causing a retry or duplicate paste.
        if program == "xterm" && bracketed {
            for (read, write, untouched) in [
                (
                    paste::ClipboardType::Regular,
                    copy::ClipboardType::Regular,
                    paste::ClipboardType::Primary,
                ),
                (
                    paste::ClipboardType::Primary,
                    copy::ClipboardType::Primary,
                    paste::ClipboardType::Regular,
                ),
            ] {
                let before = read_selection(untouched, "text/plain;charset=utf-8");
                let replacement = std::thread::spawn(move || {
                    for _ in 0..200 {
                        if paste::get_mime_types(read, paste::Seat::Unspecified)
                            .unwrap_or_default()
                            .iter()
                            .any(|mime| mime.starts_with("application/x-transcribe-"))
                        {
                            std::thread::sleep(Duration::from_millis(300));
                            let mut options = copy::Options::new();
                            options.clipboard(write);
                            options
                                .copy(
                                    copy::Source::Bytes(
                                        b"new selection".to_vec().into_boxed_slice(),
                                    ),
                                    copy::MimeType::Text,
                                )
                                .unwrap();
                            return;
                        }
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    panic!("Paste never owned the selection");
                });
                insertion::insert(&uuid::Uuid::new_v4().to_string(), "race test")
                    .await
                    .unwrap();
                replacement.join().unwrap();
                assert_eq!(
                    read_selection(read, "text/plain;charset=utf-8"),
                    b"new selection"
                );
                assert_eq!(
                    read_selection(untouched, "text/plain;charset=utf-8"),
                    before
                );
            }
            let received = std::fs::read(directory.path().join("received")).unwrap();
            assert_eq!(
                String::from_utf8_lossy(&received),
                format!("{expected}\u{1b}[200~race test\u{1b}[201~\u{1b}[200~race test\u{1b}[201~")
            );
        }
    }
}

#[tokio::test]
#[ignore = "requires a KDE Wayland desktop, Python GTK/DBus, and input/uinput access; creates temporary test windows"]
async fn native_shortcuts_and_paste() {
    let _desktop = DESKTOP_TEST.lock().unwrap();
    assert!(insertion::linux::is_wayland());
    let connection = zbus::Connection::session().await.unwrap();
    let editor = Fixture::new("editor");
    editor.focus(&connection).await;
    let (sender, events) = mpsc::channel();
    let service = hotkey::Service::new(move |event| {
        let _ = sender.send(event);
    });
    service.configure(
        service
            .validate(r#"{"platform":"wayland","keys":[{"code":0,"label":"AltRight"}]}"#)
            .unwrap(),
    );
    service.start().unwrap();
    let mut device = keyboard();
    tokio::time::sleep(Duration::from_millis(800)).await;
    stroke(&mut device, &[KeyCode::KEY_LEFTALT]);
    assert!(events.recv_timeout(Duration::from_millis(100)).is_err());
    stroke(&mut device, &[KeyCode::KEY_RIGHTALT]);
    assert!(matches!(
        events.recv_timeout(Duration::from_secs(1)).unwrap(),
        hotkey::Event::Activate
    ));
    stroke(&mut device, &[KeyCode::KEY_RIGHTALT, KeyCode::KEY_A]);
    assert!(events.recv_timeout(Duration::from_millis(100)).is_err());

    service.configure(vec![vec![30], vec![48]]);
    stroke(&mut device, &[KeyCode::KEY_A, KeyCode::KEY_B]);
    assert!(matches!(
        events.recv_timeout(Duration::from_secs(1)).unwrap(),
        hotkey::Event::Activate
    ));
    let token = service.begin_capture().unwrap();
    stroke(&mut device, &[KeyCode::KEY_F24]);
    let captured = loop {
        if let hotkey::Event::Capture {
            token: found,
            shortcut: Some(shortcut),
            ..
        } = events.recv_timeout(Duration::from_secs(1)).unwrap()
        {
            assert_eq!(found, token);
            break shortcut;
        }
    };
    assert_eq!(service.validate(&captured).unwrap(), vec![vec![194]]);
    assert!(captured.contains("linux-evdev"));

    // An unplugged, held shortcut must not activate, and a new device must work.
    service.configure(vec![vec![100]]);
    device
        .emit(&[*KeyEvent::new(KeyCode::KEY_RIGHTALT, 1)])
        .unwrap();
    tokio::time::sleep(Duration::from_millis(40)).await;
    drop(device);
    tokio::time::sleep(Duration::from_millis(700)).await;
    let unplug_event = events.try_recv();
    assert!(unplug_event.is_err(), "Unplug produced {unplug_event:?}");
    let mut device = keyboard();
    tokio::time::sleep(Duration::from_millis(800)).await;
    stroke(&mut device, &[KeyCode::KEY_RIGHTALT]);
    assert!(matches!(
        events.recv_timeout(Duration::from_secs(1)).unwrap(),
        hotkey::Event::Activate
    ));

    let _restore = RestoreClipboard::new();
    let saved = vec![
        copy::MimeSource {
            source: copy::Source::Bytes(b"original clipboard".to_vec().into_boxed_slice()),
            mime_type: copy::MimeType::Text,
        },
        copy::MimeSource {
            source: copy::Source::Bytes(b"<b>original clipboard</b>".to_vec().into_boxed_slice()),
            mime_type: copy::MimeType::Specific("text/html".into()),
        },
    ];
    copy::Options::new().copy_multi(saved).unwrap();
    editor.reset(&connection).await;
    service.configure(vec![vec![29], vec![47]]);
    insertion::insert("linux-native-editor-test", "Grüße\n世界 👋")
        .await
        .unwrap();
    assert_eq!(editor.state(&connection).await["text"], "Grüße\n世界 👋");
    assert_eq!(read_clipboard("text/html"), b"<b>original clipboard</b>");
    assert!(
        events.try_recv().is_err(),
        "Injected paste must not activate a shortcut"
    );

    let target = insertion::linux::capture().await.unwrap();
    let terminal = Fixture::new("terminal");
    terminal.focus(&connection).await;
    assert!(insertion::linux::paste(target, "wrong destination".into())
        .await
        .is_err());
    assert_eq!(terminal.state(&connection).await["text"], "");
    insertion::insert("linux-native-terminal-test", "hello\nworld\u{1b}")
        .await
        .unwrap();
    let state = terminal.state(&connection).await;
    assert_eq!(state["text"], "hello world");
    assert!(!state["keys"]
        .as_array()
        .unwrap()
        .iter()
        .any(|key| key == "Return"));

    editor.focus(&connection).await;
    editor.reset(&connection).await;
    let replacement = std::thread::spawn(|| {
        for _ in 0..200 {
            if paste::get_mime_types(paste::ClipboardType::Regular, paste::Seat::Unspecified)
                .unwrap_or_default()
                .iter()
                .any(|mime| mime.starts_with("application/x-transcribe-"))
            {
                std::thread::sleep(Duration::from_millis(300));
                copy::Options::new()
                    .copy(
                        copy::Source::Bytes(b"new user clipboard".to_vec().into_boxed_slice()),
                        copy::MimeType::Text,
                    )
                    .unwrap();
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("Paste never owned the clipboard");
    });
    insertion::insert("linux-native-copy-race-test", "Clipboard race test")
        .await
        .unwrap();
    replacement.join().unwrap();
    assert_eq!(
        read_clipboard("text/plain;charset=utf-8"),
        b"new user clipboard"
    );
}
