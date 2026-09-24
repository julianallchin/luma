//! Seeded lattice noise, a pure function of coordinates. These f32 value-noise
//! bases retain their lattice keys and interpolation precision across migration.

pub(crate) fn hash(seed: u64, value: u64) -> u64 {
    let mut x = seed ^ value;
    x = (x ^ (x >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94d049bb133111eb);
    x ^ (x >> 31)
}
fn signed(hash: u64) -> f32 {
    (hash as f64 / u64::MAX as f64) as f32 * 2. - 1.
}
fn smooth(x: f32) -> f32 {
    x * x * (3. - 2. * x)
}
fn one(x: f32, seed: u64) -> f32 {
    let lo = x.floor() as i64;
    let t = smooth(x - lo as f32);
    let a = signed(hash(seed, lo as u64));
    let b = signed(hash(seed, (lo + 1) as u64));
    a + t * (b - a)
}
fn fractal(seed: u64, octaves: f64, stride: u64, mut sample: impl FnMut(f32, u64) -> f32) -> f64 {
    let mut total = 0_f32;
    let mut frequency = 1_f32;
    let mut amplitude = 1_f32;
    let mut maximum = 0_f32;
    for octave in 0..(octaves as f32).clamp(1., 8.) as u32 {
        total += sample(frequency, hash(seed, u64::from(octave) * stride)) * amplitude;
        maximum += amplitude;
        amplitude *= 0.5;
        frequency *= 2.;
    }
    f64::from(total / maximum)
}
pub(crate) fn noise1(x: f64, octaves: f64, seed: u64) -> f64 {
    fractal(seed, octaves, 7919, |frequency, key| {
        one(x as f32 * frequency, key)
    })
}
