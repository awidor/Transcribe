use super::*;
use std::{
    future::Future,
    pin::Pin,
    time::{Duration, Instant},
};
use transcribe_core::{
    live::{LivePhase, LiveView, MetaStream, TranscriptState},
    live_audio::LiveRecorder,
    summary::Summarizer,
};

pub(crate) struct LiveState {
    pub view: LiveView,
    stop: CancellationToken,
    cancel: CancellationToken,
}
impl Default for LiveState {
    fn default() -> Self {
        Self {
            view: LiveView::default(),
            stop: CancellationToken::new(),
            cancel: CancellationToken::new(),
        }
    }
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct LiveBootstrap {
    current: LiveView,
    sessions: Vec<LiveView>,
    has_meta_key: bool,
    has_inception_key: bool,
}
fn key_path(state: &AppState, provider: &str) -> PathBuf {
    state.api_key.with_file_name(format!("{provider}-api-key"))
}
fn publish(app: &AppHandle, state: &AppState, view: &mut LiveView, persist: bool) {
    if persist && !view.id.is_empty() {
        if state
            .store
            .lock()
            .map_err(err)
            .and_then(|s| s.save_live(view).map_err(err))
            .is_err()
        {
            view.error =
                Some("Could not save this live session. Copy the text before closing.".into());
        }
    }
    let _ = app.emit_to("main", "live-session", &*view);
}
#[tauri::command]
pub(crate) async fn live_bootstrap(
    window: tauri::WebviewWindow,
    state: State<'_, Arc<AppState>>,
) -> Result<LiveBootstrap> {
    main_only(&window)?;
    let meta = key_path(&state, "meta");
    let inception = key_path(&state, "inception");
    let (has_meta_key, has_inception_key) = tauri::async_runtime::spawn_blocking(move || {
        (
            credentials::read(&meta).is_ok(),
            credentials::read(&inception).is_ok(),
        )
    })
    .await
    .map_err(err)?;
    let sessions = state
        .store
        .lock()
        .map_err(err)?
        .live_sessions()
        .map_err(err)?;
    let current = state.live.lock().await.view.clone();
    Ok(LiveBootstrap {
        current,
        sessions,
        has_meta_key,
        has_inception_key,
    })
}
#[tauri::command]
pub(crate) async fn save_live_keys(
    window: tauri::WebviewWindow,
    state: State<'_, Arc<AppState>>,
    meta: Option<String>,
    inception: Option<String>,
) -> Result<()> {
    main_only(&window)?;
    let _guard = state.settings_lock.lock().await;
    let meta_path = key_path(&state, "meta");
    let inception_path = key_path(&state, "inception");
    tauri::async_runtime::spawn_blocking(move || {
        if let Some(key) = meta {
            credentials::save(&meta_path, &key).map_err(err)?;
        }
        if let Some(key) = inception {
            credentials::save(&inception_path, &key).map_err(err)?;
        }
        Ok(())
    })
    .await
    .map_err(err)?
}
#[tauri::command]
pub(crate) async fn start_live(
    window: tauri::WebviewWindow,
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
) -> Result<()> {
    main_only(&window)?;
    let ordinary = state.session.lock().await;
    if crate::busy(&ordinary.view.phase) {
        return Err("Stop the current transcription first".into());
    }
    let mut live = state.live.lock().await;
    if live.view.phase.active() {
        return Err("A live session is already running".into());
    }
    let id = uuid::Uuid::new_v4().to_string();
    *live = LiveState {
        view: LiveView {
            id: id.clone(),
            created_at: now(),
            phase: LivePhase::Connecting,
            ..Default::default()
        },
        ..Default::default()
    };
    let cancel = live.cancel.clone();
    let stop = live.stop.clone();
    publish(&app, &state, &mut live.view, true);
    let state = state.inner().clone();
    drop(live);
    drop(ordinary);
    tauri::async_runtime::spawn(async move {
        if let Err(error) = run(app.clone(), state.clone(), id.clone(), stop, cancel.clone()).await
        {
            let mut live = state.live.lock().await;
            if live.view.id == id && !cancel.is_cancelled() {
                live.view.phase = LivePhase::Error;
                live.view.error = Some(error);
                live.view.summarizing = false;
                publish(&app, &state, &mut live.view, true);
            }
        }
    });
    Ok(())
}
#[tauri::command]
pub(crate) async fn stop_live(
    window: tauri::WebviewWindow,
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
) -> Result<()> {
    main_only(&window)?;
    let mut live = state.live.lock().await;
    if live.view.phase.active() {
        live.stop.cancel();
        live.view.phase = LivePhase::Stopping;
        publish(&app, &state, &mut live.view, false);
    }
    Ok(())
}
#[tauri::command]
pub(crate) async fn cancel_live(
    window: tauri::WebviewWindow,
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
) -> Result<()> {
    main_only(&window)?;
    let mut live = state.live.lock().await;
    if !live.view.phase.active() {
        return Ok(());
    }
    if live.view.phase == LivePhase::Listening {
        live.view.seconds = (now() - live.view.created_at).max(0) as f64 / 1000.;
    }
    live.cancel.cancel();
    live.stop.cancel();
    live.view.phase = LivePhase::Cancelled;
    live.view.summarizing = false;
    // Completed text remains available; interim text is explicitly provisional.
    publish(&app, &state, &mut live.view, true);
    Ok(())
}

type SummaryFuture = Pin<Box<dyn Future<Output = Result<String>> + Send>>;
struct SummaryJob {
    future: SummaryFuture,
    revision: usize,
    finished: bool,
}

async fn run(
    app: AppHandle,
    state: Arc<AppState>,
    id: String,
    stop: CancellationToken,
    cancel: CancellationToken,
) -> Result<()> {
    let meta_path = key_path(&state, "meta");
    let inception_path = key_path(&state, "inception");
    let (meta, inception) = tauri::async_runtime::spawn_blocking(move || {
        Ok::<_, String>((
            credentials::read(&meta_path).map_err(|_| "Add your Meta API key in Settings")?,
            credentials::read(&inception_path)
                .map_err(|_| "Add your Inception API key in Settings")?,
        ))
    })
    .await
    .map_err(err)??;
    if cancel.is_cancelled() {
        return Ok(());
    }
    let ws = tokio::select! {
        _ = stop.cancelled() => { finish_without_capture(&app, &state, &id).await; return Ok(()); }
        r = MetaStream::connect(&meta, &cancel) => r.map_err(err)?,
    };
    if cancel.is_cancelled() || stop.is_cancelled() {
        finish_without_capture(&app, &state, &id).await;
        return Ok(());
    }
    let microphone = state.store.lock().map_err(err)?.settings().microphone;
    let meter_app = app.clone();
    let (recorder, audio) = tauri::async_runtime::spawn_blocking(move || {
        LiveRecorder::start(
            microphone,
            Arc::new(move |level| {
                let _ = meter_app.emit_to("main", "live-level", level);
            }),
        )
    })
    .await
    .map_err(err)?
    .map_err(err)?;
    let mut recorder = Some(recorder);
    let started = Instant::now();
    {
        let mut live = state.live.lock().await;
        if live.view.id != id || cancel.is_cancelled() {
            return Ok(());
        }
        live.view.created_at = now();
        live.view.phase = if stop.is_cancelled() {
            LivePhase::Stopping
        } else {
            LivePhase::Listening
        };
        publish(&app, &state, &mut live.view, true);
    }
    let (events_tx, mut events) = tokio::sync::mpsc::unbounded_channel();
    let network = ws.run(
        audio,
        cancel.clone(),
        Arc::new(move |event| {
            let _ = events_tx.send(event);
        }),
    );
    tokio::pin!(network);
    let mut transcript = TranscriptState::default();
    let summarizer = Arc::new(Summarizer::default());
    let mut summary: Option<SummaryJob> = None;
    let mut last_attempt = Instant::now();
    let mut last_revision = 0;
    let mut ended = false;
    let mut final_summary_attempted = false;
    let mut ticker = tokio::time::interval(Duration::from_millis(500));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            biased;
            _ = cancel.cancelled() => return Ok(()),
            _ = stop.cancelled(), if recorder.is_some() => {
                recorder.take();
                let mut live = state.live.lock().await;
                if live.view.id != id || cancel.is_cancelled() {
                    return Ok(());
                }
                live.view.seconds = started.elapsed().as_secs_f64();
                live.view.phase = LivePhase::Stopping;
                publish(&app, &state, &mut live.view, true);
            }
            event = events.recv(), if !ended => {
                if let Some(event) = event {
                    apply_event(&app, &state, &id, &mut transcript, event).await?;
                }
            }
            result = &mut network, if !ended => {
                recorder.take();
                while let Ok(event) = events.try_recv() {
                    apply_event(&app, &state, &id, &mut transcript, event).await?;
                }
                ended = true;
                let mut live = state.live.lock().await;
                if live.view.id != id || cancel.is_cancelled() {
                    return Ok(());
                }
                if live.view.seconds == 0. {
                    live.view.seconds = started.elapsed().as_secs_f64();
                }
                live.view.phase = LivePhase::Stopping;
                live.view.error = result.err().map(err).or_else(|| {
                    (!transcript.complete()).then(||
                        "The last speech turn was not finalized. Its text is shown as provisional.".into())
                }).or(live.view.error.take());
                publish(&app, &state, &mut live.view, true);
            }
            result = async {
                match &mut summary {
                    Some(job) => job.future.as_mut().await,
                    None => std::future::pending().await,
                }
            } => {
                let job = summary.take().unwrap();
                let mut live = state.live.lock().await;
                if live.view.id != id || cancel.is_cancelled() {
                    return Ok(());
                }
                live.view.summarizing = false;
                match result {
                    Ok(text) => {
                        live.view.summary = text;
                        live.view.summary_revision = job.revision;
                        live.view.summary_updated_at = Some(now());
                        live.view.summary_error = None;
                    }
                    Err(e) => live.view.summary_error = Some(err(e)),
                }
                publish(&app, &state, &mut live.view, true);
                if job.finished {
                    break;
                }
            }
            _ = ticker.tick() => {
                if started.elapsed() >= Duration::from_secs(59 * 60) && recorder.is_some() {
                    // Meta caps each streaming connection at one hour. Finish
                    // cleanly before that limit, without losing the final turn.
                    stop.cancel();
                    let mut live = state.live.lock().await;
                    if live.view.id != id || cancel.is_cancelled() {
                        return Ok(());
                    }
                    live.view.error = Some("Reached the 59-minute live session limit. Start a new session to continue.".into());
                }
            }
        }
        if ended && transcript.text.is_empty() && summary.is_none() {
            break;
        }
        let due = ended
            || (transcript.revision > last_revision
                && last_attempt.elapsed()
                    >= Duration::from_secs(if transcript.speaking { 30 } else { 20 }));
        if due && summary.is_none() && !transcript.text.is_empty() && !final_summary_attempted {
            let mut live = state.live.lock().await;
            if live.view.id != id || cancel.is_cancelled() {
                return Ok(());
            }
            let text = transcript.text.clone();
            let previous = live.view.summary.clone();
            let client = summarizer.clone();
            let key = inception.clone();
            let token = cancel.clone();
            let finished = ended;
            summary = Some(SummaryJob {
                revision: transcript.revision,
                finished,
                future: Box::pin(async move {
                    client
                        .summarize(&text, &previous, finished, &key, token)
                        .await
                        .map_err(err)
                }),
            });
            last_revision = transcript.revision;
            last_attempt = Instant::now();
            final_summary_attempted = ended;
            live.view.summarizing = true;
            live.view.summary_error = None;
            publish(&app, &state, &mut live.view, false);
        }
    }
    let mut live = state.live.lock().await;
    if live.view.id != id || cancel.is_cancelled() {
        return Ok(());
    }
    {
        live.view.phase = if live.view.error.is_some() {
            LivePhase::Error
        } else {
            LivePhase::Done
        };
        live.view.summarizing = false;
        publish(&app, &state, &mut live.view, true);
    }
    Ok(())
}
async fn finish_without_capture(app: &AppHandle, state: &AppState, id: &str) {
    let mut live = state.live.lock().await;
    if live.view.id == id && !live.cancel.is_cancelled() {
        live.view.phase = LivePhase::Done;
        publish(app, state, &mut live.view, true);
    }
}
async fn apply_event(
    app: &AppHandle,
    state: &AppState,
    id: &str,
    transcript: &mut TranscriptState,
    event: serde_json::Value,
) -> Result<()> {
    let old_revision = transcript.revision;
    if transcript.apply(&event).map_err(err)? {
        let mut live = state.live.lock().await;
        if live.view.id != id || live.cancel.is_cancelled() {
            return Ok(());
        }
        live.view.transcript = transcript.text.clone();
        live.view.interim = transcript.interim();
        live.view.revision = transcript.revision;
        publish(
            app,
            state,
            &mut live.view,
            old_revision != transcript.revision,
        );
    }
    Ok(())
}
#[tauri::command]
pub(crate) async fn copy_live(
    window: tauri::WebviewWindow,
    state: State<'_, Arc<AppState>>,
    id: String,
    summary: bool,
) -> Result<()> {
    main_only(&window)?;
    let current = state.live.lock().await.view.clone();
    let view = if current.id == id {
        current
    } else {
        state
            .store
            .lock()
            .map_err(err)?
            .live_sessions()
            .map_err(err)?
            .into_iter()
            .find(|v| v.id == id)
            .ok_or("Live session unavailable")?
    };
    insertion::copy(if summary {
        view.summary
    } else {
        view.transcript
    })
    .await
    .map_err(err)
}
#[tauri::command]
pub(crate) async fn delete_live(
    window: tauri::WebviewWindow,
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    id: String,
) -> Result<()> {
    main_only(&window)?;
    let mut live = state.live.lock().await;
    if live.view.id == id && live.view.phase.active() {
        return Err("Stop the live session before deleting it".into());
    }
    state
        .store
        .lock()
        .map_err(err)?
        .delete_live(&id)
        .map_err(err)?;
    if live.view.id == id {
        *live = LiveState::default();
    }
    let _ = app.emit_to("main", "live-history", ());
    Ok(())
}
#[tauri::command]
pub(crate) async fn retry_live_summary(
    window: tauri::WebviewWindow,
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    id: String,
) -> Result<()> {
    main_only(&window)?;
    let mut live = state.live.lock().await;
    if live.view.phase.active() {
        return Err("Wait for the current live session to finish".into());
    }
    let mut view = state
        .store
        .lock()
        .map_err(err)?
        .live_sessions()
        .map_err(err)?
        .into_iter()
        .find(|v| v.id == id)
        .ok_or("Live session unavailable")?;
    if view.transcript.is_empty() {
        return Err("No finalized speech to summarize".into());
    }
    let path = key_path(&state, "inception");
    let key = tauri::async_runtime::spawn_blocking(move || credentials::read(&path))
        .await
        .map_err(err)?
        .map_err(err)?;
    let phase = view.phase;
    view.phase = LivePhase::Stopping;
    view.summarizing = true;
    view.summary_error = None;
    *live = LiveState {
        view,
        ..Default::default()
    };
    let cancel = live.cancel.clone();
    let text = live.view.transcript.clone();
    let previous = live.view.summary.clone();
    publish(&app, &state, &mut live.view, true);
    let state = state.inner().clone();
    drop(live);
    tauri::async_runtime::spawn(async move {
        let result = Summarizer::default()
            .summarize(&text, &previous, true, &key, cancel.clone())
            .await;
        let mut live = state.live.lock().await;
        if live.view.id != id || cancel.is_cancelled() {
            return;
        }
        live.view.phase = phase;
        live.view.summarizing = false;
        match result {
            Ok(summary) => {
                live.view.summary = summary;
                live.view.summary_revision = live.view.revision;
                live.view.summary_updated_at = Some(now());
                live.view.summary_error = None;
            }
            Err(e) => live.view.summary_error = Some(err(e)),
        }
        publish(&app, &state, &mut live.view, true);
    });
    Ok(())
}
