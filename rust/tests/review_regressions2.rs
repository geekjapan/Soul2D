mod common;

use soul2d_core::autosave::*;
use soul2d_core::{jpeg, window_shell};
use std::{fs, path::{Path, PathBuf}, collections::HashMap};
fn dir(name: &str) -> PathBuf {
 let p=common::scratch(std::env::temp_dir().join(format!("s2d-review2-{}-{name}",std::process::id())));
 let _=fs::remove_dir_all(&p); fs::create_dir_all(&p).unwrap(); fs::canonicalize(p).unwrap()
}
fn now(t:u64)->Now { Now { mono_secs:t, unix_secs:1000+t } }
fn write(p:&Path)->Result<(),String> {fs::write(p,b"backup").map_err(|e|e.to_string())}
struct BitSink {
    bytes: Vec<u8>,
    acc: u32,
    count: u32,
}

impl BitSink {
    fn put(&mut self, value: u32, bits: u32) {
        for shift in (0..bits).rev() {
            self.acc = self.acc << 1 | (value >> shift & 1);
            self.count += 1;
            if self.count == 8 {
                self.flush_byte();
            }
        }
    }
    fn flush_byte(&mut self) {
        self.bytes.push(self.acc as u8);
        if self.acc as u8 == 0xff {
            self.bytes.push(0);
        }
        self.acc = 0;
        self.count = 0;
    }
    fn align(&mut self) {
        while self.count != 0 {
            self.put(1, 1);
        }
    }
}

fn segment(out: &mut Vec<u8>, marker: u8, payload: &[u8]) {
    out.extend_from_slice(&[0xff, marker]);
    out.extend_from_slice(&((payload.len() + 2) as u16).to_be_bytes());
    out.extend_from_slice(payload);
}

/// A JPEG whose blocks are flat (DC only), so the expected samples are exact: `sampling` lists `(h, v)` per
/// component and `level(component, block_x, block_y)` the 8-bit sample of one block. Quantisation is all ones;
/// the DC table has 12 four-bit codes and the AC table only the end-of-block symbol.
fn flat_jpeg(width: u16, height: u16, sampling: &[(u8, u8)], restart: u16, level: &dyn Fn(usize, usize, usize) -> u8) -> Vec<u8> {
    let mut out = vec![0xff, 0xd8];
    let mut dqt = vec![0u8];
    dqt.extend([1u8; 64]);
    segment(&mut out, 0xdb, &dqt);
    let mut frame = vec![8];
    frame.extend(height.to_be_bytes());
    frame.extend(width.to_be_bytes());
    frame.push(sampling.len() as u8);
    for (i, (h, v)) in sampling.iter().enumerate() {
        frame.extend([i as u8 + 1, h << 4 | v, 0]);
    }
    segment(&mut out, 0xc0, &frame);
    let mut dc = vec![0x00, 0, 0, 0, 12, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
    dc.extend(0..12u8);
    segment(&mut out, 0xc4, &dc);
    let mut ac = vec![0x10, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
    ac.push(0);
    segment(&mut out, 0xc4, &ac);
    if restart > 0 {
        segment(&mut out, 0xdd, &restart.to_be_bytes());
    }
    let mut scan = vec![sampling.len() as u8];
    for i in 0..sampling.len() {
        scan.extend([i as u8 + 1, 0x00]);
    }
    scan.extend([0, 63, 0]);
    segment(&mut out, 0xda, &scan);
    let hmax = sampling.iter().map(|s| s.0).max().unwrap() as usize;
    let vmax = sampling.iter().map(|s| s.1).max().unwrap() as usize;
    let mcus_x = (width as usize).div_ceil(8 * hmax);
    let mcus_y = (height as usize).div_ceil(8 * vmax);
    let mut sink = BitSink { bytes: Vec::new(), acc: 0, count: 0 };
    let mut previous = vec![0i32; sampling.len()];
    let (mut done, mut marker) = (0usize, 0u8);
    for my in 0..mcus_y {
        for mx in 0..mcus_x {
            if restart > 0 && done > 0 && done % restart as usize == 0 {
                sink.align();
                sink.bytes.extend_from_slice(&[0xff, 0xd0 + marker % 8]);
                marker += 1;
                previous.iter_mut().for_each(|p| *p = 0);
            }
            done += 1;
            for (c, (h, v)) in sampling.iter().enumerate() {
                for by in 0..*v as usize {
                    for bx in 0..*h as usize {
                        let target = (i32::from(level(c, mx * *h as usize + bx, my * *v as usize + by)) - 128) * 8;
                        let diff = target - previous[c];
                        previous[c] = target;
                        let size = 32 - diff.unsigned_abs().leading_zeros();
                        sink.put(size, 4);
                        if size > 0 {
                            let bits = if diff < 0 { (diff - 1) as u32 & ((1 << size) - 1) } else { diff as u32 };
                            sink.put(bits, size);
                        }
                        sink.put(0, 1); // end of block
                    }
                }
            }
        }
    }
    sink.align();
    out.extend(sink.bytes);
    out.extend_from_slice(&[0xff, 0xd9]);
    out
}


#[test]
fn crc_collision_keeps_projects_apart() {
 let cfg=dir("crc"); let mut seen=HashMap::new(); let mut rng=0x123456789abcdefu64;
 let mut pair=None;
 for _ in 0..1_000_000 {rng^=rng<<13; rng^=rng>>7; rng^=rng<<17;
 let p=cfg.join(format!("p-{rng:016x}/Foo.inx"));
 let key=crc32fast::hash(p.as_os_str().as_encoded_bytes());
 if let Some(old)=seen.insert(key,p.clone()) { if old!=p {pair=Some((old,p,key));break;} }
 }
 let (a,b,key)=pair.expect("collision"); fs::create_dir_all(a.parent().unwrap()).unwrap();fs::create_dir_all(b.parent().unwrap()).unwrap();
 assert_eq!(autosave_dir(&cfg,&a).unwrap(),autosave_dir(&cfg,&b).unwrap(),"same checksum, nothing saved yet");
 let settings=Settings::new(true,1,1);let mut aa=Autosave::new(&cfg,now(0));aa.set_project(Some(a.clone()),now(0));
 let old=aa.autosave_now(&settings,now(1),write).unwrap();
 let mut bb=Autosave::new(&cfg,now(0));bb.set_project(Some(b.clone()),now(0));
 let new=bb.autosave_now(&settings,now(2),write).unwrap();
 assert!(old.exists()&&new.exists());assert_ne!(old.parent(),new.parent());
 assert_eq!(list_backups(&cfg,&a).unwrap()[0].path,old);assert_eq!(list_backups(&cfg,&b).unwrap()[0].path,new);
 bb.release_lock();assert!(unclean_exit(&cfg,&a));let _=key;
}
#[test]
fn case_alias_shares_recovery_on_a_case_insensitive_filesystem() {
 let cfg=dir("case");let a=cfg.join("Foo.inx");let b=cfg.join("foo.inx");fs::write(&a,b"same").unwrap();
 if !b.exists(){println!("UNSUPPORTED host filesystem is case-sensitive");return;}
 assert_eq!(fs::read(&a).unwrap(),fs::read(&b).unwrap());
 let mut aa=Autosave::new(&cfg,now(0));aa.set_project(Some(a.clone()),now(0));aa.autosave_now(&Settings::new(true,1,1),now(1),write).unwrap();
 assert!(unclean_exit(&cfg,&a));assert!(unclean_exit(&cfg,&b));assert_eq!(list_backups(&cfg,&b).unwrap().len(),1);
}
#[cfg(unix)]
#[test]
fn symlink_alias_agrees_until_parent_is_missing() {
 use std::os::unix::fs::symlink;
 let cfg=dir("symlink");let real=cfg.join("real");fs::create_dir_all(&real).unwrap();let alias=cfg.join("alias");
 if fs::symlink_metadata(&alias).is_err(){symlink(&real,&alias).unwrap();}
 let a=real.join("Foo.inx");let b=alias.join("Foo.inx");assert_eq!(autosave_dir(&cfg,&a).unwrap(),autosave_dir(&cfg,&b).unwrap());
 let mut aa=Autosave::new(&cfg,now(0));aa.set_project(Some(b.clone()),now(0));aa.autosave_now(&Settings::new(true,1,1),now(1),write).unwrap();
 let moved=cfg.join("moved");if moved.exists(){fs::remove_dir_all(&moved).unwrap();}fs::rename(&real,&moved).unwrap();
 assert!(!unclean_exit(&cfg,&b));assert!(list_backups(&cfg,&b).unwrap().is_empty());
 println!("CONFIRMED missing symlink target makes original alias lose crash marker/backups");
 fs::rename(moved,real).unwrap();assert!(unclean_exit(&cfg,&b));
}
// The finding this test recorded (a renamed project loses its backups) is fixed on Unix by file identity.
#[cfg(unix)]
#[test]
fn moved_or_renamed_project_finds_its_backup() {
 let cfg=dir("move");let a=cfg.join("Foo.inx");let b=cfg.join("Bar.inx");fs::write(&a,b"main").unwrap();
 let mut aa=Autosave::new(&cfg,now(0));aa.set_project(Some(a.clone()),now(0));aa.autosave_now(&Settings::new(true,1,1),now(1),write).unwrap();
 fs::rename(a,b.clone()).unwrap();assert!(unclean_exit(&cfg,&b));assert_eq!(list_backups(&cfg,&b).unwrap().len(),1);
}
#[test]
fn grayscale_with_sampling_factors_and_restarts_decodes() {
 let mut bytes=flat_jpeg(16,8,&[(1,1)],1,&|_,x,_|if x==0{64}else{192});
 let sof=bytes.windows(2).position(|w|w==[255,192]).unwrap();bytes[sof+11]=0x21;
 let image=jpeg::decode(&bytes,64,4096).unwrap();
 assert_eq!(image.get_pixel(0,0).0,[64,64,64,255]);assert_eq!(image.get_pixel(15,0).0,[192,192,192,255]);
}
#[test]
fn adobe_rgb_jpeg_keeps_its_colours() {
 let mut bytes=flat_jpeg(8,8,&[(1,1),(1,1),(1,1)],0,&|c,_,_|[200,80,40][c]);
 let sof=bytes.windows(2).position(|w|w==[255,192]).unwrap();for(i,id)in b"RGB".iter().enumerate(){bytes[sof+10+i*3]=*id;}
 let sos=bytes.windows(2).position(|w|w==[255,218]).unwrap();for(i,id)in b"RGB".iter().enumerate(){bytes[sos+5+i*2]=*id;}
 let mut app=Vec::new();segment(&mut app,0xee,b"Adobe\x00\x64\x00\x00\x00\x00\x00");bytes.splice(2..2,app);
 let image=jpeg::decode(&bytes,64,4096).unwrap();let actual=image.get_pixel(0,0).0;assert!(actual.into_iter().zip([200u8,80,40,255]).all(|(a,b)|a.abs_diff(b)<=1),"{actual:?}");
}
#[test]
fn dropped_cmo3_and_can3_files_open() {
 for name in ["model.cmo3","animation.can3"] {let plan=window_shell::plan_drops(&[PathBuf::from(name)]);assert!(matches!(&plan[0],window_shell::DropAction::Open(_)));}
}

#[test]
fn bare_relative_filename_finds_absolute_recovery() {
 let cfg=dir("relative");let abs=cfg.join("Foo.inx");fs::write(&abs,b"main").unwrap();
 let mut aa=Autosave::new(&cfg,now(0));aa.set_project(Some(abs.clone()),now(0));aa.autosave_now(&Settings::new(true,1,1),now(1),write).unwrap();
 let cwd=std::env::current_dir().unwrap();std::env::set_current_dir(&cfg).unwrap();
 let found=unclean_exit(&cfg,Path::new("Foo.inx"));let relative_key=autosave_dir(&cfg,Path::new("Foo.inx")).unwrap();std::env::set_current_dir(cwd).unwrap();
 assert!(found);assert_eq!(relative_key,autosave_dir(&cfg,&abs).unwrap());
}
#[test]
fn ordinary_missing_absolute_parent_has_stable_key_when_created() {
 let cfg=dir("ordinary-missing-parent");let parent=cfg.join("missing");if parent.exists(){fs::remove_dir_all(&parent).unwrap();}
 let path=parent.join("Foo.inx");let before=autosave_dir(&cfg,&path).unwrap();fs::create_dir_all(&parent).unwrap();assert_eq!(before,autosave_dir(&cfg,&path).unwrap());
}
