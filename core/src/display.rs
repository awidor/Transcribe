use wayland_client::{
    globals::{registry_queue_init, GlobalListContents},
    protocol::wl_registry,
    Connection, Dispatch, QueueHandle,
};
use wayland_protocols_plasma::output_order::v1::client::kde_output_order_v1::{
    self, KdeOutputOrderV1,
};

/// The name of the output the desktop ranks first. Wayland itself names no
/// primary output; KDE announces its ranking, other desktops have none.
pub fn primary_output() -> Option<String> {
    let connection = Connection::connect_to_env().ok()?;
    let (globals, mut queue) = registry_queue_init::<Order>(&connection).ok()?;
    let order: KdeOutputOrderV1 = globals.bind(&queue.handle(), 1..=1, ()).ok()?;
    let mut state = Order::default();
    queue.roundtrip(&mut state).ok()?;
    order.destroy();
    state.outputs.into_iter().next()
}

#[derive(Default)]
struct Order {
    pending: Vec<String>,
    outputs: Vec<String>,
}

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for Order {
    fn event(
        _: &mut Self,
        _: &wl_registry::WlRegistry,
        _: wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<KdeOutputOrderV1, ()> for Order {
    fn event(
        state: &mut Self,
        _: &KdeOutputOrderV1,
        event: kde_output_order_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            kde_output_order_v1::Event::Output { output_name } => state.pending.push(output_name),
            kde_output_order_v1::Event::Done => state.outputs = std::mem::take(&mut state.pending),
            _ => (),
        }
    }
}
