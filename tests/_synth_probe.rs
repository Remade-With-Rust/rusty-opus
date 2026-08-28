//! Temporary diagnostic: hash the SYNTHESISED INPUT, before any encoding.
//!
//! If this differs between platforms, the oracle's failure is in its own test
//! fixture -- the input is generated with `f32::sin`, which resolves to the
//! host libm -- and not in the encoder at all.

struct Lcg(u64);
impl Lcg {
    fn next_f32(&mut self) -> f32 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        ((self.0 >> 40) as f32 / (1u32 << 23) as f32) - 1.0
    }
}

fn synth_music(rate: u32, channels: usize, secs: f32) -> Vec<f32> {
    let n = (rate as f32 * secs) as usize;
    let mut rng = Lcg(0x5EED_CAFE_F00D_D00D);
    let mut out = vec![0.0f32; n * channels];
    let freqs = [220.0f32, 277.18, 329.63, 440.0, 554.37, 659.25];
    for i in 0..n {
        let t = i as f32 / rate as f32;
        let vib = (2.0 * std::f32::consts::PI * 5.0 * t).sin() * 0.002;
        let beat = if (t * 2.0).fract() < 0.05 { 1.8 } else { 1.0 };
        let mut s = 0.0f32;
        for (k, f) in freqs.iter().enumerate() {
            let ph = 2.0 * std::f32::consts::PI * f * (1.0 + vib) * t;
            s += ph.sin() * (0.22 / (k as f32 + 1.0));
        }
        s = (s * beat + rng.next_f32() * 0.02).clamp(-0.98, 0.98) * 0.5;
        for c in 0..channels {
            let sc = if c == 0 { s } else { s * 0.8 + rng.next_f32() * 0.01 };
            out[i * channels + c] = sc;
        }
    }
    out
}

fn hash(v: &[f32]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for x in v {
        for b in x.to_bits().to_le_bytes() {
            h ^= b as u64;
            h = h.wrapping_mul(0x100_0000_01b3);
        }
    }
    h
}

#[test]
fn synth_probe() {
    let m = synth_music(48000, 2, 20.0);
    println!("SYNTH_PROBE input_hash=0x{:016x} n={}", hash(&m), m.len());
    println!("SYNTH_PROBE first8={:?}", &m[..8]);
    let s: f32 = (2.0f32 * std::f32::consts::PI * 440.0 * 0.001).sin();
    println!("SYNTH_PROBE sin_sample={:.9} bits=0x{:08x}", s, s.to_bits());
    // Frozen on Windows/MSVC. If this differs on another host, the oracle's
    // INPUT is platform-dependent and the encoder is not implicated at all.
    assert_eq!(
        hash(&m),
        0xe324_93ae_bb64_3959u64,
        "synthesised INPUT differs from the Windows baseline --          the fixture is platform-dependent, before any encoding happens"
    );
}
