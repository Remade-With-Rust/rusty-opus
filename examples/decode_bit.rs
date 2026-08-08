// Conformance decode harness: mimics `opus_demo -d <rate> <channels> in.bit out.pcm`.
//
// Reads the opus_demo packet framing (per packet: 4-byte big-endian length,
// 4-byte big-endian encoder final-range, then `length` payload bytes), decodes
// each packet with our OpusDecoder, and writes interleaved little-endian i16 PCM
// — the format the official `opus_compare` expects.
//
// Usage: cargo run --release --example decode_bit -- <rate> <channels> <in.bit> <out.pcm>
use rusty_opus::OpusDecoder;
use std::env;
use std::fs::File;
use std::io::{Read, Write};

fn be32(b: &[u8]) -> u32 {
    ((b[0] as u32) << 24) | ((b[1] as u32) << 16) | ((b[2] as u32) << 8) | (b[3] as u32)
}

// Samples-per-frame at 48 kHz for each TOC config (0..31), in 2.5 ms = 120-sample units.
fn samples_per_frame_48k(config: u8) -> usize {
    match config {
        0..=11 => {
            // SILK: 10/20/40/60 ms
            let ms = [10usize, 20, 40, 60][(config % 4) as usize];
            ms * 48
        }
        12..=15 => {
            // Hybrid: 10/20 ms
            let ms = [10usize, 20][(config % 2) as usize];
            ms * 48
        }
        _ => {
            // CELT: 2.5/5/10/20 ms
            match config % 4 {
                0 => 120,
                1 => 240,
                2 => 480,
                _ => 960,
            }
        }
    }
}

// Total samples/channel implied by a packet's TOC.
fn packet_frame_size(payload: &[u8], rate: i32) -> usize {
    let toc = payload[0];
    let config = toc >> 3;
    let code = toc & 0x03;
    let frames = match code {
        0 => 1,
        1 | 2 => 2,
        _ => {
            if payload.len() >= 2 {
                (payload[1] & 0x3F) as usize
            } else {
                1
            }
        }
    };
    let per = samples_per_frame_48k(config) * rate as usize / 48000;
    per * frames
}

fn main() {
    let args: Vec<String> = env::args().collect();
    if args.len() != 5 {
        eprintln!("usage: decode_bit <rate> <channels> <in.bit> <out.pcm>");
        std::process::exit(2);
    }
    let rate: i32 = args[1].parse().unwrap();
    let channels: usize = args[2].parse().unwrap();

    let mut data = Vec::new();
    File::open(&args[3]).unwrap().read_to_end(&mut data).unwrap();
    let mut out = std::io::BufWriter::new(File::create(&args[4]).unwrap());

    let mut dec = OpusDecoder::new(rate, channels).unwrap();
    let max_frame = (rate as usize / 1000) * 120; // 120 ms/channel cap
    let mut pcm = vec![0f32; max_frame * channels];

    let (mut pos, mut pkt, mut errors, mut samples) = (0usize, 0u32, 0u32, 0usize);
    let mut ch_hist = [0usize; 3];
    let check_range = env::var("RANGECHK").is_ok();
    let mut range_mismatch = 0u32;
    let mut first_mismatch_pkt = 0u32;
    rusty_opus::prof::reset();
    while pos + 8 <= data.len() {
        let len = be32(&data[pos..pos + 4]) as usize;
        let enc_final_range = be32(&data[pos + 4..pos + 8]);
        pos += 8; // length + final-range
        if len == 0 || pos + len > data.len() {
            break;
        }
        let payload = &data[pos..pos + len];
        pos += len;
        pkt += 1;
        let pch = if payload[0] & 0x04 != 0 { 2 } else { 1 };
        ch_hist[pch] += 1;

        let fs = packet_frame_size(payload, rate).min(max_frame);
        let dec_res = {
            let _t = rusty_opus::prof::scope(rusty_opus::prof::Stage::Total);
            dec.decode(payload, fs, &mut pcm)
        };
        match dec_res {
            Ok(n) => {
                if check_range && dec.last_range != enc_final_range {
                    range_mismatch += 1;
                    if first_mismatch_pkt == 0 {
                        first_mismatch_pkt = pkt;
                    }
                    if range_mismatch <= 8 {
                        eprintln!(
                            "  RANGE MISMATCH pkt {pkt} (toc_ch={pch}): enc={enc_final_range} our={}",
                            dec.last_range
                        );
                    }
                }
                for &x in pcm.iter().take(n * channels) {
                    // Match libopus FLOAT2INT16 exactly: scale, clamp, then
                    // round half-to-even (lrintf), not half-away-from-zero.
                    let scaled = (x * 32768.0).clamp(-32768.0, 32767.0);
                    let s = scaled.round_ties_even() as i16;
                    out.write_all(&s.to_le_bytes()).unwrap();
                }
                samples += n;
            }
            Err(e) => {
                errors += 1;
                if errors <= 6 {
                    eprintln!("  pkt {pkt} (toc_ch={pch}, fs={fs}) decode error: {e}");
                }
            }
        }
    }
    eprintln!(
        "  packets={pkt} mono_toc={} stereo_toc={} samples/ch={samples} errors={errors}",
        ch_hist[1], ch_hist[2]
    );
    if check_range {
        eprintln!("  RANGE: {range_mismatch} mismatches (first at pkt {first_mismatch_pkt})");
    }
    rusty_opus::prof::dump();
}

// Primary allocator for this target: our rusty_alloc, the pure-Rust mimalloc
// remake. Codec hot paths allocate heavily and the system heap dominates the
// profile there (measured 1.38x end-to-end on AV2 decode). Per project
// convention this belongs in binary/bench/example roots, never in a library --
// a library that declares one hijacks every dependent's allocator choice.
#[global_allocator]
static RUSTY_ALLOC: rusty_alloc_api::RustyAlloc = rusty_alloc_api::RustyAlloc;
