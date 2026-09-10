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
fn three([x, y, z]: [f32; 3], seed: u64) -> f32 {
    let low = [x, y, z].map(|v| v.floor() as i64);
    let t = std::array::from_fn::<_, 3, _>(|i| smooth([x, y, z][i] - low[i] as f32));
    let at = |dx, dy, dz| {
        signed(hash(
            hash(hash(seed, (low[0] + dx) as u64), (low[1] + dy) as u64),
            (low[2] + dz) as u64,
        ))
    };
    let mix = |a, b, t| a + t * (b - a);
    let xy0 = mix(
        mix(at(0, 0, 0), at(1, 0, 0), t[0]),
        mix(at(0, 1, 0), at(1, 1, 0), t[0]),
        t[1],
    );
    let xy1 = mix(
        mix(at(0, 0, 1), at(1, 0, 1), t[0]),
        mix(at(0, 1, 1), at(1, 1, 1), t[0]),
        t[1],
    );
    mix(xy0, xy1, t[2])
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
pub(crate) fn noise3(x: f64, y: f64, z: f64, octaves: f64, seed: u64) -> f64 {
    fractal(seed, octaves, 12345, |frequency, key| {
        three([x, y, z].map(|v| v as f32 * frequency), key)
    })
}
