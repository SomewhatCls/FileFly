//! Small building blocks of the index file: key hashing, CRC and varints.

use std::{
    collections::HashMap,
    ffi::OsStr,
    hash::{BuildHasherDefault, Hasher},
    path::Path,
};

/// Width of a path key in bits.
pub(super) const KEY_BITS: u32 = 64;

const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

pub(super) fn mix64(mut z: u64) -> u64 {
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

/// Running hash of a path, before the final scramble. It is built component by component (like
/// `Path` equality works, so a trailing separator makes no difference), which means a child's state
/// is its parent's state plus the child's name: the scan never has to hash a whole path again.
/// Written by hand, because `std`'s hashers may change between Rust versions and the resulting key
/// is stored on disk.
pub fn key_state(p: &Path) -> u64 {
    p.components().fold(FNV_OFFSET, |h, c| extend_name(h, c.as_os_str()))
}

fn feed(h: &mut u64, bytes: &[u8]) {
    for &b in bytes {
        *h = (*h ^ b as u64).wrapping_mul(FNV_PRIME);
    }
}

fn end_component(h: u64) -> u64 {
    (h ^ 0xFF).wrapping_mul(FNV_PRIME) // 0xFF never occurs in UTF-8
}

/// The state of the path `parent/name` from the state of `parent`.
pub fn extend_name(mut h: u64, name: &OsStr) -> u64 {
    feed(&mut h, name.to_string_lossy().as_bytes());
    end_component(h)
}

/// Same as `extend_name` for a UTF-16 name (Windows directory listings), without allocating.
/// A lone surrogate counts as U+FFFD, exactly like `OsStr::to_string_lossy`.
#[cfg(windows)]
pub fn extend_wide(mut h: u64, name: &[u16]) -> u64 {
    let mut b = [0u8; 4];
    for c in char::decode_utf16(name.iter().copied()) {
        feed(&mut h, c.unwrap_or('\u{FFFD}').encode_utf8(&mut b).as_bytes());
    }
    end_component(h)
}

/// The 64-bit key of a path state.
pub fn finish_key(h: u64) -> u64 {
    mix64(h) >> (64 - KEY_BITS)
}

pub fn key_of(p: &Path) -> u64 {
    finish_key(key_state(p))
}

/// The keys are already well mixed, so the maps use them as they are.
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
        self.0 = i;
    }
}
pub(super) type KeyMap<V> = HashMap<u64, V, BuildHasherDefault<KeyHasher>>;

const fn crc_tables() -> [[u32; 256]; 8] {
    let mut t = [[0u32; 256]; 8];
    let mut i = 0;
    while i < 256 {
        let mut c = i as u32;
        let mut k = 0;
        while k < 8 {
            c = if c & 1 != 0 { 0xEDB8_8320 ^ (c >> 1) } else { c >> 1 };
            k += 1;
        }
        t[0][i] = c;
        i += 1;
    }
    i = 0;
    while i < 256 {
        let mut c = t[0][i];
        let mut j = 1;
        while j < 8 {
            c = t[0][(c & 0xFF) as usize] ^ (c >> 8);
            t[j][i] = c;
            j += 1;
        }
        i += 1;
    }
    t
}
static CRC: [[u32; 256]; 8] = crc_tables();

/// CRC-32 (IEEE), eight bytes per step.
pub(super) fn crc32(data: &[u8]) -> u32 {
    let mut c = !0u32;
    let mut chunks = data.chunks_exact(8);
    for ch in &mut chunks {
        let lo = u32::from_le_bytes([ch[0], ch[1], ch[2], ch[3]]) ^ c;
        let hi = u32::from_le_bytes([ch[4], ch[5], ch[6], ch[7]]);
        c = CRC[7][(lo & 0xFF) as usize]
            ^ CRC[6][((lo >> 8) & 0xFF) as usize]
            ^ CRC[5][((lo >> 16) & 0xFF) as usize]
            ^ CRC[4][(lo >> 24) as usize]
            ^ CRC[3][(hi & 0xFF) as usize]
            ^ CRC[2][((hi >> 8) & 0xFF) as usize]
            ^ CRC[1][((hi >> 16) & 0xFF) as usize]
            ^ CRC[0][(hi >> 24) as usize];
    }
    for &b in chunks.remainder() {
        c = CRC[0][((c ^ b as u32) & 0xFF) as usize] ^ (c >> 8);
    }
    !c
}

pub(super) fn put_varint(out: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        out.push((v as u8) | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

pub(super) fn get_varint(b: &[u8], pos: &mut usize) -> Option<u64> {
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
