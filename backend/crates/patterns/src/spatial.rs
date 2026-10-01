//! Stable per-head random draws.

/// Stable hash independent of Rust's Hash implementation, traversal order,
/// process, or frame. Cell identity makes adjacent pixels independent.
pub(crate) fn threshold(cell: &str, seed: u64) -> f64 {
    let mut h = 0xcbf29ce484222325_u64 ^ seed;
    for b in cell.bytes() {
        h = (h ^ u64::from(b)).wrapping_mul(0x100000001b3);
    }
    h = (h ^ (h >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
    h = (h ^ (h >> 27)).wrapping_mul(0x94d049bb133111eb);
    h ^= h >> 31;
    // Strictly interior, leaving exact progress endpoints unambiguous.
    ((h >> 11) as f64 + 0.5) / 9007199254740992.0
}

pub(crate) fn epoch_seed(seed: u64, epoch: i64) -> u64 {
    let mut value = seed ^ (epoch as u64).wrapping_mul(0x9e3779b97f4a7c15);
    value = (value ^ (value >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
    value = (value ^ (value >> 27)).wrapping_mul(0x94d049bb133111eb);
    value ^ (value >> 31)
}
