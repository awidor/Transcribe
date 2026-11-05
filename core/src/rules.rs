//! The rules each control follows, decided once so the windows only draw them.
use crate::{
    azure::Words,
    hotkey,
    provider::{CleanupEngine, CleanupModel, ReasoningEffort, TranscriptionModel},
    s1::Styling,
    session::{Phase, Session, UpdateView},
    storage::{Entry, Settings},
};
use std::collections::BTreeSet;

/// A session holds the microphone or the pipeline in these phases.
pub fn busy(phase: Phase) -> bool {
    matches!(
        phase,
        Phase::Starting
            | Phase::Recording
            | Phase::Transcribing
            | Phase::Cleaning
            | Phase::Inserting
    )
}

/// The phases that disable recording controls; recording itself stays stoppable.
pub fn working(phase: Phase) -> bool {
    busy(phase) && phase != Phase::Recording
}

/// Text appears as soon as insertion starts; the rest of that phase only keeps
/// the clipboard available to the destination, so it already counts as done.
pub fn finished(session: &Session) -> bool {
    matches!(session.phase, Phase::Inserting | Phase::Done)
}

pub fn processing(session: &Session) -> bool {
    matches!(session.phase, Phase::Transcribing | Phase::Cleaning) || finished(session)
}

/// "Starting", "Transcribing", "Cleaning up", "Retrying" or "Done"; nothing
/// while idle or recording.
pub fn status_label(session: &Session) -> Option<&'static str> {
    if session.phase == Phase::Starting {
        return Some("Starting");
    }
    if finished(session) {
        return Some("Done");
    }
    if !matches!(session.phase, Phase::Transcribing | Phase::Cleaning) {
        return None;
    }
    if session.retrying {
        return Some("Retrying");
    }
    Some(if session.phase == Phase::Transcribing {
        "Transcribing"
    } else {
        "Cleaning up"
    })
}

/// Words as spoken: confirmed text, then the dimmed guess that follows it.
pub fn spoken(words: &Words) -> Option<Words> {
    let text = words.text.trim_start().to_owned();
    let interim = if text.is_empty() {
        words.interim.trim_start().to_owned()
    } else {
        words.interim.clone()
    };
    if text.is_empty() && interim.trim().is_empty() {
        return None;
    }
    Some(Words { text, interim })
}

/// What the widget shows: the session pill, an error, or the unpasted transcript card.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WidgetMode {
    Session,
    Error,
    Card,
}

pub fn widget_mode(session: &Session) -> WidgetMode {
    if session.phase != Phase::Error {
        WidgetMode::Session
    } else if session.transcript.is_some() {
        WidgetMode::Card
    } else {
        WidgetMode::Error
    }
}

/// "mm:ss" since `started_at`, both in milliseconds since the epoch.
pub fn clock(started_at: Option<i64>, now: i64) -> String {
    let seconds = started_at.map_or(0, |start| ((now - start) / 1000).max(0));
    format!("{:02}:{:02}", seconds / 60, seconds % 60)
}

/// "1.2 GB" or "350 MB".
pub fn size_label(bytes: u64) -> String {
    if bytes >= 1_000_000_000 {
        format!("{:.1} GB", bytes as f64 / 1e9)
    } else {
        format!("{} MB", (bytes as f64 / 1e6 + 0.5).floor())
    }
}

pub fn word_count(text: &str) -> usize {
    text.split_whitespace().count()
}

/// Case-insensitive search through transcripts.
pub fn matches(text: &str, search: &str) -> bool {
    text.to_lowercase().contains(&search.to_lowercase())
}

/// The text a history row shows.
pub fn entry_preview(entry: &Entry, session: &Session) -> String {
    if !entry.text.is_empty() {
        return entry.text.clone();
    }
    if let Some(error) = entry.error.as_ref().filter(|error| !error.is_empty()) {
        return error.clone();
    }
    if entry.status == "transcribing" {
        if let Some(status) = status_label(session) {
            return status.into();
        }
    }
    "Processing".into()
}

/// A transcript that was saved but could not be pasted.
pub fn not_pasted(entry: &Entry) -> bool {
    entry.status == "saved" && entry.error.as_ref().is_some_and(|error| !error.is_empty())
}

/// Moves `value` toward `goal` at `rate` per second, independent of frame rate.
pub fn toward(value: f64, goal: f64, rate: f64, dt: f64) -> f64 {
    value + (goal - value) * (1. - (-rate * dt).exp())
}

/// Decibels a sound has to clear the room's noise by before it counts.
const GATE: f64 = 8.;
/// Decibels above the noise that full height takes, at the least.
const RANGE: f64 = 24.;
/// Seconds of sound the room's noise is judged from.
const WINDOW: f64 = 2.;

/// Loudness from 0 to 1, judged against this microphone's own room: quiet and
/// loud microphones both reach the top, while background noise stays near
/// zero. Readings of zero (before the microphone reports) are ignored.
#[derive(Default)]
pub struct Loudness {
    recent: Vec<(f64, f64)>,
    now: f64,
    floor: Option<f64>,
    peak: f64,
    loud: f64,
}

impl Loudness {
    pub fn update(&mut self, level: f64, dt: f64) -> f64 {
        self.now += dt;
        let mut target = 0.;
        if level > 0. {
            let db = 20. * level.max(1e-6).log10();
            self.recent.push((self.now, db));
            let now = self.now;
            self.recent.retain(|(at, _)| *at >= now - WINDOW);
            // A quiet moment of the last two seconds is the room's noise; pauses
            // between words keep it there even while someone talks.
            let mut sorted: Vec<f64> = self.recent.iter().map(|(_, db)| *db).collect();
            sorted.sort_by(f64::total_cmp);
            let quiet = sorted[((sorted.len() - 1) as f64 * 0.2).floor() as usize];
            let floor = match self.floor {
                Some(floor) => floor,
                None => {
                    self.peak = db + RANGE;
                    db
                }
            };
            let floor = toward(floor, quiet, 4., dt);
            self.floor = Some(floor);
            let ceiling = db.max(floor + RANGE);
            self.peak = toward(
                self.peak,
                ceiling,
                if ceiling > self.peak { 14. } else { 0.8 },
                dt,
            );
            let clear = (db - floor - GATE) / (self.peak - floor - GATE);
            target = clear.clamp(0., 1.).powf(0.7);
        }
        self.loud = toward(
            self.loud,
            target,
            if target > self.loud { 12. } else { 8. },
            dt,
        );
        self.loud
    }
}

pub const FOCUS_SETTINGS: &str = "Focus Settings to record a shortcut";

/// Labels for the settings controls.
pub struct Choice {
    pub label: &'static str,
    pub seconds: Option<u64>,
}

pub const UNLOAD_CHOICES: &[Choice] = &[
    Choice {
        label: "Never",
        seconds: None,
    },
    Choice {
        label: "Immediately",
        seconds: Some(0),
    },
    Choice {
        label: "After 2 minutes",
        seconds: Some(120),
    },
    Choice {
        label: "After 5 minutes",
        seconds: Some(300),
    },
    Choice {
        label: "After 10 minutes",
        seconds: Some(600),
    },
    Choice {
        label: "After 15 minutes",
        seconds: Some(900),
    },
    Choice {
        label: "After 1 hour",
        seconds: Some(3600),
    },
];

pub const REASONING_EFFORTS: [ReasoningEffort; 7] = [
    ReasoningEffort::None,
    ReasoningEffort::Minimal,
    ReasoningEffort::Low,
    ReasoningEffort::Medium,
    ReasoningEffort::High,
    ReasoningEffort::Xhigh,
    ReasoningEffort::Max,
];

pub fn reasoning_label(effort: ReasoningEffort) -> &'static str {
    match effort {
        ReasoningEffort::None => "None",
        ReasoningEffort::Minimal => "Minimal",
        ReasoningEffort::Low => "Low",
        ReasoningEffort::Medium => "Medium",
        ReasoningEffort::High => "High",
        ReasoningEffort::Xhigh => "Extra high",
        ReasoningEffort::Max => "Max",
    }
}

pub const STYLINGS: [Styling; 4] = [
    Styling::Casual,
    Styling::SemiCasual,
    Styling::SemiFormal,
    Styling::Formal,
];

pub fn styling_label(styling: Styling) -> &'static str {
    match styling {
        Styling::Casual => "Casual",
        Styling::SemiCasual => "Semi-casual",
        Styling::SemiFormal => "Semi-formal",
        Styling::Formal => "Formal",
    }
}

/// The thinking levels the chosen model offers; every level for a model
/// outside the catalog.
pub fn available_efforts(models: &[CleanupModel], model: &str) -> Vec<ReasoningEffort> {
    models
        .iter()
        .find(|candidate| candidate.id == model)
        .map_or_else(
            || REASONING_EFFORTS.to_vec(),
            |found| found.reasoning_efforts.clone(),
        )
}

/// The saved level when the model offers it, otherwise the model's default.
pub fn effective_effort(settings: &Settings, models: &[CleanupModel]) -> Option<ReasoningEffort> {
    let effort = settings.cleanup_reasoning_effort?;
    available_efforts(models, &settings.cleanup_model)
        .contains(&effort)
        .then_some(effort)
}

/// The Streaming switch, the Transcription choice and the Cleanup choice.
/// MAI-Transcribe-2's clean style needs the whole recording and
/// MAI-Transcribe-2 itself: turning streaming off or choosing MAI-Transcribe-2
/// picks it, choosing it turns streaming off and picks MAI-Transcribe-2, and
/// turning streaming on or choosing Voxtral returns to the cleanup chosen before.
pub struct StreamingChoice {
    previous: CleanupEngine,
}

impl Default for StreamingChoice {
    fn default() -> Self {
        Self {
            previous: CleanupEngine::OpenRouter,
        }
    }
}

impl StreamingChoice {
    pub fn streaming(&mut self, settings: &mut Settings, on: bool) {
        settings.streaming = on;
        if !on {
            self.model(settings, settings.transcription_model);
        } else if settings.cleanup_engine == CleanupEngine::MaiTranscribe {
            settings.cleanup_engine = self.previous;
        }
    }

    pub fn model(&mut self, settings: &mut Settings, model: TranscriptionModel) {
        settings.transcription_model = model;
        if model == TranscriptionModel::MaiTranscribe {
            self.cleanup(settings, CleanupEngine::MaiTranscribe);
        } else if settings.cleanup_engine == CleanupEngine::MaiTranscribe {
            settings.cleanup_engine = self.previous;
        }
    }

    /// Windows also call this with the saved cleanup when they load settings.
    pub fn cleanup(&mut self, settings: &mut Settings, engine: CleanupEngine) {
        settings.cleanup_engine = engine;
        if engine == CleanupEngine::MaiTranscribe {
            settings.streaming = false;
            settings.transcription_model = TranscriptionModel::MaiTranscribe;
        } else {
            self.previous = engine;
        }
    }
}

/// Replaces the first match of `pattern`, a key name followed by "Left" or
/// "Right", with that side and `name`.
fn sided(label: &str, pattern: &str, name: &str) -> Option<String> {
    let at = label.find(pattern)?;
    let rest = &label[at + pattern.len()..];
    let side = ["Left", "Right"]
        .into_iter()
        .find(|side| rest.starts_with(side))?;
    Some(format!(
        "{}{side} {name}{}",
        &label[..at],
        &rest[side.len()..]
    ))
}

/// Labels saved by the old Wayland recorder are DOM key codes such as "AltRight".
fn key_label(label: &str, platform: &str) -> String {
    if platform != "wayland" {
        return label.into();
    }
    let mut text = label.to_owned();
    for (pattern, name) in [
        ("Control", "Ctrl"),
        ("Shift", "Shift"),
        ("Alt", "Alt"),
        ("Meta", "Super"),
    ] {
        if let Some(replaced) = sided(&text, pattern, name) {
            text = replaced;
        }
    }
    for prefix in ["Key", "Digit"] {
        if let Some(rest) = text.strip_prefix(prefix) {
            text = rest.to_owned();
        }
    }
    // A space between a lowercase letter and the capital after it.
    let mut spaced = String::new();
    let mut previous: Option<char> = None;
    for c in text.chars() {
        if previous.is_some_and(|p| p.is_ascii_lowercase()) && c.is_ascii_uppercase() {
            spaced.push(' ');
        }
        spaced.push(c);
        previous = Some(c);
    }
    spaced
}

/// The keys a saved shortcut shows: a native binding's labels, or a legacy
/// accelerator split at "+".
pub fn shortcut_labels(shortcut: &str) -> Vec<String> {
    if let Ok(binding) = serde_json::from_str::<hotkey::Binding>(shortcut) {
        return binding
            .keys
            .iter()
            .map(|key| key_label(&key.label, &binding.platform))
            .collect();
    }
    shortcut
        .replacen("CommandOrControl", "Ctrl", 1)
        .split('+')
        .map(str::to_owned)
        .collect()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecorderMode {
    Idle,
    Starting,
    Listening,
}

#[derive(Debug, Default, PartialEq)]
pub struct RecorderResult {
    pub stopped: bool,
    pub shortcut: Option<String>,
    /// The capture to end once stopped.
    pub ended: Option<u64>,
}

/// Records a shortcut from capture events, which can arrive before the
/// capture's token does.
pub struct ShortcutRecorder {
    mode: RecorderMode,
    token: Option<u64>,
    platform: String,
    buffered: Vec<hotkey::Event>,
    keys: Vec<String>,
}

impl Default for ShortcutRecorder {
    fn default() -> Self {
        Self {
            mode: RecorderMode::Idle,
            token: None,
            platform: String::new(),
            buffered: Vec::new(),
            keys: Vec::new(),
        }
    }
}

impl ShortcutRecorder {
    pub fn mode(&self) -> RecorderMode {
        self.mode
    }
    pub fn token(&self) -> Option<u64> {
        self.token
    }
    pub fn keys(&self) -> &[String] {
        &self.keys
    }

    pub fn starting(&mut self) {
        *self = Self {
            mode: RecorderMode::Starting,
            ..Self::default()
        };
    }

    /// Returns what the events buffered before the token amounted to.
    pub fn begin(&mut self, token: u64, platform: &str) -> RecorderResult {
        if self.mode != RecorderMode::Starting {
            return RecorderResult {
                stopped: true,
                shortcut: None,
                ended: Some(token),
            };
        }
        self.token = Some(token);
        self.platform = platform.into();
        self.mode = RecorderMode::Listening;
        for event in std::mem::take(&mut self.buffered) {
            let result = self.receive(event);
            if result.stopped {
                return result;
            }
        }
        RecorderResult::default()
    }

    pub fn receive(&mut self, event: hotkey::Event) -> RecorderResult {
        if self.mode == RecorderMode::Idle {
            return RecorderResult::default();
        }
        let Some(token) = self.token else {
            self.buffered.push(event);
            return RecorderResult::default();
        };
        match event {
            hotkey::Event::Cancelled { token: cancelled } if cancelled == token => RecorderResult {
                stopped: true,
                shortcut: None,
                ended: self.stop(),
            },
            hotkey::Event::Capture {
                token: captured,
                keys,
                shortcut,
            } if captured == token => {
                self.keys = keys
                    .iter()
                    .map(|key| key_label(&key.label, &self.platform))
                    .collect();
                match shortcut {
                    Some(shortcut) => RecorderResult {
                        stopped: true,
                        shortcut: Some(shortcut),
                        ended: self.stop(),
                    },
                    None => RecorderResult::default(),
                }
            }
            _ => RecorderResult::default(),
        }
    }

    /// Returns the capture to end, if it had begun.
    pub fn stop(&mut self) -> Option<u64> {
        self.mode = RecorderMode::Idle;
        self.buffered.clear();
        self.token.take()
    }
}

/// Windows withholds input from the global shortcut listener while
/// Transcribe's window is in front, so the window reports its own keys. Like
/// the listener, the key completing the shortcut never reaches the window,
/// and an Alt in the shortcut must not open the window menu, which would take
/// the next keys.
#[derive(Default)]
pub struct WindowShortcut {
    held: BTreeSet<u32>,
    swallowed: BTreeSet<u32>,
}

impl WindowShortcut {
    /// True when the window should not handle the key.
    pub fn key(&mut self, code: u32, down: bool, binding: &[Vec<u32>]) -> bool {
        const MODIFIERS: [u32; 8] = [160, 161, 162, 163, 164, 165, 91, 92];
        if down && self.held.insert(code) {
            let matches = binding.len() == self.held.len()
                && binding
                    .iter()
                    .all(|options| options.iter().any(|key| self.held.contains(key)));
            if !MODIFIERS.contains(&code) && matches {
                self.swallowed.insert(code);
            }
        }
        if !down {
            self.held.remove(&code);
        }
        let alt =
            (code == 164 || code == 165) && binding.iter().any(|options| options.contains(&code));
        let suppress = self.swallowed.contains(&code) || alt;
        if !down {
            self.swallowed.remove(&code);
        }
        suppress
    }

    pub fn blur(&mut self) {
        self.held.clear();
        self.swallowed.clear();
    }
}

/// Update status text and the button it offers.
pub fn update_status(update: &UpdateView) -> Option<String> {
    match update.phase {
        "available" => Some(format!(
            "Version {} available",
            update.version.as_deref().unwrap_or_default()
        )),
        "checking" => Some("Checking".into()),
        "current" => Some("Up to date".into()),
        "installing" => Some("Installing".into()),
        "error" => Some("Update failed".into()),
        _ => None,
    }
}

pub fn update_action(update: &UpdateView) -> &'static str {
    if matches!(update.phase, "available" | "installing") {
        "Install and restart"
    } else {
        "Check for updates"
    }
}

pub fn update_busy(update: &UpdateView) -> bool {
    matches!(update.phase, "checking" | "installing")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mai_transcribe_cleanup_ties_streaming_and_transcription_together() {
        let mut settings = Settings::default();
        let mut choice = StreamingChoice::default();
        choice.cleanup(&mut settings, CleanupEngine::S1Mini);
        choice.streaming(&mut settings, false);
        assert_eq!(settings.cleanup_engine, CleanupEngine::MaiTranscribe);
        assert_eq!(
            settings.transcription_model,
            TranscriptionModel::MaiTranscribe
        );
        choice.model(&mut settings, TranscriptionModel::Voxtral);
        assert_eq!(settings.cleanup_engine, CleanupEngine::S1Mini);
        choice.cleanup(&mut settings, CleanupEngine::MaiTranscribe);
        assert!(!settings.streaming);
        assert_eq!(
            settings.transcription_model,
            TranscriptionModel::MaiTranscribe
        );
        choice.streaming(&mut settings, true);
        assert_eq!(settings.cleanup_engine, CleanupEngine::S1Mini);
    }

    #[test]
    fn loudness_fills_the_range_for_quiet_microphones_but_ignores_the_room() {
        let decibels = |db: f64| 10f64.powf(db / 20.);
        // Deterministic ±3 dB jitter, like a room's steady noise.
        let jitter = |frame: i32| (frame as f64 * 12.9898).sin() * 3.;
        let highest = |meter: &mut Loudness, base: f64, seconds: i32| {
            (0..seconds * 60)
                .map(|frame| {
                    (
                        frame,
                        meter.update(decibels(base + jitter(frame)), 1. / 60.),
                    )
                })
                .filter(|(frame, _)| *frame >= 60)
                .fold(0f64, |high, (_, reading)| high.max(reading))
        };
        let mut quiet = Loudness::default();
        highest(&mut quiet, -80., 2);
        let voice = (0..24).fold(0., |_, _| quiet.update(decibels(-58.), 1. / 60.));
        assert!(voice > 0.7, "{voice}");
        assert!(highest(&mut Loudness::default(), -65., 4) < 0.05);
    }

    #[test]
    fn recorder_replays_events_that_beat_the_token() {
        let mut recorder = ShortcutRecorder::default();
        recorder.starting();
        let key = hotkey::Key {
            code: 29,
            label: "ControlLeft".into(),
        };
        recorder.receive(hotkey::Event::Capture {
            token: 7,
            keys: vec![key],
            shortcut: Some("{}".into()),
        });
        let result = recorder.begin(7, "wayland");
        assert_eq!(result.shortcut.as_deref(), Some("{}"));
        assert_eq!(result.ended, Some(7));
        assert_eq!(recorder.keys(), ["Left Ctrl"]);
        assert_eq!(recorder.mode(), RecorderMode::Idle);
    }

    #[test]
    fn window_swallows_the_completing_key_and_a_bound_alt() {
        let binding = vec![vec![162, 163], vec![164], vec![32]];
        let mut window = WindowShortcut::default();
        assert!(!window.key(162, true, &binding));
        assert!(window.key(164, true, &binding));
        assert!(window.key(32, true, &binding));
        assert!(window.key(32, false, &binding));
        assert!(!window.key(162, false, &binding));
    }

    #[test]
    fn legacy_and_wayland_shortcuts_read_as_keys() {
        assert_eq!(
            shortcut_labels("CommandOrControl+Shift+Space"),
            ["Ctrl", "Shift", "Space"]
        );
        assert_eq!(
            shortcut_labels(
                r#"{"platform":"wayland","keys":[{"code":1,"label":"AltRight"},{"code":2,"label":"KeyK"},{"code":3,"label":"PageDown"}]}"#
            ),
            ["Right Alt", "K", "Page Down"]
        );
    }
}
