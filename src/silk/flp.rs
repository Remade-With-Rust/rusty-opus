//! Floating-point SILK analysis — a faithful port of libopus `silk/float/`.
//!
//! libopus ships two SILK encoders that share one bitstream: a fixed-point one
//! (`silk/fixed/`, which we already have) and a floating-point one
//! (`silk/float/`). They differ only in the *analysis* — pitch estimation,
//! Burg LPC, LTP, noise shaping, gains — while the noise-shaping quantizer
//! (NSQ) that actually produces the excitation is fixed-point in *both* builds.
//!
//! This module ports the float analysis. The output is written into the same
//! Q-format [`SilkEncoderControl`] and [`SideInfoIndices`] the fixed path
//! fills, so the rest of `enc_api` (bitrate loop, NSQ, entropy coding) runs
//! unchanged. The float→Q conversion at the NSQ boundary mirrors
//! `silk_NSQ_wrapper_FLP` in `wrappers_FLP.c`.
//!
//! Numeric fidelity to libopus: accumulators that C keeps in `double`
//! (inner products, energies, Schur, Burg, warped autocorrelation, the pitch
//! core) are kept in `f64` here; `silk_float` values are `f32`.

use crate::silk::define::*;
use crate::silk::gain_quant::{silk_gains_quant, silk_quant_ltp_gains};
use crate::silk::interpolate::silk_interpolate;
use crate::silk::nlsf::{silk_a2nlsf, silk_nlsf2a, silk_process_nlsfs};
use crate::silk::resampler::{silk_resampler_down2, silk_resampler_down2_3};
use crate::silk::structs::{SilkEncoderControl, SilkEncoderState};
use crate::silk::tables::{
    SILK_CB_LAGS_STAGE2, SILK_CB_LAGS_STAGE2_10_MS, SILK_CB_LAGS_STAGE3, SILK_CB_LAGS_STAGE3_10_MS,
    SILK_LAG_RANGE_STAGE3, SILK_LAG_RANGE_STAGE3_10_MS, SILK_LTP_SCALES_TABLE_Q14,
    SILK_NB_CBK_SEARCHS_STAGE3, SILK_QUANTIZATION_OFFSETS_Q10,
};
use crate::silk::tuning_parameters::*;

const SILK_MAX_ORDER_LPC: usize = 24;
const USE_HARM_SHAPING: i32 = 1;
const SHAPE_LPC_WIN_MAX: usize = 15 * MAX_FS_KHZ;
const FIND_PITCH_LPC_WIN_MAX: usize = (20 + (LA_PITCH_MS << 1)) * MAX_FS_KHZ;
const PE_SHORTLAG_BIAS: f32 = 0.2;
const PE_PREVLAG_BIAS: f32 = 0.2;
const PE_FLATCONTOUR_BIAS: f32 = 0.05;
const PE_D_SRCH_LENGTH_C: usize = PE_D_SRCH_LENGTH;
const SCRATCH_SIZE: usize = 22;

// ---------------------------------------------------------------------------
// Small scalar helpers matching the C macros.
// ---------------------------------------------------------------------------

#[inline]
fn float2int(x: f32) -> i32 {
    // libopus float2int() is lrintf: round to nearest, ties to even. Match it
    // (Rust's f32::round is ties-away, which shifts VQ decisions vs libopus).
    let r = (x as f64).round_ties_even();
    r as i32
}

#[inline]
fn sat16(x: i32) -> i32 {
    x.clamp(-32768, 32767)
}

#[inline]
fn float2short(x: f32) -> i16 {
    sat16(float2int(x)) as i16
}

#[inline]
fn silk_log2(x: f64) -> f32 {
    (3.321_928_094_887_362_f64 * x.log10()) as f32
}

#[inline]
fn silk_sigmoid(x: f32) -> f32 {
    (1.0 / (1.0 + (-x as f64).exp())) as f32
}

/// Row-major matrix element (C `matrix_ptr`/`matrix_c_ptr`).
#[inline]
fn mtx(base: &[f32], row: usize, col: usize, n: usize) -> f32 {
    base[row * n + col]
}
#[inline]
fn mtx_mut(base: &mut [f32], row: usize, col: usize, n: usize) -> &mut f32 {
    &mut base[row * n + col]
}

// ---------------------------------------------------------------------------
// Leaf DSP (SigProc_FLP).
// ---------------------------------------------------------------------------

fn inner_product(a: &[f32], b: &[f32], n: usize) -> f64 {
    let mut r = 0.0f64;
    let mut i = 0;
    while i + 3 < n {
        r += a[i] as f64 * b[i] as f64
            + a[i + 1] as f64 * b[i + 1] as f64
            + a[i + 2] as f64 * b[i + 2] as f64
            + a[i + 3] as f64 * b[i + 3] as f64;
        i += 4;
    }
    while i < n {
        r += a[i] as f64 * b[i] as f64;
        i += 1;
    }
    r
}

fn energy(a: &[f32], n: usize) -> f64 {
    inner_product(a, a, n)
}

fn autocorrelation(results: &mut [f32], input: &[f32], input_size: usize, mut count: usize) {
    if count > input_size {
        count = input_size;
    }
    for i in 0..count {
        results[i] = inner_product(input, &input[i..], input_size - i) as f32;
    }
}

fn scale_vector(data: &mut [f32], gain: f32, n: usize) {
    for v in data.iter_mut().take(n) {
        *v *= gain;
    }
}

fn scale_copy_vector(out: &mut [f32], inp: &[f32], gain: f32, n: usize) {
    for i in 0..n {
        out[i] = gain * inp[i];
    }
}

fn bwexpander(ar: &mut [f32], d: usize, chirp: f32) {
    let mut cfac = chirp;
    for i in 0..d - 1 {
        ar[i] *= cfac;
        cfac *= chirp;
    }
    ar[d - 1] *= cfac;
}

fn schur(refl_coef: &mut [f32], auto_corr: &[f32], order: usize) -> f32 {
    let mut c = [[0.0f64; 2]; SILK_MAX_ORDER_LPC + 1];
    for k in 0..=order {
        c[k][0] = auto_corr[k] as f64;
        c[k][1] = auto_corr[k] as f64;
    }
    for k in 0..order {
        let rc_tmp = -c[k + 1][0] / c[0][1].max(1e-9f32 as f64);
        refl_coef[k] = rc_tmp as f32;
        for n in 0..order - k {
            let ctmp1 = c[n + k + 1][0];
            let ctmp2 = c[n][1];
            c[n + k + 1][0] = ctmp1 + ctmp2 * rc_tmp;
            c[n][1] = ctmp2 + ctmp1 * rc_tmp;
        }
    }
    c[0][1] as f32
}

fn k2a(a: &mut [f32], rc: &[f32], order: usize) {
    for k in 0..order {
        let rck = rc[k];
        for n in 0..(k + 1) >> 1 {
            let tmp1 = a[n];
            let tmp2 = a[k - n - 1];
            a[n] = tmp1 + tmp2 * rck;
            a[k - n - 1] = tmp2 + tmp1 * rck;
        }
        a[k] = -rck;
    }
}

fn apply_sine_window(px_win: &mut [f32], px: &[f32], win_type: i32, length: usize) {
    let freq = std::f32::consts::PI / (length as f32 + 1.0);
    let c = 2.0 - freq * freq;
    let (mut s0, mut s1);
    if win_type < 2 {
        s0 = 0.0;
        s1 = freq;
    } else {
        s0 = 1.0;
        s1 = 0.5 * c;
    }
    let mut k = 0;
    while k < length {
        px_win[k] = px[k] * 0.5 * (s0 + s1);
        px_win[k + 1] = px[k + 1] * s1;
        s0 = c * s1 - s0;
        px_win[k + 2] = px[k + 2] * 0.5 * (s1 + s0);
        px_win[k + 3] = px[k + 3] * s0;
        s1 = c * s0 - s1;
        k += 4;
    }
}

fn warped_autocorrelation(corr: &mut [f32], input: &[f32], warping: f32, length: usize, order: usize) {
    let mut state = [0.0f64; MAX_SHAPE_LPC_ORDER + 1];
    let mut c = [0.0f64; MAX_SHAPE_LPC_ORDER + 1];
    let w = warping as f64;
    for n in 0..length {
        let mut tmp1 = input[n] as f64;
        let mut i = 0;
        while i < order {
            let tmp2 = state[i] + w * (state[i + 1] - tmp1);
            state[i] = tmp1;
            c[i] += state[0] * tmp1;
            tmp1 = state[i + 1] + w * (state[i + 2] - tmp2);
            state[i + 1] = tmp2;
            c[i + 1] += state[0] * tmp2;
            i += 2;
        }
        state[order] = tmp1;
        c[order] += state[0] * tmp1;
    }
    for i in 0..order + 1 {
        corr[i] = c[i] as f32;
    }
}

fn insertion_sort_decreasing(a: &mut [f32], idx: &mut [i32], l: usize, k: usize) {
    for i in 0..k {
        idx[i] = i as i32;
    }
    for i in 1..k {
        let value = a[i];
        let mut j = i as isize - 1;
        while j >= 0 && value > a[j as usize] {
            a[(j + 1) as usize] = a[j as usize];
            idx[(j + 1) as usize] = idx[j as usize];
            j -= 1;
        }
        a[(j + 1) as usize] = value;
        idx[(j + 1) as usize] = i as i32;
    }
    for i in k..l {
        let value = a[i];
        if value > a[k - 1] {
            let mut j = k as isize - 2;
            while j >= 0 && value > a[j as usize] {
                a[(j + 1) as usize] = a[j as usize];
                idx[(j + 1) as usize] = idx[j as usize];
                j -= 1;
            }
            a[(j + 1) as usize] = value;
            idx[(j + 1) as usize] = i as i32;
        }
    }
}

/// LPC residual: `r_LPC[ix] = s[ix] - sum_j s[ix-1-j]*PredCoef[j]`, first `order`
/// samples zeroed. `s` is indexed so that `s[0..length]` is the analysis window.
fn lpc_analysis_filter(r_lpc: &mut [f32], pred_coef: &[f32], s: &[f32], length: usize, order: usize) {
    for ix in order..length {
        let mut pred = 0.0f32;
        for j in 0..order {
            pred += s[ix - 1 - j] * pred_coef[j];
        }
        r_lpc[ix] = s[ix] - pred;
    }
    for v in r_lpc.iter_mut().take(order) {
        *v = 0.0;
    }
}

fn corr_matrix(x: &[f32], l: usize, order: usize, xx: &mut [f32]) {
    // ptr1 = &x[order-1]
    let p1 = order - 1;
    let mut e = energy(&x[p1..], l);
    *mtx_mut(xx, 0, 0, order) = e as f32;
    for j in 1..order {
        e += (x[p1 - j] as f64) * (x[p1 - j] as f64) - (x[p1 + l - j] as f64) * (x[p1 + l - j] as f64);
        *mtx_mut(xx, j, j, order) = e as f32;
    }
    // ptr2 = &x[order-2]
    for lag in 1..order {
        let p2 = order - 1 - lag;
        let mut energy = inner_product(&x[p1..], &x[p2..], l);
        *mtx_mut(xx, lag, 0, order) = energy as f32;
        *mtx_mut(xx, 0, lag, order) = energy as f32;
        for j in 1..(order - lag) {
            energy += (x[p1 - j] as f64) * (x[p2 - j] as f64)
                - (x[p1 + l - j] as f64) * (x[p2 + l - j] as f64);
            *mtx_mut(xx, lag + j, j, order) = energy as f32;
            *mtx_mut(xx, j, lag + j, order) = energy as f32;
        }
    }
}

fn corr_vector(x: &[f32], t: &[f32], l: usize, order: usize, xt: &mut [f32]) {
    // ptr1 = &x[order-1], decremented per lag
    for lag in 0..order {
        let p1 = order - 1 - lag;
        xt[lag] = inner_product(&x[p1..], t, l) as f32;
    }
}

// ---------------------------------------------------------------------------
// Burg LPC (burg_modified_FLP.c).
// ---------------------------------------------------------------------------

fn burg_modified(a: &mut [f32], x: &[f32], min_inv_gain: f32, subfr_length: usize, nb_subfr: usize, d: usize) -> f32 {
    let mut c_first_row = [0.0f64; SILK_MAX_ORDER_LPC];
    let mut c_last_row = [0.0f64; SILK_MAX_ORDER_LPC];
    let mut caf = [0.0f64; SILK_MAX_ORDER_LPC + 1];
    let mut cab = [0.0f64; SILK_MAX_ORDER_LPC + 1];
    let mut af = [0.0f64; SILK_MAX_ORDER_LPC];

    let mut c0 = energy(x, nb_subfr * subfr_length);
    for s in 0..nb_subfr {
        let xp = s * subfr_length;
        for n in 1..d + 1 {
            c_first_row[n - 1] += inner_product(&x[xp..], &x[xp + n..], subfr_length - n);
        }
    }
    c_last_row[..SILK_MAX_ORDER_LPC].copy_from_slice(&c_first_row[..SILK_MAX_ORDER_LPC]);

    caf[0] = c0 + FIND_LPC_COND_FAC as f64 * c0 + 1e-9;
    cab[0] = caf[0];
    let mut inv_gain = 1.0f64;
    let mut reached_max_gain = false;

    for n in 0..d {
        for s in 0..nb_subfr {
            let xp = s * subfr_length;
            let mut tmp1 = x[xp + n] as f64;
            let mut tmp2 = x[xp + subfr_length - n - 1] as f64;
            for k in 0..n {
                c_first_row[k] -= x[xp + n] as f64 * x[xp + n - k - 1] as f64;
                c_last_row[k] -=
                    x[xp + subfr_length - n - 1] as f64 * x[xp + subfr_length - n + k] as f64;
                let atmp = af[k];
                tmp1 += x[xp + n - k - 1] as f64 * atmp;
                tmp2 += x[xp + subfr_length - n + k] as f64 * atmp;
            }
            for k in 0..=n {
                caf[k] -= tmp1 * x[xp + n - k] as f64;
                cab[k] -= tmp2 * x[xp + subfr_length - n + k - 1] as f64;
            }
        }
        let mut tmp1 = c_first_row[n];
        let mut tmp2 = c_last_row[n];
        for k in 0..n {
            let atmp = af[k];
            tmp1 += c_last_row[n - k - 1] * atmp;
            tmp2 += c_first_row[n - k - 1] * atmp;
        }
        caf[n + 1] = tmp1;
        cab[n + 1] = tmp2;
        let mut num = cab[n + 1];
        let mut nrg_b = cab[0];
        let mut nrg_f = caf[0];
        for k in 0..n {
            let atmp = af[k];
            num += cab[n - k] * atmp;
            nrg_b += cab[k + 1] * atmp;
            nrg_f += caf[k + 1] * atmp;
        }
        let mut rc = -2.0 * num / (nrg_f + nrg_b);
        let tmp = inv_gain * (1.0 - rc * rc);
        if tmp <= min_inv_gain as f64 {
            rc = (1.0 - min_inv_gain as f64 / inv_gain).sqrt();
            if num > 0.0 {
                rc = -rc;
            }
            inv_gain = min_inv_gain as f64;
            reached_max_gain = true;
        } else {
            inv_gain = tmp;
        }
        for k in 0..(n + 1) >> 1 {
            let t1 = af[k];
            let t2 = af[n - k - 1];
            af[k] = t1 + rc * t2;
            af[n - k - 1] = t2 + rc * t1;
        }
        af[n] = rc;
        if reached_max_gain {
            for k in n + 1..d {
                af[k] = 0.0;
            }
            break;
        }
        for k in 0..=n + 1 {
            let t1 = caf[k];
            caf[k] += rc * cab[n - k + 1];
            cab[n - k + 1] += rc * t1;
        }
    }

    if reached_max_gain {
        for k in 0..d {
            a[k] = (-af[k]) as f32;
        }
        for s in 0..nb_subfr {
            c0 -= energy(&x[s * subfr_length..], d);
        }
        (c0 * inv_gain) as f32
    } else {
        let mut nrg_f = caf[0];
        let mut tmp1 = 1.0f64;
        for k in 0..d {
            let atmp = af[k];
            nrg_f += caf[k + 1] * atmp;
            tmp1 += atmp * atmp;
            a[k] = (-atmp) as f32;
        }
        nrg_f -= FIND_LPC_COND_FAC as f64 * c0 * tmp1;
        nrg_f as f32
    }
}

// ---------------------------------------------------------------------------
// Float control struct.
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct EncCtrlFlp {
    gains: [f32; MAX_NB_SUBFR],
    pred_coef: [[f32; MAX_LPC_ORDER]; 2],
    ltp_coef: [f32; LTP_ORDER * MAX_NB_SUBFR],
    ltp_scale: f32,
    pitch_l: [i32; MAX_NB_SUBFR],
    ar: [f32; MAX_NB_SUBFR * MAX_SHAPE_LPC_ORDER],
    lf_ma_shp: [f32; MAX_NB_SUBFR],
    lf_ar_shp: [f32; MAX_NB_SUBFR],
    tilt: [f32; MAX_NB_SUBFR],
    harm_shape_gain: [f32; MAX_NB_SUBFR],
    lambda: f32,
    input_quality: f32,
    coding_quality: f32,
    pred_gain: f32,
    ltp_red_cod_gain: f32,
    res_nrg: [f32; MAX_NB_SUBFR],
    gains_unq_q16: [i32; MAX_NB_SUBFR],
    last_gain_index_prev: i8,
}

impl Default for EncCtrlFlp {
    fn default() -> Self {
        EncCtrlFlp {
            gains: [0.0; MAX_NB_SUBFR],
            pred_coef: [[0.0; MAX_LPC_ORDER]; 2],
            ltp_coef: [0.0; LTP_ORDER * MAX_NB_SUBFR],
            ltp_scale: 0.0,
            pitch_l: [0; MAX_NB_SUBFR],
            ar: [0.0; MAX_NB_SUBFR * MAX_SHAPE_LPC_ORDER],
            lf_ma_shp: [0.0; MAX_NB_SUBFR],
            lf_ar_shp: [0.0; MAX_NB_SUBFR],
            tilt: [0.0; MAX_NB_SUBFR],
            harm_shape_gain: [0.0; MAX_NB_SUBFR],
            lambda: 0.0,
            input_quality: 0.0,
            coding_quality: 0.0,
            pred_gain: 0.0,
            ltp_red_cod_gain: 0.0,
            res_nrg: [0.0; MAX_NB_SUBFR],
            gains_unq_q16: [0; MAX_NB_SUBFR],
            last_gain_index_prev: 0,
        }
    }
}

// ---------------------------------------------------------------------------
// find_pitch_lags_FLP.
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
fn find_pitch_lags(ps_enc: &mut SilkEncoderState, ctrl: &mut EncCtrlFlp, res: &mut [f32], x_buf: &[f32]) {
    let cmn = &ps_enc.s_cmn;
    let fs_khz = cmn.fs_khz as usize;
    let la_pitch = cmn.la_pitch as usize;
    let frame_length = cmn.frame_length as usize;
    let ltp_mem_length = cmn.ltp_mem_length as usize;
    let pitch_lpc_win_length = cmn.pitch_lpc_win_length as usize;
    let pe_order = ps_enc.pitch_estimation_lpc_order as usize;

    let buf_len = la_pitch + frame_length + ltp_mem_length;
    // x_buf here is the whole float buffer (start == C's x_buf = x - ltp_mem_length).
    let mut wsig = [0.0f32; FIND_PITCH_LPC_WIN_MAX];
    let mut auto_corr = [0.0f32; MAX_FIND_PITCH_LPC_ORDER + 1];
    let mut a = [0.0f32; MAX_FIND_PITCH_LPC_ORDER];
    let mut refl_coef = [0.0f32; MAX_FIND_PITCH_LPC_ORDER];

    let base = buf_len - pitch_lpc_win_length;
    // First slope window.
    apply_sine_window(&mut wsig, &x_buf[base..], 1, la_pitch);
    // Flat middle (memcpy).
    let mid = pitch_lpc_win_length - (la_pitch << 1);
    wsig[la_pitch..la_pitch + mid].copy_from_slice(&x_buf[base + la_pitch..base + la_pitch + mid]);
    // Second slope window.
    apply_sine_window(
        &mut wsig[la_pitch + mid..],
        &x_buf[base + la_pitch + mid..],
        2,
        la_pitch,
    );

    autocorrelation(&mut auto_corr, &wsig, pitch_lpc_win_length, pe_order + 1);
    auto_corr[0] += auto_corr[0] * FIND_PITCH_WHITE_NOISE_FRACTION + 1.0;
    let res_nrg = schur(&mut refl_coef, &auto_corr, pe_order);
    ctrl.pred_gain = auto_corr[0] / res_nrg.max(1.0);
    k2a(&mut a, &refl_coef, pe_order);
    bwexpander(&mut a, pe_order, FIND_PITCH_BANDWIDTH_EXPANSION);

    lpc_analysis_filter(res, &a, x_buf, buf_len, pe_order);

    let signal_type = cmn.indices.signal_type as i32;
    if signal_type != TYPE_NO_VOICE_ACTIVITY && cmn.first_frame_after_reset == 0 {
        let mut thrhld = 0.6f32;
        thrhld -= 0.004 * pe_order as f32;
        thrhld -= 0.1 * cmn.speech_activity_q8 as f32 * (1.0 / 256.0);
        thrhld -= 0.15 * (cmn.prev_signal_type >> 1) as f32;
        thrhld -= 0.1 * cmn.input_tilt_q15 as f32 * (1.0 / 32768.0);

        let search_thres1 = cmn.pitch_estimation_threshold_q16 as f32 / 65536.0;
        let mut ltp_corr = ps_enc.flp_ltp_corr;
        let mut lag_index = ps_enc.s_cmn.indices.lag_index;
        let mut contour_index = ps_enc.s_cmn.indices.contour_index;
        let voiced = pitch_analysis_core(
            &res[..buf_len],
            &mut ctrl.pitch_l,
            &mut lag_index,
            &mut contour_index,
            &mut ltp_corr,
            ps_enc.s_cmn.prev_lag,
            search_thres1,
            thrhld,
            fs_khz as i32,
            cmn.pitch_estimation_complexity,
            cmn.nb_subfr as usize,
        );
        ps_enc.flp_ltp_corr = ltp_corr;
        ps_enc.s_cmn.indices.lag_index = lag_index;
        ps_enc.s_cmn.indices.contour_index = contour_index;
        ps_enc.s_cmn.indices.signal_type = if voiced == 0 {
            TYPE_VOICED as i8
        } else {
            TYPE_UNVOICED as i8
        };
    } else {
        ctrl.pitch_l = [0; MAX_NB_SUBFR];
        ps_enc.s_cmn.indices.lag_index = 0;
        ps_enc.s_cmn.indices.contour_index = 0;
        ps_enc.flp_ltp_corr = 0.0;
    }
}

// ---------------------------------------------------------------------------
// pitch_analysis_core_FLP.
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
fn pitch_analysis_core(
    frame: &[f32],
    pitch_out: &mut [i32; MAX_NB_SUBFR],
    lag_index: &mut i16,
    contour_index: &mut i8,
    ltp_corr: &mut f32,
    mut prev_lag: i32,
    search_thres1: f32,
    search_thres2: f32,
    fs_khz: i32,
    complexity: i32,
    nb_subfr: usize,
) -> i32 {
    let frame_length = (PE_LTP_MEM_LENGTH_MS + nb_subfr * PE_SUBFR_LENGTH_MS) * fs_khz as usize;
    let frame_length_4khz = (PE_LTP_MEM_LENGTH_MS + nb_subfr * PE_SUBFR_LENGTH_MS) * 4;
    let frame_length_8khz = (PE_LTP_MEM_LENGTH_MS + nb_subfr * PE_SUBFR_LENGTH_MS) * 8;
    let sf_length = PE_SUBFR_LENGTH_MS * fs_khz as usize;
    let sf_length_4khz = PE_SUBFR_LENGTH_MS * 4;
    let sf_length_8khz = PE_SUBFR_LENGTH_MS * 8;
    let min_lag = PE_MIN_LAG_MS * fs_khz as usize;
    let min_lag_4khz = PE_MIN_LAG_MS * 4;
    let min_lag_8khz = PE_MIN_LAG_MS * 8;
    let max_lag = PE_MAX_LAG_MS * fs_khz as usize - 1;
    let max_lag_4khz = PE_MAX_LAG_MS * 4;
    let max_lag_8khz = PE_MAX_LAG_MS * 8 - 1;

    let mut frame_8khz = [0.0f32; PE_MAX_FRAME_LENGTH_MS * 8];
    let mut frame_4khz = [0.0f32; PE_MAX_FRAME_LENGTH_MS * 4];
    let mut frame_8_fix = [0i16; PE_MAX_FRAME_LENGTH_MS * 8];
    let mut frame_4_fix = [0i16; PE_MAX_FRAME_LENGTH_MS * 4];
    let mut filt_state = [0i32; 6];

    if fs_khz == 16 {
        let mut frame_16_fix = [0i16; 16 * PE_MAX_FRAME_LENGTH_MS];
        for i in 0..frame_length {
            frame_16_fix[i] = float2short(frame[i]);
        }
        filt_state[..2].fill(0);
        silk_resampler_down2(&mut filt_state[..2], &mut frame_8_fix, &frame_16_fix[..frame_length], frame_length as i32);
        for i in 0..frame_length_8khz {
            frame_8khz[i] = frame_8_fix[i] as f32;
        }
    } else if fs_khz == 12 {
        let mut frame_12_fix = [0i16; 12 * PE_MAX_FRAME_LENGTH_MS];
        for i in 0..frame_length {
            frame_12_fix[i] = float2short(frame[i]);
        }
        filt_state[..6].fill(0);
        silk_resampler_down2_3(&mut filt_state[..6], &mut frame_8_fix, &frame_12_fix[..frame_length], frame_length as i32);
        for i in 0..frame_length_8khz {
            frame_8khz[i] = frame_8_fix[i] as f32;
        }
    } else {
        for i in 0..frame_length_8khz {
            frame_8_fix[i] = float2short(frame[i]);
        }
    }

    filt_state[..2].fill(0);
    silk_resampler_down2(&mut filt_state[..2], &mut frame_4_fix, &frame_8_fix[..frame_length_8khz], frame_length_8khz as i32);
    for i in 0..frame_length_4khz {
        frame_4khz[i] = frame_4_fix[i] as f32;
    }
    for i in (1..frame_length_4khz).rev() {
        frame_4khz[i] = sat16(frame_4khz[i] as i32 + frame_4khz[i - 1] as i32) as f32;
    }

    const CW: usize = (PE_MAX_LAG >> 1) + 5;
    let mut c = vec![[0.0f32; CW]; PE_MAX_NB_SUBFR];
    let mut xcorr = [0.0f32; PE_MAX_LAG_MS * 4 - PE_MIN_LAG_MS * 4 + 1];

    // Stage 1: 4 kHz, first-half subframes.
    let mut target = sf_length_4khz << 2;
    for _k in 0..nb_subfr >> 1 {
        let basis = target - min_lag_4khz;
        crate::pitch::pitch_xcorr(
            &frame_4khz[target..],
            &frame_4khz[target - max_lag_4khz..],
            &mut xcorr,
            sf_length_8khz,
            max_lag_4khz - min_lag_4khz + 1,
        );
        let mut cross = xcorr[max_lag_4khz - min_lag_4khz] as f64;
        let mut normalizer = energy(&frame_4khz[target..], sf_length_8khz)
            + energy(&frame_4khz[basis..], sf_length_8khz)
            + sf_length_8khz as f64 * 4000.0;
        c[0][min_lag_4khz] += (2.0 * cross / normalizer) as f32;
        let mut b = basis;
        for d in min_lag_4khz + 1..=max_lag_4khz {
            b -= 1;
            cross = xcorr[max_lag_4khz - d] as f64;
            normalizer += (frame_4khz[b] as f64) * (frame_4khz[b] as f64)
                - (frame_4khz[b + sf_length_8khz] as f64) * (frame_4khz[b + sf_length_8khz] as f64);
            c[0][d] += (2.0 * cross / normalizer) as f32;
        }
        target += sf_length_8khz;
    }
    for i in (min_lag_4khz..=max_lag_4khz).rev() {
        c[0][i] -= c[0][i] * i as f32 / 4096.0;
    }

    let mut length_d_srch = 4 + 2 * complexity as usize;
    let mut d_srch = [0i32; PE_D_SRCH_LENGTH_C];
    insertion_sort_decreasing(
        &mut c[0][min_lag_4khz..],
        &mut d_srch,
        max_lag_4khz - min_lag_4khz + 1,
        length_d_srch,
    );
    let cmax = c[0][min_lag_4khz];
    if cmax < 0.2 {
        *pitch_out = [0; MAX_NB_SUBFR];
        return 1;
    }
    let threshold = search_thres1 * cmax;
    for i in 0..length_d_srch {
        if c[0][min_lag_4khz + i] > threshold {
            d_srch[i] = (d_srch[i] + min_lag_4khz as i32) << 1;
        } else {
            length_d_srch = i;
            break;
        }
    }

    let mut d_comp = [0i16; (PE_MAX_LAG >> 1) + 5];
    for i in min_lag_8khz - 5..max_lag_8khz + 5 {
        d_comp[i] = 0;
    }
    for i in 0..length_d_srch {
        d_comp[d_srch[i] as usize] = 1;
    }
    for i in (min_lag_8khz..=max_lag_8khz + 3).rev() {
        d_comp[i] += d_comp[i - 1] + d_comp[i - 2];
    }
    length_d_srch = 0;
    for i in min_lag_8khz..max_lag_8khz + 1 {
        if d_comp[i + 1] > 0 {
            d_srch[length_d_srch] = i as i32;
            length_d_srch += 1;
        }
    }
    for i in (min_lag_8khz..=max_lag_8khz + 3).rev() {
        d_comp[i] += d_comp[i - 1] + d_comp[i - 2] + d_comp[i - 3];
    }
    let mut length_d_comp = 0usize;
    for i in min_lag_8khz..max_lag_8khz + 4 {
        if d_comp[i] > 0 {
            d_comp[length_d_comp] = (i as i16) - 2;
            length_d_comp += 1;
        }
    }

    // Stage 2: 8 kHz.
    for row in c.iter_mut() {
        row.fill(0.0);
    }
    // target base pointer into either frame (8 kHz) or original frame (8 kHz Fs).
    let use_orig = fs_khz == 8;
    let t_base = PE_LTP_MEM_LENGTH_MS * 8;
    for k in 0..nb_subfr {
        let toff = t_base + k * sf_length_8khz;
        let tgt: &[f32] = if use_orig { &frame[toff..] } else { &frame_8khz[toff..] };
        let energy_tmp = energy(tgt, sf_length_8khz) + 1.0;
        for j in 0..length_d_comp {
            let d = d_comp[j] as usize;
            let basis: &[f32] = if use_orig {
                &frame[toff - d..]
            } else {
                &frame_8khz[toff - d..]
            };
            let cross = inner_product(basis, tgt, sf_length_8khz);
            if cross > 0.0 {
                let e = energy(basis, sf_length_8khz);
                c[k][d] = (2.0 * cross / (e + energy_tmp)) as f32;
            } else {
                c[k][d] = 0.0;
            }
        }
    }

    let mut ccmax = 0.0f32;
    let mut ccmax_b = -1000.0f32;
    let mut cbimax = 0usize;
    let mut lag: i32 = -1;

    let prev_lag_log2;
    if prev_lag > 0 {
        if fs_khz == 12 {
            prev_lag = (prev_lag << 1) / 3;
        } else if fs_khz == 16 {
            prev_lag >>= 1;
        }
        prev_lag_log2 = silk_log2(prev_lag as f64);
    } else {
        prev_lag_log2 = 0.0;
    }

    let (cbk_size, nb_cbk_search): (usize, usize);
    let stage2_ext;
    if nb_subfr == PE_MAX_NB_SUBFR {
        cbk_size = PE_NB_CBKS_STAGE2_EXT;
        stage2_ext = true;
        nb_cbk_search = if fs_khz == 8 && complexity as usize > SILK_PE_MIN_COMPLEX {
            PE_NB_CBKS_STAGE2_EXT
        } else {
            PE_NB_CBKS_STAGE2
        };
    } else {
        cbk_size = PE_NB_CBKS_STAGE2_10MS;
        stage2_ext = false;
        nb_cbk_search = PE_NB_CBKS_STAGE2_10MS;
    }

    let mut cc = [0.0f32; PE_NB_CBKS_STAGE2_EXT];
    for k in 0..length_d_srch {
        let d = d_srch[k] as usize;
        for j in 0..nb_cbk_search {
            cc[j] = 0.0;
            for i in 0..nb_subfr {
                let cbval = if stage2_ext {
                    SILK_CB_LAGS_STAGE2[i][j] as i32
                } else {
                    SILK_CB_LAGS_STAGE2_10_MS[i][j] as i32
                };
                cc[j] += c[i][(d as i32 + cbval) as usize];
            }
        }
        let mut ccmax_new = -1000.0f32;
        let mut cbimax_new = 0usize;
        for i in 0..nb_cbk_search {
            if cc[i] > ccmax_new {
                ccmax_new = cc[i];
                cbimax_new = i;
            }
        }
        let lag_log2 = silk_log2(d as f64);
        let mut ccmax_new_b = ccmax_new - PE_SHORTLAG_BIAS * nb_subfr as f32 * lag_log2;
        if prev_lag > 0 {
            let mut delta = lag_log2 - prev_lag_log2;
            delta *= delta;
            ccmax_new_b -= PE_PREVLAG_BIAS * nb_subfr as f32 * (*ltp_corr) * delta / (delta + 0.5);
        }
        if ccmax_new_b > ccmax_b && ccmax_new > nb_subfr as f32 * search_thres2 {
            ccmax_b = ccmax_new_b;
            ccmax = ccmax_new;
            lag = d as i32;
            cbimax = cbimax_new;
        }
    }
    if lag == -1 {
        *pitch_out = [0; MAX_NB_SUBFR];
        *ltp_corr = 0.0;
        return 1;
    }

    // Output normalized correlation (pitch_analysis_core_FLP.c: *LTPCorr =
    // CCmax / nb_subfr). Was `let _ = ccmax;` — dropping this left LTPCorr
    // stale, so harmonic shaping / SNR adjustment in the float arm ran on a
    // dead signal (Great Gate census 2026-08-07, silk.md §2).
    *ltp_corr = ccmax / nb_subfr as f32;

    if fs_khz > 8 {
        if fs_khz == 12 {
            lag = (lag * 3 + 1) >> 1;
        } else {
            lag <<= 1;
        }
        lag = lag.clamp(min_lag as i32, max_lag as i32);
        let start_lag = (lag - 2).max(min_lag as i32);
        let end_lag = (lag + 2).min(max_lag as i32);
        let mut lag_new = lag;
        cbimax = 0;
        let mut ccmax3 = -1000.0f32;

        let (nb_cbk_search3, cbk_size3, stage3_ext);
        if nb_subfr == PE_MAX_NB_SUBFR {
            nb_cbk_search3 = SILK_NB_CBK_SEARCHS_STAGE3[complexity as usize];
            cbk_size3 = PE_NB_CBKS_STAGE3_MAX;
            stage3_ext = true;
        } else {
            nb_cbk_search3 = PE_NB_CBKS_STAGE3_10MS;
            cbk_size3 = PE_NB_CBKS_STAGE3_10MS;
            stage3_ext = false;
        }

        let mut cross_corr_st3 =
            vec![[[0.0f32; PE_NB_STAGE3_LAGS]; PE_NB_CBKS_STAGE3_MAX]; PE_MAX_NB_SUBFR];
        let mut energies_st3 =
            vec![[[0.0f32; PE_NB_STAGE3_LAGS]; PE_NB_CBKS_STAGE3_MAX]; PE_MAX_NB_SUBFR];
        calc_corr_st3(&mut cross_corr_st3, frame, start_lag, sf_length, nb_subfr, complexity);
        calc_energy_st3(&mut energies_st3, frame, start_lag, sf_length, nb_subfr, complexity);

        let contour_bias = PE_FLATCONTOUR_BIAS / lag as f32;
        let toff = PE_LTP_MEM_LENGTH_MS * fs_khz as usize;
        let energy_tmp = energy(&frame[toff..], nb_subfr * sf_length) + 1.0;
        let mut lag_counter = 0usize;
        for d in start_lag..=end_lag {
            for j in 0..nb_cbk_search3 {
                let mut cross = 0.0f64;
                let mut e = energy_tmp;
                for k in 0..nb_subfr {
                    cross += cross_corr_st3[k][j][lag_counter] as f64;
                    e += energies_st3[k][j][lag_counter] as f64;
                }
                let ccmax_new = if cross > 0.0 {
                    (2.0 * cross / e) as f32 * (1.0 - contour_bias * j as f32)
                } else {
                    0.0
                };
                if ccmax_new > ccmax3 && (d + SILK_CB_LAGS_STAGE3[0][j] as i32) <= max_lag as i32 {
                    ccmax3 = ccmax_new;
                    lag_new = d;
                    cbimax = j;
                }
            }
            lag_counter += 1;
        }
        for k in 0..nb_subfr {
            let cbval = if stage3_ext {
                SILK_CB_LAGS_STAGE3[k][cbimax] as i32
            } else {
                SILK_CB_LAGS_STAGE3_10_MS[k][cbimax] as i32
            };
            pitch_out[k] = (lag_new + cbval).clamp(min_lag as i32, (PE_MAX_LAG_MS * fs_khz as usize) as i32);
        }
        let _ = cbk_size3;
        *lag_index = (lag - min_lag as i32) as i16;
        *contour_index = cbimax as i8;
    } else {
        for k in 0..nb_subfr {
            let cbval = if stage2_ext {
                SILK_CB_LAGS_STAGE2[k][cbimax] as i32
            } else {
                SILK_CB_LAGS_STAGE2_10_MS[k][cbimax] as i32
            };
            pitch_out[k] = (lag + cbval).clamp(min_lag_8khz as i32, (PE_MAX_LAG_MS * 8) as i32);
        }
        *lag_index = (lag - min_lag_8khz as i32) as i16;
        *contour_index = cbimax as i8;
    }
    let _ = cbk_size;
    0
}

fn calc_corr_st3(
    cross_corr_st3: &mut [[[f32; PE_NB_STAGE3_LAGS]; PE_NB_CBKS_STAGE3_MAX]],
    frame: &[f32],
    start_lag: i32,
    sf_length: usize,
    nb_subfr: usize,
    complexity: i32,
) {
    let ext = nb_subfr == PE_MAX_NB_SUBFR;
    let nb_cbk_search = if ext {
        SILK_NB_CBK_SEARCHS_STAGE3[complexity as usize]
    } else {
        PE_NB_CBKS_STAGE3_10MS
    };
    let mut scratch = [0.0f32; SCRATCH_SIZE];
    let mut xcorr = [0.0f32; SCRATCH_SIZE];
    let t_base = sf_length << 2;
    for k in 0..nb_subfr {
        let toff = t_base + k * sf_length;
        let (lag_low, lag_high) = if ext {
            (
                SILK_LAG_RANGE_STAGE3[complexity as usize][k][0] as i32,
                SILK_LAG_RANGE_STAGE3[complexity as usize][k][1] as i32,
            )
        } else {
            (
                SILK_LAG_RANGE_STAGE3_10_MS[k][0] as i32,
                SILK_LAG_RANGE_STAGE3_10_MS[k][1] as i32,
            )
        };
        let n = (lag_high - lag_low + 1) as usize;
        let ystart = (toff as i32 - start_lag - lag_high) as usize;
        crate::pitch::pitch_xcorr(&frame[toff..], &frame[ystart..], &mut xcorr, sf_length, n);
        let mut lag_counter = 0usize;
        for j in lag_low..=lag_high {
            scratch[lag_counter] = xcorr[(lag_high - j) as usize];
            lag_counter += 1;
        }
        let delta = lag_low;
        for i in 0..nb_cbk_search {
            let idx = if ext {
                SILK_CB_LAGS_STAGE3[k][i] as i32 - delta
            } else {
                SILK_CB_LAGS_STAGE3_10_MS[k][i] as i32 - delta
            };
            for j in 0..PE_NB_STAGE3_LAGS {
                cross_corr_st3[k][i][j] = scratch[(idx + j as i32) as usize];
            }
        }
    }
}

fn calc_energy_st3(
    energies_st3: &mut [[[f32; PE_NB_STAGE3_LAGS]; PE_NB_CBKS_STAGE3_MAX]],
    frame: &[f32],
    start_lag: i32,
    sf_length: usize,
    nb_subfr: usize,
    complexity: i32,
) {
    let ext = nb_subfr == PE_MAX_NB_SUBFR;
    let nb_cbk_search = if ext {
        SILK_NB_CBK_SEARCHS_STAGE3[complexity as usize]
    } else {
        PE_NB_CBKS_STAGE3_10MS
    };
    let mut scratch = [0.0f32; SCRATCH_SIZE];
    let t_base = sf_length << 2;
    for k in 0..nb_subfr {
        let toff = t_base + k * sf_length;
        let (lag_low, lag_high) = if ext {
            (
                SILK_LAG_RANGE_STAGE3[complexity as usize][k][0] as i32,
                SILK_LAG_RANGE_STAGE3[complexity as usize][k][1] as i32,
            )
        } else {
            (
                SILK_LAG_RANGE_STAGE3_10_MS[k][0] as i32,
                SILK_LAG_RANGE_STAGE3_10_MS[k][1] as i32,
            )
        };
        let basis = (toff as i32 - (start_lag + lag_low)) as usize;
        let mut e = energy(&frame[basis..], sf_length) + 1e-3;
        let mut lag_counter = 0usize;
        scratch[lag_counter] = e as f32;
        lag_counter += 1;
        let lag_diff = (lag_high - lag_low + 1) as usize;
        for i in 1..lag_diff {
            e -= (frame[basis + sf_length - i] as f64) * (frame[basis + sf_length - i] as f64);
            e += (frame[basis - i] as f64) * (frame[basis - i] as f64);
            scratch[lag_counter] = e as f32;
            lag_counter += 1;
        }
        let delta = lag_low;
        for i in 0..nb_cbk_search {
            let idx = if ext {
                SILK_CB_LAGS_STAGE3[k][i] as i32 - delta
            } else {
                SILK_CB_LAGS_STAGE3_10_MS[k][i] as i32 - delta
            };
            for j in 0..PE_NB_STAGE3_LAGS {
                energies_st3[k][i][j] = scratch[(idx + j as i32) as usize];
            }
        }
    }
}

// ---------------------------------------------------------------------------
// noise_shape_analysis_FLP.
// ---------------------------------------------------------------------------

fn warped_gain(coefs: &[f32], lambda: f32, order: usize) -> f32 {
    let lambda = -lambda;
    let mut gain = coefs[order - 1];
    for i in (0..order - 1).rev() {
        gain = lambda * gain + coefs[i];
    }
    1.0 / (1.0 - lambda * gain)
}

fn warped_true2monic_coefs(coefs: &mut [f32], lambda: f32, limit: f32, order: usize) {
    for i in (1..order).rev() {
        coefs[i - 1] -= lambda * coefs[i];
    }
    let mut gain = (1.0 - lambda * lambda) / (1.0 + lambda * coefs[0]);
    for c in coefs.iter_mut().take(order) {
        *c *= gain;
    }
    for iter in 0..10 {
        let mut maxabs = -1.0f32;
        let mut ind = 0usize;
        for (i, &c) in coefs.iter().enumerate().take(order) {
            let t = c.abs();
            if t > maxabs {
                maxabs = t;
                ind = i;
            }
        }
        if maxabs <= limit {
            return;
        }
        for i in 1..order {
            coefs[i - 1] += lambda * coefs[i];
        }
        gain = 1.0 / gain;
        for c in coefs.iter_mut().take(order) {
            *c *= gain;
        }
        let chirp = 0.99 - (0.8 + 0.1 * iter as f32) * (maxabs - limit) / (maxabs * (ind as f32 + 1.0));
        bwexpander(coefs, order, chirp);
        for i in (1..order).rev() {
            coefs[i - 1] -= lambda * coefs[i];
        }
        gain = (1.0 - lambda * lambda) / (1.0 + lambda * coefs[0]);
        for c in coefs.iter_mut().take(order) {
            *c *= gain;
        }
    }
}

fn limit_coefs(coefs: &mut [f32], limit: f32, order: usize) {
    for iter in 0..10 {
        let mut maxabs = -1.0f32;
        let mut ind = 0usize;
        for (i, &c) in coefs.iter().enumerate().take(order) {
            let t = c.abs();
            if t > maxabs {
                maxabs = t;
                ind = i;
            }
        }
        if maxabs <= limit {
            return;
        }
        let chirp = 0.99 - (0.8 + 0.1 * iter as f32) * (maxabs - limit) / (maxabs * (ind as f32 + 1.0));
        bwexpander(coefs, order, chirp);
    }
}

/// `pitch_res` starts at the pitch residual frame; `x` starts at the input frame
/// (both already offset by the caller). Uses `x[-la_shape..]` internally so `x`
/// must be a slice whose index 0 is the frame start with `la_shape` history
/// available before it — we pass the buffer and a base index instead.
fn noise_shape_analysis(
    ps_enc: &mut SilkEncoderState,
    ctrl: &mut EncCtrlFlp,
    pitch_res: &[f32],
    x_buf: &[f32],
    x_frame_idx: usize,
) {
    let cmn = &ps_enc.s_cmn;
    let fs_khz = cmn.fs_khz as usize;
    let nb_subfr = cmn.nb_subfr as usize;
    let la_shape = cmn.la_shape as usize;
    let shape_win_length = cmn.shape_win_length as usize;
    let shaping_lpc_order = cmn.shaping_lpc_order as usize;
    let subfr_length = cmn.subfr_length as usize;
    let warping_q16 = cmn.warping_q16;
    let snr_db_q7 = cmn.snr_db_q7;
    let use_cbr = cmn.use_cbr;
    let speech_activity_q8 = cmn.speech_activity_q8;
    let input_quality_bands_q15 = cmn.input_quality_bands_q15;
    let signal_type = cmn.indices.signal_type as i32;

    let mut snr_adj_db = snr_db_q7 as f32 * (1.0 / 128.0);
    ctrl.input_quality = 0.5
        * (input_quality_bands_q15[0] + input_quality_bands_q15[1]) as f32
        * (1.0 / 32768.0);
    ctrl.coding_quality = silk_sigmoid(0.25 * (snr_adj_db - 20.0));

    if use_cbr == 0 {
        let b = 1.0 - speech_activity_q8 as f32 * (1.0 / 256.0);
        snr_adj_db -= BG_SNR_DECR_dB * ctrl.coding_quality * (0.5 + 0.5 * ctrl.input_quality) * b * b;
    }
    if signal_type == TYPE_VOICED {
        snr_adj_db += HARM_SNR_INCR_dB * ps_enc.flp_ltp_corr;
    } else {
        snr_adj_db += (-0.4 * snr_db_q7 as f32 * (1.0 / 128.0) + 6.0) * (1.0 - ctrl.input_quality);
    }

    if signal_type == TYPE_VOICED {
        ps_enc.s_cmn.indices.quant_offset_type = 0;
    } else {
        let n_samples = 2 * fs_khz;
        let mut energy_variation = 0.0f32;
        let mut log_energy_prev = 0.0f32;
        let n_segs = SUB_FRAME_LENGTH_MS * nb_subfr / 2;
        for k in 0..n_segs {
            let nrg = n_samples as f32 + energy(&pitch_res[k * n_samples..], n_samples) as f32;
            let log_energy = silk_log2(nrg as f64);
            if k > 0 {
                energy_variation += (log_energy - log_energy_prev).abs();
            }
            log_energy_prev = log_energy;
        }
        ps_enc.s_cmn.indices.quant_offset_type =
            if energy_variation > ENERGY_VARIATION_THRESHOLD_QNT_OFFSET * (n_segs - 1) as f32 {
                0
            } else {
                1
            };
    }

    let strength0 = FIND_PITCH_WHITE_NOISE_FRACTION * ctrl.pred_gain;
    let bw_exp = BANDWIDTH_EXPANSION / (1.0 + strength0 * strength0);
    let warping = warping_q16 as f32 / 65536.0 + 0.01 * ctrl.coding_quality;

    let mut x_windowed = [0.0f32; SHAPE_LPC_WIN_MAX];
    let mut auto_corr = [0.0f32; MAX_SHAPE_LPC_ORDER + 1];
    let mut rc = [0.0f32; MAX_SHAPE_LPC_ORDER + 1];

    // x_ptr starts at x - la_shape.
    let mut xp = x_frame_idx - la_shape;
    for k in 0..nb_subfr {
        let flat_part = fs_khz * 3;
        let slope_part = (shape_win_length - flat_part) / 2;
        apply_sine_window(&mut x_windowed, &x_buf[xp..], 1, slope_part);
        let mut shift = slope_part;
        x_windowed[shift..shift + flat_part].copy_from_slice(&x_buf[xp + shift..xp + shift + flat_part]);
        shift += flat_part;
        apply_sine_window(&mut x_windowed[shift..], &x_buf[xp + shift..], 2, slope_part);
        xp += subfr_length;

        if warping_q16 > 0 {
            warped_autocorrelation(&mut auto_corr, &x_windowed, warping, shape_win_length, shaping_lpc_order);
        } else {
            autocorrelation(&mut auto_corr, &x_windowed, shape_win_length, shaping_lpc_order + 1);
        }
        auto_corr[0] += auto_corr[0] * SHAPE_WHITE_NOISE_FRACTION + 1.0;
        let nrg = schur(&mut rc, &auto_corr, shaping_lpc_order);
        let ar_base = k * MAX_SHAPE_LPC_ORDER;
        k2a(&mut ctrl.ar[ar_base..], &rc, shaping_lpc_order);
        ctrl.gains[k] = nrg.sqrt();
        if warping_q16 > 0 {
            ctrl.gains[k] *= warped_gain(&ctrl.ar[ar_base..], warping, shaping_lpc_order);
        }
        bwexpander(&mut ctrl.ar[ar_base..], shaping_lpc_order, bw_exp);
        if warping_q16 > 0 {
            warped_true2monic_coefs(&mut ctrl.ar[ar_base..], warping, 3.999, shaping_lpc_order);
        } else {
            limit_coefs(&mut ctrl.ar[ar_base..], 3.999, shaping_lpc_order);
        }
    }

    let gain_mult = 2.0f32.powf(-0.16 * snr_adj_db);
    let gain_add = 2.0f32.powf(0.16 * MIN_QGAIN_DB as f32);
    for k in 0..nb_subfr {
        ctrl.gains[k] *= gain_mult;
        ctrl.gains[k] += gain_add;
    }

    let mut strength = LOW_FREQ_SHAPING
        * (1.0 + LOW_QUALITY_LOW_FREQ_SHAPING_DECR * (input_quality_bands_q15[0] as f32 * (1.0 / 32768.0) - 1.0));
    strength *= speech_activity_q8 as f32 * (1.0 / 256.0);
    let tilt;
    if signal_type == TYPE_VOICED {
        for k in 0..nb_subfr {
            let b = 0.2 / fs_khz as f32 + 3.0 / ctrl.pitch_l[k] as f32;
            ctrl.lf_ma_shp[k] = -1.0 + b;
            ctrl.lf_ar_shp[k] = 1.0 - b - b * strength;
        }
        tilt = -HP_NOISE_COEF
            - (1.0 - HP_NOISE_COEF) * HARM_HP_NOISE_COEF * speech_activity_q8 as f32 * (1.0 / 256.0);
    } else {
        let b = 1.3 / fs_khz as f32;
        ctrl.lf_ma_shp[0] = -1.0 + b;
        ctrl.lf_ar_shp[0] = 1.0 - b - b * strength * 0.6;
        for k in 1..nb_subfr {
            ctrl.lf_ma_shp[k] = ctrl.lf_ma_shp[0];
            ctrl.lf_ar_shp[k] = ctrl.lf_ar_shp[0];
        }
        tilt = -HP_NOISE_COEF;
    }

    let harm_shape_gain;
    if USE_HARM_SHAPING != 0 && signal_type == TYPE_VOICED {
        let mut h = HARMONIC_SHAPING;
        h += HIGH_RATE_OR_LOW_QUALITY_HARMONIC_SHAPING * (1.0 - (1.0 - ctrl.coding_quality) * ctrl.input_quality);
        h *= ps_enc.flp_ltp_corr.sqrt();
        harm_shape_gain = h;
    } else {
        harm_shape_gain = 0.0;
    }

    for k in 0..nb_subfr {
        ps_enc.s_shape.flp_harm_shape_gain_smth +=
            SUBFR_SMTH_COEF * (harm_shape_gain - ps_enc.s_shape.flp_harm_shape_gain_smth);
        ctrl.harm_shape_gain[k] = ps_enc.s_shape.flp_harm_shape_gain_smth;
        ps_enc.s_shape.flp_tilt_smth += SUBFR_SMTH_COEF * (tilt - ps_enc.s_shape.flp_tilt_smth);
        ctrl.tilt[k] = ps_enc.s_shape.flp_tilt_smth;
    }
}

// ---------------------------------------------------------------------------
// find_LTP_FLP + find_LPC_FLP + residual_energy_FLP + find_pred_coefs_FLP.
// ---------------------------------------------------------------------------

fn find_ltp(xx: &mut [f32], x_x: &mut [f32], r: &[f32], r_base: usize, lag: &[i32], subfr_length: usize, nb_subfr: usize) {
    let mut r_ptr = r_base;
    let mut xx_off = 0usize;
    let mut x_x_off = 0usize;
    for k in 0..nb_subfr {
        let lag_ptr = r_ptr - (lag[k] as usize + LTP_ORDER / 2);
        corr_matrix(&r[lag_ptr..], subfr_length, LTP_ORDER, &mut xx[xx_off..]);
        corr_vector(&r[lag_ptr..], &r[r_ptr..], subfr_length, LTP_ORDER, &mut x_x[x_x_off..]);
        let xx_energy = energy(&r[r_ptr..], subfr_length + LTP_ORDER) as f32;
        let temp = 1.0
            / xx_energy.max(LTP_CORR_INV_MAX * 0.5 * (xx[xx_off] + xx[xx_off + 24]) + 1.0);
        scale_vector(&mut xx[xx_off..], temp, LTP_ORDER * LTP_ORDER);
        scale_vector(&mut x_x[x_x_off..], temp, LTP_ORDER);
        r_ptr += subfr_length;
        xx_off += LTP_ORDER * LTP_ORDER;
        x_x_off += LTP_ORDER;
    }
}

fn find_lpc(ps_enc: &mut SilkEncoderState, nlsf_q15: &mut [i16], x: &[f32], min_inv_gain: f32) {
    let nb_subfr = ps_enc.s_cmn.nb_subfr as usize;
    let order = ps_enc.s_cmn.predict_lpc_order as usize;
    let subfr_length = ps_enc.s_cmn.subfr_length as usize + order;
    let use_interpolated_nlsfs = ps_enc.s_cmn.use_interpolated_nlsfs;
    let first_frame_after_reset = ps_enc.s_cmn.first_frame_after_reset;

    let mut a = [0.0f32; MAX_LPC_ORDER];
    ps_enc.s_cmn.indices.nlsf_interp_coef_q2 = 4;
    let mut res_nrg = burg_modified(&mut a, x, min_inv_gain, subfr_length, nb_subfr, order);

    if use_interpolated_nlsfs != 0 && first_frame_after_reset == 0 && nb_subfr == MAX_NB_SUBFR {
        let mut a_tmp = [0.0f32; MAX_LPC_ORDER];
        res_nrg -= burg_modified(
            &mut a_tmp,
            &x[(MAX_NB_SUBFR / 2) * subfr_length..],
            min_inv_gain,
            subfr_length,
            MAX_NB_SUBFR / 2,
            order,
        );
        {
            let mut a_q16 = [0i32; MAX_LPC_ORDER];
            for i in 0..order {
                a_q16[i] = float2int(a_tmp[i] * 65536.0);
            }
            silk_a2nlsf(nlsf_q15, &mut a_q16, order);
        }
        let mut res_nrg_2nd = f32::MAX;
        let mut nlsf0_q15 = [0i16; MAX_LPC_ORDER];
        let mut lpc_res = [0.0f32; 2 * (MAX_FRAME_LENGTH / MAX_NB_SUBFR + MAX_LPC_ORDER)];
        for k in (0..=3).rev() {
            nlsf0_q15[..order].copy_from_slice(
                &silk_interpolate(&ps_enc.s_cmn.prev_nlsf_q15, nlsf_q15, k, order)[..order],
            );
            let mut a_q12 = [0i16; MAX_LPC_ORDER];
            silk_nlsf2a(&mut a_q12, &nlsf0_q15, order);
            let mut a_interp = [0.0f32; MAX_LPC_ORDER];
            for i in 0..order {
                a_interp[i] = a_q12[i] as f32 * (1.0 / 4096.0);
            }
            lpc_analysis_filter(&mut lpc_res, &a_interp, x, 2 * subfr_length, order);
            let res_nrg_interp = (energy(&lpc_res[order..], subfr_length - order)
                + energy(&lpc_res[order + subfr_length..], subfr_length - order))
                as f32;
            if res_nrg_interp < res_nrg {
                res_nrg = res_nrg_interp;
                ps_enc.s_cmn.indices.nlsf_interp_coef_q2 = k as i8;
            } else if res_nrg_interp > res_nrg_2nd {
                break;
            }
            res_nrg_2nd = res_nrg_interp;
        }
    }
    if ps_enc.s_cmn.indices.nlsf_interp_coef_q2 == 4 {
        let mut a_q16 = [0i32; MAX_LPC_ORDER];
        for i in 0..order {
            a_q16[i] = float2int(a[i] * 65536.0);
        }
        silk_a2nlsf(nlsf_q15, &mut a_q16, order);
    }
}

fn residual_energy(
    nrgs: &mut [f32; MAX_NB_SUBFR],
    x: &[f32],
    a: &[[f32; MAX_LPC_ORDER]; 2],
    gains: &[f32],
    subfr_length: usize,
    nb_subfr: usize,
    lpc_order: usize,
) {
    let mut lpc_res = [0.0f32; (MAX_FRAME_LENGTH + MAX_NB_SUBFR * MAX_LPC_ORDER) / 2];
    let shift = lpc_order + subfr_length;
    lpc_analysis_filter(&mut lpc_res, &a[0], x, 2 * shift, lpc_order);
    nrgs[0] = gains[0] * gains[0] * energy(&lpc_res[lpc_order..], subfr_length) as f32;
    nrgs[1] = gains[1] * gains[1] * energy(&lpc_res[lpc_order + shift..], subfr_length) as f32;
    if nb_subfr == MAX_NB_SUBFR {
        lpc_analysis_filter(&mut lpc_res, &a[1], &x[2 * shift..], 2 * shift, lpc_order);
        nrgs[2] = gains[2] * gains[2] * energy(&lpc_res[lpc_order..], subfr_length) as f32;
        nrgs[3] = gains[3] * gains[3] * energy(&lpc_res[lpc_order + shift..], subfr_length) as f32;
    }
}

fn ltp_analysis_filter(
    ltp_res: &mut [f32],
    x: &[f32],
    x_base: usize,
    b: &[f32],
    pitch_l: &[i32],
    inv_gains: &[f32],
    subfr_length: usize,
    nb_subfr: usize,
    pre_length: usize,
) {
    let mut x_ptr = x_base;
    let mut res_ptr = 0usize;
    for k in 0..nb_subfr {
        let mut x_lag = x_ptr - pitch_l[k] as usize;
        let inv_gain = inv_gains[k];
        let mut btmp = [0.0f32; LTP_ORDER];
        btmp.copy_from_slice(&b[k * LTP_ORDER..k * LTP_ORDER + LTP_ORDER]);
        for i in 0..subfr_length + pre_length {
            let mut v = x[x_ptr + i];
            for (j, &bj) in btmp.iter().enumerate() {
                v -= bj * x[x_lag + LTP_ORDER / 2 - j];
            }
            ltp_res[res_ptr + i] = v * inv_gain;
            x_lag += 1;
        }
        res_ptr += subfr_length + pre_length;
        x_ptr += subfr_length;
    }
}

/// find_pred_coefs_FLP. `x_buf`/`x_frame_idx` locate the frame; `res`/`res_frame`
/// locate the pitch residual frame.
fn find_pred_coefs(
    ps_enc: &mut SilkEncoderState,
    ctrl: &mut EncCtrlFlp,
    res: &[f32],
    res_frame: usize,
    x_buf: &[f32],
    x_frame_idx: usize,
    cond_coding: i32,
) {
    let nb_subfr = ps_enc.s_cmn.nb_subfr as usize;
    let order = ps_enc.s_cmn.predict_lpc_order as usize;
    let subfr_length = ps_enc.s_cmn.subfr_length as usize;

    let mut inv_gains = [0.0f32; MAX_NB_SUBFR];
    for i in 0..nb_subfr {
        inv_gains[i] = 1.0 / ctrl.gains[i];
    }

    // LPC_in_pre: nb_subfr*(subfr_length+order).
    let mut lpc_in_pre = [0.0f32; MAX_NB_SUBFR * MAX_LPC_ORDER + MAX_FRAME_LENGTH];
    let signal_type = ps_enc.s_cmn.indices.signal_type as i32;

    if signal_type == TYPE_VOICED {
        let mut xx_ltp = [0.0f32; MAX_NB_SUBFR * LTP_ORDER * LTP_ORDER];
        let mut x_x_ltp = [0.0f32; MAX_NB_SUBFR * LTP_ORDER];
        find_ltp(&mut xx_ltp, &mut x_x_ltp, res, res_frame, &ctrl.pitch_l, subfr_length, nb_subfr);

        // quant_LTP_gains_FLP: float XX/xX -> Q17, fixed quant, back to float Q14.
        let mut xx_q17 = [0i32; MAX_NB_SUBFR * LTP_ORDER * LTP_ORDER];
        let mut x_x_q17 = [0i32; MAX_NB_SUBFR * LTP_ORDER];
        for i in 0..nb_subfr * LTP_ORDER * LTP_ORDER {
            xx_q17[i] = float2int(xx_ltp[i] * 131072.0);
        }
        for i in 0..nb_subfr * LTP_ORDER {
            x_x_q17[i] = float2int(x_x_ltp[i] * 131072.0);
        }
        let mut b_q14 = [0i16; MAX_NB_SUBFR * LTP_ORDER];
        let mut pred_gain_db_q7 = 0i32;
        {
            let cmn = &mut ps_enc.s_cmn;
            silk_quant_ltp_gains(
                &mut b_q14,
                &mut cmn.indices.ltp_index,
                &mut cmn.indices.per_index,
                &mut cmn.sum_log_gain_q7,
                &mut pred_gain_db_q7,
                &xx_q17,
                &x_x_q17,
                subfr_length as i32,
                nb_subfr,
                0,
            );
        }
        ctrl.ltp_red_cod_gain = pred_gain_db_q7 as f32 / 128.0;
        for i in 0..nb_subfr * LTP_ORDER {
            ctrl.ltp_coef[i] = b_q14[i] as f32 * (1.0 / 16384.0);
        }

        // LTP_scale_ctrl_FLP.
        if cond_coding == CODE_INDEPENDENTLY {
            let round_loss = ps_enc.s_cmn.packet_loss_perc + ps_enc.s_cmn.n_frames_per_packet;
            ps_enc.s_cmn.indices.ltp_scale_index =
                (round_loss as f32 * ctrl.ltp_red_cod_gain * 0.1).clamp(0.0, 2.0) as i8;
        } else {
            ps_enc.s_cmn.indices.ltp_scale_index = 0;
        }
        ctrl.ltp_scale =
            SILK_LTP_SCALES_TABLE_Q14[ps_enc.s_cmn.indices.ltp_scale_index as usize] as f32 / 16384.0;

        // LTP_analysis_filter_FLP: x - order.
        ltp_analysis_filter(
            &mut lpc_in_pre,
            x_buf,
            x_frame_idx - order,
            &ctrl.ltp_coef,
            &ctrl.pitch_l,
            &inv_gains,
            subfr_length,
            nb_subfr,
            order,
        );
    } else {
        let mut xp = x_frame_idx - order;
        let mut pre = 0usize;
        for i in 0..nb_subfr {
            scale_copy_vector(&mut lpc_in_pre[pre..], &x_buf[xp..], inv_gains[i], subfr_length + order);
            pre += subfr_length + order;
            xp += subfr_length;
        }
        ctrl.ltp_coef[..nb_subfr * LTP_ORDER].fill(0.0);
        ctrl.ltp_red_cod_gain = 0.0;
        ps_enc.s_cmn.sum_log_gain_q7 = 0;
    }

    let min_inv_gain = if ps_enc.s_cmn.first_frame_after_reset != 0 {
        1.0 / MAX_PREDICTION_POWER_GAIN_AFTER_RESET
    } else {
        let g = 2.0f32.powf(ctrl.ltp_red_cod_gain / 3.0) / MAX_PREDICTION_POWER_GAIN;
        g / (0.25 + 0.75 * ctrl.coding_quality)
    };

    let mut nlsf_q15 = [0i16; MAX_LPC_ORDER];
    find_lpc(ps_enc, &mut nlsf_q15, &lpc_in_pre, min_inv_gain);

    // process_NLSFs_FLP -> fixed process_nlsfs fills pred_coef_q12; convert to float.
    let prev = ps_enc.s_cmn.prev_nlsf_q15;
    let mut sc = SilkEncoderControl::default();
    silk_process_nlsfs_flp(ps_enc, &mut sc, &mut nlsf_q15, &prev);
    for j in 0..2 {
        for i in 0..order {
            ctrl.pred_coef[j][i] = sc.pred_coef_q12[j][i] as f32 * (1.0 / 4096.0);
        }
    }

    residual_energy(&mut ctrl.res_nrg, &lpc_in_pre, &ctrl.pred_coef, &ctrl.gains, subfr_length, nb_subfr, order);
    ps_enc.s_cmn.prev_nlsf_q15 = nlsf_q15;
}

/// Wraps the fixed `silk_process_nlsfs` (which needs an `s_enc_ctrl` to write
/// `pred_coef_q12`). We give it a scratch control and read the Q12 coefs back.
fn silk_process_nlsfs_flp(
    ps_enc: &mut SilkEncoderState,
    sc: &mut SilkEncoderControl,
    nlsf_q15: &mut [i16],
    _prev: &[i16; MAX_LPC_ORDER],
) {
    silk_process_nlsfs(ps_enc, sc, nlsf_q15);
}

// ---------------------------------------------------------------------------
// process_gains_FLP.
// ---------------------------------------------------------------------------

fn process_gains(ps_enc: &mut SilkEncoderState, ctrl: &mut EncCtrlFlp, cond_coding: i32) {
    let nb_subfr = ps_enc.s_cmn.nb_subfr as usize;
    let signal_type = ps_enc.s_cmn.indices.signal_type as i32;

    if signal_type == TYPE_VOICED {
        let s = 1.0 - 0.5 * silk_sigmoid(0.25 * (ctrl.ltp_red_cod_gain - 12.0));
        for k in 0..nb_subfr {
            ctrl.gains[k] *= s;
        }
    }

    let inv_max_sqr_val = (2.0f64
        .powf(0.33 * (21.0 - ps_enc.s_cmn.snr_db_q7 as f64 * (1.0 / 128.0)))
        / ps_enc.s_cmn.subfr_length as f64) as f32;
    for k in 0..nb_subfr {
        let gain = ctrl.gains[k];
        let gain = (gain * gain + ctrl.res_nrg[k] * inv_max_sqr_val).sqrt();
        ctrl.gains[k] = gain.min(32767.0);
    }

    let mut p_gains_q16 = [0i32; MAX_NB_SUBFR];
    for k in 0..nb_subfr {
        p_gains_q16[k] = (ctrl.gains[k] * 65536.0) as i32;
    }
    ctrl.gains_unq_q16[..nb_subfr].copy_from_slice(&p_gains_q16[..nb_subfr]);
    ctrl.last_gain_index_prev = ps_enc.s_shape.last_gain_index;

    {
        let mut last = ps_enc.s_shape.last_gain_index;
        silk_gains_quant(
            &mut ps_enc.s_cmn.indices.gains_indices,
            &mut p_gains_q16,
            &mut last,
            (cond_coding == CODE_CONDITIONALLY) as i32,
            nb_subfr,
        );
        ps_enc.s_shape.last_gain_index = last;
    }
    for k in 0..nb_subfr {
        ctrl.gains[k] = p_gains_q16[k] as f32 / 65536.0;
    }

    if signal_type == TYPE_VOICED
        && ctrl.ltp_red_cod_gain + ps_enc.s_cmn.input_tilt_q15 as f32 * (1.0 / 32768.0) > 1.0
    {
        ps_enc.s_cmn.indices.quant_offset_type = 0;
    } else if signal_type == TYPE_VOICED {
        ps_enc.s_cmn.indices.quant_offset_type = 1;
    }

    let quant_offset = SILK_QUANTIZATION_OFFSETS_Q10[(signal_type >> 1) as usize]
        [ps_enc.s_cmn.indices.quant_offset_type as usize] as f32
        / 1024.0;
    ctrl.lambda = LAMBDA_OFFSET
        + LAMBDA_DELAYED_DECISIONS * ps_enc.s_cmn.n_states_delayed_decision as f32
        + LAMBDA_SPEECH_ACT * ps_enc.s_cmn.speech_activity_q8 as f32 * (1.0 / 256.0)
        + LAMBDA_INPUT_QUALITY * ctrl.input_quality
        + LAMBDA_CODING_QUALITY * ctrl.coding_quality
        + LAMBDA_QUANT_OFFSET * quant_offset;
}

// ---------------------------------------------------------------------------
// Orchestration + float->Q NSQ boundary.
// ---------------------------------------------------------------------------

/// Runs the float SILK analysis for one frame and fills the fixed-point
/// [`SilkEncoderControl`] the NSQ / entropy coder consume. `input` is the new
/// frame's samples (as the fixed path receives it). Mirrors the analysis half
/// of `silk_encode_frame_FLP` up to (not including) the bitrate loop.
pub fn silk_encode_frame_flp_analysis(
    ps_enc: &mut SilkEncoderState,
    s_enc_ctrl: &mut SilkEncoderControl,
    cond_coding: i32,
) {
    let ltp_mem_length = ps_enc.s_cmn.ltp_mem_length as usize;
    let x_frame_idx = ltp_mem_length;

    // Build the float x_buf from the fixed i16 x_buf (same post-LP samples).
    let buf_len_total = ps_enc.s_cmn.x_buf.len();
    let mut x_buf = vec![0.0f32; buf_len_total];
    for i in 0..buf_len_total {
        x_buf[i] = ps_enc.s_cmn.x_buf[i] as f32;
    }
    // Tiny anti-denormal dither on the new frame, as in encode_frame_FLP.
    let fs_khz = ps_enc.s_cmn.fs_khz as usize;
    let frame_length = ps_enc.s_cmn.frame_length as usize;
    let new_idx = x_frame_idx + LA_SHAPE_MS * fs_khz;
    for i in 0..8 {
        let off = new_idx + i * (frame_length >> 3);
        x_buf[off] += (1 - (i as i32 & 2)) as f32 * 1e-6;
    }

    let mut ctrl = EncCtrlFlp::default();
    let mut res = vec![0.0f32; buf_len_total];

    find_pitch_lags(ps_enc, &mut ctrl, &mut res, &x_buf);
    noise_shape_analysis(ps_enc, &mut ctrl, &res[x_frame_idx..], &x_buf, x_frame_idx);
    find_pred_coefs(ps_enc, &mut ctrl, &res, x_frame_idx, &x_buf, x_frame_idx, cond_coding);
    process_gains(ps_enc, &mut ctrl, cond_coding);

    // ---- float control -> fixed SilkEncoderControl (NSQ boundary) ----
    let nb_subfr = ps_enc.s_cmn.nb_subfr as usize;
    let shaping_order = ps_enc.s_cmn.shaping_lpc_order as usize;
    let order = ps_enc.s_cmn.predict_lpc_order as usize;

    *s_enc_ctrl = SilkEncoderControl::default();
    for i in 0..nb_subfr {
        for j in 0..shaping_order {
            s_enc_ctrl.ar_q13[i * MAX_SHAPE_LPC_ORDER + j] =
                float2int(ctrl.ar[i * MAX_SHAPE_LPC_ORDER + j] * 8192.0) as i16;
        }
    }
    for i in 0..nb_subfr {
        let lf_ar = float2int(ctrl.lf_ar_shp[i] * 16384.0);
        let lf_ma = float2int(ctrl.lf_ma_shp[i] * 16384.0);
        s_enc_ctrl.lf_shp_q14[i] = (lf_ar << 16) | ((lf_ma as u16) as i32);
        s_enc_ctrl.tilt_q14[i] = float2int(ctrl.tilt[i] * 16384.0);
        s_enc_ctrl.harm_shape_gain_q14[i] = float2int(ctrl.harm_shape_gain[i] * 16384.0);
    }
    s_enc_ctrl.lambda_q10 = float2int(ctrl.lambda * 1024.0);
    for i in 0..nb_subfr * LTP_ORDER {
        s_enc_ctrl.ltp_coef_q14[i] = float2int(ctrl.ltp_coef[i] * 16384.0) as i16;
    }
    for j in 0..2 {
        for i in 0..order {
            s_enc_ctrl.pred_coef_q12[j][i] = float2int(ctrl.pred_coef[j][i] * 4096.0) as i16;
        }
    }
    for i in 0..nb_subfr {
        s_enc_ctrl.gains_q16[i] = float2int(ctrl.gains[i] * 65536.0);
    }
    if ps_enc.s_cmn.indices.signal_type as i32 == TYPE_VOICED {
        s_enc_ctrl.ltp_scale_q14 =
            SILK_LTP_SCALES_TABLE_Q14[ps_enc.s_cmn.indices.ltp_scale_index as usize] as i32;
    } else {
        s_enc_ctrl.ltp_scale_q14 = 0;
    }
    s_enc_ctrl.pitch_l = ctrl.pitch_l;
    s_enc_ctrl.gains_unq_q16 = ctrl.gains_unq_q16;
    s_enc_ctrl.last_gain_index_prev = ctrl.last_gain_index_prev;
    // input/coding quality (Q14) — used by later logic and LBRR.
    s_enc_ctrl.input_quality_q14 = float2int(ctrl.input_quality * 16384.0);
    s_enc_ctrl.coding_quality_q14 = float2int(ctrl.coding_quality * 16384.0);
    s_enc_ctrl.ltp_red_cod_gain_q7 = float2int(ctrl.ltp_red_cod_gain * 128.0);
}
