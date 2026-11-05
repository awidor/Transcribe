//! GNOME's Wayland session lets only its own shell place windows, so there the
//! pill runs in a child process on XWayland, which honours position and
//! stacking. The two talk in JSON lines over the child's stdin and stdout.
use crate::{
    assets::Assets,
    pill::{self, Action, FromHelper, Model, Placement, ToHelper},
    shell::UiEvent,
    theme,
};
use futures::{StreamExt, channel::mpsc};
use gpui::{App, QuitMode};
use std::{
    io::{BufRead, BufReader, Write},
    process::{Child, ChildStdin, Command, Stdio},
    rc::Rc,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU32, Ordering},
    },
};

pub const FLAG: &str = "--pill";

pub struct Helper {
    child: Mutex<Child>,
    stdin: Mutex<ChildStdin>,
}

impl Helper {
    pub fn spawn(events: mpsc::UnboundedSender<UiEvent>) -> anyhow::Result<Self> {
        let mut child = Command::new(std::env::current_exe()?)
            .arg(FLAG)
            .env_remove("WAYLAND_DISPLAY")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()?;
        let stdin = child.stdin.take().expect("helper stdin");
        let stdout = child.stdout.take().expect("helper stdout");
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { return };
                match serde_json::from_str::<FromHelper>(&line) {
                    Ok(message) => {
                        if events.unbounded_send(UiEvent::Helper(message)).is_err() {
                            return;
                        }
                    }
                    Err(error) => eprintln!("Recording widget: {error}"),
                }
            }
        });
        Ok(Self {
            child: Mutex::new(child),
            stdin: Mutex::new(stdin),
        })
    }

    pub fn alive(&self) -> bool {
        self.child
            .lock()
            .map(|mut child| matches!(child.try_wait(), Ok(None)))
            .unwrap_or(false)
    }

    pub fn send(&self, message: &ToHelper) {
        let Ok(mut line) = serde_json::to_string(message) else {
            return;
        };
        line.push('\n');
        if let Ok(mut stdin) = self.stdin.lock() {
            let _ = stdin
                .write_all(line.as_bytes())
                .and_then(|()| stdin.flush());
        }
    }
}

impl Drop for Helper {
    fn drop(&mut self) {
        if let Ok(mut child) = self.child.lock() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

fn reply(message: &FromHelper) {
    if let Ok(line) = serde_json::to_string(message) {
        let mut stdout = std::io::stdout().lock();
        let _ = writeln!(stdout, "{line}").and_then(|()| stdout.flush());
    }
}

/// The child: shows what the parent sends and reports clicks back. It ends
/// when the parent closes its stdin.
pub fn run() {
    let (sender, mut receiver) = mpsc::unbounded::<ToHelper>();
    std::thread::spawn(move || {
        for line in std::io::stdin().lock().lines() {
            let Ok(line) = line else { break };
            if let Ok(message) = serde_json::from_str(&line) {
                if sender.unbounded_send(message).is_err() {
                    return;
                }
            }
        }
    });
    gpui_platform::application()
        .with_assets(Assets)
        .with_quit_mode(QuitMode::Explicit)
        .run(move |cx: &mut App| {
            gpui_component::init(cx);
            theme::init(cx.window_appearance(), cx);
            let level = Arc::new(AtomicU32::new(0));
            let act: Rc<dyn Fn(Action, &mut App)> =
                Rc::new(|action, _| reply(&FromHelper::Action(action)));
            cx.spawn(async move |cx| {
                let mut open: Option<pill::Open> = None;
                while let Some(message) = receiver.next().await {
                    cx.update(|cx| match message {
                        ToHelper::Level(value) => {
                            level.store(value.to_bits(), Ordering::Relaxed);
                        }
                        ToHelper::Hide => {
                            if let Some(shown) = open.take() {
                                shown
                                    .handle
                                    .update(cx, |_, window, _| window.remove_window())
                                    .ok();
                            }
                        }
                        ToHelper::Show(model) => {
                            let shown = show(&mut open, model, &level, &act, cx);
                            reply(&FromHelper::Shown(
                                shown.err().map(|error| error.to_string()),
                            ));
                        }
                    });
                }
                cx.update(|cx| cx.quit());
            })
            .detach();
        });
}

fn show(
    open: &mut Option<pill::Open>,
    model: Model,
    level: &Arc<AtomicU32>,
    act: &Rc<dyn Fn(Action, &mut App)>,
    cx: &mut App,
) -> anyhow::Result<()> {
    if let Some(shown) = open.as_mut() {
        if shown.size == model.window_size() {
            let view = shown.view.clone();
            let updated = shown.handle.update(cx, |_, _, cx| {
                view.update(cx, |view, cx| view.set(model.clone(), cx))
            });
            if updated.is_ok() {
                return Ok(());
            }
        }
        if let Some(shown) = open.take() {
            shown
                .handle
                .update(cx, |_, window, _| window.remove_window())
                .ok();
        }
    }
    if model.closing {
        return Ok(());
    }
    *open = Some(pill::open(
        Placement::Popup,
        model,
        level.clone(),
        act.clone(),
        cx,
    )?);
    Ok(())
}
