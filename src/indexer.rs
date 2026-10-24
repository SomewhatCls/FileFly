//! Folder-size index.
//!
//! Two tiers:
//!  * EVERY folder size that was ever scanned lives in a compact on-disk hash table (`mod store`,
//!    no external database). New results are buffered in RAM and written in batches, so a cancelled
//!    scan keeps its progress, and there is no big "save everything" step on exit and no big
//!    "load everything" step at startup;
//!  * a bounded in-RAM cache sits in front of it (the limit is a user setting, `limit_mb`). A lookup
//!    that misses RAM reads one 4 KiB slot from the file and copies the entry into RAM. When RAM goes
//!    over the limit, entries are dropped from RAM only (the file still has them).
//!
//! Also:
//!  * the cache is shared (`Arc`) instead of being cloned on every navigation;
//!  * scanning is iterative, runs on a small worker pool at below-normal priority and yields regularly;
//!  * entries are only trusted while the folder's mtime matches *and* they are younger than `TTL_SECS`
//!    (a directory's mtime does not change when a file deeper down grows, so mtime alone goes stale);
//!  * a file watcher invalidates the changed path and all its ancestors, so new files show up live.

use eframe::egui;
use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use std::{
    collections::{HashSet, VecDeque},
    fs,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
        mpsc::Sender,
        Arc, Mutex,
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use store::Store;

/// Files of the earlier index formats; removed at startup (including the oversized redb file).
const OLD_FILES: [&str; 3] = ["folder_sizes.json", "folder_sizes.bin", "folder_sizes.redb"];
/// Maximum age before a cached folder is re-scanned even if its mtime is unchanged.
const TTL_SECS: u64 = 6 * 3600;
/// Entries not refreshed for this long are dropped from the file (checked at most once a day).
const PRUNE_SECS: u64 = 30 * 24 * 3600;

/// Rough RAM cost of one cached folder; only used to turn the "MB" setting into an entry count.
/// 16 B of data + 8 B key, in a hash table that is never completely full: ~40-50 B.
/// (The old path-keyed cache cost ~170 B per folder, so the same limit now holds ~3.5x more.)
const ENTRY_BYTES: usize = 48;

fn entries_for_mb(mb: usize) -> usize {
    mb.saturating_mul(1 << 20) / ENTRY_BYTES
}

#[derive(Clone)]
pub struct CachedFolder {
    pub size: u64,
    /// 32-bit fingerprint of the folder's modification time (see `ns`). It is only ever compared
    /// for equality, so it does not have to be the real timestamp.
    pub modified_ns: u64,
    /// Unix seconds of the scan.
    pub scanned_at: u64,
}

pub enum IndexMsg {
    Size(PathBuf, u64),
}

/// Number of scan worker threads per `start()`.
const WORKERS: usize = 3;

/// Counts a live scan worker: incremented before the thread is spawned, decremented when the
/// thread ends for any reason (finished, cancelled, channel closed, panic).
struct Live(Arc<AtomicUsize>);
impl Drop for Live {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// Fingerprint of a modification time: the nanoseconds since 1970, scrambled and cut to 32 bits.
/// Two different mtimes collide with a chance of 1 in 4 billion, which only means a changed folder
/// is trusted until the TTL runs out. It saves 4 bytes per folder on disk and in RAM.
fn ns(t: SystemTime) -> u64 {
    let n = t.duration_since(UNIX_EPOCH).map(|d| d.as_nanos() as u64).unwrap_or(0);
    n.wrapping_mul(0x9E37_79B9_7F4A_7C15) >> 32
}

// ----------------------------------------------------------------------------------------------
// Storage: compressed on-disk hash table + bounded cache in RAM

/// # File format (`folder_sizes.ffx`)
///
/// ```text
/// [ header 4 KiB ][ slot 0 ][ slot 1 ] ... [ slot 2^bits - 1 ]        every slot is 4 KiB
/// ```
/// * A path is reduced to a 48-bit key (stable FNV-1a + finalizer over its components). The top
///   `bits` bits of the key choose the slot, so a lookup is: hash, ONE 4 KiB read, scan the slot.
///   The path text itself is never stored, which is what makes the file small.
/// * Inside a slot the entries are sorted by key and delta-coded with varints:
///   `key delta | size (varint) | mtime fingerprint (u32) | scanned_at - slot minimum (varint)`
///   which is ~16 bytes per folder (a path-keyed B-tree needs 150+).
/// * Every slot has a CRC. A torn or corrupt slot simply counts as empty (it is only a cache).
/// * The table doubles in place when the slots get ~70% full (slot i becomes 2i and 2i+1, done
///   from the back so nothing is overwritten before it is read). A crash during that leaves a flag
///   in the header and the file is recreated empty at the next start.
/// * If a slot overflows, its oldest entries are dropped (they get rescanned when needed).
/// * Writes are batched: results collect in RAM (`pending`) and are merged into the slots they
///   touch, 32k entries at a time or when `save_if_dirty` runs. The file never has garbage that
///   needs compacting, its size only changes when it doubles.
/// * Subtree invalidation cannot enumerate descendants (no path text), so it records
///   "everything below DIR scanned before TIME is invalid" instead (kept in the header).
mod store {
    use super::{now_secs, CachedFolder};
    use std::{
        collections::HashMap,
        fs::{self, File, OpenOptions},
        hash::{BuildHasherDefault, Hasher},
        io,
        path::{Path, PathBuf},
        sync::{
            atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
            Arc, Mutex, MutexGuard, RwLock,
        },
        time::Duration,
    };

    const DB_FILE: &str = "folder_sizes.ffx";
    const MAGIC: &[u8; 8] = b"FFSIZE02";
    const HEADER: usize = 4096;
    const SLOT: usize = 4096;
    /// Slot bytes minus its own header (crc, length, count).
    const SLOT_CAP: usize = SLOT - 8;
    const KEY_BITS: u32 = 64;
    const MIN_BITS: u32 = 6;
    const MAX_BITS: u32 = 26;
    /// Double the table when it holds more than this many entries per slot on average.
    const GROW_AVG: u64 = 190;
    /// Pending (not yet written) entries that trigger a write.
    pub const FLUSH_AT: usize = 32 * 1024;
    /// Subtree invalidations remembered (and persisted in the header).
    const MAX_TOMBS: usize = 64;
    /// Entries are pruned at most this often.
    const PRUNE_EVERY: u64 = 24 * 3600;

    // ------------------------------------------------------------------------------------------
    // Small helpers: hashing, crc, varints, positional I/O

    const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

    fn mix64(mut z: u64) -> u64 {
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    /// 48-bit key of a path. Hashes the components (like `Path` equality does, so a trailing
    /// separator makes no difference). Written by hand, because `std`'s hashers may change between
    /// Rust versions and this value is stored on disk.
    pub fn key_of(p: &Path) -> u64 {
        let mut h = FNV_OFFSET;
        for c in p.components() {
            let s = c.as_os_str().to_string_lossy();
            for &b in s.as_bytes() {
                h = (h ^ b as u64).wrapping_mul(FNV_PRIME);
            }
            h = (h ^ 0xFF).wrapping_mul(FNV_PRIME); // 0xFF never occurs in UTF-8
        }
        mix64(h) >> (64 - KEY_BITS)
    }

    /// The keys are already well mixed, so the maps just multiply them once.
    #[derive(Default)]
    pub struct KeyHasher(u64);
    impl Hasher for KeyHasher {
        fn finish(&self) -> u64 {
            self.0
        }
        fn write(&mut self, bytes: &[u8]) {
            for &b in bytes {
                self.0 = (self.0 ^ b as u64).wrapping_mul(FNV_PRIME);
            }
        }
        fn write_u64(&mut self, i: u64) {
            self.0 = i.wrapping_mul(0x9E37_79B9_7F4A_7C15);
        }
    }
    type KeyMap<V> = HashMap<u64, V, BuildHasherDefault<KeyHasher>>;

    const fn crc_table() -> [u32; 256] {
        let mut t = [0u32; 256];
        let mut i = 0;
        while i < 256 {
            let mut c = i as u32;
            let mut k = 0;
            while k < 8 {
                c = if c & 1 != 0 { 0xEDB8_8320 ^ (c >> 1) } else { c >> 1 };
                k += 1;
            }
            t[i] = c;
            i += 1;
        }
        t
    }
    static CRC: [u32; 256] = crc_table();

    fn crc32(data: &[u8]) -> u32 {
        let mut c = !0u32;
        for &b in data {
            c = CRC[((c ^ b as u32) & 0xFF) as usize] ^ (c >> 8);
        }
        !c
    }

    fn put_varint(out: &mut Vec<u8>, mut v: u64) {
        while v >= 0x80 {
            out.push((v as u8) | 0x80);
            v >>= 7;
        }
        out.push(v as u8);
    }

    fn get_varint(b: &[u8], pos: &mut usize) -> Option<u64> {
        let mut v = 0u64;
        let mut shift = 0u32;
        loop {
            let byte = *b.get(*pos)?;
            *pos += 1;
            if shift >= 64 {
                return None;
            }
            v |= ((byte & 0x7F) as u64) << shift;
            if byte & 0x80 == 0 {
                return Some(v);
            }
            shift += 7;
        }
    }

    // Positional I/O: no shared file cursor, so readers need no lock among themselves.
    #[cfg(windows)]
    fn read_at(f: &File, buf: &mut [u8], mut off: u64) -> io::Result<()> {
        use std::os::windows::fs::FileExt;
        let mut done = 0;
        while done < buf.len() {
            let n = f.seek_read(&mut buf[done..], off)?;
            if n == 0 {
                return Err(io::ErrorKind::UnexpectedEof.into());
            }
            done += n;
            off += n as u64;
        }
        Ok(())
    }
    #[cfg(windows)]
    fn write_at(f: &File, buf: &[u8], mut off: u64) -> io::Result<()> {
        use std::os::windows::fs::FileExt;
        let mut done = 0;
        while done < buf.len() {
            let n = f.seek_write(&buf[done..], off)?;
            if n == 0 {
                return Err(io::ErrorKind::WriteZero.into());
            }
            done += n;
            off += n as u64;
        }
        Ok(())
    }
    #[cfg(unix)]
    fn read_at(f: &File, buf: &mut [u8], off: u64) -> io::Result<()> {
        use std::os::unix::fs::FileExt;
        f.read_exact_at(buf, off)
    }
    #[cfg(unix)]
    fn write_at(f: &File, buf: &[u8], off: u64) -> io::Result<()> {
        use std::os::unix::fs::FileExt;
        f.write_all_at(buf, off)
    }

    fn open_file(path: &Path) -> io::Result<File> {
        let mut o = OpenOptions::new();
        o.read(true).write(true).create(true).truncate(false);
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            // FILE_SHARE_READ only: a second running instance cannot open the file for writing
            // and falls back to RAM-only mode, like the database did.
            o.share_mode(1);
        }
        o.open(path)
    }

    // ------------------------------------------------------------------------------------------
    // Records and slot codec

    #[derive(Clone, Copy)]
    pub struct Rec {
        size: u64,
        modified: u32,
        scanned: u32,
    }

    impl Rec {
        fn from_cf(v: &CachedFolder) -> Rec {
            Rec {
                size: v.size,
                modified: v.modified_ns as u32,
                scanned: v.scanned_at.min(u32::MAX as u64) as u32,
            }
        }
        fn to_cf(self) -> CachedFolder {
            CachedFolder { size: self.size, modified_ns: self.modified as u64, scanned_at: self.scanned as u64 }
        }
    }

    fn bucket_of(key: u64, bits: u32) -> u64 {
        key >> (KEY_BITS - bits)
    }

    fn slot_off(i: u64) -> u64 {
        HEADER as u64 + i * SLOT as u64
    }

    /// Slot layout: crc u32 | payload length u16 | entry count u16 | payload.
    /// Payload: base scan time u32, then per entry: key delta, size, mtime u32, scan time - base.
    /// An all-zero slot is an empty slot.
    fn encode_payload(entries: &[(u64, Rec)], bits: u32) -> Vec<u8> {
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

    fn fill_slot(buf: &mut [u8], payload: &[u8], count: usize) {
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
    fn walk(buf: &[u8], bucket: u64, bits: u32, mut f: impl FnMut(u64, Rec) -> bool) -> Option<()> {
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

    fn decode(buf: &[u8], bucket: u64, bits: u32) -> Option<Vec<(u64, Rec)>> {
        let mut v = Vec::new();
        walk(buf, bucket, bits, |k, r| {
            v.push((k, r));
            true
        })?;
        Some(v)
    }

    /// Encodes `entries`, dropping the oldest ones until they fit into a slot.
    /// Returns the payload and whether anything had to be dropped.
    fn fit(entries: &mut Vec<(u64, Rec)>, bits: u32) -> (Vec<u8>, bool) {
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
    fn merge(old: Vec<(u64, Rec)>, upd: &[(u64, Option<Rec>)]) -> (Vec<(u64, Rec)>, bool) {
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

    // ------------------------------------------------------------------------------------------
    // The table on disk

    struct Table {
        file: File,
        bits: u32,
        /// Entries in the file (kept up to date at every write; approximate after a crash).
        count: AtomicU64,
        /// Unix seconds of the last prune.
        last_prune: AtomicU64,
    }

    impl Table {
        fn get(&self, key: u64) -> Option<Rec> {
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
    fn write_header(t: &Table, resizing: bool, tombs: &[(PathBuf, u64)]) -> io::Result<()> {
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
    fn open_table(dir: &Path) -> Option<(Table, Vec<(PathBuf, u64)>)> {
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

    fn reset(t: &mut Table) -> io::Result<()> {
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
    fn apply(table: &RwLock<Table>, batch: &KeyMap<Option<Rec>>, tombs: &[(PathBuf, u64)]) -> io::Result<()> {
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

    // ------------------------------------------------------------------------------------------
    // The store: pending writes + RAM cache + table

    struct Inner {
        /// Clean read cache (limited by `max_ram`). Anything in here is also on disk or pending.
        ram: KeyMap<Rec>,
        /// Changes not on disk yet (None = deleted). Never evicted.
        pending: KeyMap<Option<Rec>>,
        /// The batch that is being written right now; still visible to readers.
        flushing: Option<Arc<KeyMap<Option<Rec>>>>,
        /// "Entries below this folder scanned before this time (unix seconds) are invalid."
        tombs: Vec<(PathBuf, u64)>,
        /// Counts removals, so a slow disk read cannot put a just-removed entry back into RAM.
        removals: u64,
        salt: u64,
    }

    impl Inner {
        /// Some(None) = known to be deleted, None = not in memory.
        fn mem(&self, k: u64) -> Option<Option<Rec>> {
            if let Some(v) = self.pending.get(&k) {
                return Some(*v);
            }
            if let Some(f) = &self.flushing {
                if let Some(v) = f.get(&k) {
                    return Some(*v);
                }
            }
            self.ram.get(&k).map(|r| Some(*r))
        }

        fn tombed(&self, p: &Path, scanned: u32) -> bool {
            self.tombs.iter().any(|(dir, ts)| (scanned as u64) < *ts && p.starts_with(dir))
        }
    }

    pub struct Store {
        /// None if the file can't be opened (e.g. a second window has it open): then RAM is all
        /// there is, the limit is ignored and nothing is saved.
        table: Option<RwLock<Table>>,
        inner: Mutex<Inner>,
        /// Serializes everything that modifies slots (flush, prune, clear).
        flush_lock: Mutex<()>,
        /// Entry limit of the RAM cache (usize::MAX = unlimited).
        pub max_ram: AtomicUsize,
        /// Set while a background flush is running.
        pub bg_flush: AtomicBool,
    }

    impl Store {
        pub fn open(dir: &Path, max_ram: usize) -> Store {
            let (table, tombs) = match open_table(dir) {
                Some((t, tb)) => (Some(RwLock::new(t)), tb),
                None => (None, Vec::new()),
            };
            Store {
                table,
                inner: Mutex::new(Inner {
                    ram: KeyMap::default(),
                    pending: KeyMap::default(),
                    flushing: None,
                    tombs,
                    removals: 0,
                    salt: 0,
                }),
                flush_lock: Mutex::new(()),
                max_ram: AtomicUsize::new(max_ram),
                bg_flush: AtomicBool::new(false),
            }
        }

        fn inner(&self) -> MutexGuard<'_, Inner> {
            self.inner.lock().unwrap_or_else(|e| e.into_inner())
        }

        /// Once 10% over the limit, drops random entries back down to the limit.
        /// (Safe, because the file or `pending` still has them. Without a file nothing is dropped.)
        fn trim(&self, g: &mut Inner) {
            if self.table.is_none() {
                return;
            }
            let max = self.max_ram.load(Ordering::Relaxed);
            let len = g.ram.len();
            if len <= max.saturating_add(max / 10) {
                return;
            }
            g.salt = g.salt.wrapping_add(0x9E37_79B9_7F4A_7C15);
            let salt = g.salt;
            let thr = ((max as u128 * u64::MAX as u128) / len as u128) as u64;
            g.ram.retain(|k, _| mix64(*k ^ salt) <= thr);
        }

        /// Re-applies the RAM limit (after it was changed).
        pub fn shrink(&self) {
            let mut g = self.inner();
            self.trim(&mut g);
        }

        pub fn get(&self, p: &Path) -> Option<CachedFolder> {
            let k = key_of(p);
            let epoch;
            {
                let g = self.inner();
                epoch = g.removals;
                if let Some(found) = g.mem(k) {
                    let r = found?;
                    return if g.tombed(p, r.scanned) { None } else { Some(r.to_cf()) };
                }
            }
            // Not in memory: one slot read from the file.
            let table = self.table.as_ref()?;
            let r = table.read().unwrap_or_else(|e| e.into_inner()).get(k)?;
            let mut g = self.inner();
            if g.removals == epoch {
                g.ram.insert(k, r);
                self.trim(&mut g);
            }
            if g.tombed(p, r.scanned) {
                None
            } else {
                Some(r.to_cf())
            }
        }

        pub fn put_many(&self, items: Vec<(PathBuf, CachedFolder)>) {
            let recs: Vec<(u64, Rec)> = items.iter().map(|(p, v)| (key_of(p), Rec::from_cf(v))).collect();
            let persist = self.table.is_some();
            let need_flush = {
                let mut g = self.inner();
                for (k, r) in recs {
                    g.ram.insert(k, r);
                    if persist {
                        g.pending.insert(k, Some(r));
                    }
                }
                self.trim(&mut g);
                persist && g.pending.len() >= FLUSH_AT
            };
            if need_flush {
                self.flush();
            }
        }

        /// Removes `p` and every ancestor (their totals are no longer correct).
        pub fn remove_with_ancestors(&self, p: &Path) {
            let keys: Vec<u64> = p.ancestors().map(key_of).collect();
            let persist = self.table.is_some();
            let mut g = self.inner();
            g.removals += 1;
            for k in keys {
                g.ram.remove(&k);
                if persist {
                    g.pending.insert(k, None);
                }
            }
        }

        /// Removes `dir`, everything below it and all its ancestors. Descendants cannot be listed
        /// (only hashes are stored), so they are invalidated by time: anything below `dir` that
        /// was scanned before now is ignored from here on and gets rescanned.
        pub fn remove_tree(&self, dir: &Path) {
            self.remove_with_ancestors(dir);
            let ts = now_secs();
            let mut g = self.inner();
            g.tombs.retain(|(d, _)| !d.starts_with(dir));
            g.tombs.push((dir.to_path_buf(), ts));
            let n = g.tombs.len();
            if n > MAX_TOMBS {
                g.tombs.drain(..n - MAX_TOMBS);
            }
        }

        pub fn clear(&self) {
            let _fl = self.flush_lock.lock().unwrap_or_else(|e| e.into_inner());
            {
                let mut g = self.inner();
                g.removals += 1;
                g.ram = KeyMap::default();
                g.pending = KeyMap::default();
                g.flushing = None;
                g.tombs.clear();
            }
            if let Some(table) = &self.table {
                let mut t = table.write().unwrap_or_else(|e| e.into_inner());
                let _ = reset(&mut t);
            }
        }

        /// Number of indexed folders (all of them, not just the ones in RAM). Approximate:
        /// a rewritten entry that is still pending is counted twice.
        pub fn len(&self) -> usize {
            let g = self.inner();
            match &self.table {
                Some(table) => {
                    let disk = table.read().unwrap_or_else(|e| e.into_inner()).count.load(Ordering::Relaxed);
                    disk as usize + g.pending.values().filter(|v| v.is_some()).count()
                }
                None => g.ram.len(),
            }
        }

        pub fn pending_len(&self) -> usize {
            self.inner().pending.len()
        }

        /// Writes the pending changes into the file (no fsync, see `sync`).
        pub fn flush(&self) {
            let Some(table) = &self.table else { return };
            let _fl = self.flush_lock.lock().unwrap_or_else(|e| e.into_inner());
            let (batch, tombs) = {
                let mut g = self.inner();
                if g.pending.is_empty() {
                    return;
                }
                let b = Arc::new(std::mem::take(&mut g.pending));
                g.flushing = Some(Arc::clone(&b));
                (b, g.tombs.clone())
            };
            // (On an I/O error the batch is dropped: this is only a cache, the folders get rescanned.)
            let _ = apply(table, &batch, &tombs);
            self.inner().flushing = None;
        }

        /// Flush + fsync: everything written so far survives a crash or power loss.
        pub fn sync(&self) {
            self.flush();
            if let Some(table) = &self.table {
                let _ = table.read().unwrap_or_else(|e| e.into_inner()).file.sync_data();
            }
        }

        /// Drops entries not refreshed since `cutoff` (the PRUNE_SECS rule). Reads the whole file,
        /// so it runs at most once a day, slot by slot, without blocking other work for long.
        pub fn prune(&self, cutoff: u64) {
            {
                let mut g = self.inner();
                g.ram.retain(|_, r| r.scanned as u64 >= cutoff);
                g.tombs.retain(|(_, ts)| *ts >= cutoff);
            }
            let Some(table) = &self.table else { return };
            let last = table.read().unwrap_or_else(|e| e.into_inner()).last_prune.load(Ordering::Relaxed);
            if now_secs().saturating_sub(last) < PRUNE_EVERY {
                return;
            }
            let mut buf = vec![0u8; SLOT];
            let mut b = 0u64;
            loop {
                let more = {
                    let _fl = self.flush_lock.lock().unwrap_or_else(|e| e.into_inner());
                    let t = table.read().unwrap_or_else(|e| e.into_inner());
                    if b >= (1u64 << t.bits) {
                        false
                    } else {
                        if read_at(&t.file, &mut buf, slot_off(b)).is_ok() {
                            if let Some(entries) = decode(&buf, b, t.bits) {
                                let kept: Vec<(u64, Rec)> =
                                    entries.iter().copied().filter(|e| e.1.scanned as u64 >= cutoff).collect();
                                if kept.len() != entries.len() {
                                    fill_slot(&mut buf, &encode_payload(&kept, t.bits), kept.len());
                                    if write_at(&t.file, &buf, slot_off(b)).is_ok() {
                                        let removed = (entries.len() - kept.len()) as u64;
                                        let c = t.count.load(Ordering::Relaxed).saturating_sub(removed);
                                        t.count.store(c, Ordering::Relaxed);
                                    }
                                }
                            }
                        }
                        true
                    }
                };
                if !more {
                    break;
                }
                b += 1;
                if b % 64 == 0 {
                    std::thread::sleep(Duration::from_millis(1));
                }
            }
            let tombs = self.inner().tombs.clone();
            let _fl = self.flush_lock.lock().unwrap_or_else(|e| e.into_inner());
            let t = table.read().unwrap_or_else(|e| e.into_inner());
            t.last_prune.store(now_secs(), Ordering::Relaxed);
            let _ = write_header(&t, false, &tombs);
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn tmp(name: &str) -> PathBuf {
            let d = std::env::temp_dir().join(format!("ff_idx_{}_{}", name, std::process::id()));
            let _ = fs::remove_dir_all(&d);
            d
        }
        fn cf(n: u64) -> CachedFolder {
            CachedFolder {
                size: n * 1000 + 7,
                modified_ns: (n as u32).wrapping_mul(2_654_435_761) as u64,
                scanned_at: 1_700_000_000 + n % 5000,
            }
        }
        fn path(root: &Path, i: u64) -> PathBuf {
            root.join(format!("dir{i}")).join("sub")
        }

        #[test]
        fn roundtrip_grow_reopen_remove() {
            let dir = tmp("rt");
            let root = PathBuf::from("data_root");
            let n = 60_000u64;
            {
                let s = Store::open(&dir, 1000);
                let all: Vec<u64> = (0..n).collect();
                for chunk in all.chunks(5000) {
                    s.put_many(chunk.iter().map(|&i| (path(&root, i), cf(i))).collect());
                }
                s.sync();
                for i in (0..n).step_by(97) {
                    assert_eq!(s.get(&path(&root, i)).map(|c| c.size), Some(cf(i).size));
                }
                assert!(s.get(&root.join("nope")).is_none());
            }
            // Reopen: RAM is empty, everything comes from the file.
            let s = Store::open(&dir, 1000);
            assert!(s.len() as u64 >= n * 99 / 100);
            for i in (0..n).step_by(31) {
                let got = s.get(&path(&root, i)).expect("entry survives a restart");
                assert_eq!(got.size, cf(i).size);
                assert_eq!(got.modified_ns, cf(i).modified_ns);
                assert_eq!(got.scanned_at, cf(i).scanned_at);
            }
            // Single removal (+ ancestors), persisted.
            s.remove_with_ancestors(&path(&root, 5));
            assert!(s.get(&path(&root, 5)).is_none());
            s.sync();
            drop(s);
            let s = Store::open(&dir, 1000);
            assert!(s.get(&path(&root, 5)).is_none());
            assert!(s.get(&path(&root, 6)).is_some());
            // Subtree removal: everything below is ignored, entries scanned later are valid again.
            s.remove_tree(&root.join("dir7"));
            assert!(s.get(&path(&root, 7)).is_none());
            assert!(s.get(&path(&root, 8)).is_some());
            let mut fresh = cf(7);
            fresh.scanned_at = now_secs() + 5;
            s.put_many(vec![(path(&root, 7), fresh)]);
            assert!(s.get(&path(&root, 7)).is_some());
            let _ = fs::remove_dir_all(&dir);
        }
    }
}

// ----------------------------------------------------------------------------------------------
// Indexer

type Cache = Arc<Store>;

/// Opens the index in `dir`, clears out leftovers of older formats and starts the background
/// pruning of entries that haven't been refreshed for `PRUNE_SECS`.
fn open_store(dir: &Path, max_entries: usize) -> Cache {
    for old in OLD_FILES {
        let _ = fs::remove_file(dir.join(old));
    }
    let store = Arc::new(Store::open(dir, max_entries));
    let pruner = Arc::clone(&store);
    std::thread::spawn(move || pruner.prune(now_secs().saturating_sub(PRUNE_SECS)));
    store
}

pub struct Indexer {
    cache: Cache,
    /// Folder the index file lives in.
    dir: PathBuf,
    generation: Arc<AtomicU64>,
    cache_dirty: Arc<AtomicBool>,
    live: Arc<AtomicUsize>,
    tx: Sender<IndexMsg>,
    ctx: egui::Context,
}

impl Indexer {
    /// `dir` is where the index file lives. `limit_mb` is the RAM cache limit (0 = unlimited).
    pub fn new(ctx: egui::Context, tx: Sender<IndexMsg>, dir: &Path, limit_mb: usize) -> Self {
        let max_entries = if limit_mb == 0 { usize::MAX } else { entries_for_mb(limit_mb) };
        Self {
            cache: open_store(dir, max_entries),
            dir: dir.to_path_buf(),
            generation: Arc::new(AtomicU64::new(0)),
            cache_dirty: Arc::new(AtomicBool::new(false)),
            live: Arc::new(AtomicUsize::new(0)),
            tx,
            ctx,
        }
    }

    /// Switches to the index in `dir` (created if it doesn't exist there yet). This is what
    /// replaces the old `load(dir)` when the user picks another index folder. The old folder's
    /// file is left where it is.
    pub fn set_dir(&mut self, dir: &Path) {
        if dir == self.dir {
            return;
        }
        self.generation.fetch_add(1, Ordering::SeqCst); // stop scans that still write to the old index
        self.cache.sync(); // make the old index's last writes permanent
        let max_entries = self.cache.max_ram.load(Ordering::Relaxed);
        self.cache = open_store(dir, max_entries);
        self.dir = dir.to_path_buf();
        self.cache_dirty.store(false, Ordering::Relaxed);
    }

    /// Changes the RAM cache limit and trims RAM if it is now over the limit.
    pub fn set_limit_mb(&self, mb: usize, unlimited_cache: bool) {
        let n = if unlimited_cache { usize::MAX } else { entries_for_mb(mb) };
        self.cache.max_ram.store(n, Ordering::Relaxed);
        self.cache.shrink();
        eprintln!("Cache limit set to {} MB", if unlimited_cache { usize::MAX } else { mb });
    }

    /// Number of indexed folders (all of them, not just the ones currently in RAM; approximate).
    pub fn len(&self) -> usize {
        self.cache.len()
    }

    /// Cached size regardless of age (shown immediately; fresh value follows).
    /// A folder that is not in RAM costs one 4 KiB read from the file, so call this once per
    /// folder (when a listing loads), not for every row on every frame.
    pub fn lookup(&self, path: &Path) -> Option<u64> {
        self.cache.get(path).map(|c| c.size)
    }

    /// Drop the path and every ancestor: their totals are no longer correct.
    pub fn invalidate(&self, path: &Path) {
        self.cache.remove_with_ancestors(path);
        self.cache_dirty.store(true, Ordering::Relaxed);
        self.kick_flush();
    }

    /// Drop `dir`, everything below it and all its ancestors (used when too many changes happened
    /// to list them individually).
    pub fn invalidate_tree(&self, dir: &Path) {
        self.cache.remove_tree(dir);
        self.cache_dirty.store(true, Ordering::Relaxed);
        self.kick_flush();
    }

    /// Writes the buffered changes in a background thread once enough have piled up. (Scan
    /// threads do this themselves; this covers invalidations coming from the UI thread.)
    fn kick_flush(&self) {
        if self.cache.pending_len() >= store::FLUSH_AT && !self.cache.bg_flush.swap(true, Ordering::SeqCst) {
            let s = Arc::clone(&self.cache);
            std::thread::spawn(move || {
                s.flush();
                s.bg_flush.store(false, Ordering::SeqCst);
            });
        }
    }

    /// True while a scan started by `start()` still has a live worker thread.
    pub fn is_running(&self) -> bool {
        self.live.load(Ordering::SeqCst) > 0
    }

    /// Forgets everything: stops a running scan and empties the index (RAM and file).
    /// (The file stays open, so it is reset rather than deleted.)
    pub fn delete_index(&self, _dir: &Path) {
        self.generation.fetch_add(1, Ordering::SeqCst);
        self.cache.clear();
        self.cache_dirty.store(false, Ordering::Relaxed);
    }

    pub fn is_dirty(&self) -> bool {
        self.cache_dirty.load(Ordering::Relaxed)
    }

    pub fn clear(&self) {
        self.cache.clear();
        self.cache_dirty.store(true, Ordering::Relaxed);
    }

    /// Cancels any running scan and starts a new one for `folders`. Up-to-date folders return instantly.
    pub fn start(&self, folders: Vec<PathBuf>) {
        let my_gen = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        if folders.is_empty() {
            return;
        }
        // A small worker pool takes folders from a shared queue, so one huge folder (Windows,
        // AppData, ...) no longer holds up every row behind it.
        let workers = WORKERS.min(folders.len());
        let queue = Arc::new(Mutex::new(folders.into_iter().collect::<VecDeque<_>>()));
        for _ in 0..workers {
            let queue = Arc::clone(&queue);
            let cache = Arc::clone(&self.cache);
            let generation = Arc::clone(&self.generation);
            let dirty = Arc::clone(&self.cache_dirty);
            let tx = self.tx.clone();
            let ctx = self.ctx.clone();
            self.live.fetch_add(1, Ordering::SeqCst);
            let guard = Live(Arc::clone(&self.live));
            std::thread::spawn(move || {
                let _guard = guard;
                lower_priority();
                loop {
                    if generation.load(Ordering::Relaxed) != my_gen {
                        return;
                    }
                    let Some(folder) = queue.lock().ok().and_then(|mut q| q.pop_front()) else { return };
                    let stamp = fs::metadata(&folder).and_then(|m| m.modified()).map(ns).unwrap_or(0);
                    let now = now_secs();
                    let fresh = cache.get(&folder).filter(|c| is_fresh(c, stamp, now));
                    let size = match fresh {
                        Some(c) => c.size,
                        None => match scan(&folder, stamp, &cache, &generation, my_gen, &dirty) {
                            Some(s) => s,
                            None => return, // cancelled
                        },
                    };
                    if tx.send(IndexMsg::Size(folder, size)).is_err() {
                        return;
                    }
                    ctx.request_repaint();
                }
            });
        }
    }

    // ---------------------------------------------------------------- persistence

    /// Results are buffered in RAM and written in batches while scanning; this writes what is
    /// left and makes it permanent (fsync), in a background thread unless `wait` is set
    /// (use that on exit).
    pub fn save_if_dirty(&self, _dir: &Path, wait: bool) {
        if !self.cache_dirty.swap(false, Ordering::Relaxed) {
            return;
        }
        let store = Arc::clone(&self.cache);
        if wait {
            store.sync();
        } else {
            std::thread::spawn(move || store.sync());
        }
    }
}

fn is_fresh(c: &CachedFolder, stamp: u64, now: u64) -> bool {
    c.modified_ns == stamp && now.saturating_sub(c.scanned_at) < TTL_SECS
}

struct Frame {
    path: PathBuf,
    rd: fs::ReadDir,
    total: u64,
    stamp: u64,
}

enum Step {
    Add(u64),
    Descend(PathBuf, u64),
    Skip,
    End,
}

/// Iterative post-order walk. Returns None if cancelled. Every folder that is finished is stored.
fn scan(root: &Path, root_stamp: u64, cache: &Cache, generation: &AtomicU64, my_gen: u64, dirty: &AtomicBool) -> Option<u64> {
    let now = now_secs();
    let mut pending: Vec<(PathBuf, CachedFolder)> = Vec::new();
    let flush = |pending: &mut Vec<(PathBuf, CachedFolder)>| {
        if pending.is_empty() {
            return;
        }
        cache.put_many(std::mem::take(pending));
        dirty.store(true, Ordering::Relaxed);
    };

    let Ok(rd) = fs::read_dir(root) else { return Some(0) };
    let mut stack = vec![Frame { path: root.to_path_buf(), rd, total: 0, stamp: root_stamp }];
    let mut counter = 0u32;

    loop {
        if generation.load(Ordering::Relaxed) != my_gen {
            flush(&mut pending); // keep the completed sub-folders
            return None;
        }
        counter += 1;
        if counter % 4096 == 0 {
            // limit rewrites
            if pending.len() >= 4000 {
                flush(&mut pending);
            }
            // Prevents full hardware usage.
            std::thread::sleep(Duration::from_millis(2));
        }

        let step = {
            let top = stack.last_mut()?;
            match top.rd.next() {
                None => Step::End,
                Some(Err(_)) => Step::Skip,
                Some(Ok(de)) => match de.file_type() {
                    Ok(ft) if ft.is_symlink() => Step::Skip, // symlinks & junctions: avoid loops/double counting
                    Ok(ft) if ft.is_file() => Step::Add(de.metadata().map(|m| m.len()).unwrap_or(0)),
                    Ok(ft) if ft.is_dir() => {
                        let stamp = de.metadata().and_then(|m| m.modified()).map(ns).unwrap_or(0);
                        Step::Descend(de.path(), stamp)
                    }
                    _ => Step::Skip,
                },
            }
        };

        match step {
            Step::Skip => {}
            Step::Add(n) => {
                if let Some(top) = stack.last_mut() {
                    top.total = top.total.saturating_add(n);
                }
            }
            Step::Descend(path, stamp) => {
                let hit = cache.get(&path).filter(|c| is_fresh(c, stamp, now));
                if let Some(hit) = hit {
                    if let Some(top) = stack.last_mut() {
                        top.total = top.total.saturating_add(hit.size);
                    }
                } else if let Ok(rd) = fs::read_dir(&path) {
                    stack.push(Frame { path, rd, total: 0, stamp });
                }
            }
            Step::End => {
                let done = stack.pop()?;
                pending.push((done.path, CachedFolder { size: done.total, modified_ns: done.stamp, scanned_at: now }));
                match stack.last_mut() {
                    Some(parent) => parent.total = parent.total.saturating_add(done.total),
                    None => {
                        flush(&mut pending);
                        return Some(done.total);
                    }
                }
            }
        }
    }
}

#[cfg(windows)]
fn lower_priority() {
    // THREAD_PRIORITY_BELOW_NORMAL (-1): keeps the UI responsive without starving the scan.
    // (THREAD_MODE_BACKGROUND_BEGIN also drops I/O to "very low", which can make a scan crawl
    // whenever anything else touches the disk.)
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetCurrentThread() -> isize;
        fn SetThreadPriority(h: isize, p: i32) -> i32;
    }
    unsafe {
        SetThreadPriority(GetCurrentThread(), -1);
    }
}
#[cfg(not(windows))]
fn lower_priority() {}

// ----------------------------------------------------------------------------------------------
// File watcher: live refresh + cache invalidation

/// Most distinct changed paths remembered between two UI frames; more than that collapses into
/// a single "something changed" marker (the empty path).
const MAX_PENDING: usize = 2048;

pub struct DirWatcher {
    watcher: RecommendedWatcher,
    current: Option<PathBuf>,
    pending: Arc<Mutex<HashSet<PathBuf>>>,
}

impl DirWatcher {
    pub fn new(ctx: egui::Context) -> Option<Self> {
        // Events go into a bounded set that the UI drains once per frame. (An unbounded channel
        // grows without limit while the window is minimised or hidden and nobody reads it.)
        let pending: Arc<Mutex<HashSet<PathBuf>>> = Arc::new(Mutex::new(HashSet::new()));
        let sink = Arc::clone(&pending);
        let watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
            if let Ok(ev) = res {
                if matches!(ev.kind, EventKind::Access(_)) || ev.paths.is_empty() {
                    return;
                }
                if let Ok(mut set) = sink.lock() {
                    for p in ev.paths {
                        if set.len() < MAX_PENDING {
                            set.insert(p);
                        } else {
                            set.insert(PathBuf::new());
                            break;
                        }
                    }
                }
                ctx.request_repaint_after(Duration::from_millis(300));
            }
        })
        .ok()?;
        Some(Self { watcher, current: None, pending })
    }

    /// Changed paths since the last call. An empty path means "too many to list".
    pub fn take(&self) -> Vec<PathBuf> {
        match self.pending.lock() {
            Ok(mut s) => std::mem::take(&mut *s).into_iter().collect(),
            Err(_) => Vec::new(),
        }
    }

    pub fn watch(&mut self, dir: &Path) {
        if self.current.as_deref() == Some(dir) {
            return;
        }
        if let Some(old) = self.current.take() {
            let _ = self.watcher.unwatch(&old);
        }
        // Drive roots see constant system churn; only watch their direct children.
        // Network shares are watched shallowly too: recursive watches over SMB/NFS are slow and unreliable.
        let mode = if dir.parent().is_none() || crate::drives::is_network_path(dir) {
            RecursiveMode::NonRecursive
        } else {
            RecursiveMode::Recursive
        };
        if self.watcher.watch(dir, mode).is_ok() {
            self.current = Some(dir.to_path_buf());
        }
    }
}
