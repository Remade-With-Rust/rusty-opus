# SILK Encoder Decision-Site Census — rusty-opus

> Great Gate P0.9 census, 2026-08-07, agent sweep 2/3.
> Scope: `src\silk\` (+ `src\lib.rs` wiring). "LF" = libopus-faithful.

## 1. Missing arms

| site (file:line) | what it decides | current behavior | category | gate/fix candidate | LF? |
|---|---|---|---|---|---|
| silk\encode_indices.rs:166-181 + silk\enc_api.rs:603-605 | SILK stereo: side channel + stereo prediction weights | **Mid-only, always**: `silk_encode_stereo(rc, 0, 0, 1)` with zero pred indices; no `stereo_LR_to_MS`, no side-channel encode, no mid-only-vs-MS decision | 1 | Port libopus `silk_stereo_LR_to_MS` + side encoder; content gate = side/mid energy ratio (libopus already has one) | **NO** — libopus codes real MS stereo |
| silk\enc_api.rs:647-661 | LBRR (FEC) redundant frame content | Copies the **primary frame's indices+pulses**, bumps gains_indices by `lbrr_gain_increases`; libopus `silk_LBRR_encode` re-runs NSQ at the raised gains | 1 | Re-run NSQ for LBRR (cheap: 1-state) — current copy makes redundant frames decode ~gain-mismatched | NO (approximation) |
| src\lib.rs:933-935 | LBRR gain increase amount | Hardcoded `2` if unset; libopus: `max(7 − 0.4·packet_loss_perc, 2)` (silk_setup_LBRR) | 1/4 | Restore loss-driven formula | NO |
| silk\enc_api.rs:486-500 | Whether a frame gets LBRR | Only `use_in_band_fec && packet_loss_perc>0 && signal_type>=UNVOICED` | 1 | libopus also gates on `speech_activity > LBRR_SPEECH_ACTIVITY_THRES` (0.3) — the constant exists (tuning_parameters.rs:18) but is never consumed | NO (gate missing) |
| silk\control_codec.rs:148-161 | Target-rate smoothing / bit reservoir | `silk_control_encoder` is a 3-line shim; no bit-reservoir smoothing (`BITRESERVOIR_DECAY_TIME_MS` dead), no packet-loss rate reduction, no `silk_control_audio_bandwidth` | 1 | Port rate smoothing; interacts with the gain-loop (enc_api) doing all rate work per frame | NO |
| silk\lp_variable_cutoff.rs:64 (callers enc_api.rs:80-84, 613) | Bandwidth-transition low-pass smoothing | `ps_lp.mode` is **never set anywhere** → `silk_lp_variable_cutoff` is a permanent no-op; libopus drives it on SWB↔WB transitions | 1/6 | Wire `mode=±1` on internal-bandwidth switches | NO (machinery ported, driver missing) |

## 2. Signals with no consumer

| site (file:line) | what it computes | current behavior | category | gate/fix candidate | LF? |
|---|---|---|---|---|---|
| silk\flp.rs:731-734, 800 | **Stage-2 pitch correlation `ccmax` in the float pitch core** | `let _ = ccmax;` — discarded. C FLP writes `*LTPCorr = ccmax / nb_subfr` here; so `flp_ltp_corr` stays stale/0 → harm-shape gain and SNR-adj in the float arm run on a dead signal | 2 (**bug**) | One-line fix; plausibly part of why the float arm only *ties* the fixed arm | NO — drift from C |
| silk\vad.rs:230-240 | `input_quality_bands_q15[2..3]` | All 4 bands computed each frame; only bands 0-1 feed `input_quality_q14` (noise_shape_analysis.rs:133-134) and band 0 feeds LF shaping + HP cutoff | 2 | Free content signal for a Great-Gate dispatch (HF quality bands unused) | YES (libopus also ignores 2-3) |
| silk\control_fixed.rs:138-147 | `res_nrg`/`res_nrg_q` per subframe | Consumed only by `process_gains` — the residual-energy vector is an already-computed per-subframe content signal available free for gating | 2 (partial) | Reuse as dispatch feature | YES |

## 3. Free syntax elements shipped as constants

| site (file:line) | syntax element | current behavior | category | gate/fix candidate | LF? |
|---|---|---|---|---|---|
| silk\encode_indices.rs:174-178 | Stereo prediction weight indices (3 ICDF symbols ×2) | Always index 0 (zero weights) | 3 | Same fix as the stereo missing arm — compute real weights | NO |
| silk\encode_indices.rs:180 | `mid_only` flag | Always 1 | 3 | Content decision in libopus (side energy) | NO |
| silk\enc_api.rs:118 | `seed` (2-bit per frame) | `frame_counter & 3` — counter, not content | 3 | LF; NSQ del-dec already searches seeds implicitly | YES |
| silk\lpc_analysis.rs:21 (+ gate at 34-37) | `nlsf_interp_coef_q2` | Forced to 4 (no interpolation) whenever `use_interpolated_nlsfs==0`, i.e. **complexity < 5** — a per-frame bitstream element frozen by a complexity knob | 3/5 | Cheap content gate: run the k-search when spectral change is large even at low complexity | YES |
| silk\enc_api.rs:283-288 | `gains_indices[*] = 4` overflow-frame fallback | Fixed escape values when the frame can't fit | 3 | LF panic path, leave | YES |

## 4. Named-signal-shipping-as-constant

| site (file:line) | named signal | current behavior | category | gate/fix candidate | LF? |
|---|---|---|---|---|---|
| src\lib.rs:933-935 | `lbrr_gain_increases` | ships 2; libopus derives from packet loss | 4 | formula above | NO |
| silk\flp.rs:33 | `USE_HARM_SHAPING` | `const … = 1` compile-time | 4 | LF (C has same #define); tuning candidate only | YES |
| silk\control_codec.rs:70,80,90,… | `pitch_estimation_threshold_q16` (0.8/0.76/0.74/0.72/0.7) | "how correlated must stage-2 be to call it voiced" — keyed on complexity only; the *content* correction lives separately in `thrhld_q13` | 4/5 | Candidate: fold speech-band SNR into search_thres1 | YES |
| silk\noise_shape_analysis.rs:186 | quant-offset energy-variation threshold | Hard `77 * (n_segs-1)` (≈0.6 Q7); named constant `ENERGY_VARIATION_THRESHOLD_QNT_OFFSET` (tuning_parameters.rs:29) only used by the float arm | 4/7 | derive from the named constant | YES (value matches C) |
| silk\control_fixed.rs:236-241 | λ model coefficients (Q) | Local consts duplicating tuning_parameters.rs:55-60 floats (float arm uses those) | 4/7 | single source | YES |

## 5. Threshold-only gates ignoring content

| site (file:line) | what it decides | current behavior | category | gate/fix candidate | LF? |
|---|---|---|---|---|---|
| silk\control_codec.rs:65-146 | **The whole complexity ladder**: `n_states_delayed_decision` (1/1/2/2/2/3/4), `n_nlsf_survivors` (2..16), `shaping_lpc_order` (12..24), `pitch_estimation_lpc_order` (6..16), `la_shape` (3/5ms), `warping_q16` on/off, `use_interpolated_nlsfs` | Keyed **only** on the complexity knob, fixed for the stream; zero content terms | 5 | Prime Great-Gate target: per-frame dispatch of n_states / survivors / shaping order on cheap signals already computed (speech_activity, ltp_corr, pred_gain) | YES |
| silk\control_snr.rs:37-63 | rate → target SNR | Pure table lookup on bitrate+bandwidth (+`nb_subfr==2` rate penalty) | 5 | By design; content enters later via snr_adj — leave | YES |
| silk\enc_api.rs:181 | rate-loop tolerance `bits_margin` | `use_cbr ? 5 : max_bits/4` | 5 | LF | YES |
| silk\enc_api.rs:312-314 | VBR early exit | iter 0 accepted if `n_bits<=max_bits` — never tries to *use* spare bits | 5 | LF; a "spend the margin" arm is a compression experiment | YES |
| silk\enc_api.rs:332-343 | λ inflation when over budget | After 2 failed iters, `lambda *= 1.5` fixed step | 5 | LF | YES |
| silk\enc_api.rs:377-397 | gain multiplier search step | ×3/2 up, ×4/5 down, clamp [64,1024], then bisection | 5 | LF | YES |
| silk\enc_api.rs:623-627 | per-frame bit split in 2×10 ms packets | first frame gets `max_bits*3/5` fixed | 5 | LF; content split (VAD/energy) plausible | YES |
| silk\enc_api.rs:637-641 | CBR enforcement | hard-CBR only on the **last** frame of packet | 5 | LF | YES |
| silk\nsq_del_dec.rs:699-710 (dup nsq.rs:700-706, 857-862) | RDO dead-zone quantizer offset | Engages only when `lambda_q10 > 2048`; offset `λ/2−512` | 5 | λ is content-driven upstream, so borderline; sweepable | YES |

## 6. Dead dials / unwired capability

| site (file:line) | dial | status | note |
|---|---|---|---|
| silk\structs.rs:280,297 + silk\enc_api.rs:137 | `use_flp: bool` | **Never set anywhere** (default false); the entire 1640-line float analysis arm (flp.rs) is reachable only via `SILK_FLP` env | BUILT arm with a gate question — and it carries the LTPCorr drop bug (§2) so its current PEAQ tie is suspect |
| silk\lp_variable_cutoff.rs (whole file) | bandwidth-transition LP | No-op — `mode` unwired (see §1) | |
| silk\tuning_parameters.rs:1 | `BITRESERVOIR_DECAY_TIME_MS` | 0 consumers | bit reservoir never ported |
| silk\tuning_parameters.rs:17 | `SPEECH_ACTIVITY_DTX_THRES` (float) | 0 consumers — fixed path uses `SPEECH_ACTIVITY_DTX_THRES_Q8=13` (define.rs:14) | duplicate representation |
| silk\tuning_parameters.rs:18 | `LBRR_SPEECH_ACTIVITY_THRES` | 0 consumers — the gate it belongs to is missing (§1) | |
| silk\tuning_parameters.rs:27,45,47 | `SPARSE_SNR_INCR_dB`, `INPUT_TILT`, `HIGH_RATE_INPUT_TILT` | 0 consumers | also vestigial in libopus itself |
| silk\tuning_parameters.rs:12,31 | `VARIABLE_HP_SMTH_COEF2`, `WARPING_MULTIPLIER` (float) | 0 consumers — lib.rs:943 re-hardcodes 984; control_codec.rs:11 has `WARPING_MULTIPLIER_Q16=983` | |

**Env-var inventory (complete for the crate):**

| var | site | kind |
|---|---|---|
| `RUSTY_OPUS_NO_AVX2` | nsq_del_dec.rs:266,325; sigproc_fix.rs:806 | measurement A/B toggle (cached once, atomic) |
| `RUSTY_OPUS_NO_NSQ_AVX2` | nsq_del_dec.rs:326 | measurement A/B toggle (cached) |
| `RUSTY_OPUS_NO_WARP_AVX2` | sigproc_fix.rs:807 | measurement A/B toggle (cached) |
| `SILK_FLP` | enc_api.rs:137 | **shipped quality dial** (enables float analysis arm) — checked per frame, un-prefixed, uncached |
| `SILKD` | enc_api.rs:433 | debug index-dump — checked per frame, uncached |
| `RUSTY_OPUS_COMPLEXITY` | tests\profile_encode.rs:132 | bench-only knob |
| `NO_STEREO_TRIM`, `CELT_PF_OFF` | celt.rs:1590, 1974 | shipped dials, un-prefixed (see toplevel census) |
| `RUSTY_OPUS_GATE_HARVEST`/`_GATE_CLIP`/`_FORCE_MODE` | lib.rs (added 2026-08-07) | Great Gate P1 instrumentation, cached at construction |

Decoder side (dec_api/plc/cng/decode_*): no env toggles, no dead dials found.

## 7. Hygiene

| site (file:line) | issue | LF? |
|---|---|---|
| silk\flp.rs:800 | `let _ = ccmax;` drops the C-mandated `*LTPCorr` update (see §2) — code/reference drift, functional | NO |
| silk\enc_api.rs:137,433 | env vars parsed with `std::env::var(..).is_ok()` **every frame** in the encode path, while SIMD toggles use cached `var_os` atomics — two idioms, one hot | — |
| silk\control_fixed.rs:236-241 vs silk\tuning_parameters.rs:55-60 | λ coefficients defined twice (Q vs float), can diverge silently between arms | — |
| silk\define.rs:14 vs silk\tuning_parameters.rs:17 | DTX threshold defined twice (Q8 vs float) | — |
| silk\noise_shape_analysis.rs:186 | quant-offset threshold hardcoded `77` instead of derived from `ENERGY_VARIATION_THRESHOLD_QNT_OFFSET` | value YES |
| src\lib.rs:943 | `VARIABLE_HP_SMTH_COEF2_Q16 = 984` re-hardcoded locally, named constant unused | value YES |
| silk\nsq.rs:700-706 / 857-862 / nsq_del_dec.rs:699-710 | RDO dead-zone block duplicated 3× — a tuning change must hit all three | YES (C dups too) |
| silk\noise_shape_analysis.rs:335-337 | `gain_add_q16_val` constant expression recomputed per frame (trivial) | YES |
| silk\enc_api.rs:575-579 | LBRR-flags ICDF `_ =>` silently falls back to the 2-frame table (unreachable today, trap if 60 ms packets land) | — |
| silk\flp.rs:1422-1428 | float LTP-scale formula differs structurally from fixed path (control_fixed.rs:259-269) — matches the C float/fixed split, but diff against libopus 1.3.1 before trusting the float arm | verify |

## Already-adaptive exemplars (the house style to copy)

- **speech_activity_q8 (VAD)** drives: DTX (enc_api.rs:24-44), pitch voicing threshold (pitch_analysis.rs:98), NLSF quant μ (nlsf.rs:380), NSQ λ (control_fixed.rs:248), VBR background-SNR decrement (noise_shape_analysis.rs:139-150), LF-shaping strength (:348), voiced tilt (:357-361), HP-cutoff smoothing (hp_variable_cutoff.rs:33).
- **ltp_corr_q15** drives: SNR boost for voiced (noise_shape_analysis.rs:153), harmonic shape gain via `sqrt(ltp_corr)` (:386-389), prev-lag bias in pitch search (pitch_analysis.rs:468-478).
- **ltp_red_cod_gain_q7** drives: voiced gain reduction sigmoid (control_fixed.rs:161-169), quant_offset_type for voiced (:224-230), LTP-scale index vs packet loss (:259-269), min_inv_gain for Burg (:110-126).
- **energy variation of the pitch residual → quant_offset_type** for unvoiced (noise_shape_analysis.rs:162-191).
- **input_tilt_q15** drives pitch threshold (pitch_analysis.rs:102) and voiced quant offset (control_fixed.rs:225).
- **pitch-lag-constrained decision_delay** in del-dec NSQ (nsq_del_dec.rs:950-964).
- **Per-subframe gain_lock/best_sum** inside the rate loop (enc_api.rs:361-374).
- **NLSF interpolation k-search** by residual energy when enabled (lpc_analysis.rs:69-124).
- **prev_signal_type-conditional NSQ re-whitening** (nsq_del_dec.rs:991-1057, nsq.rs:58-80).

## Top-10 gate targets (ranked)

1. **SILK MS stereo** (encode_indices.rs:166-181, enc_api.rs:603-605) — mid-only with zero pred weights discards the side channel entirely; largest ODG headroom for stereo speech at SILK/hybrid rates. Not a tuning tweak — a missing libopus arm.
2. **flp.rs:800 LTPCorr drop** — one-line bug fix; re-score the float arm afterward (its "tie" verdict was reached with harmonic shaping running on a dead signal).
3. **Per-frame `n_states_delayed_decision` dispatch** (control_codec.rs ladder → enc_api.rs:215) — NSQ is the SILK hot spot (~59% of SILK); dropping 4→2 states on frames where the 1-state and 4-state RD agree is the canonical dispatch play (speed at neutral ODG).
4. **LBRR triple** — real re-encode (enc_api.rs:647-661), loss-driven `lbrr_gain_increases` (lib.rs:933), speech-activity gate (dead constant) — FEC quality under loss, decoder-proven infrastructure exists.
5. **`n_nlsf_survivors` per-frame gate** (control_codec.rs:76→136; consumed nlsf_encode.rs:55-95) — 16 survivors × del-dec quant at complexity ≥8 on frames whose stage-1 VQ error already separates survivor 1 from 2 is pure waste; harvest err_q24[0]/err_q24[1] ratio as the gate feature.
6. **Voicing threshold content term** (pitch_analysis.rs:94-103 + per-complexity `search_thres1`) — voiced/unvoiced flips are the highest-leverage single decision in SILK; the octave-doubling fix history shows this surface moves PEAQ.
7. **Wire `SilkLPState.mode`** (lp_variable_cutoff.rs:64) — bandwidth-transition smoothing is a no-op today; audible artifacts on SWB↔WB/mode switches; small fix.
8. **Rate-loop economics** (enc_api.rs:181, 312-314, 332-343, 623-627) — a budget-controller variant (spend VBR headroom on low-`coding_quality` frames) is a classic Great-Gate arm.
9. **Shaping order + warping dispatch** (control_codec.rs shaping_lpc_order 12→24, warping on/off) — order-24 warped autocorrelation is expensive; low-spectral-complexity frames don't need it — speed with byte-drift gate.
10. **RDO dead-zone / λ sweep** (nsq_del_dec.rs:699-710 + control_fixed.rs:236-251) — the λ linear model's five coefficients are the distilled-formula surface (symreg candidate per the CASC bridge playbook).

Caveat: everything marked LF is bitstream-compatible tuning space only; items 1, 4, and 7 change encoder outputs but stay within the Opus bitstream.
