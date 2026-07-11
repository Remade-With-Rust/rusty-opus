//! Parallel-encode a WAV and decode it back — the "parallel" leg of the R1
//! quality gate. Compare its PEAQ ODG to the serial roundtrip: if ΔODG ≤ 0.03 the
//! chunk-seam priming is perceptually neutral.
//!
//!   cargo run --release --example roundtrip_parallel -- in.wav out.wav <bitrate> <app> <warmup> [threads]

use std::io::{Read, Write};

use rusty_opus::parallel::{encode_parallel, encode_serial, ParallelConfig};
use rusty_opus::{Application, OpusDecoder};

fn read_wav(path: &str) -> (u32, u16, Vec<f32>) {
    let mut buf = Vec::new();
    std::fs::File::open(path).unwrap().read_to_end(&mut buf).unwrap();
    let rate = u32::from_le_bytes([buf[24], buf[25], buf[26], buf[27]]);
    let channels = u16::from_le_bytes([buf[22], buf[23]]);
    assert_eq!(u16::from_le_bytes([buf[34], buf[35]]), 16, "16-bit PCM only");
    let mut i = 12;
    let (mut off, mut len) = (0usize, 0usize);
    while i + 8 <= buf.len() {
        let sz = u32::from_le_bytes([buf[i + 4], buf[i + 5], buf[i + 6], buf[i + 7]]) as usize;
        if &buf[i..i + 4] == b"data" {
            off = i + 8;
            len = sz.min(buf.len() - (i + 8));
            break;
        }
        i += 8 + sz + (sz & 1);
    }
    let s: Vec<f32> = buf[off..off + len]
        .chunks_exact(2)
        .map(|b| i16::from_le_bytes([b[0], b[1]]) as f32 / 32768.0)
        .collect();
    (rate, channels, s)
}

fn write_wav(path: &str, rate: u32, channels: u16, s: &[f32]) {
    let mut data = Vec::with_capacity(s.len() * 2);
    for &v in s {
        data.extend_from_slice(&((v * 32768.0).clamp(-32768.0, 32767.0) as i16).to_le_bytes());
    }
    let mut f = std::fs::File::create(path).unwrap();
    f.write_all(b"RIFF").unwrap();
    f.write_all(&(36 + data.len() as u32).to_le_bytes()).unwrap();
    f.write_all(b"WAVEfmt ").unwrap();
    f.write_all(&16u32.to_le_bytes()).unwrap();
    f.write_all(&1u16.to_le_bytes()).unwrap();
    f.write_all(&channels.to_le_bytes()).unwrap();
    f.write_all(&rate.to_le_bytes()).unwrap();
    f.write_all(&(rate * channels as u32 * 2).to_le_bytes()).unwrap();
    f.write_all(&(channels * 2).to_le_bytes()).unwrap();
    f.write_all(&16u16.to_le_bytes()).unwrap();
    f.write_all(b"data").unwrap();
    f.write_all(&(data.len() as u32).to_le_bytes()).unwrap();
    f.write_all(&data).unwrap();
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let (inp, outp) = (&a[1], &a[2]);
    let bitrate: i32 = a.get(3).and_then(|s| s.parse().ok()).unwrap_or(64_000);
    let app = match a.get(4).map(|s| s.as_str()) {
        Some("voip") => Application::Voip,
        _ => Application::Audio,
    };
    let warmup_arg = a.get(5).cloned().unwrap_or_else(|| "8".into());
    let serial_mode = warmup_arg == "serial";
    let warmup: usize = warmup_arg.parse().unwrap_or(8);
    let threads: usize = a.get(6).and_then(|s| s.parse().ok()).unwrap_or(0);

    let (rate, channels, pcm) = read_wav(inp);
    let ch = channels as usize;
    let frame = rate as usize / 50;

    let mut cfg = ParallelConfig::new(rate as i32, ch, app);
    cfg.bitrate_bps = bitrate;
    cfg.warmup = warmup;
    cfg.threads = threads;

    let pkts = if serial_mode {
        encode_serial(&cfg, &pcm, frame)
    } else {
        encode_parallel(&cfg, &pcm, frame)
    };

    let mut dec = OpusDecoder::new(rate as i32, ch).unwrap();
    let mut dbuf = vec![0f32; frame * ch];
    let mut decoded: Vec<f32> = Vec::with_capacity(pcm.len());
    for p in &pkts {
        let n = dec.decode(p, frame, &mut dbuf).expect("decode");
        decoded.extend_from_slice(&dbuf[..n * ch]);
    }
    write_wav(outp, rate, channels, &decoded);
    let bytes: usize = pkts.iter().map(|p| p.len()).sum();
    let secs = pkts.len() as f64 * frame as f64 / rate as f64;
    eprintln!(
        "{} encode: {} pkts, {:.1} kbps, warmup={}",
        if serial_mode { "serial" } else { "parallel" },
        pkts.len(),
        bytes as f64 * 8.0 / secs / 1000.0,
        if serial_mode { 0 } else { warmup },
    );
}
