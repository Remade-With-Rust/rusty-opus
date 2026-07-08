//! R1 validation: parallel vs serial encode — correctness (decodes, packet
//! count) and wall-clock speedup. PEAQ quality is checked separately via
//! `examples/roundtrip_parallel.rs` + `tools/quality_ab.sh`.
//!
//!   cargo test --release --test parallel_encode -- --ignored --nocapture

use opus_rs::parallel::{encode_parallel, encode_serial, encode_streams, ParallelConfig};
use opus_rs::{Application, OpusDecoder};

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

fn synth_speech(rate: u32, secs: f32) -> Vec<f32> {
    let n = (rate as f32 * secs) as usize;
    let mut rng = Lcg(0xBEEF_5A5A_1234_5678);
    let mut out = vec![0.0f32; n];
    let (mut y1a, mut y2a, mut y1b, mut y2b) = (0.0f32, 0.0f32, 0.0f32, 0.0f32);
    let mut phase = 0.0f32;
    for i in 0..n {
        let t = i as f32 / rate as f32;
        let voiced = (t % 0.6) < 0.4;
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

fn total_bytes(pkts: &[Vec<u8>]) -> usize {
    pkts.iter().map(|p| p.len()).sum()
}

/// Decode a packet stream; returns total decoded samples (per channel).
fn decode_all(rate: i32, channels: usize, pkts: &[Vec<u8>], frame: usize) -> usize {
    let mut dec = OpusDecoder::new(rate, channels).unwrap();
    let mut out = vec![0f32; frame * channels];
    let mut n = 0;
    for p in pkts {
        n += dec.decode(p, frame, &mut out).expect("decode");
    }
    n
}

/// R1a: per-stream parallelism must be BYTE-IDENTICAL to serial (independent
/// streams, no chunk seams).
#[test]
fn per_stream_byte_identical() {
    let (rate, channels) = (16000u32, 1usize);
    let frame = rate as usize / 50;
    let mut cfg = ParallelConfig::new(rate as i32, channels, Application::Voip);
    cfg.bitrate_bps = 24_000;
    // Several distinct streams (vary the seed → distinct content).
    let clips: Vec<Vec<f32>> = (0..7)
        .map(|k| {
            let mut v = synth_speech(rate, 3.0);
            for x in v.iter_mut() {
                *x = (*x + 0.01 * k as f32).clamp(-0.98, 0.98);
            }
            v
        })
        .collect();
    let streams: Vec<_> = clips.iter().map(|c| (cfg, c.as_slice(), frame)).collect();
    let par = encode_streams(&streams, 0);
    for (i, clip) in clips.iter().enumerate() {
        let serial = encode_serial(&cfg, clip, frame);
        assert_eq!(par[i], serial, "stream {i}: per-stream parallel != serial");
    }
}

#[test]
#[ignore]
fn parallel_correct_and_fast() {
    let secs = 30.0f32;
    let (rate, channels) = (16000u32, 1usize);
    let frame = rate as usize / 50; // 20 ms
    let pcm = synth_speech(rate, secs);
    let total_frames = pcm.len() / (frame * channels);

    let mut cfg = ParallelConfig::new(rate as i32, channels, Application::Voip);
    cfg.bitrate_bps = 24_000;

    // Correctness: same packet count, both fully decodable.
    let serial = encode_serial(&cfg, &pcm, frame);
    let par = encode_parallel(&cfg, &pcm, frame);
    assert_eq!(serial.len(), total_frames, "serial packet count");
    assert_eq!(par.len(), total_frames, "parallel packet count");
    let sdec = decode_all(rate as i32, channels, &serial, frame);
    let pdec = decode_all(rate as i32, channels, &par, frame);
    assert_eq!(sdec, pdec, "decoded sample counts differ");
    assert!(pdec >= total_frames * frame - frame, "too few decoded samples");

    // Determinism: two parallel runs are identical.
    let par2 = encode_parallel(&cfg, &pcm, frame);
    assert_eq!(par, par2, "parallel encode is non-deterministic");

    // Speed: best-of-N wall-clock, serial vs parallel.
    let bench = |f: &dyn Fn() -> Vec<Vec<u8>>| -> f64 {
        let mut best = f64::INFINITY;
        for _ in 0..5 {
            let t0 = std::time::Instant::now();
            let r = f();
            let dt = t0.elapsed().as_secs_f64();
            std::hint::black_box(&r);
            if dt < best {
                best = dt;
            }
        }
        best
    };
    let st = bench(&|| encode_serial(&cfg, &pcm, frame));
    let pt = bench(&|| encode_parallel(&cfg, &pcm, frame));
    let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1);
    println!("\n--- R1 parallel encode ({secs}s speech @24k, {threads} cores) ---");
    println!("  serial   : {:>7.1} ms  ({:>6.0}x RT)", st * 1e3, secs as f64 / st);
    println!("  parallel : {:>7.1} ms  ({:>6.0}x RT)  = {:.1}x speedup", pt * 1e3, secs as f64 / pt, st / pt);
    println!(
        "  bytes: serial {} vs parallel {} ({:+.2}% VBR drift from chunk seams)",
        total_bytes(&serial),
        total_bytes(&par),
        100.0 * (total_bytes(&par) as f64 - total_bytes(&serial) as f64) / total_bytes(&serial) as f64,
    );
}
