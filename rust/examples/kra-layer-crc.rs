//! Prints, for every paint layer of a KRA file, its rectangle and the CRC-32 of its decoded straight-alpha RGBA bytes.
//!
//! Usage: `kra-layer-crc FILE.kra`. The reader's limits are raised so that page-sized files decode; this is a check of the
//! tile decoder against an independent one (`tools/verify-real-files.py --layer-crc`), not an import.
//! Output: one `index left top right bottom crc` line per paint layer in document order, nothing else.

use soul2d_core::kra::{self, Kind, Limits};

fn main() {
    let path = std::env::args().nth(1).expect("usage: kra-layer-crc FILE.kra");
    let bytes = std::fs::read(&path).expect("could not read the file");
    let limits = Limits {
        max_pixels: u64::MAX / 8,
        max_entry_bytes: 4 << 30,
        max_total_bytes: 64 << 30,
        ..Limits::default()
    };
    let document = match kra::parse(&bytes, &limits) {
        Ok(document) => document,
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(1);
        }
    };
    let paint = document.layers.iter().filter(|layer| layer.kind == Kind::Paint);
    for (index, layer) in paint.enumerate() {
        println!(
            "{index} {} {} {} {} {:08x}",
            layer.left,
            layer.top,
            layer.right,
            layer.bottom,
            crc32fast::hash(&layer.rgba)
        );
    }
}
