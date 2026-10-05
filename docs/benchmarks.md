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


## Quality — methodology and per-class results

Three encoders, one corpus, one metric: **18 content classes × 5 bitrates**, scored with an
external **PEAQ ODG** oracle and compared as **BD-ODG at matched *actual* bitrate**. Matching
actual rather than nominal bitrate matters: `libopus` VBR overshoots its target by 15–20% on
this corpus, and comparing at the nominal rate would credit it with those extra bits.

| | vs C `libopus` | vs FFmpeg's native Opus encoder |
|---|---:|---:|
| mean BD-ODG, 13 core classes | −0.015 (parity) | +1.532 |
| mean BD-ODG, 5 music-stress classes | +0.009 (parity) | +2.002 |
| worst class | −0.416 | +0.233 |
| classes won vs FFmpeg native | — | 18 / 18 |

### Music-stress classes (to 256 kb/s)

| class | what it stresses | vs libopus | vs ffmpeg-native |
|---|---|---:|---:|
| bass-heavy electronic (bass fraction 0.634) | sub-bass allocation, low CELT bands | +0.203 | +3.294 |
| fast/dense, 40 hits/s | block switching, transient density | +0.014 | +2.659 |
| distorted rock, decorrelated stereo | dense harmonics to Nyquist | +0.002 | +1.144 |
| loud master (8.0 dB crest) | rate control at near-full-scale | −0.031 | +1.319 |
| vocal (public-domain Mozart aria) | formants, strong harmonics | −0.143 | +1.593 |

Four of the five are synthetic (labelled in the corpus README): appropriate for stressing a
mechanism, not a substitute for commercial masters.

### Core classes

| class | vs libopus | vs ffmpeg-native |
|---|---:|---:|
| applause (stereo) | +1.107 | +1.018 |
| percussive / transient | +0.363 | +2.980 |
| noisy speech | +0.079 | +2.297 |
| wide stereo | −0.073 | +2.690 |
| silence / DTX-shaped | −0.104 | +2.313 |
| clean speech | −0.127 | +1.653 |
| guitar (stereo) | −0.162 | +0.741 |
| guitar (mono) | −0.171 | +0.499 |
| piano (stereo) | −0.217 | +2.478 |
| mixed speech + music | −0.256 | +1.643 |
| voip: noisy speech | −0.021 | +0.930 |
| voip: speech | −0.200 | +0.438 |
| voip: mixed | −0.416 | +0.233 |

**Limitations.** PEAQ is a wideband/fullband metric and saturates on narrowband speech, so the
`voip_*` rows are a no-regression tripwire rather than a ranking. The music sources are short
public-domain clips; a per-class result here is weaker evidence than the same result across a
large library. FFmpeg's native encoder is experimental and CELT-only; `libopus` is the
reference that matters.

Reproduce: `python tools/gen_gate_corpus.py`, then `python tools/gate_ladder.py --arms ours,lib,nat`
and `python tools/gate_regression.py --bd`.

## Performance — methodology

Single-thread encode, i7-14650HX (Windows, AVX2). All encoders are run as processes on a
300 s and a 150 s clip and reported as the slope `t(300 s) − t(150 s)`, so process start-up and
file I/O cancel. Pinned to one core at high priority, CPU time, arms interleaved (ABBA), 31
repetitions, with a null arm (the same binary measured twice) establishing the resolution
floor: 0.0% / 2.2% / 0.0% on the three paths. The coding path is verified from the TOC bytes
of each output. Benchmarks build with the `bench` profile (release code generation without the
overflow checks the repository's test builds keep). Reproduce:
`powershell tools/bench_encode_3way.ps1 -Reps 31`.
