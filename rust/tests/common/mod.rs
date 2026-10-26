//! Removes the scratch directories and files a test registers, once that test has ended.
//!
//! Test helpers call `scratch` with the exact path they just created. Nothing is ever removed by prefix or glob.
//! Included by `mod common;` in each integration test and by `#[path]` in the unit-test crates, so it must stay
//! free of anything a crate might leave unused.

use std::cell::{Cell, RefCell};
use std::fs;
use std::path::PathBuf;
use std::sync::Once;

thread_local! {
    static PANICKED: Cell<bool> = const { Cell::new(false) };
    static REGISTERED: RefCell<Registered> = const { RefCell::new(Registered(Vec::new())) };
}

/// The libtest harness runs each test on its own thread, so this is dropped when the test ends.
struct Registered(Vec<PathBuf>);

impl Drop for Registered {
    fn drop(&mut self) {
        // A failing test keeps its files for inspection.
        if PANICKED.with(Cell::get) {
            return;
        }
        for path in &self.0 {
            let _ = if path.is_dir() { fs::remove_dir_all(path) } else { fs::remove_file(path) };
        }
    }
}

/// Registers `path` for removal when the calling thread ends and returns it unchanged.
///
/// Call it from the test's own thread: a path registered on a spawned thread vanishes when that thread ends.
pub fn scratch(path: PathBuf) -> PathBuf {
    static HOOK: Once = Once::new();
    // The harness catches the panic of a failing test, so `thread::panicking()` is already false when thread
    // locals are dropped; a panic hook is the only place that still sees it.
    HOOK.call_once(|| {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            let _ = PANICKED.try_with(|panicked| panicked.set(true));
            previous(info);
        }));
    });
    REGISTERED.with(|registered| registered.borrow_mut().0.push(path.clone()));
    path
}
