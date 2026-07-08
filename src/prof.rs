//! Feature-gated encode/decode **stage profiler** — the instrument for perf work.
//!
//! Zero cost unless the `profile` cargo feature is enabled: with it off,
//! [`scope`] is a no-op returning a ZST guard the optimizer elides entirely, so
//! release builds are byte-identical and the hot path is untouched. With it on,
//! each stage times itself into an atomic tick bucket; [`dump`] prints the
//! per-stage breakdown and [`snapshot`] returns a calibrated reading so a driver
//! can run many passes and take per-stage medians.
//!
//! Design mirrors `rusty_h264-common/src/prof.rs` (rdtsc ticks + wall-clock
//! anchor calibration). A [`Stage::Total`] scope wraps `OpusEncoder::encode()`;
//! the **`mgmt/other`** line is the residue (`Total − Σ stages`) — decompose it
//! until every line is named, or prove it equals the timer overhead
//! (`Σ calls × ~2 × tick cost`).

/// A timed pipeline stage. Order matters: everything before [`Total`](Stage::Total)
/// is a sub-component summed for the `mgmt/other` residue.
#[derive(Clone, Copy)]
pub enum Stage {
    // --- top-level (lib.rs encode()) ---
    /// hp_cutoff / f32→i16 conversion + SILK input resampling (down2 / down2_3).
    Resample = 0,
    // --- SILK encoder ---
    /// silk_vad_get_sa_q8 (voice activity detection).
    SilkVad = 1,
    /// silk_find_pitch_lags_fix (open-loop pitch analysis).
    SilkPitch = 2,
    /// silk_noise_shape_analysis_fix (shaping filter derivation).
    SilkNoise = 3,
    /// silk_find_pred_coefs_fix (LPC/LTP analysis + NLSF quantization).
    SilkPred = 4,
    /// silk_nsq / silk_nsq_del_dec (noise-shaping quantizer, incl. rate-loop reruns).
    SilkNsq = 5,
    /// silk_encode_indices + silk_encode_pulses (range coding of SILK symbols).
    SilkCode = 6,
    // --- CELT encoder ---
    /// Pre-emphasis + input/overlap buffer plumbing.
    CeltPreemph = 7,
    /// transient_analysis (short/long block decision).
    CeltTransient = 8,
    /// run_prefilter (pitch pre-filter incl. its pitch search).
    CeltPrefilter = 9,
    /// mode.mdct.forward calls (the forward MDCT(s)).
    CeltMdct = 10,
    /// compute_band_energies + normalise_bands.
    CeltBands = 11,
    /// quant_coarse_energy (coarse energy quantization + laplace coding).
    CeltCoarse = 12,
    /// tf_analysis + tf_encode (time-frequency resolution switching).
    CeltTf = 13,
    /// dynalloc_analysis + alloc_trim_analysis + clt_compute_allocation.
    CeltAlloc = 14,
    /// quant_fine_energy.
    CeltFine = 15,
    /// quant_all_bands (PVQ search + encode — the expected workhorse).
    CeltPvq = 16,
    /// Encoder-side synthesis after coding (denormalise/IMDCT for prefilter memory).
    CeltSynth = 17,
    // --- info-tier diagnostic scopes (nested inside SilkNsq; EXCLUDED from the
    //     residue sum via INFO_FIRST). Remove call sites after reading — at n_states
    //     × length calls their own rdtsc overhead inflates the enclosing stage. ---
    /// silk_noise_shape_quantizer_short_prediction (16-tap LPC dot product).
    SilkNsqLpc = 18,
    /// Warped shaping AR filter (serial recurrence) + RD decision, per state.
    SilkNsqShape = 19,
    /// Wraps the whole `OpusEncoder::encode()` call — the denominator.
    Total = 20,
}

/// Number of buckets.
pub const N: usize = 21;

/// Index of the first info-tier stage — buckets `INFO_FIRST..Total` are nested
/// diagnostics excluded from the `mgmt/other` residue sum.
pub const INFO_FIRST: usize = Stage::SilkNsqLpc as usize;

#[cfg(feature = "profile")]
mod imp {
    use super::{Stage, N};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Mutex;
    use std::time::Instant;

    /// Index of the first non-`Total` stage — the residue sum runs `0..SUB`.
    const SUB: usize = Stage::Total as usize;

    /// A cheap monotonic tick. On x86_64 this is `rdtsc` (~5-10 ns, ~3-5× cheaper
    /// than `Instant::now()` on Windows). Buckets accumulate *ticks*; `dump()`
    /// converts via a run-length TSC calibration (invariant TSC → ticks are
    /// wall-time-proportional). Elsewhere we fall back to `Instant` nanos.
    #[cfg(target_arch = "x86_64")]
    #[inline(always)]
    fn ticks() -> u64 {
        // SAFETY: `_rdtsc` is a pure timestamp read with no memory effects; it is
        // `unsafe` only because it is a target intrinsic. Dev-only (profile feature).
        unsafe { core::arch::x86_64::_rdtsc() }
    }
    #[cfg(not(target_arch = "x86_64"))]
    #[inline(always)]
    fn ticks() -> u64 {
        use std::sync::OnceLock;
        static EPOCH: OnceLock<Instant> = OnceLock::new();
        EPOCH.get_or_init(Instant::now).elapsed().as_nanos() as u64
    }

    /// (wall-clock, tick-count) sampled at `reset()` — the calibration anchor.
    static ANCHOR: Mutex<Option<(Instant, u64)>> = Mutex::new(None);

    const NAMES: [&str; N] = [
        "resample/hp",
        "silk-vad",
        "silk-pitch",
        "silk-noise-shape",
        "silk-pred-coefs",
        "silk-nsq",
        "silk-range-code",
        "celt-preemph",
        "celt-transient",
        "celt-prefilter",
        "celt-mdct",
        "celt-bands",
        "celt-coarse-q",
        "celt-tf",
        "celt-alloc",
        "celt-fine-q",
        "celt-pvq",
        "celt-synth",
        "  ↳nsq-lpc-pred",
        "  ↳nsq-shape+rd",
        "TOTAL encode()",
    ];

    static NS: [AtomicU64; N] = [const { AtomicU64::new(0) }; N];
    static CALLS: [AtomicU64; N] = [const { AtomicU64::new(0) }; N];

    /// RAII timer: accumulates `ticks()..drop` into the stage's bucket.
    pub struct Guard {
        stage: usize,
        start: u64,
    }

    impl Drop for Guard {
        #[inline]
        fn drop(&mut self) {
            let d = ticks().wrapping_sub(self.start);
            NS[self.stage].fetch_add(d, Ordering::Relaxed);
            CALLS[self.stage].fetch_add(1, Ordering::Relaxed);
        }
    }

    #[inline]
    pub fn scope(s: Stage) -> Guard {
        Guard {
            stage: s as usize,
            start: ticks(),
        }
    }

    /// Zero all buckets and sample the calibration anchor — call before a clean run.
    pub fn reset() {
        for a in NS.iter().chain(CALLS.iter()) {
            a.store(0, Ordering::Relaxed);
        }
        *ANCHOR.lock().unwrap() = Some((Instant::now(), ticks()));
    }

    /// Human-readable name for stage index `i` (`SUB` = the `TOTAL` row).
    pub fn name(i: usize) -> &'static str {
        NAMES.get(i).copied().unwrap_or("?")
    }

    /// One calibrated reading: `(ms, calls)` per stage index `0..N`.
    pub fn snapshot() -> [(f64, u64); N] {
        let load = |i: usize| NS[i].load(Ordering::Relaxed);
        let ns_per_tick = ANCHOR
            .lock()
            .unwrap()
            .map(|(t0, c0)| {
                let wall = t0.elapsed().as_nanos() as f64;
                let cyc = ticks().wrapping_sub(c0) as f64;
                if cyc > 0.0 {
                    wall / cyc
                } else {
                    1.0
                }
            })
            .unwrap_or(1.0);
        let mut out = [(0.0f64, 0u64); N];
        for (i, o) in out.iter_mut().enumerate() {
            *o = (
                load(i) as f64 * ns_per_tick / 1e6,
                CALLS[i].load(Ordering::Relaxed),
            );
        }
        out
    }

    /// Print the per-stage breakdown (does not reset).
    pub fn dump() {
        let s = snapshot();
        let total = s[SUB].0.max(1e-9);
        let sub_sum: f64 = (0..super::INFO_FIRST).map(|i| s[i].0).sum();
        let mgmt = (total - sub_sum).max(0.0);
        let pct = |ms: f64| 100.0 * ms / total;

        eprintln!("\n--- encode stage profile (encode() wall = {total:.1} ms) ---");
        for i in 0..SUB {
            if s[i].1 == 0 {
                continue;
            }
            eprintln!(
                "  {:<18} {:>8.1} ms  {:>5.1}%   ({} calls)",
                NAMES[i],
                s[i].0,
                pct(s[i].0),
                s[i].1,
            );
        }
        eprintln!(
            "  {:<18} {:>8.1} ms  {:>5.1}%   <- residue: mode select / control / glue (or timer overhead)",
            "mgmt/other",
            mgmt,
            pct(mgmt),
        );
        eprintln!("  {:<18} {:>8.1} ms  100.0%", NAMES[SUB], total);
    }
}

#[cfg(not(feature = "profile"))]
mod imp {
    use super::{Stage, N};

    /// No-op guard (ZST) — elided in release.
    pub struct Guard;

    #[inline(always)]
    pub fn scope(_s: Stage) -> Guard {
        Guard
    }
    #[inline(always)]
    pub fn reset() {}
    #[inline(always)]
    pub fn dump() {}
    #[inline(always)]
    pub fn snapshot() -> [(f64, u64); N] {
        [(0.0, 0); N]
    }
    #[inline(always)]
    pub fn name(_i: usize) -> &'static str {
        ""
    }
}

pub use imp::{dump, name, reset, scope, snapshot, Guard};
