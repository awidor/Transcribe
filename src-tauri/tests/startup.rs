// Exercise the real private setup and IPC handler without exporting a test-only app API.
include!("../src/lib.rs");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_windows_can_bootstrap_immediately_after_creation() {
        // Bootstrap checks key availability. Never consult the user's real
        // credential store or trigger a Keychain prompt from a test binary.
        keyring::set_default_credential_builder(keyring::mock::default_credential_builder());
        let mut context = tauri::test::mock_context(tauri::test::noop_assets());
        *context.config_mut() = serde_json::from_str(include_str!("../tauri.conf.json")).unwrap();
        let app = tauri::test::mock_builder()
            .setup(|app| {
                install_state_and_windows(
                    app,
                    Arc::new(AppState {
                        store: Mutex::new(Store::open(std::path::Path::new(":memory:"))?),
                        session: tokio::sync::Mutex::new(Session::default()),
                        registry: Registry::default(),
                        recordings: PathBuf::new(),
                        hotkeys: hotkey::Service::new(|_| {}),
                        settings_lock: tokio::sync::Mutex::new(()),
                    }),
                )?;
                Ok(())
            })
            .invoke_handler(tauri::generate_handler![bootstrap])
            .build(context)
            .unwrap();
        app.run_return(|app, event| {
            if !matches!(event, tauri::RunEvent::Ready) {
                return;
            }
            for label in ["main", "widget"] {
                let window = app.get_webview_window(label).unwrap();
                let response = tauri::test::get_ipc_response(
                    &window,
                    tauri::webview::InvokeRequest {
                        cmd: "bootstrap".into(),
                        callback: tauri::ipc::CallbackFn(0),
                        error: tauri::ipc::CallbackFn(1),
                        url: if cfg!(windows) {
                            "http://tauri.localhost"
                        } else {
                            "tauri://localhost"
                        }
                        .parse()
                        .unwrap(),
                        body: tauri::ipc::InvokeBody::default(),
                        headers: Default::default(),
                        invoke_key: tauri::test::INVOKE_KEY.into(),
                    },
                )
                .expect("bootstrap must find its managed state");
                let data = response.deserialize::<serde_json::Value>().unwrap();
                assert_eq!(data["session"]["phase"], "idle");
                assert_eq!(data["entries"], serde_json::json!([]));
                assert_eq!(data["hasKey"], false);
                window.close().unwrap();
            }
        });
    }
}
