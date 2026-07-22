use sha2::{Digest, Sha256};

pub const QUOTE_BLOOM_BITS: usize = 4096;
pub const QUOTE_BLOOM_BYTES: usize = QUOTE_BLOOM_BITS / 8;
pub const QUOTE_BLOOM_HASHES: usize = 4;

/// A deterministic candidate filter for raw-byte quote verification.
///
/// Each overlapping byte trigram sets four positions. The first and second
/// big-endian `u32`s of its SHA-256 digest are the double-hash seeds:
/// `(h1 + i * h2) mod 4096`, for `i` in `0..4`. Bits are serialized least
/// significant bit first within each byte.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QuoteBloom {
    bytes: [u8; QUOTE_BLOOM_BYTES],
}

impl QuoteBloom {
    pub fn from_content(content: &[u8]) -> Self {
        let mut bloom = Self::default();
        for trigram in content.windows(3) {
            bloom.insert_trigram(trigram);
        }
        bloom
    }

    pub const fn from_bytes(bytes: [u8; QUOTE_BLOOM_BYTES]) -> Self {
        Self { bytes }
    }

    pub fn might_contain(&self, query: &[u8]) -> bool {
        query
            .windows(3)
            .all(|trigram| self.might_contain_trigram(trigram))
    }

    pub const fn as_bytes(&self) -> &[u8; QUOTE_BLOOM_BYTES] {
        &self.bytes
    }

    pub const fn into_bytes(self) -> [u8; QUOTE_BLOOM_BYTES] {
        self.bytes
    }

    fn insert_trigram(&mut self, trigram: &[u8]) {
        for position in bloom_positions(trigram) {
            self.bytes[position / 8] |= 1 << (position % 8);
        }
    }

    fn might_contain_trigram(&self, trigram: &[u8]) -> bool {
        bloom_positions(trigram)
            .into_iter()
            .all(|position| self.bytes[position / 8] & (1 << (position % 8)) != 0)
    }
}

impl Default for QuoteBloom {
    fn default() -> Self {
        Self {
            bytes: [0; QUOTE_BLOOM_BYTES],
        }
    }
}

fn bloom_positions(trigram: &[u8]) -> [usize; QUOTE_BLOOM_HASHES] {
    debug_assert_eq!(trigram.len(), 3);

    let digest = Sha256::digest(trigram);
    let first = u32::from_be_bytes([digest[0], digest[1], digest[2], digest[3]]);
    let second = u32::from_be_bytes([digest[4], digest[5], digest[6], digest[7]]);

    std::array::from_fn(|index| {
        first.wrapping_add(second.wrapping_mul(index as u32)) as usize % QUOTE_BLOOM_BITS
    })
}
