# rusty-opus — the encoder optimization plan (bricks to a house)

*Campaign started 2026-07-08. Method: `codec-analyzer` → profile first, one brick
per commit, every brick gated (byte-identical for speed bricks, PEAQ-neutral on a
corpus for anything that moves the bitstream), revert-if-flat. This file is the
running ledger — update the status column as bricks land.*

## Where we start (measured, not guessed)

Single-thread, i7-14650HX, 60 s deterministic synthetic clips
(`tests/profile_encode.rs`), best-of-7. libopus = FFmpeg 8.1.2 `-c:a libopus`,
`-benchmark`, startup-slope-corrected (30 s vs 60 s runs).

| Scenario | rusty-opus | libopus (C) | gap |
|---|---:|---:|---|
| CELT-only — 48 kHz stereo music @128k (Audio) | **308× RT** | ~137× RT | 2.2× faster **but lower quality — NOT a clean win** (see F4) |
| SILK-only — 16 kHz mono speech @24k (VoIP) | **128× RT** | ~395× RT | **~3.1× slower** |
| Hybrid — 48 kHz mono speech @32k (VoIP) | **111× RT** | ~303× RT | **~2.7× slower** |

**F4 verdict (2026-07-08, resolved the ⚠):** the CELT speed is partly bought with
quality. PEAQ ODG at 128k on real CC0 music (ours enc→dec vs libopus enc→dec):

| clip | rusty-opus | libopus | our actual rate |
|---|---:|---:|---|
| synthetic tonal music | −0.14 | −0.20 | 128 kbps (pinned) |
| real piano | −0.47 | −0.05 | 128 kbps (pinned) |
| real guitar (dense/transient) | **−2.31** | −0.36 | 128 kbps (pinned) |

Two root causes: (1) **our VBR is effectively CBR** — it emits *exactly* the
target rate on every clip, while libopus VBR spends 155 kbps on the hard guitar
(+21%); (2) even rate-adjusted, CELT bit-allocation on dense/transient content is
weaker. This is a **quality** gap (codec-tune-quality / codec-experimental), not a
speed gap — so **the CELT wing is re-scoped**: honest labeling now, speed bricks
still welcome (byte-identical), but the real CELT prize is a quality sub-campaign
(true VBR + allocation), tracked separately from the speed house.

**The campaign's battlefield is SILK** — an unambiguous 3.1× *pure-speed* gap on
Opus's home turf (speech/VoIP), gated byte-identical. That is the flagship.

## Where the time goes (stage profiler, median of 15 passes)

**SILK-only (16 kHz speech @24k)** — 84 ms / 10 s clip:

| stage | % | ns/call | class |
|---|---:|---:|---|
| silk-nsq (noise-shaping quantizer, del-dec) | **58.6%** | 87,573 | compute, serial-per-sample, parallel-across-states |
| silk-noise-shape (shaping analysis) | **18.7%** | 31,460 | compute (autocorr/Levinson/warped LPC) |
| silk-pred-coefs (LPC/LTP/NLSF VQ) | **14.5%** | 24,369 | compute + search |
| silk-pitch (open-loop pitch) | 4.7% | 7,849 | compute |
| silk-range-code / resample / vad | 3.2% | — | lean |
| mgmt/other (residue) | **0.3%** | — | fully decomposed ✓ |

**CELT-only (48 kHz stereo music @128k)** — 34.5 ms / 10 s clip:

| stage | % | ns/call | class |
|---|---:|---:|---|
| celt-pvq (`quant_all_bands`) | **56.0%** | 38,581 | search+encode; AVX2 already present |
| celt-transient | 9.3% | 6,434 | compute; partial AVX |
| celt-mdct (kiss_fft forward) | 9.1% | 3,142 | compute |
| celt-synth (encoder-side resynthesis) | 9.1% | 6,272 | feeds next frame's prefilter → bitstream-coupled |
| celt-tf / alloc / coarse-q / preemph / bands / fine-q | 11.2% | — | lean |
| mgmt/other (residue) | 5.3% | — | ≈ timer overhead at these call counts |

**Hybrid (48 kHz speech @32k)** = SILK profile (72%) + celt-mdct 10.7%
(**3,335 calls/500 frames ≈ 6.7 MDCTs/frame** — short-block anomaly, see H1) +
resample 4%.

NSQ call count is 562 per 500 frames: the CBR/VBR **rate loop re-runs NSQ ~12%**
of frames (up to 6 gain iterations) — a redundancy lever independent of the
kernel itself (S5).

## The gates (nothing lands without them)

- **Byte-identical gate** — the encoder is deterministic; a speed brick must
  reproduce the exact packet bytes of the scalar/pre-brick encoder on the full
  test matrix (3 scenarios × existing test suite). The scalar path stays in-tree
  forever as the oracle (`--no-default-features`-style; runtime-dispatched SIMD
  with scalar twin).
- **PEAQ gate** — any brick that legitimately moves the bitstream (algorithmic
  change, float reassociation, complexity remap) is gated perceptually instead:
  ΔODG ≤ 0.03 vs pre-brick on a speech+music corpus (reuse
  `remade_ffmpeg_rs/tools/quality/` PEAQ harness), plus ffmpeg-decodes-at-unity.
- **A/B protocol** — interleaved best-of-7 via the deterministic benchmark;
  revert anything within noise (~4%).

---

## The house

### Foundation — instruments (lay first, everything rests on it)

| brick | what | status |
|---|---|---|
| **F1** | Stage profiler `src/prof.rs` (rdtsc, feature-gated, 19 stages, calibrated snapshot) | ✅ 2026-07-08 |
| **F2** | Deterministic benchmark `tests/profile_encode.rs` (3 scenarios, best-of-N ×RT + median breakdown) | ✅ 2026-07-08 |
| **F3** | C-reference baseline (ffmpeg libopus, slope-corrected) — table above | ✅ 2026-07-08 |
| **F4** | **Quality oracle + byte-identity gate**: `tests/oracle_bitexact.rs` (FNV hash of the full packet stream per scenario — the workhorse gate, frozen 2026-07-08); `examples/roundtrip.rs` + `tools/quality_ab.sh` (PEAQ ODG ours vs libopus). Settled the CELT ⚠ (see verdict above) and armed the PEAQ gate. | ✅ 2026-07-08 |
| **F5** | Cheap probes: bounds-check-ceiling (`get_unchecked` newtype throwaway) + alloc-traffic audit (per-frame `vec!`/`resize` in hot paths — NSQ allocates scratch per call?). Opus frames are tiny (working set ≪ L2) so the cache sweep is expected flat — run once to confirm, then never tile. | ☐ next |

### SILK wing — the 3.1× gap (order = profiler ranking)

| brick | what | expected | status |
|---|---|---|---|
| **S1a** | **NSQ redundancy pass** (`nsq_del_dec.rs`, 930+803 lines): hoist invariants out of the per-sample loop, precompute shaping/LPC coefficient reuse, kill per-call allocations, branchless clamps. `codec-eliminate-redundancy` — biggest wins at lowest risk, byte-identical. | 1.2–1.5× on 59% | ☐ |
| **S1b** | **NSQ inner-loop decomposition**: info-tier scopes inside the sample loop (short-term pred, LTP, shaping filter, del-dec Viterbi update) to rank sub-kernels before SIMD. | data | ☐ |
| **S1c** | **NSQ AVX2**: vectorize across the 4 del-dec states (libopus 1.5's own AVX2 NSQ proves the shape); port/extend the existing NEON path to x86. Scalar twin as oracle, runtime dispatch. `codec-vectorize-kernel`. | 1.5–2.5× on the kernel | ☐ |
| **S2** | **noise_shape_analysis (18.7%)**: autocorrelation, Levinson/Schur, warped-LPC loops — redundancy then AVX2 (fixed-point dot products vectorize cleanly). | ~1.15× overall | ☐ |
| **S3** | **find_pred_coefs (14.5%)**: LTP correlation search + `nlsf_del_dec_quant` VQ — same two-step treatment; the NLSF VQ search may admit an energy-shortlist like the Vorbis classifier (that variant is PEAQ-gated). | ~1.1× overall | ☐ |
| **S4** | **pitch analysis (4.7%)**: correlation kernels share machinery with S3. | small | ☐ |
| **S5** | **Rate-loop rerun cut (~12% of NSQ)**: seed the gain iteration from the previous frame's converged gain / predict bits before re-running NSQ. If prediction only reorders iterations → byte-identical; if it changes final gains → PEAQ gate. | up to ~7% SILK | ☐ |

Amdahl check: S1 (4× on 58.6%) alone ≈ 1.8× SILK overall; S1+S2+S3 at plausible
factors ≈ 2.3–2.6×. Closing the full 3.1× single-thread likely also needs S5 +
resample/glue trims — set expectations per-brick, measure, and let the profiler
rewrite the ranking after each brick lands.

### CELT wing — extend a lead we must first prove is real

| brick | what | expected | status |
|---|---|---|---|
| **C1** | **PVQ decomposition (56%)**: info-tier scopes inside `quant_all_bands` — split search (`op_pvq_search`) vs CWRS index encode vs resynth vs band glue; AVX2 exists for search+resynth, so find what's still scalar. Then redundancy → deeper SIMD on the actual hot sub-kernel. | data → 1.2×+ | ☐ |
| **C2** | **transient_analysis (9.3%)**: 6.4 µs/call is fat for a classifier; the inverse-filter + L1-metric loops beyond the existing `sum_abs` AVX are scalar. | ~5% CELT | ☐ |
| **C3** | **MDCT (9.1%)**: kiss_fft port — first eliminate per-call twiddle/scratch recompute; a swap to our proven N/4-FFT MDCT (AAC/Vorbis campaigns) changes float rounding → bitstream moves → PEAQ gate. Try redundancy-only first (byte-identical). | ~5% CELT | ☐ |
| **C4** | **encoder resynthesis (9.1%)**: needed only to prime next frame's prefilter/comb-filter memory. Investigate: when the prefilter is inactive (low complexity or pitch gain 0), can synth be skipped bit-exactly? If not, it rides C3's MDCT-backward win. | 0–9% CELT | ☐ |
| **C5** | Complexity-knob audit: map our 0–10 to libopus's semantics (ours defaults 9 vs libopus 10) so published A/Bs are apples-to-apples. | honesty | ☐ |

### Hybrid mortar

| brick | what | status |
|---|---|---|
| **H1** | Explain the **6.7 forward-MDCTs/frame** in Hybrid (expected ≤ 2/channel; short-block transients on synthetic speech?). If transient over-triggering is the cause it also costs bits — profile + fix or document. | ☐ |
| **H2** | Resample/hp 4%: the 48→16 kHz down-1/3 FIR — redundancy pass, maybe SIMD with S4's machinery. | ☐ |

### Roof — structural (after the walls stand)

| brick | what | status |
|---|---|---|
| **R1** | **Frame-parallel encoding** (the AAC/Vorbis headline move): Opus carries real inter-frame state (preemph/synth memory, SILK LTP + entropy context across frames in a stream), so exact frame-parallel is not free like AAC. Options: chunked parallelism with overlap-primed encoders (bitstream changes → PEAQ gate, opt-in flag), or per-stream parallelism in `rff` when transcoding multiple streams. Prototype only after single-thread parity. | ☐ |
| **R2** | **rff integration bench**: `rff -i speech.wav out.opus` vs `ffmpeg -c:a libopus` wall-clock; expose bitrate/complexity knobs in `rff-codec-opus` (currently hardcoded 64 kbps / complexity 9). | ☐ |
| **R3** | Publish: README benchmark table (reproducible), upstream-PR-able bricks offered back to `restsend/opus-rs`, `docs/benchmarks.md` entry in remade_ffmpeg_rs. | ☐ |

## Decoder (second campaign, same instruments)

The decoder gets its own profile pass after the encoder walls are up — same
prof.rs stages work for decode (add decode-side scopes then). Decode is also the
correctness oracle for encoder bricks meanwhile.

## Learnings ledger (append as bricks land)

- *2026-07-08 (F1–F3)*: Residue is tiny (0.3–5.3%) — this codebase has no ghost;
  the fat is in named kernels. NSQ dominates SILK exactly as libopus's own AVX2
  history predicts. Upstream already SIMD'd PVQ search/resynth + comb filter +
  some NEON in NSQ — the x86 NSQ hole is the single highest-value target in the
  whole codec.
