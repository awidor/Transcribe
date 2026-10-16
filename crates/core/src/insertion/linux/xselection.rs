//! X11 CLIPBOARD and PRIMARY, which GNOME shares with Wayland apps. GNOME
//! offers no data-control protocol, so they are its only selection access.
use anyhow::{anyhow, ensure, Result};
use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, Condvar, Mutex, OnceLock},
    time::{Duration, Instant},
};
use x11rb::{
    connection::Connection,
    protocol::{
        xproto::{
            Atom, AtomEnum, ConnectionExt as _, CreateWindowAux, EventMask, PropMode, Property,
            SelectionNotifyEvent, SelectionRequestEvent, Window, WindowClass,
            SELECTION_NOTIFY_EVENT,
        },
        Event,
    },
    rust_connection::RustConnection,
    wrapper::ConnectionExt as _,
    CURRENT_TIME, NONE,
};

const LIMIT: usize = 64 * 1024 * 1024;

#[derive(Clone, Copy)]
pub(super) enum Selection {
    Clipboard,
    Primary,
}

struct Offer {
    formats: Vec<(Atom, Vec<u8>)>,
    // GNOME reads TARGETS when it takes over an X11 selection for Wayland apps.
    adopted: bool,
}

struct Owner {
    conn: RustConnection,
    window: Window,
    targets: Atom,
    offers: Mutex<HashMap<Atom, Offer>>,
    adopted: Condvar,
}

pub(super) struct Selections {
    owner: Arc<Owner>,
    conn: RustConnection,
    window: Window,
    property: Atom,
    incr: Atom,
    targets: Atom,
    clipboard: Atom,
}

static SELECTIONS: OnceLock<Mutex<Option<Selections>>> = OnceLock::new();

pub(super) fn with<T>(f: impl FnOnce(&Selections) -> Result<T>) -> Result<T> {
    let mut guard = SELECTIONS
        .get_or_init(Mutex::default)
        .lock()
        .map_err(|_| anyhow!("Clipboard unavailable"))?;
    if guard.is_none() {
        *guard = Some(Selections::new()?);
    }
    f(guard.as_ref().unwrap())
}

fn atom(conn: &RustConnection, name: &str) -> Result<Atom> {
    Ok(conn.intern_atom(false, name.as_bytes())?.reply()?.atom)
}

fn window(conn: &RustConnection, screen: usize, events: EventMask) -> Result<Window> {
    let window = conn.generate_id()?;
    let root = conn.setup().roots[screen].root;
    let aux = CreateWindowAux::new().event_mask(events);
    conn.create_window(
        0,
        window,
        root,
        0,
        0,
        1,
        1,
        0,
        WindowClass::INPUT_ONLY,
        0,
        &aux,
    )?
    .check()?;
    Ok(window)
}

impl Owner {
    fn serve(&self) {
        while let Ok(event) = self.conn.wait_for_event() {
            match event {
                Event::SelectionRequest(request) => {
                    let _ = self.answer(request);
                }
                // A clear can arrive after this window took the selection back.
                Event::SelectionClear(clear) => {
                    let owner = self.conn.get_selection_owner(clear.selection);
                    if owner.ok().and_then(|o| o.reply().ok()).map(|o| o.owner) != Some(self.window)
                    {
                        self.offers.lock().unwrap().remove(&clear.selection);
                    }
                }
                _ => (),
            }
        }
    }

    fn answer(&self, request: SelectionRequestEvent) -> Result<()> {
        // Obsolete clients name no property and expect the target's.
        let property = if request.property == NONE {
            request.target
        } else {
            request.property
        };
        let mut offers = self.offers.lock().unwrap();
        let sent = match offers.get_mut(&request.selection) {
            Some(offer) if request.target == self.targets => {
                let mut atoms = vec![self.targets];
                atoms.extend(offer.formats.iter().map(|(format, _)| *format));
                self.conn.change_property32(
                    PropMode::REPLACE,
                    request.requestor,
                    property,
                    AtomEnum::ATOM,
                    &atoms,
                )?;
                offer.adopted = true;
                self.adopted.notify_all();
                true
            }
            Some(offer) => match offer.formats.iter().find(|(f, _)| *f == request.target) {
                Some((format, bytes)) => {
                    self.conn.change_property8(
                        PropMode::REPLACE,
                        request.requestor,
                        property,
                        *format,
                        bytes,
                    )?;
                    true
                }
                None => false,
            },
            None => false,
        };
        drop(offers);
        self.conn.send_event(
            false,
            request.requestor,
            EventMask::NO_EVENT,
            SelectionNotifyEvent {
                response_type: SELECTION_NOTIFY_EVENT,
                sequence: 0,
                time: request.time,
                requestor: request.requestor,
                selection: request.selection,
                target: request.target,
                property: if sent { property } else { NONE },
            },
        )?;
        self.conn.flush()?;
        Ok(())
    }
}

impl Selections {
    fn new() -> Result<Self> {
        let (owner_conn, owner_screen) = x11rb::connect(None)?;
        let owner = Arc::new(Owner {
            window: window(&owner_conn, owner_screen, EventMask::NO_EVENT)?,
            targets: atom(&owner_conn, "TARGETS")?,
            conn: owner_conn,
            offers: Mutex::default(),
            adopted: Condvar::new(),
        });
        let serving = owner.clone();
        std::thread::Builder::new()
            .name("x11-selections".into())
            .spawn(move || serving.serve())?;
        let (conn, screen) = x11rb::connect(None)?;
        Ok(Self {
            owner,
            window: window(&conn, screen, EventMask::PROPERTY_CHANGE)?,
            property: atom(&conn, "TRANSCRIBE_SELECTION")?,
            incr: atom(&conn, "INCR")?,
            targets: atom(&conn, "TARGETS")?,
            clipboard: atom(&conn, "CLIPBOARD")?,
            conn,
        })
    }

    fn atom(&self, selection: Selection) -> Atom {
        match selection {
            Selection::Clipboard => self.clipboard,
            Selection::Primary => AtomEnum::PRIMARY.into(),
        }
    }

    fn event(&self, deadline: Instant) -> Result<Event> {
        loop {
            if let Some(event) = self.conn.poll_for_event()? {
                return Ok(event);
            }
            ensure!(Instant::now() < deadline, "Clipboard read timed out");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn convert(&self, selection: Atom, target: Atom) -> Result<Vec<u8>> {
        // Drop replies to earlier reads that timed out.
        while self.conn.poll_for_event()?.is_some() {}
        self.conn
            .convert_selection(self.window, selection, target, self.property, CURRENT_TIME)?;
        self.conn.flush()?;
        let mut deadline = Instant::now() + Duration::from_secs(2);
        let notify = loop {
            if let Event::SelectionNotify(notify) = self.event(deadline)? {
                if notify.selection == selection && notify.target == target {
                    break notify;
                }
            }
        };
        ensure!(notify.property != NONE, "Clipboard format unavailable");
        let reply = self
            .conn
            .get_property(true, self.window, self.property, AtomEnum::ANY, 0, u32::MAX)?
            .reply()?;
        if reply.type_ != self.incr {
            return Ok(reply.value);
        }
        // Large data arrives in chunks, each after the previous one is deleted.
        let mut data = Vec::new();
        loop {
            match self.event(deadline)? {
                Event::PropertyNotify(notify)
                    if notify.atom == self.property && notify.state == Property::NEW_VALUE =>
                {
                    let chunk = self
                        .conn
                        .get_property(true, self.window, self.property, AtomEnum::ANY, 0, u32::MAX)?
                        .reply()?;
                    if chunk.value.is_empty() {
                        return Ok(data);
                    }
                    data.extend(chunk.value);
                    ensure!(data.len() <= LIMIT, "Clipboard too large");
                    deadline = Instant::now() + Duration::from_secs(2);
                }
                _ => (),
            }
        }
    }

    pub(super) fn formats(&self, selection: Selection) -> Result<HashSet<String>> {
        let selection = self.atom(selection);
        if self.conn.get_selection_owner(selection)?.reply()?.owner == NONE {
            return Ok(HashSet::new());
        }
        let mut formats = HashSet::new();
        for target in self.convert(selection, self.targets)?.chunks_exact(4) {
            let target = u32::from_ne_bytes(target.try_into().unwrap());
            let name = self.conn.get_atom_name(target)?.reply()?.name;
            let name = String::from_utf8_lossy(&name).into_owned();
            if !["TARGETS", "MULTIPLE", "TIMESTAMP", "SAVE_TARGETS"].contains(&name.as_str()) {
                formats.insert(name);
            }
        }
        Ok(formats)
    }

    pub(super) fn read(&self, selection: Selection, format: &str) -> Result<Vec<u8>> {
        self.convert(self.atom(selection), atom(&self.conn, format)?)
    }

    /// Offers `data` on the selection, or releases it when empty, and waits
    /// until GNOME has taken the new selection over for Wayland apps.
    pub(super) fn write(&self, selection: Selection, data: HashMap<String, Vec<u8>>) -> Result<()> {
        let owner = &self.owner;
        let selection = self.atom(selection);
        if data.is_empty() {
            owner.offers.lock().unwrap().remove(&selection);
            if owner.conn.get_selection_owner(selection)?.reply()?.owner == owner.window {
                owner
                    .conn
                    .set_selection_owner(NONE, selection, CURRENT_TIME)?
                    .check()?;
            }
            return Ok(());
        }
        let formats = data
            .into_iter()
            .map(|(format, bytes)| Ok((atom(&owner.conn, &format)?, bytes)))
            .collect::<Result<_>>()?;
        owner.offers.lock().unwrap().insert(
            selection,
            Offer {
                formats,
                adopted: false,
            },
        );
        owner
            .conn
            .set_selection_owner(owner.window, selection, CURRENT_TIME)?
            .check()?;
        ensure!(
            owner.conn.get_selection_owner(selection)?.reply()?.owner == owner.window,
            "Clipboard unavailable"
        );
        let offers = owner.offers.lock().unwrap();
        let (offers, _) = owner
            .adopted
            .wait_timeout_while(offers, Duration::from_secs(1), |offers| {
                offers.get(&selection).is_some_and(|offer| !offer.adopted)
            })
            .unwrap();
        ensure!(
            offers.get(&selection).is_some_and(|offer| offer.adopted),
            "Clipboard unavailable"
        );
        Ok(())
    }
}
