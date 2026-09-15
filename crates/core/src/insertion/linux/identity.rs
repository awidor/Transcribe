use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

const APP_ID: &str = "app.transcribe.desktop";
static REGISTERED: tokio::sync::OnceCell<()> = tokio::sync::OnceCell::const_new();

pub(super) async fn register() -> Result<()> {
    REGISTERED
        .get_or_try_init(|| async {
            if !ashpd::is_sandboxed() {
                tokio::task::spawn_blocking(ensure_desktop_entry).await??;
                // This uses ASHPD's shared connection. Register before any portal
                // method associates that connection with an inferred (or empty) ID.
                ashpd::register_host_app(APP_ID.parse()?)
                    .await
                    .context("Could not register Transcribe with the desktop portal")?;
            }
            Ok::<(), anyhow::Error>(())
        })
        .await?;
    Ok(())
}

fn ensure_desktop_entry() -> Result<()> {
    let data = std::env::var_os("XDG_DATA_HOME")
        .filter(|value| Path::new(value).is_absolute())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share")))
        .context("Could not locate the Linux application directory")?;
    let system = std::env::var_os("XDG_DATA_DIRS")
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "/usr/local/share:/usr/share".into());
    let filename = format!("applications/{APP_ID}.desktop");
    if std::iter::once(data.clone())
        .chain(std::env::split_paths(&system).filter(|path| path.is_absolute()))
        .any(|directory| directory.join(&filename).is_file())
    {
        return Ok(());
    }
    // Unbundled and portable launches need metadata too. Keep this identity
    // entry hidden so it does not duplicate the packaged application launcher.
    let executable = std::env::current_exe()?;
    let entry = desktop_entry(
        executable
            .to_str()
            .context("Executable path is not UTF-8")?,
    );
    std::fs::create_dir_all(data.join("applications"))?;
    std::fs::write(data.join(filename), entry)?;
    Ok(())
}

fn desktop_entry(executable: &str) -> String {
    // Exec quoting is separate from desktop-entry string escaping. Percent
    // signs must also be escaped so paths cannot introduce field codes.
    let mut quoted = String::from("\"");
    for c in executable.chars() {
        match c {
            '%' => quoted.push_str("%%"),
            '\\' | '"' | '$' | '`' => {
                quoted.push('\\');
                quoted.push(c);
            }
            _ => quoted.push(c),
        }
    }
    quoted.push('"');
    let exec = quoted
        .replace('\\', "\\\\")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
        .replace('\t', "\\t");
    format!("[Desktop Entry]\nType=Application\nName=Transcribe\nExec={exec}\nNoDisplay=true\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_matches_desktop_bundle() {
        let config: serde_json::Value =
            serde_json::from_str(include_str!("../../../../../src-tauri/tauri.conf.json")).unwrap();
        assert_eq!(config["identifier"], APP_ID);
    }

    #[test]
    fn executable_cannot_add_fields_or_desktop_entries() {
        let entry = desktop_entry("/tmp/a b/%f\"$`\\\ntranscribe");
        assert!(entry.contains(r#"Exec="/tmp/a b/%%f\\"\\$\\`\\\\\ntranscribe""#));
        assert_eq!(entry.lines().count(), 5);
    }
}
