//! Encoder conformance harness: mimics `opus_demo -e` output framing.
//!
//! Reads raw interleaved 16-bit little-endian PCM (the `.sw` format opus_demo and
//! the RFC test-vector `.dec` files use), encodes with our OpusEncoder, and writes
//! opus_demo packet framing (per packet: 4-byte big-endian length, 4-byte
//! big-endian encoder FINAL RANGE, then the payload).
//!
//! Decoding the result with the reference `opus_demo -d` then verifies our
//! encoder's range-coder state against libopus's decoder on EVERY packet ("Error:
//! Range coder state mismatch" on any divergence) — a per-packet encoder
//! conformance gate. Quality is measured separately by comparing the decoded
//! audio against the input (e.g. with opus_compare).
//!
//! Usage: encode_bit <rate> <channels> <bitrate_bps> <frame_ms> <application> <in.sw> <out.bit>
//!   application: voip | audio | lowdelay
//!   frame_ms: 2.5 | 5 | 10 | 20 | 40 | 60

use opus_rs::{Application, OpusEncoder};
use std::fs::File;
use std::io::{Read, Write};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 8 {
        eprintln!(
            "usage: encode_bit <rate> <channels> <bitrate_bps> <frame_ms> <voip|audio|lowdelay> <in.sw> <out.bit>"
        );
        std::process::exit(2);
    }
    let rate: i32 = args[1].parse().unwrap();
    let channels: usize = args[2].parse().unwrap();
    let bitrate: i32 = args[3].parse().unwrap();
    let frame_ms: f64 = args[4].parse().unwrap();
    let app = match args[5].as_str() {
        "voip" => Application::Voip,
        "lowdelay" => Application::RestrictedLowDelay,
        _ => Application::Audio,
    };
    let frame_size = (rate as f64 * frame_ms / 1000.0) as usize;

    let mut data = Vec::new();
    File::open(&args[6]).unwrap().read_to_end(&mut data).unwrap();
    let mut out = std::io::BufWriter::new(File::create(&args[7]).unwrap());

    let mut enc = OpusEncoder::new(rate, channels, app).unwrap();
    enc.bitrate_bps = bitrate;

    let samples_per_frame = frame_size * channels;
    let total_samples = data.len() / 2;
    let mut pcm = vec![0f32; samples_per_frame];
    let mut packet = vec![0u8; 4000];
    let (mut pos, mut packets, mut bytes) = (0usize, 0u64, 0u64);

    while pos + samples_per_frame <= total_samples {
        for (i, v) in pcm.iter_mut().enumerate() {
            let off = (pos + i) * 2;
            *v = i16::from_le_bytes([data[off], data[off + 1]]) as f32 / 32768.0;
        }
        let n = match enc.encode(&pcm, frame_size, &mut packet) {
            Ok(n) => n,
            Err(e) => {
                eprintln!("encode error at packet {packets}: {e}");
                std::process::exit(1);
            }
        };
        out.write_all(&(n as u32).to_be_bytes()).unwrap();
        out.write_all(&enc.final_range().to_be_bytes()).unwrap();
        out.write_all(&packet[..n]).unwrap();
        pos += samples_per_frame;
        packets += 1;
        bytes += n as u64;
    }
    let dur = (pos / channels) as f64 / rate as f64;
    eprintln!(
        "  packets={packets} bytes={bytes} ({:.1} kbps over {:.1}s)",
        bytes as f64 * 8.0 / dur / 1000.0,
        dur
    );
}
