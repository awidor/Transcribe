use anyhow::{Context, Result};
use std::{
    collections::HashMap,
    os::fd::{AsFd, AsRawFd, RawFd},
    time::Duration,
};
use tokio::io::unix::AsyncFd;
use wayland_client::{
    backend::{ObjectId, WaylandError},
    protocol::{wl_callback, wl_registry},
    Connection, Dispatch, EventQueue, Proxy, QueueHandle,
};
use wayland_protocols_wlr::foreign_toplevel::v1::client::{
    zwlr_foreign_toplevel_handle_v1::{self as handle, ZwlrForeignToplevelHandleV1},
    zwlr_foreign_toplevel_manager_v1::{self as manager, ZwlrForeignToplevelManagerV1},
};

pub(super) struct Target {
    pub app_id: String,
}

// Keep one connection throughout delivery: protocol object IDs are meaningful
// only within that connection, and distinguish even two windows of one app.
struct Socket(Connection);

impl AsRawFd for Socket {
    fn as_raw_fd(&self) -> RawFd {
        self.0.as_fd().as_raw_fd()
    }
}

struct Tracker {
    socket: AsyncFd<Socket>,
    queue: EventQueue<State>,
    state: State,
}

impl Tracker {
    async fn sync(&mut self) -> Result<()> {
        tokio::time::timeout(Duration::from_millis(250), async {
            self.state.synced = false;
            self.socket
                .get_ref()
                .0
                .display()
                .sync(&self.queue.handle(), ());
            self.queue.flush()?;
            loop {
                self.queue.dispatch_pending(&mut self.state)?;
                if self.state.synced {
                    return Ok(());
                }
                if let Some(read) = self.queue.prepare_read() {
                    let mut ready = self.socket.readable().await?;
                    match read.read() {
                        Ok(_) => {}
                        Err(WaylandError::Io(error))
                            if error.kind() == std::io::ErrorKind::WouldBlock =>
                        {
                            ready.clear_ready();
                        }
                        Err(error) => return Err(error.into()),
                    }
                }
            }
        })
        .await
        .context("Wayland focus lookup timed out")?
    }
}

pub(super) async fn capture() -> Result<Option<Target>> {
    let connection = Connection::connect_to_env()?;
    let queue = connection.new_event_queue();
    connection.display().get_registry(&queue.handle(), ());
    let mut tracker = Tracker {
        // The socket stays owned by the connection, which outlives the registration.
        socket: unsafe { AsyncFd::register(Socket(connection)) }
            .map_err(|error| anyhow::anyhow!(error.to_string()))?,
        queue,
        state: State::default(),
    };
    tracker.sync().await?;
    if tracker.state.manager.is_none() {
        return Ok(None);
    }
    // The registry callback binds the manager; a second sync receives windows.
    tracker.sync().await?;
    let (_, window) = tracker
        .state
        .focused()
        .context("Focus a destination before pasting")?;
    Ok(Some(Target {
        app_id: window.app_id.clone(),
    }))
}

#[derive(Clone, Default)]
struct Window {
    app_id: String,
    active: bool,
}

#[derive(Default)]
struct State {
    manager: Option<u32>,
    synced: bool,
    windows: HashMap<ObjectId, (Window, Window)>,
}

impl State {
    fn focused(&self) -> Option<(&ObjectId, &Window)> {
        self.manager?;
        let mut active = self
            .windows
            .iter()
            .filter(|(_, (committed, _))| committed.active)
            .map(|(id, (committed, _))| (id, committed));
        let window = active.next()?;
        // Ambiguous focus must not choose a receiver arbitrarily.
        active.next().is_none().then_some(window)
    }
}

impl Dispatch<wl_registry::WlRegistry, ()> for State {
    fn event(
        state: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        match event {
            wl_registry::Event::Global {
                name,
                interface,
                version,
            } if interface == ZwlrForeignToplevelManagerV1::interface().name => {
                registry.bind::<ZwlrForeignToplevelManagerV1, _, _>(name, version.min(3), qh, ());
                state.manager = Some(name);
            }
            wl_registry::Event::GlobalRemove { name } if state.manager == Some(name) => {
                state.manager = None;
            }
            _ => {}
        }
    }
}

impl Dispatch<ZwlrForeignToplevelManagerV1, ()> for State {
    fn event(
        state: &mut Self,
        _: &ZwlrForeignToplevelManagerV1,
        event: manager::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            manager::Event::Toplevel { toplevel } => {
                state.windows.insert(toplevel.id(), Default::default());
            }
            manager::Event::Finished => state.manager = None,
            _ => {}
        }
    }

    wayland_client::event_created_child!(State, ZwlrForeignToplevelManagerV1, [
        0 => (ZwlrForeignToplevelHandleV1, ())
    ]);
}

impl Dispatch<ZwlrForeignToplevelHandleV1, ()> for State {
    fn event(
        state: &mut Self,
        proxy: &ZwlrForeignToplevelHandleV1,
        event: handle::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if matches!(event, handle::Event::Closed) {
            state.windows.remove(&proxy.id());
            proxy.destroy();
            return;
        }
        let Some((committed, pending)) = state.windows.get_mut(&proxy.id()) else {
            return;
        };
        match event {
            handle::Event::AppId { app_id } => pending.app_id = app_id,
            handle::Event::State { state } => {
                pending.active = state.chunks_exact(4).any(|bytes| {
                    u32::from_ne_bytes(bytes.try_into().unwrap()) == handle::State::Activated as u32
                });
            }
            handle::Event::Done => *committed = pending.clone(),
            _ => {}
        }
    }
}

impl Dispatch<wl_callback::WlCallback, ()> for State {
    fn event(
        state: &mut Self,
        _: &wl_callback::WlCallback,
        _: wl_callback::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        state.synced = true;
    }
}
