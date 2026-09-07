use anyhow::Result;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    pub microphone: Option<String>,
    pub shortcut: String,
    #[serde(default)]
    pub shortcut_label: Option<String>,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            microphone: None,
            shortcut: "CommandOrControl+Shift+Space".into(),
            shortcut_label: None,
        }
    }
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Entry {
    pub id: String,
    pub created_at: i64,
    pub text: String,
    pub seconds: f64,
    pub status: String,
    pub error: Option<String>,
}

pub struct Store {
    connection: Connection,
}
impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        let c = Connection::open(path)?;
        c.execute_batch("PRAGMA journal_mode=WAL; CREATE TABLE IF NOT EXISTS transcripts (id TEXT PRIMARY KEY, created_at INTEGER NOT NULL, text TEXT NOT NULL, seconds REAL NOT NULL, status TEXT NOT NULL, error TEXT); CREATE TABLE IF NOT EXISTS settings (id INTEGER PRIMARY KEY CHECK(id=1), data TEXT NOT NULL);")?;
        // A crash after dispatch cannot safely be retried on startup.
        c.execute(
            "UPDATE transcripts SET status='unverified' WHERE status='pending'",
            [],
        )?;
        c.execute("UPDATE transcripts SET status='failed',error='Interrupted' WHERE status='transcribing'", [])?;
        Ok(Self { connection: c })
    }
    pub fn list(&self) -> Result<Vec<Entry>> {
        let mut q = self.connection.prepare("SELECT id,created_at,text,seconds,status,error FROM transcripts ORDER BY created_at DESC")?;
        let rows = q.query_map([], |r| {
            Ok(Entry {
                id: r.get(0)?,
                created_at: r.get(1)?,
                text: r.get(2)?,
                seconds: r.get(3)?,
                status: r.get(4)?,
                error: r.get(5)?,
            })
        })?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }
    pub fn insert(&self, e: &Entry) -> Result<()> {
        self.connection.execute(
            "INSERT INTO transcripts VALUES (?1,?2,?3,?4,?5,?6)",
            params![e.id, e.created_at, e.text, e.seconds, e.status, e.error],
        )?;
        Ok(())
    }
    pub fn status(&self, id: &str, status: &str, error: Option<&str>) -> Result<()> {
        self.connection.execute(
            "UPDATE transcripts SET status=?2,error=?3 WHERE id=?1",
            params![id, status, error],
        )?;
        Ok(())
    }
    pub fn edit(&self, id: &str, text: &str) -> Result<()> {
        self.connection.execute(
            "UPDATE transcripts SET text=?2 WHERE id=?1",
            params![id, text],
        )?;
        Ok(())
    }
    pub fn delete(&self, id: &str) -> Result<()> {
        self.connection
            .execute("DELETE FROM transcripts WHERE id=?1", [id])?;
        Ok(())
    }
    pub fn settings(&self) -> Settings {
        self.connection
            .query_row("SELECT data FROM settings WHERE id=1", [], |r| {
                r.get::<_, String>(0)
            })
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }
    pub fn save_settings(&self, s: &Settings) -> Result<()> {
        self.connection.execute(
            "INSERT INTO settings VALUES (1,?1) ON CONFLICT(id) DO UPDATE SET data=excluded.data",
            [serde_json::to_string(s)?],
        )?;
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unicode_history_and_settings_roundtrip() {
        let store = Store::open(Path::new(":memory:")).unwrap();
        let e = Entry {
            id: "one".into(),
            created_at: 1,
            text: "Grüße 你好 👋".into(),
            seconds: 1.5,
            status: "pending".into(),
            error: None,
        };
        store.insert(&e).unwrap();
        store.status("one", "unverified", None).unwrap();
        assert_eq!(store.list().unwrap()[0].text, e.text);
        let settings = Settings {
            microphone: Some("USB".into()),
            ..Settings::default()
        };
        store.save_settings(&settings).unwrap();
        assert_eq!(store.settings().microphone, settings.microphone);
        store.delete("one").unwrap();
        assert!(store.list().unwrap().is_empty());
    }
}
