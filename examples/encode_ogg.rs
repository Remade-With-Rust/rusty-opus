//! Encode a WAV to a real `.opus` (Ogg-encapsulated) file, so our bitstream can
//! be handed to an INDEPENDENT decoder (ffmpeg/libopus).
//!
//! NOT a size benchmark: it writes ONE PAGE PER PACKET, so a 20 ms-frame stream
//! carries ~28 bytes of Ogg header per 20 ms (~11 kb/s of pure framing). Compare
//! the "payload bytes" line it prints, never the file size.
//!
//! Round-tripping through our own decoder cannot catch an encoder/decoder pair
//! that is self-consistently wrong, which is exactly the risk for any
//! bitstream-changing brick (the CELT silence flag, for one). This writes the
//! minimal valid Ogg stream libopus needs: an OpusHead page, an OpusTags page,
//! then one packet per page.
//!
//!   cargo run --release --example encode_ogg -- in.wav out.opus <bitrate> [app]

use std::io::{Read, Write};

use rusty_opus::{Application, OpusEncoder};

fn read_wav(path: &str) -> (u32, u16, Vec<f32>) {
    let mut buf = Vec::new();
    std::fs::File::open(path).unwrap().read_to_end(&mut buf).unwrap();
    let rate = u32::from_le_bytes([buf[24], buf[25], buf[26], buf[27]]);
    let channels = u16::from_le_bytes([buf[22], buf[23]]);
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
    let s = buf[off..off + len]
        .chunks_exact(2)
        .map(|b| i16::from_le_bytes([b[0], b[1]]) as f32 / 32768.0)
        .collect();
    (rate, channels, s)
}

const CRC_POLY: u32 = 0x04c1_1db7;

fn crc32(data: &[u8]) -> u32 {
    // Ogg uses a non-reflected CRC-32 with zero init and no final xor.
    let mut crc: u32 = 0;
    for &b in data {
        crc ^= (b as u32) << 24;
        for _ in 0..8 {
            crc = if crc & 0x8000_0000 != 0 {
                (crc << 1) ^ CRC_POLY
            } else {
                crc << 1
            };
        }
    }
    crc
}

#[allow(clippy::too_many_arguments)]
fn write_page(
    out: &mut Vec<u8>,
    body: &[u8],
    granule: u64,
    serial: u32,
    seq: u32,
    header_type: u8,
) {
    // Segment table: 255-byte laces, final lace < 255 terminates the packet.
    let mut segs = Vec::new();
    let mut rem = body.len();
    loop {
        if rem >= 255 {
            segs.push(255u8);
            rem -= 255;
        } else {
            segs.push(rem as u8);
            break;
        }
    }
    assert!(segs.len() <= 255, "packet too large for one page");
    let mut page = Vec::with_capacity(27 + segs.len() + body.len());
    page.extend_from_slice(b"OggS");
    page.push(0); // version
    page.push(header_type);
    page.extend_from_slice(&granule.to_le_bytes());
    page.extend_from_slice(&serial.to_le_bytes());
    page.extend_from_slice(&seq.to_le_bytes());
    page.extend_from_slice(&0u32.to_le_bytes()); // CRC placeholder
    page.push(segs.len() as u8);
    page.extend_from_slice(&segs);
    page.extend_from_slice(body);
    let crc = crc32(&page);
    page[22..26].copy_from_slice(&crc.to_le_bytes());
    out.extend_from_slice(&page);
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let (rate, channels, pcm) = read_wav(&a[1]);
    let bitrate: i32 = a.get(3).map(|s| s.parse().unwrap()).unwrap_or(32_000);
    let app = match a.get(4).map(|s| s.as_str()) {
        Some("voip") => Application::Voip,
        _ => Application::Audio,
    };
    let ch = channels as usize;
    let frame = rate as usize / 50;
    let step = frame * ch;
    let serial: u32 = 0x5255_5354;

    let mut enc = OpusEncoder::new(rate as i32, ch, app).unwrap();
    enc.bitrate_bps = bitrate;
    // 5th arg "cbr" exercises the constant-bitrate path, which the silence
    // brick treats differently (no coder shrink — the packet length is fixed).
    if a.get(5).map(|s| s == "cbr").unwrap_or(false) {
        enc.use_cbr = true;
    }

    let mut out = Vec::new();
    let mut seq = 0u32;

    // OpusHead: version 1, channels, 312-sample pre-skip, original rate, no gain.
    let mut head = Vec::new();
    head.extend_from_slice(b"OpusHead");
    head.push(1);
    head.push(channels as u8);
    head.extend_from_slice(&312u16.to_le_bytes());
    head.extend_from_slice(&rate.to_le_bytes());
    head.extend_from_slice(&0i16.to_le_bytes());
    head.push(0); // channel mapping family 0
    write_page(&mut out, &head, 0, serial, seq, 0x02);
    seq += 1;

    let mut tags = Vec::new();
    tags.extend_from_slice(b"OpusTags");
    let vendor = b"rusty-opus";
    tags.extend_from_slice(&(vendor.len() as u32).to_le_bytes());
    tags.extend_from_slice(vendor);
    tags.extend_from_slice(&0u32.to_le_bytes());
    write_page(&mut out, &tags, 0, serial, seq, 0x00);
    seq += 1;

    let mut buf = vec![0u8; 4000];
    let mut granule: u64 = 312;
    let nframes = pcm.len() / step;
    let mut total = 0usize;
    for (i, chunk) in pcm.chunks_exact(step).enumerate() {
        let n = enc.encode(chunk, frame, &mut buf).expect("encode");
        total += n;
        granule += (frame as u64) * 48000 / rate as u64;
        let last = i + 1 == nframes;
        write_page(&mut out, &buf[..n], granule, serial, seq, if last { 0x04 } else { 0x00 });
        seq += 1;
    }
    std::fs::File::create(&a[2]).unwrap().write_all(&out).unwrap();
    eprintln!(
        "wrote {} ({} packets, {} payload bytes, {:.1} kbps)",
        a[2],
        nframes,
        total,
        total as f64 * 8.0 / (nframes as f64 * frame as f64 / rate as f64) / 1000.0
    );
}

// Primary allocator for this target: our rusty_alloc, the pure-Rust mimalloc
// remake. Codec hot paths allocate heavily and the system heap dominates the
// profile there (measured 1.38x end-to-end on AV2 decode). Per project
// convention this belongs in binary/bench/example roots, never in a library --
// a library that declares one hijacks every dependent's allocator choice.
#[global_allocator]
static RUSTY_ALLOC: rusty_alloc_api::RustyAlloc = rusty_alloc_api::RustyAlloc;
