//! Property tests for the invariants rusty-opus documents (README "Safety
//! guarantees", docs/threat-model.md). Each property names the invariant it checks.
//!
//! Inputs are adversarial by construction: arbitrary byte strings for every
//! untrusted-input entry point, and arbitrary f32 PCM (including NaN, ±Inf and
//! denormals) for the encoder.

use proptest::prelude::*;
use rusty_opus::multistream::OpusMSDecoder;
use rusty_opus::range_coder::RangeCoder;
use rusty_opus::repacketizer::{self, Repacketizer};
use rusty_opus::{Application, Error, OpusDecoder, OpusEncoder};

const RATES: [i32; 5] = [8000, 12000, 16000, 24000, 48000];

fn rate() -> impl Strategy<Value = i32> {
    prop::sample::select(RATES.to_vec())
}

/// Any f32, weighted toward the values that break DSP code.
fn hostile_sample() -> impl Strategy<Value = f32> {
    prop_oneof![
        4 => -1.0f32..1.0,
        1 => Just(f32::NAN),
        1 => Just(f32::INFINITY),
        1 => Just(f32::NEG_INFINITY),
        1 => Just(f32::MIN_POSITIVE / 2.0), // denormal
        1 => any::<f32>(),
    ]
}

fn config() -> ProptestConfig {
    ProptestConfig {
        cases: 192,
        ..ProptestConfig::default()
    }
}

/// Encode `frames` frames of a deterministic tone; returns the packets.
fn tone_packets(rate: i32, ch: usize, app: Application, frames: usize) -> Vec<Vec<u8>> {
    let frame = rate as usize / 50;
    let mut enc = OpusEncoder::new(rate, ch, app).unwrap();
    let mut out = vec![0u8; 1500];
    (0..frames)
        .map(|f| {
            let pcm: Vec<f32> = (0..frame * ch)
                .map(|i| ((f * frame * ch + i) as f32 * 0.031).sin() * 0.4)
                .collect();
            let n = enc.encode(&pcm, frame, &mut out).unwrap();
            out[..n].to_vec()
        })
        .collect()
}

proptest! {
    #![proptest_config(config())]

    /// INVARIANT: decoding never panics on any byte string, at any rate and channel
    /// count, and a returned sample count never exceeds the frame size.
    #[test]
    fn decode_never_panics(rate in rate(), stereo in any::<bool>(),
                           packets in prop::collection::vec(prop::collection::vec(any::<u8>(), 0..1500), 1..6)) {
        let ch = if stereo { 2 } else { 1 };
        let mut dec = OpusDecoder::new(rate, ch).unwrap();
        let frame = rate as usize * 120 / 1000; // largest legal packet: 120 ms
        let mut out = vec![0f32; frame * ch];
        for p in &packets {
            if let Ok(n) = dec.decode(p, frame, &mut out) {
                prop_assert!(n <= frame);
            }
        }
    }

    /// INVARIANT: the in-band FEC entry point never panics on any byte string.
    #[test]
    fn decode_fec_never_panics(rate in rate(), stereo in any::<bool>(),
                               bytes in prop::collection::vec(any::<u8>(), 0..1500)) {
        let ch = if stereo { 2 } else { 1 };
        let mut dec = OpusDecoder::new(rate, ch).unwrap();
        let frame = rate as usize / 50;
        let mut out = vec![0f32; frame * ch];
        let _ = dec.decode_fec(&bytes, frame, &mut out);
    }

    /// INVARIANT: an undersized output buffer is an error, never a panic or an
    /// out-of-bounds write.
    #[test]
    fn small_output_buffer_is_an_error(rate in rate(), len in 0usize..64,
                                       bytes in prop::collection::vec(any::<u8>(), 1..400)) {
        let mut dec = OpusDecoder::new(rate, 2).unwrap();
        let mut out = vec![0f32; len];
        let frame = rate as usize / 50;
        let _ = dec.decode(&bytes, frame, &mut out);
    }

    /// INVARIANT: packet parsing / repacketizing / padding never panic on any
    /// byte string.
    #[test]
    fn packet_utilities_never_panic(bytes in prop::collection::vec(any::<u8>(), 0..1500),
                                    target in 0usize..2000, self_delim in any::<bool>()) {
        let _ = repacketizer::nb_frames(&bytes);
        let _ = repacketizer::parse_packet(&bytes, self_delim);
        let _ = repacketizer::unpad_packet(&bytes);
        let mut p = bytes.clone();
        let _ = repacketizer::pad_packet(&mut p, target);
        let mut rp = Repacketizer::new();
        if rp.cat(&bytes).is_ok() {
            let _ = rp.out();
            let _ = rp.out_self_delimited();
        }
    }

    /// INVARIANT: the multistream decoder never panics on any byte string.
    #[test]
    fn multistream_decode_never_panics(channels in 1usize..=6,
                                       bytes in prop::collection::vec(any::<u8>(), 0..2000)) {
        let family = i32::from(channels > 2);
        let mut dec = OpusMSDecoder::new(48000, channels, family).unwrap();
        let mut out = vec![0f32; 960 * channels];
        let _ = dec.decode(&bytes, 960, &mut out);
    }

    /// INVARIANT: the encoder accepts any f32 input (NaN, ±Inf, denormals,
    /// out-of-range) without panicking, and every packet it produces decodes to a
    /// full frame.
    #[test]
    fn encoder_survives_hostile_pcm(rate in rate(), stereo in any::<bool>(),
                                    app in prop::sample::select(vec![Application::Voip, Application::Audio, Application::RestrictedLowDelay]),
                                    pcm in prop::collection::vec(hostile_sample(), 960 * 2 * 3)) {
        let ch = if stereo { 2 } else { 1 };
        let frame = rate as usize / 50;
        let mut enc = OpusEncoder::new(rate, ch, app).unwrap();
        let mut dec = OpusDecoder::new(rate, ch).unwrap();
        let mut packet = vec![0u8; 1500];
        let mut out = vec![0f32; frame * ch];
        for chunk in pcm.chunks_exact(frame * ch).take(3) {
            let n = enc.encode(chunk, frame, &mut packet).unwrap();
            prop_assert!(n >= 1 && n <= packet.len());
            prop_assert_eq!(dec.decode(&packet[..n], frame, &mut out).unwrap(), frame);
        }
    }

    /// INVARIANT: range coder round-trip — every symbol encoded is decoded back.
    #[test]
    fn range_coder_roundtrip(ops in prop::collection::vec((0u8..3, any::<u32>(), 1u32..17), 1..200)) {
        let mut enc = RangeCoder::new_encoder(4096);
        for &(kind, v, w) in &ops {
            match kind {
                0 => enc.enc_bits(v & ((1u32 << w) - 1), w),
                1 => enc.encode_bit_logp(v & 1 == 1, w.min(15)),
                _ => enc.enc_uint(v % (w * 977 + 2), w * 977 + 2),
            }
        }
        let bytes = enc.finish();
        let mut dec = RangeCoder::new_decoder(&bytes);
        for &(kind, v, w) in &ops {
            match kind {
                0 => prop_assert_eq!(dec.dec_bits(w), v & ((1u32 << w) - 1)),
                1 => prop_assert_eq!(dec.decode_bit_logp(w.min(15)), v & 1 == 1),
                _ => prop_assert_eq!(dec.dec_uint(w * 977 + 2), v % (w * 977 + 2)),
            }
        }
    }
}

/// INVARIANT: pad then unpad is the identity on valid packets, and padding hits
/// the requested size exactly.
#[test]
fn pad_unpad_identity() {
    for &rate in &RATES {
        for p in tone_packets(rate, 1, Application::Audio, 8) {
            let mut padded = p.clone();
            let target = p.len() + 37;
            repacketizer::pad_packet(&mut padded, target).unwrap();
            assert_eq!(padded.len(), target);
            assert_eq!(repacketizer::unpad_packet(&padded).unwrap(), p);
        }
    }
}

/// INVARIANT: merging frames with the repacketizer and splitting them again
/// reproduces the original frames byte for byte.
#[test]
fn repacketize_split_merge_identity() {
    let packets = tone_packets(48000, 2, Application::Audio, 6);
    let mut rp = Repacketizer::new();
    for p in &packets[..3] {
        rp.cat(p).unwrap();
    }
    let merged = rp.out().unwrap();
    assert_eq!(repacketizer::nb_frames(&merged).unwrap(), 3);
    let mut split = Repacketizer::new();
    split.cat(&merged).unwrap();
    for (i, p) in packets[..3].iter().enumerate() {
        assert_eq!(&split.out_range(i, i + 1).unwrap(), p);
    }
}

/// INVARIANT: invalid configuration is reported as `Error::BadArg`, never a panic.
#[test]
fn invalid_configuration_is_bad_arg() {
    assert!(matches!(
        OpusEncoder::new(44100, 2, Application::Audio),
        Err(Error::BadArg(_))
    ));
    assert!(matches!(
        OpusEncoder::new(48000, 3, Application::Audio),
        Err(Error::BadArg(_))
    ));
    assert!(matches!(OpusDecoder::new(22050, 1), Err(Error::BadArg(_))));
    assert!(matches!(OpusDecoder::new(48000, 0), Err(Error::BadArg(_))));
    let mut enc = OpusEncoder::new(48000, 1, Application::Audio).unwrap();
    let mut out = [0u8; 1500];
    assert!(matches!(
        enc.encode(&[0.0; 1000], 1000, &mut out),
        Err(Error::BadArg(_))
    ));
    assert!(matches!(
        enc.encode(&[0.0; 960], 960, &mut [0u8; 1]),
        Err(Error::BufferTooSmall(_))
    ));
}

/// INVARIANT: the encoder never panics on a small output buffer, at any rate,
/// layout, application, frame size or rate-control mode, and whatever it
/// returns is a packet the decoder accepts. (Regressions, both fuzz-found: a
/// 2-byte VBR buffer, or a 40-120 ms frame with a small buffer, overran the payload
/// layout; CBR at a high rate with a large buffer planned a frame over 1275 bytes.)
#[test]
fn encoder_small_output_buffer_never_panics() {
    for &rate in &RATES {
        let fs = rate as usize;
        let frames = [
            fs / 400,
            fs / 200,
            fs / 100,
            fs / 50,
            fs / 25,
            3 * fs / 50,
            6 * fs / 50,
        ];
        for ch in 1..=2usize {
            for app in [
                Application::Voip,
                Application::Audio,
                Application::RestrictedLowDelay,
            ] {
                for cbr in [false, true] {
                    for &bitrate in &[500, 6_000, 64_000, 510_000] {
                        for &frame in &frames {
                            let mut enc = OpusEncoder::new(rate, ch, app).unwrap();
                            enc.use_cbr = cbr;
                            enc.bitrate_bps = bitrate;
                            let mut dec = OpusDecoder::new(rate, ch).unwrap();
                            let pcm: Vec<f32> = (0..frame * ch)
                                .map(|i| (i as f32 * 0.05).sin() * 0.5)
                                .collect();
                            let mut out = vec![0f32; frame * ch];
                            for size in [2usize, 3, 4, 5, 8, 12, 40, 1500, 4000] {
                                let mut packet = vec![0u8; size];
                                if let Ok(n) = enc.encode(&pcm, frame, &mut packet) {
                                    assert!((1..=size).contains(&n));
                                    assert_eq!(
                                        dec.decode(&packet[..n], frame, &mut out).unwrap(),
                                        frame
                                    );
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// INVARIANT: no packet carries a frame over 1275 bytes, however large the
/// caller's buffer. (Regression, fuzz-found: SILK-only CBR at 510 kb/s with a
/// 4000-byte buffer planned a 3825-byte 60 ms frame and overran the SILK rate
/// loop's 1275-byte snapshot.)
#[test]
fn encoder_never_exceeds_max_frame_size() {
    let mut seed = 0x2545_f491_u32;
    let mut noise = || {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        (seed >> 8) as f32 / (1 << 23) as f32 - 1.0
    };
    for &rate in &[8000, 12000, 16000] {
        let fs = rate as usize;
        for ch in 1..=2usize {
            let mut enc = OpusEncoder::new(rate, ch, Application::Voip).unwrap();
            enc.bitrate_bps = 510_000;
            enc.use_cbr = true;
            enc.use_inband_fec = true;
            enc.packet_loss_perc = 50;
            enc.complexity = -1;
            let mut dec = OpusDecoder::new(rate, ch).unwrap();
            for &frame in &[fs / 50, fs / 25, 3 * fs / 50] {
                let mut out = vec![0f32; frame * ch];
                for _ in 0..3 {
                    let pcm: Vec<f32> = (0..frame * ch).map(|_| noise()).collect();
                    let mut packet = vec![0u8; 4000];
                    let n = enc.encode(&pcm, frame, &mut packet).unwrap();
                    assert!(n <= 1276, "{rate} Hz {ch} ch {frame}: {n}-byte packet");
                    assert_eq!(dec.decode(&packet[..n], frame, &mut out).unwrap(), frame);
                }
            }
        }
    }
}

/// INVARIANT: with in-band FEC on, every packet's range-coder final state
/// matches between encoder and decoder — the per-packet check libopus applies.
/// (Regression: LBRR frames omitted the LTP-scaling symbol and, in stereo, the
/// stereo prediction, so every decoder desynced at the first voiced LBRR frame.)
#[test]
fn fec_streams_keep_range_coder_in_sync() {
    for &rate in &[8000, 12000, 16000, 24000, 48000] {
        let fs = rate as usize;
        for ch in 1..=2usize {
            let configs = [(16_000, fs / 50), (32_000, fs / 25), (24_000, 3 * fs / 50)];
            for (&(bitrate, frame), cbr) in configs.iter().flat_map(|c| [(c, false), (c, true)]) {
                let mut enc = OpusEncoder::new(rate, ch, Application::Voip).unwrap();
                enc.bitrate_bps = bitrate;
                enc.use_cbr = cbr;
                enc.use_inband_fec = true;
                enc.packet_loss_perc = 20;
                let mut dec = OpusDecoder::new(rate, ch).unwrap();
                let mut out = vec![0f32; frame * ch];
                let mut packet = [0u8; 1500];
                for f in 0..25 {
                    // Voiced-like content: a pitched pulse train with formant-ish decay.
                    let pcm: Vec<f32> = (0..frame * ch)
                        .map(|i| {
                            let t = (f * frame + i / ch) as f32 / rate as f32;
                            let ph = (t * 140.0).fract();
                            0.5 * (-ph * 18.0).exp()
                                * (t * 2.0 * std::f32::consts::PI * 700.0).sin()
                        })
                        .collect();
                    let n = enc.encode(&pcm, frame, &mut packet).unwrap();
                    dec.decode(&packet[..n], frame, &mut out).unwrap();
                    assert_eq!(
                        dec.last_range,
                        enc.final_range(),
                        "{rate} Hz {ch} ch {bitrate} b/s cbr={cbr} frame {frame}: packet {f}"
                    );
                }
            }
        }
    }
}
