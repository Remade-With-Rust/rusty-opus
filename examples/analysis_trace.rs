//! Analysis-module oracle harness: run our `analysis::run_analysis` over a raw
//! s16 PCM file exactly the way `opus_encode_native` drives the C module
//! (per-frame, analysis_pcm = the current frame), printing one line per frame
//! in the same format as the DANA-instrumented opus_demo build. Diffing the
//! two traces validates the port.
//!
//! Usage: analysis_trace <rate> <channels> <frame_ms> <in.sw>

use rusty_opus::analysis::{TonalityAnalysisState, run_analysis};
use rusty_opus::kiss_fft::KissFftState;
use std::fs::File;
use std::io::Read;

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let rate: i32 = a[1].parse().unwrap();
    let channels: usize = a[2].parse().unwrap();
    let frame_ms: f64 = a[3].parse().unwrap();
    let frame_size = (rate as f64 * frame_ms / 1000.0) as usize;

    let mut data = Vec::new();
    File::open(&a[4]).unwrap().read_to_end(&mut data).unwrap();
    let total = data.len() / 2;

    let kfft = KissFftState::new(480).expect("fft 480");
    let mut st = TonalityAnalysisState::new(rate);

    let samples_per_frame = frame_size * channels;
    let mut pcm = vec![0f32; samples_per_frame];
    let mut pos = 0usize;
    let mut n = 0usize;
    while pos + samples_per_frame <= total {
        for (i, v) in pcm.iter_mut().enumerate() {
            let off = (pos + i) * 2;
            *v = i16::from_le_bytes([data[off], data[off + 1]]) as f32 / 32768.0;
        }
        let info = run_analysis(&mut st, &kfft, &pcm, frame_size, frame_size, channels, rate, 16);
        println!(
            "DANA {} v={} ton={:.6} slope={:.6} noise={:.6} act={:.6} mp={:.6} mpmin={:.6} mpmax={:.6} bw={} actp={:.6} mpr={:.6} lb0={} lb5={} lb10={}",
            n,
            info.valid as i32,
            info.tonality,
            info.tonality_slope,
            info.noisiness,
            info.activity,
            info.music_prob,
            info.music_prob_min,
            info.music_prob_max,
            info.bandwidth,
            info.activity_probability,
            info.max_pitch_ratio,
            info.leak_boost[0],
            info.leak_boost[5],
            info.leak_boost[10]
        );
        pos += samples_per_frame;
        n += 1;
    }
}
