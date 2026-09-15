use super::{valid, Target};
use anyhow::{ensure, Context, Result};
use std::{
    collections::{HashMap, HashSet},
    os::fd::{FromRawFd, IntoRawFd},
    time::Duration,
};
use tokio::io::AsyncReadExt;
use wl_clipboard_rs::{copy, paste};

async fn formats() -> Result<HashSet<String>> {
    tokio::task::spawn_blocking(|| {
        match paste::get_mime_types(paste::ClipboardType::Regular, paste::Seat::Unspecified) {
            Ok(formats) => Ok(formats),
            Err(paste::Error::ClipboardEmpty) => Ok(HashSet::new()),
            Err(error) => Err(error.into()),
        }
    })
    .await?
}

async fn read(format: String) -> Result<Vec<u8>> {
    let pipe = tokio::task::spawn_blocking(move || {
        paste::get_contents(
            paste::ClipboardType::Regular,
            paste::Seat::Unspecified,
            paste::MimeType::Specific(&format),
        )
        .map(|(pipe, _)| pipe)
    })
    .await??;
    // Ownership of the pipe fd moves into the file; it is closed exactly once.
    let file = unsafe { std::fs::File::from_raw_fd(pipe.into_raw_fd()) };
    let mut bytes = Vec::new();
    tokio::time::timeout(
        Duration::from_secs(2),
        tokio::fs::File::from_std(file)
            .take(64 * 1024 * 1024 + 1)
            .read_to_end(&mut bytes),
    )
    .await
    .context("Clipboard read timed out")??;
    ensure!(bytes.len() <= 64 * 1024 * 1024, "Clipboard too large");
    Ok(bytes)
}

async fn snapshot() -> Result<HashMap<String, Vec<u8>>> {
    let initial = formats().await?;
    let mut data = HashMap::new();
    let mut total = 0;
    for format in &initial {
        let bytes = read(format.clone()).await?;
        total += bytes.len();
        ensure!(total <= 64 * 1024 * 1024, "Clipboard too large");
        data.insert(format.clone(), bytes);
    }
    ensure!(formats().await? == initial, "Clipboard changed");
    Ok(data)
}

async fn write(data: HashMap<String, Vec<u8>>) -> Result<()> {
    tokio::task::spawn_blocking(move || {
        if data.is_empty() {
            copy::clear(copy::ClipboardType::Regular, copy::Seat::All)?;
        } else {
            copy::Options::new().copy_multi(
                data.into_iter()
                    .map(|(mime_type, bytes)| copy::MimeSource {
                        mime_type: copy::MimeType::Specific(mime_type),
                        source: copy::Source::Bytes(bytes.into_boxed_slice()),
                    })
                    .collect(),
            )?;
        }
        Ok(())
    })
    .await?
}

pub(super) async fn copy(text: String) -> Result<()> {
    write(HashMap::from([(
        "text/plain;charset=utf-8".into(),
        text.into_bytes(),
    )]))
    .await
}

pub(super) async fn paste(target: Target, text: String) -> Result<()> {
    let saved = snapshot().await?;
    tokio::task::spawn_blocking(crate::linux_input::wait_released).await??;
    ensure!(valid(&target).await, "Destination changed");
    ensure!(snapshot().await? == saved, "Clipboard changed");
    // A unique MIME marker identifies this selection, even if the user copies
    // identical text while delivery is in progress. Never overwrite a new copy.
    let marker = format!("application/x-transcribe-{}", uuid::Uuid::new_v4());
    write(HashMap::from([
        ("text/plain;charset=utf-8".into(), text.into_bytes()),
        (marker.clone(), Vec::new()),
    ]))
    .await?;
    let dispatch = async {
        ensure!(valid(&target).await, "Destination changed");
        tokio::task::spawn_blocking(move || crate::linux_input::paste(target.terminal)).await??;
        // Keep the selection alive while native and XWayland editors request it.
        tokio::time::sleep(Duration::from_millis(1200)).await;
        Ok(())
    }
    .await;
    if formats().await?.contains(&marker) {
        write(saved).await?;
    }
    dispatch
}
