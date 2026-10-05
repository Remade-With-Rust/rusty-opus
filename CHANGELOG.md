# Changelog

## 1.0.0 — 2026-10-04

The first stable release. The public API is now covered by semantic versioning, every
packet-facing path has been hardened against hostile input, and the codec has been
verified against `libopus` across its whole configuration space.

### Breaking changes

- **Typed errors.** Every fallible function now returns `Result<_, rusty_opus::Error>`
  instead of `Result<_, &'static str>`. `Error` is a `#[non_exhaustive]` enum mirroring the
  `libopus` error codes (`BadArg`, `BufferTooSmall`, `InvalidPacket`, `Internal`), implements
  `std::error::Error`, and `Display`s the same messages as before; `Error::code()` returns the
  `libopus` integer. Code that only formats errors (`{e}`) or uses `?` into `Box<dyn Error>`
  is unaffected.
- **Smaller documented API.** Internal codec stages (`bands`, `celt`, `silk`, `pvq`,
  `range_coder`, ...) and the `CeltEncoder`/`CeltDecoder`/`SilkResampler*` re-exports are now
  hidden from the documentation and excluded from the semver guarantee. The supported API is
  `OpusEncoder`, `OpusDecoder`, `Application`, `Bandwidth`, `SignalType`, `Error`, and the
  `multistream`, `repacketizer` and `parallel` modules.
- `OpusDecoder::decode` treats `frame_size` as a capacity (as `libopus` does) and returns the
  packet's own duration.

### Security

- `OpusDecoder::decode`, `decode_fec` and packet-loss concealment returned a panic instead of
  an error when the output buffer was smaller than the decoded frame; they now return
  `Error::BufferTooSmall`. Every panic site on the decode path has been removed.
- The x86 AVX2 PVQ search could write past its output buffer when the encoder was given
  NaN or infinite samples. Non-finite input is now neutralised before the search, and the
  kernel can no longer index past its band.
- The aarch64 NEON inverse-MDCT pre-rotation read one element past its input buffer.
- Undefined behaviour removed: stereo band coding wrote through a pointer derived from a
  shared reference (12 of 1,800 tested low-rate 12 kHz stereo configurations produce
  different, equally valid bitstreams as a result, with identical quality); NEON FFT kernels
  mixed raw-pointer and indexed access; scratch buffers formed references to uninitialised
  memory.
- `OpusEncoder::encode` panicked when given a 2-byte output buffer in VBR mode, or a
  40-120 ms frame with a buffer too small to share between its sub-frames. It now emits a
  TOC-only packet, as `libopus` does.
- In CBR mode at high bitrates with a large output buffer, `OpusEncoder::encode` planned
  SILK frames over the 1,275-byte limit: 40/60 ms SILK packets were emitted that conformant
  decoders reject, and some inputs panicked. Packets are now capped at the RFC limit, as in
  `libopus`. (Both encoder defects were found by fuzzing.)
- Development toggles that changed encoder output or wrote a tuning log to a path taken
  from the environment are now compiled only with the new, non-default `research` feature.
  A default build reads no environment variable except `RUSTY_OPUS_ISA`, and touches no
  files.
- Safe functions that hand slices to SIMD kernels now enforce each kernel's length contract.
  The range decoder, rate tables and packet parsing contain no `unsafe` code at all.
- New: [SECURITY.md](SECURITY.md) disclosure policy, [threat model](docs/threat-model.md),
  and an [inventory of all `unsafe` code](UNSAFE.md).

### Fixed

- Decoding SILK wideband/mediumband streams at 8 or 12 kHz output produced garbage (the
  decoder lacked the down-sampling FIR resampler).
- Packet-loss concealment: CELT pitch search used the wrong history window, SILK bandwidth
  expansion constant and CNG ordering differed from `libopus`, and the post-loss energy
  safety, noise PLC for hybrid streams and background-energy tracking were missing.
- In-band FEC: the encoder's redundant (LBRR) frames could not be parsed by any decoder,
  `libopus` included: independently coded frames omitted the LTP-scaling symbol, and
  stereo packets omitted the stereo prediction. They were also built by reusing the main
  frame's pulses at a raised gain on every subframe, which made recovered audio louder
  and panicked on some inputs. LBRR is now a port of `libopus` `silk_LBRR_encode`
  (re-quantized at a coarser first gain), its bits are counted against the frame's rate
  target, and a CBR packet that cannot hold it is sent without it. FEC streams are
  verified against `libopus` in 480 configurations (`tools/coverage_fec.py`).
- CBR SILK at low bitrates could overflow the packet budget and emit packets `libopus`
  rejects (36 of 102 tested 60/120 ms configurations at 12-24 kb/s). The rate loop now
  always ends in its fit-guaranteed fallback, and 60 ms packets share their budget across
  frames as `libopus` does (2/5, 3/4). Those configurations now decode everywhere, with
  higher SNR in all of them.
- A stereo stream decoded to mono ignored intensity-stereo inversion.
- The decoder rejected code-1 packets carrying two empty frames (valid per RFC 6716; they
  are now concealed), accepted code-1 packets with an odd payload length, and did not
  enforce the 1,275-byte frame limit.
- Encoder: SILK multi-frame packets (40/60 ms) carried a single frame's size; the CELT
  silence flag was set in hybrid mode; mode-transition redundancy was missing.
- The AVX2/SSE/NEON PVQ search ranked candidates with an approximate reciprocal square root
  and occasionally picked a worse codeword; it is now exact.
- AVX+FMA kernels were selected on CPUs with AVX but without FMA (Sandy/Ivy Bridge, older
  AMD), which would fault with an illegal instruction.
- The NEON pitch downsampler loaded contiguous samples where every other sample was needed,
  degrading mono CELT encoding on ARM.
- `silk_sum_sqr_shift` SIMD tails were not bit-exact; the crate did not compile for i686 or
  for its declared MSRV (1.85).
- The frame-parallel encoder panicked on wasm targets without thread support.

### Added

- Verified support for aarch64 (NEON) and wasm32 (`unknown-unknown`, `wasip1`).
- `RUSTY_OPUS_ISA=scalar|sse2|avx|avx2` caps the SIMD level on x86 and aarch64.
- `OpusDecoder::decode` performs no heap allocation after warm-up, and `OpusEncoder::encode`
  none outside SILK-to-CELT switches (previously 8-25 allocations per encoded frame, 2-4 per
  decoded frame). Multi-frame packets and multistream decoding are allocation-free too.
- `Repacketizer::reset`, `Repacketizer::out_into` and `Repacketizer::out_range_into` write
  into a caller's buffer, so a reused repacketizer does not allocate.
- Property tests for every packet-facing entry point, two new fuzz targets (repacketizer,
  multistream decoder) and committed seed corpora for all fifteen.
- Complete API documentation and a compiled quick-start example.

### Assurance

Conformance matrix (3,240 encoder configurations) and decoder sweep (every output rate and
channel layout) against `libopus`; the full test suite on x86_64, aarch64 (NEON and scalar)
and wasm32; Miri; `cargo-deny`, `cargo-audit` and `cargo-vet` (all dependencies audited).
See the hardening status table in the README.

## 0.9.1 — 2026-08-08

Development-tooling release; **no library code changed**, so encoder and decoder
output are identical to 0.9.0.

- **`rusty_alloc` is now the allocator for every bench and example.**
  `rusty_alloc-api` is added as a **dev-dependency** (never a normal one — a
  library that declares a `#[global_allocator]` hijacks every downstream
  binary's choice) and all 15 example/bench roots set it. This matters for
  measurement honesty rather than speed: the binaries that ship in
  `remade_ffmpeg_rs` run under `rusty_alloc`, so a benchmark built against the
  system allocator was not comparable to production.

  Measured effect on encode, same clips and method, with the libopus/ffmpeg
  arms as a control proving the two runs comparable (they agreed to within
  0–4.4%): **no measurable change.** Normalised against libopus the three paths
  moved −4.6% / −1.4% / **+1.4%** — one apparently better, one apparently
  worse, every move within one or two 15.6 ms timer quanta and the same size as
  the control drift. Steady-state Opus encode allocates once and reuses its
  buffers, so there is little for a faster allocator to win here; its
  documented gains come from allocation-heavy paths.

- **Benchmark harness gained a pre-flight idle gate.** It samples each
  process's CPU *delta* and refuses to run above 4 busy cores, naming what is
  in the way. Three separate runs in one day were silently ruined by load, and
  the previous check keyed on `Win32_Processor.LoadPercentage`, which was
  observed reporting 100% while the true burn was 3.4 of 24 cores.

## 0.9.0 — 2026-08-07

A version-signalling release on the road to 1.0. The codec content is 0.1.26 plus
one breaking API removal and a full published benchmark; the jump to 0.9 says the
surface is settling, not that the codec changed under you.

**Breaking:** `OpusDecoder::hybrid_skip_celt` is removed. It was a `pub` field
that nothing ever read — setting it did nothing, and an in-tree example set it
expecting an effect. Removing a silent no-op is exactly what a version bump like
this is for. No other public API changed.

### Measured against C libopus and FFmpeg's native encoder

**18 content classes × 5 bitrates**, external PEAQ ODG, compared as BD-ODG at
matched *actual* bitrate (libopus's VBR overshoots its target by 15–20% on this
corpus, so a nominal-rate comparison would hand it those bits for free):

| | vs C libopus | vs FFmpeg native `-c:a opus` |
|---|---|---|
| 13 core classes | **−0.015** (parity) | **+1.532** |
| 5 music-stress classes | **+0.009** (parity) | **+2.002** |
| classes won vs ffmpeg-native | — | **18 / 18** |

Worth stating plainly: libopus beats FFmpeg's native encoder by an even wider
margin than we do (+1.704), so that column is table stakes. **The libopus column
is the real benchmark, and there we are at parity.**

### Single-thread encode speed, per coding path

Same-method comparison — every encoder run as a process on a 300 s and a 150 s clip, reporting
the slope so startup and I/O cancel for all arms; pinned to one core, CPU time, ABBA-interleaved,
41 reps, null arm for the floor. The coding path is **verified from the output's TOC bytes**
rather than assumed:

| path | rusty-opus | C libopus | FFmpeg native | vs libopus |
|---|---|---|---|---|
| CELT, 48 kHz speech @32k | **436× realtime** | 291× | 310× | **1.50× faster** |
| CELT, 48 kHz stereo music @128k | **213× realtime** | 133× | 71× | **1.60× faster** |
| SILK, 16 kHz speech @16k VoIP | 139× realtime | **145×** | n/a | 0.96× |

Null-arm floor 0.0–2.2%. **This corrects our own prior documentation**, which claimed a ~2.9×
SILK deficit against libopus; measured properly it is within 4%. FFmpeg's native encoder is
CELT-only, so it has no SILK row — where it appears fast on speech it is doing cheaper and much
lower-quality work.

### Corpus expanded 13 → 18 classes to close measured coverage holes

`tools/corpus_coverage.py` showed the music corpus was solo acoustic classical
and nothing else: bass energy never exceeded 0.137, crest never dropped below
14 dB, and the fastest real material was 7.2 onsets/s — leaving sub-bass, loud
masters and dense fast content untested. Added, and scored:

| new class | measured property | vs libopus |
|---|---|---|
| bass-heavy electronic | bass fraction **0.634** | **+0.203** |
| fast/dense (40 hits/s) | 19.8 onsets/s | +0.014 |
| distorted rock, decorrelated stereo | flatness 0.512, L/R −0.02 | +0.002 |
| loud "master" | crest **8.0 dB** | −0.031 |
| vocal (real PD Mozart aria) | crest 23.2 dB | −0.143 |

These ladders run to **256 kb/s**, covering the transparency region the old
corpus (which stopped at 160) never reached. No failure mode appeared in any of
them, and sub-bass — the class with previously *zero* coverage — is one we win.

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
