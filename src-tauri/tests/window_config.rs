#[test]
fn windows_wait_for_backend_setup() {
    let config: tauri::utils::config::Config =
        serde_json::from_str(include_str!("../tauri.conf.json")).unwrap();
    assert_eq!(config.app.windows.len(), 2);
    for window in config.app.windows {
        assert!(
            !window.create,
            "{} must not load before managed state",
            window.label
        );
    }
}
