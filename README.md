# rusty-opus

[![Remade With Rust](https://img.shields.io/badge/Remade%20With-Rust-000?logo=rust&logoColor=fff)](https://github.com/remade-with-rust)
[![By Mata Network](https://img.shields.io/badge/by-Mata%20Network-5b2be0)](https://www.mata.network)
[![License: BSD-3-Clause](https://img.shields.io/badge/license-BSD--3--Clause-blue)](COPYING)
[![Pure Rust](https://img.shields.io/badge/pure-Rust%20%C2%B7%20no%20C%20%C2%B7%20no%20FFI-orange?logo=rust&logoColor=fff)](#)

**A pure-Rust implementation of the [Opus audio codec](https://opus-codec.org/) (RFC 6716 / RFC 8251)** —
no C, no FFI, no build-time toolchain. Encoder *and* decoder, SILK + CELT + Hybrid,
conformance-verified against the reference, and fast enough to **beat `libopus` on wall-clock**.
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
  encode at/above `libopus` on CELT, and **frame-parallel encoding** wins wall-clock outright
  (~3× `libopus` on speech, which is single-threaded per stream).
- **Complete** — SILK, CELT, and Hybrid modes; VBR/CBR; DTX; in-band FEC; packet-loss
  concealment; comfort-noise generation; multistream / surround (5.1 / 7.1); a repacketizer.
- **Permissive** — BSD-3-Clause, all the way down.

## Add it to your project

```toml
[dependencies]
rusty-opus = "0.1"
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

## Quality vs `libopus`

Measured head-to-head on **PEAQ ODG** with a reconstruction-SNR guard. **Mono** (speech and
music) sits at **parity** once bitrate-matched. **Stereo music** is at or near parity at higher
rates and a modest, honest deficit at mid rates (an open item we're still closing — the decoder
is exact, this is purely encoder bit-allocation tuning). Streaming robustness — PLC (SILK **and**
CELT), FEC, DTX, CNG — is at parity with `libopus`, conformance untouched.

## Performance

Single-thread encode, Criterion (`cargo bench`), real speech input, mono, wall-clock for the
full frame set. rusty-opus adds **byte-identical AVX2 SILK kernels** (LPC short-prediction,
warped-autocorrelation, cross-state NSQ shaping filter) plus AVX2 decode kernels (resampler FIR,
CELT comb filter) on top of the pure-Rust base.

### vs C libopus 1.6.1 — x86-64 (AVX2/FMA), AMD Ryzen 7 5700X

| Config | rusty-opus | C libopus | Ratio |
|--------|-----------:|----------:|-------|
| 8 kHz / 20 ms VoIP  | **39.9 ms** | 40.6 ms | 0.98× (**2 % faster**) |
| 16 kHz / 20 ms VoIP | **66.8 ms** | 67.1 ms | 1.00× (**0.5 % faster**) |
| 16 kHz / 10 ms VoIP | 73.2 ms | **72.5 ms** | 1.01× (within noise) |
| 48 kHz / 20 ms Audio | **25.1 ms** | 28.4 ms | 0.88× (**12 % faster**) |
| 48 kHz / 10 ms Audio | **29.7 ms** | 31.2 ms | 0.95× (**5 % faster**) |

### vs C libopus 1.6.1 — Apple Silicon (aarch64, NEON)

| Config | rusty-opus | C libopus | Ratio |
|--------|-----------:|----------:|-------|
| 8 kHz / 20 ms VoIP  | 31.47 ms | **31.20 ms** | 1.01× (C 0.9 % faster) |
| 16 kHz / 20 ms VoIP | **51.19 ms** | 52.81 ms | 0.97× (**3.1 % faster**) |
| 16 kHz / 10 ms VoIP | 55.69 ms | **55.49 ms** | 1.00× (within noise) |
| 48 kHz / 20 ms Audio | **13.97 ms** | 19.39 ms | 0.72× (**28 % faster**) |
| 48 kHz / 10 ms Audio | **16.19 ms** | 20.28 ms | 0.80× (**20 % faster**) |

Where `libopus`'s hand-written assembly still leads single-thread (the SILK NSQ inner loop),
**frame-parallel encoding** (`examples/roundtrip_parallel`) wins the wall-clock race outright —
each chunk primes its inter-frame state so the seams are PEAQ-neutral, and `libopus` is
single-threaded per stream.

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
[`rusty_vp9`](https://crates.io/crates/rusty_vp9) (VP9) · [`rusty_mp3`](https://crates.io/crates/rusty_mp3) (MP3) · [`rusty_aac`](https://crates.io/crates/rusty_aac) (AAC-LC) ·
[rusty-av1-toolkit](https://github.com/Remade-With-Rust/rusty-av1-toolkit) (AV1).

**[Remade With Rust](https://github.com/remade-with-rust)** is an initiative by
[Mata Network](https://www.mata.network) to rebuild essential C and C++ tools in Rust — for the
memory safety, the predictable performance, and the freedom of a permissive license.

## License

BSD-3-Clause — see [COPYING](COPYING). Derived from `opus-rs` and, ultimately, the reference
Opus implementation, both BSD-3-Clause.
