//! A small deterministic generator, so a failing scenario replays exactly.

/// xorshift64*: adequate for scenario loss and fading, not for anything secret.
#[derive(Debug, Clone)]
pub struct Rng(u64);

impl Rng {
    /// Seed the generator. Any seed is usable, including zero.
    #[must_use]
    pub const fn new(seed: u64) -> Self {
        Self(seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1))
    }

    fn next_u64(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(2_685_821_657_736_338_717)
    }

    /// A uniform value in `[0, 1)`.
    pub fn next_f64(&mut self) -> f64 {
        // Shifted to 53 bits first, which is exactly f64's mantissa, so the
        // conversion is lossless by construction rather than by luck.
        #[allow(clippy::cast_precision_loss)]
        {
            (self.next_u64() >> 11) as f64 / 9_007_199_254_740_992.0
        }
    }

    /// A uniform integer in `[0, bound)`, or zero when `bound` is zero.
    pub fn below(&mut self, bound: u64) -> u64 {
        if bound == 0 {
            0
        } else {
            self.next_u64() % bound
        }
    }

    /// Two independent standard normal deviates, by Box-Muller.
    pub fn normal_pair(&mut self) -> (f64, f64) {
        // The log needs a strictly positive argument; next_f64 can return
        // exactly zero, so the sample is nudged off the boundary rather than
        // left to produce an infinity once every few billion draws.
        let uniform = self.next_f64().max(f64::MIN_POSITIVE);
        let angle = core::f64::consts::TAU * self.next_f64();
        let radius = (-2.0 * uniform.ln()).sqrt();
        (radius * angle.cos(), radius * angle.sin())
    }
}
