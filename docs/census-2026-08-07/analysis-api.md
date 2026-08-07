# Decision-Site Census — analysis / parallel / repacketizer / multistream / API + crate hygiene

> Great Gate P0.9 census, 2026-08-07, agent sweep 3/3.
> Scope: `src\{analysis.rs, analysis_data.rs, parallel.rs, repacketizer.rs, multistream.rs, hp_cutoff.rs, modes.rs, prof.rs}` + crate-wide env/feature/doc hygiene.

## AnalysisInfo consumer map (struct: `analysis.rs:26-40`; duplicate struct `celt.rs:97-130`; copied field-by-field at `lib.rs:1150-1163`)

| Signal | Computed at | Consumers (file:line) | Verdict |
|---|---|---|---|
| `valid` | analysis.rs:1007 | lib.rs:505,511,823 (DTX/voice-ratio gates); celt.rs:1263,1762 | consumed |
| `tonality` | analysis.rs:945 | **NONE** (copied to celt at lib.rs:1152, never read) | Cat 2 — libopus consumes it in `compute_vbr` (tonal target boost) |
| `tonality_slope` | analysis.rs:941 | **NONE** (lib.rs:1153 copy only) | Cat 2 — libopus consumes it in `alloc_trim_analysis` (trim tilt) |
| `noisiness` | analysis.rs:1006 | **NONE** (lib.rs:1154 copy only) | Cat 2 — vestigial in libopus too; pure dead weight |
| `activity` | analysis.rs:935 | **NONE** (lib.rs:1155 copy only; fed back into MLP feature[21]) | Cat 2 — libopus consumes it in `compute_vbr` (low-activity target cut) |
| `music_prob` | analysis.rs:491,1002 | lib.rs:514 (voice_ratio, no-prev-mode arm) | consumed (exemplar) |
| `music_prob_min` | analysis.rs:517 | lib.rs:518 (hysteresis: prev = SILK/hybrid) | consumed (exemplar) |
| `music_prob_max` | analysis.rs:518 | lib.rs:516 (hysteresis: prev = CELT) | consumed (exemplar) |
| `bandwidth` | analysis.rs:1004 | lib.rs:521-532 → `detected_bandwidth` → lib.rs:664-682 (bandwidth narrowing, non-CELT modes only) | consumed (exemplar) |
| `activity_probability` | analysis.rs:1001 | lib.rs:506 (VAD ≥ 0.1 → DTX lib.rs:823-850) | consumed (exemplar) |
| `max_pitch_ratio` | analysis.rs:891-895 | celt.rs:1263-1264 (prefilter gain damping) | consumed (exemplar) |
| `leak_boost[19]` | analysis.rs:809-815 | celt.rs:1762-1765 (dynalloc follower boost) | consumed (exemplar) |

Internal-only signals fully consumed by the MLP feature vector (bfcc, spec_variability, frame_stationarity, relative_e, low_e_count — analysis.rs:947-990). `features[19]` is never assigned (stays 0.0) — faithful to libopus 1.3.1, a dead input neuron in both. All 12 tables in `analysis_data.rs` are consumed — no dead tables.

## Category 1 — Missing arms

| Site | What it decides | Current behavior | Fix candidate |
|---|---|---|---|
| celt.rs:1367-1372 `compute_vbr_target` | VBR target | Self-documented: missing tonality boost, `activity<0.4` target cut, temporal-VBR | Wire the already-shipped `analysis.tonality`/`activity` — signals exist, arm doesn't |
| celt.rs:1580 `alloc_trim_analysis` | alloc trim | `trim -= 2*tf_estimate` present, but libopus's `tonality_slope` trim term absent | Add `trim -= clamp(2*(tonality_slope+0.05))` arm |
| lib.rs:475-484 + analysis.rs:453-462 | music-prob delay compensation | `run_analysis` called with `analysis_frame_size == frame_size` → zero lookahead → the delay-comp branch is **dead code in every real encode**; the `curr_lookahead < 10` fallback (analysis.rs:498-516) always runs | Feed analysis the lookahead buffer like libopus (delay_compensation), or excise the dead branch |
| lib.rs:965-969 | Non-VoIP input filtering | Audio/LowDelay get plain f32→i16 conversion; libopus applies `dc_reject` (3 Hz) for non-VOIP | Add dc_reject arm |
| multistream.rs:7-11, 141-153 | Surround bitrate allocation | Even split (coupled=2×mono); no surround masking, no LFE special-casing | Per-stream content-driven split; LFE floor rate |
| lib.rs (absent) | Stereo width / force-mono | libopus `compute_stereo_width` + stream_channels collapse at low rates — no counterpart | Low-rate stereo→mono arm |
| lib.rs:1387-1392 | CELT PLC | decoder PLC yields silence for CELT-only loss (documented Tier-1 follow-up) | CELT PLC arm |

## Category 2 — Signals with no consumer

`tonality`, `tonality_slope`, `noisiness`, `activity` (table above): computed every frame at complexity ≥ 7, copied into `celt_enc.analysis` (lib.rs:1152-1155), read by nothing in the crate. Only 3 of 12 copied fields (`valid`, `max_pitch_ratio`, `leak_boost`) are read in celt.rs.

## Category 3 — Free syntax elements shipped as constants

| Site | Element | Current | Candidate |
|---|---|---|---|
| lib.rs:1134-1136 | Hybrid redundancy flag | Always coded 0 — mode-transition redundancy frames never emitted | Emit redundancy on SILK↔CELT transitions (quality at switches) |
| lib.rs:933-935 | `lbrr_gain_increases` | Forced to 2 if 0 | libopus derives from loss rate |

## Category 4 — Named signal shipping as constant

| Site | Signal | Current | Candidate |
|---|---|---|---|
| celt.rs:1582-1592 | Stereo LF trim tilt | Constant `+1.0` for all stereo content (PEAQ-tuned; comment admits +2 was better mid-rate but hurt 64k) | The per-band correlation `log_xc` computed 20 lines up is the obvious dispatch signal; `equiv_rate` is in hand (currently `let _ =`-discarded at 1589) |
| parallel.rs:46,77 | `warmup = 8`, `min_chunk = 4*warmup` | Constants regardless of content; warmup primes SILK/CELT but leaves the tonality state in its fast-adaptation regime (`count<10` alphas, `count<=2` forces bandwidth=20 analysis.rs:902-904) → seam frames can mode-flip vs serial | Content-aware warmup (voiced/LTP-heavy needs more; noise needs less); place seams at low-energy frames |
| lib.rs:449-453 | voice_est fallback | Application constants (Voip 115 / Audio 48 / RLD 0) when analysis is off | faithful to C; note only |

## Category 5 — Threshold-only gates ignoring content

| Site | Gate | Current |
|---|---|---|
| lib.rs:473 | Run analysis at all | `complexity >= 7 && fs >= 16000` — hard gate; below it every content signal vanishes |
| lib.rs:506 | VAD → DTX | `activity_probability >= 0.1` fixed; skips libopus's peak-energy SNR fallback (documented, adds-activity-only) |
| lib.rs:734-737 | Stereo hybrid → CELT-FB reroute | `bitrate > 28000` fixed; measured crossover on one corpus, content-blind |
| celt.rs:1971-1974 | Prefilter enable | `start_band==0 && complexity>=5 && bytes>12*ch` |
| lib.rs:623-625 | CBR hybrid cap | `bitrate < 15000` → cap WB |
| parallel.rs:77-78 | Worker cap | `total_frames / (4*warmup)` |

## Category 6 — Dead dials / unwired capability

| Site | Dial | Status |
|---|---|---|
| lib.rs:1304, 1367 | `pub hybrid_skip_celt` on OpusDecoder | **Never read anywhere in src/** — `examples\wav_test.rs:212` sets it expecting effect; silent no-op |
| lib.rs:95, 375, 411-424 | `OpusEncoder.mode` field + `enable_hybrid_mode()` | `self.mode` is write-only (encode() recomputes `mode` locally); `enable_hybrid_mode` survives only via side effects |
| celt.rs:1487, 1868 | `CeltEncoder.loss_rate` | Initialized 0, **no setter** — CELT's loss ladder (celt.rs:1252-1259: halve >2%, halve >4%, zero >8%) is dead code |
| prof.rs:63-66, 75 | `Stage::SilkNsqLpc` / `Stage::SilkNsqShape` buckets | Zero call sites (intentionally removed per doc); enum + NAMES rows remain |
| parallel.rs:21-35 | `ParallelConfig` "mirrors the knobs on OpusEncoder" | Mirrors only 4 of ~10 pub knobs — no `use_dtx`, `use_inband_fec`, `packet_loss_perc`, `signal_type`, `force_bandwidth`, `max_bandwidth`, `lsb_depth` |
| multistream.rs:108-153, 304-310 | MS encoder surface | Exposes only `bitrate` + `set_max_bandwidth`; per-stream complexity/CBR/FEC/DTX/signal unreachable |

## Category 7 — Hygiene

**Env-var table:** see silk.md census (complete crate inventory). Split-brain axes: naming (`RUSTY_OPUS_*` vs bare), API (`var` vs `var_os`), caching (cached atomics vs per-frame reads), polarity (mixed opt-in/opt-out). Worst offender: `NO_STEREO_TRIM` — a behavior-changing quality knob permanently-on behind an uncached per-frame env opt-out inside a profiled stage.

**Cargo.toml features:** exactly one — `profile`, correctly wired (zero-cost ZST off-path). No dead features.

**Doc/comment drift:**
- lib.rs:433 + lib.rs:512 say "signal_type is AUTO for us" — stale: `pub signal_type` IS honored at lib.rs:436-439.
- parallel.rs:19 "mirrors the knobs on OpusEncoder" — partial (see Cat 6).
- Pub-field default claims otherwise check out (lsb_depth, tonality gate, ParallelConfig defaults).

**repacketizer.rs / hp_cutoff.rs / modes.rs:** no content decisions. Note: repacketizer `parse_packet` hardcodes fs=48000 (line 80) while `cat` uses fs=8000 (line 253) — intentional unit trick, worth a comment.

## Already-adaptive exemplars (the crown pipeline)

analysis → decisions, all live: `music_prob`/`_min`/`_max` hysteresis-selected by prev mode (lib.rs:511-520) → `voice_ratio` → `voice_est` (lib.rs:435-455) → mode threshold voice/music interpolation (lib.rs:176-207, applied 552-563) AND bandwidth-threshold interpolation `voice_est²` (lib.rs:593-616 with per-transition hysteresis); `detected_bandwidth` narrowing floored by rate (lib.rs:664-682); `activity_probability` → DTX state machine (lib.rs:503-509, 823-850); `max_pitch_ratio` → prefilter gain (celt.rs:1263-1264); `leak_boost` → dynalloc (celt.rs:1762-1765); variable HP cutoff tracking SILK's smoothed pitch frequency (lib.rs:937-950); `hysteresis_decision` for intensity/spread (celt.rs:1497-1514); prev-bandwidth-dependent masking thresholds inside the analysis itself (analysis.rs:869-889).

## Top-5 by campaign impact

1. **Wire the four orphaned analysis signals into CELT** (celt.rs:1372 `compute_vbr_target` + 1580 `alloc_trim_analysis`): `tonality`, `activity`, `tonality_slope` are computed, validated, and shipped to the struct every frame — the arms are the only missing piece, and they are libopus's own quality levers. Cheapest quality win in the census.
2. **Zero-lookahead analysis** (lib.rs:475-484): the music-prob delay-compensation branch can never fire, so mode hysteresis always runs on the degraded "not enough lookahead" path — plausibly implicated in the known stereo-music PEAQ gap; either buffer real lookahead or accept + document.
3. **`CeltEncoder.loss_rate` unwired** (celt.rs:1868 vs lib.rs:929): `packet_loss_perc` users get SILK-side robustness but CELT's prefilter loss ladder is dead — a silent robustness hole for CELT/hybrid under loss.
4. **Stereo trim `+1.0` constant** (celt.rs:1590-1592): monotonic-with-no-single-optimum across rate per its own comment, and the dispatch signals are already computed in-scope — textbook content-adaptive-dispatch candidate.
5. **Env hygiene consolidation**: 4 uncached per-frame `env::var` reads (two in profiled hot stages), mixed polarity/naming/API, the `SILK_FLP`-vs-`use_flp` split-brain, and the silent no-op `hybrid_skip_celt` — one cached, prefixed, documented knob layer fixes all seven.
