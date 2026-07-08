# rusty-opus — benchmarks

Two comparison profiles, both on an i7-14650HX (24 threads), Windows.
Reproduce with `cargo test --release --test profile_encode encode_throughput --
--ignored --nocapture` (single-thread) and `--test parallel_encode
parallel_correct_and_fast -- --ignored` (parallel). Speeds are ×realtime, median
of best-of-7 for us; libopus is FFmpeg 8.1.2 `-c:a libopus -benchmark`,
slope-corrected (60 s − 30 s) to strip process startup.

Every single-thread gain over the `opus-rs` we forked is **byte-identical**
(same bitstream, just faster). The parallel path is **PEAQ-neutral** (ΔODG ≤ 0.03
vs serial), not byte-identical (VBR chunk seams).

## Profile 1 — vs `opus-rs` upstream (the fork we optimize)

Upstream `restsend/opus-rs` v0.1.23 had x86 AVX2 for pitch/PVQ/comb but **scalar**
for the three SILK kernels we vectorized, and **no parallelism**. Measured with
our AVX2 bricks toggled off (`RUSTY_OPUS_NO_AVX2=1`) = the upstream x86 behavior.

| Mode | upstream | **ours, 1 thread** | **ours, parallel** |
|---|---:|---:|---:|
| SILK — 16k mono speech @24k | 105× | **135× (+29%)** | **878× (~8.4×)** |
| Hybrid — 48k speech @32k | 88× | **114× (+30%)** | ~similar |
| CELT — 48k stereo music @128k | 250× | 262× (+5%) | ~similar |

The +29–30% on speech/Hybrid comes from three byte-identical AVX2 bricks on the
SILK path — **S1c** (LPC short-prediction), **S2** (warped-autocorrelation
correlation), **S1d** (cross-state NSQ shaping filter, i64-lane + persistent
SoA). CELT-only music is unchanged (it doesn't touch the SILK NSQ; its PVQ was
already AVX2 upstream). On speech — Opus's core use case — **our fork is ~8×
faster than the upstream we forked**, the bulk from frame-parallelism it never
had.

## Profile 2 — vs libopus (the reference C library)

| Mode | ours, 1 thread | libopus, 1 thread | 1-thread ratio | ours, parallel |
|---|---:|---:|---|---:|
| CELT — music @128k | 262× | 143× | **1.8× faster** ⚠ | — |
| SILK — speech @24k | 135× | 390× | 2.9× slower | **878×** |
| Hybrid — speech @32k | 114× | 309× | 2.7× slower | — |

⚠ Our CELT is faster single-thread but our VBR is currently effectively CBR and
does less tonality/VBR analysis, so on dense/transient music quality trails
libopus (a separate quality campaign, not a clean win yet).

**Single-thread on speech we're ~2.9× behind** — libopus's NSQ inner loop and
fixed-point macros are hand-written assembly; our kernels are pure-Rust AVX2.
**But the shipped path is frame-parallel**, and libopus is single-threaded per
stream, so end-to-end **`rff -c:a opus` is ~3× faster than `ffmpeg -c:a libopus`
wall-clock** (85 vs 255 ms on 60 s speech) at PEAQ-neutral quality — the
AAC/Vorbis playbook applied to Opus.

## Method notes

- `RUSTY_OPUS_NO_AVX2` / `RUSTY_OPUS_NO_NSQ_AVX2` etc. toggle individual bricks in
  one binary for drift-free A/B — the only reliable way to resolve a SIMD win
  (cross-build comparisons are swamped by thermal noise).
- `RUSTY_OPUS_COMPLEXITY` sweeps the SILK `n_states` knob: complexity 5 (2 states)
  is +78% for ≤0.03 ODG — a near-free speed lever exposed as `-compression_level`.
