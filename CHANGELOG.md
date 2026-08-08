# Changelog

## 0.1.26 — 2026-08-07

### CELT per-frame silence flag (default-on, behaviour change)

Digitally-silent frames were coded at roughly the full frame cost: measured on a
DTX-shaped clip we spent **69%** of the active-frame bitrate on silence where
libopus spends **~3%**, so about a quarter of the bit budget went on coding
nothing. The flag is a bitstream element the format already carries and our
*decoder* already implemented — the encoder simply never set it.

`silence` is now detected over the frame plus the previous overlap tail (new
`overlap_max` state, mirroring `st->overlap_max`), the flag is coded, and on VBR
the range coder is shrunk to filled+2 bytes with the remaining budget marked
spent — the encoder-side mirror of the decoder's existing
`nbits_total += total_bits - tell`.

Judged by rate-matched BD-ODG across 13 content classes (the flag *moves* the
bitrate, so per-rung ODG would price the bits it saved rather than the
efficiency it bought):

- **mean +0.198, worst class exactly +0.000** — no class regresses at all.
- silence/DTX-shaped speech **+1.647**, clean speech **+0.390**, percussive
  +0.181, noisy speech +0.204, mixed speech+music +0.124.
- All VoIP classes and all pure-music classes: +0.000.

Independently validated rather than only self-round-tripped: **ffmpeg/libopus
decodes our stream to equal quality (−3.8740 vs −3.8747) using 28% fewer bits**,
and our packet profile now matches libopus's shape (236 small packets averaging
5.0 B against libopus's 214 × 3.0 B). CBR is unaffected — packet length stays
exact, decode is clean, quality unmoved.

`RUSTY_OPUS_SILENCE_FLAG=0` restores the previous behaviour, verified
byte-identical across the full corpus × rate hash matrix.

**Scope limit worth knowing:** the test is `sample_max <= 1/2^lsb_depth`, i.e.
true digital silence, exactly as in libopus. Streams whose quiet passages are
genuinely zeroed — DTX/VAD-gated telephony, edited or noise-gated material —
get the full benefit; raw microphone audio with a room-noise floor gets none.

### Also

- `RUSTY_OPUS_TONAL_VBR` — libopus's tonality VBR boost, implemented but left
  **opt-in**: 13-class BD is mean +0.114 yet it loses on transient content
  (percussive −0.045, applause −0.025, piano −0.017) while winning on speech and
  silence. That win-speech/lose-transient split is a dispatch signal, not a
  knob to switch on globally; it likely needs the `pitch_change` term we do not
  track, or a transient veto.
- `RUSTY_OPUS_LSB_DEPTH` — exposes the analysis input depth. At the float-API
  default of 24 the analysis noise floor sits 2^16 too low for s16-sourced
  material, which pins the bandwidth detector at Fullband for *all* content
  (verified: 4/6/8/12 kHz low-passed input all report FB). At 16 the detector
  tracks content again. Shipping that default is a separate change — it also
  moves the dynalloc/leak_boost floors.
- `examples/encode_ogg.rs` — writes real Ogg-encapsulated `.opus`, so our
  bitstream can be handed to an independent decoder.

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
