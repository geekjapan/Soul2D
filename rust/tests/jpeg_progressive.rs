//! Progressive and unsupported JPEG streams. The fixtures are generated: gradients written by libjpeg's `cjpeg`.

use soul2d_core::jpeg;

const SIDE: u32 = 8192;
const PIXELS: u64 = 16 * 1024 * 1024;

const COLOR_SEQUENTIAL: &[&str] = &[
    "ffd8ffe000104a46494600010100000100010000ffdb0043000a07070807060a0808080b0a0a0b0e18100e0d0d0e1d15",
    "161118231f2524221f2221262b372f26293429212230413134393b3e3e3e252e4449433c48373d3e3bffdb0043010a0b",
    "0b0e0d0e1c10101c3b2822283b3b3b3b3b3b3b3b3b3b3b3b3b3b3b3b3b3b3b3b3b3b3b3b3b3b3b3b3b3b3b3b3b3b3b3b",
    "3b3b3b3b3b3b3b3b3b3b3b3b3b3bffc00011080014001803012200021101031101ffc4001f0000010501010101010100",
    "000000000000000102030405060708090a0bffc400b5100002010303020403050504040000017d010203000411051221",
    "31410613516107227114328191a1082342b1c11552d1f02433627282090a161718191a25262728292a3435363738393a",
    "434445464748494a535455565758595a636465666768696a737475767778797a838485868788898a9293949596979899",
    "9aa2a3a4a5a6a7a8a9aab2b3b4b5b6b7b8b9bac2c3c4c5c6c7c8c9cad2d3d4d5d6d7d8d9dae1e2e3e4e5e6e7e8e9eaf1",
    "f2f3f4f5f6f7f8f9faffc4001f0100030101010101010101010000000000000102030405060708090a0bffc400b51100",
    "020102040403040705040400010277000102031104052131061241510761711322328108144291a1b1c109233352f015",
    "6272d10a162434e125f11718191a262728292a35363738393a434445464748494a535455565758595a63646566676869",
    "6a737475767778797a82838485868788898a92939495969798999aa2a3a4a5a6a7a8a9aab2b3b4b5b6b7b8b9bac2c3c4",
    "c5c6c7c8c9cad2d3d4d5d6d7d8d9dae2e3e4e5e6e7e8e9eaf2f3f4f5f6f7f8f9faffda000c03010002110311003f004d",
    "4ee7c4f0f936ed6c928e3b54f7fad6a36d611c13e8cac4f5c2d45169be249757cc57e92aa7bd4da85df89e3d41237852",
    "60bd00ef4f2fc350a508caa725a2aeddda32ad385455671506e4d4559b4417fe20d3edb485866d1d91dbef155c51516a",
    "bad6ae678e0b8d215f9e70b45704aa55c4c9d6b349ec94fa74ebf7918cc3cdd4f674a1a4125a4ccfd2bceb79a678eea7",
    "047abd54875fd5175861f6c90851900d1457d466308fd427a763dcc2d284e387e68a7ef32d59f8a3549759024955c03d",
    "d68a28af9f951a6a31b456dd830b4293954f757c4fa23fffd9",
];

const COLOR_PROGRESSIVE_RESTART: &[&str] = &[
    "ffd8ffe000104a46494600010100000100010000ffdb0043000a07070807060a0808080b0a0a0b0e18100e0d0d0e1d15",
    "161118231f2524221f2221262b372f26293429212230413134393b3e3e3e252e4449433c48373d3e3bffdb0043010a0b",
    "0b0e0d0e1c10101c3b2822283b3b3b3b3b3b3b3b3b3b3b3b3b3b3b3b3b3b3b3b3b3b3b3b3b3b3b3b3b3b3b3b3b3b3b3b",
    "3b3b3b3b3b3b3b3b3b3b3b3b3b3bffc20011080014001803012200021101031101ffc400180001010101010000000000",
    "0000000000000002030104ffc400190100020301000000000000000000000000000401020305ffdd00040002ffda000c",
    "03010002100310000001e69165210c23ffd0f2643ad6a39e7fffc4001a10010003010101000000000000000000000301",
    "0204110013ffda00080101000105024b698f5d92b4ffd0be83a941e995ffd14b6984565eff00ffd22ed661d7edffd3a6",
    "a596ffc4001d110002010501010000000000000000000001020300111221413161ffda0008010301013f01721b222dbd",
    "54d19cb151e7daffd0894111dc76a245bb6bb5ffc4001d11000201040300000000000000000000000102000304112112",
    "13c1ffda0008010201013f01b7a68a016c6a166a879fb3ffd0b803a0c28b81a9ffc4001b100002030101010000000000",
    "00000000000001020311211232ffda0008010100063f0251f3a24e93ffd0c749c9e9ffd1ccd129547fffd279267db3ff",
    "d3eb3fffc4001b10010101000203000000000000000000000100112181314191ffda0008010100013f21db8432cf57ff",
    "d022e17ce1709417ffd1188e6c757abfffd2d5fd7197a2ffd3e003d5ffda000c03010002000300000010bccfffd0800f",
    "ffc400191101000301010000000000000000000000010011213141ffda0008010301013f1018cd40a5227864327fffd0",
    "a70751783af09fffc4001b11010002030101000000000000000000000100113141512191ffda0008010201013f10f1e2",
    "2d6d226a20e035d6fecfffd0c6f90140c727ffc4001b10010002030101000000000000000000000100112131415161ff",
    "da0008010100013f10ae1f1c8ddc7751ffd0dcc1944b9fdf67ffd1195681d8dcd5cd47ffd2a84a7b221602c19fffd306",
    "0a3d8fffd9",
];

const GRAY_SEQUENTIAL: &[&str] = &[
    "ffd8ffe000104a46494600010100000100010000ffdb0043000a07070807060a0808080b0a0a0b0e18100e0d0d0e1d15",
    "161118231f2524221f2221262b372f26293429212230413134393b3e3e3e252e4449433c48373d3e3bffc0000b08000c",
    "001001011100ffc4001f0000010501010101010100000000000000000102030405060708090a0bffc400b51000020103",
    "03020403050504040000017d01020300041105122131410613516107227114328191a1082342b1c11552d1f024336272",
    "82090a161718191a25262728292a3435363738393a434445464748494a535455565758595a636465666768696a737475",
    "767778797a838485868788898a92939495969798999aa2a3a4a5a6a7a8a9aab2b3b4b5b6b7b8b9bac2c3c4c5c6c7c8c9",
    "cad2d3d4d5d6d7d8d9dae1e2e3e4e5e6e7e8e9eaf1f2f3f4f5f6f7f8f9faffda0008010100003f00a57c7568c416de4a",
    "bf4e2b62eaeae6db4f8addec012719c2d73f0ea174dada0331207ad7433eab74fa9431b32951d88affd9",
];

const GRAY_PROGRESSIVE: &[&str] = &[
    "ffd8ffe000104a46494600010100000100010000ffdb0043000a07070807060a0808080b0a0a0b0e18100e0d0d0e1d15",
    "161118231f2524221f2221262b372f26293429212230413134393b3e3e3e252e4449433c48373d3e3bffc2000b08000c",
    "001001011100ffc400160001010100000000000000000000000000050304ffda000801010000000186c3d1ffc4001810",
    "010003010000000000000000000000000201031300ffda00080101000105027ac7248d7162da6d5a7fffc40019100003",
    "0003000000000000000000000000000102111231ffda0008010100063f0253814e87448fffc400181001010003000000",
    "00000000000000000001001131a1ffda0008010100013f21d6ace0e669ad7262ffda0008010100000010efffc4001910",
    "01000301010000000000000000000000010011412151ffda0008010100013f104199e40d6ad5d401a90f63a00c49ffd9",
];

// 64x64 colour image with large flat regions and a striped corner, written by `cjpeg -quality 75` (4:2:0), once
// progressive (10 scans, optimised tables, no restart interval) and once sequential.
const FLAT_PROGRESSIVE: &[&str] = &[
    "ffd8ffe000104a46494600010100000100010000ffdb004300080606070605080707070909080a0c140d0c0b0b0c191213",
    "0f141d1a1f1e1d1a1c1c20242e2720222c231c1c2837292c30313434341f27393d38323c2e333432ffdb0043010909090c",
    "0b0c180d0d1832211c21323232323232323232323232323232323232323232323232323232323232323232323232323232",
    "3232323232323232323232ffc20011080040004003012200021101031101ffc40017000101010100000000000000000000",
    "000000060704ffc400190100020301000000000000000000000000020500030401ffda000c03010002100310000001bf12",
    "048c4b6dc4b780301db420b424625b6e25bc0180eda105a26cb949895ec13110d83b68416a5aa65aee71445bc43100d1cf",
    "ffc4001a100100020301000000000000000000000003012002043450ffda0008010100010502f2d5338dc2659bb770ddbb",
    "869fffc4001811000301010000000000000000000000000102102000ffda0008010301013f018d86c1a78cffc4001f1100",
    "000504030000000000000000000000000203041001111242203241ffda0008010201013f01863b7063b490b956c1a131bc",
    "a3d821ec7fffc4002610000003040b0100000000000000000000010203001120731213214142506182b1c3d191ffda0008",
    "010100063f02cacc14cceac0b1faa5e8b24f50f82f95e8fd8cd30394592d9d319a60728b25b3a60fffc4001d1000020104",
    "030000000000000000000000011121004051f0415071ffda0008010100013f21eac35128849687a735304e4ce5cf4e59b4",
    "11e23fffda000c03010002000300000010f3cac0f3cac0f3fa00f3c0c0ffc4001a11000202030000000000000000000000",
    "0001101141002031ffda0008010301013f1054d28c98c397c37fffc4001c110002020203000000000000000000000010c1",
    "00011131202171ffda0008010201013f10087c10cdb892defbde19dde4431fffc4001c1000020105010000000000000000",
    "0000000111213140505161b1ffda0008010100013f10c58e340c1010484e8a2f63ee86ad84cb9f7d4ecdb2364fffd9",
];

const FLAT_SEQUENTIAL: &[&str] = &[
    "ffd8ffe000104a46494600010100000100010000ffdb004300080606070605080707070909080a0c140d0c0b0b0c191213",
    "0f141d1a1f1e1d1a1c1c20242e2720222c231c1c2837292c30313434341f27393d38323c2e333432ffdb0043010909090c",
    "0b0c180d0d1832211c21323232323232323232323232323232323232323232323232323232323232323232323232323232",
    "3232323232323232323232ffc00011080040004003012200021101031101ffc4001f000001050101010101010000000000",
    "0000000102030405060708090a0bffc400b5100002010303020403050504040000017d0102030004110512213141061351",
    "6107227114328191a1082342b1c11552d1f02433627282090a161718191a25262728292a3435363738393a434445464748",
    "494a535455565758595a636465666768696a737475767778797a838485868788898a92939495969798999aa2a3a4a5a6a7",
    "a8a9aab2b3b4b5b6b7b8b9bac2c3c4c5c6c7c8c9cad2d3d4d5d6d7d8d9dae1e2e3e4e5e6e7e8e9eaf1f2f3f4f5f6f7f8f9",
    "faffc4001f0100030101010101010101010000000000000102030405060708090a0bffc400b51100020102040403040705",
    "040400010277000102031104052131061241510761711322328108144291a1b1c109233352f0156272d10a162434e125f1",
    "1718191a262728292a35363738393a434445464748494a535455565758595a636465666768696a737475767778797a8283",
    "8485868788898a92939495969798999aa2a3a4a5a6a7a8a9aab2b3b4b5b6b7b8b9bac2c3c4c5c6c7c8c9cad2d3d4d5d6d7",
    "d8d9dae2e3e4e5e6e7e8e9eaf2f3f4f5f6f7f8f9faffda000c03010002110311003f00f7fa28a2800a28a2800af892bedb",
    "af892bdfc8ff00e5e7cbf532a9d028a28af7cc8fb6e8a28af813a828a28a002be24afb6ebe24af7f23ff00979f2fd4caa7",
    "40a28a2bdf323edba28a2be04ea0a28af37d5753bf8fc6571025f5cac235089046b2b050a64d28118cf4c4b271fedb7f78",
    "e75a349d5972a13763d22be24af7bd2359d525fecbf3352bc7dff63dfba763bb77f64eecf3ce7cd973ebe63ff78e7c12be",
    "8729a0e973ddef6fd4ca6ee1451457b0667db7451457c09d415e5bac7fc8f373ff006128bff46e8f5ea55e5bac7fc8f373",
    "ff006128bff46e8f5d982fe23f4265b189a2ff00cc23fedc7ff70b5e235edda2ff00cc23fedc7ff70b5e235f4781fb5f2f",
    "d4ca414514577907ffd9",
];

fn bytes(parts: &[&str]) -> Vec<u8> {
    let hex: String = parts.concat();
    (0..hex.len()).step_by(2).map(|at| u8::from_str_radix(&hex[at..at + 2], 16).unwrap()).collect()
}

/// Decoded pixels and size.
fn decode(stream: &[u8]) -> ((u32, u32), Vec<u8>) {
    let image = jpeg::decode(stream, SIDE, PIXELS).unwrap();
    (image.dimensions(), image.into_raw())
}

// The streams are 24x20 (4:2:0) and 16x12 (grayscale) gradients written by libjpeg's cjpeg at quality 70. The
// progressive file holds the same quantised coefficients as the sequential one, so both must decode to the same
// pixels; the 4:2:0 progressive stream also carries restart markers every two blocks in each scan.

#[test]
fn progressive_color_with_restart_markers_matches_its_sequential_twin() {
    let progressive = bytes(COLOR_PROGRESSIVE_RESTART);
    assert!(progressive.windows(2).any(|w| w == [0xff, 0xc2]), "SOF2");
    assert!(progressive.windows(2).any(|w| w == [0xff, 0xd0]), "RST0");
    let (size, pixels) = decode(&progressive);
    assert_eq!(size, (24, 20));
    assert_eq!((size, pixels), decode(&bytes(COLOR_SEQUENTIAL)));
}

#[test]
fn progressive_grayscale_matches_its_sequential_twin() {
    let (size, pixels) = decode(&bytes(GRAY_PROGRESSIVE));
    assert_eq!(size, (16, 12));
    assert_eq!((size, pixels), decode(&bytes(GRAY_SEQUENTIAL)));
}

#[test]
fn progressive_scans_that_stop_early_or_lie_are_refused_without_panicking() {
    let stream = bytes(COLOR_PROGRESSIVE_RESTART);
    // Truncation at every offset ends in an `Err` or an image, never a panic; the second stream uses EOB runs.
    for length in 0..stream.len() {
        let _ = jpeg::decode(&stream[..length], SIDE, PIXELS);
    }
    let flat = bytes(FLAT_PROGRESSIVE);
    for length in 0..flat.len() {
        let _ = jpeg::decode(&flat[..length], SIDE, PIXELS);
    }
    let mut state = 0x9e37_79b9u32;
    for _ in 0..2000 {
        let mut damaged = stream.clone();
        for _ in 0..3 {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let at = (state >> 8) as usize % damaged.len();
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            damaged[at] = (state >> 16) as u8;
        }
        let _ = jpeg::decode(&damaged, 64, 4096);
    }
    // A band that mixes DC and AC coefficients is not a valid progressive scan.
    let mut mixed = bytes(GRAY_PROGRESSIVE);
    let scan = mixed.windows(2).position(|w| w == [0xff, 0xda]).unwrap();
    mixed[scan + 4 + 1 + 2 + 1] = 5; // Se of the first (DC) scan
    assert_eq!(jpeg::decode(&mixed, SIDE, PIXELS).unwrap_err(), "progressive JPEG scan mixes DC and AC coefficients");
    // The frame ends before any scan arrived.
    let header = &stream[..stream.windows(2).position(|w| w == [0xff, 0xda]).unwrap()];
    let mut empty = header.to_vec();
    empty.extend_from_slice(&[0xff, 0xd9]);
    assert_eq!(jpeg::decode(&empty, SIDE, PIXELS).unwrap_err(), "JPEG has no image data");
}

#[test]
fn unsupported_encodings_are_refused_by_name() {
    let sequential = bytes(GRAY_SEQUENTIAL);
    let frame = sequential.windows(2).position(|w| w == [0xff, 0xc0]).unwrap();
    let err = |stream: &[u8]| jpeg::decode(stream, SIDE, PIXELS).unwrap_err();
    // Frame body: length(2) precision(1) height(2) width(2) components(1) ...
    let mut cmyk = sequential.clone();
    cmyk[frame + 9] = 4;
    assert_eq!(err(&cmyk), "4-component (CMYK) JPEG is not supported");
    let mut two = sequential.clone();
    two[frame + 9] = 2;
    assert_eq!(err(&two), "2-component JPEG is not supported");
    for (marker, message) in [
        (0xc3, "lossless or hierarchical JPEG is not supported"),
        (0xc9, "arithmetic-coded JPEG is not supported"),
    ] {
        let mut other = sequential.clone();
        other[frame + 1] = marker;
        assert_eq!(err(&other), message);
    }
    let mut twelve = sequential.clone();
    twelve[frame + 4] = 12;
    assert_eq!(err(&twelve), "only 8-bit JPEG is supported");
    // A sequential frame whose scan covers a band is progressive data in the wrong frame type.
    let mut banded = sequential.clone();
    let scan = banded.windows(2).position(|w| w == [0xff, 0xda]).unwrap();
    banded[scan + 4 + 1 + 2 + 1] = 5;
    assert_eq!(err(&banded), "sequential JPEG scan selects a spectral band");
}

/// Segments (marker, payload) up to EOI; entropy-coded data after a scan header is skipped.
fn segments(stream: &[u8]) -> Vec<(u8, &[u8])> {
    let mut found = Vec::new();
    let mut at = 2;
    while at + 3 < stream.len() && stream[at + 1] != 0xd9 {
        let marker = stream[at + 1];
        let length = usize::from(u16::from_be_bytes([stream[at + 2], stream[at + 3]]));
        found.push((marker, &stream[at + 4..at + 2 + length]));
        at += 2 + length;
        if marker == 0xda {
            while !(stream[at] == 0xff && !matches!(stream[at + 1], 0 | 0xd0..=0xd7)) {
                at += 1;
            }
        }
    }
    found
}

/// Every symbol of every AC Huffman table in the stream.
fn ac_symbols(stream: &[u8]) -> Vec<u8> {
    let mut symbols = Vec::new();
    for (_, table) in segments(stream).into_iter().filter(|(marker, _)| *marker == 0xc4) {
        let mut rest = table;
        while !rest.is_empty() {
            let total: usize = rest[1..17].iter().map(|c| usize::from(*c)).sum();
            if rest[0] >> 4 == 1 {
                symbols.extend_from_slice(&rest[17..17 + total]);
            }
            rest = &rest[17 + total..];
        }
    }
    symbols
}

#[test]
fn progressive_eob_runs_and_zero_run_symbols_match_the_sequential_twin() {
    let progressive = bytes(FLAT_PROGRESSIVE);
    // The fixture must keep exercising the paths the gradient fixtures miss: its optimised AC tables define
    // EOBn (n >= 1, symbols 0x10..=0xE0 with a zero size) and ZRL (0xF0), so the encoder used them.
    let symbols = ac_symbols(&progressive);
    assert!(symbols.iter().any(|s| s & 15 == 0 && (0x10..=0xe0).contains(s)), "no EOBn symbol");
    assert!(symbols.contains(&0xf0), "no ZRL symbol");
    assert_eq!(segments(&progressive).iter().filter(|(marker, _)| *marker == 0xda).count(), 10);
    assert!(!segments(&progressive).iter().any(|(marker, _)| *marker == 0xdd), "no restart interval");
    let (size, pixels) = decode(&progressive);
    assert_eq!(size, (64, 64));
    assert_eq!((size, pixels), decode(&bytes(FLAT_SEQUENTIAL)));
}

// ---------------------------------------------------------------------------------------------------------
// Hand-built 8-bit grayscale progressive streams: quantisation all ones, the DC table holds the sizes 0..=11 as
// four-bit codes, the AC table a single one-bit symbol (code 0).
// ---------------------------------------------------------------------------------------------------------

fn segment(out: &mut Vec<u8>, marker: u8, payload: &[u8]) {
    out.extend_from_slice(&[0xff, marker]);
    out.extend_from_slice(&((payload.len() + 2) as u16).to_be_bytes());
    out.extend_from_slice(payload);
}

fn soi_and_quantisation(factor: u8) -> Vec<u8> {
    let mut out = vec![0xff, 0xd8];
    segment_dqt(&mut out, factor);
    out
}

fn segment_dqt(out: &mut Vec<u8>, factor: u8) {
    let mut dqt = vec![0u8];
    dqt.extend([factor; 64]);
    segment(out, 0xdb, &dqt);
}

fn gray_header(width: u16, height: u16, ac_symbol: u8) -> Vec<u8> {
    let mut out = soi_and_quantisation(1);
    let mut frame = vec![8];
    frame.extend(height.to_be_bytes());
    frame.extend(width.to_be_bytes());
    frame.extend([1, 1, 0x11, 0]);
    segment(&mut out, 0xc2, &frame);
    let mut dc = vec![0x00, 0, 0, 0, 12, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
    dc.extend(0..12u8);
    segment(&mut out, 0xc4, &dc);
    segment(&mut out, 0xc4, &[0x10, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, ac_symbol]);
    out
}

fn scan(out: &mut Vec<u8>, (ss, se, ah, al): (u8, u8, u8, u8), data: &[u8]) {
    segment(out, 0xda, &[1, 1, 0x00, ss, se, ah << 4 | al]);
    out.extend_from_slice(data);
}

fn finish(mut out: Vec<u8>) -> Vec<u8> {
    out.extend_from_slice(&[0xff, 0xd9]);
    out
}

#[test]
fn the_number_of_scans_is_bounded() {
    // A 64x64 image has 64 blocks; each of these scans is one EOB0 symbol per block (a zero bit) over one band
    // position, so the scan limit is what stops a long run of them, not the work budget.
    let stream = |count: usize| {
        let mut out = gray_header(64, 64, 0x00);
        scan(&mut out, (0, 0, 0, 0), &[0; 32]); // size 0 (four bits) for each of the 64 blocks: a DC first scan
        for _ in 1..count {
            scan(&mut out, (1, 1, 0, 0), &[0; 8]);
        }
        finish(out)
    };
    let image = jpeg::decode(&stream(256), SIDE, PIXELS).unwrap();
    assert_eq!(image.get_pixel(5, 5).0, [128, 128, 128, 255]);
    assert_eq!(jpeg::decode(&stream(257), SIDE, PIXELS).unwrap_err(), "JPEG has too many scans");
}

#[test]
fn end_of_band_scans_cannot_burn_unbounded_work() {
    // 4096x4096: 262,144 blocks. Each scan is a refinement of bands 1..=63 coded as sixteen EOB runs of 16,384
    // blocks (symbol 0xE0 then fourteen zero bits), 30 bytes of entropy data that make the decoder walk 16.5 million
    // coefficients. Under the work budget (sixteen passes over every coefficient) the 17th scan is refused.
    let mut out = gray_header(4096, 4096, 0xe0);
    for _ in 0..100 {
        scan(&mut out, (1, 63, 1, 0), &[0; 30]);
    }
    assert_eq!(jpeg::decode(&finish(out), SIDE, PIXELS).unwrap_err(), "JPEG has too many scans");
}

#[test]
fn refining_a_band_that_never_had_a_first_scan_decodes_as_zeros() {
    // libjpeg starts every coefficient at zero, so a refinement of an unseen band adds to zero. Same here.
    let mut out = gray_header(8, 8, 0x00);
    scan(&mut out, (0, 0, 0, 0), &[0x0f]); // size 0: DC difference 0
    scan(&mut out, (1, 63, 1, 0), &[0x00]); // EOB0 for the only block
    let image = jpeg::decode(&finish(out), SIDE, PIXELS).unwrap();
    assert!(image.pixels().all(|p| p.0 == [128, 128, 128, 255]));
}

#[test]
fn a_component_without_a_dc_first_scan_is_refused() {
    // Three components, but the only scan is the DC first scan of the first one: the others would decode as flat
    // zeros and fabricate their channels.
    let mut out = soi_and_quantisation(1);
    segment(&mut out, 0xc2, &[8, 0, 8, 0, 8, 3, 1, 0x11, 0, 2, 0x11, 0, 3, 0x11, 0]);
    let mut dc = vec![0x00, 0, 0, 0, 12, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
    dc.extend(0..12u8);
    segment(&mut out, 0xc4, &dc);
    scan(&mut out, (0, 0, 0, 0), &[0x0f]);
    let message = "progressive JPEG has no DC scan for a component";
    assert_eq!(jpeg::decode(&finish(out.clone()), SIDE, PIXELS).unwrap_err(), message);
    // A DC refinement alone does not count as the first scan either.
    let mut refined = out[..out.len() - 11].to_vec();
    scan(&mut refined, (0, 0, 1, 0), &[0x0f]);
    assert_eq!(jpeg::decode(&finish(refined), SIDE, PIXELS).unwrap_err(), message);
}

#[test]
fn a_quantisation_table_is_bound_when_the_component_first_scans() {
    // DC coefficient 64 (size 7, magnitude 1000000b) over all-ones quantisation is +8 levels. A table redefined to
    // 8s after the first scan must not scale it again: the decoder keeps the table seen at the first scan.
    let mut out = gray_header(8, 8, 0x00);
    scan(&mut out, (0, 0, 0, 0), &[0x78, 0x1f]);
    segment_dqt(&mut out, 8);
    scan(&mut out, (1, 63, 0, 0), &[0x00]);
    let image = jpeg::decode(&finish(out), SIDE, PIXELS).unwrap();
    assert!(image.pixels().all(|p| p.0 == [136, 136, 136, 255]), "{:?}", image.get_pixel(0, 0));
}

#[test]
fn scans_naming_missing_tables_or_bad_bands_are_refused_by_cause() {
    let stream = bytes(GRAY_PROGRESSIVE);
    let scans: Vec<usize> = stream.windows(2).enumerate().filter(|(_, w)| *w == [0xff, 0xda]).map(|(at, _)| at).collect();
    let err = |stream: &[u8]| jpeg::decode(stream, SIDE, PIXELS).unwrap_err();
    // Scan header: length(2) components(1) id(1) tables(1) Ss(1) Se(1) Ah/Al(1), after the marker.
    let mut no_dc = stream.clone();
    no_dc[scans[0] + 6] = 0x30;
    assert_eq!(err(&no_dc), "JPEG DC Huffman table is missing");
    let mut no_ac = stream.clone();
    no_ac[scans[1] + 6] = 0x03;
    assert_eq!(err(&no_ac), "JPEG AC Huffman table is missing");
    let mut backwards = stream.clone();
    backwards[scans[1] + 8] = 0; // Se below Ss
    assert_eq!(err(&backwards), "progressive JPEG scan has an invalid spectral band");
    let mut jump = stream.clone();
    jump[scans[1] + 9] = 0x40; // Ah 4, Al 0
    assert_eq!(err(&jump), "progressive JPEG scan has an invalid successive approximation");
}
