use flate2::{write::DeflateEncoder, Compression, Decompress, FlushDecompress, Status};
use std::collections::HashSet;
use std::io::Write;

const HEADER_LEN: usize = 58;
const TRAILER_LEN: usize = 2;
const MAX_CAFF_SIZE: usize = 256 * 1024 * 1024;
const MAX_XML_SIZE: usize = 64 * 1024 * 1024;
const MAX_ENTRIES: usize = 100_000;
const CAFF_VERSION: u32 = 45;
const ZIP_FLAGS: u16 = 0x0808;
const ZIP_METHOD_DEFLATE: u16 = 8;
const ZIP_LOCAL_HEADER: u32 = 0x0403_4b50;
const ZIP_DESCRIPTOR: &[u8; 4] = b"PK\x07\x08";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CaffEntry {
    pub name: String,
    pub alias: String,
    pub unknown: u32,
    pub offset: u32,
    pub size: u32,
    pub flags: [u8; 2],
    pub reserved: [u8; 8],
    offset_field: usize,
    size_field: usize,
}

#[derive(Clone, Debug)]
pub struct CaffDocument {
    raw: Vec<u8>,
    seed: u32,
    entries: Vec<CaffEntry>,
    main_index: usize,
    main_xml: Vec<u8>,
    zip_prefix: Vec<u8>,
}

impl CaffDocument {
    pub fn parse(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() > MAX_CAFF_SIZE {
            return Err("CAFF file exceeds the 256 MiB limit".into());
        }
        if bytes.len() < HEADER_LEN + TRAILER_LEN || bytes.get(..4) != Some(b"CAFF".as_slice()) {
            return Err("invalid or truncated CAFF header".into());
        }
        if read_be_u32(bytes, 4)? != CAFF_VERSION {
            return Err("unsupported CAFF version".into());
        }

        let seed = read_be_u32(bytes, 14)?;
        let count = (read_be_u32(bytes, 54)? ^ seed) as usize;
        if count == 0 || count > MAX_ENTRIES {
            return Err("CAFF entry count is outside the supported range".into());
        }

        let mask = seed as u8;
        let mut cursor = HEADER_LEN;
        let mut entries = Vec::with_capacity(count);
        let mut names = HashSet::with_capacity(count);
        for _ in 0..count {
            let name = take_name(bytes, &mut cursor, mask, false)?;
            let alias = take_name(bytes, &mut cursor, mask, true)?;
            if !names.insert(name.clone()) {
                return Err(format!("duplicate CAFF entry name: {name}"));
            }

            let unknown = read_be_u32(bytes, cursor)? ^ seed;
            cursor = cursor.checked_add(4).ok_or("CAFF table offset overflow")?;
            let offset_field = cursor;
            let offset = read_be_u32(bytes, cursor)? ^ seed;
            cursor = cursor.checked_add(4).ok_or("CAFF table offset overflow")?;
            let size_field = cursor;
            let size = read_be_u32(bytes, cursor)? ^ seed;
            cursor = cursor.checked_add(4).ok_or("CAFF table offset overflow")?;

            let encoded_flags = bytes
                .get(cursor..cursor.checked_add(2).ok_or("CAFF table offset overflow")?)
                .ok_or("truncated CAFF entry flags")?;
            let flags = [encoded_flags[0] ^ mask, encoded_flags[1] ^ mask];
            cursor += 2;
            let encoded_reserved = bytes
                .get(cursor..cursor.checked_add(8).ok_or("CAFF table offset overflow")?)
                .ok_or("truncated CAFF entry reserved bytes")?;
            let mut reserved = [0; 8];
            for (decoded, encoded) in reserved.iter_mut().zip(encoded_reserved) {
                *decoded = *encoded ^ mask;
            }
            cursor += 8;

            entries.push(CaffEntry {
                name,
                alias,
                unknown,
                offset,
                size,
                flags,
                reserved,
                offset_field,
                size_field,
            });
        }

        let table_end = cursor;
        let trailer_start = bytes.len() - TRAILER_LEN;
        let mut ranges = Vec::with_capacity(count);
        for entry in &entries {
            let start =
                usize::try_from(entry.offset).map_err(|_| "CAFF payload offset overflow")?;
            let size = usize::try_from(entry.size).map_err(|_| "CAFF payload size overflow")?;
            let end = start
                .checked_add(size)
                .ok_or("CAFF payload range overflow")?;
            if start < table_end || end > trailer_start {
                return Err(format!(
                    "CAFF payload range is outside the data area: {}",
                    entry.name
                ));
            }
            ranges.push((start, end, entry.name.as_str()));
        }
        ranges.sort_unstable_by_key(|(start, end, _)| (*start, *end));
        for pair in ranges.windows(2) {
            if pair[0].1 > pair[1].0 {
                return Err(format!(
                    "overlapping CAFF payloads: {} and {}",
                    pair[0].2, pair[1].2
                ));
            }
        }

        let main_index = entries
            .iter()
            .position(|entry| entry.name == "main.xml")
            .ok_or("CAFF is missing main.xml")?;
        if entries[main_index].flags != [0x01, 0x21] {
            return Err("unsupported CAFF main.xml entry type".into());
        }
        let main = &entries[main_index];
        let main_start = main.offset as usize;
        let main_end = main_start + main.size as usize;
        let decoded = xor_payload(&bytes[main_start..main_end], mask);
        let (main_xml, zip_prefix) = decode_main_xml(&decoded)?;

        Ok(Self {
            raw: bytes.to_vec(),
            seed,
            entries,
            main_index,
            main_xml,
            zip_prefix,
        })
    }

    pub fn entries(&self) -> &[CaffEntry] {
        &self.entries
    }

    pub fn entry_data(&self, name: &str) -> Option<Vec<u8>> {
        let entry = self.entries.iter().find(|entry| entry.name == name)?;
        let start = entry.offset as usize;
        let end = start + entry.size as usize;
        Some(xor_payload(&self.raw[start..end], self.seed as u8))
    }

    pub fn main_xml(&self) -> Vec<u8> {
        self.main_xml.clone()
    }

    pub fn encode(&self) -> Vec<u8> {
        self.raw.clone()
    }

    pub fn replace_main_xml(&self, xml: &[u8]) -> Result<Vec<u8>, String> {
        if xml.len() > MAX_XML_SIZE {
            return Err("main.xml exceeds the 64 MiB limit".into());
        }
        if xml == self.main_xml {
            return Ok(self.raw.clone());
        }

        let zip_payload = encode_main_xml(&self.zip_prefix, xml)?;
        self.replace_encoded_entries(&std::collections::HashMap::from([
            (self.main_index, xor_payload(&zip_payload, self.seed as u8)),
        ]))
    }

    pub(crate) fn replace_entries(&self, replacements: &[(&str, &[u8])]) -> Result<Vec<u8>, String> {
        let mut encoded = std::collections::HashMap::new();
        for (name, bytes) in replacements {
            let index = self.entries.iter().position(|entry| entry.name == *name)
                .ok_or_else(|| format!("CAFF entry {name} was not found"))?;
            if index == self.main_index {
                return Err("main.xml must use the XML writer".into());
            }
            if encoded.insert(index, xor_payload(bytes, self.seed as u8)).is_some() {
                return Err(format!("CAFF entry {name} was replaced more than once"));
            }
        }
        self.replace_encoded_entries(&encoded)
    }

    fn replace_encoded_entries(&self, replacements: &std::collections::HashMap<usize, Vec<u8>>) -> Result<Vec<u8>, String> {
        let old_size: usize = replacements.keys().map(|index| self.entries[*index].size as usize).sum();
        let new_size = replacements.values().try_fold(0usize, |total, bytes| total.checked_add(bytes.len()))
            .ok_or("CAFF output size overflow")?;
        let output_size = self
            .raw
            .len()
            .checked_sub(old_size)
            .and_then(|size| size.checked_add(new_size))
            .ok_or("CAFF output size overflow")?;
        if output_size > MAX_CAFF_SIZE {
            return Err("rewritten CAFF file exceeds the 256 MiB limit".into());
        }
        let mut ordered: Vec<_> = self.entries.iter().enumerate().collect();
        ordered.sort_unstable_by_key(|(_, entry)| (entry.offset, entry.size));
        let first_offset = ordered[0].1.offset as usize;
        let mut output = self.raw[..first_offset].to_vec();
        let mut old_cursor = first_offset;
        for (index, entry) in ordered {
            let start = entry.offset as usize;
            let end = start + entry.size as usize;
            output.extend_from_slice(&self.raw[old_cursor..start]);

            let new_offset = u32::try_from(output.len())
                .map_err(|_| "rewritten CAFF payload offset exceeds the 32-bit limit")?;
            write_be_u32_xor(&mut output, entry.offset_field, new_offset, self.seed)?;

            let payload = if let Some(replacement) = replacements.get(&index) {
                let size = u32::try_from(replacement.len()).map_err(|_| "rewritten CAFF entry exceeds the 32-bit size limit")?;
                write_be_u32_xor(&mut output, entry.size_field, size, self.seed)?;
                replacement.as_slice()
            } else {
                &self.raw[start..end]
            };
            output.extend_from_slice(payload);
            old_cursor = end;
        }
        output.extend_from_slice(&self.raw[old_cursor..]);
        if output.len() != output_size {
            return Err("rewritten CAFF size did not match its payload layout".into());
        }
        Ok(output)
    }
}

fn take_name(
    bytes: &[u8],
    cursor: &mut usize,
    mask: u8,
    allow_empty: bool,
) -> Result<String, String> {
    let length = (*bytes
        .get(*cursor)
        .ok_or("truncated CAFF entry name length")?
        ^ mask) as usize;
    *cursor = cursor.checked_add(1).ok_or("CAFF table offset overflow")?;
    if !allow_empty && length == 0 {
        return Err("CAFF entry name is empty".into());
    }
    let end = cursor
        .checked_add(length)
        .ok_or("CAFF name length overflow")?;
    let encoded = bytes.get(*cursor..end).ok_or("truncated CAFF entry name")?;
    let decoded = encoded.iter().map(|byte| byte ^ mask).collect();
    *cursor = end;
    String::from_utf8(decoded).map_err(|_| "CAFF entry name is not UTF-8".into())
}

fn read_be_u32(bytes: &[u8], offset: usize) -> Result<u32, String> {
    let end = offset.checked_add(4).ok_or("CAFF offset overflow")?;
    let raw = bytes
        .get(offset..end)
        .ok_or("truncated CAFF 32-bit field")?;
    Ok(u32::from_be_bytes(
        raw.try_into().map_err(|_| "truncated CAFF 32-bit field")?,
    ))
}

fn write_be_u32_xor(bytes: &mut [u8], offset: usize, value: u32, seed: u32) -> Result<(), String> {
    let end = offset.checked_add(4).ok_or("CAFF output offset overflow")?;
    let raw = bytes
        .get_mut(offset..end)
        .ok_or("CAFF output table field is outside the header")?;
    raw.copy_from_slice(&(value ^ seed).to_be_bytes());
    Ok(())
}

fn xor_payload(bytes: &[u8], mask: u8) -> Vec<u8> {
    bytes.iter().map(|byte| byte ^ mask).collect()
}

fn decode_main_xml(payload: &[u8]) -> Result<(Vec<u8>, Vec<u8>), String> {
    if payload.len() < 30 + 8 + 16 {
        return Err("truncated main.xml ZIP local entry".into());
    }
    if read_le_u32(payload, 0)? != ZIP_LOCAL_HEADER {
        return Err("main.xml is missing its ZIP local header".into());
    }
    if read_le_u16(payload, 6)? & 1 != 0 {
        return Err("encrypted main.xml ZIP entries are unsupported".into());
    }
    let version = read_le_u16(payload, 4)?;
    let flags = read_le_u16(payload, 6)?;
    let method = read_le_u16(payload, 8)?;
    if version != 20 || flags != ZIP_FLAGS || method != ZIP_METHOD_DEFLATE {
        return Err("unsupported main.xml ZIP header profile".into());
    }
    if read_le_u32(payload, 14)? != 0
        || read_le_u32(payload, 18)? != 0
        || read_le_u32(payload, 22)? != 0
    {
        return Err("main.xml ZIP local header must use a data descriptor".into());
    }
    let name_len = read_le_u16(payload, 26)? as usize;
    let extra_len = read_le_u16(payload, 28)? as usize;
    let name_start = 30usize;
    let name_end = name_start
        .checked_add(name_len)
        .ok_or("ZIP name length overflow")?;
    let data_start = name_end
        .checked_add(extra_len)
        .ok_or("ZIP extra length overflow")?;
    if payload.get(name_start..name_end) != Some(b"contents".as_slice()) || extra_len != 0 {
        return Err("unsupported main.xml ZIP name or extra field".into());
    }
    let descriptor_start = payload
        .len()
        .checked_sub(16)
        .ok_or("truncated ZIP data descriptor")?;
    if data_start > descriptor_start {
        return Err("truncated main.xml ZIP deflate stream".into());
    }
    if payload.get(descriptor_start..descriptor_start + 4) != Some(ZIP_DESCRIPTOR.as_slice()) {
        return Err("unsupported main.xml ZIP data descriptor".into());
    }
    let expected_crc = read_le_u32(payload, descriptor_start + 4)?;
    let expected_compressed = read_le_u32(payload, descriptor_start + 8)? as usize;
    let expected_uncompressed = read_le_u32(payload, descriptor_start + 12)? as usize;
    let compressed = &payload[data_start..descriptor_start];
    if compressed.len() != expected_compressed {
        return Err("main.xml ZIP compressed size does not match its data descriptor".into());
    }
    if expected_uncompressed > MAX_XML_SIZE {
        return Err("main.xml exceeds the 64 MiB limit".into());
    }
    let xml = inflate_bounded(compressed)?;
    if xml.len() != expected_uncompressed {
        return Err("main.xml ZIP uncompressed size does not match its data descriptor".into());
    }
    if crc32fast::hash(&xml) != expected_crc {
        return Err("main.xml ZIP CRC does not match its data descriptor".into());
    }
    Ok((xml, payload[..data_start].to_vec()))
}

fn inflate_bounded(compressed: &[u8]) -> Result<Vec<u8>, String> {
    let mut decoder = Decompress::new(false);
    let mut output = Vec::new();
    loop {
        let input_offset =
            usize::try_from(decoder.total_in()).map_err(|_| "deflate input offset overflow")?;
        if input_offset > compressed.len() {
            return Err("invalid main.xml ZIP deflate input offset".into());
        }
        let output_before = decoder.total_out();
        let mut chunk = [0u8; 8192];
        let chunk_len = (MAX_XML_SIZE - output.len() + 1).min(chunk.len());
        let status = decoder
            .decompress(
                &compressed[input_offset..],
                &mut chunk[..chunk_len],
                FlushDecompress::None,
            )
            .map_err(|error| format!("invalid main.xml ZIP deflate stream: {error}"))?;
        let consumed =
            usize::try_from(decoder.total_in()).map_err(|_| "deflate input offset overflow")?;
        let produced = usize::try_from(decoder.total_out() - output_before)
            .map_err(|_| "deflate output size overflow")?;
        if produced > MAX_XML_SIZE - output.len() {
            return Err("main.xml exceeds the 64 MiB limit".into());
        }
        output.extend_from_slice(&chunk[..produced]);
        if status == Status::StreamEnd {
            if consumed != compressed.len() {
                return Err("trailing bytes after the main.xml ZIP deflate stream".into());
            }
            return Ok(output);
        }
        if consumed == input_offset && produced == 0 {
            return Err("truncated main.xml ZIP deflate stream".into());
        }
    }
}

fn encode_main_xml(prefix: &[u8], xml: &[u8]) -> Result<Vec<u8>, String> {
    let mut encoder = DeflateEncoder::new(Vec::new(), Compression::default());
    encoder
        .write_all(xml)
        .map_err(|error| format!("failed to compress main.xml: {error}"))?;
    let compressed = encoder
        .finish()
        .map_err(|error| format!("failed to finish main.xml compression: {error}"))?;
    let compressed_len = u32::try_from(compressed.len())
        .map_err(|_| "compressed main.xml exceeds the ZIP size limit")?;
    let uncompressed_len =
        u32::try_from(xml.len()).map_err(|_| "main.xml exceeds the ZIP size limit")?;
    let mut payload = Vec::with_capacity(prefix.len() + compressed.len() + 16);
    payload.extend_from_slice(prefix);
    payload.extend_from_slice(&compressed);
    payload.extend_from_slice(ZIP_DESCRIPTOR);
    payload.extend_from_slice(&crc32fast::hash(xml).to_le_bytes());
    payload.extend_from_slice(&compressed_len.to_le_bytes());
    payload.extend_from_slice(&uncompressed_len.to_le_bytes());
    Ok(payload)
}

fn read_le_u16(bytes: &[u8], offset: usize) -> Result<u16, String> {
    let end = offset.checked_add(2).ok_or("ZIP offset overflow")?;
    let raw = bytes.get(offset..end).ok_or("truncated ZIP 16-bit field")?;
    Ok(u16::from_le_bytes(
        raw.try_into().map_err(|_| "truncated ZIP 16-bit field")?,
    ))
}

fn read_le_u32(bytes: &[u8], offset: usize) -> Result<u32, String> {
    let end = offset.checked_add(4).ok_or("ZIP offset overflow")?;
    let raw = bytes.get(offset..end).ok_or("truncated ZIP 32-bit field")?;
    Ok(u32::from_le_bytes(
        raw.try_into().map_err(|_| "truncated ZIP 32-bit field")?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestEntry {
        name: &'static str,
        alias: &'static str,
        unknown: u32,
        flags: [u8; 2],
        reserved: [u8; 8],
        payload: Vec<u8>,
    }

    struct BuiltCaff {
        bytes: Vec<u8>,
        fields: Vec<(usize, usize)>,
        offsets: Vec<usize>,
    }

    fn test_entry(
        name: &'static str,
        alias: &'static str,
        flags: [u8; 2],
        payload: Vec<u8>,
    ) -> TestEntry {
        TestEntry {
            name,
            alias,
            unknown: 0x1234_5678,
            flags,
            reserved: [1, 2, 3, 4, 5, 6, 7, 8],
            payload,
        }
    }

    fn build_caff(entries: &[TestEntry], seed: u32) -> BuiltCaff {
        let mask = seed as u8;
        let mut bytes = vec![0u8; HEADER_LEN];
        bytes[..4].copy_from_slice(b"CAFF");
        bytes[4..8].copy_from_slice(&CAFF_VERSION.to_be_bytes());
        bytes[8..14].copy_from_slice(&[9, 8, 7, 6, 5, 4]);
        bytes[14..18].copy_from_slice(&seed.to_be_bytes());
        for (index, byte) in bytes[18..54].iter_mut().enumerate() {
            *byte = index as u8 ^ 0x5a;
        }
        bytes[54..58].copy_from_slice(&(entries.len() as u32 ^ seed).to_be_bytes());

        let mut fields = Vec::new();
        for entry in entries {
            bytes.push(entry.name.len() as u8 ^ mask);
            bytes.extend(entry.name.as_bytes().iter().map(|byte| byte ^ mask));
            bytes.push(entry.alias.len() as u8 ^ mask);
            bytes.extend(entry.alias.as_bytes().iter().map(|byte| byte ^ mask));
            bytes.extend_from_slice(&(entry.unknown ^ seed).to_be_bytes());
            let offset_field = bytes.len();
            bytes.extend_from_slice(&seed.to_be_bytes());
            let size_field = bytes.len();
            bytes.extend_from_slice(&seed.to_be_bytes());
            bytes.extend(entry.flags.iter().map(|byte| byte ^ mask));
            bytes.extend(entry.reserved.iter().map(|byte| byte ^ mask));
            fields.push((offset_field, size_field));
        }

        let mut offsets = Vec::new();
        for (index, entry) in entries.iter().enumerate() {
            let offset = bytes.len();
            offsets.push(offset);
            bytes[fields[index].0..fields[index].0 + 4]
                .copy_from_slice(&((offset as u32 ^ seed).to_be_bytes()));
            bytes[fields[index].1..fields[index].1 + 4]
                .copy_from_slice(&((entry.payload.len() as u32 ^ seed).to_be_bytes()));
            bytes.extend(entry.payload.iter().map(|byte| byte ^ mask));
            if index + 1 < entries.len() {
                bytes.extend_from_slice(b"GAP!");
            }
        }
        bytes.extend_from_slice(b"bc");
        BuiltCaff {
            bytes,
            fields,
            offsets,
        }
    }

    fn test_zip(xml: &[u8]) -> Vec<u8> {
        let mut prefix = Vec::new();
        prefix.extend_from_slice(&ZIP_LOCAL_HEADER.to_le_bytes());
        prefix.extend_from_slice(&20u16.to_le_bytes());
        prefix.extend_from_slice(&ZIP_FLAGS.to_le_bytes());
        prefix.extend_from_slice(&ZIP_METHOD_DEFLATE.to_le_bytes());
        prefix.extend_from_slice(&[0; 4]);
        prefix.extend_from_slice(&[0; 12]);
        prefix.extend_from_slice(&(8u16).to_le_bytes());
        prefix.extend_from_slice(&0u16.to_le_bytes());
        prefix.extend_from_slice(b"contents");
        encode_main_xml(&prefix, xml).unwrap()
    }

    fn sample_caff(xml: &[u8]) -> BuiltCaff {
        build_caff(
            &[
                test_entry("main.xml", "main_xml", [1, 0x21], test_zip(xml)),
                test_entry("asset.bin", "", [1, 0x10], vec![0, 1, 2, 0xff]),
            ],
            0x562e_099f,
        )
    }

    fn patch_caff_u32(bytes: &mut [u8], field: usize, value: u32, seed: u32) {
        bytes[field..field + 4].copy_from_slice(&(value ^ seed).to_be_bytes());
    }

    #[test]
    fn authoring_inspection_preserves_pending_session_on_success_and_failure() {
        let sequence = crate::TEMP_SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "vsoul-authoring-{}-{sequence}.cmo3",
            std::process::id()
        ));
        std::fs::write(&path, sample_caff(b"<root/>").bytes).unwrap();
        let mut session = crate::Session::new();
        session.deform_cursor = Some(vec![]);
        let before = session.snapshot();
        let request = serde_json::json!({"op":"live2d.authoring.inspect", "path":path});
        assert_eq!(session.handle_request(&request)["succeeded"], true);
        assert_eq!(session.snapshot(), before);
        std::fs::write(&path, b"invalid").unwrap();
        assert_eq!(session.handle_request(&request)["succeeded"], false);
        assert_eq!(session.snapshot(), before);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn native_source_read_preserves_pending_session_on_success_and_failure() {
        let sequence = crate::TEMP_SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "vsoul-native-source-{}-{sequence}.cmo3",
            std::process::id()
        ));
        let xml = br#"<root><CModelSource>
            <CImageCanvas xs.n="canvas"><i xs.n="pixelWidth">100</i><i xs.n="pixelHeight">200</i></CImageCanvas>
            <CParameterSourceSet xs.n="parameterSourceSet"><carray_list xs.n="_sources" count="0"/></CParameterSourceSet>
            <CDrawableSourceSet xs.n="drawableSourceSet"><carray_list xs.n="_sources" count="0"/></CDrawableSourceSet>
            <CDeformerSourceSet xs.n="deformerSourceSet"><carray_list xs.n="_sources" count="0"/></CDeformerSourceSet>
            <CPartSourceSet xs.n="partSourceSet"><carray_list xs.n="_sources" count="1"><CPartSource xs.id="root-part"><CPartGuid xs.n="guid" uuid="root-guid"/><CPartId xs.n="id" idstr="root"/><carray_list xs.n="keyforms" count="0"/></CPartSource></carray_list></CPartSourceSet>
            <CPartSource xs.n="rootPart" xs.ref="root-part"/>
        </CModelSource></root>"#;
        std::fs::write(&path, sample_caff(xml).bytes).unwrap();
        let mut session = crate::Session::new();
        session.deform_cursor = Some(vec![]);
        let before = session.snapshot();
        let request = serde_json::json!({"op":"live2d.authoring.read", "path":path});
        let response = session.handle_request(&request);
        assert_eq!(response["succeeded"], true, "{response}");
        assert_eq!(response["result"]["canvas"], serde_json::json!([100, 200]));
        assert_eq!(session.snapshot(), before);
        let weights = serde_json::json!({"op":"live2d.authoring.linear_weights", "path":path});
        let response = session.handle_request(&weights);
        assert_eq!(response["succeeded"], true, "{response}");
        assert_eq!(
            response["result"]["sources"][0]["weights"],
            serde_json::Value::Null
        );
        assert_eq!(session.snapshot(), before);
        let invalid_weights = serde_json::json!({"op":"live2d.authoring.linear_weights", "path":path, "sourceGuid":"missing"});
        assert_eq!(session.handle_request(&invalid_weights)["succeeded"], false);
        assert_eq!(session.snapshot(), before);
        let forms = serde_json::json!({"op":"live2d.authoring.primary_forms", "path":path});
        let response = session.handle_request(&forms);
        assert_eq!(response["succeeded"], true, "{response}");
        assert_eq!(
            response["result"]["sources"][0]["evaluatedForms"],
            serde_json::Value::Null
        );
        assert_eq!(session.snapshot(), before);
        let invalid_forms = serde_json::json!({"op":"live2d.authoring.primary_forms", "path":path, "parameterValues":[]});
        assert_eq!(session.handle_request(&invalid_forms)["succeeded"], false);
        assert_eq!(session.snapshot(), before);
        std::fs::write(&path, sample_caff(b"<root/>").bytes).unwrap();
        assert_eq!(session.handle_request(&request)["succeeded"], false);
        assert_eq!(session.handle_request(&weights)["succeeded"], false);
        assert_eq!(session.handle_request(&forms)["succeeded"], false);
        assert_eq!(session.snapshot(), before);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn rewrites_empty_entry_sharing_main_payload_start_without_panicking() {
        let seed = 0x562e_099f;
        let mut built = build_caff(
            &[
                test_entry("main.xml", "main_xml", [1, 0x21], test_zip(b"<root/>")),
                test_entry("empty.bin", "", [1, 0x10], vec![]),
            ],
            seed,
        );
        patch_caff_u32(
            &mut built.bytes,
            built.fields[1].0,
            built.offsets[0] as u32,
            seed,
        );
        let document = CaffDocument::parse(&built.bytes).unwrap();
        let bytes = document.replace_main_xml(b"<root>changed</root>").unwrap();
        let reopened = CaffDocument::parse(&bytes).unwrap();
        assert_eq!(reopened.main_xml(), b"<root>changed</root>");
        assert_eq!(reopened.entry_data("empty.bin"), Some(vec![]));
        assert_eq!(reopened.entries()[0].offset, reopened.entries()[1].offset);
    }

    #[test]
    fn unchanged_encode_and_main_xml_replacement_preserve_other_data() {
        let old_xml = b"<root>old</root>";
        let built = sample_caff(old_xml);
        let document = CaffDocument::parse(&built.bytes).unwrap();
        assert_eq!(document.encode(), built.bytes);
        assert_eq!(document.main_xml(), old_xml.to_vec());
        assert_eq!(document.entry_data("asset.bin"), Some(vec![0, 1, 2, 0xff]));

        let replaced_xml = vec![b'x'; 10_000];
        let replaced = document.replace_main_xml(&replaced_xml).unwrap();
        let parsed = CaffDocument::parse(&replaced).unwrap();
        assert_eq!(parsed.main_xml(), replaced_xml);
        assert_eq!(parsed.entry_data("asset.bin"), Some(vec![0, 1, 2, 0xff]));
        assert_eq!(parsed.entries()[0].alias, "main_xml");
        assert_eq!(parsed.entries()[0].unknown, 0x1234_5678);
        assert_eq!(parsed.entries()[0].flags, [1, 0x21]);
        assert_eq!(parsed.entries()[0].reserved, [1, 2, 3, 4, 5, 6, 7, 8]);
        assert_eq!(
            parsed.entries()[1].offset as usize,
            built.offsets[1]
                + (parsed.entries()[0].size as usize - document.entries()[0].size as usize)
        );
        let main_end = parsed.entries()[0].offset as usize + parsed.entries()[0].size as usize;
        assert_eq!(&replaced[main_end..main_end + 4], b"GAP!");
        assert_eq!(&replaced[8..14], &built.bytes[8..14]);
        assert_eq!(&replaced[18..54], &built.bytes[18..54]);
        assert_eq!(&replaced[replaced.len() - 2..], b"bc");
    }

    #[test]
    fn replacing_payloads_preserves_xml_metadata_gaps_and_untouched_entries() {
        let mut built = build_caff(&[
            test_entry("before.png", "", [1, 0x10], vec![1, 2]),
            test_entry("main.xml", "main_xml", [1, 0x21], test_zip(b"<root/>")),
            test_entry("after.png", "", [1, 0x10], vec![3, 4, 5]),
            test_entry("opaque.bin", "", [1, 0x10], vec![6, 7]),
        ], 7);
        patch_caff_u32(&mut built.bytes, built.fields[0].0, built.offsets[2] as u32, 7);
        patch_caff_u32(&mut built.bytes, built.fields[0].1, 3, 7);
        patch_caff_u32(&mut built.bytes, built.fields[2].0, built.offsets[0] as u32, 7);
        patch_caff_u32(&mut built.bytes, built.fields[2].1, 2, 7);
        let original = CaffDocument::parse(&built.bytes).unwrap();
        let bytes = original.replace_entries(&[("before.png", &[8; 40]), ("after.png", &[9])]).unwrap();
        let changed = CaffDocument::parse(&bytes).unwrap();
        assert_eq!(changed.entry_data("before.png"), Some(vec![8; 40]));
        assert_eq!(changed.entry_data("after.png"), Some(vec![9]));
        assert_eq!(changed.entry_data("opaque.bin"), original.entry_data("opaque.bin"));
        assert_eq!(changed.main_xml(), original.main_xml());
        assert_eq!(&bytes[bytes.len() - 2..], &built.bytes[built.bytes.len() - 2..]);
        for (old, new) in original.entries().iter().zip(changed.entries()) {
            assert_eq!((&old.name, &old.alias, old.unknown, old.flags, old.reserved),
                (&new.name, &new.alias, new.unknown, new.flags, new.reserved));
        }
        let mut physical: Vec<_> = changed.entries().iter().collect();
        physical.sort_by_key(|entry| entry.offset);
        for entry in &physical[..physical.len() - 1] {
            let end = (entry.offset + entry.size) as usize;
            assert_eq!(&bytes[end..end + 4], b"GAP!");
        }
        assert_eq!(original.replace_entries(&[]).unwrap(), built.bytes);
        assert!(original.replace_entries(&[("missing.png", &[1])]).is_err());
        assert!(original.replace_entries(&[("main.xml", &[1])]).is_err());
        assert!(original.replace_entries(&[("before.png", &[1]), ("before.png", &[2])]).is_err());
        let before = original.entry_data("before.png").unwrap();
        let after = original.entry_data("after.png").unwrap();
        let restored = changed.replace_entries(&[("before.png", &before), ("after.png", &after)]).unwrap();
        assert_eq!(restored, built.bytes);
    }

    #[test]
    fn rejects_duplicate_names_ranges_limits_and_truncation() {
        let xml = b"<root/>";
        let duplicate = build_caff(
            &[
                test_entry("same", "", [1, 0x10], vec![1]),
                test_entry("same", "", [1, 0x10], vec![2]),
                test_entry("main.xml", "main_xml", [1, 0x21], test_zip(xml)),
            ],
            7,
        );
        assert!(CaffDocument::parse(&duplicate.bytes)
            .unwrap_err()
            .contains("duplicate"));

        let valid = sample_caff(xml);
        let mut table_overlap = valid.bytes.clone();
        patch_caff_u32(
            &mut table_overlap,
            valid.fields[0].0,
            HEADER_LEN as u32,
            0x562e_099f,
        );
        assert!(CaffDocument::parse(&table_overlap)
            .unwrap_err()
            .contains("outside"));

        let mut payload_overlap = valid.bytes.clone();
        patch_caff_u32(
            &mut payload_overlap,
            valid.fields[1].0,
            valid.offsets[0] as u32,
            0x562e_099f,
        );
        assert!(CaffDocument::parse(&payload_overlap)
            .unwrap_err()
            .contains("overlapping"));

        let mut excessive_count = valid.bytes.clone();
        patch_caff_u32(
            &mut excessive_count,
            54,
            (MAX_ENTRIES + 1) as u32,
            0x562e_099f,
        );
        assert!(CaffDocument::parse(&excessive_count)
            .unwrap_err()
            .contains("entry count"));

        let mut truncated = valid.bytes;
        truncated.pop();
        assert!(CaffDocument::parse(&truncated).is_err());
    }

    #[test]
    fn rejects_bad_crc_deflate_zip_profile_and_xml_size_limit() {
        let seed = 0x1234_5678;
        let mut bad_crc_zip = test_zip(b"<root/>");
        let crc_offset = bad_crc_zip.len() - 12;
        bad_crc_zip[crc_offset] ^= 1;
        let bad_crc = build_caff(
            &[test_entry("main.xml", "main_xml", [1, 0x21], bad_crc_zip)],
            seed,
        );
        assert!(CaffDocument::parse(&bad_crc.bytes)
            .unwrap_err()
            .contains("CRC"));

        let truncated_zip = test_zip(b"<root/>");
        let descriptor = truncated_zip.len() - 16;
        let data_start = 38;
        let compressed_len = descriptor - data_start;
        let mut truncated_deflate = truncated_zip[..data_start + compressed_len / 2].to_vec();
        truncated_deflate.extend_from_slice(&truncated_zip[descriptor..]);
        let descriptor_start = truncated_deflate.len() - 16;
        truncated_deflate[descriptor_start + 8..descriptor_start + 12]
            .copy_from_slice(&((compressed_len / 2) as u32).to_le_bytes());
        let truncated_deflate = build_caff(
            &[test_entry(
                "main.xml",
                "main_xml",
                [1, 0x21],
                truncated_deflate,
            )],
            seed,
        );
        assert!(CaffDocument::parse(&truncated_deflate.bytes)
            .unwrap_err()
            .contains("deflate"));

        let mut bad_profile_zip = test_zip(b"<root/>");
        bad_profile_zip[6] ^= 1;
        let bad_profile = build_caff(
            &[test_entry(
                "main.xml",
                "main_xml",
                [1, 0x21],
                bad_profile_zip,
            )],
            seed,
        );
        assert!(CaffDocument::parse(&bad_profile.bytes)
            .unwrap_err()
            .contains("encrypted"));

        let mut oversized_zip = test_zip(b"<root/>");
        let descriptor = oversized_zip.len() - 16;
        oversized_zip[descriptor + 12..descriptor + 16]
            .copy_from_slice(&((MAX_XML_SIZE as u32) + 1).to_le_bytes());
        let oversized = build_caff(
            &[test_entry("main.xml", "main_xml", [1, 0x21], oversized_zip)],
            seed,
        );
        assert!(CaffDocument::parse(&oversized.bytes)
            .unwrap_err()
            .contains("64 MiB"));
    }
}
