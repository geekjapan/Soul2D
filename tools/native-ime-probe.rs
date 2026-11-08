use eframe::egui;
use std::{
    ffi::{c_char, c_void, CStr},
    fs::{File, OpenOptions},
    io::{Seek, Write},
    os::{
        fd::AsFd,
        unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt},
    },
    path::Path,
    sync::mpsc::{self, Receiver, Sender, TryRecvError},
    thread,
    time::{Duration, Instant},
};
use wayland_client::{
    delegate_noop,
    protocol::{wl_registry, wl_seat},
    Connection, Dispatch, QueueHandle, WEnum,
};
use wayland_protocols_misc::{
    zwp_input_method_v2::client::{zwp_input_method_manager_v2, zwp_input_method_v2},
    zwp_virtual_keyboard_v1::client::{zwp_virtual_keyboard_manager_v1, zwp_virtual_keyboard_v1},
};

#[repr(C)]
struct XkbNames {
    rules: *const c_char,
    model: *const c_char,
    layout: *const c_char,
    variant: *const c_char,
    options: *const c_char,
}

#[link(name = "xkbcommon")]
extern "C" {
    fn xkb_context_new(flags: u32) -> *mut c_void;
    fn xkb_context_unref(context: *mut c_void);
    fn xkb_keymap_new_from_names(
        context: *mut c_void,
        names: *const XkbNames,
        flags: u32,
    ) -> *mut c_void;
    fn xkb_keymap_get_as_string(keymap: *mut c_void, format: u32) -> *mut c_char;
    fn xkb_keymap_unref(keymap: *mut c_void);
    fn free(pointer: *mut c_void);
}

fn keymap(runtime: &Path) -> File {
    let names = XkbNames {
        rules: b"evdev\0".as_ptr().cast(),
        model: b"pc105\0".as_ptr().cast(),
        layout: b"us\0".as_ptr().cast(),
        variant: std::ptr::null(),
        options: std::ptr::null(),
    };
    let bytes = unsafe {
        let context = xkb_context_new(0);
        assert!(!context.is_null(), "XKB context unavailable");
        let map = xkb_keymap_new_from_names(context, &names, 0);
        let text = if map.is_null() {
            std::ptr::null_mut()
        } else {
            xkb_keymap_get_as_string(map, 1)
        };
        let bytes = if text.is_null() {
            None
        } else {
            Some(CStr::from_ptr(text).to_bytes_with_nul().to_vec())
        };
        if !text.is_null() {
            free(text.cast());
        }
        if !map.is_null() {
            xkb_keymap_unref(map);
        }
        xkb_context_unref(context);
        bytes.expect("XKB keymap serialization failed")
    };
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(runtime.join("probe-keymap.xkb"))
        .unwrap();
    file.write_all(&bytes).unwrap();
    file.rewind().unwrap();
    file
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Command {
    OldPreedit,
    TailPreedit,
    TailCommit,
    FreshPreedit,
    FreshCommit,
    SiblingPreedit,
    SiblingCommit,
    Stop,
}

impl Command {
    fn input(self) -> (&'static str, bool) {
        match self {
            Self::OldPreedit => ("original-preedit", false),
            Self::TailPreedit => ("retired-tail-preedit", false),
            Self::TailCommit => ("retired-commit", true),
            Self::FreshPreedit => ("fresh-search-preedit", false),
            Self::FreshCommit => ("fresh-search-commit", true),
            Self::SiblingPreedit => ("sibling-preedit", false),
            Self::SiblingCommit => ("sibling-commit", true),
            Self::Stop => panic!("Stop has no input"),
        }
    }
}

#[derive(Debug)]
enum Notice {
    Ready,
    State { active: bool, generation: u32 },
}

struct Engine {
    globals: std::collections::BTreeMap<u32, (String, u32)>,
    bound: Vec<u32>,
    seat_name: Option<String>,
    keyboard: bool,
    serial: u32,
    active: bool,
    pending_active: Option<bool>,
    generation: u32,
    deactivations: u32,
    notices: Sender<Notice>,
}

impl Dispatch<wl_registry::WlRegistry, ()> for Engine {
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
                println!("native_registry id={name} interface={interface} version={version}");
                state.globals.insert(name, (interface, version));
            }
            wl_registry::Event::GlobalRemove { name } => {
                assert!(!state.bound.contains(&name), "required global removed");
                state.globals.remove(&name);
            }
            _ => {}
        }
    }
}

impl Dispatch<wl_seat::WlSeat, ()> for Engine {
    fn event(
        state: &mut Self,
        _: &wl_seat::WlSeat,
        event: wl_seat::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        println!("native_seat {event:?}");
        match event {
            wl_seat::Event::Name { name } => state.seat_name = Some(name),
            wl_seat::Event::Capabilities {
                capabilities: WEnum::Value(capabilities),
            } => {
                state.keyboard = capabilities.contains(wl_seat::Capability::Keyboard);
            }
            _ => {}
        }
    }
}

impl Dispatch<zwp_input_method_v2::ZwpInputMethodV2, ()> for Engine {
    fn event(
        state: &mut Self,
        _: &zwp_input_method_v2::ZwpInputMethodV2,
        event: zwp_input_method_v2::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        println!("native_im_event {event:?}");
        match event {
            zwp_input_method_v2::Event::Activate => state.pending_active = Some(true),
            zwp_input_method_v2::Event::Deactivate => state.pending_active = Some(false),
            zwp_input_method_v2::Event::Done => {
                state.serial = state
                    .serial
                    .checked_add(1)
                    .expect("IM done count exhausted");
                if let Some(active) = state.pending_active.take() {
                    state.active = active;
                    if active {
                        state.generation = state.generation.checked_add(1).unwrap();
                    } else {
                        state.deactivations = state.deactivations.checked_add(1).unwrap();
                    }
                }
                println!(
                    "native_im_done count={} active={} generation={} deactivations={}",
                    state.serial, state.active, state.generation, state.deactivations
                );
                if state
                    .notices
                    .send(Notice::State {
                        active: state.active,
                        generation: state.generation,
                    })
                    .is_err()
                {
                    println!("native_app_observer_closed=true");
                }
            }
            zwp_input_method_v2::Event::Unavailable => panic!("owned input method unavailable"),
            _ => {}
        }
    }
}

delegate_noop!(Engine: ignore zwp_input_method_manager_v2::ZwpInputMethodManagerV2);
delegate_noop!(Engine: ignore zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1);
delegate_noop!(Engine: ignore zwp_virtual_keyboard_v1::ZwpVirtualKeyboardV1);

impl Engine {
    fn global(&mut self, interface: &str) -> (u32, u32) {
        let matches: Vec<_> = self
            .globals
            .iter()
            .filter(|(_, (name, _))| name == interface)
            .collect();
        assert_eq!(matches.len(), 1, "expected exactly one {interface}");
        let (&id, (_, version)) = matches[0];
        assert!(*version > 0);
        self.bound.push(id);
        (id, *version)
    }
}

fn run_engine(runtime: &Path, commands: Receiver<Command>, notices: Sender<Notice>) -> u32 {
    let connection = Connection::connect_to_env().unwrap();
    let mut queue = connection.new_event_queue();
    let handle = queue.handle();
    let registry = connection.display().get_registry(&handle, ());
    let mut state = Engine {
        globals: Default::default(),
        bound: Vec::new(),
        seat_name: None,
        keyboard: false,
        serial: 0,
        active: false,
        pending_active: None,
        generation: 0,
        deactivations: 0,
        notices,
    };
    queue.roundtrip(&mut state).unwrap();
    state.global("zwp_text_input_manager_v3");
    let (id, version) = state.global("wl_seat");
    assert!(version >= 2);
    let seat = registry.bind::<wl_seat::WlSeat, _, _>(id, version.min(8), &handle, ());
    let (id, _) = state.global("zwp_virtual_keyboard_manager_v1");
    let manager = registry
        .bind::<zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1, _, _>(
            id,
            1,
            &handle,
            (),
        );
    let keyboard = manager.create_virtual_keyboard(&seat, &handle, ());
    let map = keymap(runtime);
    keyboard.keymap(
        1,
        map.as_fd(),
        map.metadata().unwrap().len().try_into().unwrap(),
    );
    keyboard.modifiers(0, 0, 0, 0);
    let (id, _) = state.global("zwp_input_method_manager_v2");
    let manager = registry.bind::<zwp_input_method_manager_v2::ZwpInputMethodManagerV2, _, _>(
        id,
        1,
        &handle,
        (),
    );
    let input = manager.get_input_method(&seat, &handle, ());
    queue.roundtrip(&mut state).unwrap();
    assert_eq!(state.seat_name.as_deref(), Some("seat0"));
    assert!(
        state.keyboard,
        "virtual keyboard did not establish keyboard capability"
    );
    println!("native_engine_ready keyboard=true physical_key_requests=0");
    state.notices.send(Notice::Ready).unwrap();
    let mut original = None;
    let mut submitted = 0_u32;
    loop {
        queue.roundtrip(&mut state).unwrap();
        let command = match commands.recv_timeout(Duration::from_millis(10)) {
            Ok(command) => command,
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(error) => panic!("app command channel lost: {error}"),
        };
        if command == Command::Stop {
            break;
        }
        queue.roundtrip(&mut state).unwrap();
        assert!(
            state.active,
            "input sent without an applied active IM state"
        );
        if command == Command::OldPreedit {
            original = Some((state.generation, state.deactivations));
        }
        if matches!(
            command,
            Command::OldPreedit | Command::TailPreedit | Command::TailCommit
        ) {
            assert_eq!(
                original,
                Some((state.generation, state.deactivations)),
                "original composition was natively reset; hypothesis changed"
            );
        }
        let (text, commit) = command.input();
        submitted = submitted.checked_add(1).unwrap();
        println!("native_submission sequence={submitted} command={command:?} im_done_count={} generation={} text={text:?}", state.serial, state.generation);
        if commit {
            input.commit_string(text.to_owned());
            input.set_preedit_string(String::new(), 0, 0);
        } else {
            let cursor = i32::try_from(text.len()).unwrap();
            input.set_preedit_string(text.to_owned(), cursor, cursor);
        }
        input.commit(state.serial);
        connection.flush().unwrap();
    }
    input.destroy();
    keyboard.destroy();
    connection.flush().unwrap();
    println!("native_engine_stopped submissions={submitted}");
    submitted
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    Initial,
    OldPreedit,
    Reopen,
    Reopened,
    TailPreedit,
    TailCommit,
    FreshPreedit,
    FreshCommit,
    FocusSibling,
    SiblingPreedit,
    SiblingCommit,
    Complete,
}

struct Probe {
    phase: Phase,
    search: String,
    sibling: String,
    reopened: bool,
    autofocus: bool,
    reopened_frame: u64,
    sibling_focus_frame: Option<u64>,
    raw: Vec<egui::ImeEvent>,
    commands: Sender<Command>,
    notices: Receiver<Notice>,
    active: bool,
    started: Instant,
    completed: Option<Sender<()>>,
}

impl Probe {
    fn send(&mut self, command: Command, phase: Phase) {
        println!(
            "native_phase from={:?} to={phase:?} command={command:?} search={:?} sibling={:?}",
            self.phase, self.search, self.sibling
        );
        self.commands.send(command).unwrap();
        self.phase = phase;
    }
}

impl eframe::App for Probe {
    fn raw_input_hook(&mut self, _: &egui::Context, input: &mut egui::RawInput) {
        for event in &input.events {
            if let egui::Event::Ime(event) = event {
                println!("native_egui_ingress phase={:?} event={event:?}", self.phase);
                self.raw.push(event.clone());
            }
        }
    }

    fn update(&mut self, ctx: &egui::Context, _: &mut eframe::Frame) {
        assert!(
            self.started.elapsed() < Duration::from_secs(40),
            "native experiment incomplete; deadline is not evidence of safe rejection"
        );
        loop {
            match self.notices.try_recv() {
                Ok(Notice::State { active, generation }) => {
                    self.active = active;
                    println!("native_app_im_state active={active} generation={generation}");
                }
                Ok(Notice::Ready) => panic!("duplicate readiness"),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => panic!("native IM worker disconnected"),
            }
        }
        if self.phase == Phase::Reopen {
            self.reopened = true;
            self.search.clear();
            self.autofocus = true;
            self.reopened_frame = ctx.cumulative_frame_nr();
            self.phase = Phase::Reopened;
        }
        let search_id = egui::Id::new(("native-ime-search", self.reopened));
        let sibling_id = egui::Id::new("native-ime-sibling");
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.label("Search");
            let response = ui.add(egui::TextEdit::singleline(&mut self.search).id(search_id));
            if self.autofocus {
                response.request_focus();
                self.autofocus = false;
            }
            ui.label("Sibling");
            let response = ui.add(egui::TextEdit::singleline(&mut self.sibling).id(sibling_id));
            if self.phase == Phase::FocusSibling && self.sibling_focus_frame.is_none() {
                response.request_focus();
                self.sibling_focus_frame = Some(ctx.cumulative_frame_nr());
            }
        });
        let focused = ctx.memory(|memory| memory.focused());
        let native_focus = ctx.input(|input| input.focused);
        let raw = std::mem::take(&mut self.raw);
        if !raw.is_empty() {
            println!("native_frame frame={} phase={:?} search_owner={search_id:?} focused={focused:?} viewport_focused={native_focus} search={:?} sibling={:?}", ctx.cumulative_frame_nr(), self.phase, self.search, self.sibling);
        }
        let observed = |command: Command| {
            let (text, commit) = command.input();
            let count = raw
                .iter()
                .filter(|event| match event {
                    egui::ImeEvent::Preedit(value) => !commit && value == text,
                    egui::ImeEvent::Commit(value) => commit && value == text,
                    _ => false,
                })
                .count();
            assert!(count <= 1, "duplicate native payload");
            count == 1
        };
        match self.phase {
            Phase::Initial if native_focus && focused == Some(search_id) && self.active => {
                self.send(Command::OldPreedit, Phase::OldPreedit)
            }
            Phase::OldPreedit if observed(Command::OldPreedit) => {
                assert_eq!(self.search, Command::OldPreedit.input().0);
                self.phase = Phase::Reopen;
            }
            Phase::Reopened
                if ctx.cumulative_frame_nr() > self.reopened_frame
                    && native_focus
                    && focused == Some(search_id)
                    && self.active =>
            {
                assert!(
                    self.search.is_empty(),
                    "replacement must start empty with normal autofocus"
                );
                println!("native_reopened owner={search_id:?} focused=true value=empty");
                self.send(Command::TailPreedit, Phase::TailPreedit);
            }
            Phase::TailPreedit if observed(Command::TailPreedit) => {
                assert_eq!(self.search, Command::TailPreedit.input().0);
                self.send(Command::TailCommit, Phase::TailCommit);
            }
            Phase::TailCommit if observed(Command::TailCommit) => {
                assert_eq!(self.search, Command::TailCommit.input().0);
                self.send(Command::FreshPreedit, Phase::FreshPreedit);
            }
            Phase::FreshPreedit if observed(Command::FreshPreedit) => {
                assert_eq!(
                    self.search,
                    format!(
                        "{}{}",
                        Command::TailCommit.input().0,
                        Command::FreshPreedit.input().0
                    )
                );
                self.send(Command::FreshCommit, Phase::FreshCommit);
            }
            Phase::FreshCommit if observed(Command::FreshCommit) => {
                assert_eq!(
                    self.search,
                    format!(
                        "{}{}",
                        Command::TailCommit.input().0,
                        Command::FreshCommit.input().0
                    )
                );
                self.phase = Phase::FocusSibling;
            }
            Phase::FocusSibling
                if self
                    .sibling_focus_frame
                    .is_some_and(|frame| ctx.cumulative_frame_nr() > frame)
                    && native_focus
                    && focused == Some(sibling_id)
                    && self.active =>
            {
                self.send(Command::SiblingPreedit, Phase::SiblingPreedit)
            }
            Phase::SiblingPreedit if observed(Command::SiblingPreedit) => {
                assert_eq!(self.sibling, Command::SiblingPreedit.input().0);
                self.send(Command::SiblingCommit, Phase::SiblingCommit);
            }
            Phase::SiblingCommit if observed(Command::SiblingCommit) => {
                assert_eq!(self.sibling, Command::SiblingCommit.input().0);
                assert_eq!(
                    self.search,
                    format!(
                        "{}{}",
                        Command::TailCommit.input().0,
                        Command::FreshCommit.input().0
                    )
                );
                self.phase = Phase::Complete;
                println!("scope=LOCKED_NATIVE_EFRAME_DIAGNOSTIC\noutcome=BUG_OBSERVED\nretired_nonempty_tail=OBSERVED\nfresh_search=OBSERVED\nsibling=OBSERVED\nphysical_ime=NOT_RUN\nsoul2d_field_acceptance=NOT_RUN\nnative_reset_boundary=NOT_ESTABLISHED");
                self.completed.take().unwrap().send(()).unwrap();
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            _ => {}
        }
        ctx.request_repaint_after(Duration::from_millis(10));
    }
}

fn main() {
    let runtime = std::env::args().nth(1).expect("private runtime required");
    assert_eq!(std::env::args().len(), 2);
    assert_eq!(std::env::var("XDG_RUNTIME_DIR").unwrap(), runtime);
    assert!(std::env::var_os("WAYLAND_SOCKET").is_none());
    assert!(std::env::var_os("DISPLAY").is_none());
    assert!(std::env::var_os("DBUS_SESSION_BUS_ADDRESS").is_none());
    assert_eq!(std::env::var("WAYLAND_DEBUG").unwrap(), "client");
    let display = std::env::var("WAYLAND_DISPLAY").unwrap();
    assert!(display.starts_with("wayland-") && !display.contains('/'));
    let path = Path::new(&runtime);
    assert_eq!(path.metadata().unwrap().mode() & 0o777, 0o700);
    assert!(path
        .join(display)
        .metadata()
        .unwrap()
        .file_type()
        .is_socket());
    let (commands, receiver) = mpsc::channel();
    let (sender, notices) = mpsc::channel();
    let worker_runtime = runtime.clone();
    let worker = thread::spawn(move || run_engine(Path::new(&worker_runtime), receiver, sender));
    loop {
        if matches!(
            notices
                .recv_timeout(Duration::from_secs(10))
                .expect("native readiness missing"),
            Notice::Ready
        ) {
            break;
        }
    }
    let (completed, receipt) = mpsc::channel();
    let app_commands = commands.clone();
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([640.0, 480.0]),
        renderer: eframe::Renderer::Glow,
        ..Default::default()
    };
    let result = eframe::run_native(
        "Soul2D native IME diagnostic",
        options,
        Box::new(move |_| {
            Ok(Box::new(Probe {
                phase: Phase::Initial,
                search: String::new(),
                sibling: String::new(),
                reopened: false,
                autofocus: true,
                reopened_frame: 0,
                sibling_focus_frame: None,
                raw: Vec::new(),
                commands: app_commands,
                notices,
                active: false,
                started: Instant::now(),
                completed: Some(completed),
            }))
        }),
    );
    let _ = commands.send(Command::Stop);
    let submitted = worker.join().expect("native IM worker failed");
    result.unwrap();
    receipt
        .try_recv()
        .expect("native experiment did not complete every phase");
    assert_eq!(submitted, 7);
}
