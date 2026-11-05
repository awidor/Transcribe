#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod assets;
mod controls;
#[cfg(target_os = "linux")]
mod helper;
mod history;
mod instance;
mod main_window;
mod pill;
mod settings;
mod shell;
mod theme;
mod tray;
#[cfg(windows)]
mod windows;

use futures::StreamExt;
use gpui::{App, AppContext, QuitMode};
use main_window::Tab;
use shell::{Shell, UiEvent};
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU32, Ordering},
};
use transcribe_core::{
    session::{self, Listener},
    update::{self, Updater},
};

fn main() {
    update::startup();
    #[cfg(target_os = "linux")]
    if std::env::args().any(|arg| arg == helper::FLAG) {
        helper::run();
        return;
    }
    if instance::forward() {
        return;
    }
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("async runtime");
    let handle = runtime.handle().clone();
    let (sender, mut receiver) = futures::channel::mpsc::unbounded::<UiEvent>();
    instance::listen(sender.clone());

    let level = Arc::new(AtomicU32::new(0));
    let focused = Arc::new(AtomicBool::new(false));
    let events = sender.clone();
    let levels = level.clone();
    let listener = Listener {
        event: Box::new(move |event| {
            let _ = events.unbounded_send(UiEvent::Core(event));
        }),
        level: Box::new(move |value| levels.store(value.to_bits(), Ordering::Relaxed)),
        main_focused: focused.clone(),
    };
    let core = session::data_dir()
        .and_then(|data| session::App::new(&data, listener, Updater::new(), handle.clone()));

    gpui_platform::application()
        .with_assets(assets::Assets)
        .with_quit_mode(QuitMode::Explicit)
        .run(move |cx: &mut App| {
            gpui_component::init(cx);
            theme::init(cx.window_appearance(), cx);
            let app = match core {
                Ok(app) => app,
                Err(error) => {
                    eprintln!("{error:#}");
                    cx.quit();
                    return;
                }
            };
            tray::start(&handle, sender.clone(), cx);
            let shell = cx.new(|cx| Shell::new(app, handle.clone(), level, focused, sender, cx));
            shell.update(cx, |shell, cx| shell.show(Tab::History, cx));
            cx.on_app_quit(|_| async { tray::stop() }).detach();
            cx.spawn(async move |cx| {
                while let Some(event) = receiver.next().await {
                    shell.update(cx, |shell, cx| shell.handle(event, cx));
                }
            })
            .detach();
        });
    drop(runtime);
}
