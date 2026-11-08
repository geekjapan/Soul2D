use std::collections::BTreeMap;
use wayland_client::{protocol::wl_registry, Connection, Dispatch, QueueHandle};

#[derive(Default)]
struct RegistrySnapshot {
    globals: BTreeMap<u32, (String, u32)>,
}

impl Dispatch<wl_registry::WlRegistry, ()> for RegistrySnapshot {
    fn event(
        state: &mut Self,
        _: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wl_registry::Event::Global {
                name,
                interface,
                version,
            } => {
                state.globals.insert(name, (interface, version));
            }
            wl_registry::Event::GlobalRemove { name } => {
                state.globals.remove(&name);
            }
            _ => {}
        }
    }
}

fn main() {
    assert!(
        std::env::var_os("WAYLAND_SOCKET").is_none(),
        "inherited display descriptors are not allowed"
    );
    assert_eq!(
        std::env::var("WAYLAND_DISPLAY").expect("owned display required"),
        "soul2d-launch"
    );
    let connection = Connection::connect_to_env().expect("connect to the owned Weston socket");
    let mut queue = connection.new_event_queue();
    let _registry = connection.display().get_registry(&queue.handle(), ());
    let mut snapshot = RegistrySnapshot::default();
    queue
        .roundtrip(&mut snapshot)
        .expect("complete the registry roundtrip");
    assert!(
        snapshot
            .globals
            .values()
            .any(|(name, version)| name == "wl_compositor" && *version > 0),
        "the registry must advertise a compositor"
    );
    println!("scope=OWNED_WESTON_REGISTRY_CAPABILITIES_ONLY");
    println!("registry_roundtrip=completed");
    println!("registry_snapshot_global_count={}", snapshot.globals.len());
    for name in [
        "wl_compositor",
        "wl_seat",
        "zwp_text_input_manager_v3",
        "zwp_input_method_v1",
        "zwp_input_method_manager_v2",
    ] {
        let mut versions: Vec<_> = snapshot
            .globals
            .values()
            .filter(|(interface, _)| interface == name)
            .map(|(_, version)| *version)
            .collect();
        versions.sort_unstable();
        println!("capability.{name}.versions={versions:?}");
    }
    println!("native_window=NOT_CREATED");
    println!("seat_and_input_method_binding=NOT_ATTEMPTED");
    println!("native_ime_route=NOT_VERIFIED");
    println!("reset_and_fresh_sibling_acceptance=NOT_RUN");
}
