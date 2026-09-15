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

struct Fixture {
    child: Child,
    bus: String,
}

struct RestoreClipboard(Option<Vec<copy::MimeSource>>);
impl Drop for RestoreClipboard {
    fn drop(&mut self) {
        if let Some(original) = self.0.take() {
            if original.is_empty() {
                let _ = copy::clear(copy::ClipboardType::Regular, copy::Seat::All);
            } else {
                let _ = copy::Options::new().copy_multi(original);
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
        let mut script = tempfile::NamedTempFile::new().unwrap();
        write!(script, "for (const w of workspace.windowList()) {{ if (w.pid === {}) workspace.activeWindow = w; }}", self.child.id()).unwrap();
        let scripting = zbus::Proxy::new(
            connection,
            "org.kde.KWin",
            "/Scripting",
            "org.kde.kwin.Scripting",
        )
        .await
        .unwrap();
        let name = format!("transcribe-test-{}", self.child.id());
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
    let (mut pipe, _) = paste::get_contents(
        paste::ClipboardType::Regular,
        paste::Seat::Unspecified,
        paste::MimeType::Specific(mime),
    )
    .unwrap();
    let mut bytes = Vec::new();
    pipe.read_to_end(&mut bytes).unwrap();
    bytes
}

#[tokio::test]
#[ignore = "requires a KDE Wayland desktop, Python GTK/DBus, and input/uinput access; creates temporary test windows"]
async fn native_shortcuts_and_paste() {
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

    let formats = paste::get_mime_types(paste::ClipboardType::Regular, paste::Seat::Unspecified)
        .unwrap_or_default();
    let original = formats
        .into_iter()
        .map(|mime| copy::MimeSource {
            source: copy::Source::Bytes(read_clipboard(&mime).into_boxed_slice()),
            mime_type: copy::MimeType::Specific(mime),
        })
        .collect::<Vec<_>>();
    let _restore = RestoreClipboard(Some(original));
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
