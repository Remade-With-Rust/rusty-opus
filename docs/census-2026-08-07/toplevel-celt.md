# Decision-Site Census — rusty-opus top level (`lib.rs`) + CELT layer

> Great Gate P0.9 census, 2026-08-07, agent sweep 1/3.
> Scope: `src\{lib.rs, celt.rs, bands.rs, rate.rs, quant_bands.rs, pvq.rs}`.
> "Faithful" = mirrors libopus `opus_encoder.c`/`celt_encoder.c` (tuning only under
> bitstream-compatible rules). Line numbers as of the census date.

## 1. Missing arms (capability doesn't exist)

| site | what it decides | current behavior | category | gate/fix candidate | libopus-faithful? |
|---|---|---|---|---|---|
| celt.rs:2560, bands.rs:2353 | Stereo theta rounding RDO (complexity≥8) | `theta_rdo` only enables resynth; `theta_round` hardcoded 0 in `quant_all_bands`; the ±1 rounding arms in `compute_theta` (bands.rs:602-614) are never exercised | 1 (+6: half-built machinery) | Port libopus's dual-round theta RD loop; arms already exist | NO — libopus has the full RDO loop; we lack it |
| celt.rs:2026-2030, 210 | Weak-transient + tone analysis inputs to transient decision | `allow_weak_transients=false`, `tone_freq=0.0`, `toneishness=0.0` hardcoded; the toneishness gate at celt.rs:210 can never fire | 1 | Port libopus tone_analysis; enable weak transients for starved hybrid frames | NO — stubbed |
| celt.rs:1367-1427 | VBR target boosts | `compute_vbr_target` lacks libopus's tonality boost, low-activity reduction, surround masking, LFE and temporal-VBR terms (comment admits) | 1 (+2: tonality/activity ARE computed) | Add `target += f(analysis.tonality)`, `analysis.activity < 0.4` reduction — signals already in `self.analysis` | NO — deliberately omitted subset |
| celt.rs:2407-2408 | Hybrid VBR target nudge by SILK quant offset | Not tracked ("we don't track silk_info yet") | 1 | Plumb SILK signal type/quant offset into CELT hybrid target | NO — missing |
| lib.rs:626-630, 685-691 | NB/MB SILK from >16 kHz API input | Clamped to WB floor — no 48k→8k/12k encode resamplers exist | 1 | Build the two FIR resamplers; unlocks true NB/MB at 48 kHz API | NO — capability gap |
| lib.rs:727-737 | Float stereo SILK for hybrid | Fixed-point stereo SILK loses to CELT-FB >28 kb/s so hybrid is rerouted (see §5) | 1 | Port float stereo SILK (tracked in roadmap) | NO — capability gap |
| lib.rs (encode API) | Variable frame duration | Encoder codes exactly the caller's frame size; no OPUS_FRAMESIZE auto/delay-based frame-size selection | 1 | Frame-size selection from transient density | NO — missing |
| lib.rs:1393-1410 | CELT PLC for lost CELT frames — decoder side | Present (conceal_lost); noted only for completeness | — | — | yes |

## 2. Signals with no consumer

| site | what it decides | current behavior | category | gate/fix candidate | libopus-faithful? |
|---|---|---|---|---|---|
| lib.rs:1150-1163 → celt.rs | `analysis.tonality`, `tonality_slope`, `noisiness`, `activity`, `music_prob*`, `bandwidth` copied into `CeltEncoder.analysis` | Only `valid`, `max_pitch_ratio` (celt.rs:1263-1265), `leak_boost` (celt.rs:1762-1766) are ever read in the CELT layer; the other 7 fields are plumbed and dropped | 2 | Feed tonality/activity into `compute_vbr_target`; bandwidth into `signal_bandwidth` | libopus consumes them; we don't |
| celt.rs:2520 vs 2538 | `last_coded_bands` band-skip hysteresis | Stored each frame and used in `compute_vbr_target` (celt.rs:2420), but `clt_compute_allocation` is called with `prev=0` (celt.rs:2538) → the `j < prev ? 7 : 9` depth threshold in rate.rs:344-345 always takes 9 | 2 | Pass `self.last_coded_bands` as `prev` — one-line, restores libopus hysteresis | NO — libopus passes `st->lastCodedBands`; this is drift |
| celt.rs:2019, 2028 | `weak_transient` out-param | Computed slot exists; always false and never read downstream (libopus uses it to force the tf pattern) | 2 | Wire with the missing arm above | NO |
| lib.rs:93 → celt.rs:1487 | `packet_loss_perc` → CELT `loss_rate` | `OpusEncoder.packet_loss_perc` reaches SILK and equiv-rate math, but `celt_enc.loss_rate` is never assigned (stays 0) → prefilter loss ladder (celt.rs:1252-1259) and coarse-energy `intra_bias` (quant_bands.rs:207) are permanently dead | 2 (+6) | `self.celt_enc.loss_rate = self.packet_loss_perc` in encode() | NO — plumbing gap vs libopus |
| celt.rs:1589, 2518 | `equiv_rate` at two sites | Computed then `let _ = equiv_rate;` — deliberately unused leftovers | 2 (+7) | Delete or consume | n/a |

## 3. Free syntax elements shipped as one constant

| site | what it decides | current behavior | category | gate/fix candidate | libopus-faithful? |
|---|---|---|---|---|---|
| lib.rs:1134-1136 | Hybrid redundancy flag (per frame) | Always encodes `redundancy=0`; encoder never emits a 5 ms CELT redundant frame at mode transitions. The decoder fully implements both directions (lib.rs:2044-2140, 2332-2458) | 3 | Encode redundancy on SILK↔CELT transitions (celt_to_silk / silk_to_celt), exactly what the decoder already handles | NO — libopus encodes these |
| SILK-only path (lib.rs:1216-1270) | Implicit SILK-only trailing CELT redundancy | Never appended (decoder reads it at lib.rs:2057) | 3 | Same as above | NO |
| celt.rs:2094-2097 | CELT per-frame silence flag | `let silence = false;` — the silence bit is coded (logp 15) but never true, even for digital silence; CBR silence frames burn full rate | 3 | Compute silence from band energies as libopus does | NO — libopus detects silence |
| celt.rs:2589-2594 | `anti_collapse_on` bit | `consec_transient < 2` only | 3/5 | Content term = actual collapse-mask density; cheap per-frame | YES (faithful) |

## 4. Named-signal-shipping-as-constant

| site | what it decides | current behavior | category | gate/fix candidate | libopus-faithful? |
|---|---|---|---|---|---|
| celt.rs:1582-1592 | alloc_trim +1 LF tilt for stereo | Comment names the target content ("coupled **stereo music**… mono untouched") but ships an unconditional `trim += 1.0` for ALL stereo — including speech — with only an env opt-out (`NO_STEREO_TRIM`) | 4 | Gate on `music_prob`/`voice_est` (signal exists at Opus level); classic per-clip sign-flip suspect | NO — house deviation (PEAQ-tuned) |
| celt.rs:2512-2517 | `signal_bandwidth` for band-skip decision | Comment names C's signal ("C uses the **analysis bandwidth**, celt_encoder.c:2174") but ships `end_band-1`; refuted twice by PEAQ (leak_boost retest included) | 4 (+6 refuted-and-kept) | Re-attempt as per-clip dispatch, not always-on (sign-flip rule): narrow only when analysis bandwidth is decisively below end AND content is speech-like | NO — deliberate deviation, do not flip without corpus win |
| lib.rs:113-115, 384 | `lsb_depth` for analysis + dynalloc noise floors | Doc names the signal ("set 16 for s16-sourced content"); ships 24 unless caller remembers | 4 (+7 doc/default drift) | Auto-detect from input API (i16 vs f32 entry points) | Float-API default 24 is faithful; the drift is ours |
| lib.rs:727-737 | Stereo hybrid→CELT-FB reroute threshold | Comment names the evidence (PEAQ on stereo **speech**) but the gate is `bitrate_bps > 28000` only — content-blind | 4/5 | See §5 row 1 | NO — house deviation |

## 5. Threshold-only gates that ignore content

| site | what it decides | current behavior | category | gate/fix candidate | libopus-faithful? |
|---|---|---|---|---|---|
| lib.rs:734 | Stereo hybrid vs CELT-FB routing | `channels==2 && mode==Hybrid && bitrate_bps > 28000` → force CELT-FB | 5 | Add voice_est/music_prob term: hybrid measured to WIN at 24k and lose at 32k+ on speech — per-content the crossover certainly moves | NO — house deviation |
| lib.rs:623-625 | Hybrid CBR bandwidth cap | `use_cbr && bitrate < 15000` → cap WB | 5 | Content term plausible (speech tolerates WB better than music) | NO — house deviation |
| celt.rs:2275-2287 | Intensity-stereo start band | Hysteresis on `equiv_rate/1000` only (INTEN_THRESHOLDS/INTEN_HYSTERESIS celt.rs:1490-1495) | 5 | Inter-channel correlation is ALREADY computed per-frame in `alloc_trim_analysis` (celt.rs:1541-1568) — a stereo-width term is free; memory notes stereo-music gap is "broader mid/overall coding" so gate carefully | YES (faithful) — tuning only |
| celt.rs:2239 | tf_analysis enable | `complexity>=2 && effective_bytes >= 15*channels` | 5 | — | YES |
| celt.rs:2295 | Spreading decision enable | `is_transient \|\| complexity<3 \|\| effective_bytes<10*c` → SPREAD_NORMAL | 5 | — | YES |
| celt.rs:1970-1974 | Prefilter enable | `start_band==0 && complexity>=5 && bytes>12*c` | 5 | — | YES |
| celt.rs:1267-1285 | Prefilter gain threshold ladder | `pf_threshold` bumps keyed on byte budget (25/35) and prior gain (0.4/0.55) | 5 | — | YES |
| lib.rs:473 | Run tonality analysis | `complexity>=7 && Fs>=16000` | 5 | — | YES |
| celt.rs:2237 | tf Viterbi lambda | `80.max(20480/effective_bytes+2)` | 5 | — | YES |
| celt.rs:2179-2185 | intra_ener at complexity<4 | all `old_band_e <= -27.0` | 5 | — | YES (approx) |
| quant_bands.rs:218-222 | coarse max_decay | `min(16, 0.125*bytes)` when >10 bands | 5 | — | YES |
| rate.rs:344-351 | Band-skip stop decision | `depth_threshold` 7/9 keyed on `coded_bands>17`, `j<prev`, `j<=signal_bandwidth` | 5 | Fix `prev` (see §2) and revisit signal_bandwidth (see §4) | YES in shape; `prev=0` is drift |
| lib.rs:503-509, 823-850 | DTX activity + hangover | `activity_probability >= 0.1`, LO=400/HI=1200 ms Q1 | 5 | — | YES (SNR fallback omission documented, adds-activity-only) |

## 6. Dead dials and unwired capability

| site | what it decides | current behavior | category | gate/fix candidate | libopus-faithful? |
|---|---|---|---|---|---|
| lib.rs:1304, 1367 | `OpusDecoder.hybrid_skip_celt` | pub field, initialized false, never read anywhere | 6 | Delete or wire | n/a |
| pvq.rs:2334-2450 | `alg_quant_qext` (+ `pvq_search_qext`, `ec_enc_refine`, refine coding) | ~120 lines of extra-bits/qext quantizer with ZERO callers | 6 | Delete or park behind a feature; it silently drifts | NO — experimental leftover |
| celt.rs:1448, 1829 | `constrained_vbr` | pub, defaults true; no setter anywhere in crate (OPUS_SET_VBR_CONSTRAINT unexposed at Opus level) | 6 | Expose knob; unconstrained VBR is a known quality lever for file encoding | default faithful |
| celt.rs:1487, 1868 | `loss_rate` | Always 0 (see §2); loss ladder + intra_bias dead | 6 | Plumb from packet_loss_perc | NO |
| bands.rs:23, 705, 730; celt.rs:2585 | `disable_inv` (phase inversion disable) | Machinery on both sides; every caller passes `false` (OPUS_SET_PHASE_INVERSION_DISABLED missing) | 6 | Expose CTL; matters for mono-downmix delivery | default faithful |
| bands.rs:602-614 | `theta_round` ±1 arms | Present, never selected (see §1) | 6 | Theta RDO | NO |
| bands.rs:197-251, 270-307 | `haar1_avx`/`haar1_neon` | Disabled dead code — known deinterleave bug, scalar forced (bands.rs:197-205) | 6 | Fix & re-verify bit-exact, or delete | n/a |
| bands.rs:1843-1974 | `stereo_merge_avx2/neon/scalar` twins | `#[allow(dead_code)]`, unused — live `stereo_merge` (bands.rs:1710) has its own loop | 6 | Delete or wire with oracle test | n/a |
| celt.rs:137-153 | Weak-transient forward_decay switch (0.0625 vs 0.03125) | Reachable only via `allow_weak_transients` which is always false | 6 | see §1 | NO |
| quant_bands.rs:199; celt.rs:2203 | `lfe` arm of coarse quant | Parameter exists, always false (no LFE channel support) | 6 | Needed for surround LFE streams | NO (arm exists in C) |
| silk/enc_api.rs:137 (context) | `SILK_FLP` env — float SILK analysis | Opt-in, default-off, PEAQ-tied | 6 | Leave; documented | deviation kept off |

## 7. Hygiene

| site | what it decides | current behavior | category | gate/fix candidate | libopus-faithful? |
|---|---|---|---|---|---|
| celt.rs:1590 | `NO_STEREO_TRIM` env check | `std::env::var` **inside `alloc_trim_analysis` — runs every stereo frame** (allocates a String on the error path too) | 7 | Read once at encoder construction into a bool | n/a |
| celt.rs:1974 | `CELT_PF_OFF` env check | `std::env::var` every frame in encode_impl | 7 | Same hoist | n/a |
| celt.rs:1590 vs silk/nsq_del_dec.rs:266 | Env parsing convention | `var().is_err()` / `.is_ok()` (String alloc) vs `var_os().is_none()` — three styles across the crate | 7 | Standardize on `var_os` read-once | n/a |
| celt.rs:64, 223, 626, 1127; bands.rs:2577, 2648, 2747, 2927; pvq.rs:717, 1250, 2175, 2250 | SIMD dispatch | `is_x86_feature_detected!` per invocation in hot kernels (sum_abs, l1_metric, comb filters, band energies, normalise, renormalise, pvq resynth) | 7 | Cache once per encoder (std caches the CPUID but each call is still an atomic load + branch in leaf kernels) | n/a |
| bands.rs:906-1120 vs 1124-1414 | `quant_partition_encode` vs `quant_partition` | Two ~250-line copies of the split recursion that must stay in sync (plus 4 near-identical n2/n4/n8/n16 leaf copies at bands.rs:763-902) | 7 | Collapse; divergence here = silent conformance bug risk | n/a |
| lib.rs:541-548 vs 573-580 | `compute_equiv_rate` | Computed twice per frame with identical arguments (mode decision + bandwidth walk) | 7 | Compute once | n/a |
| celt.rs:2358-2360 vs 3059-3062 | dynalloc_logp update, enc vs dec | `2.max(x-1)` vs `x.max(2)-1; .max(2)` — numerically equal, textually diverged (trap for future editors) | 7 | Unify expression | both faithful |
| lib.rs:113-115 | lsb_depth doc vs default | Doc prescribes 16 for s16 content; default 24; nothing enforces | 7 | See §4 | n/a |
| celt.rs:1589, 2518 | `let _ = equiv_rate;` ×2 | Leftover suppressions from removed experiments | 7 | Remove | n/a |

## Already-adaptive exemplars (house styles)

- **Speech/music classifier → mode + bandwidth**: `voice_ratio` from `analysis.music_prob` with hysteresis-correct min/max selection by previous mode (lib.rs:511-520), squared-interpolated into mode threshold (lib.rs:176-207) and bandwidth threshold tables (lib.rs:593-616).
- **Detected bandwidth → coded bandwidth cap** for SILK/hybrid, rate-floored (lib.rs:664-682).
- **DTX**: analysis VAD probability gate + silence detection + hangover ladder (lib.rs:499-509, 823-850).
- **analysis.leak_boost → dynalloc follower** (celt.rs:1762-1766) and **max_pitch_ratio → prefilter gain** (celt.rs:1263-1265) — the two analysis signals the CELT layer actually consumes.
- **Transient analysis → short blocks / tf_estimate** (celt.rs:132-218) feeding trim, VBR target, and tf Viterbi.
- **Dynalloc analysis**: median-filtered follower vs noise floor → per-band boosts, importance weights, spread weights (celt.rs:1617-1802).
- **Spreading/tapset decision** from normalized-spectrum statistics with SMR masking weights + hysteresis vs last decision (bands.rs:96-195).
- **alloc_trim** from stereo correlation, spectral tilt, tf_estimate (celt.rs:1517-1596).
- **dual_stereo** from L/R-vs-M/S L1 comparison (celt.rs:569-596); **intensity hysteresis** (celt.rs:1497-1514); **inv flag** from itheta (bands.rs:705).
- **Two-pass coarse energy** intra-vs-inter selection by coded badness + rate tiebreak (quant_bands.rs:234-291).
- **Prefilter on/off** from measured pitch gain vs adaptive threshold (celt.rs:1267-1298).
- **Adaptive HP cutoff** from SILK pitch smoother (lib.rs:937-950).

## Top-10 new gate targets (encoder), ranked

1. **Wire `analysis.tonality`/`activity` into `compute_vbr_target`** (celt.rs:1372-1427). Signals computed every frame and dropped; libopus's tonality boost is one of its main VBR quality levers. Faithful upgrade, no bitstream risk, corpus-wide ODG upside.
2. **Gate the +1 stereo trim tilt on music_prob** (celt.rs:1590). Its own comment says the win is "coupled stereo music"; stereo speech pays it unconditionally today. Direct sign-flip-dispatch candidate; also fixes the per-frame env::var.
3. **Content-aware stereo-hybrid reroute** (lib.rs:734). Replace `bitrate > 28000` with a voice_est×rate surface — hybrid already measured to WIN at 24k and lose above; per-content the crossover moves. Textbook dispatch trigger.
4. **Pass `last_coded_bands` as `prev` to `clt_compute_allocation`** (celt.rs:2538 → rate.rs:344). One-line drift fix restoring libopus's band-skip hysteresis (depth threshold 7-vs-9).
5. **Theta RDO for stereo at complexity≥8** (celt.rs:2560, bands.rs:2353). The ±1 rounding arms already exist; only the outer RD loop is missing. Targets the known ~0.4-0.5 ODG stereo-music gap.
6. **Plumb `packet_loss_perc` → `celt_enc.loss_rate`** (lib.rs encode → celt.rs:1487). Activates the prefilter loss ladder and coarse intra_bias — quality-under-loss currently silently disabled.
7. **Encoder mode-transition redundancy frames** (lib.rs:1134, SILK-only tail). Free syntax the decoder already fully supports; removes audible SILK↔CELT transition artifacts on mixed content.
8. **Intensity start band with a stereo-width term** (celt.rs:2275): the inter-band correlation is already computed in `alloc_trim_analysis` — add it to the rate-only hysteresis (bitstream-compatible tuning; ITHETA lesson says be conservative).
9. **CELT silence flag + weak-transient/tone analysis** (celt.rs:2094, 2026-2030): silence saves CBR bits and stabilizes energy state; weak transients are libopus's fix for low-rate hybrid attack smearing.
10. **Hygiene sweep for speed**: hoist the two per-frame `env::var` calls, cache SIMD dispatch per encoder, deduplicate `quant_partition`/`quant_partition_encode` (conformance-risk insurance).

Notable "do NOT re-flip without a corpus win" markers found in-code: CELT-only detected-bandwidth narrowing (lib.rs:649-663) and `signal_bandwidth` = end−1 (celt.rs:2512-2517) — both PEAQ-refuted twice; any retry should be per-clip dispatch, not always-on.
