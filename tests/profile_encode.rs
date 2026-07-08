//! Encoder analysis instruments (codec-analyzer):
//!
//! * `encode_throughput` — the deterministic best-of-N benchmark (×realtime),
//!   run with the `profile` feature **OFF** for honest numbers:
//!   `cargo test --release --test profile_encode encode_throughput -- --ignored --nocapture`
//!
//! * `profile_breakdown` — the per-stage median-of-N breakdown, run with the
//!   `profile` feature **ON** (percentages only — the timer inflates totals):
//!   `cargo test --release --features profile --test profile_encode profile_breakdown -- --ignored --nocapture`
//!
//! Scenarios cover the three Opus modes: CELT-only (48 kHz stereo music,
//! 128 kbps Audio), SILK-only (16 kHz mono speech, 24 kbps VoIP), and Hybrid
//! (48 kHz mono speech, 32 kbps VoIP → SILK WB + CELT high bands).

use opus_rs::{Application, OpusEncoder};

/// Deterministic LCG — no external RNG, byte-identical clips forever.
struct Lcg(u64);
impl Lcg {
    fn next_f32(&mut self) -> f32 {
        // Numerical Recipes LCG; top 24 bits → [-1, 1).
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        ((self.0 >> 40) as f32 / (1u32 << 23) as f32) - 1.0
    }
}

/// Music-like: chord of detuned partials + vibrato + noise floor + beat accents.
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
            // Slight stereo decorrelation: right channel gets a phase-shifted mix.
            let sc = if c == 0 { s } else { s * 0.8 + rng.next_f32() * 0.01 };
            out[i * channels + c] = sc;
        }
    }
    out
}

/// Speech-like: glottal pulse train (~120 Hz, drifting) through 2 "formant"
/// resonators + unvoiced noise bursts — enough structure for VAD/pitch/LTP.
fn synth_speech(rate: u32, secs: f32) -> Vec<f32> {
    let n = (rate as f32 * secs) as usize;
    let mut rng = Lcg(0xBEEF_5A5A_1234_5678);
    let mut out = vec![0.0f32; n];
    let (mut y1a, mut y2a, mut y1b, mut y2b) = (0.0f32, 0.0f32, 0.0f32, 0.0f32);
    let mut phase = 0.0f32;
    for i in 0..n {
        let t = i as f32 / rate as f32;
        // Alternate voiced (0.4 s) / unvoiced (0.2 s) "syllables".
        let seg = t % 0.6;
        let voiced = seg < 0.4;
        let f0 = 120.0 + 25.0 * (2.0 * std::f32::consts::PI * 0.7 * t).sin();
        phase += f0 / rate as f32;
        if phase >= 1.0 {
            phase -= 1.0;
        }
        let exc = if voiced {
            // Rosenberg-ish pulse: sharp at phase 0, decaying.
            (1.0 - phase).powi(3) * if phase < 0.1 { 1.5 } else { 0.3 }
        } else {
            rng.next_f32() * 0.4
        };
        // Two fixed resonators (≈700 Hz, ≈1800 Hz at 16 kHz) as crude formants.
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

struct Scenario {
    name: &'static str,
    rate: u32,
    channels: usize,
    app: Application,
    bitrate: i32,
    pcm: Vec<f32>,
}

fn scenarios(secs: f32) -> Vec<Scenario> {
    vec![
        Scenario {
            name: "CELT  48k stereo music @128k (Audio)",
            rate: 48000,
            channels: 2,
            app: Application::Audio,
            bitrate: 128_000,
            pcm: synth_music(48000, 2, secs),
        },
        Scenario {
            name: "SILK  16k mono speech  @24k  (Voip) ",
            rate: 16000,
            channels: 1,
            app: Application::Voip,
            bitrate: 24_000,
            pcm: synth_speech(16000, secs),
        },
        Scenario {
            name: "HYBRID 48k mono speech @32k  (Voip) ",
            rate: 48000,
            channels: 1,
            app: Application::Voip,
            bitrate: 32_000,
            pcm: synth_speech(48000, secs),
        },
    ]
}

/// Encode the whole clip in 20 ms frames; returns (encoded bytes, packets).
fn encode_clip(sc: &Scenario) -> (usize, usize) {
    let mut enc = OpusEncoder::new(sc.rate as i32, sc.channels, sc.app).unwrap();
    enc.bitrate_bps = sc.bitrate;
    let frame = sc.rate as usize / 50; // 20 ms
    let step = frame * sc.channels;
    let mut out = vec![0u8; 4000];
    let (mut bytes, mut packets) = (0usize, 0usize);
    for chunk in sc.pcm.chunks_exact(step) {
        let n = enc.encode(chunk, frame, &mut out).expect("encode");
        bytes += n;
        packets += 1;
    }
    (bytes, packets)
}

/// Best-of-N ×realtime throughput (run with `profile` OFF).
#[test]
#[ignore]
fn encode_throughput() {
    let secs = 30.0f32;
    let passes = 7;
    for sc in scenarios(secs) {
        let (bytes, packets) = encode_clip(&sc); // warm-up + sanity
        assert!(packets > 0 && bytes > packets); // non-empty packets
        let mut best = f64::INFINITY;
        let mut all = Vec::new();
        for _ in 0..passes {
            let t0 = std::time::Instant::now();
            let (b, _) = encode_clip(&sc);
            let dt = t0.elapsed().as_secs_f64();
            assert_eq!(b, bytes); // deterministic
            all.push(dt);
            if dt < best {
                best = dt;
            }
        }
        all.sort_by(f64::total_cmp);
        let median = all[all.len() / 2];
        let kbps = bytes as f64 * 8.0 / secs as f64 / 1000.0;
        println!(
            "{}  best {:>7.1}x RT  median {:>7.1}x RT  ({} pkts, {:.1} kbps)",
            sc.name,
            secs as f64 / best,
            secs as f64 / median,
            packets,
            kbps,
        );
    }
}

/// Per-stage median-of-N breakdown (run with `profile` ON; read percentages).
#[test]
#[ignore]
fn profile_breakdown() {
    let secs = 10.0f32;
    let passes = 15;
    for sc in scenarios(secs) {
        // Collect a snapshot per pass; report the per-stage median.
        let mut per_stage: Vec<Vec<(f64, u64)>> = vec![Vec::new(); opus_rs::prof::N];
        for _ in 0..passes {
            opus_rs::prof::reset();
            encode_clip(&sc);
            let snap = opus_rs::prof::snapshot();
            for (i, v) in snap.iter().enumerate() {
                per_stage[i].push(*v);
            }
        }
        let med = |v: &mut Vec<(f64, u64)>| {
            v.sort_by(|a, b| a.0.total_cmp(&b.0));
            v[v.len() / 2]
        };
        let stages: Vec<(f64, u64)> = per_stage.iter_mut().map(med).collect();
        let total = stages[opus_rs::prof::Stage::Total as usize].0.max(1e-9);
        let sub: f64 = stages[..opus_rs::prof::INFO_FIRST]
            .iter()
            .map(|s| s.0)
            .sum();
        println!("\n=== {} — {:.1} ms total (median of {passes}) ===", sc.name, total);
        let mut rows: Vec<(usize, f64, u64)> = stages[..opus_rs::prof::Stage::Total as usize]
            .iter()
            .enumerate()
            .filter(|(_, s)| s.1 > 0)
            .map(|(i, s)| (i, s.0, s.1))
            .collect();
        rows.sort_by(|a, b| b.1.total_cmp(&a.1));
        for (i, ms, calls) in rows {
            println!(
                "  {:<18} {:>8.2} ms  {:>5.1}%   ({} calls, {:.0} ns/call)",
                opus_rs::prof::name(i),
                ms,
                100.0 * ms / total,
                calls,
                ms * 1e6 / calls as f64,
            );
        }
        println!(
            "  {:<18} {:>8.2} ms  {:>5.1}%   <- residue (mode select / control / glue / timer overhead)",
            "mgmt/other",
            total - sub,
            100.0 * (total - sub) / total,
        );
    }
}

