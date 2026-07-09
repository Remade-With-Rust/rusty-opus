//! **Byte-identity oracle** — the gate every byte-identical speed brick must pass.
//!
//! The encoder is fully deterministic, so a speed optimization (redundancy
//! elimination, SIMD with a scalar twin, loop hoisting) MUST reproduce the exact
//! packet bytes it produced before. This test encodes the three canonical
//! scenarios in 20 ms frames and hashes the full concatenated packet stream; the
//! expected hashes below are frozen from the pre-optimization baseline.
//!
//! If a brick legitimately MOVES the bitstream (algorithmic change, float
//! reassociation), it is NOT byte-identical — gate it with PEAQ instead
//! (`tests/README_oracle.md`) and, only then, re-freeze these hashes with the
//! `--nocapture` output.
//!
//!   cargo test --release --test oracle_bitexact -- --nocapture
//!
//! The scenario synths are duplicated from `profile_encode.rs` verbatim so the
//! oracle stands alone (a bench edit can't silently move the gate).

use opus_rs::{Application, OpusEncoder};

struct Lcg(u64);
impl Lcg {
    fn next_f32(&mut self) -> f32 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
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

fn synth_speech(rate: u32, secs: f32) -> Vec<f32> {
    let n = (rate as f32 * secs) as usize;
    let mut rng = Lcg(0xBEEF_5A5A_1234_5678);
    let mut out = vec![0.0f32; n];
    let (mut y1a, mut y2a, mut y1b, mut y2b) = (0.0f32, 0.0f32, 0.0f32, 0.0f32);
    let mut phase = 0.0f32;
    for i in 0..n {
        let t = i as f32 / rate as f32;
        let seg = t % 0.6;
        let voiced = seg < 0.4;
        let f0 = 120.0 + 25.0 * (2.0 * std::f32::consts::PI * 0.7 * t).sin();
        phase += f0 / rate as f32;
        if phase >= 1.0 {
            phase -= 1.0;
        }
        let exc = if voiced {
            (1.0 - phase).powi(3) * if phase < 0.1 { 1.5 } else { 0.3 }
        } else {
            rng.next_f32() * 0.4
        };
        let (r, c1) = (0.95f32, (2.0 * std::f32::consts::PI * 700.0 / rate as f32).cos());
        let ya = exc + 2.0 * r * c1 * y1a - r * r * y2a;
        y2a = y1a;
        y1a = ya;
        let c2 = (2.0 * std::f32::consts::PI * 1800.0 / rate as f32).cos();
        let yb = exc + 2.0 * r * c2 * y1b - r * r * y2b;
        y2b = y1b;
        y1b = yb;
        out[i] = ((ya * 0.6 + yb * 0.4) * 0.05).clamp(-0.98, 0.98);
    }
    out
}

/// FNV-1a over the full packet stream + a running byte/packet tally.
fn encode_hash(rate: u32, channels: usize, app: Application, bitrate: i32, pcm: &[f32]) -> (u64, usize, usize) {
    let mut enc = OpusEncoder::new(rate as i32, channels, app).unwrap();
    enc.bitrate_bps = bitrate;
    let frame = rate as usize / 50;
    let step = frame * channels;
    let mut out = vec![0u8; 4000];
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let (mut bytes, mut packets) = (0usize, 0usize);
    for chunk in pcm.chunks_exact(step) {
        let n = enc.encode(chunk, frame, &mut out).expect("encode");
        for &b in &out[..n] {
            h ^= b as u64;
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
        bytes += n;
        packets += 1;
    }
    (h, bytes, packets)
}

#[test]
fn oracle_bitexact() {
    let secs = 20.0f32;
    // (name, expected_hash, expected_bytes, expected_packets)
    // Re-frozen 2026-07-09 on the conformance-fixed tree (haar1, alloc row 10, anti-collapse rsv, alloc_trim fallback, prefilter off). Layout-stability verified by struct-padding perturbation + scratch-buffer canaries.
    let cases: [(&str, u64, usize, usize); 3] = [
        ("CELT  48k stereo music @128k", 0xff9e_58ff_819e_bba7, 320000, 1000),
        ("SILK  16k mono speech  @24k ", 0xd9ce_e0b6_4e49_7653, 38500, 1000),
        ("HYBRID 48k mono speech @32k ", 0x2e84_540c_0bd1_6327, 80000, 1000),
    ];
    let got = [
        encode_hash(48000, 2, Application::Audio, 128_000, &synth_music(48000, 2, secs)),
        encode_hash(16000, 1, Application::Voip, 24_000, &synth_speech(16000, secs)),
        encode_hash(48000, 1, Application::Voip, 32_000, &synth_speech(48000, secs)),
    ];
    println!("\n--- byte-identity oracle (freeze these) ---");
    for (i, (name, _, _, _)) in cases.iter().enumerate() {
        let (h, bytes, packets) = got[i];
        println!("  {name}  hash=0x{h:016x}  bytes={bytes}  packets={packets}");
    }
    // Correctness invariants that hold regardless of the frozen hash:
    for (i, (name, _, _, exp_pk)) in cases.iter().enumerate() {
        let (_, bytes, packets) = got[i];
        assert_eq!(packets, *exp_pk, "{name}: packet count changed");
        assert!(bytes > packets, "{name}: implausibly small output");
    }

    // Once frozen (set FROZEN=true and paste hashes above), this asserts identity.
    const FROZEN: bool = true;
    if FROZEN {
        for (i, (name, exp_h, exp_b, _)) in cases.iter().enumerate() {
            let (h, bytes, _) = got[i];
            assert_eq!(h, *exp_h, "{name}: BITSTREAM MOVED (hash) — not byte-identical");
            assert_eq!(bytes, *exp_b, "{name}: BITSTREAM MOVED (bytes)");
        }
    }
}
