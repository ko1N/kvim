//! Stable non-cryptographic hashes for bounded local persistence formats.
//!
//! These hashes identify data inside kvim. They do not authenticate data and
//! must not be used for security decisions.

/// Returns the FNV-1a 64-bit hash of one byte sequence.
#[must_use]
pub(crate) fn content_hash(bytes: &[u8]) -> u64 {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;

    let mut hash = OFFSET;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(PRIME);
    }
    hash
}
