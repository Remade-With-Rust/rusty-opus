//! Encode-only speed probe on a real WAV: best-of-N in-process timing so process
//! startup and file I/O stay out of the number (the ffmpeg arms strip theirs via
//! 60s−30s slope correction instead).
//!
//!   cargo run --release --example encode_speed -- in.wav <bitrate> <passes>

use std::io::Read;

use rusty_opus::{Application, OpusEncoder};

fn read_wav(path: &str) -> (u32, u16, Vec<f32>) {
    let mut buf = Vec::new();
    std::fs::File::open(path).unwrap().read_to_end(&mut buf).unwrap();
    let rate = u32::from_le_bytes([buf[24], buf[25], buf[26], buf[27]]);
    let channels = u16::from_le_bytes([buf[22], buf[23]]);
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

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let (rate, channels, pcm) = read_wav(&args[1]);
    let bitrate: i32 = args.get(2).map(|s| s.parse().unwrap()).unwrap_or(64_000);
    let passes: usize = args.get(3).map(|s| s.parse().unwrap()).unwrap_or(7);
    let ch = channels as usize;
    let frame = rate as usize / 50;
    let step = frame * ch;
    let secs = (pcm.len() / step) as f64 * frame as f64 / rate as f64;

    let run = || {
        let mut enc = OpusEncoder::new(rate as i32, ch, Application::Audio).unwrap();
        enc.bitrate_bps = bitrate;
        let mut out = vec![0u8; 4000];
        let (mut bytes, mut packets) = (0usize, 0usize);
        for chunk in pcm.chunks_exact(step) {
            let n = enc.encode(chunk, frame, &mut out).expect("encode");
            bytes += n;
            packets += 1;
        }
        (bytes, packets)
    };
    let (bytes, packets) = run(); // warm-up + determinism anchor
    let mut times = Vec::new();
    for _ in 0..passes {
        let t0 = std::time::Instant::now();
        let (b, _) = run();
        assert_eq!(b, bytes);
        times.push(t0.elapsed().as_secs_f64());
    }
    times.sort_by(f64::total_cmp);
    println!(
        "encode_speed {} ch={} br={}  best {:.1}x RT  median {:.1}x RT  ({} pkts, {} bytes, {:.1}s)",
        args[1], ch, bitrate,
        secs / times[0],
        secs / times[times.len() / 2],
        packets, bytes, secs
    );
}
