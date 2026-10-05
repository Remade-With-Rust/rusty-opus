# rusty-opus

[![crates.io](https://img.shields.io/crates/v/rusty-opus.svg)](https://crates.io/crates/rusty-opus)
[![docs.rs](https://img.shields.io/docsrs/rusty-opus)](https://docs.rs/rusty-opus)
[![CI](https://github.com/Remade-With-Rust/rusty-opus/actions/workflows/ci.yml/badge.svg)](https://github.com/Remade-With-Rust/rusty-opus/actions/workflows/ci.yml)
[![License: BSD-3-Clause](https://img.shields.io/badge/license-BSD--3--Clause-blue)](COPYING)
[![MSRV 1.85](https://img.shields.io/badge/MSRV-1.85-informational)](#platform-support)

**A complete, pure-Rust implementation of the [Opus audio codec](https://opus-codec.org/)
(RFC 6716 / RFC 8251).** Encoder and decoder, SILK, CELT and Hybrid modes — with no C
code, no FFI and no build-time toolchain. Conformance-verified against the reference
implementation, at `libopus` quality and up to 1.6× its single-thread encode speed.

## Highlights

- **Conformant.** Bit-exact on all 12 official RFC 6716 / RFC 8251 decoder test vectors;
  interoperates with `libopus` in both directions across every sample rate, channel layout,
  application, frame size and bandwidth (3,240-configuration matrix).
- **Fast.** Runtime-dispatched AVX2/FMA, AVX and SSE2 kernels on x86, NEON on aarch64, each
  with a bit-checked scalar fallback. Frame-parallel encoding is available on top.
- **Safe by construction.** Packet parsing, the range decoder and every other stage that
  touches untrusted bytes are entirely safe Rust. `unsafe` is confined to documented SIMD
  kernels, and every kernel's contract is enforced at its safe entry point.
- **Allocation-free streaming.** After warm-up, `OpusDecoder::decode` performs zero heap
  allocations — including packet-loss concealment, multi-frame packets and mode
  transitions — and `OpusEncoder::encode` allocates only on a switch from SILK to CELT
  coding. Suitable for real-time audio threads.
- **Complete feature set.** VBR/CVBR/CBR, DTX, in-band FEC, packet-loss concealment,
  comfort noise, multistream (stereo through 7.1), and a repacketizer.
- **Zero dependencies.** The published crate depends on nothing but `std`.

## Installation

```toml
[dependencies]
rusty-opus = "1"
```

## Usage

```rust
use rusty_opus::{Application, Error, OpusDecoder, OpusEncoder};

fn main() -> Result<(), Error> {
    const RATE: i32 = 48_000;
    const FRAME: usize = 960; // 20 ms at 48 kHz

    // Encode one stereo frame.
    let mut encoder = OpusEncoder::new(RATE, 2, Application::Audio)?;
    encoder.bitrate_bps = 96_000;
    let pcm = vec![0.0f32; FRAME * 2]; // interleaved, nominal range [-1, 1]
    let mut packet = [0u8; 1500];
    let len = encoder.encode(&pcm, FRAME, &mut packet)?;

    // Decode it. An empty packet means "lost": the decoder conceals it.
    let mut decoder = OpusDecoder::new(RATE, 2)?;
    let mut out = vec![0.0f32; FRAME * 2];
    let samples = decoder.decode(&packet[..len], FRAME, &mut out)?;
    assert_eq!(samples, FRAME);
    Ok(())
}
```

Errors are reported as [`rusty_opus::Error`](https://docs.rs/rusty-opus/latest/rusty_opus/enum.Error.html),
whose variants mirror the `libopus` error codes (`BadArg`, `BufferTooSmall`,
`InvalidPacket`, `Internal`). A malformed packet is always an `Err`, never a panic.

| API | Purpose |
|---|---|
| `OpusEncoder` / `OpusDecoder` | Mono and stereo encode / decode, PLC, FEC |
| `multistream::{OpusMSEncoder, OpusMSDecoder}` | Surround (mapping families 0 and 1) |
| `repacketizer` | Merge, split, pad and unpad packets without re-encoding |
| `parallel` | Frame-parallel and batch encoding across threads |

Runnable programs are in [`examples/`](examples/): WAV round-trip, packet-loss
concealment, in-band FEC, multistream, frame-parallel encoding, and `opus_demo`-compatible
bitstream tools.

## Quality

Measured against `libopus` with an external PEAQ (ODG) oracle over 18 content classes at
five bitrates each, compared at matched *actual* bitrate (BD-ODG):

| | vs `libopus` |
|---|---:|
| Mean, 13 core classes (speech, music, mixed, stereo, VoIP) | −0.015 — parity |
| Mean, 5 stress classes (sub-bass, dense transients, loud masters, distortion, vocal) | +0.009 — parity |
| Strongest class (applause) | +1.107 |
| Weakest class (low-rate VoIP mixed content) | −0.416 |

Packet-loss concealment, DTX and comfort noise are at parity with `libopus`, and in-band
FEC streams are verified against `libopus` in 480 configurations. Corpus, method and
per-class results: [docs/benchmarks.md](docs/benchmarks.md).

## Performance

Single-thread encode on an Intel i7-14650HX (AVX2), CPU time, pinned, interleaved runs,
coding path verified from the output TOC bytes:

| Coding path | rusty-opus | `libopus` | |
|---|---:|---:|---|
| CELT — 48 kHz speech, 32 kb/s | 436× realtime | 291× | 1.50× faster |
| CELT — 48 kHz stereo music, 128 kb/s | 213× realtime | 133× | 1.60× faster |
| SILK — 16 kHz VoIP speech, 16 kb/s | 139× realtime | 145× | within 4% |

Frame-parallel encoding (`parallel`) scales further, since `libopus` encodes each stream
on a single thread. Methodology and reproduction: [docs/benchmarks.md](docs/benchmarks.md).

## Platform support

| Target | SIMD | Validation |
|---|---|---|
| x86_64 (Linux, Windows, macOS) | AVX2/FMA, AVX, SSE2, runtime-selected | Full test suite, libopus conformance matrix |
| i686 | AVX, SSE2 where available | Test suite |
| aarch64 (Linux, macOS, Android) | NEON | Full test suite at NEON and scalar level; conformance matrix |
| wasm32 (`unknown-unknown`, `wasip1`) | Scalar | Test suite under wasmtime; conformance matrix; bit-identical to the scalar x86_64 build with the same libm |

SIMD is selected at runtime, so one binary runs on any CPU of its architecture. Set
`RUSTY_OPUS_ISA=scalar` (or `sse2`, `avx`, `avx2`) to cap the instruction set, for example
to reproduce a result on older hardware. The minimum supported Rust version is **1.85**.

## Security

rusty-opus is built to decode packets from untrusted peers.

- **Reporting:** see [SECURITY.md](SECURITY.md) for private disclosure and response times.
- **Threat model:** [docs/threat-model.md](docs/threat-model.md).
- **Unsafe code:** every `unsafe` item is listed with its justification in
  [UNSAFE.md](UNSAFE.md); the list is regenerated and checked in CI.
- **Assurance:** fuzzing of every packet-facing entry point, property tests, Miri,
  differential testing against `libopus`, `cargo-deny`/`cargo-audit`/`cargo-vet` on every
  change. The full hardening status is the table at the end of this page.

## Stability

The public API — the items documented on [docs.rs](https://docs.rs/rusty-opus) — follows
semantic versioning. Internal codec stages are reachable for testing but hidden from the
documentation and exempt from the stability guarantee. Changes are recorded in
[CHANGELOG.md](CHANGELOG.md).

## Cargo features

| Feature | Default | Description |
|---|---|---|
| `profile` | off | Per-stage profiler for optimization work. Zero cost and output-neutral when off. |
| `research` | off | Development toggles read from the environment, used by the tuning harnesses in `tools/`. Not for production: without it, the library never reads the environment except `RUSTY_OPUS_ISA`. |

## Related projects

rusty-opus is the Opus engine of
[remade_ffmpeg_rs](https://github.com/Remade-With-Rust/remade_ffmpeg_rs), a permissively
licensed Rust rebuild of FFmpeg. Sibling codec crates:
[`rusty_h264`](https://crates.io/crates/rusty_h264) ·
[`rusty_vp9`](https://crates.io/crates/rusty_vp9) ·
[`rusty_mp3`](https://crates.io/crates/rusty_mp3) ·
[`rusty_aac`](https://crates.io/crates/rusty_aac) ·
[`rusty_vorbis`](https://crates.io/crates/rusty_vorbis).
Part of [Remade With Rust](https://github.com/remade-with-rust), an initiative by
[Mata Network](https://www.mata.network).

## License

BSD-3-Clause — see [COPYING](COPYING). Derived from
[`opus-rs`](https://github.com/restsend/opus-rs) and, ultimately, the reference Opus
implementation, both BSD-3-Clause.

---

<!-- HARDENING-TABLE:BEGIN generated by use-protection-please — edit docs/plans/use-protection-please.md, not this block -->
## Hardening status

**Tier** critical-path · **Audited** 2026-10-04 (deep) · **v1.0.0 gates** 13/15 · [Full checklist](https://github.com/Remade-With-Rust/rusty-opus/blob/v1.0.0/docs/plans/use-protection-please.md)

`█████████████████░░░` **88%** &nbsp;·&nbsp; 30 Completed · 0 Scheduled · 4 Incomplete · 21 N/A

| Phase | ✅ Completed | 🗓 Scheduled | ⬜ Incomplete | · N/A |
|---|--:|--:|--:|--:|
| 0 — Threat modeling | 2 | 0 | 0 | 0 |
| 1 — Toolchain | 4 | 0 | 0 | 0 |
| 2 — Supply chain | 7 | 0 | 1 | 0 |
| 3 — Code level | 6 | 0 | 0 | 1 |
| 4 — Static analysis | 1 | 0 | 0 | 0 |
| 5 — Dynamic analysis | 3 | 0 | 0 | 0 |
| 6 — Fuzzing and properties | 3 | 0 | 1 | 0 |
| 7 — Formal verification | 0 | 0 | 1 | 0 |
| 8 — Build and binary | 0 | 0 | 0 | 2 |
| 9 — Runtime privilege | 0 | 0 | 0 | 1 |
| 10 — Cryptography | 0 | 0 | 0 | 3 |
| 11 — CI/CD, release, and operations | 4 | 0 | 1 | 0 |
| 12 — Compliance controls | 0 | 0 | 0 | 14 |
| **Total** | **30** | **0** | **4** | **21** |

**Architect** — [@Ttimmahlax](https://github.com/Ttimmahlax)
<!-- HARDENING-TABLE:END -->
