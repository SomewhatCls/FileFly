//! Layout constants, the record type and the slot codec of the index file.

use super::super::model::CachedFolder;
use super::util::{crc32, get_varint, put_varint, KEY_BITS};

pub(super) const HEADER: usize = 4096;
pub(super) const SLOT: usize = 4096;
/// Slot bytes minus its own header (crc, length, count).
const SLOT_CAP: usize = SLOT - 8;
pub(super) const MIN_BITS: u32 = 6;
pub(super) const MAX_BITS: u32 = 26;
/// Double the table when it holds more than this many entries per slot on average.
pub(super) const GROW_AVG: u64 = 190;
/// Pending (not yet written) entries that trigger a write.
pub const FLUSH_AT: usize = 32 * 1024;
/// Subtree invalidations remembered (and persisted in the header).
pub(super) const MAX_TOMBS: usize = 64;
/// Entries are pruned at most this often.
pub(super) const PRUNE_EVERY: u64 = 24 * 3600;

#[derive(Clone, Copy)]
pub struct Rec {
    pub(super) size: u64,
    pub(super) modified: u32,
    pub(super) scanned: u32,
}

impl Rec {
    pub(super) fn from_cf(v: &CachedFolder) -> Rec {
        Rec {
            size: v.size,
            modified: v.modified_ns as u32,
            scanned: v.scanned_at.min(u32::MAX as u64) as u32,
        }
    }
    pub(super) fn to_cf(self) -> CachedFolder {
        CachedFolder { size: self.size, modified_ns: self.modified as u64, scanned_at: self.scanned as u64 }
    }
}

pub(super) fn bucket_of(key: u64, bits: u32) -> u64 {
    key >> (KEY_BITS - bits)
}

pub(super) fn slot_off(i: u64) -> u64 {
    HEADER as u64 + i * SLOT as u64
}

/// Slot layout: crc u32 | payload length u16 | entry count u16 | payload.
/// Payload: base scan time u32, then per entry: key delta, size, mtime u32, scan time - base.
/// An all-zero slot is an empty slot.
pub(super) fn encode_payload(entries: &[(u64, Rec)], bits: u32) -> Vec<u8> {
    if entries.is_empty() {
        return Vec::new();
    }
    let mask = (1u64 << (KEY_BITS - bits)) - 1;
    let base = entries.iter().map(|e| e.1.scanned).min().unwrap_or(0);
    let mut out = Vec::with_capacity(4 + entries.len() * 16);
    out.extend_from_slice(&base.to_le_bytes());
    let mut prev = 0u64;
    for (k, r) in entries {
        let low = k & mask;
        put_varint(&mut out, low - prev);
        prev = low;
        put_varint(&mut out, r.size);
        out.extend_from_slice(&r.modified.to_le_bytes());
        put_varint(&mut out, (r.scanned - base) as u64);
    }
    out
}

pub(super) fn fill_slot(buf: &mut [u8], payload: &[u8], count: usize) {
    buf.fill(0);
    if payload.is_empty() {
        return;
    }
    buf[4..6].copy_from_slice(&(payload.len() as u16).to_le_bytes());
    buf[6..8].copy_from_slice(&(count as u16).to_le_bytes());
    buf[8..8 + payload.len()].copy_from_slice(payload);
    let crc = crc32(&buf[4..8 + payload.len()]);
    buf[0..4].copy_from_slice(&crc.to_le_bytes());
}

/// Calls `f(key, record)` for the entries of a slot in key order until it returns false.
/// None = the slot is corrupt (bad CRC or malformed).
pub(super) fn walk(buf: &[u8], bucket: u64, bits: u32, mut f: impl FnMut(u64, Rec) -> bool) -> Option<()> {
    let len = u16::from_le_bytes([buf[4], buf[5]]) as usize;
    let count = u16::from_le_bytes([buf[6], buf[7]]) as usize;
    if len == 0 && count == 0 {
        return Some(());
    }
    if len < 4 || len > SLOT_CAP {
        return None;
    }
    let crc = u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]);
    if crc != crc32(&buf[4..8 + len]) {
        return None;
    }
    let p = &buf[8..8 + len];
    let base = u32::from_le_bytes([p[0], p[1], p[2], p[3]]);
    let kw = KEY_BITS - bits;
    let mut pos = 4usize;
    let mut low = 0u64;
    for _ in 0..count {
        low = low.checked_add(get_varint(p, &mut pos)?)?;
        let size = get_varint(p, &mut pos)?;
        let m = p.get(pos..pos + 4)?;
        let modified = u32::from_le_bytes([m[0], m[1], m[2], m[3]]);
        pos += 4;
        let sd = get_varint(p, &mut pos)?;
        let scanned = base.checked_add(u32::try_from(sd).ok()?)?;
        if !f((bucket << kw) | low, Rec { size, modified, scanned }) {
            break;
        }
    }
    Some(())
}

pub(super) fn decode(buf: &[u8], bucket: u64, bits: u32) -> Option<Vec<(u64, Rec)>> {
    let mut v = Vec::new();
    walk(buf, bucket, bits, |k, r| {
        v.push((k, r));
        true
    })?;
    Some(v)
}

/// Encodes `entries`, dropping the oldest ones until they fit into a slot.
/// Returns the payload and whether anything had to be dropped.
pub(super) fn fit(entries: &mut Vec<(u64, Rec)>, bits: u32) -> (Vec<u8>, bool) {
    let mut dropped = false;
    loop {
        let enc = encode_payload(entries, bits);
        if enc.len() <= SLOT_CAP || entries.is_empty() {
            return (enc, dropped);
        }
        dropped = true;
        let k = ((enc.len() - SLOT_CAP) / 8 + 1).min(entries.len());
        let mut order: Vec<usize> = (0..entries.len()).collect();
        order.sort_by_key(|&i| entries[i].1.scanned);
        let mut dead = vec![false; entries.len()];
        for &i in order.iter().take(k) {
            dead[i] = true;
        }
        let mut idx = 0;
        entries.retain(|_| {
            let keep = !dead[idx];
            idx += 1;
            keep
        });
    }
}

/// Merges updates (sorted by key; None = delete) into the sorted entries of a slot.
/// The bool says whether anything actually changed.
pub(super) fn merge(old: Vec<(u64, Rec)>, upd: &[(u64, Option<Rec>)]) -> (Vec<(u64, Rec)>, bool) {
    let mut out = Vec::with_capacity(old.len() + upd.len());
    let mut changed = false;
    let (mut i, mut j) = (0, 0);
    while i < old.len() || j < upd.len() {
        if j >= upd.len() {
            out.push(old[i]);
            i += 1;
        } else if i >= old.len() || upd[j].0 < old[i].0 {
            if let Some(r) = upd[j].1 {
                out.push((upd[j].0, r));
                changed = true;
            }
            j += 1;
        } else if upd[j].0 == old[i].0 {
            if let Some(r) = upd[j].1 {
                out.push((upd[j].0, r));
            }
            changed = true;
            i += 1;
            j += 1;
        } else {
            out.push(old[i]);
            i += 1;
        }
    }
    (out, changed)
}
