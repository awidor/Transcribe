//! Global input is reduced to a chord in memory. Only an activation or an explicitly
//! requested capture is published; ordinary keyboard events never leave this module.
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeSet,
    sync::{mpsc, Arc, Mutex},
    time::{Duration, Instant},
};

#[cfg(target_os = "windows")]
mod windows;
#[cfg(target_os = "windows")]
use windows as native;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
use macos as native;
#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
use linux as native;
#[cfg(target_os = "linux")]
pub use linux::portal_trigger;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Key {
    pub code: u32,
    pub label: String,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Binding {
    pub platform: String,
    pub keys: Vec<Key>,
}
#[derive(Clone, Debug, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Event {
    Activate,
    Capture {
        token: u64,
        keys: Vec<Key>,
        shortcut: Option<String>,
    },
    Cancelled {
        token: u64,
    },
    Error {
        message: String,
    },
}

struct Capture {
    token: u64,
    deadline: Instant,
    keys: Vec<Key>,
    armed: bool,
}
#[derive(Default)]
struct Engine {
    binding: Vec<Vec<u32>>,
    held: BTreeSet<u32>,
    stroke: BTreeSet<u32>,
    swallowed: BTreeSet<u32>,
    releasing: bool,
    invalid: bool,
    capture: Option<Capture>,
    next_token: u64,
    paused: bool,
    portal_inhibit_until: Option<Instant>,
}
impl Engine {
    fn matches(&self) -> bool {
        !self.binding.is_empty()
            && self.stroke.len() == self.binding.len()
            && self
                .binding
                .iter()
                .all(|options| options.iter().any(|k| self.stroke.contains(k)))
    }
    fn reset(&mut self) {
        self.stroke.clear();
        self.releasing = false;
        // Never activate from the tail of a chord held while settings changed.
        self.invalid = !self.held.is_empty();
    }
    fn cancel(&mut self) -> Option<Event> {
        let capture = self.capture.take()?;
        self.portal_inhibit_until = Some(Instant::now() + Duration::from_millis(250));
        self.reset();
        Some(Event::Cancelled {
            token: capture.token,
        })
    }
    fn input(&mut self, key: Key, down: bool, modifier: bool) -> (bool, Vec<Event>) {
        let mut events = Vec::new();
        if self
            .capture
            .as_ref()
            .is_some_and(|c| c.deadline <= Instant::now())
        {
            events.extend(self.cancel());
        }
        let capturing = self.capture.is_some();
        let swallowed = self.swallowed.contains(&key.code);
        // A keyboard-activated button may start capture before its Enter/Space
        // key-up. Drain that initiating chord without treating it as a binding.
        if self.capture.as_ref().is_some_and(|c| !c.armed) {
            if down {
                self.held.insert(key.code);
            } else {
                self.held.remove(&key.code);
                self.swallowed.remove(&key.code);
            }
            if self.held.is_empty() {
                if let Some(capture) = self.capture.as_mut() {
                    capture.armed = true;
                }
                self.reset();
            }
            return (swallowed, events);
        }
        if down {
            if !self.held.insert(key.code) {
                return (capturing || swallowed, events);
            }
            if self.releasing {
                self.invalid = true;
            }
            self.stroke.insert(key.code);
            if let Some(capture) = self.capture.as_mut() {
                capture.keys.push(key.clone());
                events.push(Event::Capture {
                    token: capture.token,
                    keys: capture.keys.clone(),
                    shortcut: None,
                });
            }
        } else {
            if !self.held.remove(&key.code) {
                return (swallowed, events);
            }
            self.swallowed.remove(&key.code);
            self.releasing = true;
        }
        // Modifiers must remain usable for normal typing and other shortcuts.
        let suppress = capturing
            || swallowed
            || (down && !modifier && !self.paused && !self.invalid && self.matches());
        if down && suppress {
            self.swallowed.insert(key.code);
        }
        if self.held.is_empty() {
            if let Some(capture) = self.capture.take() {
                if !self.invalid && !capture.keys.is_empty() {
                    let keys = capture.keys;
                    let binding = Binding {
                        platform: platform().into(),
                        keys: keys.clone(),
                    };
                    events.push(Event::Capture {
                        token: capture.token,
                        keys,
                        shortcut: Some(
                            serde_json::to_string(&binding).expect("serializable binding"),
                        ),
                    });
                } else {
                    events.push(Event::Cancelled {
                        token: capture.token,
                    });
                }
            } else if !self.paused && !self.invalid && self.matches() {
                events.push(Event::Activate);
            }
            self.reset();
        }
        (suppress, events)
    }
}

pub fn platform() -> &'static str {
    #[cfg(target_os = "linux")]
    if crate::insertion::linux::is_wayland() {
        return "wayland";
    }
    std::env::consts::OS
}

pub struct Service {
    engine: Mutex<Engine>,
    started: Mutex<bool>,
    sender: mpsc::Sender<Event>,
    #[cfg(target_os = "windows")]
    listener: Mutex<Option<windows::Listener>>,
}
pub struct Pause<'a>(&'a Service);
impl Drop for Pause<'_> {
    fn drop(&mut self) {
        self.0.paused(false);
    }
}
impl Service {
    pub fn new(handler: impl Fn(Event) + Send + 'static) -> Arc<Self> {
        let (sender, receiver) = mpsc::channel();
        std::thread::spawn(move || {
            for event in receiver {
                handler(event);
            }
        });
        Arc::new(Self {
            engine: Mutex::new(Engine::default()),
            started: Mutex::new(false),
            sender,
            #[cfg(target_os = "windows")]
            listener: Mutex::new(None),
        })
    }
    pub fn start(self: &Arc<Self>) -> Result<()> {
        if platform() == "wayland" {
            return Ok(());
        }
        let mut started = self.started.lock().unwrap();
        if !*started {
            native::start(self.clone())?;
            *started = true;
        }
        Ok(())
    }
    pub fn validate(&self, text: &str) -> Result<Vec<Vec<u32>>> {
        #[cfg(target_os = "linux")]
        if platform() == "wayland" {
            portal_trigger(text)?;
            return Ok(Vec::new());
        }
        if text.starts_with('{') {
            let binding: Binding = serde_json::from_str(text).context("Invalid shortcut")?;
            if binding.platform != platform() {
                bail!("Shortcut belongs to another platform");
            }
            if binding.keys.is_empty() || binding.keys.len() > 512 {
                bail!("Invalid shortcut");
            }
            let keys: BTreeSet<_> = binding.keys.iter().map(|k| k.code).collect();
            if keys.len() != binding.keys.len() {
                bail!("Duplicate shortcut key");
            }
            native::validate(&keys)?;
            Ok(keys.into_iter().map(|k| vec![k]).collect())
        } else {
            let binding = native::legacy(text)?;
            let mut seen = BTreeSet::new();
            for alternatives in &binding {
                for key in alternatives {
                    if !seen.insert(*key) {
                        bail!("Duplicate shortcut key");
                    }
                }
            }
            native::validate(&seen)?;
            Ok(binding)
        }
    }
    pub fn configure(&self, binding: Vec<Vec<u32>>) {
        let mut engine = self.engine.lock().unwrap();
        engine.binding = binding;
        engine.reset();
    }
    pub fn begin_capture(self: &Arc<Self>) -> Result<u64> {
        self.start()?;
        #[cfg(target_os = "windows")]
        windows::prepare_capture(self)?;
        let mut engine = self.engine.lock().unwrap();
        engine.cancel();
        engine.reset();
        engine.next_token += 1;
        let token = engine.next_token;
        engine.capture = Some(Capture {
            token,
            deadline: Instant::now() + Duration::from_secs(15),
            keys: Vec::new(),
            armed: engine.held.is_empty(),
        });
        Ok(token)
    }
    pub fn cancel_capture(&self, token: Option<u64>) {
        let mut engine = self.engine.lock().unwrap();
        if token.is_none()
            || engine
                .capture
                .as_ref()
                .is_some_and(|c| Some(c.token) == token)
        {
            if let Some(event) = engine.cancel() {
                let _ = self.sender.send(event);
            }
        }
    }
    /// Focused-window input is scoped to an explicit capture; it can never
    /// activate a global binding or revive an expired/cancelled recorder.
    pub fn capture_key(&self, token: u64, key: Key, down: bool) -> Result<Vec<Event>> {
        #[cfg(target_os = "windows")]
        {
            native::validate(&BTreeSet::from([key.code]))?;
            let mut engine = self.engine.lock().unwrap();
            if !engine
                .capture
                .as_ref()
                .is_some_and(|capture| capture.token == token)
            {
                return Ok(Vec::new());
            }
            if engine
                .capture
                .as_ref()
                .is_some_and(|capture| capture.deadline <= Instant::now())
            {
                return Ok(engine.cancel().into_iter().collect());
            }
            let modifier = matches!(key.code, 0xA0..=0xA5 | 0x5B..=0x5C);
            let (_, events) = engine.input(key, down, modifier);
            Ok(events)
        }
        #[cfg(not(target_os = "windows"))]
        {
            let _ = (token, key, down);
            bail!("Focused-window capture is unavailable on this platform")
        }
    }
    pub fn paused(&self, paused: bool) {
        let mut engine = self.engine.lock().unwrap();
        engine.paused = paused;
        engine.reset();
    }
    pub fn pause(&self) -> Pause<'_> {
        self.paused(true);
        Pause(self)
    }
    pub fn portal_activate(&self) {
        let engine = self.engine.lock().unwrap();
        if !engine.paused
            && engine.capture.is_none()
            && !engine
                .portal_inhibit_until
                .is_some_and(|until| until > Instant::now())
        {
            let _ = self.sender.send(Event::Activate);
        }
    }
    fn input(&self, code: u32, label: String, down: bool, modifier: bool) -> bool {
        let (suppress, events) =
            self.engine
                .lock()
                .unwrap()
                .input(Key { code, label }, down, modifier);
        for event in events {
            let _ = self.sender.send(event);
        }
        suppress
    }
    fn failed(&self, message: String) {
        *self.started.lock().unwrap() = false;
        let mut engine = self.engine.lock().unwrap();
        if let Some(event) = engine.cancel() {
            let _ = self.sender.send(event);
        }
        engine.held.clear();
        engine.swallowed.clear();
        engine.reset();
        let _ = self.sender.send(Event::Error { message });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn input(e: &mut Engine, key: u32, down: bool) -> Vec<Event> {
        e.input(
            Key {
                code: key,
                label: key.to_string(),
            },
            down,
            key < 10,
        )
        .1
    }
    fn engine(binding: &[u32]) -> Engine {
        Engine {
            binding: binding.iter().map(|k| vec![*k]).collect(),
            ..Default::default()
        }
    }
    fn fires(events: &[Event]) -> bool {
        events.iter().any(|e| matches!(e, Event::Activate))
    }
    #[test]
    fn modifier_tap_fires_once_on_release() {
        let mut e = engine(&[1]);
        assert!(!fires(&input(&mut e, 1, true)));
        assert!(!fires(&input(&mut e, 1, true)));
        assert!(fires(&input(&mut e, 1, false)));
        assert!(!fires(&input(&mut e, 1, false)));
    }
    #[test]
    fn modifier_used_in_another_shortcut_never_fires() {
        let mut e = engine(&[1]);
        for (k, d) in [(1, true), (20, true), (20, false), (1, false)] {
            assert!(!fires(&input(&mut e, k, d)));
        }
    }
    #[test]
    fn arbitrary_chord_and_order_and_sides() {
        let mut e = engine(&[1, 20, 30]);
        for (k, d) in [(30, true), (1, true), (20, true), (1, false), (30, false)] {
            assert!(!fires(&input(&mut e, k, d)));
        }
        assert!(fires(&input(&mut e, 20, false)));
        for (k, d) in [
            (2, true),
            (20, true),
            (30, true),
            (2, false),
            (20, false),
            (30, false),
        ] {
            assert!(!fires(&input(&mut e, k, d)));
        }
    }
    #[test]
    fn sequential_keys_are_not_a_chord_and_extra_keys_invalidate() {
        for sequence in [
            vec![
                (1, true),
                (20, true),
                (20, false),
                (30, true),
                (30, false),
                (1, false),
            ],
            vec![
                (1, true),
                (20, true),
                (30, true),
                (40, true),
                (40, false),
                (30, false),
                (20, false),
                (1, false),
            ],
        ] {
            let mut e = engine(&[1, 20, 30]);
            for (k, d) in sequence {
                assert!(!fires(&input(&mut e, k, d)));
            }
        }
    }
    #[test]
    fn capture_does_not_activate_and_preserves_entire_chord() {
        let mut e = engine(&[1]);
        e.capture = Some(Capture {
            token: 1,
            deadline: Instant::now() + Duration::from_secs(5),
            keys: Vec::new(),
            armed: true,
        });
        input(&mut e, 1, true);
        input(&mut e, 2, true);
        input(&mut e, 1, false);
        let events = input(&mut e, 2, false);
        assert!(!fires(&events));
        assert!(events.iter().any(
            |event| matches!(event,Event::Capture { keys, shortcut: Some(_), .. } if keys.len()==2)
        ));
    }
    #[test]
    fn reconfiguration_or_pause_cannot_fire_a_held_key() {
        let mut e = engine(&[1]);
        input(&mut e, 1, true);
        e.reset();
        assert!(!fires(&input(&mut e, 1, false)));
        e.paused = true;
        input(&mut e, 1, true);
        assert!(!fires(&input(&mut e, 1, false)));
    }
    #[test]
    fn cancelled_capture_releases_swallowed_keys_without_activation() {
        let mut e = engine(&[1]);
        e.capture = Some(Capture {
            token: 1,
            deadline: Instant::now() + Duration::from_secs(5),
            keys: Vec::new(),
            armed: true,
        });
        input(&mut e, 1, true);
        e.cancel();
        let (suppress, events) = e.input(
            Key {
                code: 1,
                label: "Ctrl".into(),
            },
            false,
            true,
        );
        assert!(suppress);
        assert!(!fires(&events));
        assert!(e.swallowed.is_empty());
    }
    #[test]
    fn stale_capture_cancellation_cannot_cancel_a_new_capture() {
        let service = Service::new(|_| {});
        {
            let mut e = service.engine.lock().unwrap();
            e.capture = Some(Capture {
                token: 2,
                deadline: Instant::now() + Duration::from_secs(5),
                keys: Vec::new(),
                armed: true,
            });
        }
        service.cancel_capture(Some(1));
        assert_eq!(
            service
                .engine
                .lock()
                .unwrap()
                .capture
                .as_ref()
                .unwrap()
                .token,
            2
        );
        service.cancel_capture(Some(2));
        assert!(service.engine.lock().unwrap().capture.is_none());
    }
    #[test]
    fn capture_drains_the_key_used_to_open_the_recorder() {
        let mut e = engine(&[1]);
        input(&mut e, 20, true);
        e.capture = Some(Capture {
            token: 1,
            deadline: Instant::now() + Duration::from_secs(5),
            keys: Vec::new(),
            armed: false,
        });
        e.reset();
        assert!(input(&mut e, 20, false).is_empty());
        assert!(e.capture.as_ref().unwrap().armed);
        input(&mut e, 1, true);
        let events = input(&mut e, 1, false);
        assert!(events.iter().any(|event| matches!(event, Event::Capture { keys, shortcut:Some(_),.. } if keys.len()==1 && keys[0].code==1)));
        assert!(!fires(&events));
    }
    #[test]
    fn capture_expiry_cannot_activate_the_binding() {
        let mut e = engine(&[1]);
        input(&mut e, 1, true);
        e.capture = Some(Capture {
            token: 1,
            deadline: Instant::now() - Duration::from_secs(1),
            keys: Vec::new(),
            armed: true,
        });
        let events = input(&mut e, 1, false);
        assert!(events
            .iter()
            .any(|e| matches!(e, Event::Cancelled { token: 1 })));
        assert!(!fires(&events));
    }
    #[test]
    fn synthetic_paste_pause_is_restored_by_drop() {
        let service = Service::new(|_| {});
        {
            let _pause = service.pause();
            assert!(service.engine.lock().unwrap().paused);
        }
        assert!(!service.engine.lock().unwrap().paused);
    }
    #[cfg(target_os = "windows")]
    #[test]
    fn focused_capture_handles_duplicates_and_stale_tokens_without_activation() {
        let service = Service::new(|_| {});
        let key = Key {
            code: 162,
            label: "Left Ctrl".into(),
        };
        service.configure(vec![vec![162]]);
        {
            service.engine.lock().unwrap().capture = Some(Capture {
                token: 7,
                deadline: Instant::now() + Duration::from_secs(5),
                keys: Vec::new(),
                armed: true,
            });
        }
        assert!(service
            .capture_key(6, key.clone(), true)
            .unwrap()
            .is_empty());
        let down = service.capture_key(7, key.clone(), true).unwrap();
        assert!(matches!(&down[..], [Event::Capture { shortcut: None, .. }]));
        assert!(service
            .capture_key(7, key.clone(), true)
            .unwrap()
            .is_empty());
        let up = service.capture_key(7, key.clone(), false).unwrap();
        assert!(
            matches!(&up[..], [Event::Capture { shortcut: Some(_), keys, .. }] if keys.len() == 1)
        );
        assert!(service.capture_key(7, key, false).unwrap().is_empty());
        assert!(!fires(&up));
        assert!(service.engine.lock().unwrap().held.is_empty());
    }
    #[cfg(target_os = "windows")]
    #[test]
    fn expired_focused_capture_cannot_arm_a_global_shortcut() {
        let service = Service::new(|_| {});
        service.configure(vec![vec![162]]);
        service.engine.lock().unwrap().capture = Some(Capture {
            token: 7,
            deadline: Instant::now() - Duration::from_secs(1),
            keys: Vec::new(),
            armed: true,
        });
        let events = service
            .capture_key(
                7,
                Key {
                    code: 162,
                    label: "Left Ctrl".into(),
                },
                true,
            )
            .unwrap();
        assert!(matches!(&events[..], [Event::Cancelled { token: 7 }]));
        let engine = service.engine.lock().unwrap();
        assert!(engine.held.is_empty());
        assert!(engine.stroke.is_empty());
    }
    #[cfg(target_os = "windows")]
    #[test]
    fn preserves_legacy_defaults_and_rejects_reserved_or_foreign_bindings() {
        let service = Service::new(|_| {});
        assert_eq!(
            service.validate("CommandOrControl+Shift+Space").unwrap(),
            vec![vec![162, 163], vec![160, 161], vec![32]]
        );
        assert!(service.validate("Control+Alt+Delete").is_err());
        assert!(service.validate("Super+L").is_err());
        assert!(service.validate("Ctrl+Ctrl").is_err());
        assert!(service
            .validate(r#"{"platform":"macos","keys":[{"code":59,"label":"Ctrl"}]}"#)
            .is_err());
        assert!(service
            .validate(r#"{"platform":"windows","keys":[]}"#)
            .is_err());
        assert_eq!(service.validate("NumpadEnter").unwrap(), vec![vec![269]]);
    }
    #[test]
    #[ignore = "Starts the native listener; requires a desktop session"]
    fn native_listener_starts_without_injecting_input() {
        assert_ne!(platform(), "wayland");
        let service = Service::new(|_| {});
        service.start().unwrap();
        assert!(*service.started.lock().unwrap());
    }
}
