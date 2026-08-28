//! Temporary diagnostic #2: is the ENCODER platform-dependent, independently of
//! the fixture?
//!
//! Probe #1 proved the oracle's synthesised input differs across platforms
//! (it is built with `f32::sin`, which resolves to the host libm). That alone
//! explains the oracle failure. This asks the separate question: given a
//! bit-identical input built with no transcendentals at all, does the encoder
//! still produce the same bytes everywhere?
//!
//! It matters because it decides the fix. If the encoder is portable, making the
//! fixture deterministic is enough. If it is not, the oracle can never be a
//! cross-platform gate and must say so.

use rusty_opus::{Application, OpusEncoder};

/// Deterministic input: integer LCG only, no floating-point transcendentals,
/// so this array is bit-identical on every platform by construction.
fn synth_deterministic(n: usize, channels: usize) -> Vec<f32> {
    let mut s: u64 = 0x1234_5678_9abc_def0;
    let mut out = vec![0.0f32; n * channels];
    for v in out.iter_mut() {
        s = s.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        // Map the top bits to a small exactly-representable rational.
        let q = ((s >> 44) as i32) - 1024; // -1024..1023
        *v = q as f32 / 4096.0; // exact in binary32
    }
    out
}

fn hash_bytes(v: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in v {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100_0000_01b3);
    }
    h
}

fn fhash(v: &[f32]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for x in v {
        for b in x.to_bits().to_le_bytes() {
            h ^= b as u64;
            h = h.wrapping_mul(0x100_0000_01b3);
        }
    }
    h
}

#[test]
fn encoder_portability_probe() {
    let frame = 960; // 20 ms @ 48k
    let frames = 200;
    let pcm = synth_deterministic(frame * frames, 2);
    println!("ENCPROBE input_hash=0x{:016x} (must be identical everywhere)", fhash(&pcm));

    let mut enc = OpusEncoder::new(48000, 2, Application::Audio).expect("encoder");
    enc.bitrate_bps = 128_000;
    enc.use_cbr = true;
    let mut all = Vec::new();
    let mut buf = vec![0u8; 4000];
    for f in 0..frames {
        let s = f * frame * 2;
        let n = enc.encode(&pcm[s..s + frame * 2], frame, &mut buf).expect("encode");
        all.extend_from_slice(&buf[..n]);
    }
    println!("ENCPROBE output_hash=0x{:016x} bytes={}", hash_bytes(&all), all.len());

    // Frozen on Windows/MSVC. A mismatch here means the ENCODER itself is
    // platform-dependent, not merely the oracle's fixture.
    assert_eq!(
        hash_bytes(&all),
        0x4b25_66f2_e882_f3e9u64,
        "encoder output from a bit-identical input"
    );
}
