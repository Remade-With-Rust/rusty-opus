//! Great Gate P2: the SPEED PAIR for a candidate arm — the deterministic work
//! counter (primary) and pinned-CPU encode time (confirmatory).
//!
//! The calculator refuses a bankable verdict from a quality-only harvest
//! (`_greatgate/great-gate.md` §4, the 2026-08-06 law). This probe supplies the
//! missing half for one (clip, rate, arm) unit:
//!
//!   work   = Σ per-stage scope CALL COUNTS over the real stages. Calls are
//!            exact and deterministic (same input + config → same count), so
//!            this is a one-run counter immune to timing noise
//!            (codec-measurement §15). Requires `--features profile`; without
//!            the feature it reports work=0 and says so.
//!   cpu_ms = best-of-N in-process encode time. Take this from a build WITHOUT
//!            `profile`: the profiler is part of the system under test
//!            (codec-measurement §6) and its rdtsc pairs tax the very stages
//!            being measured.
//!
//! So the harness runs this twice per unit — profiled for `work`, plain for
//! `cpu_ms` — and joins the two. Arm selection reuses the shipped truth-table
//! lever `RUSTY_OPUS_FORCE_MODE` (unset = the shipped auto decision).
//!
//!   cargo run --release --example gate_arm_cost -- in.wav <bitrate> [passes]
//!   cargo run --release --features profile --example gate_arm_cost -- …
//!
//! Output is one machine-readable line:
//!   ARMCOST clip=<stem> rate=<kbps> arm=<mode> work=<calls> cpu_ms=<best> bytes=<n>

use std::io::Read;

use rusty_opus::{Application, OpusEncoder};

fn read_wav(path: &str) -> (u32, u16, Vec<f32>) {
    let mut buf = Vec::new();
    std::fs::File::open(path).unwrap().read_to_end(&mut buf).unwrap();
    let rate = u32::from_le_bytes([buf[24], buf[25], buf[26], buf[27]]);
    let channels = u16::from_le_bytes([buf[22], buf[23]]);
    let mut i = 12;
    let (mut off, mut len) = (0usize, 0usize);
    while i + 8 <= buf.len() {
        let sz = u32::from_le_bytes([buf[i + 4], buf[i + 5], buf[i + 6], buf[i + 7]]) as usize;
        if &buf[i..i + 4] == b"data" {
            off = i + 8;
            len = sz.min(buf.len() - (i + 8));
            break;
        }
        i += 8 + sz + (sz & 1);
    }
    let samples = buf[off..off + len]
        .chunks_exact(2)
        .map(|b| i16::from_le_bytes([b[0], b[1]]) as f32 / 32768.0)
        .collect();
    (rate, channels, samples)
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let path = &args[1];
    let bitrate: i32 = args.get(2).map(|s| s.parse().unwrap()).unwrap_or(64_000);
    let passes: usize = args.get(3).map(|s| s.parse().unwrap()).unwrap_or(7);
    let arm = std::env::var("RUSTY_OPUS_FORCE_MODE").unwrap_or_else(|_| "auto".into());
    let stem = std::path::Path::new(path)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();

    let (rate, channels, pcm) = read_wav(path);
    let ch = channels as usize;
    let frame = rate as usize / 50;
    let step = frame * ch;

    let run = || {
        let mut enc = OpusEncoder::new(rate as i32, ch, Application::Audio).unwrap();
        enc.bitrate_bps = bitrate;
        let mut out = vec![0u8; 4000];
        let mut bytes = 0usize;
        for chunk in pcm.chunks_exact(step) {
            bytes += enc.encode(chunk, frame, &mut out).expect("encode");
        }
        bytes
    };

    // --- work: one deterministic pass with the buckets zeroed first ---
    rusty_opus::prof::reset();
    let bytes = run();
    let snap = rusty_opus::prof::snapshot();
    // Sum calls over the real stages only: skip the info-tier diagnostics and
    // the Total wrapper, which would double-count.
    let work: u64 = snap[..rusty_opus::prof::INFO_FIRST].iter().map(|(_, c)| *c).sum();

    // --- cpu_ms: best-of-N, and a determinism anchor on every pass ---
    let mut best = f64::INFINITY;
    for _ in 0..passes {
        let t0 = std::time::Instant::now();
        let b = run();
        let dt = t0.elapsed().as_secs_f64() * 1000.0;
        assert_eq!(b, bytes, "encode not deterministic across passes");
        if dt < best {
            best = dt;
        }
    }

    println!(
        "ARMCOST clip={} rate={} arm={} work={} cpu_ms={:.3} bytes={}{}",
        stem,
        bitrate / 1000,
        arm,
        work,
        best,
        bytes,
        if work == 0 {
            "  # work=0: built WITHOUT --features profile (counter unavailable)"
        } else {
            "  # cpu_ms from a PROFILED build is taxed — use the plain build's value"
        }
    );
}

// Primary allocator for this target: our rusty_alloc, the pure-Rust mimalloc
// remake. Codec hot paths allocate heavily and the system heap dominates the
// profile there (measured 1.38x end-to-end on AV2 decode). Per project
// convention this belongs in binary/bench/example roots, never in a library --
// a library that declares one hijacks every dependent's allocator choice.
#[global_allocator]
static RUSTY_ALLOC: rusty_alloc_api::RustyAlloc = rusty_alloc_api::RustyAlloc;
