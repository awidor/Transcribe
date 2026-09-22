use super::*;
use tauri_plugin_updater::{Update, UpdaterExt};

// Releases publish updater packages for these targets only.
const SUPPORTED: bool = cfg!(any(windows, target_os = "macos"));

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct UpdateView {
    phase: &'static str,
    current: String,
    version: Option<String>,
    error: Option<String>,
}
struct Inner {
    view: UpdateView,
    update: Option<Update>,
}
pub(crate) struct UpdateState(Mutex<Inner>);
impl UpdateState {
    pub(crate) fn new(current: String) -> Self {
        Self(Mutex::new(Inner {
            view: UpdateView {
                phase: if SUPPORTED { "idle" } else { "unsupported" },
                current,
                version: None,
                error: None,
            },
            update: None,
        }))
    }
    fn change(&self, app: &AppHandle, f: impl FnOnce(&mut Inner)) -> Result<UpdateView> {
        let mut inner = self.0.lock().map_err(err)?;
        f(&mut inner);
        let _ = app.emit_to("main", "update", &inner.view);
        Ok(inner.view.clone())
    }
}
async fn recording(state: &AppState) -> bool {
    matches!(
        state.session.lock().await.view.phase.as_str(),
        "starting" | "recording" | "transcribing" | "inserting"
    ) || state.live.lock().await.view.phase.active()
}
async fn check(app: &AppHandle, manual: bool) -> Result<UpdateView> {
    if !SUPPORTED {
        return Err("Updates unavailable".into());
    }
    let state = app.state::<UpdateState>();
    let previous = {
        let mut inner = state.0.lock().map_err(err)?;
        if matches!(inner.view.phase, "checking" | "installing") {
            return Ok(inner.view.clone());
        }
        let previous = inner.view.clone();
        inner.view.phase = "checking";
        inner.view.error = None;
        let _ = app.emit_to("main", "update", &inner.view);
        previous
    };
    let result = match app.updater() {
        Ok(updater) => updater.check().await.map_err(err),
        Err(e) => Err(err(e)),
    };
    state.change(app, |inner| match result {
        Ok(Some(update)) => {
            inner.view.phase = "available";
            inner.view.version = Some(update.version.clone());
            inner.update = Some(update);
        }
        Ok(None) => {
            inner.view.phase = "current";
            inner.view.version = None;
            inner.update = None;
        }
        // Background checks stay quiet: offline machines and releases
        // without an updater manifest are not actionable.
        Err(_) if !manual => inner.view = previous,
        Err(message) => {
            inner.view.phase = "error";
            inner.view.error = Some(message);
        }
    })
}
pub(crate) fn watch(app: AppHandle) {
    if !SUPPORTED || cfg!(debug_assertions) {
        return;
    }
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_secs(10)).await;
        loop {
            let _ = check(&app, false).await;
            tokio::time::sleep(std::time::Duration::from_secs(6 * 60 * 60)).await;
        }
    });
}
#[tauri::command]
pub(crate) fn update_state(
    window: tauri::WebviewWindow,
    state: State<'_, UpdateState>,
) -> Result<UpdateView> {
    main_only(&window)?;
    Ok(state.0.lock().map_err(err)?.view.clone())
}
#[tauri::command]
pub(crate) async fn check_update(
    window: tauri::WebviewWindow,
    app: AppHandle,
) -> Result<UpdateView> {
    main_only(&window)?;
    check(&app, true).await
}
#[tauri::command]
pub(crate) async fn install_update(
    window: tauri::WebviewWindow,
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
) -> Result<()> {
    main_only(&window)?;
    const BUSY: &str = "Stop recording before updating";
    if recording(&state).await {
        return Err(BUSY.into());
    }
    let updates = app.state::<UpdateState>();
    let update = {
        let mut inner = updates.0.lock().map_err(err)?;
        let update = match (inner.view.phase, &inner.update) {
            ("available", Some(update)) => update.clone(),
            _ => return Err("Update unavailable".into()),
        };
        inner.view.phase = "installing";
        let _ = app.emit_to("main", "update", &inner.view);
        update
    };
    let result = async {
        let bytes = update.download(|_, _| {}, || {}).await.map_err(err)?;
        // A recording started during the download would be lost on restart.
        if recording(&state).await {
            return Err(BUSY.to_string());
        }
        // On Windows this launches the installer and exits the process.
        tauri::async_runtime::spawn_blocking(move || update.install(bytes))
            .await
            .map_err(err)?
            .map_err(err)
    }
    .await;
    match result {
        Ok(()) => app.restart(),
        Err(message) if message == BUSY => {
            updates.change(&app, |inner| inner.view.phase = "available")?;
            Err(message)
        }
        Err(message) => {
            updates.change(&app, |inner| {
                inner.view.phase = "error";
                inner.view.error = Some(message.clone());
            })?;
            Err(message)
        }
    }
}
