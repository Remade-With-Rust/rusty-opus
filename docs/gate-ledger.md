# Gate ledger — rusty-opus

Every content gate in canonical form (template §4):
`GATE := (unit, signal, threshold-form, arms, fallback, ledger-entry)`.
Regenerate the per-class evidence with `tools/gate_regression.py` (one run);
CI rule: **no sign-flip on any tracked class** vs the banked baseline.
Baseline: `target/ladder_baseline.csv` tag `baseline-2026-08-07` (10 clips ×
5 rates, arms ours+libopus, external PEAQ).

Published-version law: rff and other consumers pin the **registry** crate; a
gate is only "shipped downstream" when the version in its row is on crates.io.

## Shipped gates (by construction / prior campaigns)

| gate | unit | signal | threshold-form | arms | fallback | per-class evidence | shipped in |
|---|---|---|---|---|---|---|---|
| SILK/CELT/hybrid mode | frame (hysteresis via prev-mode) | `music_prob{,_min,_max}` → voice_est | libopus threshold tables, voice/music interpolated | SILK / CELT / (hybrid via bw) | n/a (always on, libopus-faithful) | conformance suite; baseline ladder | 0.1.x (since bring-up) |
| Coded bandwidth | frame | equiv rate × voice_est² + `detected_bandwidth` cap | rate walk + hysteresis | NB…FB | force_bandwidth override | baseline ladder | 0.1.x |
| DTX | frame run | VAD `activity_probability` + silence | fixed 0.1 + hangover ladder | transmit / 1-byte DTX | `use_dtx=false` (default off) | oracle-clean (Tier-1 campaign c40b9ab) | 0.1.23 |
| Stereo hybrid→CELT-FB reroute | stream | bitrate only (**content-blind — candidate for refit**) | `>28 kb/s` | hybrid / CELT-FB | force_bandwidth | in-code PEAQ table (lib.rs:734 comment): hybrid wins 24k, loses 32k/48k on stereo speech | 0.1.23 |
| CELT-only detected-bw narrowing | frame | analysis bandwidth | — | narrow / **hold FB (chosen)** | n/a | PEAQ-refuted 2×, recorded lib.rs:649-663 — do not re-flip without per-clip dispatch | 0.1.23 (held) |
| SILK complexity ladder | stream | complexity knob (**no content term — P2 queue**) | fixed ladder | n_states 1-4, survivors 2-16, order 12-24 | complexity=9 default | benchmarks.md (+78% at ≤0.03 ODG for c5) | 0.1.23 |
| **CELT silence flag** | frame | `sample_max` over frame + previous overlap tail vs `1/2^lsb_depth` | digital-silence test (libopus-exact) | code silence + collapse the budget / normal coding | `RUSTY_OPUS_SILENCE_FLAG=0` — verified byte-identical on all 39 hash rows | 13-class rate-matched BD: **mean +0.198, worst class exactly +0.000**; silence_dtx **+1.647**, speech_clean +0.390, percussive +0.181. Independent libopus decode: equal quality (−3.8740 vs −3.8747) at **28% fewer bits**, packet shape now matches libopus. CBR: exact packet length preserved. Suite 218/0 | **rusty-opus 0.1.26** (2026-08-07, crates.io); rff lock updated |
| **Analysis warm-up guard** | frame | analysis frame count vs convergence threshold | N = 10, from the analysis's own `count < 10` fast-adaptation window (not fitted to our corpus) | classifier verdict / application default | `RUSTY_OPUS_ANALYSIS_WARMUP=0` — **verified byte-identical on all 39 hash rows** | 13-class × 5-rate ladder: **14 wins / 0 losses / 1 neutral (−0.005)**, mean +0.385 on the 15 changed rungs, best +1.212 (silence_dtx@32k), **all VoIP rungs +0.000**. Suite 218/0 | **rusty-opus 0.1.25** (2026-08-07, on crates.io). Downstream verified: rff Cargo.lock resolves 0.1.25 and `rff -c:a opus` now emits celt/FB 100% on speech (was 4% startup hybrid) |

## Instrumentation (not gates; byte-identical proven)

| lever | env | proof | landed |
|---|---|---|---|
| Harvest tap | `RUSTY_OPUS_GATE_HARVEST` / `_GATE_CLIP` | hash-equal output, equal bytes, suite green | 2026-08-07, unpublished |
| Force mode | `RUSTY_OPUS_FORCE_MODE` | unset path untouched; suite green | 2026-08-07, unpublished |

## Defects found by the campaign's own instruments (2026-08-07)

| # | defect | instrument that found it | status |
|---|---|---|---|
| D0 | **CELT silence flag never set** (`celt.rs:2104 let silence = false`) — we spend **69%** of the active-frame bitrate on silent frames where libopus spends **2.9%** (3.0 B vs 102.4 B packets). ~25% of the bit budget on the silence class is spent coding nothing | P0 per-class baseline: `silence_dtx` **−1.707 BD-ODG**, the worst class by 3×; confirmed by per-frame byte stats from the tap + libopus packet-size histogram | root-caused, mechanism + C in hand (`docs/great-gate.md` §5.5); **#1 P3 build**. Decoder side already implements it |
| D1 | ✅ **ROOT-CAUSED 2026-08-07 — it IS `lsb_depth` after all.** Probe (`RUSTY_OPUS_BW_DEBUG=1`) on lp4000: `noise_floor = 7.6e-17` (from lsb_depth 24), measured `hp_e = 7.4e-10` vs threshold `1.2e-13` — **6000× over**, so the `hp_ener` branch forces `bandwidth = 20` every frame; the band loop independently saturates to 18 for the same reason; `is_masked[NB_TBANDS] = 0` so the masking rescue never fires. At lsb_depth 16 the threshold becomes 2.4e-8, *above* `hp_e`, and the detector tracks content: lp4000→NB, lp8000→WB, lp12000→SWB, unfiltered→SWB/FB. My earlier refutation was wrong in scope: it correctly showed lsb_depth cannot explain the *divergence from libopus* (ffmpeg never sets it), but lsb_depth IS the operative variable on our side. **Ship question is open**: lsb_depth also feeds the dynalloc/leak_boost floors, so setting it from the input format needs its own ladder A/B | (superseded) | root cause closed; the fix is a separate brick |
| D1-old | ~~`detected_bandwidth` is pinned at FB regardless of content~~ — the bandwidth signal is a constant, so every consumer of it (incl. the SILK/hybrid narrowing) is a no-op. libopus is content-driven on the same clips, so this is a divergence | P1 detector truth table (`gate_bw_truthtable.ps1`): 4/6/8/12 kHz low-pass all report FB, 100% of frames; libopus TOC cross-check | **measured + localized, root cause OPEN**. Structurally must be the `hp_ener→bandwidth=20` branch or the `count<=2` warm-up (the band loop caps at 18=SWB). `lsb_depth` and a resampler port error both REFUTED |
| D2 | float SILK `LTPCorr` never written (`let _ = ccmax`) — harmonic shaping ran on a dead signal | census sweep 2 | ✔ fixed 2026-08-07; float arm needs re-scoring (its "tie" verdict is void) |
| D3 | `CeltEncoder.loss_rate` never assigned — CELT prefilter loss ladder + coarse intra bias dead under packet loss | census sweep 1/3 | ✔ fixed 2026-08-07 (default loss=0 path unchanged) |
| D4 | `lbrr_gain_increases` hardcoded 2 instead of libopus's `max(7−0.4·loss%, 2)` | census sweep 2 | ✔ fixed 2026-08-07 (FEC-off unaffected) |
| D5 | band-skip hysteresis drift: `clt_compute_allocation(prev=0)` instead of `last_coded_bands` | census sweep 1 | open — output-changing, own brick + ladder A/B |
| D6 | `tonality`, `tonality_slope`, `activity` computed every frame, consumed by nothing (libopus's VBR/trim levers) | census sweep 1/3 | open — P3 arm, formula in hand from celt_encoder.c |

## Refuted hypotheses (recorded so they are not re-chased)

- **"The orphaned analysis signals are worth +0.114 ODG."** REFUTED 2026-08-07.
  That figure came from scoring the tonality arm against `ladder_baseline.csv`
  — the pre-warm-up-guard, pre-silence-flag baseline — while the arm itself ran
  on a binary that already had the warm-up guard. It was crediting the warm-up
  guard's gain to the tonality boost. Re-measured against a like-for-like
  baseline (same binary, only `RUSTY_OPUS_TONAL_VBR` toggled), the complete
  group — tonality boost + activity reduction + tonality_slope trim — over
  **18 classes × 5 rates** is worth **mean +0.001**, winning exactly one class
  (bass-heavy electronic +0.058) and costing −0.045 on percussive. **Kept
  opt-in.** Lesson: a brick's baseline must differ from its arm in *one* thing;
  ours differed in three, and the extra two were the whole effect.

- **"The speech/music classifier is broken."** `music_prob` saturates to 1.000
  on clean speech, but libopus routes the same clip to CELT too under
  `-application audio` (TOC histogram, `tools/opus_toc_stats.py`). Mode
  selection does not diverge; bandwidth does. Refuted by the reference's own
  behaviour on our content, before any code was changed.

## Candidate gates (open, calculator-gated — none bankable yet)

| candidate | unit | signal | status |
|---|---|---|---|
| ~~Analysis warm-up guard~~ | — | — | ✅ **SHIPPED — promoted to the table above.** |
| ~~Mode-dwell hysteresis~~ (`mode_dwell`, kept default-off) | frame | mode run-length | ❌ **REFUTED 2026-08-07, measured worse, kept behind the env toggle.** Built for "isolated flips" that do not exist: the non-CELT frames are ONE contiguous run (frames 0-23). Dwell delays transitions in both directions, so it only postpones the exit — non-CELT frames went 24 → 25/26/28/33 for N=2/3/5/10, exactly +(N−1). Superseded by the warm-up guard |
| *(superseded context)* force-CELT ceiling experiment | stream | — | ★★ **CONFIRMED PRIZE, now captured by the warm-up guard.** Full-corpus force-on: **18 wins / 0 losses / 1 neutral** over 50 rungs, 31 of them byte-identical; mean +0.390 on changed rungs, best **+1.212** (silence_dtx@32k), bitrate slightly *down*. Moves us from −0.212 to **−0.034** mean vs libopus and halves the worst class. Mechanism proven 100% (non-CELT frames present ⟺ score changed, 19/19 and 31/31); the cost is the mode TRANSITIONS, not the hybrid frames. Calculator: **VERDICT-CAPABLE**, zero predicates ⇒ not a dispatch, a straight fix. **Do NOT ship `force_mode=celt`** — it removes SILK; corpus covers only `audio` ≥16 kbps (no voip/low-rate/DTX/FEC). Ship dwell hysteresis + close the corpus gap first |
| Band-limited bandwidth-narrowing gate | frame | detected_bandwidth × rate × voice_est | **BLOCKED by D1** — the signal is a constant; cannot fit until the detector is fixed |
| VBR target tonality/activity terms | frame | orphaned `tonality`, `activity` | census #1; faithful port, ladder A/B next |
| Stereo trim +1 on music_prob | frame | `music_prob`, `log_xc` | census #2; sign-flip suspect |
| Stereo-hybrid reroute refit | stream | voice_est × rate | census #3 |
| Per-frame n_states dispatch | frame | VAD, ltp_corr, pred_gain | NSQ speed play; needs work counter |

## Reverted / refuted (with kind — template law §12)

- CELT detected-bandwidth narrowing: measured worse (PEAQ, twice, incl. with
  leak_boost live). Not noise. Machinery kept, held off.
- `signal_bandwidth` = analysis bandwidth for band-skip: measured worse; ships
  `end_band-1` (celt.rs:2512-2517).
- NB/MB low-rate modes as quality play: PEAQ-refuted (our WB/fixed beats
  libopus's narrowing) — completeness-only now.
- Float SILK analysis arm: **RE-SCORED 2026-08-07** after the LTPCorr fix, on
  the voip classes (the only ones where SILK is the working arm), rate-matched
  BD-ODG: `voip_speech_noisy` **+0.082**, `voip_mixed` −0.010, `voip_speech`
  **−0.123**. Still default-off — but the old "ties fixed-point" verdict is
  replaced by something more useful: a **per-class sign flip** (wins noisy
  speech, loses clean speech), i.e. a dispatch candidate on a noisiness signal
  rather than a knob to discard. Weak evidence though — 3 classes, and PEAQ
  saturates on narrowband speech; needs a speech-domain metric before banking.
