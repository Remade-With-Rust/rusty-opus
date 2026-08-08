# rusty-opus

[![Remade With Rust](https://img.shields.io/badge/Remade%20With-Rust-000?logo=rust&logoColor=fff)](https://github.com/remade-with-rust)
[![By Mata Network](https://img.shields.io/badge/by-Mata%20Network-5b2be0)](https://www.mata.network)
[![License: BSD-3-Clause](https://img.shields.io/badge/license-BSD--3--Clause-blue)](COPYING)
[![Pure Rust](https://img.shields.io/badge/pure-Rust%20%C2%B7%20no%20C%20%C2%B7%20no%20FFI-orange?logo=rust&logoColor=fff)](#)

**A pure-Rust implementation of the [Opus audio codec](https://opus-codec.org/) (RFC 6716 / RFC 8251)** —
no C, no FFI, no build-time toolchain. Encoder *and* decoder, SILK + CELT + Hybrid,
conformance-verified against the reference — **`libopus` quality at ~1.5× its per-core encode
speed**, measured across 18 content classes.
The only `unsafe` is in the SIMD kernels (AVX2 / NEON, runtime-detected, each with a scalar
fallback that doubles as its correctness oracle).

> **rusty-opus** is [Mata Network](https://www.mata.network)'s performance fork of
> [`opus-rs`](https://github.com/restsend/opus-rs) (BSD-3-Clause), maintained under the
> [Remade With Rust](https://github.com/remade-with-rust) initiative. It is the Opus backend of
> [remade_ffmpeg_rs](https://github.com/Remade-With-Rust/remade_ffmpeg_rs), and usable
> standalone. Every optimization is gated against a reference oracle — **byte-identical** where
> the bitstream must not move, **PEAQ-validated** where it may.

## Why rusty-opus?

- **Pure Rust, no C** — no `libopus`, no `bindgen`, no build-time toolchain surprises; trivial
  to cross-compile and embed. `unsafe` is confined to the SIMD kernels, each gated behind a
  runtime CPU-feature check and backed by a scalar fallback.
- **Conformance-verified** — the decoder is **bit-exact on all 12 official RFC 6716 / RFC 8251
  test vectors** (float-libopus parity to 1e-6) and decodes `libopus`'s own streams to identical
  output.
- **Fast** — hand-written **AVX2/FMA** (x86-64) and **NEON** (aarch64) kernels put single-thread
  encode **1.50× ahead of `libopus` on CELT speech and 1.60× on stereo music**, and within 4% on
  the SILK path (measured, [see below](#performance--single-thread-encode-three-coding-paths)).
  **Frame-parallel encoding** adds wall-clock on top, since `libopus` is single-threaded per stream.
- **Complete** — SILK, CELT, and Hybrid modes; VBR/CBR; DTX; in-band FEC; packet-loss
  concealment; comfort-noise generation; multistream / surround (5.1 / 7.1); a repacketizer.
- **Permissive** — BSD-3-Clause, all the way down.

## Add it to your project

```toml
[dependencies]
rusty-opus = "0.9"
```

The crate is imported as `rusty_opus`:

```rust
use rusty_opus::{OpusEncoder, OpusDecoder, Application};

// --- Encode ---
let mut encoder = OpusEncoder::new(16_000, 1, Application::Voip).unwrap();
encoder.bitrate_bps = 16_000;
encoder.use_cbr = true;

let input = vec![0.0f32; 320];       // one 20 ms frame at 16 kHz, mono
let mut packet = vec![0u8; 4000];
let n = encoder.encode(&input, 320, &mut packet).unwrap();

// --- Decode ---
let mut decoder = OpusDecoder::new(16_000, 1).unwrap();
let mut pcm = vec![0.0f32; 320];
let samples = decoder.decode(&packet[..n], 320, &mut pcm).unwrap();
```

Runnable examples live in [`examples/`](examples/) — WAV round-trip, packet-loss concealment,
in-band FEC, multistream, and the `opus_demo`-compatible conformance harnesses:

```bash
cargo run --release --example roundtrip          # encode → decode a WAV
cargo run --release --example plc_test           # packet-loss concealment
cargo run --release --example roundtrip_parallel # frame-parallel encode
cargo test  --release                            # full suite incl. conformance vectors
```

## Correctness

The reference C decoder is the oracle. Every brick is gated **byte-identical** against a scalar
twin where the bitstream must not move (SIMD kernels, entropy paths, resampler), and **PEAQ-ODG
validated** where it legitimately may (encoder analysis, block switching, quality tuning). The
decoder passes all 12 official conformance vectors bit-exactly; the encoder round-trips through
`libopus` and vice-versa with zero interop errors (including 5.1 / 7.1 multistream).

## Quality — measured, per content class

Three encoders, one corpus, one metric. **18 content classes × 5 bitrates each**, scored with
an external **PEAQ ODG** oracle and compared as **BD-ODG at matched *actual* bitrate** — not at
the nominal target, because `libopus`'s VBR overshoots its target by 15–20% on this corpus and
comparing at the nominal rate would hand it those bits for free.

| | vs **C libopus** | vs **FFmpeg's native Opus encoder** |
|---|---:|---:|
| mean BD-ODG, 13 core classes | **−0.015** (parity) | **+1.532** |
| mean BD-ODG, 5 music-stress classes | **+0.009** (parity) | **+2.002** |
| worst class overall | −0.416 | **+0.233** |
| classes won vs ffmpeg-native | — | **18 / 18** |

### The music-stress classes

The original corpus was solo acoustic classical and nothing else — measured with
`tools/corpus_coverage.py` it never exceeded 0.137 bass-energy fraction, never went below
14 dB crest, and its fastest real material was 7.2 onsets/s. That left sub-bass, loudness-war
masters and dense fast content untested, which is where an unnoticed failure would live. Those
classes now exist, and they run to **256 kb/s** (the old ladder stopped at 160):

| class | what it stresses | vs libopus | vs ffmpeg-native |
|---|---|---:|---:|
| bass-heavy electronic (bass frac **0.634**) | sub-bass allocation, low CELT bands | **+0.203** | +3.294 |
| fast/dense, 40 hits/s | block switching, transient density | **+0.014** | +2.659 |
| distorted rock, decorrelated stereo | dense harmonics to Nyquist | **+0.002** | +1.144 |
| loud master (**8.0 dB** crest) | rate control at constant near-full-scale | −0.031 | +1.319 |
| vocal (real PD Mozart aria) | formants + strong harmonics | −0.143 | +1.593 |

**No failure mode appeared in any of them** — and the class we were most exposed on, sub-bass,
is one we're ahead on. Four of the five are synthetic and labelled as such in the corpus README:
they are correct for stressing a mechanism, not a substitute for real commercial masters.

**Against `libopus` we are at parity** — a mean of −0.015 ODG is well inside the noise of the
metric. We are genuinely *ahead* on noise-like and transient material (applause **+1.107**,
percussive **+0.363**, noisy speech +0.079) and behind on VoIP-application low-rate speech
(−0.416 mixed, −0.200 speech) and stereo music (−0.217 piano, −0.162 guitar).

**Against FFmpeg's built-in `-c:a opus` encoder we win every single class**, by +1.53 ODG on
average. Worth stating plainly, though: that encoder is experimental and CELT-only, and
`libopus` beats it by an even wider margin (+1.704) — so this says more about that encoder than
about us. **The `libopus` column is the benchmark that matters.**

<details>
<summary>Full per-class table</summary>

| class | ours vs libopus | ours vs ffmpeg-native |
|---|---:|---:|
| applause (stereo) | **+1.107** | +1.018 |
| percussive / transient | **+0.363** | +2.980 |
| noisy speech | **+0.079** | +2.297 |
| wide stereo | −0.073 | +2.690 |
| silence / DTX-shaped | −0.104 | +2.313 |
| clean speech | −0.127 | +1.653 |
| guitar (stereo) | −0.162 | +0.741 |
| guitar (mono) | −0.171 | +0.499 |
| piano (stereo) | −0.217 | +2.478 |
| mixed speech+music | −0.256 | +1.643 |
| voip: noisy speech | −0.021 | +0.930 |
| voip: speech | −0.200 | +0.438 |
| voip: mixed | −0.416 | +0.233 |

Reproduce: `python tools/gen_gate_corpus.py` then `python tools/gate_ladder.py --arms ours,lib,nat`,
and compare with `python tools/gate_regression.py --bd`.

</details>

**Caveats we'd want to read in someone else's README.** PEAQ is a wideband/fullband metric and
saturates on narrowband speech, so the three `voip_*` rows are soft in *both* directions — treat
them as a no-regression tripwire, not a quality ranking. The music sources are short public-domain
clips; a per-class win here is weaker evidence than the same win across a large library.

Streaming robustness — PLC (SILK **and** CELT), FEC, DTX, CNG — is at parity with `libopus`,
conformance untouched.

## Performance — single-thread encode, three coding paths

Measured on an i7-14650HX (Windows, x86-64 AVX2). All three encoders are driven the **same
way**: as processes, encoding a 300 s and a 150 s clip, reporting the **slope** `t(300s) − t(150s)`
so process startup and file I/O cancel out for everyone. Pinned to one core at High priority,
**CPU time** (not wall), arms ABBA-interleaved, 31 reps, with a **null arm** — the same binary
measured twice — establishing the resolution floor.

The path matters more than the sample rate, so the table is organised by which coder actually
ran, **verified from the TOC bytes of the output** rather than assumed:

| path (verified) | **rusty-opus** | C libopus | FFmpeg native | vs libopus |
|---|---:|---:|---:|---|
| **CELT** — 48 kHz speech @32k | **436× realtime** | 291× | 310× | **1.50× faster** |
| **CELT** — 48 kHz stereo music @128k | **213× realtime** | 133× | 71× | **1.60× faster** |
| **SILK** — 16 kHz speech @16k, VoIP | 139× realtime | **145×** | n/a | 0.96× (**libopus 4% ahead**) |

Null-arm floor: 0.0% / 2.2% / 0.0% respectively — the CELT wins are far outside the noise, and
the SILK gap is small but real. Medians were checked at 15 / 31 / 41 reps and settle (ours on
CELT speech read 343.8 ms at all three).

**We're 1.5–1.6× faster than `libopus` on both CELT paths, and within 4% on SILK.** That last
row corrects our own older documentation, which claimed a ~2.9× SILK deficit: with the AVX2
SILK kernels (LPC short-prediction, warped autocorrelation, cross-state NSQ shaping filter) it
is near-parity, and `libopus`'s hand-written NSQ assembly keeps only a few percent.

Two honesty notes. FFmpeg's native encoder is **CELT-only**, so on the SILK row it isn't doing
comparable work — it looks fast because it is coding something cheaper and much worse (see the
quality table). And on top of the single-thread numbers, **frame-parallel encoding**
(`examples/roundtrip_parallel`) wins wall-clock outright, because `libopus` is single-threaded
per stream — each chunk primes its inter-frame state so the seams are PEAQ-neutral (ΔODG ≤ 0.03).

Reproduce with `powershell tools/bench_encode_3way.ps1 -Reps 31`.

## Feature flags

- **`profile`** *(dev-only, off by default)* — a zero-cost-when-off stage profiler for
  optimization work; release builds are byte-identical with it off.

SIMD is always compiled and selected at runtime via CPU-feature detection, with a scalar
fallback path — so the same binary runs on machines with or without AVX2/NEON.

## Part of Remade With Rust

**rusty-opus** is the Opus engine of
**[remade_ffmpeg_rs](https://github.com/Remade-With-Rust/remade_ffmpeg_rs)** — a ground-up,
permissively-licensed Rust rebuild of FFmpeg: the `ffmpeg`/`ffprobe` CLI you already know,
pure-Rust codecs end to end, and no copyleft anywhere in the tree. If you want Opus inside a
full demux → decode → filter → encode → mux pipeline, start there.

Also check out **[FFAI](https://github.com/Remade-With-Rust/FFAI)**, our sister project —
media infrastructure for an AI-first world.

More standalone codec engines from the same family:
[`rusty_h264`](https://crates.io/crates/rusty_h264) (H.264, on crates.io) ·
[`rusty_vp9`](https://crates.io/crates/rusty_vp9) (VP9) · [`rusty_mp3`](https://crates.io/crates/rusty_mp3) (MP3) · [`rusty_aac`](https://crates.io/crates/rusty_aac) (AAC-LC) · [`rusty_vorbis`](https://crates.io/crates/rusty_vorbis) (Vorbis) ·
[rusty-av1-toolkit](https://github.com/Remade-With-Rust/rusty-av1-toolkit) (AV1).

**[Remade With Rust](https://github.com/remade-with-rust)** is an initiative by
[Mata Network](https://www.mata.network) to rebuild essential C and C++ tools in Rust — for the
memory safety, the predictable performance, and the freedom of a permissive license.

## License

BSD-3-Clause — see [COPYING](COPYING). Derived from `opus-rs` and, ultimately, the reference
Opus implementation, both BSD-3-Clause.
