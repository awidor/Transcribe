use anyhow::{bail, Result};
use std::{collections::HashSet, future::Future, sync::OnceLock};
use tokio::sync::Mutex;

mod destination;
#[cfg(windows)]
mod windows;
#[cfg(windows)]
use windows as platform;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
use macos as platform;
#[cfg(target_os = "linux")]
pub mod linux;
#[cfg(target_os = "linux")]
use linux as platform;

static INSERTIONS: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
static CLIPBOARD: OnceLock<Mutex<()>> = OnceLock::new();
pub async fn insert(id: &str, text: &str) -> Result<()> {
    insert_with(id, text, platform::capture, platform::paste).await
}
async fn insert_with<T, C, P, CF, PF>(id: &str, text: &str, capture: C, paste: P) -> Result<()>
where
    C: FnOnce() -> CF,
    P: FnOnce(T, String) -> PF,
    CF: Future<Output = Result<T>>,
    PF: Future<Output = Result<()>>,
{
    let mut attempted = INSERTIONS.get_or_init(Mutex::default).lock().await;
    if !attempted.insert(id.into()) {
        bail!("Insertion already attempted");
    }
    drop(attempted);
    let _guard = CLIPBOARD.get_or_init(Mutex::default).lock().await;
    // Resolve the current app and field only when this paste can proceed. A
    // recording never owns a destination, even while waiting on the clipboard.
    let target = capture().await?;
    paste(target, text.to_owned()).await
}
pub async fn copy(text: String) -> Result<()> {
    let _guard = CLIPBOARD.get_or_init(Mutex::default).lock().await;
    platform::copy(text).await
}
/// Chromium and Electron build their accessibility trees only after a client
/// asks. Asking when recording starts lets delivery recognize their text fields.
pub async fn prepare() {
    platform::prepare().await
}

pub(crate) fn prepare_text(text: &str, terminal: bool) -> String {
    // Terminal line breaks can submit commands even without an Enter event.
    // Keep the original, unmodified transcript in history.
    if terminal {
        text.split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .chars()
            .filter(|c| !c.is_control())
            .collect()
    } else {
        text.chars()
            .filter(|c| !c.is_control() || *c == '\n' || *c == '\t')
            .collect()
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };

    #[tokio::test]
    async fn follows_the_destination_at_delivery_after_switching_away_and_back() {
        let current = Arc::new(AtomicUsize::new(1));
        // These switches happen while recording/transcribing. There is no
        // captured target or stale accessibility element to invalidate.
        current.store(2, Ordering::SeqCst);
        current.store(1, Ordering::SeqCst);
        let active = current.clone();
        insert_with(
            &uuid::Uuid::new_v4().to_string(),
            "Hello",
            || async { Ok(active.load(Ordering::SeqCst)) },
            |target, text| async move {
                assert_eq!(target, 1);
                assert_eq!(text, "Hello");
                Ok(())
            },
        )
        .await
        .unwrap();
        current.store(3, Ordering::SeqCst);
        insert_with(
            &uuid::Uuid::new_v4().to_string(),
            "Terminal",
            || async { Ok(current.load(Ordering::SeqCst)) },
            |target, _| async move {
                assert_eq!(target, 3);
                Ok(())
            },
        )
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn resolves_focus_after_waiting_for_clipboard_and_never_dispatches_twice() {
        let clipboard = CLIPBOARD.get_or_init(Mutex::default).lock().await;
        let current = Arc::new(AtomicUsize::new(1));
        let captures = Arc::new(AtomicUsize::new(0));
        let active = current.clone();
        let observed = captures.clone();
        let id = uuid::Uuid::new_v4().to_string();
        let job_id = id.clone();
        let job = tokio::spawn(async move {
            insert_with(
                &job_id,
                "Hello",
                || async {
                    observed.fetch_add(1, Ordering::SeqCst);
                    Ok(active.load(Ordering::SeqCst))
                },
                |target, _| async move {
                    assert_eq!(target, 2);
                    Ok(())
                },
            )
            .await
        });
        tokio::task::yield_now().await;
        assert_eq!(captures.load(Ordering::SeqCst), 0);
        current.store(2, Ordering::SeqCst);
        drop(clipboard);
        job.await.unwrap().unwrap();
        assert_eq!(captures.load(Ordering::SeqCst), 1);
        let duplicate = insert_with(
            &id,
            "duplicate",
            || async {
                panic!("Must not recapture a dispatched job");
                #[allow(unreachable_code)]
                Ok(0)
            },
            |_: i32, _| async {
                panic!("Must not paste twice");
                #[allow(unreachable_code)]
                Ok(())
            },
        )
        .await;
        assert!(duplicate.is_err());
    }

    #[tokio::test]
    async fn unavailable_current_destination_does_not_dispatch() {
        let result = insert_with(
            &uuid::Uuid::new_v4().to_string(),
            "Hello",
            || async { anyhow::bail!("No editable field") },
            |_: (), _| async {
                panic!("No target must never dispatch");
                #[allow(unreachable_code)]
                Ok(())
            },
        )
        .await;
        assert_eq!(result.unwrap_err().to_string(), "No editable field");
    }
    #[test]
    fn terminal_input_cannot_submit_or_inject_escape_sequences() {
        assert_eq!(
            prepare_text("echo hi\r\nrm file\n\u{1b}\u{3}", true),
            "echo hi rm file "
        );
        assert_eq!(prepare_text("Grüße\n世界 👋", false), "Grüße\n世界 👋");
    }
}
