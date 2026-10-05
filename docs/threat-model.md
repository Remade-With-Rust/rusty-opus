# rusty-opus threat model

**Last revised:** 2026-10-04 (v1.0.0 hardening pass) · **Next review:** 2027-01-04, or
after any change to packet parsing, the public API, `unsafe` code, or dependencies.

## 1. What rusty-opus is

A pure-Rust implementation of the Opus audio codec (RFC 6716): an encoder, a decoder,
packet utilities (repacketizer, padding), multistream (surround) wrappers, and a
frame-parallel encoder. It is a library with **no runtime dependencies**, performs no
I/O, opens no files or sockets, spawns no processes (the opt-in `parallel` module uses
scoped threads only), and reads exactly one environment variable, `RUSTY_OPUS_ISA`, which
can only lower the SIMD level. Development toggles that read the environment or write a
tuning log exist only in builds with the non-default `research` feature;
`tools/check_invariants.py` enforces this in CI.

## 2. Assets

| Asset | Why it matters |
|---|---|
| **Memory safety of the host process** | rusty-opus runs inside VoIP clients, media servers, browsers (wasm) and tools that also hold credentials, keys and user data. A memory-safety bug in the codec is a foothold into all of it. |
| **Availability of the host** | A panic, unbounded allocation or infinite loop on crafted input is a remote denial of service against every peer that decodes attacker packets. |
| **Integrity of decoded audio** | Callers rely on the output being a faithful decode; silent corruption can mask attacks (e.g. desynchronised streams) or degrade service. |
| **Supply chain of the published crate** | A compromised release or dependency would execute in every downstream build. |

The codec processes no secrets and no personal data beyond the audio itself, which it
does not retain beyond its working buffers.

## 3. Adversaries

| Adversary | Capability |
|---|---|
| **A1 — Remote packet sender** (primary) | Fully controls the bytes, sizes, ordering and loss pattern of packets handed to `OpusDecoder::decode` / `decode_fec`, `repacketizer::*`, and the multistream decoder. The most realistic attacker: any peer in a call, any uploaded file. |
| **A2 — Malicious or malformed PCM source** | Controls the sample values (NaN, ±Inf, denormals, full-scale) passed to the encoder. Lower risk: usually local, but can be remote in transcoding services. |
| **A3 — Misconfiguring caller** | Passes out-of-range sample rates, channel counts, frame sizes, bitrates. Not hostile, but must not trigger UB. |
| **A4 — Supply-chain attacker** | Attempts to ship malicious code via a dependency, the build, or the release process. |

Out of scope: an attacker who can already execute code in the host process, side
channels on audio content (the codec handles no secrets), and physical attacks.

## 4. Trust boundaries and entry points

```
untrusted bytes ──► OpusDecoder::decode / decode_fec ──► packet framing (RFC 6716 §3)
                                                    ├──► range decoder ──► SILK / CELT / hybrid
                                                    └──► PLC (empty input = loss)
untrusted bytes ──► repacketizer::{parse_packet, cat, pad_packet, unpad_packet, nb_frames}
untrusted bytes ──► MultistreamDecoder::decode
caller PCM ───────► OpusEncoder::encode, MultistreamEncoder::encode, parallel::*
```

Every byte crossing the left edge is hostile. Configuration values (rates, channels,
frame sizes) are caller-controlled and validated at construction or per call.

## 5. STRIDE analysis

| Threat | Applies? | Analysis and mitigation |
|---|---|---|
| **S**poofing | No | No identities or authentication in a codec. Stream authenticity is the transport's job (SRTP, DTLS). |
| **T**ampering | Yes (A1) | A crafted packet steering the decoder into out-of-bounds reads/writes. **Mitigations:** packet framing validates every length before slicing and rejects with `Error::InvalidPacket`; the range decoder bounds-checks its input buffer; `unsafe` is confined to SIMD kernels with documented, audited contracts ([UNSAFE.md](../UNSAFE.md)); fuzzing (`fuzz/`) and differential testing against libopus. |
| **R**epudiation | No | No actions to repudiate. |
| **I**nformation disclosure | Low (A1) | An out-of-bounds read could copy host memory into decoded audio returned to the attacker. Same mitigations as Tampering; all scratch buffers are initialised before use (Miri-checked). |
| **D**enial of service | Yes (A1, A2) | Panics, unbounded work, or allocation on crafted input. **Mitigations:** typed errors instead of panics on the decode path; packets are capped at 120 ms and 48 frames (RFC limits) before any work; the decoder performs **zero heap allocations** after warm-up, whatever the input (the encoder allocates only on a SILK-to-CELT switch); integer overflow checks are enabled in release builds of the test suite and fuzzers so arithmetic bugs fail loudly; NaN/Inf PCM is tolerated by the encoder. |
| **E**levation of privilege | Indirect | Only via a memory-safety bug (see Tampering). Mitigated by the same controls. |

## 6. Highest-value attack path

A remote peer (A1) sends a crafted packet that drives a SIMD kernel's raw-pointer loop
past the end of a buffer. Defences in depth along that path: framing validation →
range-decoder bounds → stage-level length checks before each kernel call → per-kernel
`# Safety` contracts → scalar-vs-SIMD oracle tests on every architecture → fuzzing with
sanitizers.

## 7. Residual risks

Tracked with owners and review dates in the hardening plan's risk register:
[docs/plans/use-protection-please.md](plans/use-protection-please.md#residual-risk-register).
