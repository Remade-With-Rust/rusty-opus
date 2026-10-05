use crate::celt_lpc::{autocorr, lpc};

pub fn inner_prod(x: &[f32], y: &[f32], n: usize) -> f32 {
    // Contract enforced because the SIMD kernels read raw pointers up to `n`.
    assert!(
        x.len() >= n && y.len() >= n,
        "inner_prod: n = {n} out of range"
    );
    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    // SAFETY: `isa::avx_fma()` confirmed AVX+FMA, exactly the kernel's
    // `#[target_feature(enable = "avx,fma")]`. The kernel reads `x[..n]` and
    // `y[..n]` unchecked, so it needs `x.len() >= n && y.len() >= n`, which
    // the `assert!` at the top of this function enforces (a short slice
    // panics instead of reading out of bounds).
    unsafe {
        if crate::isa::avx_fma() {
            return inner_prod_avx(x, y, n);
        }
    }
    #[cfg(target_arch = "aarch64")]
    if crate::isa::neon() {
        // SAFETY: `isa::neon()` confirmed NEON (aarch64 baseline). Requires
        // `x.len() >= n && y.len() >= n` (unchecked loads), enforced by the
        // `assert!` at the top of this function.
        return unsafe { inner_prod_neon(x, y, n) };
    }
    #[cfg(all(target_arch = "x86_64", target_feature = "sse"))]
    if crate::isa::sse2() {
        // SAFETY: SSE is compile-time enabled (cfg) and `isa::sse2()` confirmed
        // it. Requires `x.len() >= n && y.len() >= n` (unchecked loads),
        // enforced by the `assert!` at the top of this function.
        return unsafe { inner_prod_sse(x, y, n) };
    }
    {
        let mut sum = 0.0f32;
        for i in 0..n {
            sum += x[i] * y[i];
        }
        sum
    }
}

pub fn dual_inner_prod(x: &[f32], y1: &[f32], y2: &[f32], n: usize) -> (f32, f32) {
    assert!(
        x.len() >= n && y1.len() >= n && y2.len() >= n,
        "dual_inner_prod: n = {n} out of range"
    );
    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    // SAFETY: `isa::avx_fma()` confirmed AVX+FMA, exactly the kernel's
    // `#[target_feature(enable = "avx,fma")]`. The kernel reads `x[..n]`,
    // `y1[..n]` and `y2[..n]` unchecked, so all three must hold at least `n`
    // elements, which the `assert!` at the top of this function enforces.
    unsafe {
        if crate::isa::avx_fma() {
            return dual_inner_prod_avx(x, y1, y2, n);
        }
    }
    #[cfg(target_arch = "aarch64")]
    if crate::isa::neon() {
        // SAFETY: `isa::neon()` confirmed NEON (aarch64 baseline). Requires
        // `x`, `y1`, `y2` to each hold `>= n` elements (unchecked loads),
        // enforced by the `assert!` at the top of this function.
        return unsafe { dual_inner_prod_neon(x, y1, y2, n) };
    }
    #[cfg(all(target_arch = "x86_64", target_feature = "sse"))]
    if crate::isa::sse2() {
        // SAFETY: SSE is compile-time enabled (cfg) and `isa::sse2()` confirmed
        // it. Requires `x`, `y1`, `y2` to each hold `>= n` elements (unchecked
        // loads), enforced by the `assert!` at the top of this function.
        return unsafe { dual_inner_prod_sse(x, y1, y2, n) };
    }
    {
        let mut xy1 = 0.0f32;
        let mut xy2 = 0.0f32;
        for i in 0..n {
            xy1 += x[i] * y1[i];
            xy2 += x[i] * y2[i];
        }
        (xy1, xy2)
    }
}

pub fn pitch_xcorr(x: &[f32], y: &[f32], xcorr: &mut [f32], len: usize, max_pitch: usize) {
    // The 4-lag kernels read y[i..i + len + 3] for i + 4 <= max_pitch, hence
    // y.len() >= max_pitch + len - 1 (was only debug_assert!ed).
    assert!(
        x.len() >= len
            && xcorr.len() >= max_pitch
            && (max_pitch == 0 || y.len() + 1 >= max_pitch + len),
        "pitch_xcorr: len = {len}, max_pitch = {max_pitch} out of range"
    );
    if len == 0 {
        // Every correlation is an empty sum (and the NEON kernel needs len >= 1).
        xcorr[..max_pitch].fill(0.0);
        return;
    }
    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    // SAFETY: `isa::avx_fma()` confirmed AVX+FMA, exactly the kernel's
    // `#[target_feature(enable = "avx,fma")]`. The kernel needs
    // `x.len() >= len` and `y.len() >= max_pitch + len - 1` (the 4-lag
    // kernel reads `y[i..i + len + 3]` for `i + 4 <= max_pitch`); `xcorr`
    // writes are bounds-checked. Both bounds are enforced by the `assert!`
    // at the top of this function (`y.len() + 1 >= max_pitch + len` for
    // `max_pitch > 0`), and `len >= 1` holds past the early return.
    unsafe {
        if crate::isa::avx_fma() {
            return pitch_xcorr_avx(x, y, xcorr, len, max_pitch);
        }
    }
    #[cfg(target_arch = "aarch64")]
    if crate::isa::neon() {
        if max_pitch >= 32 {
            // SAFETY: `isa::neon()` confirmed NEON. Requires `x.len() >= len`
            // and `y.len() >= max_pitch + len - 1` (unchecked loads), both
            // enforced by the `assert!` at the top of this function. The
            // kernel also needs `len >= 1` (the 4-lag kernel reads `x[0]` /
            // `y[..4]` even for `len == 0`); the `len == 0` early return
            // above guarantees it.
            unsafe {
                return pitch_xcorr_neon(x, y, xcorr, len, max_pitch);
            }
        }
        for i in 0..max_pitch {
            // SAFETY: `isa::neon()` confirmed NEON. `&y[i..]` is bounds-checked;
            // `inner_prod_neon` additionally needs `x.len() >= len` and
            // `y.len() - i >= len`, which follow from the asserted
            // `y.len() >= max_pitch + len - 1` (i < max_pitch).
            xcorr[i] = unsafe { inner_prod_neon(x, &y[i..], len) };
        }
        return;
    }
    #[cfg(all(target_arch = "x86_64", target_feature = "sse"))]
    if crate::isa::sse2() {
        // SAFETY: SSE is compile-time enabled (cfg) and `isa::sse2()` confirmed
        // it. Requires `x.len() >= len` and `y.len() >= max_pitch + len - 1`
        // (unchecked loads), both enforced by the `assert!` at the top of this
        // function.
        return unsafe { pitch_xcorr_sse(x, y, xcorr, len, max_pitch) };
    }
    {
        for i in 0..max_pitch {
            xcorr[i] = inner_prod(x, &y[i..], len);
        }
    }
}

/// NEON dot product of `x[..n]` and `y[..n]`.
///
/// # Safety
///
/// - The CPU must support NEON (baseline on aarch64; gate on `isa::neon()`).
/// - `x.len() >= n` and `y.len() >= n`: the 4-wide loads read `x[i..i + 4]`
///   and `y[i..i + 4]` for `i + 4 <= n` without bounds checks (the scalar
///   tail is bounds-checked).
#[cfg(target_arch = "aarch64")]
#[inline(always)]
#[allow(unsafe_op_in_unsafe_fn)]
unsafe fn inner_prod_neon(x: &[f32], y: &[f32], n: usize) -> f32 {
    use std::arch::aarch64::*;

    let mut xy = vdupq_n_f32(0.0);
    let mut i = 0;

    while i + 8 <= n {
        let x0 = vld1q_f32(x.as_ptr().add(i));
        let y0 = vld1q_f32(y.as_ptr().add(i));
        xy = vfmaq_f32(xy, x0, y0);

        let x1 = vld1q_f32(x.as_ptr().add(i + 4));
        let y1 = vld1q_f32(y.as_ptr().add(i + 4));
        xy = vfmaq_f32(xy, x1, y1);

        i += 8;
    }

    if i + 4 <= n {
        let x0 = vld1q_f32(x.as_ptr().add(i));
        let y0 = vld1q_f32(y.as_ptr().add(i));
        xy = vfmaq_f32(xy, x0, y0);
        i += 4;
    }

    let mut sum = vaddvq_f32(xy);

    for j in i..n {
        sum += x[j] * y[j];
    }

    sum
}

/// NEON pair of dot products `(x . y1, x . y2)` over the first `n` elements.
///
/// # Safety
///
/// - The CPU must support NEON (baseline on aarch64; gate on `isa::neon()`).
/// - `x.len() >= n`, `y1.len() >= n` and `y2.len() >= n`: the 4-wide loads
///   read `[i..i + 4]` of each for `i + 4 <= n` without bounds checks (the
///   scalar tail is bounds-checked).
#[cfg(target_arch = "aarch64")]
#[inline(always)]
#[allow(unsafe_op_in_unsafe_fn)]
unsafe fn dual_inner_prod_neon(x: &[f32], y1: &[f32], y2: &[f32], n: usize) -> (f32, f32) {
    use std::arch::aarch64::*;

    let mut xy1 = vdupq_n_f32(0.0);
    let mut xy2 = vdupq_n_f32(0.0);
    let mut i = 0;

    while i + 8 <= n {
        let x0 = vld1q_f32(x.as_ptr().add(i));
        let x4 = vld1q_f32(x.as_ptr().add(i + 4));

        let y1_0 = vld1q_f32(y1.as_ptr().add(i));
        let y1_4 = vld1q_f32(y1.as_ptr().add(i + 4));
        let y2_0 = vld1q_f32(y2.as_ptr().add(i));
        let y2_4 = vld1q_f32(y2.as_ptr().add(i + 4));

        xy1 = vfmaq_f32(xy1, x0, y1_0);
        xy2 = vfmaq_f32(xy2, x0, y2_0);
        xy1 = vfmaq_f32(xy1, x4, y1_4);
        xy2 = vfmaq_f32(xy2, x4, y2_4);

        i += 8;
    }

    if i + 4 <= n {
        let x0 = vld1q_f32(x.as_ptr().add(i));
        let y1_0 = vld1q_f32(y1.as_ptr().add(i));
        let y2_0 = vld1q_f32(y2.as_ptr().add(i));
        xy1 = vfmaq_f32(xy1, x0, y1_0);
        xy2 = vfmaq_f32(xy2, x0, y2_0);
        i += 4;
    }

    let sum1 = vaddvq_f32(xy1);
    let sum2 = vaddvq_f32(xy2);

    let mut s1 = sum1;
    let mut s2 = sum2;
    for j in i..n {
        s1 += x[j] * y1[j];
        s2 += x[j] * y2[j];
    }

    (s1, s2)
}

/// NEON 4-lag cross-correlation: `sum[k] = sum_j x[j] * y[j + k]`, `k < 4`,
/// `j < len` (overwrites `sum`).
///
/// # Safety
///
/// - The CPU must support NEON (baseline on aarch64; gate on `isa::neon()`).
/// - `len >= 1`, `x.len() >= len` and `y.len() >= len + 3`: all loads are
///   raw-pointer reads with no bounds checks (only `debug_assert!`s). Note
///   that even `len == 0` reads `x[0]` and `y[0..4]`, hence `len >= 1`.
#[cfg(target_arch = "aarch64")]
#[inline(always)]
#[allow(unsafe_op_in_unsafe_fn)]
unsafe fn xcorr_kernel_neon(x: &[f32], y: &[f32], sum: &mut [f32; 4], mut len: usize) {
    use std::arch::aarch64::*;

    debug_assert!(x.len() >= len, "xcorr_kernel_neon: x too short");
    debug_assert!(
        y.len() >= len + 3,
        "xcorr_kernel_neon: y too short (need len+3 for vextq)"
    );

    let mut summ = vdupq_n_f32(0.0);
    let mut xi = x.as_ptr();
    let mut yi = y.as_ptr();

    let mut yy = vld1q_f32(yi);

    while len > 8 {
        yi = yi.add(4);
        let yy1 = vld1q_f32(yi);
        yi = yi.add(4);
        let yy2 = vld1q_f32(yi);

        let xx0 = vld1q_f32(xi);
        xi = xi.add(4);
        let xx1 = vld1q_f32(xi);
        xi = xi.add(4);

        summ = vfmaq_lane_f32(summ, yy, vget_low_f32(xx0), 0);
        let yext = vextq_f32(yy, yy1, 1);
        summ = vfmaq_lane_f32(summ, yext, vget_low_f32(xx0), 1);
        let yext = vextq_f32(yy, yy1, 2);
        summ = vfmaq_lane_f32(summ, yext, vget_high_f32(xx0), 0);
        let yext = vextq_f32(yy, yy1, 3);
        summ = vfmaq_lane_f32(summ, yext, vget_high_f32(xx0), 1);

        summ = vfmaq_lane_f32(summ, yy1, vget_low_f32(xx1), 0);
        let yext = vextq_f32(yy1, yy2, 1);
        summ = vfmaq_lane_f32(summ, yext, vget_low_f32(xx1), 1);
        let yext = vextq_f32(yy1, yy2, 2);
        summ = vfmaq_lane_f32(summ, yext, vget_high_f32(xx1), 0);
        let yext = vextq_f32(yy1, yy2, 3);
        summ = vfmaq_lane_f32(summ, yext, vget_high_f32(xx1), 1);

        yy = yy2;
        len -= 8;
    }

    if len > 4 {
        yi = yi.add(4);
        let yy1 = vld1q_f32(yi);

        let xx0 = vld1q_f32(xi);
        xi = xi.add(4);

        summ = vfmaq_lane_f32(summ, yy, vget_low_f32(xx0), 0);
        let yext = vextq_f32(yy, yy1, 1);
        summ = vfmaq_lane_f32(summ, yext, vget_low_f32(xx0), 1);
        let yext = vextq_f32(yy, yy1, 2);
        summ = vfmaq_lane_f32(summ, yext, vget_high_f32(xx0), 0);
        let yext = vextq_f32(yy, yy1, 3);
        summ = vfmaq_lane_f32(summ, yext, vget_high_f32(xx0), 1);

        yy = yy1;
        len -= 4;
    }

    while len > 1 {
        let xx = vld1_dup_f32(xi);
        xi = xi.add(1);
        summ = vfmaq_lane_f32(summ, yy, xx, 0);
        yi = yi.add(1);
        yy = vld1q_f32(yi);
        len -= 1;
    }

    let xx = vld1_dup_f32(xi);
    summ = vfmaq_lane_f32(summ, yy, xx, 0);

    vst1q_f32(sum.as_mut_ptr(), summ);
}

/// NEON `pitch_xcorr`: `xcorr[i] = x[..len] . y[i..i + len]` for `i < max_pitch`.
///
/// # Safety
///
/// - The CPU must support NEON (baseline on aarch64; gate on `isa::neon()`).
/// - `len >= 1` when `max_pitch >= 4` (required by `xcorr_kernel_neon`).
/// - `x.len() >= len` and `y.len() >= max_pitch + len - 1` (only
///   `debug_assert!`ed): the 4-lag kernel reads `y[i..i + len + 3]` for
///   `i + 4 <= max_pitch`. `xcorr` writes and the `&y[i..]` re-slices are
///   bounds-checked.
#[cfg(target_arch = "aarch64")]
#[inline(always)]
#[allow(unsafe_op_in_unsafe_fn)]
unsafe fn pitch_xcorr_neon(x: &[f32], y: &[f32], xcorr: &mut [f32], len: usize, max_pitch: usize) {
    debug_assert!(x.len() >= len, "pitch_xcorr_neon: x too short");
    debug_assert!(
        y.len() >= max_pitch + len - 1,
        "pitch_xcorr_neon: y too short"
    );
    debug_assert!(
        xcorr.len() >= max_pitch,
        "pitch_xcorr_neon: xcorr too short"
    );

    let mut i = 0;

    while i + 4 <= max_pitch {
        let mut sum = [0.0f32; 4];
        // SAFETY: NEON is part of this fn's own `# Safety` contract. `&y[i..]`
        // is bounds-checked, and with `i + 4 <= max_pitch` the contract
        // `y.len() >= max_pitch + len - 1` gives `y[i..].len() >= len + 3`;
        // `x.len() >= len` and `len >= 1` are forwarded from the contract.
        unsafe { xcorr_kernel_neon(x, &y[i..], &mut sum, len) };
        xcorr[i] = sum[0];
        xcorr[i + 1] = sum[1];
        xcorr[i + 2] = sum[2];
        xcorr[i + 3] = sum[3];
        i += 4;
    }

    for j in i..max_pitch {
        // SAFETY: NEON is part of this fn's own `# Safety` contract. `&y[j..]`
        // is bounds-checked, and `j < max_pitch` with the contract
        // `y.len() >= max_pitch + len - 1` gives `y[j..].len() >= len`;
        // `x.len() >= len` is forwarded from the contract.
        xcorr[j] = unsafe { inner_prod_neon(x, &y[j..], len) };
    }
}

/// SSE dot product of `x[..n]` and `y[..n]`.
///
/// # Safety
///
/// - The CPU must support SSE (compile-time enabled via the `cfg`; callers
///   also gate on `isa::sse2()`).
/// - `x.len() >= n` and `y.len() >= n`: the 4-wide loads read `[i..i + 4]`
///   for `i + 4 <= n` without bounds checks (the scalar tail is checked).
#[cfg(all(target_arch = "x86_64", target_feature = "sse"))]
#[inline(always)]
#[allow(unsafe_op_in_unsafe_fn)]
unsafe fn inner_prod_sse(x: &[f32], y: &[f32], n: usize) -> f32 {
    #[cfg(target_arch = "x86")]
    use std::arch::x86::*;
    #[cfg(target_arch = "x86_64")]
    use std::arch::x86_64::*;

    let mut sum0 = _mm_setzero_ps();
    let mut sum1 = _mm_setzero_ps();
    let mut i = 0;

    while i + 8 <= n {
        let x0 = _mm_loadu_ps(x.as_ptr().add(i));
        let y0 = _mm_loadu_ps(y.as_ptr().add(i));
        sum0 = _mm_add_ps(sum0, _mm_mul_ps(x0, y0));
        let x1 = _mm_loadu_ps(x.as_ptr().add(i + 4));
        let y1 = _mm_loadu_ps(y.as_ptr().add(i + 4));
        sum1 = _mm_add_ps(sum1, _mm_mul_ps(x1, y1));
        i += 8;
    }

    if i + 4 <= n {
        let x0 = _mm_loadu_ps(x.as_ptr().add(i));
        let y0 = _mm_loadu_ps(y.as_ptr().add(i));
        sum0 = _mm_add_ps(sum0, _mm_mul_ps(x0, y0));
        i += 4;
    }

    let sum = _mm_add_ps(sum0, sum1);
    let tmp = _mm_movehl_ps(sum, sum);
    let sum = _mm_add_ps(sum, tmp);
    let tmp2 = _mm_shuffle_ps(sum, sum, 0x55);
    let sum = _mm_add_ss(sum, tmp2);
    let mut result = _mm_cvtss_f32(sum);

    while i < n {
        result += x[i] * y[i];
        i += 1;
    }

    result
}

/// SSE pair of dot products `(x . y1, x . y2)` over the first `n` elements.
///
/// # Safety
///
/// - The CPU must support SSE (compile-time enabled via the `cfg`; callers
///   also gate on `isa::sse2()`).
/// - `x.len() >= n`, `y1.len() >= n` and `y2.len() >= n`: the 4-wide loads
///   read `[i..i + 4]` of each for `i + 4 <= n` without bounds checks.
#[cfg(all(target_arch = "x86_64", target_feature = "sse"))]
#[inline(always)]
#[allow(unsafe_op_in_unsafe_fn)]
unsafe fn dual_inner_prod_sse(x: &[f32], y1: &[f32], y2: &[f32], n: usize) -> (f32, f32) {
    #[cfg(target_arch = "x86")]
    use std::arch::x86::*;
    #[cfg(target_arch = "x86_64")]
    use std::arch::x86_64::*;

    let mut xy1 = _mm_setzero_ps();
    let mut xy2 = _mm_setzero_ps();
    let mut i = 0;

    while i + 4 <= n {
        let xi = _mm_loadu_ps(x.as_ptr().add(i));
        let y1i = _mm_loadu_ps(y1.as_ptr().add(i));
        let y2i = _mm_loadu_ps(y2.as_ptr().add(i));
        xy1 = _mm_add_ps(xy1, _mm_mul_ps(xi, y1i));
        xy2 = _mm_add_ps(xy2, _mm_mul_ps(xi, y2i));
        i += 4;
    }

    let tmp = _mm_movehl_ps(xy1, xy1);
    let xy1 = _mm_add_ps(xy1, tmp);
    let tmp2 = _mm_shuffle_ps(xy1, xy1, 0x55);
    let xy1 = _mm_add_ss(xy1, tmp2);
    let mut s1 = _mm_cvtss_f32(xy1);

    let tmp = _mm_movehl_ps(xy2, xy2);
    let xy2 = _mm_add_ps(xy2, tmp);
    let tmp2 = _mm_shuffle_ps(xy2, xy2, 0x55);
    let xy2 = _mm_add_ss(xy2, tmp2);
    let mut s2 = _mm_cvtss_f32(xy2);

    while i < n {
        s1 += x[i] * y1[i];
        s2 += x[i] * y2[i];
        i += 1;
    }

    (s1, s2)
}

/// SSE 4-lag cross-correlation: `sum[k] += sum_j x[j] * y[j + k]`, `k < 4`,
/// `j < len`.
///
/// # Safety
///
/// - The CPU must support SSE (compile-time enabled via the `cfg`; callers
///   also gate on `isa::sse2()`).
/// - `x.len() >= len` and `y.len() >= len + 3` (when `len > 0`): every load
///   is an unchecked raw-pointer read; the highest `y` index touched is
///   `len + 2` (the `y[j + 3..j + 7]` load and the tail's `y[j..j + 4]`).
#[cfg(all(target_arch = "x86_64", target_feature = "sse"))]
#[inline(always)]
#[allow(unsafe_op_in_unsafe_fn)]
unsafe fn xcorr_kernel_sse(x: &[f32], y: &[f32], sum: &mut [f32; 4], len: usize) {
    #[cfg(target_arch = "x86")]
    use std::arch::x86::*;
    #[cfg(target_arch = "x86_64")]
    use std::arch::x86_64::*;

    let mut xsum1 = _mm_loadu_ps(sum.as_ptr());
    let mut xsum2 = _mm_setzero_ps();

    let mut j = 0;

    while j + 4 <= len {
        let x0 = _mm_loadu_ps(x.as_ptr().add(j));
        let yj = _mm_loadu_ps(y.as_ptr().add(j));
        let y3 = _mm_loadu_ps(y.as_ptr().add(j + 3));

        xsum1 = _mm_add_ps(xsum1, _mm_mul_ps(_mm_shuffle_ps(x0, x0, 0x00), yj));

        xsum2 = _mm_add_ps(
            xsum2,
            _mm_mul_ps(_mm_shuffle_ps(x0, x0, 0x55), _mm_shuffle_ps(yj, y3, 0x49)),
        );

        xsum1 = _mm_add_ps(
            xsum1,
            _mm_mul_ps(_mm_shuffle_ps(x0, x0, 0xaa), _mm_shuffle_ps(yj, y3, 0x9e)),
        );

        xsum2 = _mm_add_ps(xsum2, _mm_mul_ps(_mm_shuffle_ps(x0, x0, 0xff), y3));

        j += 4;
    }

    if j < len {
        xsum1 = _mm_add_ps(
            xsum1,
            _mm_mul_ps(
                _mm_set1_ps(*x.as_ptr().add(j)),
                _mm_loadu_ps(y.as_ptr().add(j)),
            ),
        );
        j += 1;
        if j < len {
            xsum2 = _mm_add_ps(
                xsum2,
                _mm_mul_ps(
                    _mm_set1_ps(*x.as_ptr().add(j)),
                    _mm_loadu_ps(y.as_ptr().add(j)),
                ),
            );
            j += 1;
            if j < len {
                xsum1 = _mm_add_ps(
                    xsum1,
                    _mm_mul_ps(
                        _mm_set1_ps(*x.as_ptr().add(j)),
                        _mm_loadu_ps(y.as_ptr().add(j)),
                    ),
                );
            }
        }
    }

    _mm_storeu_ps(sum.as_mut_ptr(), _mm_add_ps(xsum1, xsum2));
}

/// SSE `pitch_xcorr`: `xcorr[i] = x[..len] . y[i..i + len]` for `i < max_pitch`.
///
/// # Safety
///
/// - The CPU must support SSE (compile-time enabled via the `cfg`; callers
///   also gate on `isa::sse2()`).
/// - `x.len() >= len` and `y.len() >= max_pitch + len - 1`: the 4-lag kernel
///   reads `y[i..i + len + 3]` for `i + 4 <= max_pitch` unchecked. `xcorr`
///   writes and the `&y[i..]` re-slices are bounds-checked.
#[cfg(all(target_arch = "x86_64", target_feature = "sse"))]
#[inline(always)]
#[allow(unsafe_op_in_unsafe_fn)]
unsafe fn pitch_xcorr_sse(x: &[f32], y: &[f32], xcorr: &mut [f32], len: usize, max_pitch: usize) {
    let mut i = 0;

    while i + 4 <= max_pitch {
        let mut sum = [0.0f32; 4];
        xcorr_kernel_sse(x, &y[i..], &mut sum, len);
        xcorr[i] = sum[0];
        xcorr[i + 1] = sum[1];
        xcorr[i + 2] = sum[2];
        xcorr[i + 3] = sum[3];
        i += 4;
    }

    for j in i..max_pitch {
        xcorr[j] = inner_prod_sse(x, &y[j..], len);
    }
}

/// AVX+FMA dot product of `x[..n]` and `y[..n]`.
///
/// # Safety
///
/// - The CPU must support AVX and FMA (gate on `isa::avx_fma()`).
/// - `x.len() >= n` and `y.len() >= n`: the 8-wide loads read `[i..i + 8]`
///   for `i + 8 <= n` without bounds checks (the scalar tail is checked).
#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
#[target_feature(enable = "avx,fma")]
unsafe fn inner_prod_avx(x: &[f32], y: &[f32], n: usize) -> f32 {
    #[cfg(target_arch = "x86")]
    use std::arch::x86::*;
    #[cfg(target_arch = "x86_64")]
    use std::arch::x86_64::*;

    let mut acc0 = _mm256_setzero_ps();
    let mut acc1 = _mm256_setzero_ps();
    let mut i = 0usize;

    while i + 16 <= n {
        let x0 = _mm256_loadu_ps(x.as_ptr().add(i));
        let y0 = _mm256_loadu_ps(y.as_ptr().add(i));
        acc0 = _mm256_fmadd_ps(x0, y0, acc0);

        let x1 = _mm256_loadu_ps(x.as_ptr().add(i + 8));
        let y1 = _mm256_loadu_ps(y.as_ptr().add(i + 8));
        acc1 = _mm256_fmadd_ps(x1, y1, acc1);
        i += 16;
    }

    while i + 8 <= n {
        let x0 = _mm256_loadu_ps(x.as_ptr().add(i));
        let y0 = _mm256_loadu_ps(y.as_ptr().add(i));
        acc0 = _mm256_fmadd_ps(x0, y0, acc0);
        i += 8;
    }

    let acc = _mm256_add_ps(acc0, acc1);
    let hi = _mm256_extractf128_ps(acc, 1);
    let lo = _mm256_castps256_ps128(acc);
    let sum4 = _mm_add_ps(lo, hi);
    let tmp = _mm_movehl_ps(sum4, sum4);
    let sum2 = _mm_add_ps(sum4, tmp);
    let tmp2 = _mm_shuffle_ps(sum2, sum2, 0x55);
    let mut result = _mm_cvtss_f32(_mm_add_ss(sum2, tmp2));

    while i < n {
        result += x[i] * y[i];
        i += 1;
    }

    result
}

/// AVX+FMA pair of dot products `(x . y1, x . y2)` over the first `n` elements.
///
/// # Safety
///
/// - The CPU must support AVX and FMA (gate on `isa::avx_fma()`).
/// - `x.len() >= n`, `y1.len() >= n` and `y2.len() >= n`: the 8-wide loads
///   read `[i..i + 8]` of each for `i + 8 <= n` without bounds checks.
#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
#[target_feature(enable = "avx,fma")]
unsafe fn dual_inner_prod_avx(x: &[f32], y1: &[f32], y2: &[f32], n: usize) -> (f32, f32) {
    #[cfg(target_arch = "x86")]
    use std::arch::x86::*;
    #[cfg(target_arch = "x86_64")]
    use std::arch::x86_64::*;

    let mut acc1 = _mm256_setzero_ps();
    let mut acc2 = _mm256_setzero_ps();
    let mut acc1b = _mm256_setzero_ps();
    let mut acc2b = _mm256_setzero_ps();
    let mut i = 0usize;

    while i + 16 <= n {
        let xv0 = _mm256_loadu_ps(x.as_ptr().add(i));
        let xv1 = _mm256_loadu_ps(x.as_ptr().add(i + 8));
        let y1v0 = _mm256_loadu_ps(y1.as_ptr().add(i));
        let y2v0 = _mm256_loadu_ps(y2.as_ptr().add(i));
        let y1v1 = _mm256_loadu_ps(y1.as_ptr().add(i + 8));
        let y2v1 = _mm256_loadu_ps(y2.as_ptr().add(i + 8));
        acc1 = _mm256_fmadd_ps(xv0, y1v0, acc1);
        acc2 = _mm256_fmadd_ps(xv0, y2v0, acc2);
        acc1b = _mm256_fmadd_ps(xv1, y1v1, acc1b);
        acc2b = _mm256_fmadd_ps(xv1, y2v1, acc2b);
        i += 16;
    }

    while i + 8 <= n {
        let xv = _mm256_loadu_ps(x.as_ptr().add(i));
        let y1v = _mm256_loadu_ps(y1.as_ptr().add(i));
        let y2v = _mm256_loadu_ps(y2.as_ptr().add(i));
        acc1 = _mm256_fmadd_ps(xv, y1v, acc1);
        acc2 = _mm256_fmadd_ps(xv, y2v, acc2);
        i += 8;
    }

    let acc1 = _mm256_add_ps(acc1, acc1b);
    let acc2 = _mm256_add_ps(acc2, acc2b);

    let hi1 = _mm256_extractf128_ps(acc1, 1);
    let lo1 = _mm256_castps256_ps128(acc1);
    let sum41 = _mm_add_ps(lo1, hi1);
    let t11 = _mm_movehl_ps(sum41, sum41);
    let s21 = _mm_add_ps(sum41, t11);
    let t12 = _mm_shuffle_ps(s21, s21, 0x55);
    let mut s1 = _mm_cvtss_f32(_mm_add_ss(s21, t12));

    let hi2 = _mm256_extractf128_ps(acc2, 1);
    let lo2 = _mm256_castps256_ps128(acc2);
    let sum42 = _mm_add_ps(lo2, hi2);
    let t21 = _mm_movehl_ps(sum42, sum42);
    let s22 = _mm_add_ps(sum42, t21);
    let t22 = _mm_shuffle_ps(s22, s22, 0x55);
    let mut s2 = _mm_cvtss_f32(_mm_add_ss(s22, t22));

    while i < n {
        s1 += x[i] * y1[i];
        s2 += x[i] * y2[i];
        i += 1;
    }

    (s1, s2)
}

/// AVX+FMA `pitch_xcorr`: `xcorr[i] = x[..len] . y[i..i + len]`, `i < max_pitch`.
///
/// # Safety
///
/// - The CPU must support AVX and FMA (gate on `isa::avx_fma()`).
/// - `x.len() >= len` and `y.len() >= max_pitch + len - 1`: the 4-lag kernel
///   reads `y[i..i + len + 3]` for `i + 4 <= max_pitch` unchecked. `xcorr`
///   writes and the `&y[i..]` re-slices are bounds-checked.
#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
#[target_feature(enable = "avx,fma")]
unsafe fn pitch_xcorr_avx(x: &[f32], y: &[f32], xcorr: &mut [f32], len: usize, max_pitch: usize) {
    let mut i = 0;

    while i + 4 <= max_pitch {
        let mut sum = [0.0f32; 4];
        xcorr_kernel_avx(x, &y[i..], &mut sum, len);
        xcorr[i] = sum[0];
        xcorr[i + 1] = sum[1];
        xcorr[i + 2] = sum[2];
        xcorr[i + 3] = sum[3];
        i += 4;
    }

    for j in i..max_pitch {
        xcorr[j] = inner_prod_avx(x, &y[j..], len);
    }
}

/// AVX+FMA 4-lag cross-correlation: `sum[k] += sum_j x[j] * y[j + k]`,
/// `k < 4`, `j < len`.
///
/// # Safety
///
/// - The CPU must support AVX and FMA (gate on `isa::avx_fma()`).
/// - `x.len() >= len` and `y.len() >= len + 3` (when `len > 0`): every load
///   is an unchecked raw-pointer read; the highest `y` index touched is
///   `len + 2` (the `y[j + 7..j + 11]` / `y[j + 3..j + 7]` loads and the
///   tail's `y[j..j + 4]`).
#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
#[target_feature(enable = "avx,fma")]
unsafe fn xcorr_kernel_avx(x: &[f32], y: &[f32], sum: &mut [f32; 4], len: usize) {
    #[cfg(target_arch = "x86")]
    use std::arch::x86::*;
    #[cfg(target_arch = "x86_64")]
    use std::arch::x86_64::*;

    let mut xsum1 = _mm_loadu_ps(sum.as_ptr());
    let mut xsum2 = _mm_setzero_ps();

    let mut j = 0;

    while j + 8 <= len {
        let x0 = _mm_loadu_ps(x.as_ptr().add(j));
        let yj = _mm_loadu_ps(y.as_ptr().add(j));
        let y3 = _mm_loadu_ps(y.as_ptr().add(j + 3));

        xsum1 = _mm_fmadd_ps(_mm_shuffle_ps(x0, x0, 0x00), yj, xsum1);
        xsum2 = _mm_fmadd_ps(
            _mm_shuffle_ps(x0, x0, 0x55),
            _mm_shuffle_ps(yj, y3, 0x49),
            xsum2,
        );
        xsum1 = _mm_fmadd_ps(
            _mm_shuffle_ps(x0, x0, 0xaa),
            _mm_shuffle_ps(yj, y3, 0x9e),
            xsum1,
        );
        xsum2 = _mm_fmadd_ps(_mm_shuffle_ps(x0, x0, 0xff), y3, xsum2);

        let x1 = _mm_loadu_ps(x.as_ptr().add(j + 4));
        let yj4 = _mm_loadu_ps(y.as_ptr().add(j + 4));
        let y7 = _mm_loadu_ps(y.as_ptr().add(j + 7));

        xsum1 = _mm_fmadd_ps(_mm_shuffle_ps(x1, x1, 0x00), yj4, xsum1);
        xsum2 = _mm_fmadd_ps(
            _mm_shuffle_ps(x1, x1, 0x55),
            _mm_shuffle_ps(yj4, y7, 0x49),
            xsum2,
        );
        xsum1 = _mm_fmadd_ps(
            _mm_shuffle_ps(x1, x1, 0xaa),
            _mm_shuffle_ps(yj4, y7, 0x9e),
            xsum1,
        );
        xsum2 = _mm_fmadd_ps(_mm_shuffle_ps(x1, x1, 0xff), y7, xsum2);

        j += 8;
    }

    if j + 4 <= len {
        let x0 = _mm_loadu_ps(x.as_ptr().add(j));
        let yj = _mm_loadu_ps(y.as_ptr().add(j));
        let y3 = _mm_loadu_ps(y.as_ptr().add(j + 3));

        xsum1 = _mm_fmadd_ps(_mm_shuffle_ps(x0, x0, 0x00), yj, xsum1);
        xsum2 = _mm_fmadd_ps(
            _mm_shuffle_ps(x0, x0, 0x55),
            _mm_shuffle_ps(yj, y3, 0x49),
            xsum2,
        );
        xsum1 = _mm_fmadd_ps(
            _mm_shuffle_ps(x0, x0, 0xaa),
            _mm_shuffle_ps(yj, y3, 0x9e),
            xsum1,
        );
        xsum2 = _mm_fmadd_ps(_mm_shuffle_ps(x0, x0, 0xff), y3, xsum2);

        j += 4;
    }

    if j < len {
        xsum1 = _mm_fmadd_ps(
            _mm_set1_ps(*x.as_ptr().add(j)),
            _mm_loadu_ps(y.as_ptr().add(j)),
            xsum1,
        );
        j += 1;
        if j < len {
            xsum2 = _mm_fmadd_ps(
                _mm_set1_ps(*x.as_ptr().add(j)),
                _mm_loadu_ps(y.as_ptr().add(j)),
                xsum2,
            );
            j += 1;
            if j < len {
                xsum1 = _mm_fmadd_ps(
                    _mm_set1_ps(*x.as_ptr().add(j)),
                    _mm_loadu_ps(y.as_ptr().add(j)),
                    xsum1,
                );
            }
        }
    }

    _mm_storeu_ps(sum.as_mut_ptr(), _mm_add_ps(xsum1, xsum2));
}

fn celt_fir5(x: &mut [f32], num: &[f32], n: usize) {
    let mut mem = [0.0f32; 5];

    let num0 = num[0];
    let num1 = num[1];
    let num2 = num[2];
    let num3 = num[3];
    let num4 = num[4];

    for i in 0..n {
        let mut sum = x[i];
        sum += num0 * mem[0];
        sum += num1 * mem[1];
        sum += num2 * mem[2];
        sum += num3 * mem[3];
        sum += num4 * mem[4];

        mem[4] = mem[3];
        mem[3] = mem[2];
        mem[2] = mem[1];
        mem[1] = mem[0];
        mem[0] = x[i];

        x[i] = sum;
    }
}

pub fn pitch_downsample(x: &[&[f32]], x_lp: &mut [f32], len: usize, c: usize, factor: usize) {
    let offset = factor / 2;

    if x_lp.len() < len {
        return;
    }

    #[cfg(target_arch = "aarch64")]
    if factor == 2 && c <= 2 && crate::isa::neon() {
        pitch_downsample_neon(x, x_lp, len, c, offset);
        return;
    }

    for i in 1..len {
        let mut val = 0.0f32;
        for k in 0..c {
            let x_k = x[k];

            let idx_m = factor * i - offset;
            let idx_p = factor * i + offset;
            let idx_c = factor * i;

            if idx_p < x_k.len() {
                val += 0.25 * x_k[idx_m] + 0.25 * x_k[idx_p] + 0.5 * x_k[idx_c];
            }
        }
        x_lp[i] = val;
    }

    pitch_downsample_boundary(x, x_lp, c, offset);
    pitch_lp_whiten(x_lp, len);
}

/// The second half of libopus pitch_downsample: whiten the decimated signal
/// with a 4th-order LPC (noise floor, lag windowing, 0.9 bandwidth expansion)
/// plus a zero at 0.8, via celt_fir5. Shared by every arch path.
///
/// Until this was factored out, only the aarch64 NEON variant ran it: the
/// generic path (every x86 build) lost it in 7a12f04 (2026-04-08, the SIMD
/// commit), so the CELT encoder's prefilter pitch search and the CELT PLC
/// pitch search ran on an UNwhitened signal on x86 -- different from libopus
/// and from our own ARM builds (found via a PLC lag of 387 vs libopus's 393).
fn pitch_lp_whiten(x_lp: &mut [f32], len: usize) {
    let mut ac = [0.0f32; 5];
    autocorr(&x_lp[0..len], &mut ac, None, 0, 4, len);

    ac[0] *= 1.0001;

    for i in 1..=4 {
        let f = 0.008 * (i as f32);
        ac[i] -= ac[i] * f * f;
    }

    let mut lpc_coeffs = [0.0f32; 4];
    lpc(&mut lpc_coeffs, &ac, 4);

    let mut tmp = 1.0f32;
    for i in 0..4 {
        tmp *= 0.9;
        lpc_coeffs[i] *= tmp;
    }

    let c1 = 0.8f32;
    let mut lpc2 = [0.0f32; 5];
    lpc2[0] = lpc_coeffs[0] + c1;
    lpc2[1] = lpc_coeffs[1] + c1 * lpc_coeffs[0];
    lpc2[2] = lpc_coeffs[2] + c1 * lpc_coeffs[1];
    lpc2[3] = lpc_coeffs[3] + c1 * lpc_coeffs[2];
    lpc2[4] = c1 * lpc_coeffs[3];

    celt_fir5(x_lp, &lpc2, len);
}

#[inline]
fn pitch_downsample_boundary(x: &[&[f32]], x_lp: &mut [f32], c: usize, offset: usize) {
    {
        let mut val = 0.0f32;
        for k in 0..c {
            let x_k = x[k];

            let idx_offset = offset;
            let idx_0 = 0;
            if idx_offset < x_k.len() {
                val += 0.25 * x_k[idx_offset] + 0.5 * x_k[idx_0];
            }
        }
        x_lp[0] = val;
    }
}

#[cfg(target_arch = "aarch64")]
fn pitch_downsample_neon(x: &[&[f32]], x_lp: &mut [f32], len: usize, c: usize, offset: usize) {
    use std::arch::aarch64::*;

    // SAFETY: NEON is baseline on aarch64 and the only caller
    // (`pitch_downsample`) checked `isa::neon()`. The only unchecked accesses
    // are in the mono vector loop: its guard `2 * i + 9 <= x0.len()` keeps both
    // `vld2q_f32` reads (`x0[2i - 1..2i + 7]` and `x0[2i + 1..2i + 9]`, with
    // `i >= 1` so `2i - 1 >= 1`) in bounds, and `i + 4 <= len` with
    // `x_lp.len() >= len` (checked by `pitch_downsample`'s early return)
    // keeps the `vst1q_f32` to `x_lp[i..i + 4]` in bounds. `x[0]`/`x[1]` and
    // every scalar access are bounds-checked.
    unsafe {
        let v025 = vdupq_n_f32(0.25);
        let v05 = vdupq_n_f32(0.5);

        if c == 1 {
            let x0 = x[0];

            // Output lane j needs x[2(i+j)-1], x[2(i+j)], x[2(i+j)+1]: STRIDE 2.
            // vld2q deinterleaves: from 2i-1, .0 = odd taps (idx_m) and .1 = the
            // centres; from 2i+1, .0 = idx_p. (Until 2026-10-04 this loaded
            // contiguous x[2i-1..], so lanes 1..3 were wrong on every vector and
            // the mono CELT pitch search ran on a mangled signal on ARM only.)
            // Same op order as the scalar (two muls, two adds, no FMA) so ARM
            // stays bit-identical to the scalar path.
            debug_assert_eq!(offset, 1);
            let vz = vdupq_n_f32(0.0);
            let mut i = 1;
            while i + 4 <= len && 2 * i + 9 <= x0.len() {
                let mc = vld2q_f32(x0.as_ptr().add(2 * i - 1));
                let pp = vld2q_f32(x0.as_ptr().add(2 * i + 1));
                let mut val = vaddq_f32(vmulq_f32(mc.0, v025), vmulq_f32(pp.0, v025));
                val = vaddq_f32(val, vmulq_f32(mc.1, v05));
                vst1q_f32(x_lp.as_mut_ptr().add(i), vaddq_f32(vz, val));
                i += 4;
            }

            while i < len {
                let idx_m = 2 * i - offset;
                let idx_p = 2 * i + offset;
                let idx_c = 2 * i;

                let mut val = 0.0f32;
                if idx_p < x0.len() {
                    val += 0.25 * x0[idx_m] + 0.25 * x0[idx_p] + 0.5 * x0[idx_c];
                }
                x_lp[i] = val;
                i += 1;
            }
        } else {
            let x0 = x[0];
            let x1 = x[1];
            let mut i = 1;
            while i < len {
                let idx_m = 2 * i - offset;
                let idx_p = 2 * i + offset;
                let idx_c = 2 * i;

                let mut val = 0.0f32;
                if idx_p < x0.len() {
                    val += 0.25 * x0[idx_m] + 0.25 * x0[idx_p] + 0.5 * x0[idx_c];
                }
                if idx_p < x1.len() {
                    val += 0.25 * x1[idx_m] + 0.25 * x1[idx_p] + 0.5 * x1[idx_c];
                }
                x_lp[i] = val;
                i += 1;
            }
        }
    }

    pitch_downsample_boundary(x, x_lp, c, offset);
    pitch_lp_whiten(x_lp, len);
}

#[inline(always)]
fn find_best_pitch(
    xcorr: &[f32],
    y: &[f32],
    len: usize,
    max_pitch: usize,
    best_pitch: &mut [usize; 2],
) {
    assert!(
        y.len() >= len && xcorr.len() >= max_pitch,
        "find_best_pitch: out of range"
    );
    let mut best_num = [-1.0f32, -1.0f32];
    let mut best_den = [0.0f32, 0.0f32];

    best_pitch[0] = 0;
    best_pitch[1] = 1;

    #[cfg(target_arch = "aarch64")]
    let mut syy = if !crate::isa::neon() {
        let mut sum = 1.0f32;
        for j in 0..len {
            sum += y[j] * y[j];
        }
        sum
    } else {
        // SAFETY: NEON confirmed by `isa::neon()` in the condition above. The
        // loads read `y[j..j + 4]` only while `j + 4 <= len`, so they need
        // `y.len() >= len`, which the `assert!` at the top of this function
        // enforces.
        unsafe {
            use std::arch::aarch64::*;
            let mut sum_vec = vdupq_n_f32(0.0);
            let mut j = 0;
            while j + 16 <= len {
                let y0 = vld1q_f32(y.as_ptr().add(j));
                let y1 = vld1q_f32(y.as_ptr().add(j + 4));
                let y2 = vld1q_f32(y.as_ptr().add(j + 8));
                let y3 = vld1q_f32(y.as_ptr().add(j + 12));
                sum_vec = vfmaq_f32(sum_vec, y0, y0);
                sum_vec = vfmaq_f32(sum_vec, y1, y1);
                sum_vec = vfmaq_f32(sum_vec, y2, y2);
                sum_vec = vfmaq_f32(sum_vec, y3, y3);
                j += 16;
            }
            while j + 4 <= len {
                let y0 = vld1q_f32(y.as_ptr().add(j));
                sum_vec = vfmaq_f32(sum_vec, y0, y0);
                j += 4;
            }
            let mut sum = 1.0f32 + vaddvq_f32(sum_vec);
            while j < len {
                sum += y[j] * y[j];
                j += 1;
            }
            sum
        }
    };
    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    // SAFETY: AVX intrinsics run only inside `if crate::isa::avx()`; the else
    // arm is plain scalar code. The 8-wide loads read `y[j..j + 8]` only while
    // `j + 8 <= len`, so they need `y.len() >= len`, which the `assert!` at
    // the top of this function enforces. Note: this block is
    // not in a `#[target_feature(enable = "avx")]` fn, so the `_mm256_*`
    // intrinsics are called from a context without AVX enabled at compile
    // time (they still execute correctly after the runtime check, but may not
    // inline).
    let mut syy = unsafe {
        if crate::isa::avx() {
            #[cfg(target_arch = "x86")]
            use std::arch::x86::*;
            #[cfg(target_arch = "x86_64")]
            use std::arch::x86_64::*;
            let mut acc0 = _mm256_setzero_ps();
            let mut acc1 = _mm256_setzero_ps();
            let mut j = 0;
            while j + 16 <= len {
                let y0 = _mm256_loadu_ps(y.as_ptr().add(j));
                let y1 = _mm256_loadu_ps(y.as_ptr().add(j + 8));
                acc0 = _mm256_add_ps(acc0, _mm256_mul_ps(y0, y0));
                acc1 = _mm256_add_ps(acc1, _mm256_mul_ps(y1, y1));
                j += 16;
            }
            while j + 8 <= len {
                let y0 = _mm256_loadu_ps(y.as_ptr().add(j));
                acc0 = _mm256_add_ps(acc0, _mm256_mul_ps(y0, y0));
                j += 8;
            }
            let acc = _mm256_add_ps(acc0, acc1);
            let hi = _mm256_extractf128_ps(acc, 1);
            let lo = _mm256_castps256_ps128(acc);
            let sum4 = _mm_add_ps(lo, hi);
            let tmp = _mm_movehl_ps(sum4, sum4);
            let sum2 = _mm_add_ps(sum4, tmp);
            let tmp2 = _mm_shuffle_ps(sum2, sum2, 0x55);
            let mut sum = 1.0f32 + _mm_cvtss_f32(_mm_add_ss(sum2, tmp2));
            while j < len {
                sum += y[j] * y[j];
                j += 1;
            }
            sum
        } else {
            let mut sum = 1.0f32;
            for j in 0..len {
                sum += y[j] * y[j];
            }
            sum
        }
    };
    #[cfg(not(any(target_arch = "aarch64", target_arch = "x86", target_arch = "x86_64")))]
    let mut syy = {
        let mut sum = 1.0f32;
        for j in 0..len {
            sum += y[j] * y[j];
        }
        sum
    };

    for i in 0..max_pitch {
        if xcorr[i] > 0.0 {
            let num = xcorr[i] * xcorr[i];
            if num * best_den[1] > best_num[1] * syy {
                if num * best_den[0] > best_num[0] * syy {
                    best_num[1] = best_num[0];
                    best_den[1] = best_den[0];
                    best_pitch[1] = best_pitch[0];
                    best_num[0] = num;
                    best_den[0] = syy;
                    best_pitch[0] = i;
                } else {
                    best_num[1] = num;
                    best_den[1] = syy;
                    best_pitch[1] = i;
                }
            }
        }
        syy += y[i + len] * y[i + len] - y[i] * y[i];
        if syy < 1.0 {
            syy = 1.0;
        }
    }
}

pub fn pitch_search(x_lp: &[f32], y: &[f32], mut len: usize, mut max_pitch: usize) -> usize {
    let mut best_pitch = [0, 0];

    max_pitch >>= 1;
    len >>= 1;
    let lag = len + max_pitch;
    let len4 = len >> 1;
    let lag4 = lag >> 1;

    const MAX_LEN4: usize = 512;
    const MAX_LAG4: usize = 1024;
    const MAX_PITCH: usize = 512;

    let mut x_lp4_stack = [0.0f32; MAX_LEN4];
    let mut y_lp4_stack = [0.0f32; MAX_LAG4];
    let mut xcorr_stack = [0.0f32; MAX_PITCH];

    let x_lp4: &mut [f32];
    let y_lp4: &mut [f32];
    let xcorr: &mut [f32];

    if len4 <= MAX_LEN4 && lag4 <= MAX_LAG4 && max_pitch <= MAX_PITCH {
        x_lp4 = &mut x_lp4_stack[..len4];
        y_lp4 = &mut y_lp4_stack[..lag4];
        xcorr = &mut xcorr_stack[..max_pitch];
    } else {
        return pitch_search_heap(x_lp, y, len << 1, max_pitch << 1);
    }

    for j in 0..len4 {
        x_lp4[j] = x_lp[2 * j];
    }
    for j in 0..lag4 {
        y_lp4[j] = y[2 * j];
    }

    pitch_xcorr(x_lp4, y_lp4, xcorr, len >> 1, max_pitch >> 1);

    find_best_pitch(xcorr, y_lp4, len >> 1, max_pitch >> 1, &mut best_pitch);

    for i in 0..max_pitch {
        // Lags outside the +-2 windows stay 0 (libopus pitch_search sets
        // xcorr[i]=0 before `continue`). -1 changed the pseudo-interpolation
        // below whenever the best lag sat on a window edge (its outer
        // neighbour is a skipped lag): a different `offset`, a different lag.
        xcorr[i] = 0.0;
        if (i as i32 - 2 * best_pitch[0] as i32).abs() > 2
            && (i as i32 - 2 * best_pitch[1] as i32).abs() > 2
        {
            continue;
        }
        xcorr[i] = inner_prod(x_lp, &y[i..], len);
        if xcorr[i] < -1.0 {
            xcorr[i] = -1.0;
        }
    }

    find_best_pitch(xcorr, y, len, max_pitch, &mut best_pitch);

    let mut offset = 0;
    if best_pitch[0] > 0 && best_pitch[0] < max_pitch - 1 {
        let a = xcorr[best_pitch[0] - 1];
        let b = xcorr[best_pitch[0]];
        let c = xcorr[best_pitch[0] + 1];
        if (c - a) > 0.7 * (b - a) {
            offset = 1;
        } else if (a - c) > 0.7 * (b - c) {
            offset = -1;
        }
    }

    ((2 * best_pitch[0]) as isize).wrapping_sub(offset as isize) as usize
}

fn pitch_search_heap(x_lp: &[f32], y: &[f32], mut len: usize, mut max_pitch: usize) -> usize {
    let mut best_pitch = [0, 0];

    max_pitch >>= 1;
    len >>= 1;
    let lag = len + max_pitch;

    let mut x_lp4 = vec![0.0f32; len >> 1];
    let mut y_lp4 = vec![0.0f32; lag >> 1];
    let mut xcorr = vec![0.0f32; max_pitch];

    for j in 0..(len >> 1) {
        x_lp4[j] = x_lp[2 * j];
    }
    for j in 0..(lag >> 1) {
        y_lp4[j] = y[2 * j];
    }

    pitch_xcorr(&x_lp4, &y_lp4, &mut xcorr, len >> 1, max_pitch >> 1);

    find_best_pitch(&xcorr, &y_lp4, len >> 1, max_pitch >> 1, &mut best_pitch);

    for i in 0..max_pitch {
        // Lags outside the +-2 windows stay 0 (libopus pitch_search sets
        // xcorr[i]=0 before `continue`). -1 changed the pseudo-interpolation
        // below whenever the best lag sat on a window edge (its outer
        // neighbour is a skipped lag): a different `offset`, a different lag.
        xcorr[i] = 0.0;
        if (i as i32 - 2 * best_pitch[0] as i32).abs() > 2
            && (i as i32 - 2 * best_pitch[1] as i32).abs() > 2
        {
            continue;
        }
        xcorr[i] = inner_prod(x_lp, &y[i..], len);
        if xcorr[i] < -1.0 {
            xcorr[i] = -1.0;
        }
    }

    find_best_pitch(&xcorr, y, len, max_pitch, &mut best_pitch);

    let mut offset = 0;
    if best_pitch[0] > 0 && best_pitch[0] < max_pitch - 1 {
        let a = xcorr[best_pitch[0] - 1];
        let b = xcorr[best_pitch[0]];
        let c = xcorr[best_pitch[0] + 1];
        if (c - a) > 0.7 * (b - a) {
            offset = 1;
        } else if (a - c) > 0.7 * (b - c) {
            offset = -1;
        }
    }

    ((2 * best_pitch[0]) as isize).wrapping_sub(offset as isize) as usize
}

fn compute_pitch_gain(xy: f32, xx: f32, yy: f32) -> f32 {
    if xy <= 0.0 || xx <= 0.0 || yy <= 0.0 {
        return 0.0;
    }
    xy / (1.0 + xx * yy).sqrt()
}

static SECOND_CHECK: [usize; 16] = [0, 0, 3, 2, 3, 2, 5, 2, 3, 2, 3, 2, 5, 2, 3, 2];

#[inline(always)]
fn sum_squares(x: &[f32], n: usize) -> f32 {
    assert!(x.len() >= n, "sum_squares: n = {n} > x.len()");
    #[cfg(target_arch = "aarch64")]
    if crate::isa::neon() {
        // SAFETY: `isa::neon()` confirmed NEON (aarch64 baseline).
        // `inner_prod_neon` needs `x.len() >= n`, which the `assert!` at the
        // top of this function enforces.
        return unsafe { inner_prod_neon(x, x, n) };
    }
    {
        let mut sum = 0.0f32;
        for i in 0..n {
            sum += x[i] * x[i];
        }
        sum
    }
}

pub fn remove_doubling(
    x: &[f32],
    mut max_period: usize,
    mut min_period: usize,
    mut n: usize,
    t0_ptr: &mut usize,
    mut prev_period: usize,
    prev_gain: f32,
) -> f32 {
    let min_period0 = min_period;
    max_period /= 2;
    min_period /= 2;
    *t0_ptr /= 2;
    prev_period /= 2;
    n /= 2;

    let x_target = &x[max_period..];

    if *t0_ptr >= max_period {
        *t0_ptr = max_period - 1;
    }

    let mut t = *t0_ptr;
    let t0 = *t0_ptr;

    const MAX_YY_SIZE: usize = 1024;
    let mut yy_lookup_buf = [0.0f32; MAX_YY_SIZE];
    let yy_lookup = &mut yy_lookup_buf[..=max_period];

    let xx = sum_squares(x_target, n);
    let xy = inner_prod(x_target, &x[max_period - t0..], n);

    yy_lookup[0] = xx;
    let mut yy_curr = xx;
    for i in 1..=max_period {
        yy_curr = yy_curr + x[max_period - i] * x[max_period - i]
            - x[max_period + n - i] * x[max_period + n - i];
        if yy_curr < 0.0 {
            yy_curr = 0.0;
        }
        yy_lookup[i] = yy_curr;
    }

    let mut best_xy = xy;
    let mut best_yy = yy_lookup[t0];
    let mut g = compute_pitch_gain(best_xy, xx, best_yy);
    let g0 = g;

    for k in 2..=15 {
        let t1 = (2 * t0 + k) / (2 * k);
        if t1 < min_period {
            break;
        }

        let t1b;
        if k == 2 {
            if t1 + t0 > max_period {
                t1b = t0;
            } else {
                t1b = t0 + t1;
            }
        } else {
            t1b = (2 * SECOND_CHECK[k] * t0 + k) / (2 * k);
        }

        let (xy_a, xy_b) =
            dual_inner_prod(x_target, &x[max_period - t1..], &x[max_period - t1b..], n);
        let xy_new = 0.5 * (xy_a + xy_b);
        let yy_new = 0.5 * (yy_lookup[t1] + yy_lookup[t1b]);
        let g1 = compute_pitch_gain(xy_new, xx, yy_new);

        let mut cont = 0.0f32;
        if (t1 as i32 - prev_period as i32).abs() <= 1 {
            cont = prev_gain;
        } else if (t1 as i32 - prev_period as i32).abs() <= 2 && 5 * k * k < t0 {
            cont = 0.5 * prev_gain;
        }

        let mut thresh = (0.7 * g0 - cont).max(0.3);
        if t1 < 3 * min_period {
            thresh = (0.85 * g0 - cont).max(0.4);
        } else if t1 < 2 * min_period {
            thresh = (0.9 * g0 - cont).max(0.5);
        }

        if g1 > thresh {
            best_xy = xy_new;
            best_yy = yy_new;
            t = t1;
            g = g1;
        }
    }

    best_xy = best_xy.max(0.0);
    let pg = if best_yy <= best_xy {
        1.0f32
    } else {
        best_xy / (best_yy + 1.0)
    };

    let mut xcorr_res = [0.0f32; 3];
    for k_idx in 0..3 {
        let lag = (t as i32 + k_idx as i32 - 1) as usize;
        xcorr_res[k_idx] = inner_prod(x_target, &x[max_period - lag..], n);
    }

    let mut offset = 0;
    if (xcorr_res[2] - xcorr_res[0]) > 0.7 * (xcorr_res[1] - xcorr_res[0]) {
        offset = 1;
    } else if (xcorr_res[0] - xcorr_res[2]) > 0.7 * (xcorr_res[1] - xcorr_res[2]) {
        offset = -1;
    }

    let pg = pg.min(g);
    *t0_ptr = (2 * t as i32 + offset) as usize;
    if *t0_ptr < min_period0 {
        *t0_ptr = min_period0;
    }

    pg
}

/// SIMD-vs-scalar oracle: every pitch dispatcher, capped at the scalar rung vs
/// the host's best kernel (`crate::isa`), over random sizes and contents.
#[cfg(test)]
mod isa_oracle {
    use super::*;
    use crate::isa::oracle::{Rng, both, close, close_slices};

    #[test]
    fn inner_prod_and_dual_match_scalar() {
        let mut r = Rng(0x1111_2222_3333_4444);
        for _ in 0..crate::isa::oracle::iters(2000) {
            let n = 1 + r.below(1200);
            let (x, y1, y2) = (r.vec(n, 3000.0), r.vec(n, 3000.0), r.vec(n, 3000.0));
            let scale1: f32 = x.iter().zip(&y1).map(|(a, b)| (a * b).abs()).sum();
            let scale2: f32 = x.iter().zip(&y2).map(|(a, b)| (a * b).abs()).sum();
            let (s, c) = both(|| inner_prod(&x, &y1, n));
            close(s, c, scale1, &format!("inner_prod n={n}"));
            let ((s1, s2), (c1, c2)) = both(|| dual_inner_prod(&x, &y1, &y2, n));
            close(s1, c1, scale1, &format!("dual_inner_prod.0 n={n}"));
            close(s2, c2, scale2, &format!("dual_inner_prod.1 n={n}"));
        }
    }

    /// The NEON downsampler keeps the scalar's op order (no FMA); the whitening
    /// after it goes through the (reassociating) xcorr, so the gate is float-close.
    /// Its mono path once loaded x[2i-1..] CONTIGUOUSLY where the
    /// taps are stride 2: three of four lanes wrong, ARM only, caught by the
    /// end-to-end ARM-vs-x86 sweep because no test could reach the scalar twin.
    #[test]
    fn pitch_downsample_matches_scalar() {
        let mut r = Rng(0x0dd5_a3b1_e000_0001);
        for _ in 0..crate::isa::oracle::iters(400) {
            let len = 64 + r.below(1100); // callers: >= 512 (prefilter), 1024 (PLC)
            for c in 1..=2usize {
                let n = 2 * len + r.below(3);
                let chans: Vec<Vec<f32>> = (0..c).map(|_| r.vec(n, 20000.0)).collect();
                let x: Vec<&[f32]> = chans.iter().map(std::vec::Vec::as_slice).collect();
                let (s, sc) = both(|| {
                    let mut o = vec![0.0f32; len];
                    pitch_downsample(&x, &mut o, len, c, 2);
                    o
                });
                close_slices(&s, &sc, &format!("pitch_downsample len={len} c={c}"));
            }
        }
    }

    #[test]
    fn pitch_xcorr_matches_scalar() {
        let mut r = Rng(0x5555_6666_7777_8888);
        for _ in 0..crate::isa::oracle::iters(300) {
            let len = 4 + r.below(1024);
            let max_pitch = 1 + r.below(400);
            let x = r.vec(len, 2000.0);
            let y = r.vec(len + max_pitch, 2000.0);
            let (s, c) = both(|| {
                let mut o = vec![0.0f32; max_pitch];
                pitch_xcorr(&x, &y, &mut o, len, max_pitch);
                o
            });
            for k in 0..max_pitch {
                let scale: f32 = (0..len).map(|i| (x[i] * y[i + k]).abs()).sum();
                close(s[k], c[k], scale, &format!("pitch_xcorr len={len} lag={k}"));
            }
        }
    }

    /// Integer output from float scores: a reassociated score can flip a
    /// near-tie, so require agreement on (almost) every trial, not all.
    #[test]
    fn find_best_pitch_matches_scalar() {
        let mut r = Rng(0x9999_aaaa_bbbb_cccc);
        let (mut trials, mut differ) = (0, 0);
        for _ in 0..crate::isa::oracle::iters(2000) {
            let len = 8 + r.below(512);
            let max_pitch = 2 + r.below(300);
            let xcorr = r.vec(max_pitch, 1e6);
            let y = r.vec(len + max_pitch, 1000.0);
            let (s, c) = both(|| {
                let mut b = [0usize; 2];
                find_best_pitch(&xcorr, &y, len, max_pitch, &mut b);
                b
            });
            trials += 1;
            differ += (s != c) as u32;
        }
        assert!(
            differ * 200 <= trials,
            "find_best_pitch: {differ}/{trials} trials differ"
        );
    }
}
