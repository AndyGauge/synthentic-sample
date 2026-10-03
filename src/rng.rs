/// SplitMix64. Hand-rolled so output is stable across dependency upgrades:
/// the same seed must yield the same training row forever.
#[derive(Clone, Debug)]
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self(seed)
    }

    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform in `0..n` (`n` must be > 0). Modulo bias is irrelevant at these sizes.
    pub fn below(&mut self, n: usize) -> usize {
        (self.next_u64() % n as u64) as usize
    }

    /// Inclusive range.
    pub fn range(&mut self, lo: usize, hi: usize) -> usize {
        lo + self.below(hi - lo + 1)
    }

    pub fn chance(&mut self, percent: usize) -> bool {
        self.below(100) < percent
    }

    pub fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[self.below(items.len())]
    }

    /// `k` distinct items, order randomised.
    pub fn sample<T: Clone>(&mut self, items: &[T], k: usize) -> Vec<T> {
        let mut pool = items.to_vec();
        let mut out = Vec::with_capacity(k);
        for _ in 0..k.min(pool.len()) {
            out.push(pool.swap_remove(self.below(pool.len())));
        }
        out
    }
}

/// FNV-1a: a stable hash for deriving seeds from ids (never changes between releases).
pub fn hash64(s: &str) -> u64 {
    s.bytes().fold(0xcbf2_9ce4_8422_2325, |h, b| (h ^ b as u64).wrapping_mul(0x0000_0100_0000_01b3))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stable_hash() {
        assert_eq!(hash64(""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(hash64("a"), 0xaf63_dc4c_8601_ec8c);
    }

    #[test]
    fn stable_stream() {
        // Golden values: changing these breaks reproducibility of existing datasets.
        let mut r = Rng::new(0);
        assert_eq!(r.next_u64(), 0xE220_A839_7B1D_CDAF);
        assert_eq!(r.next_u64(), 0x6E78_9E6A_A1B9_65F4);
    }
}
