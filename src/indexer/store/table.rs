//! The table on disk: header, open / reset / grow, and batched writes.

use super::format::{
    bucket_of, decode, encode_payload, fill_slot, fit, merge, slot_off, walk, Rec, GROW_AVG, HEADER, MAX_BITS, MIN_BITS,
    SLOT,
};
use super::io::{open_file, read_at, write_at};
use super::util::{crc32, KeyMap};
use super::super::model::now_secs;
use std::{
    fs::{self, File},
    io,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        RwLock,
    },
};

pub const DB_FILE: &str = "folder_sizes.ffx";
const MAGIC: &[u8; 8] = b"FFSIZE02";

// ------------------------------------------------------------------------------------------
// The table on disk

pub(super) struct Table {
    pub(super) file: File,
    pub(super) bits: u32,
    /// Entries in the file (kept up to date at every write; approximate after a crash).
    pub(super) count: AtomicU64,
    /// Unix seconds of the last prune.
    pub(super) last_prune: AtomicU64,
}

impl Table {
    pub(super) fn get(&self, key: u64) -> Option<Rec> {
        let b = bucket_of(key, self.bits);
        let mut buf = [0u8; SLOT];
        read_at(&self.file, &mut buf, slot_off(b)).ok()?;
        let mut found = None;
        walk(&buf, b, self.bits, |k, r| {
            if k == key {
                found = Some(r);
                false
            } else {
                k < key
            }
        })?;
        found
    }
}

/// Header: magic | crc u32 | bits u8 | flags u8 (1 = resizing) | .. | count u64 | last prune u64 |
/// tombstone count u32 | tombstones (time u64, length u16, path), newest first.
pub(super) fn write_header(t: &Table, resizing: bool, tombs: &[(PathBuf, u64)]) -> io::Result<()> {
    let mut h = vec![0u8; HEADER];
    h[..8].copy_from_slice(MAGIC);
    h[12] = t.bits as u8;
    h[13] = resizing as u8;
    h[16..24].copy_from_slice(&t.count.load(Ordering::Relaxed).to_le_bytes());
    h[24..32].copy_from_slice(&t.last_prune.load(Ordering::Relaxed).to_le_bytes());
    let mut pos = 36usize;
    let mut n = 0u32;
    for (p, ts) in tombs.iter().rev() {
        let Some(s) = p.to_str() else { continue };
        let need = 10 + s.len();
        if s.len() > u16::MAX as usize || pos + need > HEADER {
            continue;
        }
        h[pos..pos + 8].copy_from_slice(&ts.to_le_bytes());
        h[pos + 8..pos + 10].copy_from_slice(&(s.len() as u16).to_le_bytes());
        h[pos + 10..pos + need].copy_from_slice(s.as_bytes());
        pos += need;
        n += 1;
    }
    h[32..36].copy_from_slice(&n.to_le_bytes());
    let crc = crc32(&h[12..]);
    h[8..12].copy_from_slice(&crc.to_le_bytes());
    write_at(&t.file, &h, 0)
}

type Parsed = (u32, u64, u64, Vec<(PathBuf, u64)>);

fn parse_header(buf: &[u8]) -> Option<Parsed> {
    if buf.len() < HEADER || buf[..8] != MAGIC[..] {
        return None;
    }
    let crc = u32::from_le_bytes(buf[8..12].try_into().ok()?);
    if crc != crc32(&buf[12..HEADER]) || buf[13] != 0 {
        return None;
    }
    let bits = buf[12] as u32;
    if !(MIN_BITS..=MAX_BITS).contains(&bits) {
        return None;
    }
    let count = u64::from_le_bytes(buf[16..24].try_into().ok()?);
    let last_prune = u64::from_le_bytes(buf[24..32].try_into().ok()?);
    let n = u32::from_le_bytes(buf[32..36].try_into().ok()?) as usize;
    let mut pos = 36usize;
    let mut tombs = Vec::new();
    for _ in 0..n {
        if pos + 10 > HEADER {
            break;
        }
        let ts = u64::from_le_bytes(buf[pos..pos + 8].try_into().ok()?);
        let len = u16::from_le_bytes([buf[pos + 8], buf[pos + 9]]) as usize;
        if pos + 10 + len > HEADER {
            break;
        }
        if let Ok(s) = std::str::from_utf8(&buf[pos + 10..pos + 10 + len]) {
            tombs.push((PathBuf::from(s), ts));
        }
        pos += 10 + len;
    }
    tombs.reverse(); // stored newest first, kept oldest first in RAM
    Some((bits, count, last_prune, tombs))
}

fn init_table(file: File) -> Option<Table> {
    file.set_len(0).ok()?;
    file.set_len(HEADER as u64 + (1u64 << MIN_BITS) * SLOT as u64).ok()?; // reads back as zeros
    let t = Table {
        file,
        bits: MIN_BITS,
        count: AtomicU64::new(0),
        last_prune: AtomicU64::new(now_secs()),
    };
    write_header(&t, false, &[]).ok()?;
    Some(t)
}

/// Opens the table, or creates a fresh one if the file is new, damaged or from an interrupted resize.
pub(super) fn open_table(dir: &Path) -> Option<(Table, Vec<(PathBuf, u64)>)> {
    fs::create_dir_all(dir).ok()?;
    let file = open_file(&dir.join(DB_FILE)).ok()?;
    let len = file.metadata().ok()?.len();
    let mut hb = vec![0u8; HEADER];
    if len >= HEADER as u64 && read_at(&file, &mut hb, 0).is_ok() {
        if let Some((bits, count, last_prune, tombs)) = parse_header(&hb) {
            if len >= HEADER as u64 + (1u64 << bits) * SLOT as u64 {
                let t = Table {
                    file,
                    bits,
                    count: AtomicU64::new(count),
                    last_prune: AtomicU64::new(last_prune),
                };
                return Some((t, tombs));
            }
        }
    }
    init_table(file).map(|t| (t, Vec::new()))
}

pub(super) fn reset(t: &mut Table) -> io::Result<()> {
    t.file.set_len(0)?;
    t.file.set_len(HEADER as u64 + (1u64 << MIN_BITS) * SLOT as u64)?;
    t.bits = MIN_BITS;
    t.count.store(0, Ordering::Relaxed);
    write_header(t, false, &[])
}

/// Doubles the number of slots in place (see the format notes).
fn double(t: &mut Table, tombs: &[(PathBuf, u64)]) -> io::Result<()> {
    let old_bits = t.bits;
    let new_bits = old_bits + 1;
    let old_slots = 1u64 << old_bits;
    write_header(t, true, tombs)?; // "resizing": a crash from here on means "start over"
    t.file.set_len(HEADER as u64 + old_slots * 2 * SLOT as u64)?;
    let mut buf = vec![0u8; SLOT];
    let mut out = vec![0u8; SLOT];
    for i in (0..old_slots).rev() {
        let entries = if read_at(&t.file, &mut buf, slot_off(i)).is_ok() {
            decode(&buf, i, old_bits).unwrap_or_default()
        } else {
            Vec::new()
        };
        // Sorted by key, so the entries of the lower new slot (next bit 0) come first.
        let split = entries.partition_point(|e| bucket_of(e.0, new_bits) == 2 * i);
        let (lo, hi) = entries.split_at(split);
        fill_slot(&mut out, &encode_payload(lo, new_bits), lo.len());
        write_at(&t.file, &out, slot_off(2 * i))?;
        fill_slot(&mut out, &encode_payload(hi, new_bits), hi.len());
        write_at(&t.file, &out, slot_off(2 * i + 1))?;
    }
    t.bits = new_bits;
    write_header(t, false, tombs)
}

/// Merges a batch of updates into the slots it touches.
pub(super) fn apply(table: &RwLock<Table>, batch: &KeyMap<Option<Rec>>, tombs: &[(PathBuf, u64)]) -> io::Result<()> {
    let mut items: Vec<(u64, Option<Rec>)> = batch.iter().map(|(k, v)| (*k, *v)).collect();
    items.sort_unstable_by_key(|x| x.0);
    let mut overflow = false;
    {
        let t = table.read().unwrap_or_else(|e| e.into_inner());
        let mut buf = vec![0u8; SLOT];
        let mut delta = 0i64;
        let mut i = 0;
        while i < items.len() {
            let b = bucket_of(items[i].0, t.bits);
            let mut j = i;
            while j < items.len() && bucket_of(items[j].0, t.bits) == b {
                j += 1;
            }
            let old = if read_at(&t.file, &mut buf, slot_off(b)).is_ok() {
                decode(&buf, b, t.bits).unwrap_or_default()
            } else {
                Vec::new()
            };
            let before = old.len() as i64;
            let (mut merged, changed) = merge(old, &items[i..j]);
            if changed {
                let (payload, dropped) = fit(&mut merged, t.bits);
                overflow |= dropped;
                fill_slot(&mut buf, &payload, merged.len());
                write_at(&t.file, &buf, slot_off(b))?;
                delta += merged.len() as i64 - before;
            }
            i = j;
        }
        let c = t.count.load(Ordering::Relaxed) as i64 + delta;
        t.count.store(c.max(0) as u64, Ordering::Relaxed);
        write_header(&t, false, tombs)?;
    }
    let (bits, count) = {
        let t = table.read().unwrap_or_else(|e| e.into_inner());
        (t.bits, t.count.load(Ordering::Relaxed))
    };
    let slots = 1u64 << bits;
    // Also grow when slots overflowed and the table is at least a third full by count.
    if bits < MAX_BITS && (count > slots * GROW_AVG || (overflow && count > slots * 64)) {
        let mut t = table.write().unwrap_or_else(|e| e.into_inner());
        double(&mut t, tombs)?;
        while t.bits < MAX_BITS && t.count.load(Ordering::Relaxed) > (1u64 << t.bits) * GROW_AVG {
            double(&mut t, tombs)?;
        }
    }
    Ok(())
}
