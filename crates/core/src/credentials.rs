use anyhow::{Context, Result};
use std::{io::Write, path::Path};

pub fn read(path: &Path) -> Result<String> {
    let key = std::fs::read_to_string(path).context("API key unavailable")?;
    anyhow::ensure!(!key.trim().is_empty(), "API key required");
    Ok(key.trim().to_owned())
}

pub fn save(path: &Path, key: &str) -> Result<()> {
    let key = key.trim();
    anyhow::ensure!(!key.is_empty(), "API key required");
    let parent = path.parent().context("API key path has no parent")?;
    // NamedTempFile creates owner-only files on Unix. Persist replaces the
    // destination atomically, so a failed write leaves the previous key intact.
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    file.write_all(key.as_bytes())?;
    file.as_file().sync_all()?;
    file.persist(path).map_err(|error| error.error)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saves_and_replaces_key_without_a_keyring() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("api-key");
        assert!(read(&path).is_err());
        save(&path, "  first-key\n").unwrap();
        assert_eq!(read(&path).unwrap(), "first-key");
        save(&path, "replacement").unwrap();
        assert_eq!(read(&path).unwrap(), "replacement");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn empty_input_preserves_saved_key() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("api-key");
        save(&path, "saved-key").unwrap();
        assert!(save(&path, " \n\t").is_err());
        assert_eq!(read(&path).unwrap(), "saved-key");
    }

    #[cfg(unix)]
    #[test]
    fn key_file_is_private_even_when_replacing_a_public_file() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("api-key");
        std::fs::write(&path, "old-key").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        save(&path, "new-key").unwrap();
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}
