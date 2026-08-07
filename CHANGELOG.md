# Changelog

## 0.1.25 — 2026-08-07

### Encoder quality: analysis warm-up guard (default-on, behaviour change)

The tonality classifier needs ~20 frames to converge, and `run_analysis` is fed
zero lookahead (libopus feeds it a lookahead buffer). So `music_prob` starts at
0.13 "voice", the first ~480 ms of every stream codes as hybrid, and the encoder
then flips to CELT for the rest — paying for both the weak fixed-point hybrid
frames and the transition.

`analysis_warmup` (default **10**) ignores the classifier until it has
converged and falls back to the *application* default, which is the right
answer at both ends: Audio → 48 (music-leaning), VoIP → 115 (speech-leaning).

Measured on a 13-class × 5-rate PEAQ ladder against the previous release:

- **14 wins / 0 losses / 1 neutral** (−0.005); 15 of 65 rungs changed, the
  other 50 byte-identical.
- Mean **+0.385 ODG** on the changed rungs; best **+1.212** (silence/DTX-shaped
  speech @32k); clean speech **+0.701** @32k.
- **All 15 VoIP rungs +0.000 — bit-for-bit unchanged**, because the fallback is
  what the classifier converges to on VoIP content.

Set `RUSTY_OPUS_ANALYSIS_WARMUP=0` to restore the previous behaviour; that path
is verified byte-identical to 0.1.24 across the full corpus × rate hash matrix.

Caveat worth knowing: the gain is a *startup* artifact removal, so its ODG
magnitude scales with clip length — ~4% of a 12 s clip, ~0.16% of a 5 min
stream. The fix is unambiguously correct (it removes an audible artifact at no
bitrate cost) but the mean should not be extrapolated to long-form content.

### Fixes (all inert on the default path)

- **SILK float analysis arm**: `pitch_analysis_core_FLP`'s `*LTPCorr` output was
  discarded, so harmonic shaping and SNR adjustment in the float arm ran on a
  dead signal. The arm is opt-in (`SILK_FLP`) and its previously recorded
  "ties fixed-point" verdict is void until re-scored.
- **CELT `loss_rate` was never plumbed** from `packet_loss_perc`, leaving the
  prefilter loss ladder and the coarse-energy intra bias dead under packet
  loss. Default (0% loss) output is unchanged.
- **`lbrr_gain_increases`** hardcoded 2 instead of libopus's
  `max(7 − 0.4·loss%, 2)`; affects FEC-enabled streams only.
- Four per-frame `env::var` reads hoisted to `OnceLock` (two sat inside
  profiled hot stages).

### Also

- `RUSTY_OPUS_MODE_DWELL` — mode-dwell hysteresis, built and then **refuted by
  measurement** (it delays transitions in both directions, so on a single
  contiguous run it only postpones the exit). Default-off; kept behind the
  toggle with the refutation recorded in-tree.
- Development tooling for the content-class campaign lives in `tools/` and
  `docs/great-gate.md`; none of it ships in the published crate.

## 0.1.24 — 2026-07-29

- Fix a debug-mode `subtract with overflow` panic in tonality analysis
  (`src/analysis.rs`, frame-tonality sliding window): the C reference's
  `b-NB_TBANDS+NB_TONAL_SKIP_BANDS` int expression has a negative intermediate;
  reordered so the (identical) final index is computed without usize underflow.
  Release output is unchanged (byte-identity oracle green).
- Harden the decoder against three malformed-packet panics found by fuzzing
  (out-of-bounds / underflow in the redundancy cross-fade when a hostile
  frame count shrinks the per-frame region below 5 ms): mirror C libopus's
  packet validation — the 120 ms packet cap of `opus_packet_parse_impl`
  (OPUS_INVALID_PACKET) and `opus_decode_native`'s
  `count*packet_frame_size > frame_size` check (OPUS_BUFFER_TOO_SMALL) —
  returning errors instead of panicking.
- No change to valid-stream output: full test suite (debug + release,
  including the byte-identity bitstream oracle) green; 30k-case decode fuzz
  harness clean.
- Test-only: `oracle_bitexact`'s speech synth used `.powi(3)`, which lowers
  differently at O0 vs O2, so the debug-profile input PCM (and hashes)
  diverged from release; replaced with explicit multiplies, bit-identical to
  the release expansion (frozen hashes unchanged).
