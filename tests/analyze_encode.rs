//! Isolated core-encoder profiler (no CLI wrapper, no WAV/mux). Run:
//!   cargo test --release --features profile analyze_encode -- --ignored --nocapture
use rusty_opus::{Application, OpusEncoder};
use std::time::Instant;

fn synth_stereo_music(rate: usize, secs: usize) -> Vec<f32> {
    let n = rate * secs;
    let mut v = vec![0.0f32; n * 2];
    let mut seed = 0x1234_5678u64;
    let mut rng = || {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
        ((seed >> 40) as f32 / (1u32 << 23) as f32) - 1.0
    };
    let freqs = [220.0f32, 277.18, 329.63, 440.0, 554.37, 659.25, 880.0];
    for i in 0..n {
        let t = i as f32 / rate as f32;
        let beat = if (t * 2.0).fract() < 0.06 { 1.7 } else { 1.0 };
        let mut s = 0.0f32;
        for (k, f) in freqs.iter().enumerate() {
            s += (std::f32::consts::TAU * f * t).sin() * (0.20 / (k as f32 + 1.0));
        }
        let l = (s * beat + rng() * 0.03).clamp(-0.95, 0.95) * 0.6;
        let r = (s * beat * 0.9 + rng() * 0.05).clamp(-0.95, 0.95) * 0.6; // decorrelated
        v[i * 2] = l;
        v[i * 2 + 1] = r;
    }
    v
}

#[test]
#[ignore]
fn analyze_encode() {
    let (rate, ch, br) = (48_000usize, 2usize, 128_000i32);
    // Load real content from ANALYZE_PCM (raw f32le interleaved stereo 48k) if set,
    // else fall back to synthetic music.
    let pcm: Vec<f32> = match std::env::var("ANALYZE_PCM") {
        Ok(p) => {
            let bytes = std::fs::read(&p).expect("read ANALYZE_PCM");
            bytes
                .chunks_exact(4)
                .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
                .collect()
        }
        Err(_) => synth_stereo_music(rate, 20),
    };
    let secs = pcm.len() / ch / rate;
    let frame = rate / 1000 * 20; // 960 samples/ch, 20 ms
    let nframes = (rate * secs) / frame;

    let mut enc = OpusEncoder::new(rate as i32, ch, Application::Audio).unwrap();
    enc.bitrate_bps = br;
    let mut out = vec![0u8; 4000];

    // warm up
    for f in 0..8 {
        let c = &pcm[f * frame * ch..(f + 1) * frame * ch];
        let _ = enc.encode(c, frame, &mut out);
    }

    let mut best = f64::INFINITY;
    for _pass in 0..5 {
        let mut e2 = OpusEncoder::new(rate as i32, ch, Application::Audio).unwrap();
        e2.bitrate_bps = br;
        rusty_opus::prof::reset();
        let t = Instant::now();
        for f in 0..nframes {
            let c = &pcm[f * frame * ch..(f + 1) * frame * ch];
            let _t = rusty_opus::prof::scope(rusty_opus::prof::Stage::Total);
            e2.encode(c, frame, &mut out).unwrap();
        }
        let ms = t.elapsed().as_secs_f64() * 1000.0;
        if ms < best {
            best = ms;
        }
    }
    eprintln!(
        "\n=== CORE ENCODER: {nframes} frames (20s stereo 48k @128k), best {best:.1} ms = {:.1} ms/frame, {:.0}x realtime ===",
        best / nframes as f64,
        (secs as f64 * 1000.0) / best
    );
    rusty_opus::prof::dump();
}
