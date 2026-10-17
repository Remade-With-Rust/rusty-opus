use crate::pitch::pitch_xcorr;

pub fn lpc(lpc: &mut [f32], ac: &[f32], p: usize) {
    let mut error = ac[0];
    if error <= 1e-10 {
        for x in lpc.iter_mut() {
            *x = 0.0;
        }
        return;
    }

    for i in 0..p {
        let mut rr = 0.0f32;
        for j in 0..i {
            rr += lpc[j] * ac[i - j];
        }
        rr += ac[i + 1];
        let r = -rr / error;

        lpc[i] = r;
        for j in 0..i.div_ceil(2) {
            let tmp1 = lpc[j];
            let tmp2 = lpc[i - 1 - j];
            lpc[j] = tmp1 + r * tmp2;
            lpc[i - 1 - j] = tmp2 + r * tmp1;
        }

        error = error - r * r * error;

        if error <= 0.001 * ac[0] {
            break;
        }
    }
}

pub fn autocorr(
    x: &[f32],
    ac: &mut [f32],
    window: Option<&[f32]>,
    overlap: usize,
    lag: usize,
    n: usize,
) {
    let xx_vec;
    let xx: &[f32] = if let Some(win) = window {
        if x.len() < n {
            return;
        }
        xx_vec = {
            let mut v = x[0..n].to_vec();
            for i in 0..overlap {
                v[i] *= win[i];
                v[n - 1 - i] *= win[i];
            }
            v
        };
        &xx_vec
    } else {
        &x[0..n]
    };

    let fast_n = n - lag;

    pitch_xcorr(xx, xx, ac, fast_n, lag + 1);

    for k in 0..=lag {
        let mut d = 0.0f32;
        for i in (k + fast_n)..n {
            d += xx[i] * xx[i - k];
        }
        ac[k] += d;
    }
}

pub fn celt_fir(x: &[f32], num: &[f32], y: &mut [f32], n: usize, ord: usize) {
    assert!(
        num.len() >= ord && x.len() >= n && y.len() >= n,
        "celt_fir: n/ord out of range"
    );
    #[cfg(target_arch = "aarch64")]
    if crate::isa::neon() {
        // SAFETY: NEON is baseline on aarch64 and `isa::neon()` confirmed it.
        // `celt_fir_neon` additionally requires `num.len() >= ord` (its unchecked
        // 4-wide tap loads reach `num[ord - 1]`); its `x` loads stay inside
        // `x[..i]` with `i < n`, and `x[i]` / `y[i]` are bounds-checked.
        // `num.len() >= ord` is enforced by the `assert!` at the top of this
        // function.
        unsafe { celt_fir_neon(x, num, y, n, ord) };
        return;
    }
    {
        for i in 0..n {
            let mut sum = x[i];
            for j in 0..ord {
                if i > j {
                    sum += num[j] * x[i - j - 1];
                }
            }
            y[i] = sum;
        }
    }
}

pub fn celt_iir(x: &[f32], den: &[f32], y: &mut [f32], n: usize, ord: usize, mem: &mut [f32]) {
    assert!(
        den.len() >= ord && mem.len() >= ord && x.len() >= n && y.len() >= n,
        "celt_iir: n/ord out of range"
    );
    #[cfg(target_arch = "aarch64")]
    if crate::isa::neon() {
        // SAFETY: NEON is baseline on aarch64 and `isa::neon()` confirmed it.
        // `celt_iir_neon` additionally requires `den.len() >= ord` and
        // `mem.len() >= ord` (its unchecked 4-wide loads reach index `ord - 1`
        // of both); `x[i]` / `y[i]` are bounds-checked. Both lengths are
        // enforced by the `assert!` at the top of this function.
        unsafe { celt_iir_neon(x, den, y, n, ord, mem) };
        return;
    }
    {
        for i in 0..n {
            let mut sum = x[i];
            for j in 0..ord {
                sum -= den[j] * mem[j];
            }
            for j in (1..ord).rev() {
                mem[j] = mem[j - 1];
            }
            mem[0] = sum;
            y[i] = sum;
        }
    }
}

/// NEON twin of the scalar `celt_fir` loop.
///
/// # Safety
///
/// - The CPU must support NEON (baseline on aarch64; gate on `isa::neon()`).
/// - `num.len() >= ord`: the 4-wide tap loads read `num[j..j + 4]` for every
///   `j + 4 <= ord` without bounds checks.
/// - The `x` loads read `x[i - j - 4..i - j]` only when `i > j + 3`, so they
///   stay within `x[..i]`; `x[i]`, `y[i]` and the scalar tail are
///   bounds-checked (a short `x` / `y` panics rather than reading out of bounds).
#[cfg(target_arch = "aarch64")]
#[inline(always)]
#[allow(unsafe_op_in_unsafe_fn)]
unsafe fn celt_fir_neon(x: &[f32], num: &[f32], y: &mut [f32], n: usize, ord: usize) {
    use std::arch::aarch64::*;

    if ord < 4 {
        for i in 0..n {
            let mut sum = x[i];
            for j in 0..ord {
                if i > j {
                    sum += num[j] * x[i - j - 1];
                }
            }
            y[i] = sum;
        }
        return;
    }

    for i in 0..n {
        let mut sum = vdupq_n_f32(x[i]);

        let mut j = 0;
        while j + 4 <= ord && i > j + 3 {
            let coeff = vld1q_f32(num.as_ptr().add(j));
            let x_vals = vld1q_f32(x.as_ptr().add(i - j - 4));
            let x_reversed = vrev64q_f32(x_vals);
            let x_reversed = vextq_f32(x_reversed, x_reversed, 2);
            sum = vfmaq_f32(sum, coeff, x_reversed);
            j += 4;
        }

        let sum_low = vget_low_f32(sum);
        let sum_high = vget_high_f32(sum);
        let sum_pair = vadd_f32(sum_low, sum_high);
        let mut result = vget_lane_f32(sum_pair, 0) + vget_lane_f32(sum_pair, 1);

        while j < ord {
            if i > j {
                result += num[j] * x[i - j - 1];
            }
            j += 1;
        }

        y[i] = result;
    }
}

/// NEON twin of the scalar `celt_iir` loop.
///
/// # Safety
///
/// - The CPU must support NEON (baseline on aarch64; gate on `isa::neon()`).
/// - `den.len() >= ord` and `mem.len() >= ord`: the 4-wide loads read
///   `den[j..j + 4]` and `mem[j..j + 4]` for every `j + 4 <= ord` without
///   bounds checks. `x[i]`, `y[i]` and the scalar tail are bounds-checked.
#[cfg(target_arch = "aarch64")]
#[inline(always)]
#[allow(unsafe_op_in_unsafe_fn)]
unsafe fn celt_iir_neon(
    x: &[f32],
    den: &[f32],
    y: &mut [f32],
    n: usize,
    ord: usize,
    mem: &mut [f32],
) {
    use std::arch::aarch64::*;

    if ord < 4 {
        for i in 0..n {
            let mut sum = x[i];
            for j in 0..ord {
                sum -= den[j] * mem[j];
            }
            for j in (1..ord).rev() {
                mem[j] = mem[j - 1];
            }
            mem[0] = sum;
            y[i] = sum;
        }
        return;
    }

    for i in 0..n {
        let mut feedback = vdupq_n_f32(0.0);

        let mut j = 0;
        while j + 4 <= ord {
            let coeff = vld1q_f32(den.as_ptr().add(j));
            let mem_vals = vld1q_f32(mem.as_ptr().add(j));
            feedback = vfmaq_f32(feedback, coeff, mem_vals);
            j += 4;
        }

        let fb_low = vget_low_f32(feedback);
        let fb_high = vget_high_f32(feedback);
        let fb_pair = vadd_f32(fb_low, fb_high);
        let mut fb_sum = vget_lane_f32(fb_pair, 0) + vget_lane_f32(fb_pair, 1);

        while j < ord {
            fb_sum += den[j] * mem[j];
            j += 1;
        }

        let sum = x[i] - fb_sum;

        for j in (1..ord).rev() {
            mem[j] = mem[j - 1];
        }
        mem[0] = sum;
        y[i] = sum;
    }
}

// ---- libopus scalar (celt_lpc.c / pitch.h C reference) accumulation order ----
// The CELT concealment LPC is ill-conditioned (-40 dB floor), so float rounding
// order decides the output: these mirror the C reference build exactly
// (xcorr_kernel_c: each lag summed sequentially; celt_fir_c / celt_iir with
// reversed taps, unrolled by 4 with the IIR patch-up terms). PLC-only.

/// `xcorr_kernel_c`: sum[k] += x[j] * y[j + k], j ascending.
#[inline]
fn xcorr_kernel_c(x: &[f32], y: &[f32], sum: &mut [f32; 4], len: usize) {
    for j in 0..len {
        let t = x[j];
        sum[0] += t * y[j];
        sum[1] += t * y[j + 1];
        sum[2] += t * y[j + 2];
        sum[3] += t * y[j + 3];
    }
}

/// Bounds of the `*_c_order` helpers' stack scratch: they serve the CELT PLC
/// (n <= MAX_PERIOD 1024 or frame + overlap <= 1080, order 24).
const ORDER_HELPER_MAX_N: usize = 2048;
const ORDER_HELPER_MAX_ORD: usize = 32;

/// `_celt_autocorr` (float) with `celt_pitch_xcorr_c` + the split tail.
pub fn autocorr_c_order(
    x: &[f32],
    ac: &mut [f32],
    window: &[f32],
    overlap: usize,
    lag: usize,
    n: usize,
) {
    // Stack scratch (PLC-only helper: n = MAX_PERIOD = 1024).
    let mut xx_buf = [0.0f32; ORDER_HELPER_MAX_N];
    let xx = &mut xx_buf[..n];
    xx.copy_from_slice(&x[..n]);
    for i in 0..overlap {
        let w = window[i];
        xx[i] = x[i] * w;
        xx[n - i - 1] = x[n - i - 1] * w;
    }
    let fast_n = n - lag;
    let max_pitch = lag + 1;
    let mut i = 0;
    while i + 3 < max_pitch {
        let mut sum = [0.0f32; 4];
        xcorr_kernel_c(xx, &xx[i..], &mut sum, fast_n);
        ac[i..i + 4].copy_from_slice(&sum);
        i += 4;
    }
    while i < max_pitch {
        let mut s = 0.0f32;
        for j in 0..fast_n {
            s += xx[j] * xx[i + j];
        }
        ac[i] = s;
        i += 1;
    }
    for k in 0..=lag {
        let mut d = 0.0f32;
        for i in (k + fast_n)..n {
            d += xx[i] * xx[i - k];
        }
        ac[k] += d;
    }
}

/// `celt_fir_c`: `x` carries `ord` history samples before the `n` inputs; `y`
/// receives the `n` outputs.
pub fn celt_fir_c_order(x: &[f32], num: &[f32], y: &mut [f32], n: usize, ord: usize) {
    let mut rnum_buf = [0.0f32; ORDER_HELPER_MAX_ORD];
    let rnum = &mut rnum_buf[..ord];
    for (i, r) in rnum.iter_mut().enumerate() {
        *r = num[ord - i - 1];
    }
    let rnum = &*rnum;
    let mut i = 0;
    while i + 3 < n {
        let mut sum = [x[ord + i], x[ord + i + 1], x[ord + i + 2], x[ord + i + 3]];
        xcorr_kernel_c(rnum, &x[i..], &mut sum, ord);
        y[i..i + 4].copy_from_slice(&sum);
        i += 4;
    }
    while i < n {
        let mut sum = x[ord + i];
        for j in 0..ord {
            sum += rnum[j] * x[i + j];
        }
        y[i] = sum;
        i += 1;
    }
}

/// `celt_iir` (non-SMALL_FOOTPRINT float path), including its tail-loop sign.
pub fn celt_iir_c_order(
    x: &[f32],
    den: &[f32],
    y: &mut [f32],
    n: usize,
    ord: usize,
    mem: &mut [f32],
) {
    let mut rden_buf = [0.0f32; ORDER_HELPER_MAX_ORD];
    for i in 0..ord {
        rden_buf[i] = den[ord - i - 1];
    }
    let rden = &rden_buf[..ord];
    let mut yb_buf = [0.0f32; ORDER_HELPER_MAX_N + ORDER_HELPER_MAX_ORD];
    let yb = &mut yb_buf[..n + ord];
    for i in 0..ord {
        yb[i] = -mem[ord - i - 1];
    }
    let mut i = 0;
    while i + 3 < n {
        let mut sum = [x[i], x[i + 1], x[i + 2], x[i + 3]];
        xcorr_kernel_c(rden, &yb[i..], &mut sum, ord);
        yb[i + ord] = -sum[0];
        y[i] = sum[0];
        sum[1] += yb[i + ord] * den[0];
        yb[i + ord + 1] = -sum[1];
        y[i + 1] = sum[1];
        sum[2] += yb[i + ord + 1] * den[0];
        sum[2] += yb[i + ord] * den[1];
        yb[i + ord + 2] = -sum[2];
        y[i + 2] = sum[2];
        sum[3] += yb[i + ord + 2] * den[0];
        sum[3] += yb[i + ord + 1] * den[1];
        sum[3] += yb[i + ord] * den[2];
        yb[i + ord + 3] = -sum[3];
        y[i + 3] = sum[3];
        i += 4;
    }
    while i < n {
        let mut sum = x[i];
        for j in 0..ord {
            sum -= rden[j] * yb[i + j];
        }
        yb[i + ord] = sum;
        y[i] = sum;
        i += 1;
    }
    for i in 0..ord {
        mem[i] = y[n - i - 1];
    }
}
