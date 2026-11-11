//! `Random` — Love2D's `love.math` random numbers and noise.
//!
//! Saule's own `Math.random` is there for everyday use. This one is seedable
//! and repeatable, which is what procedural generation needs — the same seed
//! has to build the same dungeon — and it carries the coherent noise Love2D
//! has as `love.math.noise`.

use std::cell::RefCell;

use saule_sdk::prelude::*;
use saule_sdk::saule_export;

/// xoshiro256** — small, fast, and good enough for games. Not for secrets.
#[derive(Clone)]
pub(crate) struct Rng {
    s: [u64; 4],
    seed: i64,
}

impl Rng {
    pub(crate) fn new(seed: i64) -> Self {
        // SplitMix64 spreads one seed over the four words, so seeds that are
        // close together still give unrelated streams.
        let mut z = seed as u64;
        let mut next = || {
            z = z.wrapping_add(0x9E37_79B9_7F4A_7C15);
            let mut x = z;
            x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            x ^ (x >> 31)
        };
        Rng {
            s: [next(), next(), next(), next()],
            seed,
        }
    }

    pub(crate) fn from_clock() -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as i64)
            .unwrap_or(0x5EED);
        Rng::new(nanos)
    }

    fn next_u64(&mut self) -> u64 {
        let result = self.s[1].wrapping_mul(5).rotate_left(7).wrapping_mul(9);
        let t = self.s[1] << 17;
        self.s[2] ^= self.s[0];
        self.s[3] ^= self.s[1];
        self.s[1] ^= self.s[2];
        self.s[0] ^= self.s[3];
        self.s[2] ^= t;
        self.s[3] = self.s[3].rotate_left(45);
        result
    }

    /// Uniform in `[0, 1)`.
    pub(crate) fn float(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 * (1.0 / (1u64 << 53) as f64)
    }

    /// Uniform in `[lo, hi)`.
    pub(crate) fn range(&mut self, lo: f64, hi: f64) -> f64 {
        lo + (hi - lo) * self.float()
    }

    /// Uniform integer in `[lo, hi]`.
    pub(crate) fn int(&mut self, lo: i64, hi: i64) -> Result<i64, String> {
        if hi < lo {
            return Err(format!("the range {lo}..{hi} is empty"));
        }
        let span = (hi as i128 - lo as i128 + 1) as u128;
        Ok((lo as i128 + (self.next_u64() as u128 % span) as i128) as i64)
    }

    /// Normally distributed, by Box–Muller.
    pub(crate) fn normal(&mut self, stddev: f64, mean: f64) -> f64 {
        let u1 = 1.0 - self.float();
        let u2 = self.float();
        mean + stddev * (-2.0 * u1.ln()).sqrt() * (std::f64::consts::TAU * u2).cos()
    }
}

thread_local! {
    static GLOBAL: RefCell<Rng> = RefCell::new(Rng::from_clock());
}

fn global<R>(f: impl FnOnce(&mut Rng) -> R) -> R {
    GLOBAL.with(|g| f(&mut g.borrow_mut()))
}

/// `Random.random()` — a float in `[0, 1)`.
#[saule_export(class = "Random", name = "random")]
fn random_random() -> f64 {
    global(Rng::float)
}

/// `Random.randomInt(min, max)` — an integer in `[min, max]`. Love2D's
/// `love.math.random(min, max)`.
#[saule_export(class = "Random", name = "randomInt")]
fn random_random_int(min: i64, max: i64) -> Result<i64, String> {
    global(|r| r.int(min, max)).map_err(|e| format!("Random.randomInt: {e}"))
}

/// `Random.randomRange(min, max)` — a float in `[min, max)`.
#[saule_export(class = "Random", name = "randomRange")]
fn random_random_range(min: f64, max: f64) -> f64 {
    global(|r| r.range(min, max))
}

/// `Random.randomNormal([stddev, mean])` — normally distributed; `stddev`
/// defaults to 1 and `mean` to 0.
#[saule_export(class = "Random", name = "randomNormal")]
fn random_random_normal(stddev: Option<f64>, mean: Option<f64>) -> f64 {
    global(|r| r.normal(stddev.unwrap_or(1.0), mean.unwrap_or(0.0)))
}

/// `Random.setSeed(seed)` — restart the shared generator, so what follows
/// repeats exactly.
#[saule_export(class = "Random", name = "setSeed")]
fn random_set_seed(seed: i64) {
    global(|r| *r = Rng::new(seed));
}

/// `Random.getSeed()` — the seed the shared generator last started from.
#[saule_export(class = "Random", name = "getSeed")]
fn random_get_seed() -> i64 {
    global(|r| r.seed)
}

/// `Random.newRandomGenerator([seed])` — a generator of its own, so one
/// system's draws do not disturb another's. Seeded from the clock when no
/// seed is given.
#[saule_export(class = "Random", name = "newRandomGenerator")]
fn random_new_generator(seed: Option<i64>) -> RandomGenerator {
    RandomGenerator::new(seed)
}

/// `Random.noise(x [, y, z])` — smooth noise in `[0, 1]`: Perlin noise, so
/// nearby inputs give nearby outputs. Love2D's `love.math.noise`.
#[saule_export(class = "Random", name = "noise")]
fn random_noise(x: f64, y: Option<f64>, z: Option<f64>) -> f64 {
    let v = match (y, z) {
        (None, _) => perlin3(x, 0.0, 0.0),
        (Some(y), None) => perlin3(x, y, 0.0),
        (Some(y), Some(z)) => perlin3(x, y, z),
    };
    (v * 0.5 + 0.5).clamp(0.0, 1.0)
}

/// A seeded random number generator of its own.
#[saule_class]
pub struct RandomGenerator {
    rng: Rng,
}

#[saule_methods]
impl RandomGenerator {
    /// `RandomGenerator([seed])`.
    pub fn new(seed: Option<i64>) -> Self {
        RandomGenerator {
            rng: seed.map_or_else(Rng::from_clock, Rng::new),
        }
    }

    /// A float in `[0, 1)`.
    pub fn random(&mut self) -> f64 {
        self.rng.float()
    }

    /// An integer in `[min, max]`.
    pub fn random_int(&mut self, min: i64, max: i64) -> Result<i64, String> {
        self.rng
            .int(min, max)
            .map_err(|e| format!("RandomGenerator.randomInt: {e}"))
    }

    /// A float in `[min, max)`.
    pub fn random_range(&mut self, min: f64, max: f64) -> f64 {
        self.rng.range(min, max)
    }

    /// Normally distributed.
    pub fn random_normal(&mut self, stddev: Option<f64>, mean: Option<f64>) -> f64 {
        self.rng.normal(stddev.unwrap_or(1.0), mean.unwrap_or(0.0))
    }

    /// Restart from `seed`.
    pub fn set_seed(&mut self, seed: i64) {
        self.rng = Rng::new(seed);
    }

    /// The seed it last started from.
    pub fn get_seed(&self) -> i64 {
        self.rng.seed
    }
}

// ---------------------------------------------------------------------------
// Perlin noise (Ken Perlin's improved noise, 2002)
// ---------------------------------------------------------------------------

const PERM: [u8; 256] = [
    151, 160, 137, 91, 90, 15, 131, 13, 201, 95, 96, 53, 194, 233, 7, 225, 140, 36, 103, 30, 69,
    142, 8, 99, 37, 240, 21, 10, 23, 190, 6, 148, 247, 120, 234, 75, 0, 26, 197, 62, 94, 252, 219,
    203, 117, 35, 11, 32, 57, 177, 33, 88, 237, 149, 56, 87, 174, 20, 125, 136, 171, 168, 68, 175,
    74, 165, 71, 134, 139, 48, 27, 166, 77, 146, 158, 231, 83, 111, 229, 122, 60, 211, 133, 230,
    220, 105, 92, 41, 55, 46, 245, 40, 244, 102, 143, 54, 65, 25, 63, 161, 1, 216, 80, 73, 209, 76,
    132, 187, 208, 89, 18, 169, 200, 196, 135, 130, 116, 188, 159, 86, 164, 100, 109, 198, 173,
    186, 3, 64, 52, 217, 226, 250, 124, 123, 5, 202, 38, 147, 118, 126, 255, 82, 85, 212, 207, 206,
    59, 227, 47, 16, 58, 17, 182, 189, 28, 42, 223, 183, 170, 213, 119, 248, 152, 2, 44, 154, 163,
    70, 221, 153, 101, 155, 167, 43, 172, 9, 129, 22, 39, 253, 19, 98, 108, 110, 79, 113, 224, 232,
    178, 185, 112, 104, 218, 246, 97, 228, 251, 34, 242, 193, 238, 210, 144, 12, 191, 179, 162,
    241, 81, 51, 145, 235, 249, 14, 239, 107, 49, 192, 214, 31, 181, 199, 106, 157, 184, 84, 204,
    176, 115, 121, 50, 45, 127, 4, 150, 254, 138, 236, 205, 93, 222, 114, 67, 29, 24, 72, 243, 141,
    128, 195, 78, 66, 215, 61, 156, 180,
];

fn p(i: usize) -> usize {
    PERM[i & 255] as usize
}

fn fade(t: f64) -> f64 {
    t * t * t * (t * (t * 6.0 - 15.0) + 10.0)
}

fn lerp(t: f64, a: f64, b: f64) -> f64 {
    a + t * (b - a)
}

fn grad(hash: usize, x: f64, y: f64, z: f64) -> f64 {
    let h = hash & 15;
    let u = if h < 8 { x } else { y };
    let v = if h < 4 {
        y
    } else if h == 12 || h == 14 {
        x
    } else {
        z
    };
    (if h & 1 == 0 { u } else { -u }) + (if h & 2 == 0 { v } else { -v })
}

/// Improved Perlin noise in roughly `[-1, 1]`.
pub(crate) fn perlin3(x: f64, y: f64, z: f64) -> f64 {
    let (xf, yf, zf) = (x.floor(), y.floor(), z.floor());
    let (xi, yi, zi) = (
        (xf as i64 & 255) as usize,
        (yf as i64 & 255) as usize,
        (zf as i64 & 255) as usize,
    );
    let (x, y, z) = (x - xf, y - yf, z - zf);
    let (u, v, w) = (fade(x), fade(y), fade(z));

    let a = p(xi) + yi;
    let aa = p(a) + zi;
    let ab = p(a + 1) + zi;
    let b = p(xi + 1) + yi;
    let ba = p(b) + zi;
    let bb = p(b + 1) + zi;

    lerp(
        w,
        lerp(
            v,
            lerp(u, grad(p(aa), x, y, z), grad(p(ba), x - 1.0, y, z)),
            lerp(
                u,
                grad(p(ab), x, y - 1.0, z),
                grad(p(bb), x - 1.0, y - 1.0, z),
            ),
        ),
        lerp(
            v,
            lerp(
                u,
                grad(p(aa + 1), x, y, z - 1.0),
                grad(p(ba + 1), x - 1.0, y, z - 1.0),
            ),
            lerp(
                u,
                grad(p(ab + 1), x, y - 1.0, z - 1.0),
                grad(p(bb + 1), x - 1.0, y - 1.0, z - 1.0),
            ),
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_seed_repeats_its_sequence() {
        let mut a = Rng::new(42);
        let mut b = Rng::new(42);
        for _ in 0..100 {
            assert_eq!(a.next_u64(), b.next_u64());
        }
        assert_ne!(Rng::new(1).next_u64(), Rng::new(2).next_u64());
    }

    #[test]
    fn integers_stay_inside_their_inclusive_range() {
        let mut r = Rng::new(7);
        let mut seen = [false; 6];
        for _ in 0..1000 {
            let v = r.int(1, 6).unwrap();
            assert!((1..=6).contains(&v));
            seen[(v - 1) as usize] = true;
        }
        assert!(seen.iter().all(|&s| s), "every face should come up");
        assert!(r.int(5, 4).is_err());
    }

    #[test]
    fn noise_is_smooth_and_bounded() {
        for i in 0..200 {
            let x = i as f64 * 0.37;
            let n = random_noise(x, Some(x * 0.5), None);
            assert!((0.0..=1.0).contains(&n));
            let near = random_noise(x + 0.001, Some(x * 0.5), None);
            assert!((n - near).abs() < 0.01, "noise jumped at {x}");
        }
    }
}
