# The Great Gate — rusty-opus instantiation

**Campaign doc + development plan.** This is the rusty-opus instantiation of the
workspace-wide Great Gate campaign
(`remade_ffmpeg_rs/_greatgate/great-gate.md` — the generalized template;
`rs_h264/docs/great-gate.md` — the reference instantiation). Finish line for
every gate: **worst content class ≤ 0, verified per class, never on average.**
Governing skills: `codec-content-adaptive-dispatch` + `codec-measurement`.
Rule-search authority: `remade_ffmpeg_rs/_greatgate/gate-calculator/` — no gate
ships without surviving the calculator's instrument audit.

Status ledger of shipped/candidate gates: [gate-ledger.md](gate-ledger.md).
Census (P0.9, 2026-08-07, 3 sweeps): [census-2026-08-07/](census-2026-08-07/).

---

## The campaign phases — status

| phase | deliverable | status 2026-08-07 |
|---|---|---|
| **P0 — Corpus** | 10-class corpus + per-clip ladder harness (external PEAQ oracle, ≥4 points) | ✅ **COMPLETE** — `tools/gen_gate_corpus.py` (deterministic, seed 0x0FF5) → `fixtures/gate_corpus/` (+ its README documenting what each class stresses and the corpus's own limitations); `tools/gate_ladder.py` (freshness-gated, ours+libopus arms). **Baseline banked: 100/100 scores**, 10 classes × 5 rates × 2 arms, plus a 600-frame/clip signal harvest. Per-class report: `tools/gate_baseline_report.py` (§5.5) |
| **P0.9 — Hygiene census** | 7-category decision-site census; hygiene fixed before fitting | ✅ **COMPLETE** — 3 sweeps (`census-2026-08-07/`), ~7 categories across lib/CELT/SILK/analysis. Safe batch applied and **PROVEN**: `gate_hash_matrix.ps1` **30/30 rows byte-identical** pre/post, conformance suite **214 passed / 0 failed**. Real defects fixed en route: D2 flp LTPCorr, D3 CELT loss_rate, D4 LBRR gain formula, plus 4 per-frame env reads hoisted. Output-changing drift fixes held for their own ladder A/B (D5) |
| **P1 — Signal audit** | One signal vector all gates read; harvest tap; per-class truth tables | ✅ **COMPLETE** — harvest tap + force-mode lever landed and byte-identical-proven; signal vector = `AnalysisInfo` (exists by construction; 4 of 12 fields orphaned). Forced-mode truth tables for silk/celt/hybrid. **The phase paid for itself twice**: validating signals before fitting killed one gate (D1: the bandwidth signal is a constant) and refuted one wrong bug report (the "broken classifier") |
| **P2 — Gate fits** | Calculator-surviving transcribed gates | ✅ **COMPLETE for this round, with a REFUSAL** — the intended gate cannot be fitted: its signal (`detected_bandwidth`) is a constant (§3.5 b2), and a rule on a constant is a rule on nothing. Machinery is built and verified end-to-end (`gate_p2_harvest.py` → calculator CSV incl. the speed pair from `gate_speedpair.ps1`/`gate_arm_cost.rs`; calculator self-check green). A refusal recorded with its evidence IS the phase's correct output — banking a rule here would have been fitting noise |
| **P3 — Missing arms** | Build the absent capabilities, then gate them | ✅ **inventory COMPLETE + ranked** (§6), and the top item is now *specified*, not just listed: the CELT silence flag, with −1.707 ODG measured, both encoders' byte spend compared, and the exact C mechanism (§5.5). Builds themselves are the acknowledged long tail |
| **P4 — Ledger + regression** | gate-ledger.md + one-run per-class regression with sign-flip CI gate | ✅ **COMPLETE** — [gate-ledger.md](gate-ledger.md) carries shipped gates, candidates, the defect table (D0-D6) and refuted hypotheses; `tools/gate_regression.py` validated against the banked baseline (all 10 classes tracked, self-diff 0.000, PASS) and enforces *worst class ≤ 0*, not the mean |

## Where to begin (the resume sequence)

**Done 2026-08-07** (kept for the method, and because each step is the standing
recipe): baseline ladder 100/100 → rebuild → hygiene hash proof **30/30
byte-identical** → conformance suite **214 passed / 0 failed** → truth-table
ladders → detector truth table → per-class baseline report.

Standing rules that cost real time to learn here:
- **Never rebuild while a ladder runs.** On Windows the running `roundtrip.exe`
  is locked (the link fails), and the freshness gate only checks at harness
  *start*, so a mid-run swap would silently mix binaries.
- **Capture reference hashes BEFORE the edit batch you intend to prove**
  (`tools/gate_hash_matrix.ps1 -Out target/hashes_pre.txt`). Post-hoc there is
  nothing to compare against.
- Keep the proof pristine: land the batch you are proving, rebuild, diff, and
  only *then* add the next change.

### Next, in order

**Done since:** the analysis warm-up guard (§4.8) — shipped default-on, 14 wins
/ 0 losses over 13 classes, VoIP bit-identical, suite 218/0. Also landed: the
VoIP/low-rate corpus classes, and `mode_dwell` (built, refuted, default-off).

**Not yet done — the standing queue:**

0. **Publish.** The warm-up win is repo-only. `rff` and every other consumer
   pin the **registry** crate (0.1.23), so nothing downstream has it yet — and
   a 0.x pin can never pick it up on its own (§7). Cut a release, or the gate
   ledger's "shipped in" column is a promise rather than a fact.
1. **Root-cause D1** — one-frame instrumented dump of `hp_ener`,
   `bandwidth_mask`, `is_masked[NB_TBANDS]` and pre/post-masking `bandwidth`
   on `lp4000`. Structurally it must be the `hp_ener→20` branch or the
   `count<=2` warm-up (§3.5 b2). Until this is fixed, no bandwidth gate can be
   fitted — the signal is a constant.
2. **Build the CELT silence arm** (§5.5) — the largest measured hole in the
   corpus (−1.707 ODG). Gates: round-trip, conformance suite, and the
   `silence_dtx` ladder rung specifically.
3. **Re-score the float SILK arm** (`SILK_FLP=1` ladder) — its recorded "tie"
   was measured with the LTPCorr bug live, so the verdict is void, not
   confirmed.
4. **Wire the orphaned analysis signals** into `compute_vbr_target` (§6 item 3;
   formula in hand) behind a cached opt-in flag, then ladder A/B.
5. Band-skip `prev` drift fix (D5), stereo-trim `music_prob` gate, then the
   rest of the P3 queue.

---

## 0. The toolkit — what was deployed, and what each thing is for

| tool | phase | what it does |
|---|---|---|
| `tools/gen_gate_corpus.py` | P0 | Builds the 10-class corpus deterministically (seed 0x0FF5) into `fixtures/gate_corpus/`. Real sources where they exist, synthesized gap classes otherwise |
| `tools/gate_ladder.py` | P0/P4 | The per-clip ODG ladder: ours and/or libopus, ≥4 rates/clip, external PEAQ oracle, records **actual** kbps. Refuses to run against a stale binary |
| `tools/gate_baseline_report.py` | P0 | Per-class BD-ODG vs libopus at matched actual rate (§5.5) |
| `tools/gate_hash_matrix.ps1` | P0.9 | Default-path byte-identical proof over corpus × rate. Run before and after an edit batch, diff |
| `tools/gate_bw_truthtable.ps1` | P1 | Validates the bandwidth *detector* against known low-pass cutoffs — the probe that found D1 |
| `tools/opus_toc_stats.py` | P1 | Mode/bandwidth histogram from any Opus stream's TOC bytes. The cross-encoder decision probe: needs no instrumentation of the other encoder |
| `tools/gate_p2_harvest.py` | P2 | Joins ladder + tap + speed pair into calculator CSVs; also prints the per-class truth table |
| `tools/gate_speedpair.ps1` + `examples/gate_arm_cost.rs` | P2 | The speed pair: deterministic stage-call **work** counter (profiled build) + pinned best-of-N **cpu_ms** (plain build), kept in separate target dirs so neither taxes the other |
| `tools/gate_regression.py` | P4 | One-run per-class regression vs the banked baseline; fails on any class regressing past the noise band |

Encoder-side levers (all env, all read ONCE at construction, all
byte-identical when unset): `RUSTY_OPUS_GATE_HARVEST`, `RUSTY_OPUS_GATE_CLIP`,
`RUSTY_OPUS_FORCE_MODE`.

## 1. Instrumentation (landed 2026-08-07, suite-green)

- **Harvest tap** — `RUSTY_OPUS_GATE_HARVEST=<csv>` (+ `RUSTY_OPUS_GATE_CLIP=<label>`)
  on `OpusEncoder`: one CSV row per frame with every signal the mode/bandwidth
  decision consumed (equiv rate, voice_est, all AnalysisInfo fields, silence/
  activity) plus the outcome (mode, bw, bytes). Env read ONCE at construction.
  **Proven byte-identical on/off** (hash-equal decode, equal payload bytes).
  Serial encoders only — the parallel path would interleave rows.
- **Force-mode lever** — `RUSTY_OPUS_FORCE_MODE=silk|celt|hybrid` pins the mode
  after the auto decision, bandwidth reconciled to a valid TOC config. The
  truth-table lever for P1/P2. Unset = byte-identical.
- Existing levers reused: `signal_type` (voice/music bias), `force_bandwidth`,
  `complexity`, `RUSTY_OPUS_COMPLEXITY` (bench).

## 2. Taxonomy instantiated — Opus signals per axis

The audio axes from the template §2, with rusty-opus's ALREADY-COMPUTED signal
for each (the P1 audit's core finding: the signal vector exists; several
consumers don't):

| axis | signal (computed at) | consumer today |
|---|---|---|
| Speech vs music | `music_prob{,_min,_max}` (analysis.rs) | ✅ mode + bandwidth thresholds (the crown exemplar) |
| Tonality vs noise | `tonality`, `tonality_slope` | ❌ **NONE — orphaned** (libopus: VBR boost, trim tilt) |
| Silence / activity | `activity_probability`, VAD `speech_activity_q8` | ✅ DTX; ❌ `activity` orphaned (libopus: VBR cut) |
| Bandwidth | `analysis.bandwidth` → `detected_bandwidth` | ✅ SILK/hybrid cap; CELT narrowing PEAQ-refuted (recorded) |
| Harmonicity / pitch | `ltp_corr_q15`, `max_pitch_ratio` | ✅ SILK shaping ladder; ✅ CELT prefilter gain |
| Transient density | CELT transient_analysis, `tf_estimate` | ✅ short blocks/tf; ❌ weak-transient + tone arms stubbed |
| Stereo correlation | `log_xc` in alloc_trim_analysis; VAD bands | 🔶 trim only; intensity start band is rate-only |
| Noisiness | `noisiness` | ❌ orphaned (vestigial in libopus too) |

Corpus classes (all in `fixtures/gate_corpus/`, 48 kHz s16, 12 s, deterministic):
clean speech, noisy speech (12 dB SNR pink), tonal music mono (guitar), stereo
music ×2 (guitar, piano), percussive/transient (castanet+glockenspiel synth),
applause (noise-like stereo), wide-stereo (Haas-decorrelated), silence/DTX
(burst-gapped speech), mixed speech+music (the variable-content class).

## 3. Function inventory — every core decision and its gate

Legend: ✅ gated · 🔶 built/opt-in/partial · ❌ no gate or missing arm.
Full per-site detail: the three census files.

### Encoder decision functions (judged by per-clip ODG ladder, ≥4 rates)

| function | unit | state | signal / plan |
|---|---|---|---|
| **SILK/CELT/hybrid mode** | frame (hysteresis) | ✅ by construction (music_prob → voice_est → threshold) | P2 target: the 24-48k band-limited-content anomaly (below) |
| **Coded bandwidth** | frame | ✅ rate×voice_est walk + detected-bw cap | Harvest shows det_bw=FB on 8 kHz-band content → P2 target |
| **DTX** | frame run | ✅ (abstention exemplar) | leave |
| **Stereo hybrid→CELT-FB reroute** | stream | 🔶 rate-only threshold (lib.rs:734) | add voice_est term — crossover is content-dependent (census #3) |
| **VBR target** | frame | ❌ tonality/activity arms MISSING (celt.rs:1367) | wire orphaned signals — cheapest quality win (census #1) |
| **alloc_trim stereo +1** | frame | ❌ constant for all stereo (celt.rs:1590) | gate on music_prob/log_xc — sign-flip suspect (census #2) |
| **Intensity start band** | frame | 🔶 rate hysteresis only | add stereo-width term (log_xc is free) |
| **Band-skip hysteresis** | frame | ❌ DRIFT: `prev=0` (celt.rs:2538) | one-line fix — hygiene batch |
| **CELT loss adaptation** | stream | ❌ DEAD: `loss_rate` never set | plumb packet_loss_perc — hygiene batch |
| **SILK complexity ladder** (n_states, survivors, shaping order, warping) | stream | 🔶 complexity-only (control_codec.rs:65-146) | per-frame dispatch on VAD/ltp_corr/pred_gain — the NSQ speed play |
| **SILK voicing threshold** | frame | ✅ content-corrected (thrhld_q13) | complexity term audit only |
| **LBRR/FEC** | frame | 🔶 copy-approximation + fixed gain bump | P3: real re-encode + loss-driven gains + activity gate |
| **NLSF interpolation** | frame | 🔶 frozen off below complexity 5 | spectral-change gate at low complexity |
| **Theta RDO (stereo)** | band | ❌ arms exist, RD loop missing | P3 build — targets the stereo-music gap |
| **SILK MS stereo** | frame | ❌ MISSING ARM: mid-only, zero pred weights | P3 flagship (largest stereo-speech headroom) |
| **Mode-transition redundancy** | transition | ❌ free syntax, never emitted (decoder handles it) | P3: emit on SILK↔CELT switches |
| **CELT silence flag** | frame | ❌ coded but never true | compute from band energies |

### Decoder throughput functions (bit-exact by law; conformance 12/12 official)

Decode-side dispatch is by construction (TOC declares mode/bw). Remaining:
CELT PLC (silence today), stereo PLC, CNG — Tier-1 robustness follow-ups, and
`hybrid_skip_celt` is a dead pub field (delete or wire).

## 3.5 P1 findings — what the first harvest + truth tables actually showed

Three results from the first live harvest (601-frame taps + partial ladder),
recorded in the order they were obtained, including the refutation:

**(a) REFUTED — "the speech/music classifier is broken."** The tap showed
`music_prob` saturating to exactly 1.000 on clean speech (median 1.000; only
the first ~5 frames read voice-like), collapsing `voice_est` to ~4.6 and
routing **576/600 frames to CELT-only** on 32 kbps speech. That looked like a
first-order bug. It is not: dumping **libopus's own TOC configs** on the same
clip and application (`-application audio`) shows libopus routes the same
content to CELT too (74-96% celt across 24/32/48k). Both encoders call this
content music under the `audio` application, so mode selection is NOT
diverging. *The reference's behaviour on your own content is the cheapest
refutation instrument available — one ffmpeg call and a 40-line TOC parser
beat a code audit of the MLP (which, checked afterwards, ports `mlp.c`
exactly).*

**(b) CONFIRMED — the divergence is BANDWIDTH, not mode.** Same TOC dump, same
clips:

| clip / rate | libopus | ours (from the tap) |
|---|---|---|
| speech_clean @24k | celt/**SWB** 74%, celt/**WB** 21% | celt/**FB** 100% |
| speech_clean @32k | celt/**SWB** 95% | celt/**FB** 100% |
| speech_clean @48k | celt/FB 96% | celt/FB 100% |
| mixed @32k | celt/FB 66%, celt/**SWB** 29% | celt/**FB** 100% |

libopus narrows the coded bandwidth at low rates; we hold Fullband and spend
bits on empty spectrum. This is exactly the site the census flagged as
"deliberately held, PEAQ-refuted twice as an always-on change"
(lib.rs:649-663 — CELT-only detected-bandwidth narrowing). The campaign's
answer to *refuted as always-on* is not to re-flip it: it is to make it a
**dispatch**, gated per content class and per rate, which is what P2 now is.

**(b2) ROOT-CAUSED — `detected_bandwidth` is a CONSTANT, and the cause is
`lsb_depth`.** Validating the signal before fitting on it (§2's law) killed the
gate as designed and produced something better. The detector truth table
(`tools/gate_bw_truthtable.ps1`, same source low-passed at known cutoffs):

| input low-pass | expected det_bw | measured |
|---|---|---|
| 4 kHz | NB | **FB (100% of frames)** |
| 6 kHz | MB | **FB (100%)** |
| 8 kHz | WB | **FB (100%)** |
| 12 kHz | SWB | **FB (100%)** |
| 16/20 kHz, unfiltered | FB | FB ✓ |

It reports Fullband for *everything*, including content low-passed at 4 kHz —
so it is not a signal at all, and the SILK/hybrid narrowing that consumes it
(`bw.min(det_bw.max(floor))`) has been a **no-op** as well.

**libopus's detector, on the same clips, IS content-driven** — so this is a
real divergence, not a shared property of the design:

| stimulus (libopus, 32k, `-application audio`) | TOC |
|---|---|
| speech_clean (true bandwidth 8 kHz) | celt/**SWB** 95% |
| same, `-cutoff 20000` and `-cutoff 0` | celt/SWB 95% — **identical**, so ffmpeg's cutoff is NOT the cause |
| speech_clean + real 14 kHz-plus noise | celt/**FB** 95% — it tracks CONTENT |

**Root cause: still open, but bounded by construction.** Two attributions were
tried and both are *refuted* — recorded so neither is re-chased:

- ~~`lsb_depth`=24 on s16 content makes the noise floor 2^16 too low.~~
  **REFUTED**: ffmpeg never calls `OPUS_SET_LSB_DEPTH`, so libopus runs at the
  same default 24 and narrows anyway. (The `lsb_depth` doc/default drift the
  census flagged is still worth tidying — it is just not this bug.)
- ~~A porting error in the HP-branch resampler.~~ **REFUTED**:
  `resampler_down2_hp` matches analysis.c character-for-character, both
  0.15063 sections included; `noise_floor` and the band-activity test match too.

What survives is a *structural* localization that needs no instrumentation:
the band loop can only raise `bandwidth` to `NB_TBANDS` (=18 → SWB), so
**FB(>18) can only come from the `hp_ener` branch that sets `bandwidth = 20`,
or from the `count <= 2` warm-up forcing**. One of those two fires on ~every
frame for us. The next step is a one-frame instrumented dump of `hp_ener`,
`bandwidth_mask`, `is_masked[NB_TBANDS]` and the pre/post-masking `bandwidth`
on lp4000 — cheap, but it needs a rebuild, so it is queued behind the hygiene
hash proof rather than rushed in beside it.

**This supersedes the "narrowing gate" as the P2 work item:** a gate keyed on a
signal that is a constant cannot be fitted. Fix the signal, re-validate against
the truth table, and only then ask whether a dispatch is still needed on top.

**(c) A sign flip is already visible in the truth table.** Forcing SILK against
the shipped auto choice, on the partial baseline:

| clip | 24k | 32k | 48k |
|---|---|---|---|
| mixed_speech_music | **+0.132** | **+0.212** | −0.293 |
| mus_guitar | — | −0.174 | −0.750 |
| mus_guitar_st | — | — | −0.427 |

SILK **beats** the shipped decision on mixed content at ≤32k and loses above
it, and loses on music everywhere. Per the house rule (a tool that wins on some
content and loses on other is a dispatch signal, never a mean), this is a gate,
not a knob — and the crossover sits in the same 24-48k band as the
non-monotonic speech RD from the ffmpeg benchmark.

## 4. P2 — first gate target (evidence in hand)

**CELT-only bandwidth narrowing, as a dispatch.** Refined by §3.5: the target
is not the mode decision (which matches libopus) but the **coded bandwidth**
held at Fullband where libopus narrows to SWB/WB. The prize is the bits we
currently spend above the content's real bandwidth at low rates — consistent
with the non-monotonic speech RD (−2.75 @25k vs −3.17 @33k) from the ffmpeg
benchmark. Candidate gate:

```
GATE  unit      = frame (bandwidth is per-frame, hysteresis already exists)
      signal    = detected_bandwidth (analysis) × equiv rate × voice_est
      threshold = rate-floored cap, population-relative where possible
      arms      = hold FB (shipped) | narrow to detected_bandwidth.max(floor)
      fallback  = hold FB — byte-identical OFF
```

Two prerequisites before fitting, both real:
1. **Is `det_bw=FB` on 8 kHz-band content correct?** The tap says Fullband on
   16k-sourced speech, whose true bandwidth is WB. Validate the detector
   against a per-class truth table (synthesize band-limited sweeps) BEFORE
   fitting a threshold on top of it — a gate on a lying signal fits the lie.
2. **A bandwidth truth-table lever.** `force_bandwidth` exists as an API field
   but has no env lever, so the ladder cannot sweep it. Add
   `RUSTY_OPUS_FORCE_BW` alongside `RUSTY_OPUS_FORCE_MODE` (after the hygiene
   hash proof, so the two changes do not contaminate each other).

The prior always-on refutation stands and is respected: this ships only if the
per-class table shows narrowing winning on the band-limited classes with **no
class worse than 0**.

Harvest → calculator contract: unit = (clip × rate); `gain` = ODG(arm) −
ODG(shipped) from forced-mode ladder runs; features = harvest-tap signal means
per clip; `work` = deterministic per-arm op counter; `cpu_ms` = pinned per-clip
encode time. Without the speed pair the calculator downgrades to HYPOTHESES
ONLY — that downgrade is the audit working.

## 4.5 P2 RESULT — the calculator's verdict, and what it actually found

The complete truth table (all three arms vs the shipped auto decision, dODG,
positive = the arm beats what we ship today):

| clip | rate | silk | **celt** | hybrid | auto picks |
|---|---|---|---|---|---|
| speech_clean | 24k | −0.353 | **+0.446** | −0.694 | celt (96%) |
| speech_clean | 32k | +0.292 | **+0.701** | −0.151 | celt |
| speech_clean | 48k | −0.134 | **+0.357** | −0.564 | celt |
| mixed_speech_music | 24k | +0.132 | **+0.118** | −0.138 | celt |
| mixed_speech_music | 32k | +0.212 | **+0.201** | −0.208 | celt |
| mixed_speech_music | 48k | −0.293 | **+0.168** | −0.936 | celt |
| mus_guitar | 32k | −0.174 | **−0.005** | −0.415 | celt |
| mus_guitar | 48k | −0.750 | **+0.020** | −1.037 | celt |
| mus_guitar_st | 48k | −0.427 | **+0.000** | −0.614 | celt |

**Forcing CELT beats the shipped decision on 9 of 9 units** (+0.70 at best,
worst −0.005 = noise) **and is cheaper on both instruments** (7890 vs 8076
stage calls; 23.6 vs 26.7 ms pinned). The auto path is already ~96% CELT — the
difference is the sporadic **~4% hybrid frames and the mode transitions they
force**. That thrash is costing up to 0.7 ODG on speech.

**Calculator verdict (harvest audit fully green — VERDICT-CAPABLE, the first in
this campaign): NO RULE, and that is the finding.** The exhaustive search
generated *zero* predicates: no feature separates wins from losses because
there are no losses. Per the house rule, a dispatch exists to route a tool that
wins on some content and loses on other; a tool that wins uniformly is not a
gate, it is a **straight fix**. So the correct next step is the template's own
prerequisite — *force-on-everywhere must nearly tie the anchor on the full
ladder before a dispatch is built on it* — i.e. run force-CELT across all 10
classes × 5 rates and check worst-class ≤ 0. **That run is in flight**
(`target/ladder_forcecelt_full.csv`). Two possible outcomes, both actionable:

- holds corpus-wide → fix the mode hysteresis outright (no gate needed);
- some class (low-rate speech, silence, noisy speech) loses → *then* fit the
  dispatch, and the losing class is the separation the calculator needs.

### 4.6 The force-on result — it HOLDS, and it is large

50 rungs, judged by `gate_regression.py` against the banked baseline:
**18 wins, 0 losses, 1 neutral (−0.005)**; the other 31 rungs are
**byte-identical** because the auto path was already 100% CELT there — so the
neutral end satisfies the fallback law literally, not approximately.

| class | rungs changed | dODG on those rungs |
|---|---|---|
| silence_dtx | 16/24/32/48k | +0.223 / **+1.029** / **+1.212** / +0.782 |
| speech_clean | 16/24/32/48k | +0.130 / +0.446 / **+0.701** / +0.357 |
| percussive | 32/48k | **+0.650** / +0.496 |
| speech_noisy | 16/24/32/48k | +0.185 / +0.269 / +0.364 / +0.062 |
| mixed_speech_music | 24/32/48k | +0.118 / +0.201 / +0.168 |
| mus_guitar | 32/48k | −0.005 / +0.020 |

Mean on changed rungs **+0.390**, best **+1.212**, and bitrate goes slightly
*down* on 17 of 19 (−0.4..−0.9 kbps) — this is not quality bought with bits.

**Standing vs libopus, recomputed:**

| | was | now |
|---|---|---|
| mean BD-ODG | −0.212 | **−0.034** (near parity) |
| worst class | −1.707 | **−0.774** |
| classes we win | 2/10 | 3/10 |
| speech_clean | −0.531 | **−0.109** |

**Mechanism confirmed, 100% agreement, zero counterexamples:** "the auto path
emitted non-CELT frames on this rung" predicts "force-CELT changed the score"
19/19 and 31/31. The non-CELT fraction is only **3.8-8.8%** of frames, yet
costs up to 1.2 ODG — far more than those frames' own coding could account for.
The damage is the **transitions** (CELT state resets / prefill discontinuities
at every flip), not the hybrid frames themselves.

### 4.7 The mechanism, corrected — it is analysis WARM-UP, not thrash

§4.6 said the loss came from "isolated single-frame mode flips". **That was
wrong, and building the obvious fix disproved it in ten minutes.** Mode-dwell
hysteresis (require a proposed change to persist N frames) made things *worse*:
non-CELT frames went 24 → 25 / 26 / 28 / 33 for N = 2 / 3 / 5 / 10 — exactly
+(N−1), the signature of delaying the exit from **one long run** rather than
suppressing many short ones.

The run structure says it plainly: the non-CELT frames are a single contiguous
block, **frames 0-23**, on every clip measured. The cause is the tonality
classifier warming up — `music_prob` starts at 0.13 (voice-like) and only
reaches its steady state around frame 20, so `voice_est` reads "voice" and the
encoder runs hybrid for the first ~480 ms, then flips to CELT for the remaining
576 frames and never returns. libopus does not have this problem because it
feeds its analysis a **lookahead** buffer, so the classifier is converged before
the first coded frame; we call `run_analysis` with zero lookahead (the census
already flagged that the delay-compensation branch is dead code for us).

**The fix that works — `analysis_warmup`:** ignore the classifier's verdict for
its first N frames and fall back to the *application* default, which is the
right answer at both ends (Audio → 48, music-leaning; VoIP → 115,
speech-leaning). Threshold N = 10 is taken from the analysis's own
fast-adaptation window (`count < 10` in analysis.rs), not fitted to our corpus.

Verification on speech_clean @32k, one clip, three numbers:

| arm | ODG | non-CELT frames |
|---|---|---|
| baseline (warmup 0) | −3.1438 | 24 / 600 |
| **warmup = 10** | **−2.4429** | **0 / 600** |
| force-CELT (the ceiling) | −2.443 | 0 / 600 |

The guard recovers the force-CELT gain **to four decimal places** while leaving
SILK fully available — which is exactly the shippable form the ceiling
experiment could not be.

**How much should you believe "+0.70 ODG"?** Put the three arms side by side on
speech_clean @32k: all-hybrid −3.295, baseline −3.144, all-CELT −2.443. The
baseline is **96% CELT frames** yet scores far closer to all-hybrid than to
all-CELT — 24 frames out of 600 move the score across 0.70 of the 0.85 gap
between the two pure arms. That is disproportionate to their share, and the
reason is that PEAQ's ODG is built partly on worst-case-sensitive model
variables (peak probability of detection), not a flat average.

So read the number as: **there is a clearly audible artifact in the first half
second, and the guard removes it.** That is a real defect worth fixing and the
per-class ladder is the right gate for it — but "+0.70 ODG" should not be
re-quoted as a 0.70 improvement in average quality across the stream. The
honest claim is a startup artifact eliminated, measured by a metric that
(correctly) punishes audible localized damage. It also means part of the gain
is *avoiding our weak fixed-point hybrid path* rather than fixing it; the
hybrid weakness remains its own open item.

### 4.8 SHIPPED — `analysis_warmup = 10` is now the default

Full 13-class regression, 65 rungs, judged by `tools/gate_regression.py`
against the banked baseline. **PASS on every class.**

| | |
|---|---|
| rungs changed | 15 of 65 (50 byte-identical) |
| wins / losses | **14 / 0** (1 neutral at −0.005) |
| mean on changed rungs | **+0.385** |
| best | **+1.212** (silence_dtx @32k) |
| worst any class | **−0.005** (mus_guitar @32k, inside the noise band) |
| VoIP classes | **+0.000 on all 15 rungs — bit-for-bit unchanged** |

| class | rungs gained |
|---|---|
| silence_dtx | +1.212 / +1.029 / +0.782 |
| speech_clean | +0.701 / +0.446 / +0.357 |
| speech_noisy | +0.364 / +0.269 / +0.062 |
| mixed_speech_music | +0.201 / +0.168 / +0.118 |
| percussive | +0.044 |
| mus_guitar | +0.020 / −0.005 |

The VoIP result is the one that mattered most: the guard is **inert** there,
because its fallback (`voice_est = 115`, speech-leaning) is exactly what the
classifier converges to on VoIP content. The path the force-CELT ceiling
experiment could not speak for is not merely unharmed — it is unchanged.

**Free reproducibility check:** a `--clips` substring overlap caused five rungs
to be scored twice by two independent processes. All five agreed to the last
digit, confirming encode + PEAQ are deterministic across processes.

**Promotion to default (architecture law 7, opt-in → default-on):** the default
is now 10. `RUSTY_OPUS_ANALYSIS_WARMUP=0` is the neutral end and was verified
to reproduce the pre-change output on **all 39** hash-matrix rows; the flip
itself changes 13 of those 39, which is the intended behaviour change. Suite:
**218 passed / 0 failed** (four new `reconcile_bandwidth` branch tests
included).

**One caveat to carry forward:** the corpus is 12-second clips, so a 480 ms
startup artifact is ~4% of each clip. On a five-minute stream the same fix
touches ~0.16% of frames and the ODG delta will be far smaller. The fix is
still unambiguously correct — it removes an audible artifact and costs nothing
— but do not extrapolate "+0.385 mean" to long-form content.

> **The force-CELT experiment below was NOT SHIPPABLE — kept for its evidence.**
> `RUSTY_OPUS_FORCE_MODE=celt` disables SILK outright. This corpus is entirely
> `audio` application at ≥16 kbps; it does not test VoIP application, 8-12 kbps
> speech, DTX-on, or FEC — all of which need SILK, and all of which this result
> says nothing about. The shippable change is **mode-dwell hysteresis that
> suppresses isolated mode flips** (keep the mode unless the new decision
> persists for N frames), which captures the same prize without removing an
> arm. Closing the corpus gap (a voip/low-rate class) is a prerequisite to
> shipping it.

**Instrument caveat, recorded because it will mislead someone otherwise:** the
`work` counter (stage-scope call counts) and `cpu_ms` **disagree in rank across
modes** — SILK shows the FEWEST calls (4800 vs CELT's 7800) but the HIGHEST
time (82 ms vs 24 ms), because its NSQ costs far more per call. Call counts are
a valid work proxy *within* an arm, not *across* arms with different per-call
costs. For cross-mode comparisons read `cpu_ms`; do not let the counter's
"SILK is cheapest" reading stand unqualified.

## 4.9 Round 2 (2026-08-07 evening) — release, D1 closed, three bricks

**Published: rusty-opus 0.1.25** (crates.io), commits pushed. The warm-up win is
now genuinely downstream, not merely merged: rff's `Cargo.lock` resolves 0.1.25
and `rff -c:a opus` emits `celt/FB 100%` on speech where it previously carried
4% startup hybrid. Note the version-parity law needed a correction in practice —
0.1.24 was *already* published and rff pinned `^0.1.24`, which **does** accept
0.1.25 (caret on `0.1.x` admits later patches), so a patch bump was consumable.
The "a 0.x pin can never pick up the fix" hazard applies to `0.1`→`0.2`.

### D1 CLOSED — and it *was* `lsb_depth`, reversing my own refutation

`RUSTY_OPUS_BW_DEBUG=1` on lp4000, per frame:

```
bw_raw=18  bw_premask=20  bw_final=20
hp_e=7.4e-10   thresh=1.2e-13   masked_hp=0   noise_floor=7.6e-17
```

`hp_e` sits **6000× above** the threshold, so the `hp_ener` branch forces
`bandwidth = 20` (FB) every frame; the band loop independently saturates to its
own ceiling of 18 for the same reason; the masking rescue never fires. Both
paths trace to `noise_floor = (5.7e-4 / 2^(lsb_depth−8))²` with `lsb_depth = 24`.
At 16 the threshold becomes 2.4e-8 — above the measured `hp_e` — and the
detector starts tracking content: lp4000→NB, lp8000→WB, lp12000→SWB,
unfiltered→SWB/FB.

I had refuted `lsb_depth` earlier, and that refutation was **right about the
question it asked and wrong as generalised**: ffmpeg never calls
`OPUS_SET_LSB_DEPTH`, so lsb_depth cannot explain why *libopus* narrows and we
do not — but it is squarely the operative variable on *our* side. (The
libopus-divergence question is still open.) Shipping `lsb_depth = 16` for
s16-sourced input is a separate brick: it also moves the dynalloc/leak_boost
noise floors, so it needs its own ladder.

### The float SILK arm is a DISPATCH, not a tie

Re-scored after the LTPCorr fix on the voip classes (the only place SILK is the
working arm), rate-matched BD-ODG: `voip_speech_noisy` **+0.082**, `voip_mixed`
−0.010, `voip_speech` **−0.123**. It stays default-off, but the old "ties
fixed-point" verdict is now something more useful — a per-class **sign flip**,
i.e. a dispatch candidate keyed on noisiness. Weak evidence (3 classes, and PEAQ
saturates on narrowband speech), so it is a lead, not a bankable gate.

### A new instrument: rate-matched BD in the regression harness

Both new bricks MOVE the bitrate — the silence flag spends 28% fewer bits on the
DTX clip (32.4→23.3 kb/s), the tonality boost spends ~1.6% more on tonal music.
Per-rung ODG would price *the bits they moved* rather than the efficiency they
bought, so `gate_regression.py --bd` interpolates both ladders on log(actual
kbps) and reports per-class BD-ODG at matched rate. Any future
bit-redistribution knob must be judged this way.

### Process note, recorded because it nearly cost a dataset

I rebuilt the encoder while a ladder was running — the exact mistake documented
at the top of this file. The run spanned two binaries that differed only in
default-off code, which is an assumption rather than a proof, so the run was
**discarded and re-run against a pinned binary copy**. The pinning lever
(`RUSTY_OPUS_ROUNDTRIP`) exists precisely so concurrent ladders can be held to
one provable encoder.

## 5. P0.9 hygiene batch — fix BEFORE any fitting

1. ✔ APPLIED 2026-08-07 — **flp.rs** `let _ = ccmax;` dropped C's `*LTPCorr`
   update; the float arm's harmonic shaping ran on a dead signal. Fixed
   faithful to pitch_analysis_core_FLP.c (incl. `*ltp_corr = 0` on the
   no-candidate path). Default-off arm → default path byte-identical.
   **Follow-up open: re-score the float arm** (resume step 7).
2. ⏳ HELD — **celt.rs:2538** pass `last_coded_bands` as `prev` (band-skip
   hysteresis drift vs libopus). Changes default output → own brick + ladder A/B.
3. ✔ APPLIED — **loss_rate plumbing**: `celt_enc.loss_rate = packet_loss_perc`
   per frame; default (loss 0) path unchanged. Also LBRR gain bump restored to
   libopus's `max(7 − 0.4·loss%, 2)` (was hardcoded 2; FEC-off unaffected —
   FEC-on streams move toward libopus parity, re-run the FEC harness).
4. ✔ APPLIED (caching only) — per-frame env reads hoisted to `OnceLock`:
   `NO_STEREO_TRIM`, `CELT_PF_OFF`, `SILK_FLP`, `SILKD`. Naming/polarity
   standardization (`RUSTY_OPUS_*`, `var_os`) and the `SILK_FLP`-vs-`use_flp`
   split-brain: still open (naming change breaks existing scripts — decide
   deliberately).
5. ✔ APPLIED — stale docs: lib.rs "signal_type is AUTO" ×2 corrected.
   parallel.rs:19 "mirrors the knobs" still open.
6. ⏳ OPEN — dead dials: `hybrid_skip_celt` (silent no-op set by wav_test!),
   `alg_quant_qext` family, `stereo_merge_*` twins, `haar1_avx` — prune or wire.
7. ⏳ OPEN — λ/threshold constant duplication (control_fixed vs
   tuning_parameters; define.rs DTX threshold) — single source.

Applied items compile (`cargo check` clean) but are **not yet rebuilt, suite-run,
or hash-proven** — that is resume steps 2-3, blocked behind the baseline run.

## 5.5 The P0 baseline — per-class, ours vs libopus (BD-ODG at matched ACTUAL rate)

Regenerate with `tools/gate_baseline_report.py`. Positive = we win.

| class | BD-ODG | verdict |
|---|---:|---|
| applause_st | **+1.101** | we win big (noise-like content) |
| percussive | **+0.169** | we win |
| wide_stereo | −0.073 | parity |
| speech_noisy | −0.129 | libopus ahead |
| mus_guitar_st | −0.162 | libopus ahead |
| mus_guitar | −0.192 | libopus ahead |
| mus_piano_st | −0.217 | libopus ahead |
| mixed_speech_music | −0.374 | libopus ahead |
| speech_clean | −0.531 | libopus ahead |
| **silence_dtx** | **−1.707** | **worst class by 3×** |

Mean −0.212 across a **2.8 ODG spread** — the corpus law earning its keep: a
single average would have hidden both a +1.1 win and a −1.7 hole.

### The VoIP/low-rate classes (added 2026-08-07, closing the corpus gap)

| class | BD-ODG vs libopus | note |
|---|---:|---|
| voip_speech_noisy | −0.021 | parity |
| voip_speech | −0.200 | |
| voip_mixed | −0.416 | worst of the three |

**Caveat that must travel with these numbers:** PEAQ is a wideband/fullband
metric and saturates on narrowband speech. Across the whole 8→32 kb/s ladder
*both* encoders are nearly flat (ours −3.86→−3.45, libopus −2.22→−2.15), which
reads as the metric running out of resolution rather than either codec failing
to spend bits. Treat these classes as a **no-regression tripwire** for
mode-selection changes; use a speech-domain metric (POLQA/PESQ) before claiming
any positive result on the low-rate speech path.

### The −1.707: we spend 69% of full rate on SILENT frames; libopus spends 3%

Measured, same clip, same nominal rate:

| | our bytes/frame on inactive frames, vs active | libopus |
|---|---|---|
| 16k | 32.3 / 48.7 = **0.66** | — |
| 32k | 64.9 / 94.4 = **0.69** | small packets 3.0 B vs 102.4 B = **0.029** |
| 64k | 131.9 / 180.4 = **0.73** | — |

240 of 600 frames in that clip are silence, so roughly **a quarter of the whole
bit budget is spent coding nothing**, and at matched rate libopus spends those
bits on the speech bursts instead. libopus's small packets are 3 bytes and 35%
of its stream — that is not DTX (off in both encoders), it is the **CELT
silence flag**, which the census already found us shipping as a constant
(`celt.rs:2104 let silence = false;`) even though our *decoder* implements it.

Mechanism, from celt_encoder.c (exact):
```c
sample_max = MAX32(st->overlap_max, celt_maxabs16(pcm, C*(N-overlap)/st->upsample));
st->overlap_max = celt_maxabs16(pcm + C*(N-overlap)/st->upsample, C*overlap/st->upsample);
sample_max = MAX32(sample_max, st->overlap_max);
silence = (sample_max <= (opus_val16)1/(1<<st->lsb_depth));   /* float build */
...
if (silence) {                       /* VBR: send only the minimum */
   effectiveBytes = nbCompressedBytes = IMIN(nbCompressedBytes, nbFilledBytes+2);
   total_bits = nbCompressedBytes*8;  nbAvailableBytes = 2;
   ec_enc_shrink(enc, nbCompressedBytes);
   tell = nbCompressedBytes*8;  enc->nbits_total += tell - ec_tell(enc);
}
```
Implementation notes for whoever lands it: `rc.shrink()` already exists
(`range_coder.rs:297` = `ec_enc_shrink`), and the VBR sizing/shrink machinery is
at `celt.rs:2401-2481`. The subtle part is that libopus does **not** early-return
on silence — it runs the whole frame path on a 2-byte budget so encoder and
decoder stay in lockstep. That makes this a bitstream-changing brick needing the
full round-trip + conformance + per-class ladder gates, not a quick patch. It
also needs `overlap_max` as new encoder state.

## 6. P3 — missing-arms queue (ranked)

0. **CELT silence flag** — the measured −1.707 ODG hole above, mechanism and C
   in hand, decoder side already implemented. Highest evidence-to-effort ratio
   in the campaign.
0b. **`detected_bandwidth` is a constant (D1)** — not a missing arm but the
   input that makes an existing arm (bandwidth narrowing) functional at all;
   root cause still open (§3.5 b2).
1. **SILK MS stereo** (side channel + pred weights) — stereo speech/hybrid headroom.
2. **Theta RDO** outer loop (arms exist) — stereo music gap.
3. **VBR target tonality/activity terms** — technically an arm, cheap, census #1.
   Formula from celt_encoder.c `compute_vbr`, in hand:
   `tonal = max(0, tonality − 0.15) − 0.12;  target += (coded_bins<<BITRES)·1.2·tonal`
   (plus a `pitch_change` term we do not track yet — omit it, stay a faithful
   subset). Land behind a cached opt-in flag so the A/B is drift-free in ONE
   binary, then flip the default only if worst class ≤ 0.
4. **Float pitch core LTPCorr fix + float-arm re-score** — may unblock the
   float SILK arm (the 24-48k speech gap's root per the conformance campaign).
5. **Mode-transition redundancy frames** — free syntax, decoder-proven.
6. **LBRR real re-encode** + loss-driven gains.
7. **Weak-transient + tone analysis** (CELT stubs).
8. **lp_variable_cutoff driver** (bandwidth-transition smoothing no-op).
9. **CELT PLC / stereo PLC / CNG** (decoder robustness tier).
10. **48k→8k/12k encode resamplers** (true NB/MB from 48k API) — note NB/MB
    modes were PEAQ-refuted as a *quality* play; this is completeness only.
11. **Stereo width / force-mono at low rates** (libopus compute_stereo_width).
12. **Surround masking + LFE allocation** (multistream).

## 7. The downstream loop — version parity law

rff consumes **registry** rusty-opus (0.1.23 published; this tree is ahead).
Every capability row in the ledger carries the published version it landed in;
a downstream 0.x pin CANNOT pick up 0.(x+1) fixes — check `cargo tree` in the
consumer before believing any content-shaped failure report (the h264 campaign
burned a session on exactly this).

## 8. Measurement law (binding)

Per `codec-measurement` + template §6: pinned CPU + ABBA + per-session null arm
for speed; per-clip ODG ladder (≥4 rates, external PEAQ — `tools/quality/` in
remade_ffmpeg_rs) for quality; work-count parity; |z|>2; the conformance suite
(`cargo test --release`, 12/12 official vectors among them) for any output-
changing edit; `tools/gate_ladder.py` prints the exe mtime and refuses stale
binaries. PEAQ_python full-scale-input crash is fixed (2026-08-07) — a clip
with a hard-clipped sample no longer aborts the oracle.
