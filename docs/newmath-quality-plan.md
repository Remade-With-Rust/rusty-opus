# rusty-opus — the "new-math" quality campaign (trial-and-error, PEAQ-gated)

*Companion to `optimization-plan.md` (which is the SPEED campaign). This one changes
encoder DECISIONS to improve compression QUALITY, gated by PEAQ ODG on a corpus — the
same measurement-first discipline that closed the MP3↔LAME gap. Update the status column
as bricks land.*

## The thesis (honest, evidence-based)

The "cool-japan new-math" hope (symbolic regression replacing LUTs with hyper-fast math)
is **not** where the wins are on these codecs, and the evidence is hard:
- The decision/LUT surfaces are **2–9% of encode time** (profiler); the fat is the *fixed*
  kernels (PVQ 55%, NSQ 49%, MDCT 10%). A perfect LUT→math swap on decisions saves a sliver.
- Every LUT→math swap we tried was a no-op or regression (MP3 `exp2` = bit-identical asm;
  rav1e redundancy removals lost 6–7%). On these codecs the winning direction is the
  *opposite* — math→LUT.
- oxieml's MP3 discoveries are **0-for-4** (psy001–004 all "open", poor fits).

But the notes are **right about two things**: (1) the *method* — data → hypothesis →
BD-rate/PEAQ gate — is the measurement engine that delivered every MP3 win; (2) the
*target* — **RDO / bit-allocation is the holy grail**. MP3 proved it: the bit reservoir
(an allocation decision) closed the entire LAME gap.

**So this campaign uses the measurement engine as the workhorse and oxieml as a minor
supporting tool** (fit a closed form where a table is genuinely the bottleneck AND a
formula beats it on the gate). All bricks below are **decoder-safe** (encoder emits legal
syntax) unless flagged — so they never break Opus interop and gate on PEAQ alone.

## The gates (nothing lands without them)

1. **Round-trip / interop:** our-encode → **libopus-decode** (the strict reference) must
   produce the measured audio; the fork decoder is NOT the oracle for encoder work.
2. **Quality:** PEAQ ODG on the corpus (guitar+piano music @96/128/160/256k; speech
   @16/24/32k), reported vs **libopus at equal bitrate**. Keeper = net ODG gain, no clip
   regressed beyond noise (~0.03).
3. **Speed:** within budget (the decision surfaces are ~cheap, so this is rarely binding).
4. **Safety:** no NaN/overflow; interval-checked where a new formula is introduced.

Harness: `tools/quality_ab.sh <in.wav> <bps> <audio|voip>` (PEAQ ours vs libopus);
`tests/oracle_bitexact.rs` (byte gate for byte-identical bricks only).

## ⚠ Build note (dev workflow)

`remade_ffmpeg_rs` pins the fork as a **git rev**
(`rusty_opus = { git = …, rev = a0b671a }`), so edits to the local `../rusty-opus` have
NO effect until committed+pushed+re-pinned. For iteration, temporarily point the dep at
`{ path = "../rusty-opus" }` (done 2026-07-08); **commit → push → re-pin the rev → restore
the git dep before landing.**

## Baseline v2 (RE-MEASURED 2026-07-08 AFTER the CM1 mono fix + decomposed by lever)

*Delay-aligned PEAQ (peaq_run best_delay), 6 s corp clips resampled to 48 kHz s16, @96k.
Old baseline v1 below is a different/earlier corpus and is superseded.*

| content | ours (72 kB, CBR) | libopus **CBR** (73 kB) | libopus **VBR** (86 kB) |
|---|---|---|---|
| piano **stereo**  | −1.17 | −0.71 | −0.33 |
| guitar **stereo** | −2.76 | −1.39 | −1.01 |
| piano **mono**    | −3.62 | — | +0.03 |

**★ THE GAP DECOMPOSES INTO TWO LEVERS:**
1. **Allocation EFFICIENCY (per-bit) — the big lever.** At EQUAL bytes (~72–73 kB), ours vs
   libopus-CBR: piano **0.46**, guitar **1.37** ODG behind. Pure per-bit quality (RDO/allocation).
2. **VBR bit-spend — a clean +0.38 ODG** on both (spending +12–14 kB). ★ Un-refutes CQ1: VBR was
   ruled out only for the mono *correctness* bug; as a *quality* lever post-fix it's a real,
   well-scoped `compute_vbr` port.

**Mono (post CM1):** no longer a correctness bug — corr vs source UNIFORM (0.77/0.84/0.84/0.86/0.84,
no drift), 0.26→~0.82. Same per-bit efficiency gap as stereo (larger); rides on lever 1.

### Baseline v1 (SUPERSEDED — earlier/different corpus)

| content | ours ODG | libopus ODG | verdict |
|---|---|---|---|
| piano **stereo** | **−0.13** | −0.16 | we ~tie/BEAT libopus ✓ |
| guitar **stereo** | −1.27 | −0.56 | ~0.7 behind (a real tuning gap) |
| piano **mono** | **−3.88** | +0.02 | BROKEN |
| guitar **mono** | **−3.34** | −0.06 | BROKEN |

**★ ROOT-CAUSE FINDING (2026-07-08) — the campaign anchor, and it OVERTURNS "CQ1 VBR is the
fix":** the catastrophe is **the MONO encode path**, not rate control. Rigorously established:
- **CQ1 (VBR) REFUTED.** The break is **bitrate-independent** — guitar mono is −3.3…−3.9 at
  every target 32k→160k, spending the *same* bits as libopus (e.g. @64k both ~76k bps, ours
  −3.74 vs libopus −0.68). VBR only changes the bitrate; it cannot fix a bitrate-independent
  bug. (This isolation probe saved porting the entire `compute_vbr` machinery for zero gain.)
- **It is MONO-specific, not content-specific.** BOTH guitar AND piano are broken in mono
  (−3.34 / −3.88) and BOTH are fine in stereo (guitar −1.27, piano −0.13 which BEATS libopus).
  Cross-correlation vs the source: mono corr **0.18–0.26** (near-decorrelated, real — not an
  alignment artifact, verified by FFT cross-correlation) vs stereo ~0.99.
- **Ruled out** (each by a same-binary A/B knob): transient/short-block path (fires only
  2/364 frames; forcing off is *worse*), the pitch prefilter (forcing off is *worse*), the
  coarse-energy intra bug (byte-identical/inert), and the encode MODE (audio/lowdelay/voip
  all give an identical −3.34 → mono breaks in every mode).
- **Signature for the fix:** mono output carries a ~120-sample lag stereo doesn't, and
  guitar-mono correlation *degrades over time* (0.54→0.07 across 5 s) — smells like a
  mono-path state/desync or a channel-count-gated buffer bug in the CELT/Opus mono encode.

**So the real Phase 0 is a MONO-path correctness bug (codec-bringup-encoder), not a quality
optimization.** Until mono is fixed, mono quality experiments are meaningless (the #1 MP3
lesson). Stereo, by contrast, is ALREADY competitive — piano stereo beats libopus — so the
stereo quality campaign (guitar's ~0.7 gap) is the tuning work, separate from the mono fix.

## The house — MONO fix FIRST (correctness, blocking), then stereo tuning

Attack order rewritten by the baseline finding: the flagship is the **mono correctness bug**,
NOT VBR (refuted). Mono quality experiments are meaningless until it's fixed; stereo is
already competitive, so its tuning comes after.

| brick | what | file:line | expected | status |
|---|---|---|---|---|
| **CM1** | **★ FIX THE MONO ENCODE PATH (flagship).** Mono −3.3…−3.9 (corr 0.2) at all bitrates/modes; libopus mono transparent; stereo fine. codec-bringup method: probe libopus's mono intermediate state (band energies, allocation, pulses, range-coder dif/rng) per frame and diff to find where the channels==1 path diverges. Signature: ~120-sample mono-only lag + correlation degrading over time (state/desync). **Coordinate with the decoder-tools session.** | channels==1 paths in `celt.rs`/`lib.rs` | **−3.5→~0** | ☐ NEXT |
| ~~CQ1 VBR~~ | **REFUTED as the fix** — the break is bitrate-independent; VBR became CS2 (a genuine efficiency lever *after* mono works, not the catastrophe fix). | — | — | ⊘ refuted |
| **CS2** | **True CELT VBR + wire up dead `stereo_saving`** — efficiency lever (BD-rate), not the catastrophe. Gate on corpus BD-rate once mono works. | `lib.rs:414-419`, `celt.rs:1893,2018,2129,2318` | med | ☐ |
| **CQ3** | **Restore tf/spread perceptual weighting.** `importance=[1.0;21]` (`celt.rs:393`) and `spread_weights=[32;21]` (`celt.rs:2040`) are hard-coded flat; libopus derives them from band importance. Flat → pre-echo/smear on transients. | `celt.rs:393,2040`, `bands.rs:96-195` | med (transients) | ☐ |
| **CQ4** | **Coarse-energy intra fix** — DONE the edit, but **inert** on our clips (byte-identical; `is_transient` doesn't fire on this guitar). Keep as a libopus-correctness alignment; re-gate once CQ1/CQ2 make CELT measurable. | `celt.rs:1975` (fixed) | ~0 here | ◑ applied, unproven |
| **CQ5** | **theta-RDO: paid for, never performed.** `theta_rdo` forces resynth (cost) but `quant_all_bands` always passes `theta_round:0`; the ±1 search is never driven → wasted work + a missing lever. | `celt.rs:2206`, `bands.rs:609,2347` | small + speed | ☐ |
| **CQ6** | **`dynalloc_analysis_simple` cap early-break** starves upper bands vs libopus per-band `cap[]`. | `celt.rs:1497-1626` (1609-1612) | small | ☐ |
| **CQ7** | **`alloc_trim` / tf-`lambda` tuning** (heuristic constants → corpus-tuned or oxieml-fit). | `celt.rs:1413-1480,1984` | small | ☐ (oxieml candidate) |

## The house — SILK wing (encoder works; pure tuning headroom)

All decoder-safe. Order by ROI (from the SILK surface map):

| brick | what | file:line | status |
|---|---|---|---|
| **SQ1** | **NSQ `lambda` RD model + beam width** — the direct bits↔distortion knob feeding the trellis (the SILK "reservoir" analog). 6-term linear fit → retune/oxieml-fit. | `control_fixed.rs:243-251`, `nsq_del_dec.rs:538,697` | ☐ |
| **SQ2** | **rate→SNR curves** `SILK_TARGET_RATE_{NB,MB,WB}_21` — 3 empirical monotone curves; the cleanest oxieml closed-form candidate, decoder-safe. | `control_snr.rs:1-32` | ☐ (oxieml candidate) |
| **SQ3** | **Noise-shaping perceptual model** (tilt/harmonic/warp/gain formulas) — the richest "better perceptual model" surface, all fitted heuristics. | `noise_shape_analysis.rs:102-409` | ☐ |
| **SQ4** | **NLSF weighting (`Laroia`) + `nlsf_mu` + wider survivors** — better spectral-distortion weighting; survivor count is free quality. | `nlsf.rs:251,340,380`, `control_codec.rs:65` | ☐ |
| **SQ5** | **Pitch/LTP biases, Burg conditioning** — tuning constants. | `pitch_analysis.rs:441-559`, `lpc_analysis.rs:210` | ☐ |

**Do NOT touch (bitstream-defining — a change forks the format, breaks interop):** CELT
coarse-energy α/β + `E_PROB_MODEL` and the `BAND_ALLOCATION`/`CACHE_*` tables; theta
codebook/`compute_qn`; PVQ `alg_quant`; MDCT. SILK NLSF/LTP VQ codebooks + entropy tables,
pitch-lag codebooks, shell codes. These are the shared decoder tables.

## Where cool-japan actually plugs in (honest)

- **oxieml (symbolic regression):** the ONLY applicable tool. Use ONLY on the flagged
  candidates where a fixed empirical CURVE is the surface (SQ2 rate→SNR, SQ1/CQ7 the
  linear RD/trim models, CG3 shaping formulas). Workflow per the notes: harvest telemetry
  → fit a compact closed form → PEAQ-gate the swap. **Treat as speculative** (0-for-4 in
  MP3); it competes against a human-tuned reference, so it only lands if it beats the gate.
- **scirs2-symbolic:** simplify a discovered formula — only if oxieml discovers a keeper.
- **oxiz (SMT):** BLOCKED (rhai version); use interval arithmetic (prom-prove) for safety.
- **numrs2 / phop:** offline data manip / alternate symreg — marginal, try if oxieml stalls.
- **kizzasi:** N/A (signal predictor, not codec infra).

## Method (the loop, per brick)

1. LOCATE — the baseline table above + a bit-accountant if needed.
2. ISOLATE — offline ceiling where possible (does the change even *can* help before wiring).
3. INTEGRATE — encoder-side; add an env A/B knob (`RUSTY_OPUS_*`) for same-binary A/B
   (thermal-drift lesson: cross-build A/B lies).
4. GATE — the four gates above; **revert non-wins, record the learning.**
5. RECORD — a dated line in the Learnings ledger below + this table's status.

## Learnings ledger (append as bricks land)

- **2026-07-08 (baseline):** CELT catastrophe root-caused to the **MONO encode path**, not
  rate control. BOTH guitar & piano are broken in mono (−3.34/−3.88, corr 0.2) and fine in
  stereo (piano stereo −0.13 BEATS libopus −0.16). Bitrate-independent (32k→160k all bad,
  same bits as libopus) ⇒ **CQ1/VBR REFUTED** (the isolation probe saved a full `compute_vbr`
  port for zero gain). Ruled out via same-binary A/B knobs: transient (2/364 frames; off is
  worse), prefilter (off is worse), coarse-energy intra (byte-identical), mode (all identical).
  Flagship is now **CM1: fix the mono path** (codec-bringup, coordinate w/ decoder session).
  Method lesson (again): prove the ceiling with a cheap isolation probe BEFORE the expensive
  integration — it flipped the entire plan from "port VBR" to "fix a mono correctness bug."
- **2026-07-08 (CM1 localization):** narrowed the mono bug to the CELT **`channels==1`
  band-coding/resynth path**. Decisive probe: **dual-mono** (C=2 with L=R identical, same
  signal) works (−1.21, like stereo) while true mono (C=1) is −3.34 ⇒ the bug is the C=1 code
  path, NOT the content. Mode-independent (mono@96k picks CeltOnly, same as dual-mono; forcing
  lowdelay CeltOnly unchanged). RULED OUT with same-binary A/B knobs: CELT input feed (planar
  layout correct for C=1), forward MDCT (channel-symmetric), the two mono no-op branches
  (`celt.rs:1851` dead `max_val`, `:1886` `let _ = freq[0]`), the pitch **prefilter** (the
  120-sample lag PERSISTS with prefilter off, so the lag isn't the prefilter; corr 0.26→0.45
  off but still broken), and mode selection. Self-tolerated≠legal signal: our mono stream is
  bad in BOTH our decoder (corr 0.55) AND libopus (0.26), and the two decoders disagree (0.79)
  ⇒ a genuinely bad stream. Corr degrades over time (0.54→0.07) ⇒ a C=1 **state/resynth drift**
  (prime suspect: the `syn_mem`/overlap resynth emulation at `celt.rs:1844-1852`, the known
  libopus-divergence the comment flags — note the vestigial `c==0&&b==1&&channels==1` probe
  someone left at 1851). **NEXT (decisive, no libopus needed):** dual-mono C=2 is a WORKING
  oracle for the same signal — instrument CELT to dump per-frame post-resynth `syn_mem`/band
  state for C=1 vs C=2-ch0 and diff to the first divergence (accounting for legit C=1/C=2
  coding-structure differences). OR use the decoder-session's libopus float oracle to diff our
  C=1 CELT frame state against libopus's. The fork's CELT C=1 path was likely never tested
  (tests are stereo).
- **2026-07-08 (CM1 deep dive — range coder IN SYNC, forward path corrupts):** drove CM1 much
  deeper. Instrumented the per-band range-coder `tell` for encode vs decode of the same mono
  stream: **they MATCH at every band (0,9,15,20 → 973/3256/6615/13507)** — so it is NOT a
  desync, NOT a truncation, NOT the fold/resynth (forcing encode resynth=true is byte-identical;
  `enc_decode_mem` is write-only/dead). The range coder writes the right #bits at the right
  positions, yet libopus reconstructs corr 0.25 ⇒ **the encoder codes the wrong VALUES** — the
  mono forward path (MDCT→energy→normalize) feeds wrong data into an otherwise-correct coder.
  The dual-mono oracle is CONFOUNDED: mono carries a **120-sample (one-overlap) encode-side
  delay** that stereo doesn't, so mono and dual-mono never process the same frame at the same
  index (their `in_buf` abs-sums differ 10× = different frames, not a transform scaling bug —
  the overlap-region samples matched when aligned). So the mono bug is a **channels==1
  delay/alignment offset in the CELT/Opus forward buffering** that misaligns the windowed input.
  Ruled out to date (9 hypotheses): input-feed layout, MDCT symmetry, mono no-op branches
  (celt.rs:1851/1886), prefilter, mode selection, resynth-MDCT (dead), resynth-fold
  (byte-identical), band-loop truncation (reaches band 20), range-coder desync (tells match).
  **NEXT (needs the libopus oracle — coordinate with the decoder session):** dump libopus's mono
  CELT `in_buf`/`freq`/band-energy per frame and diff against ours to find the 120-sample
  forward-buffer misalignment; the dual-mono self-oracle can't (delay-confounded). ⚠ COORDINATION:
  the decoder-tools session is now editing this fork's working tree (`src/lib.rs`,
  `src/silk/decode_frame.rs`, resampler-delay traces) — CM1 encoder work must not commingle;
  do it on a branch or hand the localization to that session which already has the libopus oracle.
- **2026-07-08 (CM1 + libopus ENCODER oracle built):** the decoder session's libopus float oracle
  (`.../38a3d8eb-.../scratchpad/oracle/`, `opus_demo.exe` + `srcs.txt` + opus C src at
  `~/.cargo/.../audiopus_sys-0.2.2/opus/`) also builds an ENCODER-instrumented variant. Recipe:
  copy `celt/celt_encoder.c`, inject an `fprintf` after the `compute_band_energies` at line 1732
  (dumps `in`/`freq`/`bandE`, env `LIBFWD`), swap it into `srcs.txt`, build with
  `clang -O2 -DOPUS_BUILD -DVAR_ARRAYS -D_CRT_SECURE_NO_WARNINGS -I<opus> -I<opus>/include
  -I<opus>/celt -I<opus>/silk -I<opus>/silk/float $(cat srcs.txt) <opus>/src/opus_demo.c`, run
  `opus_demo_enc -e audio 48000 <C> 96000 in.pcm out.bit`. Built in my scratchpad at
  `.../e28d56c6-.../scratchpad/encoracle/`. FIRST cross-reference vs ground truth: libopus frame 0
  `in`=all-zeros (its lookahead delay); OURS frame 0 = real data → our encoder runs on a DIFFERENT
  startup delay. This confounds raw frame-by-frame `in_buf` diffing (our WORKING stereo also differs
  from libopus `in_buf`), so "differs from libopus" ≠ the bug. Real signal: for identical content our
  MONO vs STEREO `in_buf` share the same first-6 samples but diverge in the tail (sums 305678 vs
  510877) — a mono-specific mid-frame divergence, not a clean shift. NEXT: **delay-align** first
  (skip libopus's lookahead frame + find the constant sample offset), THEN diff `in`→`freq`→`bandE`
  →`X` per aligned frame to the first divergent stage. The oracle + recipe make this tractable now.
- **2026-07-08 (CM1 — MINIMAL REPRODUCER + forward ruled out via the oracle):** the transient-content
  delay confound is gone — found a **minimal reproducer**: an **8 kHz mono sine breaks (corr 0.045)
  while the SAME signal in stereo works (0.999)**; a 1 kHz mono sine works (0.995). So the bug is
  **mono coding of HIGH bands** (broadband/high content: white-noise 0.015, multi-tone −0.907, guitar
  0.25; low sine fine). MECHANISM (FFT of the decode): the 8 kHz tone reconstructs at **~9 kHz as
  spread noise** in mono — a single spike becomes shifted-up broadband. Using the libopus encoder
  oracle + our own probes, RULED OUT (all IDENTICAL mono vs stereo, or no-effect when forced):
  forward transform/energy (`bandE` matches mono/stereo exactly bar the M/S ×√2 at the tone band),
  allocation (both give the tone's band ~1000-2000 pulses), resynth/fold (byte-identical when forced),
  spreading (NO_SPREAD is *worse*), tf-resolution (`tf_res` identical, band-17=0=freq-res as it should
  be), PVQ inversion (`disable_inv` no-effect). ★ So the bug is INSIDE the **mono PVQ shape
  reconstruction** — the `c_channels==1` pulse-placement/rotation math in `quant_band`/`alg_quant`
  (`bands.rs`/`pvq.rs`) — the spike's pulses land in the wrong bins for mono. NOTE: our forward also
  leaks ~4% of the tone into bands 18-20 vs libopus ~0 (a shared mono+stereo windowing diff, tolerated
  by stereo). NEXT: instrument the PVQ output (reconstructed band-17 normalized vector, `y`/pulses)
  for our mono vs our stereo vs libopus (celt/bands.c `alg_quant`) on the 8 kHz sine, one band, and
  diff bin-by-bin — that's the exact line. Reproducer + oracle at `.../encoracle/` (hi.wav = 8 kHz
  mono; opus_demo_enc.exe = encoder oracle).
- **2026-07-08 (CM1 — PINPOINTED to the intra-band split, oracle-confirmed):** built BOTH an
  encoder- and decoder-instrumented libopus (`opus_demo_enc/dec/split.exe` in `.../encoracle/`;
  decode our ogg via the python ogg→opus_demo repackager). Chain of findings on the 8 kHz mono
  sine: (1) our decoder reconstructs the tone SHAPE correctly at bin 320 for BOTH mono & stereo —
  shape coding fine; (2) libopus reads our stream's ENERGIES correctly (tone at band 17, bands
  18-20 silent) — coarse-energy fine; (3) yet libopus decodes the tone at ~9 kHz → **self-tolerated
  ≠ legal** (our encode+decode agree, libopus differs). (4) `exp_rotation` verified byte-matching
  libopus. ★ **ROOT: the intra-band SPLIT `compute_theta`.** For band 17 (N=64, split 32+32) our
  encoder codes **itheta=682** (energy in the FIRST half = bin ~320) while libopus codes
  **itheta=10352** (SECOND half = bin ~360) for the SAME signal — that IS the 8→9 kHz shift. Also
  our **allocation gives band 17 half the bits** (b=1306 vs libopus 2716). So the divergence is in
  the **forward-energy → clt_compute_allocation → compute_theta** chain (ours vs libopus), at the
  large-band mono split. NEXT: dump the compute_theta INPUTS — the pre-split mid/side energies
  (stereo_itheta's X,Y) ours vs libopus on band 17 — to decide if it's (a) our forward placing the
  tone's energy in the wrong half, or (b) our allocation (fewer bits → coarser/biased itheta). One
  probe each side lands the exact line. Instrumented libopus recipe: swap `celt/bands.c`→bands_instr
  (fprintf after `sctx->itheta=itheta` at bands.c:901, gate `ctx->i==17`) into srcs.txt + rebuild.
- **2026-07-08 (CM1 — it's a range-coder DESYNC, not the theta math; window = 3 symbols):** got the
  decisive answer via the libopus DECODER instrumented on OUR mono stream (ogg→opus_demo repackager).
  At band-17's N=32 split libopus reads **itheta→9, qn=24** while our encoder wrote **itheta→1, qn=24**
  — **qn MATCHES**, so it's not an allocation-qn bug; the range coder is at a DIFFERENT bit position
  when libopus reaches theta → an upstream desync. VERIFIED byte-matching libopus (so NOT the bug):
  the mono triangular theta coding, `exp_rotation`, the prefilter params (`pf_on`/octave/period/`qg`/
  tapset incl. `pitch_index+=1`), and the dynalloc-boost loop. Desync is AFTER coarse-energy (libopus
  reads band-17 energy 6.66 correctly ⇒ in sync there) and BEFORE the band loop ⇒ confined to
  **`tf_encode` / spread-icdf / `alloc_trim`-icdf** (the only remaining pre-band mono symbols;
  intensity/dual-stereo don't code in mono). SEPARATELY: our forward puts the tone in band-17's FIRST
  half vs libopus SECOND half — but STEREO shares that and works, so the forward diff is NOT the mono
  cause; the desync is. ★ THE FINISH: dump decoder tell after `unquant_coarse_energy`(cd.c:1012) /
  `tf_decode`(1013) / spread(1018) / dynalloc, and the same points in our encoder, on the 8 kHz mono
  sine — the first stage whose tell-delta diverges is the exact non-conformant symbol; then diff that
  coder against libopus. (C-probe gotcha: insert WHOLE statements, never mid-declaration.)
- **2026-07-08 (CM1 — RESOLVED: root cause was BAND_ALLOCATION row 10 shifted; conformance-verified):**
  The mono CELT desync is FIXED. Root cause pinned two ways that agree: (1) the decoder-tools session
  landed commit `a69cb2d fix(celt): correct BAND_ALLOCATION row 10 (was shifted, causing stereo CELT
  desyncs)`; (2) my independent forward-side probe of `clt_compute_allocation` converged on the SAME
  table: with the shifted row 10, our `bits2[]` (the hi allocation vector, row 10) diverged from
  libopus starting band 7 (ours 146/300/304… vs libopus 150/320/324…) while `bits1[]` (row 9),
  `thresh`, `cap`, `offsets` all matched — so the interp binary search converged to a different weight
  (lo 14 vs 12), shifting every band's pulse count by 1–2 and desyncing the range coder at the first
  PVQ band. The earlier "range-coder desync at band 2 / itheta divergence" symptom was the DOWNSTREAM
  effect of the wrong pulse allocation, not a theta-coding bug (theta math, exp_rotation, prefilter,
  dynalloc all verified byte-matching — correctly ruled out). ★ CONFORMANCE GATE PASSED: 8 kHz mono
  sine → our encode → **libopus decode == our decode, corr 1.0000** (bit-identical), and both vs the
  original = 0.9815 (inherent lossy quality of a pure tone at 96k, not a bug). Mono CELT now emits a
  fully legal bitstream. rff picks up the fix via the `path = "../rusty-opus"` dev override; the rff
  Cargo.toml git pin must be bumped to ≥`a69cb2d` (NOT reverted to the pre-fix `a0b671a`) once the fork
  publishes. All CM1 probes reverted (rate.rs/celt.rs restored clean); bands.rs/pvq.rs untouched.

## RDO campaign — Brick 1 attempt: transient path (2026-07-09)

**Measurement harness FIX (critical):** the correct encoder-quality gate is `ours-encode →
ffmpeg(libopus)-decode → PEAQ` (a plain `ffmpeg -i ours.opus out.wav` IS a libopus decode — no
repackager needed). `tools/quality_ab.sh`'s `roundtrip` example uses the FORK decoder, which
violates plan gate #1 and chokes on short-block streams — do NOT trust it for encoder work.
Corpus prep: `ffmpeg -i corp.wav -ar 48000 -c:a pcm_s16le` (roundtrip only reads 16-bit; PEAQ
needs ref+test at the same rate). Baseline v2 OFF numbers reproduce via libopus-decode, so the
fork decoder was OK there (few short blocks).

**LOCATE (guitar @96k, ours vs libopus per-frame decisions):** ours detected **4/300 transient
frames, libopus 69/301** (17× under); our `tf_res` was **0 on every frame** (tf_analysis inert)
vs libopus non-zero; our dynalloc boost weaker (≤192 vs ≤528). Root of the transient under-count:
a **scaling bug in `transient_analysis`** — the post-echo threshold `tmp2` used `x2 = |tmp|²/16`
while the running `mean` used `/65536` (the extra `/4096`); libopus uses ONE x2 for both. So tmp2
was 4096× too large → `id = floor(64·norm·tmp2)` pinned at 127 (saturation) → `mask_metric` a
near-constant ~21 (p25=p50=p75=21.4) vs libopus p50=94/p90=927. Folding `/4096` into x2 made our
mask distribution MATCH libopus (p50 94.9, p90 954, 80 transients).

**GATE RESULT — REVERTED (regression).** The detection fix is CORRECT but REGRESSES quality under
the proper libopus-decode gate: guitar stereo −2.67→−3.75, piano stereo −1.14→−2.82, piano mono
−3.61→−3.91. ★ Diagnosis: our **short-block / transient HANDLING is broken** — triggering more
transients (short-block MDCT + the inert tf_analysis, tf_res=0) produces worse audio, so the low
transient count was MASKING a broken short-block encoder (the "self-consistent shared flaw"
lesson). The mask fix only pays off once the short-block path is fixed. Reverted the mask fix; it
is a *precondition-blocked* brick, not a dead one.

**★ NEXT (Brick 1, the real one): fix the short-block / tf handling.** Two coupled targets: (a)
`tf_analysis` produces tf_res=0 always (inert — investigate why; likely another scale/threshold
port bug), and (b) the short-block MDCT/encode path degrades transient frames. Oracle-diff a
single forced-transient frame: ours vs libopus (opus_demo_ei.exe encoder probe already built at
scratchpad/encoracle; add tf_res + short-block MDCT dumps). Only after the handling is conformant
+ quality-neutral does the (already-found) `transient_analysis` /4096 mask fix become the win that
takes guitar toward libopus. NOTE the 1.28 ODG equal-bits guitar gap is bigger than 4 bad frames
can explain → the transient path is ONE lever; expect additional allocation/tf contributors.
