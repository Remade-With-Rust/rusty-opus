# Changelog

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
