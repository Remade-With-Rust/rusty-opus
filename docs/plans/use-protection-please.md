# rusty-opus — hardening audit

**Standard**: Remade-With-Rust recursive hardening process — see the skill's `STANDARD.md`
**Registry**: 41 gates / 12 phases (`use-protection-please` v1)
**Unit**: `.` — library crate (`rusty-opus` on crates.io)
**Tier**: critical-path — decodes untrusted Opus packets from network peers and files
**Mirrors**: crates.io page `https://crates.io/crates/rusty-opus` (renders this README at publish; the block uses an absolute link). Re-render every mirror in the same pass as this file.
**Compliance**: none — a codec library that stores, transmits and logs no personal data and holds no keys; frameworks bind on the applications embedding it
**Architect**: [@Ttimmahlax](https://github.com/Ttimmahlax)
**Audit depth**: deep
**Audited**: 2026-10-04 by Claude (Opus 5.5) for the maintainer · **Next review**: 2027-01-04

> Source of truth for this unit's hardening status. The README's status table is
> **generated from this file** — edit here, then run:
> `python tools/render_hardening_table.py --plan docs/plans/use-protection-please.md --readme README.md --link https://github.com/Remade-With-Rust/rusty-opus/blob/v1.0.0/docs/plans/use-protection-please.md`

**Status tokens**: `Completed` (evidenced pass) · `Scheduled` (owner + date in Target) ·
`Incomplete` (not done, or not evidenced) · `N/A` (out of tier — reason required in
Evidence; excluded from the totals).

---

## Threat sketch

*Assets* — memory safety and availability of every host process that decodes Opus (VoIP clients, media servers, browsers via wasm); integrity of decoded audio; the published crate's supply chain.
*Adversaries* — A1 remote packet sender (controls packet bytes, sizes, order, loss); A2 hostile PCM source (NaN/Inf/denormal input to the encoder); A3 misconfiguring caller; A4 supply-chain attacker.
*Highest-value attack path* — a crafted packet steering a SIMD kernel's raw-pointer loop out of bounds. Defences: framing validation → safe range decoder → stage length checks → per-kernel `# Safety` contracts enforced by asserts → scalar-twin oracles on x86_64 and aarch64 → fuzzing + Miri.
*Full model* — [docs/threat-model.md](../threat-model.md)

---

## Checklist

`★` = v1.0.0-blocking. Full probe and pass criteria per gate: the skill's `CHECKLIST.md`.

### Phase 0 — Threat modeling

| ID | Gate | Status | Evidence | Target |
|---|---|---|---|---|
| H-01 | ★ Threat model documented and linked from README | Completed | `docs/threat-model.md`: assets, 4 adversaries, trust boundaries, STRIDE table, attack path; linked from README "Security" and SECURITY.md | |
| H-02 | Threat model revisited after last major change | Completed | Model dated 2026-10-04, written after this pass's API (typed `Error`) and `unsafe` changes; next review 2027-01-04 | |

### Phase 1 — Toolchain

| ID | Gate | Status | Evidence | Target |
|---|---|---|---|---|
| H-03 | Toolchain pinned (`rust-toolchain.toml`) | Completed | `rust-toolchain.toml`: channel 1.98.0, components rustfmt+clippy, profile minimal; MSRV 1.85 in Cargo.toml and checked in CI (`msrv` job) | |
| H-04 | Committed `.cargo/config.toml` hardening defaults | Completed | `.cargo/config.toml`: force-frame-pointers on all targets; Linux x86_64/aarch64 `-z relro -z now -z noexecstack`; MSVC `/CETCOMPAT`; full test suite run under it (246/246) | |
| H-05 | ★ Release profile hardened (overflow-checks, LTO, panic policy) | Completed | `[profile.release]` overflow-checks=true, lto="fat", codegen-units=1; library stays unwind-safe (no shipped binary); separate `[profile.bench]` for perf numbers; `cargo test --release` 246/246 under it | |
| H-06 | Security toolchain available to CI and developers | Completed | `.github/workflows/ci.yml` + `scheduled.yml` install cargo-deny 0.19.9, cargo-audit 0.22.2, cargo-vet 0.10.2, cargo-fuzz 0.13.2, cargo-careful 0.4.10, semgrep 1.173.0 at pinned versions via SHA-pinned `taiki-e/install-action` | |

### Phase 2 — Supply chain

| ID | Gate | Status | Evidence | Target |
|---|---|---|---|---|
| H-07 | ★ `Cargo.lock` committed | Completed | Removed from `.gitignore` and committed with the v1.0.0 release commit (`git ls-files Cargo.lock`); CI builds with `--locked` | |
| H-08 | ★ `deny.toml` policy present and enforced | Completed | `deny.toml` (advisories deny, yanked deny, permissive license allow-list, wildcards deny, unknown registry/git deny); `cargo deny check` → advisories ok, bans ok, licenses ok, sources ok; CI `supply-chain` job | |
| H-09 | ★ Vulnerability scan clean (`cargo audit`) | Completed | `cargo audit --deny warnings` exit 0 against 1290 advisories (2026-10-04); weekly re-run in `scheduled.yml` | |
| H-10 | ★ `cargo vet` coverage complete | Completed | `cargo vet` → "Vetting Succeeded (76 fully audited)", 0 exemptions: imported audits (mozilla, google, bytecode-alliance, isrg, zcash, embark), publisher trust as used by those sets, 11 dev-only `safe-to-run` side-effect reviews recorded in `supply-chain/audits.toml`; zero runtime deps | |
| H-11 | Unsafe inventory measured and trending down (geiger) | Completed | `docs/audit/geiger-2026-10-04.txt` (69/71 fns, 7436/7642 exprs, all first-party); this pass removed `unsafe` entirely from range_coder, rate, pitch_analysis and deleted 7 dead unsafe kernels | |
| H-12 | ★ SBOM generated and published with releases | Completed | `release.yml` attached `rusty-opus-sbom.json` (CycloneDX, `cargo cyclonedx`) to the [v1.0.0 release](https://github.com/Remade-With-Rust/rusty-opus/releases/tag/v1.0.0), 2026-10-04 | |
| H-13 | Git deps pinned; no unknown registries or sources | Completed | No git or path dependencies (zero runtime deps); `deny.toml [sources]` unknown-registry/unknown-git = deny | |
| H-14 | Dependency freshness reviewed, human-in-the-loop updates | Completed | `.github/dependabot.yml` (cargo weekly, fuzz monthly, actions weekly) → PRs need review + green CI; `cargo outdated` 2026-10-04: only dev-dep `rusty_alloc-api` 0.4→2.2 (major; deferred to a reviewed PR) | |

### Phase 3 — Code level

| ID | Gate | Status | Evidence | Target |
|---|---|---|---|---|
| H-15 | ★ Workspace lint policy set and clean | Completed | `[lints]` in Cargo.toml: clippy all=deny, pedantic+nursery=warn, `undocumented_unsafe_blocks=deny`, every allow carries a rationale; `cargo clippy -- -D warnings` clean on x86_64 + i686 (all targets) and aarch64, wasm32, wasip1 (lib) | |
| H-16 | ★ `unsafe` isolated, SAFETY-commented, inventoried | Completed | `unsafe` confined to 12 SIMD-kernel files (semgrep rule `unsafe-only-in-simd-kernels`); every block has `// SAFETY:` (clippy deny), every `unsafe fn` a `# Safety` contract; `UNSAFE.md` lists all 173 items, regenerated + `--check`ed in CI | |
| H-17 | Arithmetic safety explicit | Completed | overflow-checks in release + debug test suite (236/236 with checks); range coder / rate tables use checked indexing; explicit `wrapping_*` in fixed-point code; untrusted packet lengths validated against RFC limits before use | |
| H-18 | ★ No `unwrap`/`expect`/panic on untrusted paths; typed errors | Completed | Typed `rusty_opus::Error` (libopus codes) replaced `&'static str`; every decode-path panic site removed (aux decoder, framing, NLSF codebook made non-optional, CELT MDCT, dual-stereo) and three fuzz-found encoder panics (2-byte output buffer, now a TOC-only packet; CBR frames planned over 1275 bytes, now capped; LBRR gain-index overflow, LBRR re-ported), all as in libopus; `tools/check_invariants.py` forbids unwrap/expect on decode modules; property tests `decode_never_panics` etc. | |
| H-19 | Input validation — external bytes treated as hostile | Completed | Framing validates every length (code 0-3, padding, 48-frame / 120 ms caps) before slicing; output-buffer size validated (`BufferTooSmall`, found by property test); 15 fuzz targets incl. new repacketizer + multistream | |
| H-20 | ★ Secrets zeroized; never logged | N/A | No key material or secrets: the codec processes audio samples only (threat model §2) | |
| H-21 | Concurrency discipline | Completed | No `static mut`, no manual `Send`/`Sync`; `parallel` uses `std::thread::scope` with disjoint owned chunks; shared state limited to `OnceLock`/atomic feature caches | |

### Phase 4 — Static analysis

| ID | Gate | Status | Evidence | Target |
|---|---|---|---|---|
| H-22 | Static analysis beyond the default linter runs on every PR | Completed | `.semgrep/rusty-opus.yml` (unsafe confinement, no unchecked indexing on untrusted input, no I/O) + `tools/check_invariants.py` (no unwrap on decode paths, CPU features only via `crate::isa`, environment read only via `crate::isa`/`research_env` and filesystem only under the non-default `research` feature — negative-tested) in the CI `static-analysis` job; both clean locally | |

### Phase 5 — Dynamic analysis

| ID | Gate | Status | Evidence | Target |
|---|---|---|---|---|
| H-23 | ★ Tests pass under Miri | Completed | `cargo +nightly miri test --lib` with `-Zmiri-strict-provenance`, `RUSTY_OPUS_ISA=scalar`, 2026-10-04: all 65 runnable unit tests pass, 0 UB (1 wall-clock benchmark `cfg_attr(miri, ignore)`); randomised loops scaled via `isa::oracle::iters`; default-ISA (SSE2) shards also 0 UB; SIMD modules again with `-C target-feature=+avx,+avx2,+fma` (AVX2/FMA kernels and their oracles, 37 tests in pvq/bands/kiss_fft/mdct/pitch/silk): 0 UB; native pass in the weekly job | |
| H-24 | Critical paths pass the sanitizers (ASan/MSan/TSan) | Completed | AddressSanitizer (`-Zsanitizer=address`, MSVC runtime) on the full lib + integration suite, 2026-10-04: 246 passed, 0 failed, 0 sanitizer reports; weekly Linux ASan job in `scheduled.yml`. MSan/TSan not run: no FFI or uninitialised memory remains, and the only concurrency is `thread::scope` over owned chunks | |
| H-25 | `cargo careful test` green | Completed | `cargo +nightly careful test --release --lib --tests` (std with debug assertions + UB precondition checks), 2026-10-04: 246 passed, 0 failed | |
### Phase 6 — Fuzzing and properties

| ID | Gate | Status | Evidence | Target |
|---|---|---|---|---|
| H-26 | ★ Fuzz target per public parser, decoder, or message handler | Completed | 15 targets in `fuzz/fuzz_targets/` covering `OpusDecoder::decode`/`decode_fec` (every rate × channels × TOC), SILK/CELT/hybrid decoders, multi-frame, repacketizer, multistream decoder, and the encoders; seed corpora committed in `fuzz/seeds/` (`fuzz/make_seeds.py`) | |
| H-27 | ★ Continuous fuzzing with no open crashes | Incomplete | Weekly 10-min-per-target campaign in `scheduled.yml`; local seeded campaign 2026-10-04 found three library panics in the encoder (2-byte output buffer; CBR frames over 1275 bytes, which also produced invalid packets; an LBRR gain-index overflow, root-caused to a non-conformant FEC encoder), each fixed with a regression test (`encoder_small_output_buffer_never_panics`, `encoder_never_exceeds_max_frame_size`, `fec_streams_keep_range_coder_in_sync`), and one harness bug (fixed); re-runs on the fixed code are recorded in the Audit log. The ≥30-day continuous requirement is not yet met — see Waivers | |
| H-28 | Property tests cover the documented invariants | Completed | `tests/properties.rs`: decode / FEC / packet utilities / multistream never panic on any bytes, small output buffer is an error, encoder survives NaN/±Inf/denormal PCM and its packets decode, range-coder round-trip, pad/unpad and split/merge identities, bad config → `BadArg`, encoder never panics on small output buffers; found 3 real bugs (fixed), including a decoder conformance bug (code-1 packets with two empty frames rejected) | |
| H-29 | Mutation and/or differential testing on critical modules | Completed | Differential harness vs libopus (`tools/coverage_matrix.py` 3240 cells, `coverage_decoder.py` every rate × layout, PLC parity) run 2026-10-03/04, plus scalar-vs-SIMD oracles on x86_64 and aarch64; scheduled CI job builds libopus 1.5.2 from source and runs it weekly. 2026-10-04 re-run: matrix 3240/3240 (0 range mismatches); new `tools/coverage_fec.py` (480 in-band-FEC cells, libopus range check with and without loss) found that FEC streams were undecodable by libopus and that low-rate 60/120 ms CBR SILK overflowed its budget; both fixed, now 480/480 and in the weekly job | |

### Phase 7 — Formal verification

| ID | Gate | Status | Evidence | Target |
|---|---|---|---|---|
| H-30 | Proof of panic-freedom / UB-freedom per `unsafe` module | Incomplete | No Kani harnesses: the `unsafe` is SIMD intrinsics, which Kani does not model; covered instead by Miri, scalar-twin oracles and fuzzing (residual risk R-002) | |

### Phase 8 — Build and binary

| ID | Gate | Status | Evidence | Target |
|---|---|---|---|---|
| H-31 | ★ Binary hardening applied and verified | N/A | Library crate: no shipped binary (examples are dev tools); hardening is the embedding application's build | |
| H-32 | Build is reproducible or fully auditable | N/A | Library crate: no release artifacts beyond the source crate (`bin` tier gate) | |

### Phase 9 — Runtime privilege

| ID | Gate | Status | Evidence | Target |
|---|---|---|---|---|
| H-33 | Least privilege documented and tested | N/A | Library crate: performs no I/O, opens no files/sockets, spawns no processes (enforced by semgrep `library-performs-no-io`); privilege is the host's | |

### Phase 10 — Cryptography

| ID | Gate | Status | Evidence | Target |
|---|---|---|---|---|
| H-34 | Vetted crypto only; no bespoke primitives | N/A | No cryptography in the crate | |
| H-35 | Side-channel discipline (constant-time, no secret branches) | N/A | No secrets or cryptography; audio content is not a secret the codec protects | |
| H-36 | Post-quantum migration plan for long-lived keys | N/A | No keys | |

### Phase 11 — CI/CD, release, and operations

| ID | Gate | Status | Evidence | Target |
|---|---|---|---|---|
| H-37 | CI runs the hardening gate on every PR | Completed | `.github/workflows/ci.yml`: fmt, clippy (4 targets), test (Linux, Windows, macOS-arm64 ± scalar ISA), debug test, MSRV, deny+audit+vet, semgrep+invariants+UNSAFE.md+README checks, wasm; Miri/sanitizers/careful/fuzz/differential weekly; actions SHA-pinned; `permissions: contents: read`; the org-managed `portfolio-check.yml` (Remade-With-Rust shared harness, maintained by remade-updater) is the one workflow not SHA-pinned in this repo | |
| H-38 | Releases signed, attested, and changelogged for security | Completed | v1.0.0: SSH-signed tag; `gh attestation verify rusty-opus-1.0.0.crate` passes (SLSA provenance v1, built by `release.yml@refs/tags/v1.0.0`); CHANGELOG has a Security section; published to crates.io from the tagged commit | |
| H-39 | ★ `SECURITY.md` with a coordinated disclosure process | Completed | `SECURITY.md`: private reporting via GitHub advisories, 3-day ack / 10-day assessment / 30-day fix windows, coordinated disclosure + RustSec advisory, scope | |
| H-40 | Advisory monitoring and scheduled re-audit | Completed | Weekly `cargo audit` + `cargo deny check advisories` in `scheduled.yml`; Dependabot; full re-audit scheduled quarterly (next 2027-01-04) | |
| H-41 | ★ Residual risks listed and accepted; waivers time-bounded | Completed | Risk register R-001/R-002 and the H-27 waiver (expires 2026-11-04) accepted by the maintainer, 2026-10-04 | |

### Phase 12 — Compliance controls

Only in play when a framework is declared in scope above. With none in scope, every row is
`N/A` — reason: "no compliance framework in scope". Mapping: the skill's `COMPLIANCE.md`.

| ID | Gate | Status | Evidence | Target |
|---|---|---|---|---|
| C-01 | Data inventory — personal/health/card data touched | N/A | no compliance framework in scope | |
| C-02 | Data-flow map including third-party egress | N/A | no compliance framework in scope | |
| C-03 | Encryption in transit for all egress | N/A | no compliance framework in scope | |
| C-04 | Encryption at rest for stored sensitive data | N/A | no compliance framework in scope | |
| C-05 | Key management — generation, storage, rotation, destruction | N/A | no compliance framework in scope | |
| C-06 | Retention limits and honoured deletion | N/A | no compliance framework in scope | |
| C-07 | Audit logging of security-relevant events | N/A | no compliance framework in scope | |
| C-08 | Log hygiene — no PII, secrets, or card data in logs | N/A | no compliance framework in scope | |
| C-09 | Least-privilege access to sensitive data | N/A | no compliance framework in scope | |
| C-10 | Subprocessor and third-party inventory | N/A | no compliance framework in scope | |
| C-11 | Incident response and breach notification path | N/A | no compliance framework in scope | |
| C-12 | Change management — reviewed, approved, traceable | N/A | no compliance framework in scope | |
| C-13 | Availability commitments and their evidence | N/A | no compliance framework in scope | |
| C-14 | Machine-readable SBOM + provenance for regulators | N/A | no compliance framework in scope | |

---

## Scheduled work

In execution order. Owners and dates are the maintainer's to assign.

| # | Gates | Work | Owner | Target | Notes |
|---|---|---|---|---|---|
| 1 | H-27 | Keep the weekly fuzz campaign green; consider OSS-Fuzz / ClusterFuzzLite for continuous coverage | | | 30-day clock starts with the first scheduled run |
| 2 | H-30 | Kani harnesses for the scalar twins' index arithmetic (the SIMD bodies are out of Kani's model) | | | |

---

## Residual risk register

Every open risk carries an owner, an acceptance, and a review date (H-41).

| ID | Risk | Likelihood | Impact | Mitigation status | Accepted by | Review date |
|---|---|---|---|---|---|---|
| R-001 | Fuzzing history shorter than 30 days (H-27): an input class not yet explored could crash the decoder | Low | High (DoS) | 15 seeded targets, weekly CI campaign, property tests over all entry points, decoder never panics on the corpus | maintainer (Tim), 2026-10-04 | 2026-11-04 |
| R-002 | No formal proofs for SIMD kernels (H-30): a kernel contract error would be memory-unsafe | Low | High | Every kernel's length contract asserted at its safe entry point; scalar-twin oracles on x86_64 + aarch64; Miri; zero `unsafe` in parsing/entropy coding | maintainer (Tim), 2026-10-04 | 2027-01-04 |

---

## Waivers

Time-bounded only. An expired waiver is an `Incomplete` gate, not a `Completed` one.

| Gate | Reason | Granted by | Expires |
|---|---|---|---|
| H-27 | 30 days of continuous fuzzing cannot exist before the first release; weekly campaign in place; the local campaign's three crashes are fixed and the re-runs are clean | maintainer (Tim), 2026-10-04 | 2026-11-04 |

---

## Audit log

Append one line per pass; never rewrite history. The trend is the point.

| Date | Depth | Auditor | Completed / Scheduled / Incomplete | ★ met | Note |
|---|---|---|---|---|---|
| 2026-10-04 | deep | Claude (Opus 5.5) | 24 / 0 / 10 | 11/15 | first pass: started at 2 evidenced gates; fixed 2 memory-safety bugs found by audit agents (PVQ AVX2 OOB write on NaN, NEON MDCT OOB read), aliasing UB in compute_theta, 2 property-test findings; unsafe removed from all parsing/entropy code |
| 2026-10-04 | deep | Claude (Opus 5.5) | 28 / 0 / 6 | 11/15 | second pass: fuzzing found three encoder defects (2-byte output buffer; CBR frames over 1275 bytes, also emitting invalid packets; an LBRR gain-index overflow that exposed a non-conformant FEC encoder, re-ported from libopus and verified 480/480 against it), differential testing found low-rate 60/120 ms CBR SILK overflowing its budget (fixed, +2.3 dB), and a property test found a decoder conformance bug (empty code-1 frames), both fixed; Miri extended to the AVX2/FMA kernels (0 UB); environment-driven dev toggles and the env-pathed tuning log moved behind a `research` feature with a CI invariant; multi-frame encode and multistream decode made allocation-free; parity: all FEC-off output byte-identical except the 12 explained cells and the 102 low-rate 60/120 ms CBR cells the budget fix changed (libopus now accepts all of them; it rejected 36 before) |
| 2026-10-04 | release | Claude (Opus 5.5) | 32 / 0 / 2 | 14/15 | v1.0.0 shipped: PR #8 merged with all 16 checks green (CI found a Linux-libm oracle-hash failure pre-existing on main and an illegal PVQ bench size; both fixed), signed tag, SBOM + verified SLSA provenance on the GitHub release, published to crates.io; maintainer accepted R-001/R-002 and the H-27 waiver |
