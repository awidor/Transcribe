use anyhow::{Context, Result};
use serde::Deserialize;
use std::{io::Write, sync::Mutex, time::Duration};
use tokio::sync::oneshot;

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub(super) struct Window {
    pub id: String,
    pub class: String,
}

struct FocusReply(Mutex<Option<oneshot::Sender<String>>>);

#[zbus::interface(name = "app.transcribe.Focus")]
impl FocusReply {
    fn report(&self, window: String) {
        if let Some(sender) = self.0.lock().unwrap().take() {
            let _ = sender.send(window);
        }
    }
}

pub(super) fn available() -> bool {
    std::env::var("XDG_CURRENT_DESKTOP")
        .unwrap_or_default()
        .split(':')
        .any(|d| d.eq_ignore_ascii_case("KDE"))
}

pub(super) async fn focused() -> Result<Window> {
    let (sender, receiver) = oneshot::channel();
    let connection = zbus::connection::Builder::session()?
        .serve_at(
            "/app/transcribe/Focus",
            FocusReply(Mutex::new(Some(sender))),
        )?
        .build()
        .await?;
    let destination = serde_json::to_string(
        connection
            .unique_name()
            .context("Session bus unavailable")?
            .as_str(),
    )?;
    // A short-lived script reads only the current window's identity. It never
    // activates a destination or depends on applications enabling AT-SPI.
    let mut script = tempfile::NamedTempFile::new()?;
    write!(
        script,
        r#"const w = workspace.activeWindow;
callDBus({destination}, "/app/transcribe/Focus", "app.transcribe.Focus", "Report",
    JSON.stringify(w && !w.desktopWindow && !w.dock ? {{id: String(w.internalId), class: String(w.resourceClass)}} : null));"#
    )?;
    let name = format!("transcribe-focus-{}", uuid::Uuid::new_v4());
    let scripting = zbus::Proxy::new(
        &connection,
        "org.kde.KWin",
        "/Scripting",
        "org.kde.kwin.Scripting",
    )
    .await?;
    let id: i32 = scripting
        .call(
            "loadScript",
            &(
                script.path().to_str().context("Invalid script path")?,
                &name,
            ),
        )
        .await?;
    anyhow::ensure!(id >= 0, "KDE focus lookup unavailable");
    let result = tokio::time::timeout(Duration::from_secs(2), async {
        let path = format!("/Scripting/Script{id}");
        let runner = zbus::Proxy::new(
            &connection,
            "org.kde.KWin",
            path.as_str(),
            "org.kde.kwin.Script",
        )
        .await?;
        runner.call::<_, _, ()>("run", &()).await?;
        let json = receiver.await?;
        let window: Option<Window> = serde_json::from_str(&json)?;
        window
            .filter(|w| !w.id.is_empty())
            .context("Focus a destination before pasting")
    })
    .await
    .context("KDE focus lookup timed out");
    let _: std::result::Result<bool, _> = scripting.call("unloadScript", &(&name,)).await;
    result?
}
