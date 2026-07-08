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
| SILK-only — 16 kHz mono speech @24k (VoIP) | **128× RT** (→ **~134× after S1c+S2**) | ~395× RT | ~3.1× → **~2.9× slower** |
| Hybrid — 48 kHz mono speech @32k (VoIP) | **111× RT** (→ **~117× after S1c+S2**) | ~303× RT | ~2.7× → **~2.6× slower** |

> **SILK progress (byte-identical bricks):** all-scalar baseline ~120× → **~134× (+12%)**
> via S1c (AVX2 LPC prediction, +7–9%) + S2 (AVX2 warped-autocorr correlation, +9%).
> The gap to libopus is closing kernel by kernel; the big remaining lever is S1d
> (cross-state NSQ AVX2, ~35% of SILK).

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

## Status — three AVX2 bricks landed, SILK ~+31% over scalar (2026-07-08)

**S1c** (LPC prediction) + **S2** (warped autocorr) + **S1d/Path 2** (cross-state NSQ
shaping filter, i64-lane + persistent SoA) — all **byte-identical**, together
**~+31% SILK** over the all-scalar baseline (~120→~157× RT), closing the libopus
gap from ~3.1× toward ~2.5×. Plus **Path 1**: the complexity/`n_states` knob is a
near-free speed lever (4→2 states = +78%, ≤0.03 ODG). The key S1d lesson: *the
cross-state SIMD idea was right; my first implementation was wrong* — an isolated
micro-benchmark found the two fixes (i64 lanes not per-op-narrow; persistent SoA
not per-sample transpose). **Next: R1 frame-parallel** for the wall-clock win.

## (historical) Status — the clean-vectorization seam is mined out (2026-07-08)

Two fat data-parallel SILK kernels existed and both are **landed, byte-identical**:
**S1c** (NSQ LPC prediction, +7–9%) and **S2** (warped-autocorrelation correlation,
+9%) → **SILK ~+12%, gap to libopus 3.1×→~2.9×**. Two candidates were correctly
**ruled out by measurement**, not hunch: **S1d-lite** (NSQ shaping-dot — vectorized
but flat, too thin a slice of a serial/branchy loop) and **S3** (Burg/find_lpc —
ceiling probe showed the vectorizable inner loops are 1.5%; the rest is already-AVX2
FIR + serial root-finding/recursion). The remaining SILK cost is **serial-by-nature**
(NSQ shape+RD recurrence + Viterbi, LPC root-finding) — it does not yield to
per-kernel SIMD. The two remaining levers are structural, not another dot-product:

1. **S1d — cross-state NSQ SIMD** (~35% of SILK): the 4 del-dec states are
   *independent* → vectorize across them (4 lanes), the libopus-1.5 approach.
   Hard: data-dependent RD compare / seed / state-swap per sample. The only big
   single-thread lever left.
2. **R1 — frame/chunk parallelism** (the AAC +6× / Vorbis +5.3× structural move):
   libopus is single-threaded per stream, so multi-core beats it wall-clock even at
   2.9× slower per-thread. Opus carries inter-frame state (SILK LTP/NSQ/entropy,
   CELT preemph/overlap) so exact frame-parallel isn't free like AAC/Vorbis —
   chunked with primed state (PEAQ-gated) or per-stream in `rff`.

Recommendation: **R1 (threads)** is the higher-value, better-precedented next move
for wall-clock parity/win; **S1d** is the harder single-thread purist play. Both are
their own focused campaign.

## Kernel-improvement frontier (2026-07-08) — why SIMD wins some kernels and loses others

Micro-benchmarking each candidate kernel *in isolation* (scalar 4-chain vs cross-
lane SIMD) gives a clean rule, and it settles what's left:

| kernel | structure | SIMD result | why |
|---|---|---|---|
| LPC prediction (S1c) | reduction | **win** | dot product, no dep |
| warped-autocorr corr (S2) | reduction | **win** | MAC over order |
| NSQ shaping filter (S1d) | **serial recurrence** | **1.56× win** (i64-lane) | recurrence caps scalar ILP → filling lanes helps |
| **NSQ RD decision** | **branchy, no recurrence** | **5.3× LOSS** (measured, `tests/nsq_rd_microbench.rs`) | scalar's 4 independent chains already saturate ILP; SIMD needs sat-i32/16-bit-mul/4-way-blendv emulation at only 4 lanes |

**The rule:** cross-lane SIMD wins when a *serial dependency* limits per-chain
scalar ILP (recurrences) and loses when the scalar already has abundant ILP
(branchy straight-line code) — because 4 lanes can't amortize the fixed-point
emulation. The RD (16% of SILK, the biggest remaining kernel) is firmly in the
LOSE column: **not improvable by SIMD.** SILK is otherwise ~98.5% named DSP
kernel, ~1.5% glue — no structural/glue win left either. Remaining single-thread
options are all marginal or non-SIMD: warped-autocorr state update cross-*subframe*
(recurrence → could win ~3%, but conflicts with S2's cross-order correlation), the
rate-loop rerun cut (~5%, algorithmic/PEAQ-gated), or matching libopus's hand-asm
instruction scheduling (large). The realistic verdict: **single-thread SILK is
near its practical pure-Rust ceiling; the wall-clock win is R1 (already landed).**

## The house

### Foundation — instruments (lay first, everything rests on it)

| brick | what | status |
|---|---|---|
| **F1** | Stage profiler `src/prof.rs` (rdtsc, feature-gated, 19 stages, calibrated snapshot) | ✅ 2026-07-08 |
| **F2** | Deterministic benchmark `tests/profile_encode.rs` (3 scenarios, best-of-N ×RT + median breakdown) | ✅ 2026-07-08 |
| **F3** | C-reference baseline (ffmpeg libopus, slope-corrected) — table above | ✅ 2026-07-08 |
| **F4** | **Quality oracle + byte-identity gate**: `tests/oracle_bitexact.rs` (FNV hash of the full packet stream per scenario — the workhorse gate, frozen 2026-07-08); `examples/roundtrip.rs` + `tools/quality_ab.sh` (PEAQ ODG ours vs libopus). Settled the CELT ⚠ (see verdict above) and armed the PEAQ gate. | ✅ 2026-07-08 |
| **F5** | Cheap probes — **done 2026-07-08**: (a) **alloc traffic ZERO** in the hot SILK path (`nsq`/`nsq_del_dec`/`noise_shape`/`control_fixed` use only fixed stack arrays — no `vec!`/`resize`/`clone`); (b) **cache-tiles N/A** — NSQ state is a few KB ≪ L2 and frames are fixed 20 ms, no working-set sweep to bind on; (c) rate-loop clones a whole `SilkNSQState` + range coder per frame but that sits in SILK's 0.3% residue — the S5 lever is NSQ *recompute* (~12% extra calls), not copy cost; (d) bounds-check-ceiling deferred into S1a (measure on the real inner loop, not a speculative 900-line newtype). | ✅ 2026-07-08 |

### SILK wing — the 3.1× gap (order = profiler ranking)

| brick | what | expected | status |
|---|---|---|---|
| **S1a** | **NSQ redundancy pass** — folded into F5/S1b: F5 found **zero heap allocs** (all stack arrays) and the loop is a direct fixed-point libopus port with no recompute/re-walk vein. No byte-identical redundancy brick here; the lever is SIMD (S1c). | — | ✅ none found |
| **S1b** | **NSQ inner-loop decomposition** (info-tier scopes, removed after reading): the 16-tap **LPC short-prediction ≈ 14–28% of NSQ**; the **warped shaping AR filter + RD ≈ 55–70%** but it's a *serial recurrence* (tmp1/tmp2 carried across taps) + branchy RD — hard to vectorize (why upstream only NEON'd the LPC dot product). Confirmed **n_states=4** (the cross-state SIMD axis for a future big brick). | data | ✅ 2026-07-08 |
| **S1c** | **AVX2 LPC short-prediction** (`silk_lpc_prediction_avx2`): 8-tap/iter, emulated signed 64-bit `>>16`, i64-lane accumulate → i32 truncate (== scalar wrapping sum). Runtime-dispatched (`RUSTY_OPUS_NO_AVX2` A/B knob), scalar twin kept as oracle. **Result: +7–9% SILK, +8% Hybrid, BYTE-IDENTICAL** (same-binary interleaved A/B; the cross-build read was noise). Unit test: 200k random cases × orders {10,12,14,16} exact. | landed | ✅ 2026-07-08 |
| **S1d-lite** | **AVX2 shaping-filter dot** — tried & **REVERTED (flat)**. Same split trick as S2 (serial `s_ar2` state update + a vectorized `n_ar = Σ s_ar2[j]·ar_shp[j]` smlawb dot; byte-identical, unit-tested 200k cases, oracle unchanged). But the alternating same-binary A/B was flat (ON median 109.6 vs OFF 109.8, ON slightly *lower* 3/5). Why: the dot is too small a slice of shape+rd (dominated by the serial state update + branchy 2-candidate RD), and the loop split added store/reload overhead that offset the SIMD gain on a small fraction. **The vectorization boundary: split-and-SIMD wins only when the vectorized part is a substantial, cleanly-separable fraction (S1c: only vec op + loop-carried dep; S2: half of a 17%-SILK kernel) — not a thin slice inside a serial/branchy loop.** | flat | ⊘ reverted |
| **S1d** | **NSQ cross-state SIMD — LANDED +7% SILK (Path 2, 2026-07-08).** The first S1d attempt was flat/slower for TWO fixable reasons, both found by an isolated micro-benchmark (`tests/nsq_shape_microbench.rs`): (1) the hand-AVX2 **narrowed/permuted per op** (`smlawb4_avx2`); (2) it **transposed per sample**. The fix: keep the 4 states as **i64 lanes throughout** the recurrence (narrow once, on store — no per-op permute) → **1.56× the scalar in isolation**; and keep `s_ar2` in a **persistent SoA** buffer transposed once per subframe (not per sample). Integrated: split the per-k loop into a shaping pre-pass (cross-state `nsq_shape_filter_soa_avx2` over `sar_soa`) + the branchy RD pass; the RD state-swap copies an SoA column. **Byte-identical** (oracle + 40k-case stress unit test; `silk_sub32_ovflw` never wraps for the bounded Q14 states), **+7% SILK / same-binary A/B** (`RUSTY_OPUS_NO_NSQ_AVX2` knob). With S1c+S2, the full AVX2 stack is **~+31% SILK over all-scalar** (~120→~157× RT). | **+7%** | ✅ 2026-07-08 |
| ~~S1d (first attempt)~~ | **superseded** — the flat/slower cross-state try (per-op permute + per-sample transpose). Kept as the learning that the *implementation*, not the idea, was wrong. Full plan executed: (Phase 1) decomposed — warped shaping filter ≈33% of NSQ, branchless; RD ≈57%, branchy; LPC ≈10% (S1c-done). (Phase 2a) split the per-k loop into a shaping pre-pass + RD pass — **byte-identical**, perf-neutral. (Phase 3) cross-state SoA + vectorized the shaping recurrence (4 states = 4 lanes): **byte-identical both as scalar `[i32;4]` and hand-AVX2**, but **FLAT (scalar) → −7% (hand-AVX2)** on clean same-binary A/B. Why: **4 lanes is too narrow** to amortize the i64-`smlawb` narrow/permute overhead + the SoA transpose, and scalar per-lane `smlawb` already pipelines well. The RD (57%) is branchy — cross-state masking is libopus's full-inner-loop 8-wide-over-2-samples approach, a fundamentally larger byte-identical-hard rewrite (out of scope). **NSQ single-thread kernel SIMD is at its practical pure-Rust ceiling.** All reverted. | flat/slower | ⊘ not viable |
| **S2** | **warped autocorrelation** (`silk_warped_autocorrelation_fix`) — decomposition showed it's **87% of noise-shape = 17% of SILK** (sine-window 0.6%, negligible). The warped all-pass state update is a serial recurrence, but the **correlation accumulation splits out cleanly**: `corr[i] += (state[i]·state[0])>>16` is a vector×scalar i64 MAC over the order dim. Split the fused loop → AVX2 the MAC (reuses the S1c i64/asr16 pattern; `warped_corr_update_avx2`, own `RUSTY_OPUS_NO_WARP_AVX2` knob). **Result: +9% SILK, +9.6% Hybrid, BYTE-IDENTICAL** (200k-case unit test + oracle unchanged). | landed | ✅ 2026-07-08 |
| **S3** | **find_pred_coefs (14.5%) — RULED OUT by ceiling probe.** `silk_find_lpc_fix` is 13.8% of SILK, but it's **diffuse, not one fat kernel**: (a) Burg's inner O(d²) update loops — the obvious SIMD target — are only **1.5% of SILK** (ceiling-probe scope; d²/2 iterations of *short* k-loops, k=0..n, dominated by per-(n,s) scalar setup); (b) `silk_lpc_analysis_filter` (the interp-loop FIR, ×4) is **already AVX2**; (c) the rest is `silk_a2nlsf` root-finding, `nlsf2a` polynomial eval, and Burg's rc recursion (variable per-k shifts) — **serial/iterative, not vectorizable**. Vectorizing Burg's inner loops would gain ≤0.75% for ~150 risky lines. **The probe saved the brick.** | <1% | ⊘ not viable |
| **S4** | **pitch analysis (4.7%)**: correlation kernels share machinery with S3. | small | ☐ |
| **S5** | **Rate-loop rerun cut (~12% of NSQ)**: seed the gain iteration from the previous frame's converged gain / predict bits before re-running NSQ. If prediction only reorders iterations → byte-identical; if it changes final gains → PEAQ gate. | up to ~7% SILK | ☐ |

Amdahl check: S1 (4× on 58.6%) alone ≈ 1.8× SILK overall; S1+S2+S3 at plausible
factors ≈ 2.3–2.6×. Closing the full 3.1× single-thread likely also needs S5 +
resample/glue trims — set expectations per-brick, measure, and let the profiler
rewrite the ranking after each brick lands.

### SILK speed/quality knob (Path 1 — done 2026-07-08)

The `complexity` knob (already exposed as `-compression_level`, R2) drives SILK's
`n_states_delayed_decision` (del-dec depth = the NSQ's dominant cost). Measured
speed (SILK 16 k @24 k) and PEAQ ODG on **real speech** (10 s fixture, upsampled
48 k so PEAQ can score it — absolute ODG is bandwidth-limited/unreliable, the
**deltas** are the signal):

| complexity | n_states | SILK speed | ΔODG vs cx9 |
|---|:---:|---:|---:|
| 9 (default) | 4 | 137× RT | — |
| 7 | 3 | 169× RT (+24%) | −0.07* |
| **5** | **2** | **244× RT (+78%)** | **−0.02** |
| 3 | 1 (no del-dec) | 354× RT (+159%) | −0.04 |

<sub>*synthetic clip; real-speech Δ for 4→2 was −0.022, 4→1 was −0.044 — at/below
PEAQ's ~0.03 noise floor.</sub>

**Finding: `n_states` barely affects speech quality** — 4→2 states is ~1.8× faster
for a PEAQ-neutral (≤0.03 ODG) cost. So the knob is a **near-free speed lever**;
`-compression_level 5` is a strong speed/quality operating point for latency- or
throughput-sensitive use. (This is a quality *trade*, not equal-quality parity —
at equal complexity we're still ~1.9× behind libopus; but the trade is cheap.)
Tooling: `RUSTY_OPUS_COMPLEXITY` env on the bench + a 5th complexity arg to
`examples/roundtrip.rs`.

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
| **R1** | **Frame/chunk-parallel encoding — LANDED 2026-07-08.** ★ **`rff -c:a opus` is ~3× faster than `ffmpeg -c:a libopus` wall-clock** (85 vs 255 ms on 60 s speech; 11.5× its own serial on 24 cores), PEAQ-neutral. **R1b** (`src/parallel.rs::encode_parallel`): chunk the frames, each worker primes with `warmup` frames then keeps its chunk; `std::thread::scope`, no rayon, deterministic. Not byte-identical (VBR seams drift ~2–4%) but PEAQ-neutral at W≥4 (speech ΔODG −0.02 @W8, music −0.002; W=0 naive −0.66 proves priming is essential). **R1a** (`encode_streams`): per-stream parallelism, **byte-identical** to serial (independent streams, bounded pool) — for batch/short-stream workloads. Wired into `rff-codec-opus` (default on; `-opus_parallel 0/1`, `-opus_warmup N`, `-threads N`; buffers the stream and encodes at flush). | ✅ **~3× vs libopus** |
| **R2** | **rff integration bench + knobs — DONE 2026-07-08.** `rff-codec-opus` now honours `-b:a` (bitrate), `-compression_level` (complexity 0–10), `-vbr off` (CBR), `-application voip` — previously hardcoded 64 kbps. Added `Dictionary::get_bitrate` (FFmpeg `k`/`M` suffixes — was a latent bug affecting all audio codecs; `-b:a 128k` never parsed). End-to-end `rff -i speech.wav out.opus @24k` vs system `ffmpeg -c:a libopus`: full-transcode wall-clock **4.33× (overhead-dominated** on a 60 s clip — process startup + WAV decode + Ogg mux + I/O ≈ 0.45 s fixed); the pure-encode gap is ~2.1× (fork benchmark 128× vs libopus ~269× RT). **R1 (parallel) is the lever to win wall-clock.** *(Left uncommitted in remade_ffmpeg_rs — entangled with the local-path fork wiring; commit when the fork is hosted and the dep re-points to a git URL.)* | ✅ 2026-07-08 |
| **R3** | Publish: README benchmark table (reproducible), upstream-PR-able bricks offered back to `restsend/opus-rs`, `docs/benchmarks.md` entry in remade_ffmpeg_rs. | ☐ |

## R1 — frame/chunk-parallel encoding (the plan)

**Goal.** libopus is single-threaded *per stream*, so multi-core wins wall-clock
even at our ~2.5× slower single-thread — the exact structural move that took AAC
to +6× and Vorbis to +5.3×. On a 24-core box, a well-chunked encode should land
**several × faster than libopus wall-clock**, ffmpeg-decodable throughout.

**Why Opus is harder than AAC/Vorbis.** Those had ~no inter-frame state (AAC's
MDCT overlap = just the previous block's samples; Vorbis packets independent), so
frame ranges were *byte-identically* concatenable. Opus carries real state across
frames in one stream (inventoried in `OpusEncoder`):
- **SILK**: LTP history (pitch lag up to ~18 ms), NSQ filter state (`s_nsq`),
  NLSF/gain inter-frame prediction, entropy-context (`ec_prev_*`), `x_buf`.
- **CELT**: pre-emphasis + overlap (`syn_mem`), prefilter/comb memory, per-band
  energy prediction (prev-frame energies).
- **Top level**: variable-HP filter (`hp_mem`, `variable_hp_smth2`), the SILK
  input resampler states (`down2*`, `down_1_3`). (The range coder resets per packet
  — not inter-frame.)

So naive chunking (thread starts cold at frame T·S) gives *wrong* state at the
boundary → a different (still valid) bitstream. Two tiers:

**R1a — per-stream parallelism (byte-identical, easy, ship first).** When `rff`
transcodes *multiple* audio streams/files, run each on its own thread with its own
`OpusEncoder`. Zero correctness risk (each stream is independent), byte-identical,
and it's a real win for batch/server workloads. Lives in the `rff` transcode
scheduler, not the fork. ~1 day.

**R1b — chunked + primed (single-stream speedup, PEAQ-gated, the headline).**
Buffer the whole input, split into `N` contiguous frame-chunks, and give each
thread a *fresh* encoder that **warms up** by encoding `W` frames *before* its
chunk (output discarded), so its state converges to the true continuous state by
the chunk start (a stable encoder forgets initial conditions over a few frames).
Keep only the chunk's own packets; concatenate in order.
- **Warm-up length `W`**: must exceed the deepest state memory — SILK LTP lag
  (~18 ms) + NSQ decision delay + CELT overlap. Start `W = 8` frames (160 ms);
  sweep down under the PEAQ gate. CELT-only (music) needs far less (~2) than
  SILK/Hybrid (speech).
- **Overhead**: `N·W` extra frames. Keep chunks ≫ `W` (e.g. 60 s → 3000 frames,
  24 threads → 125/chunk, `W=8` ⇒ ~6% redundant compute). Cap `N` by
  `chunk ≥ k·W`.
- **Gate (this MOVES the bitstream at chunk seams → not byte-identical)**: PEAQ
  **ΔODG ≤ 0.03 vs the single-thread encode** on a speech+music corpus, AND
  ffmpeg-decodes at unity. Reuse `tools/quality_ab.sh` + the R2 harness. Sweep `W`
  to the smallest that stays neutral.
- **Threads**: `std::thread::scope` over chunk ranges — no `rayon` dep (matches
  AAC/Vorbis). Deterministic (fixed chunk boundaries) so re-runs are identical.
- **Home**: an opt-in fork API (`OpusEncoder::encode_parallel(pcm, …)` or a
  `ParallelOpusEncoder` wrapper) makes rusty-opus *"the first parallel Opus
  encoder"* — a real differentiator — with `rff-codec-opus` calling it when the
  input is fully buffered (file encode). Falls back to serial for streaming/live.
- **Validation ladder**: (1) prototype 2 chunks, prove concat decodes; (2) PEAQ
  ΔODG vs serial at `W=8`; (3) sweep `W` down; (4) scale `N`, measure wall-clock
  vs libopus; (5) A/B the seam artifacts by PEAQ on transient-heavy speech.

**Expected**: near-linear scaling to the core count minus the `~N·W/total`
overhead — realistically **5–15× wall-clock** on 24 cores, decisively beating
libopus's single-threaded encode. Order: **R1a first** (free, byte-identical),
then **R1b** (the headline, PEAQ-gated).

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
- *2026-07-08 (F4)*: PEAQ exposed the CELT speed as a quality trade, not a clean
  win — and revealed our VBR is pinned to the target rate (effectively CBR). The
  synthetic tonal clip *lied* (ours scored better); real dense music told the
  truth. **Bench data-dependent quality on real content** (the Vorbis lesson,
  re-confirmed for Opus).
- *2026-07-08 (S1d-lite revert)*: The split-and-SIMD trick (S2's winning shape)
  went FLAT when applied to the NSQ shaping-filter dot — same correctness (byte-
  identical, oracle green) but no speed. The vectorized dot was a thin slice of a
  serial+branchy loop, and the loop split's store/reload offset the gain. **Lesson:
  the boundary for split-and-SIMD is the vectorized part being a *substantial,
  cleanly-separable* fraction. When the surrounding serial/branchy work dominates,
  vectorizing a sliver is a revert.** The real NSQ lever is cross-state (4 lanes),
  not per-state inner math. Measured revert = a legit result (maps the boundary).
- *2026-07-08 (S1c)*: First optimization brick landed, +7–9% SILK byte-identical.
  Two lessons: (1) the scalar LPC dot product has a **loop-carried dep on `out`**
  so it can't auto-vectorize — a clean case where hand-AVX2 wins (unlike the
  auto-vec-wins pattern). (2) The **cross-build A/B read it as flat (~2%)**; only
  the **same-binary env-toggled interleave** (`RUSTY_OPUS_NO_AVX2`) showed the true
  +7–9% — thermal drift between separate `cargo test` runs swamped the signal.
  Always A/B in one binary. (3) AVX2 has no signed 64-bit shift; accumulate i64
  lanes and truncate once (i32 wrapping sum == i64 sum mod 2³²) to sidestep it.
