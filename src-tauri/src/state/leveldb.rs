//! WebView2/Chromium Local Storage 用的 LevelDB。
//! `.log` 是写入批次，`.ldb` 是按序列号保存的表。同一键取序列号最大的值。

use std::collections::BTreeMap;
use std::fs::File;
use std::io::Read;
use std::path::Path;

const BLOCK_SIZE: usize = 32 * 1024;
const HEADER_SIZE: usize = 7;
const MAGIC: u64 = 0xdb47_7524_8b80_fb57;
const SCAN_LIMIT: u64 = 64 * 1024 * 1024;

#[derive(Clone)]
pub(crate) struct LogBatch {
    pub sequence: u64,
    pub puts: Vec<(Vec<u8>, Vec<u8>)>,
}

pub(crate) fn local_storage_key(name: &str) -> Vec<u8> {
    let mut key = b"_http://tauri.localhost".to_vec();
    key.push(0);
    key.push(1);
    key.extend(name.as_bytes());
    key
}

pub(crate) fn encode_utf16_value(text: &str) -> Vec<u8> {
    let mut value = vec![0];
    value.extend(text.encode_utf16().flat_map(|unit| unit.to_le_bytes()));
    value
}

pub(crate) fn encode_latin1_value(text: &str) -> Vec<u8> {
    let mut value = vec![1];
    value.extend(text.bytes());
    value
}

pub(crate) fn encode_log(batches: &[LogBatch]) -> Vec<u8> {
    let mut out = Vec::new();
    for batch in batches {
        let payload = batch_payload(batch);
        let mut signed = Vec::with_capacity(1 + payload.len());
        signed.push(1);
        signed.extend(&payload);
        let crc = mask_crc(crc32c(&signed));
        out.extend(crc.to_le_bytes());
        out.extend((payload.len() as u16).to_le_bytes());
        out.extend(signed);
    }
    out
}

/// 一个数据块，Snappy 压缩。索引放在数据后面，所以键出现时值的 JSON 已经在前面的压缩块里。
pub(crate) fn encode_sst(entries: &[(Vec<u8>, Vec<u8>, u64)]) -> Vec<u8> {
    let mut block = Vec::new();
    let mut restarts = Vec::new();
    for (user, value, sequence) in entries {
        restarts.push(block.len() as u32);
        let mut key = user.clone();
        key.extend(((sequence << 8) | 1).to_le_bytes());
        push_varint(&mut block, 0);
        push_varint(&mut block, key.len() as u64);
        push_varint(&mut block, value.len() as u64);
        block.extend(&key);
        block.extend(value);
    }
    for offset in &restarts {
        block.extend(offset.to_le_bytes());
    }
    block.extend((restarts.len() as u32).to_le_bytes());
    let compressed = snappy_literals(&block);
    let mut file = Vec::new();
    file.extend(&compressed);
    file.push(1);
    file.extend(0u32.to_le_bytes());
    let index_off = file.len() as u64;
    let handle = {
        let mut bytes = Vec::new();
        push_varint(&mut bytes, 0);
        push_varint(&mut bytes, compressed.len() as u64);
        bytes
    };
    let mut index = Vec::new();
    if let Some((user, _, sequence)) = entries.last() {
        let mut key = user.clone();
        key.extend(((sequence << 8) | 1).to_le_bytes());
        push_varint(&mut index, 0);
        push_varint(&mut index, key.len() as u64);
        push_varint(&mut index, handle.len() as u64);
        index.extend(&key);
        index.extend(&handle);
    }
    index.extend(0u32.to_le_bytes());
    index.extend(1u32.to_le_bytes());
    let index_comp = snappy_literals(&index);
    file.extend(&index_comp);
    file.push(1);
    file.extend(0u32.to_le_bytes());
    let index_size = index_comp.len() as u64;
    let meta_off = file.len() as u64;
    file.extend(0u32.to_le_bytes());
    file.push(0);
    file.extend(0u32.to_le_bytes());
    let mut footer = Vec::new();
    push_varint(&mut footer, meta_off);
    push_varint(&mut footer, 4);
    push_varint(&mut footer, index_off);
    push_varint(&mut footer, index_size);
    footer.resize(40, 0);
    footer.extend(MAGIC.to_le_bytes());
    file.extend(footer);
    file
}

pub(crate) fn latest_values(dir: &Path) -> Result<BTreeMap<String, String>, String> {
    let mut best: BTreeMap<String, (u64, Option<String>)> = BTreeMap::new();
    let mut files = Vec::new();
    let entries = std::fs::read_dir(dir).map_err(|err| format!("读不了 LevelDB 目录：{err}"))?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_file() && is_leveldb_table(&path) {
            files.push(path);
        }
    }
    files.sort();
    for path in files {
        let bytes = read_capped(&path)?;
        for (sequence, kind, key, value) in records_in(&path, &bytes) {
            let Some(name) = storage_name(&key) else {
                continue;
            };
            let slot = best.entry(name.to_owned()).or_insert((0, None));
            if sequence < slot.0 {
                continue;
            }
            if kind == 1 {
                if let Some(text) = value.as_deref().and_then(decode_storage_value) {
                    *slot = (sequence, Some(text));
                }
            } else {
                *slot = (sequence, None);
            }
        }
    }
    Ok(best.into_iter().filter_map(|(key, (_, text))| text.map(|text| (key, text))).collect())
}

fn is_leveldb_table(path: &Path) -> bool {
    let stem = path.file_stem().and_then(|name| name.to_str()).unwrap_or("");
    let ext = path.extension().and_then(|ext| ext.to_str()).unwrap_or("");
    stem.chars().all(|ch| ch.is_ascii_digit()) && (ext.eq_ignore_ascii_case("log") || ext.eq_ignore_ascii_case("ldb") || ext.eq_ignore_ascii_case("sst"))
}

fn records_in(path: &Path, bytes: &[u8]) -> Vec<(u64, u8, Vec<u8>, Option<Vec<u8>>)> {
    let ext = path.extension().and_then(|ext| ext.to_str()).unwrap_or("");
    if ext.eq_ignore_ascii_case("log") {
        return read_log(bytes);
    }
    read_sst(bytes)
}

fn read_capped(path: &Path) -> Result<Vec<u8>, String> {
    let file = File::open(path).map_err(|err| format!("读不了 {}：{err}", path.display()))?;
    let mut reader = file.take(SCAN_LIMIT);
    let mut bytes = Vec::new();
    reader.read_to_end(&mut bytes).map_err(|err| format!("读不了 {}：{err}", path.display()))?;
    Ok(bytes)
}

fn batch_payload(batch: &LogBatch) -> Vec<u8> {
    let mut payload = Vec::new();
    payload.extend(batch.sequence.to_le_bytes());
    payload.extend((batch.puts.len() as u32).to_le_bytes());
    for (key, value) in &batch.puts {
        payload.push(1);
        push_varint(&mut payload, key.len() as u64);
        payload.extend(key);
        push_varint(&mut payload, value.len() as u64);
        payload.extend(value);
    }
    payload
}

fn read_log(bytes: &[u8]) -> Vec<(u64, u8, Vec<u8>, Option<Vec<u8>>)> {
    let mut out = Vec::new();
    let mut logical = Vec::new();
    let mut i = 0;
    while i + HEADER_SIZE <= bytes.len() {
        let block_off = i / BLOCK_SIZE * BLOCK_SIZE;
        if BLOCK_SIZE - (i - block_off) < HEADER_SIZE {
            i = block_off + BLOCK_SIZE;
            continue;
        }
        let stored = u32::from_le_bytes(bytes[i..i + 4].try_into().unwrap());
        let len = u16::from_le_bytes(bytes[i + 4..i + 6].try_into().unwrap()) as usize;
        let kind = bytes[i + 6];
        if kind == 0 && len == 0 {
            i = block_off + BLOCK_SIZE;
            logical.clear();
            continue;
        }
        if i + HEADER_SIZE + len > bytes.len() || i + HEADER_SIZE + len > block_off + BLOCK_SIZE {
            break;
        }
        let signed = &bytes[i + 6..i + HEADER_SIZE + len];
        if crc32c(signed) != unmask_crc(stored) {
            i = block_off + BLOCK_SIZE;
            logical.clear();
            continue;
        }
        let payload = bytes[i + HEADER_SIZE..i + HEADER_SIZE + len].to_vec();
        i += HEADER_SIZE + len;
        match kind {
            1 => out.extend(parse_batch(&payload)),
            2 => {
                logical.clear();
                logical.extend(payload);
            }
            3 => logical.extend(payload),
            4 => {
                logical.extend(payload);
                out.extend(parse_batch(&logical));
                logical.clear();
            }
            _ => logical.clear(),
        }
    }
    out
}

fn parse_batch(payload: &[u8]) -> Vec<(u64, u8, Vec<u8>, Option<Vec<u8>>)> {
    if payload.len() < 12 {
        return Vec::new();
    }
    let sequence = u64::from_le_bytes(payload[0..8].try_into().unwrap());
    let count = u32::from_le_bytes(payload[8..12].try_into().unwrap()) as usize;
    if count > 100_000 {
        return Vec::new();
    }
    let mut index = 12;
    let mut out = Vec::new();
    for step in 0..count {
        if index >= payload.len() {
            return Vec::new();
        }
        let kind = payload[index];
        index += 1;
        let Some((key_len, next)) = read_varint(payload, index) else {
            return Vec::new();
        };
        index = next;
        let key_len = key_len as usize;
        if index + key_len > payload.len() {
            return Vec::new();
        }
        let key = payload[index..index + key_len].to_vec();
        index += key_len;
        let value = if kind == 1 {
            let Some((value_len, next)) = read_varint(payload, index) else {
                return Vec::new();
            };
            index = next;
            let value_len = value_len as usize;
            if index + value_len > payload.len() {
                return Vec::new();
            }
            let value = payload[index..index + value_len].to_vec();
            index += value_len;
            Some(value)
        } else {
            None
        };
        out.push((sequence + step as u64, kind, key, value));
    }
    out
}

fn read_sst(bytes: &[u8]) -> Vec<(u64, u8, Vec<u8>, Option<Vec<u8>>)> {
    if bytes.len() < 48 || u64::from_le_bytes(bytes[bytes.len() - 8..].try_into().unwrap()) != MAGIC {
        return Vec::new();
    }
    let footer = &bytes[bytes.len() - 48..];
    let Some((index_off, index_size)) = footer_index(footer) else {
        return Vec::new();
    };
    let Some(index) = read_block(bytes, index_off, index_size) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (_, handle) in parse_block(&index) {
        let Some((off, next)) = read_varint(&handle, 0) else {
            continue;
        };
        let Some((size, _)) = read_varint(&handle, next) else {
            continue;
        };
        let Some(data) = read_block(bytes, off, size) else {
            continue;
        };
        for (key, value) in parse_block(&data) {
            if key.len() < 8 {
                continue;
            }
            let tag = u64::from_le_bytes(key[key.len() - 8..].try_into().unwrap());
            let user = key[..key.len() - 8].to_vec();
            let kind = (tag & 0xff) as u8;
            let stored = if kind == 1 { Some(value) } else { None };
            out.push((tag >> 8, kind, user, stored));
        }
    }
    out
}

fn footer_index(footer: &[u8]) -> Option<(u64, u64)> {
    let (_, index) = read_varint(footer, 0)?;
    let (_, index) = read_varint(footer, index)?;
    let (offset, index) = read_varint(footer, index)?;
    let (size, _) = read_varint(footer, index)?;
    Some((offset, size))
}

fn read_block(bytes: &[u8], offset: u64, size: u64) -> Option<Vec<u8>> {
    let offset = usize::try_from(offset).ok()?;
    let size = usize::try_from(size).ok()?;
    if offset.checked_add(size)?.checked_add(5)? > bytes.len() {
        return None;
    }
    let data = &bytes[offset..offset + size];
    match bytes[offset + size] {
        0 => Some(data.to_vec()),
        1 => snappy_decompress(data).ok(),
        _ => None,
    }
}

fn parse_block(block: &[u8]) -> Vec<(Vec<u8>, Vec<u8>)> {
    if block.len() < 4 {
        return Vec::new();
    }
    let restarts = u32::from_le_bytes(block[block.len() - 4..].try_into().unwrap()) as usize;
    if restarts > 1_000_000 {
        return Vec::new();
    }
    let tail = 4 + restarts * 4;
    if tail > block.len() {
        return Vec::new();
    }
    let end = block.len() - tail;
    let mut index = 0;
    let mut key = Vec::new();
    let mut entries = Vec::new();
    while index < end {
        let Some((shared, next)) = read_varint(block, index) else {
            break;
        };
        index = next;
        let Some((unshared, next)) = read_varint(block, index) else {
            break;
        };
        index = next;
        let Some((value_len, next)) = read_varint(block, index) else {
            break;
        };
        index = next;
        let shared = shared as usize;
        let unshared = unshared as usize;
        let value_len = value_len as usize;
        if shared > key.len() || index + unshared + value_len > end {
            break;
        }
        key.truncate(shared);
        key.extend_from_slice(&block[index..index + unshared]);
        index += unshared;
        entries.push((key.clone(), block[index..index + value_len].to_vec()));
        index += value_len;
    }
    entries
}

fn storage_name(key: &[u8]) -> Option<&'static str> {
    for name in ["rillsql.session", "rillsql.shell"] {
        let mut suffix = vec![1];
        suffix.extend(name.as_bytes());
        if key.ends_with(&suffix) {
            return Some(name);
        }
        let mut wide = vec![1, 0];
        wide.extend(name.encode_utf16().flat_map(|unit| unit.to_le_bytes()));
        if key.ends_with(&wide) {
            return Some(name);
        }
    }
    None
}

fn decode_storage_value(bytes: &[u8]) -> Option<String> {
    match bytes.first().copied() {
        Some(0) => {
            let rest = &bytes[1..];
            if rest.len() % 2 != 0 {
                return None;
            }
            let units: Vec<u16> = rest.chunks_exact(2).map(|pair| u16::from_le_bytes([pair[0], pair[1]])).collect();
            String::from_utf16(&units).ok()
        }
        Some(1) => Some(bytes[1..].iter().copied().map(char::from).collect()),
        _ => None,
    }
}

fn snappy_literals(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    push_varint(&mut out, data.len() as u64);
    let mut index = 0;
    while index < data.len() {
        let size = (data.len() - index).min(60);
        out.push(((size - 1) as u8) << 2);
        out.extend_from_slice(&data[index..index + size]);
        index += size;
    }
    out
}

fn snappy_decompress(src: &[u8]) -> Result<Vec<u8>, ()> {
    let (expected, mut index) = read_varint(src, 0).ok_or(())?;
    let mut out = Vec::new();
    while index < src.len() {
        let tag = src[index];
        index += 1;
        match tag & 3 {
            0 => {
                let literal = tag >> 2;
                let length = if literal < 60 {
                    usize::from(literal) + 1
                } else {
                    let bytes = usize::from(literal) - 59;
                    if index + bytes > src.len() {
                        return Err(());
                    }
                    let mut value = 0usize;
                    for step in 0..bytes {
                        value |= usize::from(src[index + step]) << (8 * step);
                    }
                    index += bytes;
                    value + 1
                };
                if index + length > src.len() {
                    return Err(());
                }
                out.extend_from_slice(&src[index..index + length]);
                index += length;
            }
            1 => {
                let length = usize::from((tag >> 2) & 7) + 4;
                if index >= src.len() {
                    return Err(());
                }
                let offset = (usize::from(tag >> 5) << 8) | usize::from(src[index]);
                index += 1;
                copy_from(&mut out, offset, length)?;
            }
            2 => {
                let length = usize::from(tag >> 2) + 1;
                if index + 2 > src.len() {
                    return Err(());
                }
                let offset = u16::from_le_bytes([src[index], src[index + 1]]) as usize;
                index += 2;
                copy_from(&mut out, offset, length)?;
            }
            _ => {
                let length = usize::from(tag >> 2) + 1;
                if index + 4 > src.len() {
                    return Err(());
                }
                let offset = u32::from_le_bytes(src[index..index + 4].try_into().unwrap()) as usize;
                index += 4;
                copy_from(&mut out, offset, length)?;
            }
        }
    }
    if out.len() != expected as usize {
        return Err(());
    }
    Ok(out)
}

fn copy_from(out: &mut Vec<u8>, offset: usize, length: usize) -> Result<(), ()> {
    if offset == 0 || offset > out.len() {
        return Err(());
    }
    for _ in 0..length {
        let byte = out[out.len() - offset];
        out.push(byte);
    }
    Ok(())
}

fn push_varint(out: &mut Vec<u8>, mut value: u64) {
    loop {
        let mut byte = (value & 0x7f) as u8;
        value >>= 7;
        if value != 0 {
            byte |= 0x80;
        }
        out.push(byte);
        if value == 0 {
            break;
        }
    }
}

fn read_varint(bytes: &[u8], mut index: usize) -> Option<(u64, usize)> {
    let mut value = 0u64;
    let mut shift = 0;
    while index < bytes.len() && shift <= 63 {
        let byte = bytes[index];
        index += 1;
        value |= u64::from(byte & 0x7f) << shift;
        if byte < 0x80 {
            return Some((value, index));
        }
        shift += 7;
    }
    None
}

fn crc32c(data: &[u8]) -> u32 {
    let mut crc = 0xffff_ffffu32;
    for &byte in data {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0x82f6_3b78 & mask);
        }
    }
    !crc
}

fn mask_crc(crc: u32) -> u32 {
    crc.rotate_right(15).wrapping_add(0xa282_ead8)
}

fn unmask_crc(masked: u32) -> u32 {
    masked.wrapping_sub(0xa282_ead8).rotate_left(15)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc32c_matches_the_castagnoli_vector() {
        assert_eq!(crc32c(b"123456789"), 0xe306_9283);
    }

    #[test]
    fn newest_sequence_wins_across_a_log_and_a_snappy_table() {
        let dir = std::env::temp_dir().join(format!("rillsql-leveldb-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let session = local_storage_key("rillsql.session");
        let shell = local_storage_key("rillsql.shell");
        let log = encode_log(&[
            LogBatch {
                sequence: 1,
                puts: vec![
                    (session.clone(), encode_latin1_value(r#"{"tabs":[{"id":"old","title":"旧","draft":"STALE-DRAFT"}],"activeId":"old"}"#)),
                    (shell.clone(), encode_latin1_value(r#"{"theme":"system"}"#)),
                ],
            },
            LogBatch {
                sequence: 3,
                puts: vec![(session.clone(), encode_utf16_value(r#"{"tabs":[{"id":"new","title":"新","draft":"CURRENT"}],"activeId":"new"}"#))],
            },
        ]);
        std::fs::write(dir.join("000010.log"), log).unwrap();
        let table = encode_sst(&[(shell, encode_latin1_value(r#"{"theme":"dark"}"#), 9)]);
        std::fs::write(dir.join("000011.ldb"), table).unwrap();
        let values = latest_values(&dir).unwrap();
        assert!(values["rillsql.session"].contains("CURRENT"));
        assert!(!values["rillsql.session"].contains("STALE-DRAFT"));
        assert!(values["rillsql.shell"].contains("dark"));
        assert!(!values["rillsql.shell"].contains("system"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
