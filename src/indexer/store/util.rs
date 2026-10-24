//! Small building blocks of the index file: key hashing, CRC and varints.

use std::{
    collections::HashMap,
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
pub(super) type KeyMap<V> = HashMap<u64, V, BuildHasherDefault<KeyHasher>>;

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

pub(super) fn crc32(data: &[u8]) -> u32 {
    let mut c = !0u32;
    for &b in data {
        c = CRC[((c ^ b as u32) & 0xFF) as usize] ^ (c >> 8);
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
