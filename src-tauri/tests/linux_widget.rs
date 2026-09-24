#![cfg(target_os = "linux")]

#[path = "../src/widget.rs"]
mod widget;
struct SessionView;

#[test]
#[ignore = "requires a Wayland desktop with layer-shell; briefly shows the real recording window"]
fn widget_maps_as_a_bottom_layer_without_keyboard_focus() {
    use gtk::prelude::*;
    use gtk_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};
    std::env::set_var("WEBKIT_DISABLE_DMABUF_RENDERER", "1");
    let mut context = tauri::test::mock_context(tauri::test::noop_assets());
    *context.config_mut() = serde_json::from_str(include_str!("../tauri.conf.json")).unwrap();
    let app = tauri::Builder::default()
        .any_thread()
        .setup(|app| {
            let config = app
                .config()
                .app
                .windows
                .iter()
                .find(|w| w.label == "widget")
                .unwrap();
            let window = tauri::WebviewWindowBuilder::from_config(app, config)?.build()?;
            widget::init_linux(&window)?;
            let native = window.gtk_window()?;
            assert!(native.is_layer_window());
            assert!(native.is_anchor(Edge::Bottom));
            assert!(!native.is_anchor(Edge::Top));
            assert!(!native.is_anchor(Edge::Left));
            assert!(!native.is_anchor(Edge::Right));
            assert_eq!(native.layer(), Layer::Overlay);
            assert_eq!(native.keyboard_mode(), KeyboardMode::None);
            assert_eq!(native.layer_shell_margin(Edge::Bottom), 24);
            assert!(!native.accepts_focus());
            widget::show(&window, &SessionView).unwrap();
            let handle = app.handle().clone();
            gtk::glib::timeout_add_local_once(std::time::Duration::from_millis(500), move || {
                assert!(native.is_mapped());
                assert_eq!(
                    (native.allocated_width(), native.allocated_height()),
                    (188, 48)
                );
                widget::hide(&window).unwrap();
                widget::show(&window, &SessionView).unwrap();
                gtk::glib::timeout_add_local_once(
                    std::time::Duration::from_millis(500),
                    move || {
                        assert!(native.is_mapped());
                        assert!(!native.is_active());
                        handle.exit(0);
                    },
                );
            });
            Ok(())
        })
        .build(context)
        .unwrap();
    app.run_return(|_, _| {});
}
