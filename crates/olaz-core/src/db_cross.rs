//! Pure-Rust port of `db-cross-v4` (`ZDB4.0` backup archive decryptor + XZ extractor
//! and nested `TlvBox` `parseBinNet` parser).

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{Cursor, Read};
use std::path::{Component, Path, PathBuf};

use aes::Aes256;
use cbc::Decryptor;
use cipher::block_padding::NoPadding;
use cipher::{BlockDecryptMut, KeyIvInit};
use serde_json::{json, Map, Value};

use crate::fs_bridge::{BridgeResult, PlatformPaths};
use crate::wire;

type Aes256CbcDec = Decryptor<Aes256>;

const ZDB4_MAGIC: &[u8; 6] = b"ZDB4.0";
const SQLITE3_HEADER: &[u8; 16] = b"SQLite format 3\0";
const CHUNK_SIZE: usize = 65_536;
const AES_BLOCK_SIZE: usize = 16;
const MAX_ZDB4_FILES: u32 = 10_000;
const MAX_DECOMPRESSED_BYTES: usize = 1024 * 1024 * 1024; // 1 GiB safety cap

#[derive(Debug, Clone)]
struct Zdb4FileEntry {
    rel_path: PathBuf,
    raw_name: String,
    uncompressed_size: usize,
}

/// Derives the 32-byte AES-256 key for `ZDB4.0` archives from `private_key`
/// (`private_key.to_ascii_uppercase()` truncated/zero-padded to 32 ASCII bytes).
pub fn derive_zdb4_key(private_key: &str) -> [u8; 32] {
    let mut key = [0u8; 32];
    for (dst, src) in key.iter_mut().zip(private_key.as_bytes().iter().take(32)) {
        *dst = src.to_ascii_uppercase();
    }
    key
}

fn read_chunk_full<R: Read>(reader: &mut R, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut total = 0usize;
    while total < buf.len() {
        match reader.read(&mut buf[total..]) {
            Ok(0) => break,
            Ok(n) => total += n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }
    Ok(total)
}

fn decrypt_chunk_reset_iv(buf: &mut [u8], key: &[u8; 32]) {
    let aligned = (buf.len() / AES_BLOCK_SIZE) * AES_BLOCK_SIZE;
    if aligned > 0 {
        let iv = [0u8; AES_BLOCK_SIZE];
        let _ = Aes256CbcDec::new(key.into(), (&iv).into())
            .decrypt_padded_mut::<NoPadding>(&mut buf[..aligned]);
    }
}

fn decrypt_stream_chunked_64k<R: Read>(
    reader: &mut R,
    key: &[u8; 32],
) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    let mut chunk = vec![0u8; CHUNK_SIZE];
    loop {
        let n = read_chunk_full(reader, &mut chunk)
            .map_err(|e| format!("Failed to read input archive: {e}"))?;
        if n == 0 {
            break;
        }
        decrypt_chunk_reset_iv(&mut chunk[..n], key);
        out.extend_from_slice(&chunk[..n]);
    }
    Ok(out)
}

fn validate_rel_path(rel: &str) -> Result<PathBuf, &'static str> {
    if rel.is_empty()
        || rel.contains('\0')
        || rel.starts_with('/')
        || rel.starts_with('\\')
    {
        return Err("Unsafe or invalid entry path in ZDB4.0 archive");
    }
    if rel.split(['/', '\\']).any(|seg| seg == "..") {
        return Err("Path traversal detected in ZDB4.0 archive");
    }
    let mut clean = PathBuf::new();
    for comp in Path::new(rel).components() {
        match comp {
            Component::Normal(seg) if !seg.is_empty() => clean.push(seg),
            Component::CurDir => {}
            _ => return Err("Path traversal detected in ZDB4.0 archive"),
        }
    }
    if clean.as_os_str().is_empty() {
        return Err("Empty entry path in ZDB4.0 archive");
    }
    Ok(clean)
}

fn zdb4_err(code: i32, msg: impl Into<String>) -> Value {
    let s = msg.into();
    json!({
        "result": code,
        "inner_error": s,
        "error_message": s,
    })
}

fn decompress_xz_allow_aes_tail(
    xz_payload: &[u8],
    expected_uncompressed: usize,
) -> Result<Vec<u8>, String> {
    if xz_payload.is_empty() {
        return Err("Empty XZ payload in ZDB4.0 archive".to_string());
    }
    let mut decompressed = Vec::with_capacity(expected_uncompressed.min(16 * 1024 * 1024));
    let mut cursor = Cursor::new(xz_payload);
    match lzma_rs::xz_decompress(&mut cursor, &mut decompressed) {
        Ok(()) => Ok(decompressed),
        Err(lzma_rs::error::Error::XzError(msg))
            if msg.contains("Unexpected data after last XZ block") =>
        {
            let pos = cursor.position() as usize;
            let footer_verified =
                pos >= 12 && pos <= xz_payload.len() && &xz_payload[pos - 2..pos] == b"YZ";
            if footer_verified && decompressed.len() >= expected_uncompressed {
                Ok(decompressed)
            } else {
                Err(format!("XZ decompression failed: {msg}"))
            }
        }
        Err(e) => Err(format!("XZ decompression failed: {e}")),
    }
}

/// Decrypts and extracts a `ZDB4.0` encrypted SQLite backup archive into `output_path`.
///
/// Never panics; returns `{ result: 0, inner_error: "", error_message: "" }` on success
/// or `{ result: <neg>, inner_error: "...", error_message: "..." }` on error.
pub fn decompress_and_decrypt_db_v2(
    input_path: &Path,
    output_path: &Path,
    private_key: &str,
) -> Value {
    if input_path.as_os_str().is_empty() || output_path.as_os_str().is_empty() {
        return zdb4_err(-1, "Invalid args");
    }

    let mut file = match File::open(input_path) {
        Ok(f) => f,
        Err(e) => return zdb4_err(-1, format!("Cannot open input {}: {e}", input_path.display())),
    };

    let key = derive_zdb4_key(private_key);
    let mut plaintext = match decrypt_stream_chunked_64k(&mut file, &key) {
        Ok(pt) => pt,
        Err(e) => return zdb4_err(-1, e),
    };

    if plaintext.len() < 18 {
        return zdb4_err(-1, "Input too small");
    }

    // Support optional 4-byte outer prefix variant observed in legacy archives.
    if &plaintext[0..6] != ZDB4_MAGIC {
        if let Ok(raw) = std::fs::read(input_path) {
            if raw.len() > 22 {
                let mut cursor = Cursor::new(&raw[4..]);
                if let Ok(alt) = decrypt_stream_chunked_64k(&mut cursor, &key) {
                    if alt.len() >= 18 && &alt[0..6] == ZDB4_MAGIC {
                        plaintext = alt;
                    }
                }
            }
        }
    }

    if &plaintext[0..6] != ZDB4_MAGIC {
        return zdb4_err(-1, "Invalid ZDB4.0 Magic Header");
    }

    // [0..6] = "ZDB4.0", [6..14] = 8-byte meta, [14..18] = u32 BE file_count
    let file_count = u32::from_be_bytes([
        plaintext[14],
        plaintext[15],
        plaintext[16],
        plaintext[17],
    ]);
    if !(1..=MAX_ZDB4_FILES).contains(&file_count) {
        return zdb4_err(
            -2,
            format!("Invalid ZDB4.0 file count: {file_count} (expected 1..={MAX_ZDB4_FILES})"),
        );
    }

    let mut off = 18usize;
    let mut entries = Vec::with_capacity(file_count as usize);
    let mut total_expected = 0usize;

    for _ in 0..file_count {
        if off + 4 > plaintext.len() {
            return zdb4_err(-2, "Truncated file table");
        }
        let name_len = u32::from_be_bytes([
            plaintext[off],
            plaintext[off + 1],
            plaintext[off + 2],
            plaintext[off + 3],
        ]) as usize;
        off += 4;

        if name_len == 0 || name_len > 4096 || off + name_len + 4 > plaintext.len() {
            return zdb4_err(-2, "Truncated or invalid file table entry");
        }
        let raw_name = match std::str::from_utf8(&plaintext[off..off + name_len]) {
            Ok(s) => s.to_string(),
            Err(_) => return zdb4_err(-2, "Invalid UTF-8 filename in ZDB4.0 file table"),
        };
        off += name_len;

        let rel_path = match validate_rel_path(&raw_name) {
            Ok(p) => p,
            Err(msg) => return zdb4_err(-3, msg),
        };

        let uncompressed_size = u32::from_be_bytes([
            plaintext[off],
            plaintext[off + 1],
            plaintext[off + 2],
            plaintext[off + 3],
        ]) as usize;
        off += 4;

        total_expected = match total_expected.checked_add(uncompressed_size) {
            Some(sum) if sum <= MAX_DECOMPRESSED_BYTES => sum,
            _ => return zdb4_err(-4, "Uncompressed ZDB4.0 total size exceeds limit"),
        };

        entries.push(Zdb4FileEntry {
            rel_path,
            raw_name,
            uncompressed_size,
        });
    }

    let decompressed = match decompress_xz_allow_aes_tail(&plaintext[off..], total_expected) {
        Ok(buf) => buf,
        Err(msg) => return zdb4_err(-12, msg),
    };

    if decompressed.len() < total_expected {
        return zdb4_err(-13, "Not all files were extracted from XZ stream");
    }

    // Validate SQLite magic on every `.db` slice BEFORE writing any files to disk.
    let mut check_off = 0usize;
    for entry in &entries {
        let slice = &decompressed[check_off..check_off + entry.uncompressed_size];
        check_off += entry.uncompressed_size;
        if entry.raw_name.to_ascii_lowercase().ends_with(".db")
            && (slice.len() < SQLITE3_HEADER.len() || &slice[..16] != SQLITE3_HEADER)
        {
            return zdb4_err(
                -14,
                format!(
                    "Extracted .db file '{}' does not start with SQLite format 3 header",
                    entry.raw_name
                ),
            );
        }
    }

    if let Err(e) = std::fs::create_dir_all(output_path) {
        return zdb4_err(
            -10,
            format!("Failed to create output directory {}: {e}", output_path.display()),
        );
    }

    let mut slice_off = 0usize;
    for entry in &entries {
        let slice = &decompressed[slice_off..slice_off + entry.uncompressed_size];
        slice_off += entry.uncompressed_size;

        let dest = output_path.join(&entry.rel_path);
        if let Some(parent) = dest.parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                return zdb4_err(
                    -10,
                    format!("Failed to create parent directory {}: {e}", parent.display()),
                );
            }
        }
        if let Err(e) = std::fs::write(&dest, slice) {
            return zdb4_err(
                -11,
                format!("Failed to write output file {}: {e}", dest.display()),
            );
        }
    }

    json!({
        "result": 0,
        "inner_error": "",
        "error_message": "",
    })
}

/// Legacy V1 backup stub matching `db-cross-v4/src/main.cc`.
pub fn decompress_and_decrypt_db_v1(
    _input_path: &Path,
    _output_path: &Path,
    _private_key: &str,
) -> Value {
    json!({
        "result": -1,
        "err_message": "V1 format not fully implemented in this build",
    })
}

// ============================================================================
// Nested `TlvBox` (`parseBinNet`) parser
// ============================================================================

#[derive(Debug, Clone)]
struct ParsedTlv {
    entries: BTreeMap<u32, Vec<Vec<u8>>>,
    little_endian: bool,
    framed_u16: bool,
}

fn read_u16_le(p: &[u8]) -> u16 {
    u16::from_le_bytes([p[0], p[1]])
}

fn read_u32_le(p: &[u8]) -> u32 {
    u32::from_le_bytes([p[0], p[1], p[2], p[3]])
}

fn read_u32_be(p: &[u8]) -> u32 {
    u32::from_be_bytes([p[0], p[1], p[2], p[3]])
}

fn parse_tlv_u16_le_body(body: &[u8]) -> Option<BTreeMap<u32, Vec<Vec<u8>>>> {
    if body.is_empty() {
        return None;
    }
    let mut off = 0usize;
    let mut map: BTreeMap<u32, Vec<Vec<u8>>> = BTreeMap::new();
    while off + 6 <= body.len() {
        let tag = read_u16_le(&body[off..off + 2]) as u32;
        let len = read_u32_le(&body[off + 2..off + 6]) as usize;
        off += 6;
        if tag == 0 || tag > 0x0100 || len > body.len() - off {
            return None;
        }
        map.entry(tag)
            .or_default()
            .push(body[off..off + len].to_vec());
        off += len;
    }
    (off == body.len() && !map.is_empty()).then_some(map)
}

fn parse_tlv_u32_core(buf: &[u8], little_endian: bool) -> Result<BTreeMap<u32, Vec<Vec<u8>>>, &'static str> {
    if buf.is_empty() {
        return Err("Empty buffer");
    }
    if buf.len() < 8 {
        return Err("Buffer shorter than TLV header");
    }
    let mut off = 0usize;
    let mut map: BTreeMap<u32, Vec<Vec<u8>>> = BTreeMap::new();
    while off + 8 <= buf.len() {
        let key = if little_endian {
            read_u32_le(&buf[off..off + 4])
        } else {
            read_u32_be(&buf[off..off + 4])
        };
        let len = if little_endian {
            read_u32_le(&buf[off + 4..off + 8]) as usize
        } else {
            read_u32_be(&buf[off + 4..off + 8]) as usize
        };
        off += 8;
        if len > buf.len() - off {
            return Err("TLV length out of bounds");
        }
        map.entry(key)
            .or_default()
            .push(buf[off..off + len].to_vec());
        off += len;
    }
    if off != buf.len() && (buf.len() - off) > 8 {
        return Err("Trailing bytes");
    }
    if map.is_empty() {
        return Err("No TLV entries");
    }
    Ok(map)
}

fn score_bin_net_shape(values: &BTreeMap<u32, Vec<Vec<u8>>>) -> i32 {
    const EXPECTED_TOP: &[u32] = &[1, 2, 3, 4, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b];
    const EXPECTED_NESTED: &[u32] = &[
        0x1e, 0x1f, 0x20, 0x21, 0x22, 0x28, 0x29, 0x2a, 0x2b, 0x2c, 0x2d, 0x2e, 0x2f, 0x30, 0x31,
        0x32, 0x33, 0x34, 0x35, 0x36, 0x37, 0x40, 0x41, 0x42, 0x43, 0x44, 0x50, 0x51, 0x52, 0x53,
        0x54, 0x55, 0x56, 0x57, 0x58, 0x5a, 0x65, 0x66, 0x67, 0x68,
    ];
    let mut score = 0i32;
    for k in EXPECTED_TOP {
        if values.contains_key(k) {
            score += 6;
        }
    }
    for k in EXPECTED_NESTED {
        if values.contains_key(k) {
            score += 4;
        }
    }
    for &k in values.keys() {
        if (1..=0x80).contains(&k) {
            score += 1;
        }
        if k == 0 || k > 0x10000 {
            score -= 6;
        }
    }
    score
}

fn parse_tlv_any(buf: &[u8]) -> Result<ParsedTlv, String> {
    if buf.is_empty() {
        return Err("Empty BinNet".to_string());
    }

    // 1. Canonical Zalo `TlvBox`: `[u32 LE total_bytes]` header (`total_bytes == buf.len()` or
    //    `total_bytes + 4 == buf.len()`) + `[u16 LE tag][u32 LE len][val]` entries.
    if buf.len() >= 10 {
        let hdr_len = read_u32_le(&buf[0..4]) as usize;
        if hdr_len == buf.len() || hdr_len + 4 == buf.len() {
            if let Some(entries) = parse_tlv_u16_le_body(&buf[4..]) {
                return Ok(ParsedTlv {
                    entries,
                    little_endian: true,
                    framed_u16: true,
                });
            }
            if let Ok(entries) = parse_tlv_u32_core(&buf[4..], true) {
                if score_bin_net_shape(&entries) > 0 {
                    return Ok(ParsedTlv {
                        entries,
                        little_endian: true,
                        framed_u16: false,
                    });
                }
            }
        }
    }

    // 2. Unprefixed `[u16 LE tag][u32 LE len][val]` vs `db-cross-v4/src/main.cc` `[u32 key][u32 len][val]`.
    let le32 = parse_tlv_u32_core(buf, true);
    let be32 = parse_tlv_u32_core(buf, false);

    match (le32, be32) {
        (Ok(le_map), Ok(be_map)) => {
            let le_score = score_bin_net_shape(&le_map);
            let be_score = score_bin_net_shape(&be_map);
            if be_score > le_score {
                Ok(ParsedTlv {
                    entries: be_map,
                    little_endian: false,
                    framed_u16: false,
                })
            } else {
                Ok(ParsedTlv {
                    entries: le_map,
                    little_endian: true,
                    framed_u16: false,
                })
            }
        }
        (Ok(le_map), Err(_)) => Ok(ParsedTlv {
            entries: le_map,
            little_endian: true,
            framed_u16: false,
        }),
        (Err(_), Ok(be_map)) => Ok(ParsedTlv {
            entries: be_map,
            little_endian: false,
            framed_u16: false,
        }),
        (Err(le_err), Err(_)) => {
            if let Some(entries) = parse_tlv_u16_le_body(buf) {
                return Ok(ParsedTlv {
                    entries,
                    little_endian: true,
                    framed_u16: true,
                });
            }
            Err(le_err.to_string())
        }
    }
}

impl ParsedTlv {
    fn first_bytes(&self, keys: &[u32]) -> Option<&[u8]> {
        for k in keys {
            if let Some(list) = self.entries.get(k) {
                if let Some(first) = list.first() {
                    return Some(first.as_slice());
                }
            }
        }
        None
    }

    fn decode_i32_bytes(&self, bytes: &[u8]) -> Option<i32> {
        match bytes.len() {
            1 => Some(bytes[0] as i32),
            2 => Some(if self.little_endian {
                i16::from_le_bytes([bytes[0], bytes[1]]) as i32
            } else {
                i16::from_be_bytes([bytes[0], bytes[1]]) as i32
            }),
            n if n >= 4 => Some(if self.little_endian {
                i32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
            } else {
                i32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
            }),
            _ => None,
        }
    }

    fn get_i32(&self, keys: &[u32]) -> Option<i32> {
        self.first_bytes(keys)
            .and_then(|b| self.decode_i32_bytes(b))
    }

    fn get_i64(&self, keys: &[u32]) -> Option<i64> {
        let b = self.first_bytes(keys)?;
        if b.len() >= 8 {
            let arr: [u8; 8] = b[0..8].try_into().ok()?;
            Some(if self.little_endian {
                i64::from_le_bytes(arr)
            } else {
                i64::from_be_bytes(arr)
            })
        } else {
            self.decode_i32_bytes(b).map(i64::from)
        }
    }

    fn get_str(&self, keys: &[u32]) -> Option<String> {
        let b = self.first_bytes(keys)?;
        decode_text_smart(b)
    }
}

fn is_likely_utf8_printable(v: &[u8]) -> bool {
    if v.is_empty() {
        return true;
    }
    if v.contains(&0) {
        return false;
    }
    let Ok(s) = std::str::from_utf8(v) else {
        return false;
    };
    !s.chars()
        .any(|c| c.is_control() && !matches!(c, '\t' | '\n' | '\r'))
}

fn decode_utf16le_printable(v: &[u8]) -> Option<String> {
    if v.len() < 4 || v.len() % 2 != 0 {
        return None;
    }
    let units = v.len() / 2;
    let mut zero_hi = 0usize;
    let mut out = String::with_capacity(units);
    for pair in v.chunks_exact(2) {
        let (lo, hi) = (pair[0], pair[1]);
        if hi == 0 {
            zero_hi += 1;
            if (0x20..=0x7e).contains(&lo) || matches!(lo, b'\t' | b'\n' | b'\r') {
                out.push(lo as char);
            }
        }
    }
    (!out.is_empty() && zero_hi * 100 >= units * 60).then_some(out)
}

fn decode_text_smart(raw: &[u8]) -> Option<String> {
    let trimmed = raw.strip_suffix(&[0u8]).unwrap_or(raw);
    if is_likely_utf8_printable(trimmed) {
        return std::str::from_utf8(trimmed).ok().map(str::to_string);
    }
    decode_utf16le_printable(raw)
}

fn parse_property_tlv(buf: &[u8]) -> Option<Map<String, Value>> {
    let tlv = parse_tlv_any(buf).ok()?;
    let mut prop = Map::new();
    if let Some(v) = tlv.get_i32(&[0x1e, 1]) {
        prop.insert("size".into(), json!(v));
    }
    if let Some(v) = tlv.get_i32(&[0x1f, 2]) {
        prop.insert("subType".into(), json!(v));
    }
    if let Some(v) = tlv.get_i32(&[0x20, 3]) {
        prop.insert("type".into(), json!(v));
    }
    if let Some(v) = tlv.get_str(&[0x21, 4]) {
        prop.insert("ext".into(), json!(v));
    }
    if let Some(v) = tlv.get_i32(&[0x22, 5]) {
        prop.insert("color".into(), json!(v));
    }
    (!prop.is_empty()).then_some(prop)
}

fn parse_single_attach_tlv(tlv: &ParsedTlv) -> Option<Map<String, Value>> {
    let mut out = Map::new();
    let str_fields: &[(&str, &[u32])] = &[
        ("type", &[0x28, 1]),
        ("extInfo", &[0x2b, 4]),
        ("action", &[0x2d, 6]),
        ("params", &[0x2e, 7]),
        ("title", &[0x2f, 8]),
        ("href", &[0x30, 9]),
        ("thumb", &[0x31, 10]),
        ("description", &[0x32, 11]),
        ("size", &[0x33, 12]),
        ("ext", &[0x34, 13]),
        ("subType", &[0x35, 14]),
        ("color", &[0x36, 15]),
        ("property", &[0x37, 16]),
        ("dataSource", &[0x40, 17]),
        ("data", &[0x41, 18]),
        ("remains", &[0x42, 19]),
        ("zinstantData", &[0x43, 20]),
        ("zinstantMsg", &[0x44, 21]),
    ];
    for &(name, keys) in str_fields {
        if let Some(s) = tlv.get_str(keys) {
            out.insert(name.into(), json!(s));
        }
    }
    if let Some(v) = tlv.get_i32(&[0x29, 2]) {
        out.insert("catId".into(), json!(v));
    }
    if let Some(v) = tlv.get_i32(&[0x2a, 3]) {
        out.insert("id".into(), json!(v));
    }
    if let Some(v) = tlv.get_i32(&[0x2c, 5]) {
        out.insert("childNumber".into(), json!(v));
    }
    (!out.is_empty()).then_some(out)
}

fn collect_attach_objects(raw_blobs: &[Vec<u8>], out: &mut Vec<Value>) {
    for blob in raw_blobs {
        let Ok(tlv) = parse_tlv_any(blob) else {
            continue;
        };
        let has_hex_attach_key = tlv
            .entries
            .keys()
            .any(|k| (0x28..=0x44).contains(k));
        if has_hex_attach_key {
            if let Some(obj) = parse_single_attach_tlv(&tlv) {
                out.push(Value::Object(obj));
                continue;
            }
        }
        // Check if `blob` is a nested list container of serialized `Attach` `TlvBox`es.
        let mut nested_items = Vec::new();
        for sub_list in tlv.entries.values() {
            for sub in sub_list {
                if let Ok(sub_tlv) = parse_tlv_any(sub) {
                    if let Some(obj) = parse_single_attach_tlv(&sub_tlv) {
                        nested_items.push(Value::Object(obj));
                    }
                }
            }
        }
        if !nested_items.is_empty() {
            out.extend(nested_items);
        } else if let Some(obj) = parse_single_attach_tlv(&tlv) {
            out.push(Value::Object(obj));
        }
    }
}

fn parse_quote_tlv(buf: &[u8]) -> Option<Map<String, Value>> {
    let tlv = parse_tlv_any(buf).ok()?;
    let mut q = Map::new();
    if let Some(v) = tlv.get_i32(&[0x50, 1]) {
        q.insert("ownerId".into(), json!(v));
    }
    if let Some(v) = tlv.get_i64(&[0x51, 2]) {
        q.insert("cliMsgId".into(), json!(v));
    }
    if let Some(v) = tlv.get_i64(&[0x52, 3]) {
        q.insert("globalMsgId".into(), json!(v));
    }
    if let Some(v) = tlv.get_i32(&[0x53, 4]) {
        q.insert("cliMsgType".into(), json!(v));
    }
    if let Some(v) = tlv.get_i64(&[0x54, 5]) {
        q.insert("ts".into(), json!(v));
    }
    if let Some(v) = tlv.get_i64(&[0x55, 6]) {
        q.insert("ttl".into(), json!(v));
    }
    if let Some(v) = tlv.get_str(&[0x56, 7]) {
        q.insert("msg".into(), json!(v));
    }
    if let Some(v) = tlv.get_str(&[0x57, 8]) {
        q.insert("attach".into(), json!(v));
    }
    if let Some(v) = tlv.get_str(&[0x58, 9]) {
        q.insert("quoteStatus".into(), json!(v));
    }
    if let Some(v) = tlv.get_str(&[0x5a, 10]) {
        q.insert("fromD".into(), json!(v));
    }
    (!q.is_empty()).then_some(q)
}

fn parse_single_mention_tlv(tlv: &ParsedTlv) -> Option<Map<String, Value>> {
    let mut m = Map::new();
    if let Some(v) = tlv.get_i32(&[0x65, 1]) {
        m.insert("uid".into(), json!(v));
    }
    if let Some(v) = tlv.get_i32(&[0x66, 2]) {
        m.insert("pos".into(), json!(v));
    }
    if let Some(v) = tlv.get_i32(&[0x67, 3]) {
        m.insert("len".into(), json!(v));
    }
    if let Some(v) = tlv.get_i32(&[0x68, 4]) {
        m.insert("type".into(), json!(v));
    }
    (!m.is_empty()).then_some(m)
}

fn collect_mention_objects(raw_blobs: &[Vec<u8>], out: &mut Vec<Value>) {
    for blob in raw_blobs {
        let Ok(tlv) = parse_tlv_any(blob) else {
            continue;
        };
        let has_hex_mention_key = tlv.entries.keys().any(|k| (0x65..=0x68).contains(k));
        if has_hex_mention_key {
            if let Some(obj) = parse_single_mention_tlv(&tlv) {
                out.push(Value::Object(obj));
                continue;
            }
        }
        let mut nested_items = Vec::new();
        for sub_list in tlv.entries.values() {
            for sub in sub_list {
                if let Ok(sub_tlv) = parse_tlv_any(sub) {
                    if let Some(obj) = parse_single_mention_tlv(&sub_tlv) {
                        nested_items.push(Value::Object(obj));
                    }
                }
            }
        }
        if !nested_items.is_empty() {
            out.extend(nested_items);
        } else if let Some(obj) = parse_single_mention_tlv(&tlv) {
            out.push(Value::Object(obj));
        }
    }
}

fn uses_sequential_top_tags(tlv: &ParsedTlv) -> bool {
    if tlv.entries.keys().any(|&k| (1..=4).contains(&k)) {
        return true;
    }
    if tlv.entries.keys().any(|&k| (0x08..=0x0b).contains(&k)) {
        return false;
    }
    if tlv.framed_u16 {
        // When framed with `[u32 LE total_bytes] + [u16 LE tag][u32 LE len]` and only tags in
        // `{5, 6, 7}` appear, check whether tag 5 is a 4-byte `quoteStatus` int vs nested `Property`.
        if let Some(v) = tlv.first_bytes(&[5]) {
            if v.len() <= 4 && parse_property_tlv(v).is_none() {
                return true;
            }
            if parse_property_tlv(v).is_some() {
                return false;
            }
        }
        return true;
    }
    false
}

/// Parses Zalo's nested `TlvBox` (`BinNet`) payload into `{ result, error_message, inner_error, data }`.
pub fn parse_bin_net(bytes: &[u8]) -> Value {
    if bytes.is_empty() {
        return json!({
            "result": 2,
            "error_message": "Something went wrong while parsing",
            "inner_error": "Empty BinNet",
            "data": {},
        });
    }

    let tlv = match parse_tlv_any(bytes) {
        Ok(t) => t,
        Err(err) => {
            return json!({
                "result": 2,
                "error_message": "Something went wrong while parsing",
                "inner_error": err,
                "data": {},
            });
        }
    };

    let mut data = Map::new();
    let mut attachs = Vec::new();
    let mut mentions = Vec::new();

    if uses_sequential_top_tags(&tlv) {
        // Tag 1: Property, Tag 2: Attachs, Tag 3: Quote, Tag 4: Mentions,
        // Tag 5: quoteStatus, Tag 6: dataSource, Tag 7: data
        if let Some(raw) = tlv.first_bytes(&[1]) {
            if let Some(prop) = parse_property_tlv(raw) {
                data.insert("property".into(), Value::Object(prop));
            }
        }
        if let Some(blobs) = tlv.entries.get(&2) {
            collect_attach_objects(blobs, &mut attachs);
        }
        if let Some(raw) = tlv.first_bytes(&[3]) {
            if let Some(q) = parse_quote_tlv(raw) {
                data.insert("quote".into(), Value::Object(q));
            }
        }
        if let Some(blobs) = tlv.entries.get(&4) {
            collect_mention_objects(blobs, &mut mentions);
        }
        if let Some(qst) = tlv.get_i32(&[5]) {
            data.insert("quoteStatus".into(), json!(qst));
        }
        if let Some(ds) = tlv.get_str(&[6]) {
            data.insert("dataSource".into(), json!(ds));
        }
        if let Some(d) = tlv.get_str(&[7]) {
            data.insert("data".into(), json!(d));
        }
    } else {
        // `db-cross-v4/src/main.cc` top-level tag set (`0x05..=0x0b`)
        if let Some(raw) = tlv.first_bytes(&[0x05]) {
            if let Some(prop) = parse_property_tlv(raw) {
                data.insert("property".into(), Value::Object(prop));
            }
        }
        if let Some(blobs) = tlv.entries.get(&0x06) {
            collect_attach_objects(blobs, &mut attachs);
        }
        if let Some(raw) = tlv.first_bytes(&[0x07]) {
            if let Some(q) = parse_quote_tlv(raw) {
                data.insert("quote".into(), Value::Object(q));
            }
        }
        if let Some(blobs) = tlv.entries.get(&0x08) {
            collect_mention_objects(blobs, &mut mentions);
        }
        if let Some(qst) = tlv.get_i32(&[0x09]) {
            data.insert("quoteStatus".into(), json!(qst));
        }
        if let Some(ds) = tlv.get_str(&[0x0a]) {
            data.insert("dataSource".into(), json!(ds));
        }
        if let Some(d) = tlv.get_str(&[0x0b]) {
            data.insert("data".into(), json!(d));
        }
    }

    if !attachs.is_empty() {
        data.insert("attachs".into(), Value::Array(attachs));
    }
    if !mentions.is_empty() {
        data.insert("mentions".into(), Value::Array(mentions));
    }

    json!({
        "result": 0,
        "error_message": "",
        "inner_error": "",
        "data": data,
    })
}

fn normalize_method(method: &str) -> &str {
    method
        .strip_prefix("$znode.nativelibs.dbUtilsAddon.")
        .unwrap_or(method)
}

fn decode_bin_arg(val: Option<&Value>) -> Option<Vec<u8>> {
    let v = val?;
    if let Some(bytes) = wire::decode_bytes(v) {
        return Some(bytes);
    }
    if let Some(arr) = v.as_array() {
        let mut out = Vec::with_capacity(arr.len());
        for item in arr {
            let b = item.as_u64().filter(|&n| n <= 255)? as u8;
            out.push(b);
        }
        return Some(out);
    }
    None
}

/// Synchronous dispatcher for `dbUtilsAddon` (`decompressAndDecryptDb_V2`,
/// `decompressAndDecryptDb`, `parseBinNet`).
pub fn dispatch_db_utils_sync(
    platform: &dyn PlatformPaths,
    method: &str,
    args: &[Value],
) -> BridgeResult {
    match normalize_method(method) {
        "decompressAndDecryptDb_V2" => {
            let (Some(in_str), Some(out_str), Some(key_str)) = (
                args.first().and_then(Value::as_str),
                args.get(1).and_then(Value::as_str),
                args.get(2).and_then(Value::as_str),
            ) else {
                return Ok(zdb4_err(-1, "Invalid args"));
            };
            let out_path = Path::new(out_str);
            if out_str.is_empty() || !platform.is_writable(out_path) {
                return Ok(zdb4_err(
                    -10,
                    format!("EACCES: output path '{out_str}' is outside writable sandbox"),
                ));
            }
            Ok(decompress_and_decrypt_db_v2(
                Path::new(in_str),
                out_path,
                key_str,
            ))
        }
        "decompressAndDecryptDb" => {
            let in_path = Path::new(args.first().and_then(Value::as_str).unwrap_or(""));
            let out_path = Path::new(args.get(1).and_then(Value::as_str).unwrap_or(""));
            let key_str = args.get(2).and_then(Value::as_str).unwrap_or("");
            Ok(decompress_and_decrypt_db_v1(in_path, out_path, key_str))
        }
        "parseBinNet" => {
            let Some(bytes) = decode_bin_arg(args.first()) else {
                return Ok(json!({
                    "result": 1,
                    "error_message": "Wrong arguments",
                    "inner_error": "",
                    "data": {},
                }));
            };
            Ok(parse_bin_net(&bytes))
        }
        other => Err(format!("Unsupported dbUtilsAddon method: {other}")),
    }
}

/// Asynchronous dispatcher for `dbUtilsAddon` offloaded onto Tokio's blocking pool.
pub async fn dispatch_db_utils_async(
    platform: &dyn PlatformPaths,
    method: &str,
    args: &[Value],
) -> BridgeResult {
    let short = normalize_method(method).to_string();
    match short.as_str() {
        "decompressAndDecryptDb_V2" => {
            let (Some(in_str), Some(out_str), Some(key_str)) = (
                args.first().and_then(Value::as_str).map(str::to_string),
                args.get(1).and_then(Value::as_str).map(str::to_string),
                args.get(2).and_then(Value::as_str).map(str::to_string),
            ) else {
                return Ok(zdb4_err(-1, "Invalid args"));
            };
            if out_str.is_empty() || !platform.is_writable(Path::new(&out_str)) {
                return Ok(zdb4_err(
                    -10,
                    format!("EACCES: output path '{out_str}' is outside writable sandbox"),
                ));
            }
            tokio::task::spawn_blocking(move || {
                Ok(decompress_and_decrypt_db_v2(
                    Path::new(&in_str),
                    Path::new(&out_str),
                    &key_str,
                ))
            })
            .await
            .map_err(|e| format!("EIO: {e}"))?
        }
        "decompressAndDecryptDb" => {
            let in_str = args
                .first()
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let out_str = args
                .get(1)
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let key_str = args
                .get(2)
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            Ok(decompress_and_decrypt_db_v1(
                Path::new(&in_str),
                Path::new(&out_str),
                &key_str,
            ))
        }
        "parseBinNet" => {
            let Some(bytes) = decode_bin_arg(args.first()) else {
                return Ok(json!({
                    "result": 1,
                    "error_message": "Wrong arguments",
                    "inner_error": "",
                    "data": {},
                }));
            };
            tokio::task::spawn_blocking(move || Ok(parse_bin_net(&bytes)))
                .await
                .map_err(|e| format!("EIO: {e}"))?
        }
        other => Err(format!("Unsupported dbUtilsAddon method: {other}")),
    }
}
