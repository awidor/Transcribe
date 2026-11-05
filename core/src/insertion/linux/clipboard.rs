use super::xselection;
use anyhow::{ensure, Context, Result};
use std::{
    collections::{HashMap, HashSet},
    os::fd::{FromRawFd, IntoRawFd},
    sync::OnceLock,
    time::Duration,
};
use tokio::io::AsyncReadExt;
use wl_clipboard_rs::{copy, paste};

/// True where selections go through X11: an X11 session, or GNOME, which has
/// no data-control protocol and shares its XWayland selections instead.
fn x11() -> bool {
    static X11: OnceLock<bool> = OnceLock::new();
    *X11.get_or_init(|| {
        !crate::is_wayland()
            || matches!(
                paste::get_mime_types(paste::ClipboardType::Regular, paste::Seat::Unspecified),
                Err(paste::Error::MissingProtocol { .. })
            )
    })
}

fn x_selection(clipboard: paste::ClipboardType) -> xselection::Selection {
    match clipboard {
        paste::ClipboardType::Regular => xselection::Selection::Clipboard,
        paste::ClipboardType::Primary => xselection::Selection::Primary,
    }
}

pub(super) async fn formats(clipboard: paste::ClipboardType) -> Result<HashSet<String>> {
    tokio::task::spawn_blocking(move || {
        if x11() {
            return xselection::with(|s| s.formats(x_selection(clipboard)));
        }
        match paste::get_mime_types(clipboard, paste::Seat::Unspecified) {
            Ok(formats) => Ok(formats),
            Err(paste::Error::ClipboardEmpty) => Ok(HashSet::new()),
            Err(error) => Err(error.into()),
        }
    })
    .await?
}

async fn read(clipboard: paste::ClipboardType, format: String) -> Result<Vec<u8>> {
    if x11() {
        return tokio::task::spawn_blocking(move || {
            xselection::with(|s| s.read(x_selection(clipboard), &format))
        })
        .await?;
    }
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

pub(super) async fn snapshot(clipboard: paste::ClipboardType) -> Result<HashMap<String, Vec<u8>>> {
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

pub(super) async fn write(
    clipboard: copy::ClipboardType,
    data: HashMap<String, Vec<u8>>,
) -> Result<()> {
    tokio::task::spawn_blocking(move || {
        if x11() {
            let selections = match clipboard {
                copy::ClipboardType::Regular => vec![xselection::Selection::Clipboard],
                copy::ClipboardType::Primary => vec![xselection::Selection::Primary],
                copy::ClipboardType::Both => vec![
                    xselection::Selection::Clipboard,
                    xselection::Selection::Primary,
                ],
            };
            return xselection::with(|s| {
                selections
                    .into_iter()
                    .try_for_each(|selection| s.write(selection, data.clone()))
            });
        }
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

// X11 clients, and GNOME's bridge from the X11 clipboard, ask for UTF8_STRING.
pub(super) fn plain_text(text: String) -> HashMap<String, Vec<u8>> {
    let bytes = text.into_bytes();
    HashMap::from([
        ("text/plain;charset=utf-8".into(), bytes.clone()),
        ("UTF8_STRING".into(), bytes),
    ])
}

pub(super) async fn restore(
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
