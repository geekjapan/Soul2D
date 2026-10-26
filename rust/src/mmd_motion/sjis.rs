//! A small Shift-JIS decoder for MMD bone, morph and file names, with the Windows code page 932 (CP932) mapping.
//!
//! Decoded: ASCII, half-width katakana (0xA1..=0xDF) and every double-byte pair that Python's `cp932` codec maps
//! (9604 of them: JIS X 0208, NEC row 13, the NEC-selected IBM extensions, the IBM extensions and the user-defined
//! area 0xF040..=0xF9FC, which the codec maps to the private use area). The pairs are looked up in `sjis_table`,
//! one `u16` per cell with 0 for "unmapped". Every other byte or pair decodes to U+FFFD: one for a bad single byte
//! (0x80, 0xA0, 0xFD..=0xFF), one per lead byte that has no trail byte or a trail byte outside 0x40..=0x7E and
//! 0x80..=0xFC (that trail byte is then decoded on its own), and one per pair the table leaves at 0. The name is kept
//! as raw bytes next to its text (`MmdName`), so an undecodable name can still be reported (hex) and matched by bytes.
//! The module only decodes; nothing here encodes text to Shift-JIS.
//!
//! The table is generated: `python3 tools/gen-sjis-table.py` rewrites `sjis_table.rs` from the codec, and
//! `python3 tools/gen-sjis-table.py --check` fails when the committed file is stale. Do not edit the table by hand.

use super::sjis_table::{TABLE, TRAIL_COUNT};

/// A name as stored in a VMD or VPD file: decoded text plus the exact bytes (up to the first NUL).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MmdName {
    pub text: String,
    pub raw: Vec<u8>,
}

impl MmdName {
    /// Cuts `field` at the first NUL (MMD pads fixed-width fields with NUL, sometimes followed by 0xFD filler).
    pub fn from_field(field: &[u8]) -> Self {
        let end = field.iter().position(|&byte| byte == 0).unwrap_or(field.len());
        Self::from_bytes(&field[..end])
    }

    pub fn from_bytes(bytes: &[u8]) -> Self {
        Self { text: decode(bytes), raw: bytes.to_vec() }
    }

    /// Lowercase hex of the raw bytes, for reporting names this decoder cannot read.
    pub fn hex(&self) -> String {
        self.raw.iter().map(|byte| format!("{byte:02x}")).collect()
    }

    pub fn matches_bytes(&self, bytes: &[u8]) -> bool {
        self.raw == bytes
    }

    /// Printable form for error messages: at most 16 characters, control characters (which the decoder passes
    /// through from ASCII bytes) replaced by `?`, so file content cannot inject line breaks or escape sequences.
    pub fn label(&self) -> String {
        self.text.chars().take(16).map(|c| if c.is_control() { '?' } else { c }).collect()
    }

    /// False when any byte decoded to U+FFFD.
    pub fn is_fully_decoded(&self) -> bool {
        !self.text.contains('\u{FFFD}')
    }
}

/// Decodes `bytes`; undecodable sequences become U+FFFD (one per lead byte, or per bad single byte).
pub fn decode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        let lead = bytes[index];
        index += 1;
        match lead {
            0x00..=0x7F => out.push(char::from(lead)),
            0xA1..=0xDF => out.push(char::from_u32(0xFF61 + u32::from(lead - 0xA1)).unwrap_or('\u{FFFD}')),
            0x81..=0x9F | 0xE0..=0xFC => {
                let Some(&trail) = bytes.get(index) else {
                    out.push('\u{FFFD}');
                    continue;
                };
                if !matches!(trail, 0x40..=0x7E | 0x80..=0xFC) {
                    // The trail byte is not part of this character; it is decoded on its own next.
                    out.push('\u{FFFD}');
                    continue;
                }
                index += 1;
                out.push(decode_pair(lead, trail).unwrap_or('\u{FFFD}'));
            }
            _ => out.push('\u{FFFD}'),
        }
    }
    out
}

fn decode_pair(lead: u8, trail: u8) -> Option<char> {
    let row = usize::from(if lead <= 0x9F { lead - 0x81 } else { lead - 0xC1 });
    let column = usize::from(if trail <= 0x7E { trail - 0x40 } else { trail - 0x41 });
    match TABLE[row * TRAIL_COUNT + column] {
        0 => None,
        code => char::from_u32(u32::from(code)),
    }
}
