**rusty-opus 1.0.0** — the first stable release of the pure-Rust Opus codec.

- Stable, semver-covered API with typed errors (`rusty_opus::Error`).
- Hardened against hostile input: no panics on malformed packets, no `unsafe` in any
  parsing or entropy-decoding code, memory-safety fixes in SIMD kernels.
- Verified on x86_64, aarch64 (NEON) and wasm32; conformance-tested against `libopus`,
  now including in-band FEC streams, which earlier versions encoded incorrectly.
- Allocation-free streaming decode; the encoder allocates only when switching from SILK
  to CELT coding.

Full details: [CHANGELOG.md](https://github.com/Remade-With-Rust/rusty-opus/blob/v1.0.0/CHANGELOG.md).
Attached: the packaged crate, a CycloneDX SBOM, and a build-provenance attestation.
