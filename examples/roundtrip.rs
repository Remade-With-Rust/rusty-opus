//! Encode a WAV through rusty-opus and decode it back to a WAV — the quality
//! harness's "ours" leg (the C leg is `ffmpeg -c:a libopus`). Both decoded WAVs
//! are then PEAQ-scored against the original by `tools/quality/peaq_run.py`.
//!
//!   cargo run --release --example roundtrip -- in.wav out.wav <bitrate> <app>
//!
//! app = "audio" | "voip". Handles mono/stereo 16-bit PCM WAV at 48/24/16 kHz.

use std::io::{Read, Write};

use opus_rs::{Application, OpusDecoder, OpusEncoder};

fn read_wav(path: &str) -> (u32, u16, Vec<f32>) {
    let mut buf = Vec::new();
    std::fs::File::open(path).unwrap().read_to_end(&mut buf).unwrap();
    // Minimal RIFF/WAVE parse: find "fmt " and "data".
    let rate = u32::from_le_bytes([buf[24], buf[25], buf[26], buf[27]]);
    let channels = u16::from_le_bytes([buf[22], buf[23]]);
    let bits = u16::from_le_bytes([buf[34], buf[35]]);
    assert_eq!(bits, 16, "only 16-bit PCM WAV supported");
    // Locate the "data" chunk (may not be at a fixed offset).
    let mut i = 12;
    let (mut data_off, mut data_len) = (0usize, 0usize);
    while i + 8 <= buf.len() {
        let id = &buf[i..i + 4];
        let sz = u32::from_le_bytes([buf[i + 4], buf[i + 5], buf[i + 6], buf[i + 7]]) as usize;
        if id == b"data" {
            data_off = i + 8;
            data_len = sz.min(buf.len() - (i + 8));
            break;
        }
        i += 8 + sz + (sz & 1);
    }
    let samples: Vec<f32> = buf[data_off..data_off + data_len]
        .chunks_exact(2)
        .map(|b| i16::from_le_bytes([b[0], b[1]]) as f32 / 32768.0)
        .collect();
    (rate, channels, samples)
}

fn write_wav(path: &str, rate: u32, channels: u16, samples: &[f32]) {
    let mut data = Vec::with_capacity(samples.len() * 2);
    for &s in samples {
        let v = (s * 32768.0).clamp(-32768.0, 32767.0) as i16;
        data.extend_from_slice(&v.to_le_bytes());
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
    let args: Vec<String> = std::env::args().collect();
    let inp = &args[1];
    let outp = &args[2];
    let bitrate: i32 = args.get(3).map(|s| s.parse().unwrap()).unwrap_or(64_000);
    let app = match args.get(4).map(|s| s.as_str()) {
        Some("voip") => Application::Voip,
        _ => Application::Audio,
    };

    let (rate, channels, pcm) = read_wav(inp);
    let ch = channels as usize;
    let frame = rate as usize / 50; // 20 ms
    let step = frame * ch;

    let mut enc = OpusEncoder::new(rate as i32, ch, app).unwrap();
    enc.bitrate_bps = bitrate;
    let mut dec = OpusDecoder::new(rate as i32, ch).unwrap();

    let mut ebuf = vec![0u8; 4000];
    let mut dbuf = vec![0f32; frame * ch];
    let mut decoded: Vec<f32> = Vec::with_capacity(pcm.len());
    let mut enc_bytes = 0usize;
    let mut nframes = 0usize;
    for chunk in pcm.chunks_exact(step) {
        let n = enc.encode(chunk, frame, &mut ebuf).expect("encode");
        enc_bytes += n;
        nframes += 1;
        let got = dec.decode(&ebuf[..n], frame, &mut dbuf).expect("decode");
        decoded.extend_from_slice(&dbuf[..got * ch]);
    }
    let secs = nframes as f64 * frame as f64 / rate as f64;
    eprintln!(
        "  encoded {} bytes over {:.2}s = {:.1} kbps (target {} kbps)",
        enc_bytes,
        secs,
        enc_bytes as f64 * 8.0 / secs / 1000.0,
        bitrate / 1000,
    );
    write_wav(outp, rate, channels, &decoded);
    eprintln!(
        "roundtrip: {} -> {} ({} Hz {}ch, {} kbps, {})",
        inp,
        outp,
        rate,
        ch,
        bitrate / 1000,
        if matches!(app, Application::Voip) { "voip" } else { "audio" },
    );
}
