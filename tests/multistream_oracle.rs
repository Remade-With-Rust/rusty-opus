//! Multistream (surround, mapping families 1 and 255) decode against libopus.
//!
//! Fixtures come from a libopus 1.6.1 oracle (`opus_multistream_encode_float`
//! then `opus_multistream_decode_float`): `<name>.msp` holds the header and
//! raw packets, `<name>.ref` libopus's decoded float PCM. The `.msp` layout:
//!
//! ```text
//! "MSPK" u32 sample_rate  u8 channels u8 streams u8 coupled u8 family
//! u8 mapping[channels]  u16 pre_skip  u32 input_len  u32 n_packets
//! n_packets x (u32 len, len bytes)
//! ```
//!
//! All integers little-endian. Set `RUSTY_OPUS_MS_ORACLE=<dir>` to also run
//! every `.msp`/`.ref` pair in a directory and print a per-config table.

use std::path::Path;

use rusty_opus::multistream::{ChannelLayout, OpusMSDecoder};

struct Msp {
    sample_rate: i32,
    channels: usize,
    streams: usize,
    coupled: usize,
    family: u8,
    mapping: Vec<u8>,
    packets: Vec<Vec<u8>>,
}

fn u32le(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

fn parse_msp(b: &[u8]) -> Msp {
    assert_eq!(&b[..4], b"MSPK", "not an .msp file");
    let sample_rate = u32le(b, 4) as i32;
    let (channels, streams, coupled, family) =
        (b[8] as usize, b[9] as usize, b[10] as usize, b[11]);
    let mapping = b[12..12 + channels].to_vec();
    let mut at = 12 + channels + 2 + 4;
    let n = u32le(b, at) as usize;
    at += 4;
    let mut packets = Vec::with_capacity(n);
    for _ in 0..n {
        let len = u32le(b, at) as usize;
        at += 4;
        packets.push(b[at..at + len].to_vec());
        at += len;
    }
    assert_eq!(at, b.len(), "trailing bytes in .msp");
    Msp {
        sample_rate,
        channels,
        streams,
        coupled,
        family,
        mapping,
        packets,
    }
}

/// Per-config comparison against the libopus reference.
#[derive(Debug)]
struct Report {
    packets: usize,
    samples: usize,
    /// Largest |ours - libopus| over every output sample (float units).
    max_diff: f32,
    /// Packets whose decoded samples match libopus bit for bit.
    exact_packets: usize,
    /// Largest |ours - libopus| per output channel.
    per_channel: Vec<f32>,
}

impl Report {
    fn bit_exact(&self) -> bool {
        self.exact_packets == self.packets
    }
}

fn compare(msp: &Msp, reference: &[f32]) -> Report {
    let layout = ChannelLayout::new(msp.channels, msp.streams, msp.coupled, msp.mapping.clone())
        .expect("valid layout");
    let mut dec = OpusMSDecoder::with_layout(msp.sample_rate, layout).expect("decoder");
    let cap = msp.sample_rate as usize / 1000 * 120;
    let mut out = vec![0f32; cap * msp.channels];
    let mut at = 0usize;
    let mut report = Report {
        packets: msp.packets.len(),
        samples: 0,
        max_diff: 0.0,
        exact_packets: 0,
        per_channel: vec![0.0; msp.channels],
    };
    for p in &msp.packets {
        let n = match dec.decode(p, cap, &mut out) {
            Ok(n) => n,
            Err(e) => panic!("decode packet {}: {e:?}", report.exact_packets),
        };
        let len = n * msp.channels;
        let want = &reference[at..at + len];
        let got = &out[..len];
        if got
            .iter()
            .zip(want)
            .all(|(a, b)| a.to_bits() == b.to_bits())
        {
            report.exact_packets += 1;
        }
        for (i, (a, b)) in got.iter().zip(want).enumerate() {
            let d = (a - b).abs();
            report.max_diff = report.max_diff.max(d);
            let c = &mut report.per_channel[i % msp.channels];
            *c = c.max(d);
        }
        at += len;
        report.samples += n;
    }
    assert_eq!(at, reference.len(), "decoded length differs from libopus");
    report
}

fn load(dir: &Path, name: &str) -> (Msp, Vec<f32>) {
    let msp = parse_msp(&std::fs::read(dir.join(format!("{name}.msp"))).unwrap());
    let reference: Vec<f32> = std::fs::read(dir.join(format!("{name}.ref")))
        .unwrap()
        .chunks_exact(4)
        .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .collect();
    (msp, reference)
}

/// Every `.msp` in `$RUSTY_OPUS_MS_ORACLE`, as a table (run with
/// `--nocapture`). Informational: asserts only that each config decodes.
#[test]
fn oracle_directory_table() {
    let Ok(dir) = std::env::var("RUSTY_OPUS_MS_ORACLE") else {
        return;
    };
    let dir = Path::new(&dir);
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .filter_map(|e| {
            let p = e.unwrap().path();
            (p.extension()? == "msp").then(|| p.file_stem().unwrap().to_string_lossy().into())
        })
        .collect();
    names.sort();
    println!(
        "{:<20} {:>3} {:>3} {:>7} {:>8} {:>12} {:>9}  bit-exact",
        "config", "fam", "ch", "packets", "samples", "max|diff|", "s16 LSB"
    );
    for name in names {
        let (msp, reference) = load(dir, &name);
        let r = compare(&msp, &reference);
        println!(
            "{:<20} {:>3} {:>3} {:>7} {:>8} {:>12.3e} {:>9.3}  {} ({}/{})  per-ch s16 {:?}",
            name,
            msp.family,
            msp.channels,
            r.packets,
            r.samples,
            r.max_diff,
            r.max_diff * 32768.0,
            if r.bit_exact() { "yes" } else { "no" },
            r.exact_packets,
            r.packets,
            r.per_channel
                .iter()
                .map(|d| (d * 32768.0).round() as i64)
                .collect::<Vec<_>>()
        );
    }
}

/// FNV-1a 64 over the little-endian f32 bytes (what `to_r16.py` stores).
fn fnv1a64(samples: &[f32]) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    for s in samples {
        for b in s.to_le_bytes() {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x0100_0000_01b3);
        }
    }
    h
}

/// Decode every packet of `msp` into one interleaved buffer.
fn decode_all(msp: &Msp) -> Vec<f32> {
    let layout = ChannelLayout::new(msp.channels, msp.streams, msp.coupled, msp.mapping.clone())
        .expect("valid layout");
    let mut dec = OpusMSDecoder::with_layout(msp.sample_rate, layout).expect("decoder");
    let cap = msp.sample_rate as usize / 1000 * 120;
    let mut out = vec![0f32; cap * msp.channels];
    let mut all = Vec::new();
    for p in &msp.packets {
        let n = dec.decode(p, cap, &mut out).expect("decode");
        all.extend_from_slice(&out[..n * msp.channels]);
    }
    all
}

/// The committed fixtures (`fixtures/multistream`): packets from libopus
/// 1.6.1's multistream encoder; `.r16` holds an FNV-1a hash of libopus's
/// whole float decode plus every 4th frame of it as rounded s16. Our decode
/// must stay within 1 s16 LSB of libopus on every stored sample; SILK-only
/// streams (`exact`) must match libopus's float output bit for bit.
#[test]
fn libopus_fixtures() {
    const FIXTURES: [(&str, bool); 5] = [
        // 5.1 from libopus's surround encoder: coupled CELT, mono hybrid.
        ("ms_f1_6ch_surround_20", false),
        // 7.1 CELT, 60 ms (code-3, multi-frame self-delimited sub-packets).
        ("ms_f1_8ch_celt_60", false),
        // 3.0 SILK through the generic multistream encoder.
        ("ms_f1_3ch_silk_20", true),
        // Family 255: a silent output and one mono stream on two outputs.
        ("ms_f255_6ch_hyb_10", false),
        // Family 255, no coupled streams, CBR (a padded last stream).
        ("ms_f255_4ch_silk_cbr_20", true),
    ];
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/multistream");
    for (name, exact) in FIXTURES {
        let msp = parse_msp(&std::fs::read(dir.join(format!("{name}.msp"))).unwrap());
        let r16 = std::fs::read(dir.join(format!("{name}.r16"))).unwrap();
        assert_eq!(&r16[..4], b"R16\0");
        let step = u32le(&r16, 4) as usize;
        let hash = u64::from_le_bytes(r16[8..16].try_into().unwrap());
        let frames = u32le(&r16, 16) as usize;
        assert_eq!(r16[20] as usize, msp.channels);
        let reference: Vec<i16> = r16[21..]
            .chunks_exact(2)
            .map(|b| i16::from_le_bytes([b[0], b[1]]))
            .collect();

        let ours = decode_all(&msp);
        let ch = msp.channels;
        assert_eq!(ours.len(), frames * ch, "{name}: decoded length");
        let mut worst = 0i32;
        for (k, want) in reference.chunks_exact(ch).enumerate() {
            let i = k * step;
            for c in 0..ch {
                let got = (ours[i * ch + c] * 32768.0)
                    .round()
                    .clamp(-32768.0, 32767.0) as i32;
                worst = worst.max((got - i32::from(want[c])).abs());
            }
        }
        assert!(
            worst <= 1,
            "{name}: {worst} s16 LSB from libopus (sampled every {step} frames)"
        );
        if exact {
            assert_eq!(fnv1a64(&ours), hash, "{name}: not bit-exact with libopus");
        }
    }
}
