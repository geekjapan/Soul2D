use egui_winit::{egui, winit, State};
use winit::{application::ApplicationHandler, event::{Ime, WindowEvent},
    event_loop::{ActiveEventLoop, EventLoop}, window::{Window, WindowId}};

fn converted(state: &mut State, window: &Window, events: &[Ime]) -> Vec<egui::Event> {
    state.egui_input_mut().events.clear();
    for event in events {
        let response=state.on_window_event(window,&WindowEvent::Ime(event.clone()));
        assert!(response.repaint);
        assert!(!response.consumed,"the fixture has no focused egui field");
    }
    state.egui_input().events.clone()
}

fn state(window: &Window) -> State {
    State::new(egui::Context::default(),egui::ViewportId::ROOT,window,None,None,None)
}

struct Probe { completed: bool }

impl ApplicationHandler for Probe {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.completed {return;}
        let window=event_loop.create_window(Window::default_attributes()
            .with_visible(false).with_active(false).with_title("Soul2D controlled IME adapter probe")).unwrap();
        let mut retired=state(&window);
        let mut fresh=state(&window);
        let preedit=Ime::Preedit("controlled composition".into(),Some((0,22)));
        converted(&mut retired,&window,&[preedit.clone(),Ime::Disabled]);
        let tail=[preedit.clone(),Ime::Commit("controlled commit".into()),Ime::Disabled];
        let old=converted(&mut retired,&window,&tail);
        let new=converted(&mut fresh,&window,&tail);
        assert_eq!(old,new,"adapter output carries no experiment-supplied old/fresh label");
        assert_eq!(old,vec![egui::Event::Ime(egui::ImeEvent::Enabled),
            egui::Event::Ime(egui::ImeEvent::Preedit("controlled composition".into())),
            egui::Event::Ime(egui::ImeEvent::Commit("controlled commit".into())),
            egui::Event::Ime(egui::ImeEvent::Disabled),
            egui::Event::Ime(egui::ImeEvent::Disabled)]);
        let enabled=converted(&mut fresh,&window,&[Ime::Enabled]);
        if cfg!(target_os="linux") {assert!(enabled.is_empty());}
        else {assert_eq!(enabled,vec![egui::Event::Ime(egui::ImeEvent::Enabled)]);}
        converted(&mut fresh,&window,&[Ime::Disabled]);
        let cancellation=converted(&mut fresh,&window,&[preedit,Ime::Preedit(String::new(),None),Ime::Disabled]);
        assert_eq!(cancellation,vec![egui::Event::Ime(egui::ImeEvent::Enabled),
            egui::Event::Ime(egui::ImeEvent::Preedit("controlled composition".into())),
            egui::Event::Ime(egui::ImeEvent::Disabled),
            egui::Event::Ime(egui::ImeEvent::Disabled)]);
        assert_eq!(converted(&mut fresh,&window,&[Ime::Commit("repeated commit".into())]),
            vec![egui::Event::Ime(egui::ImeEvent::Commit("repeated commit".into())),
                egui::Event::Ime(egui::ImeEvent::Disabled)]);
        assert!(!fresh.allow_ime(),"the platform-output debounce cache is not an acknowledgement");
        println!("scope=ACTUAL_ADAPTER_SYNTHETIC_INGRESS_NOT_PHYSICAL_IME");
        println!("host={}-{}",std::env::consts::OS,std::env::consts::ARCH);
        println!("owned_hidden_window=true");
        println!("actual_on_window_event=true");
        println!("old_and_fresh_converted_events_equal=true");
        println!("field_lifecycle_and_soul2d_acceptance=NOT_RUN");
        println!("native_reset_callback_ordering=NOT_RUN");
        println!("physical_ime_fresh_and_sibling_liveness=NOT_RUN");
        self.completed=true;
        event_loop.exit();
    }

    fn window_event(&mut self, _: &ActiveEventLoop, _: WindowId, _: WindowEvent) {}
}

fn main() {
    let backend=std::env::args().nth(1).expect("explicit backend required");
    let mut builder=EventLoop::builder();
    #[cfg(target_os="macos")]
    {
        use winit::platform::macos::EventLoopBuilderExtMacOS;
        builder.with_activate_ignoring_other_apps(false);
    }
    #[cfg(target_os="linux")]
    match backend.as_str() {
        "x11" => {use winit::platform::x11::EventLoopBuilderExtX11;builder.with_x11();},
        "wayland" => {use winit::platform::wayland::EventLoopBuilderExtWayland;builder.with_wayland();},
        _ => panic!("Linux probe requires x11 or wayland"),
    }
    #[cfg(not(target_os="linux"))]
    assert_eq!(backend,std::env::consts::OS,"explicit backend must match this host");
    let event_loop=builder.build().unwrap();
    let mut probe=Probe {completed:false};
    event_loop.run_app(&mut probe).unwrap();
    assert!(probe.completed,"probe did not execute");
}
