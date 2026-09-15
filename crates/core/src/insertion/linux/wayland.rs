use super::{valid, Target};
use crate::linux_input::PasteShortcut;
use anyhow::{ensure, Context, Result};
use std::{
    collections::{HashMap, HashSet},
    os::fd::{FromRawFd, IntoRawFd},
    time::Duration,
};
use tokio::io::AsyncReadExt;
use wl_clipboard_rs::{copy, paste};

async fn formats(clipboard: paste::ClipboardType) -> Result<HashSet<String>> {
    tokio::task::spawn_blocking(move || {
        match paste::get_mime_types(clipboard, paste::Seat::Unspecified) {
            Ok(formats) => Ok(formats),
            Err(paste::Error::ClipboardEmpty) => Ok(HashSet::new()),
            Err(error) => Err(error.into()),
        }
    })
    .await?
}

async fn read(clipboard: paste::ClipboardType, format: String) -> Result<Vec<u8>> {
    let pipe = tokio::task::spawn_blocking(move || {
        paste::get_contents(
            clipboard,
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

async fn snapshot(clipboard: paste::ClipboardType) -> Result<HashMap<String, Vec<u8>>> {
    let initial = formats(clipboard).await?;
    let mut data = HashMap::new();
    let mut total = 0;
    for format in &initial {
        let bytes = read(clipboard, format.clone()).await?;
        total += bytes.len();
        ensure!(total <= 64 * 1024 * 1024, "Clipboard too large");
        data.insert(format.clone(), bytes);
    }
    ensure!(formats(clipboard).await? == initial, "Clipboard changed");
    Ok(data)
}

async fn write(clipboard: copy::ClipboardType, data: HashMap<String, Vec<u8>>) -> Result<()> {
    tokio::task::spawn_blocking(move || {
        if data.is_empty() {
            copy::clear(clipboard, copy::Seat::All)?;
        } else {
            let mut options = copy::Options::new();
            options.clipboard(clipboard);
            options.copy_multi(
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
    write(
        copy::ClipboardType::Regular,
        HashMap::from([("text/plain;charset=utf-8".into(), text.into_bytes())]),
    )
    .await
}

pub(super) async fn paste(target: Target, text: String) -> Result<()> {
    let saved = snapshot(paste::ClipboardType::Regular).await?;
    // Shift+Insert is also supported by traditional terminals that pass
    // Ctrl+Shift+V through to the running program. Some paste PRIMARY, others
    // CLIPBOARD, so offer the same text on both without changing user settings.
    let primary = if target.terminal {
        match snapshot(paste::ClipboardType::Primary).await {
            Ok(saved) => Some(saved),
            Err(error)
                if matches!(
                    error.downcast_ref::<paste::Error>(),
                    Some(paste::Error::PrimarySelectionUnsupported)
                ) =>
            {
                None
            }
            Err(error) => return Err(error),
        }
    } else {
        None
    };
    tokio::task::spawn_blocking(crate::linux_input::wait_released).await??;
    ensure!(valid(&target).await, "Destination changed");
    ensure!(
        snapshot(paste::ClipboardType::Regular).await? == saved,
        "Clipboard changed"
    );
    if let Some(saved) = &primary {
        ensure!(
            snapshot(paste::ClipboardType::Primary).await? == *saved,
            "Selection changed"
        );
    }
    // A unique MIME marker identifies this selection, even if the user copies
    // identical text while delivery is in progress. Never overwrite a new copy.
    let marker = format!("application/x-transcribe-{}", uuid::Uuid::new_v4());
    let clipboard = if primary.is_some() {
        copy::ClipboardType::Both
    } else {
        copy::ClipboardType::Regular
    };
    let shortcut = if primary.is_some() {
        PasteShortcut::TerminalSelection
    } else if target.terminal {
        PasteShortcut::TerminalClipboard
    } else {
        PasteShortcut::Clipboard
    };
    write(
        clipboard,
        HashMap::from([
            ("text/plain;charset=utf-8".into(), text.into_bytes()),
            (marker.clone(), Vec::new()),
        ]),
    )
    .await?;
    let dispatch = async {
        ensure!(valid(&target).await, "Destination changed");
        tokio::task::spawn_blocking(move || crate::linux_input::paste(shortcut)).await??;
        // Keep the selection alive while native and XWayland editors request it.
        tokio::time::sleep(Duration::from_millis(1200)).await;
        Ok(())
    }
    .await;
    // Restore each selection independently; a new copy or mouse selection must
    // survive even if the other selection is still ours. Attempt both cleanups.
    let regular_restore = restore(paste::ClipboardType::Regular, saved, &marker).await;
    let primary_restore = if let Some(saved) = primary {
        restore(paste::ClipboardType::Primary, saved, &marker).await
    } else {
        Ok(())
    };
    regular_restore?;
    primary_restore?;
    dispatch
}

async fn restore(
    clipboard: paste::ClipboardType,
    saved: HashMap<String, Vec<u8>>,
    marker: &str,
) -> Result<()> {
    if formats(clipboard).await?.contains(marker) {
        let clipboard = match clipboard {
            paste::ClipboardType::Regular => copy::ClipboardType::Regular,
            paste::ClipboardType::Primary => copy::ClipboardType::Primary,
        };
        write(clipboard, saved).await?;
    }
    Ok(())
}
