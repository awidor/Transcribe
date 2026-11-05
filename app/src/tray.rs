//! The tray icon: History and Quit. Closing the window keeps the app here.
use crate::shell::UiEvent;
use futures::channel::mpsc::UnboundedSender;
use gpui::App;
use tokio::runtime::Handle;

const ICON: &[u8] = include_bytes!("../../assets/icons/64.png");

fn rgba() -> Option<(u32, u32, Vec<u8>)> {
    let image = image::load_from_memory(ICON).ok()?.to_rgba8();
    Some((image.width(), image.height(), image.into_raw()))
}

#[cfg(target_os = "linux")]
pub fn start(runtime: &Handle, events: UnboundedSender<UiEvent>, _: &mut App) {
    use ksni::{Icon, MenuItem, TrayMethods, menu::StandardItem};

    struct Tray {
        events: UnboundedSender<UiEvent>,
        icon: Vec<Icon>,
    }

    impl Tray {
        fn send(&self, event: UiEvent) {
            let _ = self.events.unbounded_send(event);
        }
    }

    impl ksni::Tray for Tray {
        fn id(&self) -> String {
            "app.transcribe.desktop".into()
        }

        fn title(&self) -> String {
            "Transcribe".into()
        }

        fn icon_name(&self) -> String {
            "app.transcribe.desktop".into()
        }

        fn icon_pixmap(&self) -> Vec<Icon> {
            self.icon.clone()
        }

        fn activate(&mut self, _: i32, _: i32) {
            self.send(UiEvent::Show);
        }

        fn menu(&self) -> Vec<MenuItem<Self>> {
            vec![
                StandardItem {
                    label: "History".into(),
                    activate: Box::new(|tray: &mut Self| tray.send(UiEvent::Show)),
                    ..Default::default()
                }
                .into(),
                StandardItem {
                    label: "Quit".into(),
                    activate: Box::new(|tray: &mut Self| tray.send(UiEvent::Quit)),
                    ..Default::default()
                }
                .into(),
            ]
        }
    }

    // ARGB in network byte order.
    let icon = rgba()
        .map(|(width, height, pixels)| {
            let data = pixels
                .chunks_exact(4)
                .flat_map(|p| [p[3], p[0], p[1], p[2]])
                .collect();
            vec![Icon {
                width: width as i32,
                height: height as i32,
                data,
            }]
        })
        .unwrap_or_default();
    runtime.spawn(async move {
        match (Tray { events, icon }).spawn().await {
            // The tray lives as long as the app.
            Ok(handle) => std::mem::forget(handle),
            Err(error) => eprintln!("Tray unavailable: {error}"),
        }
    });
}

#[cfg(windows)]
thread_local! {
    static TRAY: std::cell::RefCell<Option<tray_icon::TrayIcon>> = const { std::cell::RefCell::new(None) };
}

#[cfg(windows)]
pub fn start(_: &Handle, events: UnboundedSender<UiEvent>, _: &mut App) {
    use tray_icon::{
        Icon, MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent,
        menu::{Menu, MenuEvent, MenuItem},
    };

    let history = MenuItem::new("History", true, None);
    let quit = MenuItem::new("Quit", true, None);
    let menu = Menu::new();
    if menu
        .append(&history)
        .and_then(|()| menu.append(&quit))
        .is_err()
    {
        return;
    }
    let (history_id, quit_id) = (history.id().clone(), quit.id().clone());
    let menu_events = events.clone();
    MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
        if event.id == history_id {
            let _ = menu_events.unbounded_send(UiEvent::Show);
        } else if event.id == quit_id {
            let _ = menu_events.unbounded_send(UiEvent::Quit);
        }
    }));
    TrayIconEvent::set_event_handler(Some(move |event: TrayIconEvent| {
        if let TrayIconEvent::Click {
            button: MouseButton::Left,
            button_state: MouseButtonState::Up,
            ..
        } = event
        {
            let _ = events.unbounded_send(UiEvent::Show);
        }
    }));
    let mut builder = TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_menu_on_left_click(false)
        .with_tooltip("Transcribe");
    if let Some(icon) = rgba().and_then(|(w, h, pixels)| Icon::from_rgba(pixels, w, h).ok()) {
        builder = builder.with_icon(icon);
    }
    match builder.build() {
        Ok(tray) => TRAY.with(|slot| *slot.borrow_mut() = Some(tray)),
        Err(error) => eprintln!("Tray unavailable: {error}"),
    }
}

/// Removes the icon before the process ends.
pub fn stop() {
    #[cfg(windows)]
    TRAY.with(|slot| slot.borrow_mut().take());
}
