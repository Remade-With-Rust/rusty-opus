//! Runtime ISA dispatch for the hand-written x86 and NEON kernels.
//!
//! Every dispatcher asks ONE of these predicates, and each predicate checks
//! exactly the features its kernels are compiled with (`#[target_feature]`):
//! an `"avx,fma"` kernel behind an AVX-only check executes FMA instructions on
//! AVX CPUs without FMA3 (Sandy/Ivy Bridge, AMD Jaguar/Bulldozer) -> SIGILL.
//!
//! The decision is detected once and cached. `RUSTY_OPUS_ISA` caps the rung
//! (`scalar`, `sse2`, `avx`, `avx2`) so the scalar twins can be run on any host:
//! the whole-rung A/B and the SIMD-vs-scalar end-to-end checks use it. The older
//! `RUSTY_OPUS_NO_AVX2` still caps at `avx`. Tests cap per THREAD with
//! `with_cap` so oracle tests can run a dispatcher under both rungs in
//! parallel without racing each other.
//!
//! On aarch64 NEON is baseline, so [`neon`] is true unless the cap is `scalar`:
//! that is what lets the same oracle tests and the end-to-end scalar A/B reach
//! the NEON kernels' scalar twins on ARM.

/// Rungs, in order. A cap admits every rung at or below it.
pub const SCALAR: u8 = 0;
pub const SSE2: u8 = 1;
pub const AVX: u8 = 2;
pub const AVX2: u8 = 3;

#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
mod imp {
    use std::sync::atomic::{AtomicU8, Ordering};

    // Feature bits of the HOST, after the env cap.
    pub const F_SSE2: u8 = 1;
    pub const F_AVX: u8 = 2;
    pub const F_FMA: u8 = 4;
    pub const F_AVX2: u8 = 8;
    const KNOWN: u8 = 0x80;

    static FEATURES: AtomicU8 = AtomicU8::new(0);

    fn detect() -> u8 {
        let mut f = 0;
        if std::arch::is_x86_feature_detected!("sse2") {
            f |= F_SSE2;
        }
        if std::arch::is_x86_feature_detected!("avx") {
            f |= F_AVX;
        }
        if std::arch::is_x86_feature_detected!("fma") {
            f |= F_FMA;
        }
        if std::arch::is_x86_feature_detected!("avx2") {
            f |= F_AVX2;
        }
        let cap = match std::env::var("RUSTY_OPUS_ISA").ok().as_deref() {
            Some("scalar") => super::SCALAR,
            Some("sse2" | "sse") => super::SSE2,
            Some("avx") => super::AVX,
            _ if crate::research_env("RUSTY_OPUS_NO_AVX2").is_some() => super::AVX,
            _ => super::AVX2,
        };
        mask(f, cap)
    }

    pub fn mask(f: u8, cap: u8) -> u8 {
        match cap {
            super::SCALAR => 0,
            super::SSE2 => f & F_SSE2,
            super::AVX => f & (F_SSE2 | F_AVX | F_FMA),
            _ => f,
        }
    }

    #[inline(always)]
    pub fn features() -> u8 {
        #[cfg(test)]
        if let Some(cap) = super::TEST_CAP.with(std::cell::Cell::get) {
            return mask(host(), cap);
        }
        host()
    }

    #[inline(always)]
    fn host() -> u8 {
        let f = FEATURES.load(Ordering::Relaxed);
        if f & KNOWN != 0 {
            return f;
        }
        let f = detect() | KNOWN;
        FEATURES.store(f, Ordering::Relaxed);
        f
    }
}

#[cfg(target_arch = "aarch64")]
mod imp {
    use std::sync::atomic::{AtomicU8, Ordering};

    pub const F_NEON: u8 = 1;
    const KNOWN: u8 = 0x80;

    static FEATURES: AtomicU8 = AtomicU8::new(0);

    pub fn mask(f: u8, cap: u8) -> u8 {
        if cap == super::SCALAR { 0 } else { f }
    }

    fn detect() -> u8 {
        let cap = match std::env::var("RUSTY_OPUS_ISA").ok().as_deref() {
            Some("scalar") => super::SCALAR,
            _ => super::AVX2,
        };
        mask(F_NEON, cap)
    }

    #[inline(always)]
    pub fn features() -> u8 {
        #[cfg(test)]
        if let Some(cap) = super::TEST_CAP.with(|c| c.get()) {
            return mask(host(), cap);
        }
        host()
    }

    #[inline(always)]
    fn host() -> u8 {
        let f = FEATURES.load(Ordering::Relaxed);
        if f & KNOWN != 0 {
            return f;
        }
        let f = detect() | KNOWN;
        FEATURES.store(f, Ordering::Relaxed);
        f
    }
}

/// NEON kernels (aarch64 baseline; false only when capped to `scalar`).
#[inline(always)]
pub fn neon() -> bool {
    #[cfg(target_arch = "aarch64")]
    {
        imp::features() & imp::F_NEON != 0
    }
    #[cfg(not(target_arch = "aarch64"))]
    {
        false
    }
}

#[cfg(test)]
thread_local! {
    static TEST_CAP: std::cell::Cell<Option<u8>> = const { std::cell::Cell::new(None) };
}

/// Run `f` with this thread's dispatch capped at `cap` (tests only).
#[cfg(test)]
pub fn with_cap<R>(cap: u8, f: impl FnOnce() -> R) -> R {
    let prev = TEST_CAP.with(|c| c.replace(Some(cap)));
    let r = f();
    TEST_CAP.with(|c| c.set(prev));
    r
}

macro_rules! pred {
    ($(#[$m:meta])* $name:ident, $bits:expr) => {
        $(#[$m])*
        #[inline(always)]
        pub fn $name() -> bool {
            #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
            {
                let need = $bits;
                imp::features() & need == need
            }
            #[cfg(not(any(target_arch = "x86", target_arch = "x86_64")))]
            {
                false
            }
        }
    };
}

pred!(
    /// SSE2 kernels (baseline on x86_64, but still cappable to scalar).
    sse2, imp::F_SSE2
);
pred!(
    /// `#[target_feature(enable = "avx")]` kernels.
    avx, imp::F_AVX
);
pred!(
    /// `#[target_feature(enable = "avx,fma")]` kernels.
    avx_fma, imp::F_AVX | imp::F_FMA
);
pred!(
    /// `#[target_feature(enable = "avx2")]` kernels.
    avx2, imp::F_AVX2
);
pred!(
    /// `#[target_feature(enable = "avx2,fma")]` kernels.
    avx2_fma, imp::F_AVX2 | imp::F_FMA
);

/// Shared helpers for the per-module `isa_oracle` tests: every dispatcher is run
/// capped at SCALAR (its scalar twin) and uncapped (the host's best kernel).
#[cfg(test)]
pub mod oracle {
    /// Deterministic xorshift64 stream.
    pub struct Rng(pub u64);
    impl Rng {
        pub fn u64(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }
        pub fn below(&mut self, n: usize) -> usize {
            (self.u64() % n as u64) as usize
        }
        /// Uniform in [-a, a).
        pub fn f32(&mut self, a: f32) -> f32 {
            ((self.u64() >> 40) as f32 / (1u64 << 24) as f32 * 2.0 - 1.0) * a
        }
        pub fn vec(&mut self, n: usize, a: f32) -> Vec<f32> {
            (0..n).map(|_| self.f32(a)).collect()
        }
        pub fn i16s(&mut self, n: usize, a: i32) -> Vec<i16> {
            (0..n)
                .map(|_| ((self.u64() % (2 * a as u64 + 1)) as i32 - a) as i16)
                .collect()
        }
    }

    /// Float kernels reassociate, so they are only float-close to the scalar
    /// twin: |simd - scalar| <= tol * scale, where `scale` is the magnitude the
    /// result was accumulated from (e.g. sum |x_i * y_i|), not the result itself
    /// -- a cancelling sum would otherwise make any rounding look huge.
    #[track_caller]
    pub fn close(simd: f32, scalar: f32, scale: f32, what: &str) {
        let tol = 1e-5 * scale.abs().max(1e-6);
        assert!(
            (simd - scalar).abs() <= tol || (simd.is_nan() && scalar.is_nan()),
            "{what}: simd {simd} vs scalar {scalar} (|diff| {} > {tol})",
            (simd - scalar).abs()
        );
    }

    #[track_caller]
    pub fn close_slices(simd: &[f32], scalar: &[f32], what: &str) {
        let scale = scalar.iter().fold(0.0f32, |m, v| m.max(v.abs()));
        for (i, (a, b)) in simd.iter().zip(scalar).enumerate() {
            close(*a, *b, scale, &format!("{what}[{i}]"));
        }
    }

    /// Randomised-test iteration count: `n` normally, ~n/500 (at least 4) under
    /// Miri, which interprets every instruction. Same code paths, fewer draws.
    pub const fn iters(n: usize) -> usize {
        if cfg!(miri) {
            let m = n / 500;
            if m < 4 { 4 } else { m }
        } else {
            n
        }
    }

    /// Run `f` once at the scalar rung and once uncapped.
    pub fn both<R>(mut f: impl FnMut() -> R) -> (R, R) {
        let scalar = super::with_cap(super::SCALAR, &mut f);
        (f(), scalar)
    }
}
