//! Crash report on panic, ported from `utils/crashdump.d` (`genCrashDump`, `writeCrashDump`). Native
//! signal handling is not ported: Rust code does not segfault on bad input.
//!
//! The report has D's two sections, `=== Args State ===` (the command line) and `=== Exception ===` (the
//! panic message, its location and a backtrace). It is written to `<dir>/<name>-<timestamp>.txt`; `dir` is
//! the Desktop on Windows, `~/Library/Logs` on macOS and `$XDG_STATE_HOME` (default `~/.local/state`)
//! elsewhere, or `SOUL2D_CRASHDUMP_DIR` when set.

use std::backtrace::Backtrace;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// Where reports go. `None` when the platform directory cannot be found (no home directory).
pub fn dump_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("SOUL2D_CRASHDUMP_DIR").filter(|dir| !dir.is_empty()) {
        return Some(dir.into());
    }
    let home = || std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")).map(PathBuf::from);
    if cfg!(windows) {
        home().map(|home| home.join("Desktop"))
    } else if cfg!(target_os = "macos") {
        home().map(|home| home.join("Library").join("Logs"))
    } else {
        std::env::var_os("XDG_STATE_HOME")
            .filter(|dir| !dir.is_empty())
            .map(PathBuf::from)
            .or_else(|| home().map(|home| home.join(".local").join("state")))
    }
}

/// `YYYYMMDDTHHMMSS.uuuuuu` in UTC, the shape of D's `toISOString` (which uses local time).
pub fn timestamp(now: SystemTime) -> String {
    let since = now.duration_since(UNIX_EPOCH).unwrap_or_default();
    let (days, rest) = ((since.as_secs() / 86_400) as i64, since.as_secs() % 86_400);
    // Civil-from-days (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}{month:02}{day:02}T{:02}{:02}{:02}.{:06}",
        rest / 3_600,
        rest % 3_600 / 60,
        rest % 60,
        since.subsec_micros()
    )
}

/// The command line as the report shows it. Values after `--bearer` are secrets and are replaced.
pub fn args_state(args: &[String]) -> Vec<String> {
    let mut hide = false;
    args.iter()
        .map(|arg| {
            let shown = if hide { "<redacted>".to_owned() } else { arg.clone() };
            hide = arg == "--bearer";
            shown
        })
        .collect()
}

pub fn format_report(args: &[String], exception: &str) -> String {
    format!("=== Args State ===\n{}\n\n=== Exception ===\n{exception}", args_state(args).join(",\n"))
}

/// Writes `report` to `<dir>/<name>-<timestamp>.txt`, creating `dir` (mode 0700 on Unix) when needed.
pub fn write_report(dir: &Path, name: &str, report: &str) -> std::io::Result<PathBuf> {
    if !dir.exists() {
        std::fs::create_dir_all(dir)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
        }
    }
    let path = dir.join(format!("{name}-{}.txt", timestamp(SystemTime::now())));
    std::fs::write(&path, report)?;
    Ok(path)
}

/// Installs a panic hook that writes a report to `dir` (default: [`dump_dir`]) and tells the user where it
/// is, then runs the hook that was installed before.
pub fn install_in(dir: Option<PathBuf>, name: &'static str) {
    // Request handlers catch their own panics, so a hostile request could repeat one forever.
    const MAX_REPORTS: usize = 8;
    static REPORTS: AtomicUsize = AtomicUsize::new(0);
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if REPORTS.fetch_add(1, Ordering::Relaxed) >= MAX_REPORTS {
            return previous(info);
        }
        let location = info.location().map(|at| format!(" at {at}")).unwrap_or_default();
        let exception = format!("panic{location}: {info}\n\n{}", Backtrace::force_capture());
        let args: Vec<String> = std::env::args().collect();
        match dir.clone().or_else(dump_dir) {
            Some(dir) => match write_report(&dir, name, &format_report(&args, &exception)) {
                Ok(path) => eprintln!(
                    "Soul2D has crashed. A crash report was written to {}\nPlease attach it when filing an issue: \
                     https://github.com/geekjapan/Soul2D/issues",
                    path.display()
                ),
                Err(error) => eprintln!("Soul2D has crashed; the crash report could not be written: {error}"),
            },
            None => eprintln!("Soul2D has crashed; no directory is available for a crash report."),
        }
        previous(info);
    }));
}

/// [`install_in`] with the platform directory.
pub fn install(name: &'static str) {
    install_in(None, name);
}
