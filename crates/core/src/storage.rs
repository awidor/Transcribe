use crate::live::{LivePhase, LiveView};
use crate::provider::{CleanupEngine, ReasoningEffort, DEFAULT_CLEANUP_MODEL};
use crate::s1::Styling;
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
    #[serde(default = "default_cleanup_model")]
    pub cleanup_model: String,
    #[serde(default = "default_cleanup_reasoning_effort")]
    pub cleanup_reasoning_effort: Option<ReasoningEffort>,
    #[serde(default)]
    pub cleanup_engine: CleanupEngine,
    #[serde(default)]
    pub cleanup_styling: Styling,
}
fn default_cleanup_model() -> String {
    DEFAULT_CLEANUP_MODEL.into()
}
fn default_cleanup_reasoning_effort() -> Option<ReasoningEffort> {
    Some(ReasoningEffort::Low)
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            microphone: None,
            shortcut: "CommandOrControl+Shift+Space".into(),
            shortcut_label: None,
            cleanup_model: default_cleanup_model(),
            cleanup_reasoning_effort: default_cleanup_reasoning_effort(),
            cleanup_engine: CleanupEngine::OpenRouter,
            cleanup_styling: Styling::SemiFormal,
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
        c.execute_batch("CREATE TABLE IF NOT EXISTS live_sessions (id TEXT PRIMARY KEY, created_at INTEGER NOT NULL, data TEXT NOT NULL);")?;
        let store = Self { connection: c };
        for mut session in store.live_sessions()? {
            if session.phase.active() || session.summarizing {
                session.phase = LivePhase::Error;
                session.summarizing = false;
                session.error = Some(
                    "Session interrupted. Completed text was saved; audio was not retained.".into(),
                );
                store.save_live(&session)?;
            }
        }
        Ok(store)
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
    pub fn save_live(&self, session: &LiveView) -> Result<()> {
        self.connection.execute("INSERT INTO live_sessions VALUES (?1,?2,?3) ON CONFLICT(id) DO UPDATE SET data=excluded.data,created_at=excluded.created_at", params![session.id, session.created_at, serde_json::to_string(session)?])?;
        Ok(())
    }
    pub fn live_sessions(&self) -> Result<Vec<LiveView>> {
        let mut q = self
            .connection
            .prepare("SELECT data FROM live_sessions ORDER BY created_at DESC")?;
        let rows = q.query_map([], |r| r.get::<_, String>(0))?;
        rows.map(|row| Ok(serde_json::from_str(&row?)?)).collect()
    }
    pub fn delete_live(&self, id: &str) -> Result<()> {
        self.connection
            .execute("DELETE FROM live_sessions WHERE id=?1", [id])?;
        Ok(())
    }
}

pub fn remove_legacy_audio(data: &Path) -> Result<()> {
    let path = data.join("recordings");
    if !path.exists() {
        return Ok(());
    }
    for entry in std::fs::read_dir(&path)? {
        let entry = entry?;
        let file = entry.path();
        if file.extension().and_then(|s| s.to_str()) == Some("audio")
            && file
                .file_stem()
                .and_then(|s| s.to_str())
                .is_some_and(|s| uuid::Uuid::parse_str(s).is_ok())
        {
            std::fs::remove_file(file)?;
        }
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn legacy_settings_preserve_preferences_and_reload_cleanup_choices() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.sqlite");
        {
            let store = Store::open(&path).unwrap();
            store.connection.execute(
                "INSERT INTO settings VALUES (1,?1)",
                [r#"{"microphone":"Studio USB","shortcut":"Alt+Space","shortcutLabel":"Alt + Space"}"#],
            ).unwrap();
            let mut settings = store.settings();
            assert_eq!(settings.microphone.as_deref(), Some("Studio USB"));
            assert_eq!(settings.shortcut, "Alt+Space");
            assert_eq!(settings.shortcut_label.as_deref(), Some("Alt + Space"));
            assert_eq!(settings.cleanup_model, DEFAULT_CLEANUP_MODEL);
            assert_eq!(
                settings.cleanup_reasoning_effort,
                Some(ReasoningEffort::Low)
            );
            assert_eq!(settings.cleanup_engine, CleanupEngine::OpenRouter);
            assert_eq!(settings.cleanup_styling, Styling::SemiFormal);
            settings.cleanup_model = "custom/new-model".into();
            settings.cleanup_reasoning_effort = None;
            settings.cleanup_engine = CleanupEngine::S1Mini;
            settings.cleanup_styling = Styling::SemiCasual;
            store.save_settings(&settings).unwrap();
        }
        let store = Store::open(&path).unwrap();
        let mut settings = store.settings();
        assert_eq!(settings.microphone.as_deref(), Some("Studio USB"));
        assert_eq!(settings.shortcut, "Alt+Space");
        assert_eq!(settings.cleanup_model, "custom/new-model");
        assert_eq!(settings.cleanup_reasoning_effort, None);
        assert_eq!(settings.cleanup_engine, CleanupEngine::S1Mini);
        assert_eq!(settings.cleanup_styling, Styling::SemiCasual);
        settings.cleanup_reasoning_effort = Some(ReasoningEffort::Xhigh);
        store.save_settings(&settings).unwrap();
        drop(store);
        assert_eq!(
            Store::open(&path)
                .unwrap()
                .settings()
                .cleanup_reasoning_effort,
            Some(ReasoningEffort::Xhigh),
        );
    }
    #[test]
    fn interrupted_live_session_preserves_text_and_marks_it_incomplete() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.sqlite");
        {
            let store = Store::open(&path).unwrap();
            let view = LiveView {
                id: "live".into(),
                phase: LivePhase::Listening,
                transcript: "First point.".into(),
                interim: "But".into(),
                summary: "A draft".into(),
                summarizing: true,
                ..Default::default()
            };
            store.save_live(&view).unwrap();
            store.save_live(&view).unwrap();
        }
        let store = Store::open(&path).unwrap();
        let views = store.live_sessions().unwrap();
        assert_eq!(views.len(), 1);
        assert!(views[0].phase == LivePhase::Error);
        assert!(!views[0].summarizing);
        assert_eq!(views[0].transcript, "First point.");
        assert_eq!(views[0].summary, "A draft");
        assert!(views[0].error.as_ref().unwrap().contains("interrupted"));
        assert!(store.list().unwrap().is_empty());
        store.delete_live("live").unwrap();
        assert!(store.live_sessions().unwrap().is_empty());
    }
    #[test]
    fn removes_only_legacy_app_audio_files() {
        let dir = tempfile::tempdir().unwrap();
        let recordings = dir.path().join("recordings");
        std::fs::create_dir(&recordings).unwrap();
        let audio = recordings.join(format!("{}.audio", uuid::Uuid::new_v4()));
        std::fs::write(&audio, [0, 1, 2]).unwrap();
        let other = recordings.join("unrelated.txt");
        std::fs::write(&other, "keep").unwrap();
        remove_legacy_audio(dir.path()).unwrap();
        assert!(!audio.exists());
        assert!(other.exists());
        remove_legacy_audio(dir.path()).unwrap();
    }
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
