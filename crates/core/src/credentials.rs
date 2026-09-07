use anyhow::{Context, Result};
pub fn read() -> Result<String> {
    keyring::Entry::new("app.transcribe.desktop", "openrouter")?
        .get_password()
        .context("API key unavailable")
}
pub fn save(key: &str) -> Result<()> {
    anyhow::ensure!(!key.trim().is_empty(), "API key required");
    keyring::Entry::new("app.transcribe.desktop", "openrouter")?
        .set_password(key.trim())
        .context("Key could not be saved")
}
