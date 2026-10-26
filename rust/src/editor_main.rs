mod editor_agent;
mod editor_binding_values;
mod editor_depth;
mod editor_depth_draw;
mod editor_inspector;
mod editor_live2d_forms;
mod editor_live2d_objects;
mod editor_mesh_tools;
mod editor_motion3;
mod editor_motion_sync;
mod editor_part_split;
mod editor_physics_groups;
mod editor_psd_depth;
mod editor_sequence;
#[cfg(test)]
#[path = "../tests/common/mod.rs"]
mod scratch;
#[cfg(test)]
mod editor_test_support;
mod editor_shell;
mod editor_stdio;
mod editor_ui;
mod editor_viewport;
mod editor_windows;

fn main() -> eframe::Result {
    soul2d_core::crashdump::install("soul2d-crashdump");
    let mut mcp = false;
    let mut open: Option<String> = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--mcp" => mcp = true,
            "--open" => {
                let Some(path) = args.next() else {
                    eprintln!("usage: soul2d-editor [--mcp] [--open <path>]");
                    std::process::exit(2);
                };
                open = Some(path);
            }
            // A bare path is what file associations pass (`soul2d.exe "%1"`).
            path if !path.starts_with('-') => open = Some(path.to_owned()),
            _ => {
                eprintln!("usage: soul2d-editor [--mcp] [--open <path> | <path>]");
                std::process::exit(2);
            }
        }
    }
    // The last window size comes from the settings file (D `WinW`, `WinH`, `WinMax`); reading it here must not
    // move a corrupt file aside, which is the editor's job at startup.
    let geometry = editor_ui::stored_window_geometry();
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_inner_size([geometry.width, geometry.height])
            .with_maximized(geometry.maximized)
            .with_title(soul2d_core::window_shell::APP_NAME),
        renderer: eframe::Renderer::Glow,
        ..Default::default()
    };
    eframe::run_native(
        soul2d_core::window_shell::APP_NAME,
        options,
        Box::new(move |cc| {
            Ok(Box::new(editor_ui::EditorApp::new(cc, open, mcp)))
        }),
    )
}
