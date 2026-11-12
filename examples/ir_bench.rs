//! Deterministic instruction-count harness (callgrind) - see `tools/ir/measure.sh`.
//!
//!   ir_bench enc <wav> <bitrate> <audio|voip> <complexity> [packets.bin]
//!   ir_bench dec <packets.bin> <rate> <channels>
//!
//! Every run prints an `ANCHOR` line: counts that cannot move if the work is
//! unchanged (packets, bytes, a hash of every output byte, and the coding mode
//! of each packet from its TOC). An optimization brick must leave it untouched.

use std::io::{Read, Write};

use rusty_opus::{Application, OpusDecoder, OpusEncoder};

fn read_wav(path: &str) -> (u32, u16, Vec<f32>) {
    let mut buf = Vec::new();
    std::fs::File::open(path)
        .unwrap()
        .read_to_end(&mut buf)
        .unwrap();
    let rate = u32::from_le_bytes([buf[24], buf[25], buf[26], buf[27]]);
    let channels = u16::from_le_bytes([buf[22], buf[23]]);
    let mut i = 12;
    let (mut off, mut len) = (0usize, 0usize);
    while i + 8 <= buf.len() {
        let sz = u32::from_le_bytes([buf[i + 4], buf[i + 5], buf[i + 6], buf[i + 7]]) as usize;
        if &buf[i..i + 4] == b"data" {
            off = i + 8;
            len = sz.min(buf.len() - off);
            break;
        }
        i += 8 + sz + (sz & 1);
    }
    let samples = buf[off..off + len]
        .chunks_exact(2)
        .map(|b| f32::from(i16::from_le_bytes([b[0], b[1]])) / 32768.0)
        .collect();
    (rate, channels, samples)
}

const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0100_0000_01b3;

fn main() {
    let a: Vec<String> = std::env::args().collect();
    match a[1].as_str() {
        "enc" => {
            let (rate, channels, pcm) = read_wav(&a[2]);
            let ch = channels as usize;
            let app = if a[4] == "voip" {
                Application::Voip
            } else {
                Application::Audio
            };
            let mut enc = OpusEncoder::new(rate as i32, ch, app).unwrap();
            enc.bitrate_bps = a[3].parse().unwrap();
            enc.complexity = a[5].parse().unwrap();
            enc.lsb_depth = 16;
            let frame = rate as usize / 50;
            let mut out = vec![0u8; 4000];
            let mut packets_bin: Vec<u8> = Vec::with_capacity(pcm.len());
            let (mut bytes, mut packets, mut h) = (0usize, 0usize, FNV_OFFSET);
            let mut toc = [0usize; 3];
            for chunk in pcm.chunks_exact(frame * ch) {
                let n = enc.encode(chunk, frame, &mut out).expect("encode");
                for &b in &out[..n] {
                    h = (h ^ u64::from(b)).wrapping_mul(FNV_PRIME);
                }
                let config = out[0] >> 3;
                let mode = if config < 12 {
                    0
                } else if config < 16 {
                    1
                } else {
                    2
                };
                toc[mode] += 1;
                packets_bin.extend_from_slice(&(n as u32).to_le_bytes());
                packets_bin.extend_from_slice(&out[..n]);
                bytes += n;
                packets += 1;
            }
            if let Some(p) = a.get(6) {
                std::fs::File::create(p)
                    .unwrap()
                    .write_all(&packets_bin)
                    .unwrap();
            }
            println!(
                "ANCHOR packets {packets} bytes {bytes} fnv {h:016x} silk {} hybrid {} celt {}",
                toc[0], toc[1], toc[2]
            );
        }
        "dec" => {
            let mut pk = Vec::new();
            std::fs::File::open(&a[2])
                .unwrap()
                .read_to_end(&mut pk)
                .unwrap();
            let rate: i32 = a[3].parse().unwrap();
            let ch: usize = a[4].parse().unwrap();
            let mut dec = OpusDecoder::new(rate, ch).unwrap();
            let frame = rate as usize / 50;
            let mut out = vec![0f32; frame * ch];
            let (mut i, mut packets, mut samples, mut h) = (0usize, 0usize, 0usize, FNV_OFFSET);
            while i + 4 <= pk.len() {
                let n = u32::from_le_bytes([pk[i], pk[i + 1], pk[i + 2], pk[i + 3]]) as usize;
                i += 4;
                let got = dec.decode(&pk[i..i + n], frame, &mut out).expect("decode");
                i += n;
                // One multiply per sample: bit-exact over every output sample, and
                // cheap enough that the harness stays out of the decode census.
                for s in &out[..got * ch] {
                    h = (h ^ u64::from(s.to_bits())).wrapping_mul(FNV_PRIME);
                }
                samples += got;
                packets += 1;
            }
            println!("ANCHOR packets {packets} samples {samples} fnv {h:016x}");
        }
        other => panic!("unknown mode {other}"),
    }
}

// Primary allocator for this target: our rusty_alloc, the pure-Rust mimalloc
// remake - the project convention for every encoder-carrying binary, so an
// instruction count taken here is comparable to what ships.
#[global_allocator]
static RUSTY_ALLOC: rusty_alloc_api::RustyAlloc = rusty_alloc_api::RustyAlloc;
