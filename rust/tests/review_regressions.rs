mod common;

use serde_json::{json, Value};
use soul2d_core::autosave::*;
use soul2d_core::editor_controller::{EditorController, FILE_OP_BLOCKED_MESSAGE};
use std::fs;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

fn tmp(name: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!("s2dv-{}-{}", std::process::id(), name));
    let _ = fs::remove_dir_all(&p);
    fs::create_dir_all(&p).unwrap();
    common::scratch(p)
}
fn now(t: u64) -> Now { Now { mono_secs: t, unix_secs: 1_000_000 + t } }
fn ok_write(p: &Path) -> Result<(), String> { fs::write(p, b"data").map_err(|e| e.to_string()) }
fn bad_write(_: &Path) -> Result<(), String> { Err("disk full".into()) }

// ---------- 1
fn failing_write_keeps_backups(max: u32) {
    let cfg = tmp(&format!("a1-{max}"));
    let proj = cfg.join("proj/Foo.inx");
    let s = Settings::new(true, 1, max);
    let mut a = Autosave::new(&cfg, now(0));
    a.set_project(Some(proj.clone()), now(0));
    for i in 0..(max as u64 + 4) {
        a.autosave_now(&s, now(i), ok_write).unwrap();
        assert!(list_backups(&cfg, &proj).unwrap().len() <= max as usize, "rotation cap");
    }
    let before = list_backups(&cfg, &proj).unwrap();
    assert_eq!(before.len(), max as usize, "rotation fills to cap");
    for i in 0..3 {
        assert!(a.autosave_now(&s, now(100 + i), bad_write).is_err());
        let after = list_backups(&cfg, &proj).unwrap();
        assert_eq!(after, before, "failed write must not change backups");
        assert!(after.iter().all(|b| b.path.exists()));
    }
    a.autosave_now(&s, now(200), ok_write).unwrap();
    assert_eq!(list_backups(&cfg, &proj).unwrap().len(), max as usize);
}
#[test] fn item1_failed_write_max1() { failing_write_keeps_backups(1); }
#[test] fn item1_failed_write_max3() { failing_write_keeps_backups(3); }
#[test] fn item1_failed_write_via_tick_keeps_backup() {
    let cfg = tmp("a1tick");
    let proj = cfg.join("p/Foo.inx");
    let s = Settings::new(true, 1, 1);
    let mut a = Autosave::new(&cfg, now(0));
    a.set_project(Some(proj.clone()), now(0));
    let d = DocState { revision: 1, depth_revision: 0, modified: true };
    assert!(a.tick(&s, d, now(60), ok_write).unwrap().is_some());
    let d2 = DocState { revision: 2, depth_revision: 0, modified: true };
    assert!(a.tick(&s, d2, now(120), bad_write).is_err());
    assert_eq!(list_backups(&cfg, &proj).unwrap().len(), 1);
}

// ---------- 2
fn recovery_names(dir: &Path) -> PathBuf { dir.join("rust-recovery-1.inx") }
#[test] fn item2_outside_file_never_deleted() {
    let cfg = tmp("r2");
    let outside = tmp("r2-outside");
    let victim = recovery_names(&outside);
    fs::write(&victim, b"precious").unwrap();
    assert!(is_recovery_path(&victim), "name matches the pattern (test is non-trivial)");
    let mut r = Recovery::new(&cfg, now(0));
    r.adopt(victim.clone());
    assert!(r.current().is_none(), "outside file must not be adopted");
    r.tick(true, "inx", 1, now(60), ok_write).unwrap();
    r.release(now(61));
    assert!(victim.exists());
    // subdirectory of the recovery dir
    let dir = recovery_dir(&cfg);
    let sub = dir.join("sub");
    fs::create_dir_all(&sub).unwrap();
    let nested = recovery_names(&sub);
    fs::write(&nested, b"nested").unwrap();
    let mut r = Recovery::new(&cfg, now(0));
    r.adopt(nested.clone());
    assert!(r.current().is_none(), "nested file not adopted");
    r.tick(true, "inx", 1, now(60), ok_write).unwrap();
    r.release(now(61));
    assert!(nested.exists());
    // dotdot path that resolves outside
    let sneaky = dir.join("..").join(format!("../s2dv-{}-r2-outside", std::process::id())).join("rust-recovery-1.inx");
    let mut r = Recovery::new(&cfg, now(0));
    r.adopt(sneaky.clone());
    r.release(now(1));
    assert!(victim.exists(), "dotdot path");
}
#[cfg(unix)]
#[test] fn item2_symlink_in_recovery_dir() {
    let cfg = tmp("r2s");
    let outside = tmp("r2s-outside");
    let target = recovery_names(&outside);
    fs::write(&target, b"precious").unwrap();
    let dir = recovery_dir(&cfg);
    fs::create_dir_all(&dir).unwrap();
    let link = dir.join("rust-recovery-link.inx");
    std::os::unix::fs::symlink(&target, &link).unwrap();
    assert!(!list_recovery(&cfg).unwrap().contains(&link), "a symlink is not listed as a snapshot");
    let mut r = Recovery::new(&cfg, now(0));
    r.adopt(link.clone());
    println!("symlink adopted: {}", r.current().is_some());
    r.release(now(1));
    println!("after release: link exists={} (symlink_metadata ok={}), target exists={}",
        link.exists(), fs::symlink_metadata(&link).is_ok(), target.exists());
    assert!(target.exists(), "target must survive");
    // tick path
    let link2 = dir.join("rust-recovery-link2.inx");
    std::os::unix::fs::symlink(&target, &link2).unwrap();
    let mut r = Recovery::new(&cfg, now(0));
    r.adopt(link2.clone());
    r.tick(true, "inx", 1, now(60), ok_write).unwrap();
    assert!(target.exists(), "target must survive tick replacement");
    println!("after tick: link2 symlink present={}", fs::symlink_metadata(&link2).is_ok());
    // delete_recovery on the link
    delete_recovery(&cfg, &link).ok();
    assert!(target.exists());
}

// ---------- 3
#[test] fn item3_slow_reader_bounded() {
    #[cfg(windows)]
    fn socket_buffer(stream: &TcpStream, option: i32, size: i32) {
        use std::os::windows::io::AsRawSocket;
        #[link(name = "Ws2_32")]
        extern "system" {
            fn setsockopt(socket: usize, level: i32, option: i32, value: *const std::ffi::c_char, len: i32) -> i32;
        }
        let result = unsafe {
            setsockopt(stream.as_raw_socket() as usize, 0xffff, option,
                (&size as *const i32).cast(), std::mem::size_of_val(&size) as i32)
        };
        assert_eq!(result, 0, "setsockopt: {}", std::io::Error::last_os_error());
    }
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let client = std::thread::spawn(move || {
        let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
        #[cfg(windows)]
        socket_buffer(&s, 0x1002, 4096); // SO_RCVBUF
        let body = r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#;
        write!(s, "POST /mcp HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Length: {}\r\n\r\n{body}", body.len()).unwrap();
        let start = Instant::now();
        let mut total = 0usize;
        let mut buf = [0u8; 1024];
        // trickle: 1 KiB every 20 ms, for up to 15 s (would need ~13 minutes for the whole body)
        while start.elapsed() < Duration::from_secs(15) {
            match s.read(&mut buf) { Ok(0) | Err(_) => break, Ok(n) => total += n }
            std::thread::sleep(Duration::from_millis(20));
        }
        total
    });
    let (stream, _) = listener.accept().unwrap();
    // Windows can otherwise buffer the entire response before the slow reader drains it.
    #[cfg(windows)]
    socket_buffer(&stream, 0x1001, 0); // SO_SNDBUF: disable send buffering.
    let mut session = soul2d_core::Session::new();
    let budget = Duration::from_secs(2);
    let big = "x".repeat(4 * 1024 * 1024);
    let t = Instant::now();
    let result = soul2d_core::mcp_http::handle_connection_with(stream, None, budget, &mut |_m: &Value| {
        soul2d_core::mcp_http::RpcReply::plain(Some(json!({"jsonrpc":"2.0","id":1,"result":{"pad":big}})))
    });
    let elapsed = t.elapsed();
    let got = client.join().unwrap();
    assert!(result.is_err(), "the bounded slow reader forces the response to time out");
    let _ = &mut session;
    println!("elapsed={elapsed:?} client received {got} bytes of 4MiB");
    assert!(elapsed < budget + Duration::from_millis(1500) + Duration::from_millis(500), "elapsed {elapsed:?}");
    assert!(got < 4 * 1024 * 1024, "client never got everything, so the cut-off really happened");
}

// ---------- 4
fn mcp(c: &mut EditorController, id: u64, name: &str, args: Value) -> Value {
    c.mcp_request(&json!({"jsonrpc":"2.0","id":id,"method":"tools/call","params":{"name":name,"arguments":args}})).unwrap()
}
#[test] fn item4_panel_drafts_blocks_remote_file_ops() {
    let dir = tmp("e4");
    let file = dir.join("a.inx");
    let mut c = EditorController::new();
    c.panel_drafts = true;
    let p = file.to_str().unwrap();
    for (i, (name, args)) in [("new", json!({})), ("open", json!({"path": p})), ("save", json!({"path": p})),
        ("import_psd", json!({"path": p})), ("import_kra", json!({"path": p})),
        ("import_inp", json!({"path": p})), ("import_folder", json!({"path": dir.to_str().unwrap()}))].into_iter().enumerate() {
        let r = mcp(&mut c, i as u64 + 1, name, args);
        assert_eq!(r["result"]["isError"], true, "{name}: {r}");
        assert_eq!(r["result"]["content"][0]["text"], FILE_OP_BLOCKED_MESSAGE, "{name}");
    }
    assert!(!file.exists());
    c.panel_drafts = false;
    let r = mcp(&mut c, 20, "save", json!({"path": p}));
    assert_eq!(r["result"]["isError"], false, "{r}");
    assert!(file.exists());
    let r = mcp(&mut c, 21, "open", json!({"path": p}));
    assert_eq!(r["result"]["isError"], false, "{r}");
    let r = mcp(&mut c, 22, "new", json!({}));
    assert_eq!(r["result"]["isError"], false, "{r}");
    let r = mcp(&mut c, 23, "import_psd", json!({"path": dir.join("none.psd").to_str().unwrap()}));
    assert_ne!(r["result"]["content"][0]["text"], FILE_OP_BLOCKED_MESSAGE, "not blocked when false: {r}");
}

// ---------- 5
#[test] fn item5_same_stem_projects_do_not_share_backups() {
    let cfg = tmp("c5");
    let pa = cfg.join("dirA/Foo.inx");
    let pb = cfg.join("dirB/Foo.inx");
    let s = Settings::new(true, 1, 1);
    let mut a = Autosave::new(&cfg, now(0));
    a.set_project(Some(pa.clone()), now(0));
    let ba = a.autosave_now(&s, now(10), ok_write).unwrap();
    assert!(!unclean_exit(&cfg, &pb));
    assert!(list_backups(&cfg, &pb).unwrap_or_default().is_empty());
    let mut b = Autosave::new(&cfg, now(0));
    b.set_project(Some(pb.clone()), now(0));
    let bb = b.autosave_now(&s, now(20), ok_write).unwrap();
    assert!(ba.exists() && bb.exists());
    assert_ne!(bb.parent(), ba.parent());
    b.release_lock();
    assert!(unclean_exit(&cfg, &pa));
}

// ---------- fuzz
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 { self.0 ^= self.0 << 13; self.0 ^= self.0 >> 7; self.0 ^= self.0 << 17; self.0 }
    fn below(&mut self, n: usize) -> usize { (self.next() % n as u64) as usize }
}
fn hex(b: &[u8]) -> String { b.iter().map(|x| format!("{x:02x}")).collect() }
#[test] fn fuzz_jpeg_decoder() {
    const SEED: u64 = 0x9E3779B97F4A7C15;
    let mut rng = Rng(SEED);
    let mut seeds = Vec::new();
    for &(w, h) in &[(1u32, 1u32), (7, 5), (16, 16), (33, 17), (64, 48), (130, 9)] {
        let mut img = image::RgbaImage::new(w, h);
        for (x, y, p) in img.enumerate_pixels_mut() {
            *p = image::Rgba([(x * 7 + y) as u8, (x * y) as u8 ^ (rng.next() as u8 & 0x3f), (y * 11) as u8, 255]);
        }
        let bytes = soul2d_core::export::encode_jpeg(&img).unwrap();
        let back = soul2d_core::jpeg::decode(&bytes, 256, 1 << 20).unwrap();
        assert_eq!(back.dimensions(), (w, h));
        seeds.push(bytes);
    }
    let (max_side, max_pixels) = (64u32, 64u64 * 48);
    let (mut ok, mut err, mut panics) = (0u32, 0u32, 0u32);
    let started = Instant::now();
    for i in 0..20_000u32 {
        let base = &seeds[rng.below(seeds.len())];
        let mut m = base.clone();
        match rng.below(4) {
            0 => { let n = rng.below(m.len()); m[n] = rng.next() as u8; }
            1 => for _ in 0..(2 + rng.below(8)) { let n = rng.below(m.len()); m[n] = rng.next() as u8; },
            2 => { let n = rng.below(m.len()); m.truncate(n); }
            _ => { let lim = m.len().min(700); for _ in 0..(1 + rng.below(4)) { let n = rng.below(lim); m[n] = rng.next() as u8; } if rng.below(2) == 0 { let n = rng.below(m.len()); m.truncate(n.max(2)); } }
        }
        let r = std::panic::catch_unwind(|| soul2d_core::jpeg::decode(&m, max_side, max_pixels));
        match r {
            Ok(Ok(img)) => {
                ok += 1;
                let (w, h) = img.dimensions();
                assert!(w <= max_side && h <= max_side && (w as u64) * (h as u64) <= max_pixels, "cap violated iter {i} seed {SEED:#x} {w}x{h} bytes {}", hex(&m));
            }
            Ok(Err(_)) => err += 1,
            Err(_) => { panics += 1; println!("PANIC iter {i} seed {SEED:#x} bytes {}", hex(&m)); }
        }
    }
    println!("fuzz: ok={ok} err={err} panics={panics} in {:?}", started.elapsed());
    assert_eq!(panics, 0);
}
