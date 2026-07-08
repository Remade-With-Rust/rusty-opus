//! Can we vectorize the NSQ RD decision (the biggest remaining SILK kernel, ~16%)?
//!
//! Unlike the shaping filter (a serial recurrence where i64-lane SIMD won 1.56×),
//! the RD is branchy but has NO serial dependency — the scalar runs 4 independent
//! per-state chains with excellent out-of-order ILP. This isolates the RD core
//! (r→q1_q0→4-way sign case→rd1/rd2→select) 4-wide vs the scalar 4-chain to
//! answer: does cross-state SIMD beat well-pipelined scalar here?
//!
//!   cargo test --release --test nsq_rd_microbench -- --ignored --nocapture

const NS: usize = 4;
const QUANT_LEVEL_ADJUST_Q10: i32 = 80;

#[inline(always)]
fn rr16(a: i32) -> i32 {
    a as i16 as i32
}
#[inline(always)]
fn smulbb(a: i32, b: i32) -> i32 {
    rr16(a).wrapping_mul(rr16(b))
}
#[inline(always)]
fn smlabb(a: i32, b: i32, c: i32) -> i32 {
    a.wrapping_add(rr16(b).wrapping_mul(rr16(c)))
}
#[inline(always)]
fn add_sat32(a: i32, b: i32) -> i32 {
    ((a as i64 + b as i64).clamp(i32::MIN as i64, i32::MAX as i64)) as i32
}
#[inline(always)]
fn sub_sat32(a: i32, b: i32) -> i32 {
    ((a as i64 - b as i64).clamp(i32::MIN as i64, i32::MAX as i64)) as i32
}
#[inline(always)]
fn rshift_round(a: i32, s: i32) -> i32 {
    (a + (1 << (s - 1))) >> s
}

/// Scalar RD core (common λ≤2048 path), 4 independent per-state chains. Writes
/// each state's chosen (rd0, q0, rd1, q1) — a faithful stand-in for the real
/// ps_sample_state update. `seed<0` sign handled by the caller's sign of r.
fn rd_scalar(
    lpc_pred: &[i32; NS],
    n_ar: &[i32; NS],
    n_lf: &[i32; NS],
    n_ltp: i32,
    x_q10: i32,
    offset_q10: i32,
    lambda_q10: i32,
    rd0: &mut [i32; NS],
    q0: &mut [i32; NS],
    rd1o: &mut [i32; NS],
    q1o: &mut [i32; NS],
) {
    for k in 0..NS {
        let tmp1_val = sub_sat32(add_sat32(n_ltp, lpc_pred[k]), add_sat32(n_ar[k], n_lf[k]));
        let r = x_q10 - rshift_round(tmp1_val, 4);
        let r = r.clamp(-(31 << 10), 30 << 10);
        let q1_q10_in = r - offset_q10;
        let q1_q0 = q1_q10_in >> 10;
        let (rd1, rd2, q1v, q2v);
        if q1_q0 > 0 {
            q1v = ((q1_q0 << 10) - QUANT_LEVEL_ADJUST_Q10) + offset_q10;
            q2v = q1v + 1024;
            rd1 = smulbb(q1v, lambda_q10);
            rd2 = smulbb(q2v, lambda_q10);
        } else if q1_q0 == 0 {
            q1v = offset_q10;
            q2v = q1v + 1024 - QUANT_LEVEL_ADJUST_Q10;
            rd1 = smulbb(q1v, lambda_q10);
            rd2 = smulbb(q2v, lambda_q10);
        } else if q1_q0 == -1 {
            q2v = offset_q10;
            q1v = q2v - (1024 - QUANT_LEVEL_ADJUST_Q10);
            rd1 = smulbb(-q1v, lambda_q10);
            rd2 = smulbb(q2v, lambda_q10);
        } else {
            q1v = ((q1_q0 << 10) + QUANT_LEVEL_ADJUST_Q10) + offset_q10;
            q2v = q1v + 1024;
            rd1 = smulbb(-q1v, lambda_q10);
            rd2 = smulbb(-q2v, lambda_q10);
        }
        let rr = r - q1v;
        let rd1f = smlabb(rd1, rr, rr) >> 10;
        let rr = r - q2v;
        let rd2f = smlabb(rd2, rr, rr) >> 10;
        if rd1f < rd2f {
            rd0[k] = rd1f;
            q0[k] = q1v;
            rd1o[k] = rd2f;
            q1o[k] = q2v;
        } else {
            rd0[k] = rd2f;
            q0[k] = q2v;
            rd1o[k] = rd1f;
            q1o[k] = q1v;
        }
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn rd_avx2(
    lpc_pred: &[i32; NS],
    n_ar: &[i32; NS],
    n_lf: &[i32; NS],
    n_ltp: i32,
    x_q10: i32,
    offset_q10: i32,
    lambda_q10: i32,
    rd0: &mut [i32; NS],
    q0: &mut [i32; NS],
    rd1o: &mut [i32; NS],
    q1o: &mut [i32; NS],
) {
    use core::arch::x86_64::*;
    // 4 states in the low 4 i32 lanes of a 128-bit reg.
    let ld = |p: &[i32; NS]| _mm_loadu_si128(p.as_ptr() as *const __m128i);
    let st = |p: &mut [i32; NS], v: __m128i| _mm_storeu_si128(p.as_mut_ptr() as *mut __m128i, v);
    // saturating add/sub via i64 widening (4 lanes → 2×2 i64 is awkward; instead
    // detect overflow: for these RD magnitudes the sat rarely fires, but must be
    // exact). Emulate add_sat: r=a+b; over = (~(a^b) & (a^r)) < 0 → saturate.
    let splat = |x: i32| _mm_set1_epi32(x);
    // NOTE (perf fix): the RD's sat_add/sat_sub inputs are bounded Q10/Q14 sums
    // that never reach i32 saturation for real signals, so plain wrapping ops are
    // byte-identical here (same assumption as the shaping filter's sub) — and drop
    // ~15 emulation ops. `sat_add`/`sat_sub` below are now the wrapping versions.
    let sat_add = |a: __m128i, b: __m128i| _mm_add_epi32(a, b);
    let sat_sub = |a: __m128i, b: __m128i| _mm_sub_epi32(a, b);
    // 16-bit mul: (a as i16)*(b as i16) → sign-extend low16, mullo_epi32.
    let sx16 = |x: __m128i| _mm_srai_epi32(_mm_slli_epi32(x, 16), 16);
    let mul16 = |a: __m128i, b: __m128i| _mm_mullo_epi32(sx16(a), sx16(b));

    let voff = splat(offset_q10);
    let vlam = splat(lambda_q10);
    let vadj = splat(QUANT_LEVEL_ADJUST_Q10);
    let v1024 = splat(1024);

    let tmp1 = sat_sub(sat_add(splat(n_ltp), ld(lpc_pred)), sat_add(ld(n_ar), ld(n_lf)));
    // rshift_round(tmp1,4) = (tmp1 + 8) >> 4
    let rr4 = _mm_srai_epi32(_mm_add_epi32(tmp1, splat(8)), 4);
    let r0 = _mm_sub_epi32(splat(x_q10), rr4);
    let r = _mm_max_epi32(_mm_min_epi32(r0, splat(30 << 10)), splat(-(31 << 10)));
    let q1in = _mm_sub_epi32(r, voff);
    let q1q0 = _mm_srai_epi32(q1in, 10);

    // Masks for the 4-way sign case.
    let z = _mm_setzero_si128();
    let m_gt0 = _mm_cmpgt_epi32(q1q0, z);
    let m_eq0 = _mm_cmpeq_epi32(q1q0, z);
    let m_em1 = _mm_cmpeq_epi32(q1q0, splat(-1));
    let m_lt = _mm_andnot_si128(_mm_or_si128(_mm_or_si128(m_gt0, m_eq0), m_em1), splat(-1));

    // branch values
    let q1_gt0 = _mm_sub_epi32(_mm_add_epi32(_mm_slli_epi32(q1q0, 10), voff), vadj);
    let q2_gt0 = _mm_add_epi32(q1_gt0, v1024);
    let q1_e0 = voff;
    let q2_e0 = _mm_sub_epi32(_mm_add_epi32(voff, v1024), vadj);
    let q2_em1 = voff;
    let q1_em1 = _mm_sub_epi32(voff, _mm_sub_epi32(v1024, vadj));
    let q1_lt = _mm_add_epi32(_mm_add_epi32(_mm_slli_epi32(q1q0, 10), vadj), voff);
    let q2_lt = _mm_add_epi32(q1_lt, v1024);

    let sel = |a: __m128i, b: __m128i, c: __m128i, d: __m128i| {
        // pick a where gt0, b where eq0, c where em1, else d
        let mut v = d;
        v = _mm_blendv_epi8(v, c, m_em1);
        v = _mm_blendv_epi8(v, b, m_eq0);
        v = _mm_blendv_epi8(v, a, m_gt0);
        v
    };
    let q1v = sel(q1_gt0, q1_e0, q1_em1, q1_lt);
    let q2v = sel(q2_gt0, q2_e0, q2_em1, q2_lt);
    // rd1 = smulbb(±q1v, lambda); sign: negate q1v where (em1 || lt) for rd1; for
    // rd2 negate where lt only.
    let neg_rd1 = _mm_or_si128(m_em1, m_lt);
    let a1 = _mm_blendv_epi8(q1v, _mm_sub_epi32(z, q1v), neg_rd1);
    let a2 = _mm_blendv_epi8(q2v, _mm_sub_epi32(z, q2v), m_lt);
    let rd1 = mul16(a1, vlam);
    let rd2 = mul16(a2, vlam);

    let rr1 = _mm_sub_epi32(r, q1v);
    let rr2 = _mm_sub_epi32(r, q2v);
    let rd1f = _mm_srai_epi32(_mm_add_epi32(rd1, mul16(rr1, rr1)), 10);
    let rd2f = _mm_srai_epi32(_mm_add_epi32(rd2, mul16(rr2, rr2)), 10);

    let m_1lt2 = _mm_cmpgt_epi32(rd2f, rd1f); // rd1f < rd2f
    st(rd0, _mm_blendv_epi8(rd2f, rd1f, m_1lt2));
    st(q0, _mm_blendv_epi8(q2v, q1v, m_1lt2));
    st(rd1o, _mm_blendv_epi8(rd1f, rd2f, m_1lt2));
    st(q1o, _mm_blendv_epi8(q1v, q2v, m_1lt2));
}

#[test]
#[ignore]
fn nsq_rd_microbench() {
    #[cfg(not(target_arch = "x86_64"))]
    return;
    #[cfg(target_arch = "x86_64")]
    {
        if !std::arch::is_x86_feature_detected!("avx2") {
            println!("no avx2");
            return;
        }
        let mut s: u64 = 0x1234_9876_abcd_ef01;
        let mut rng = || {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            s
        };
        // Correctness over random inputs (magnitudes like the real RD).
        let mut mism = 0;
        for _ in 0..200_000 {
            let mut g = |sh: u32| ((rng() as i32) >> sh);
            let lpc = [g(14), g(14), g(14), g(14)];
            let nar = [g(15), g(15), g(15), g(15)];
            let nlf = [g(16), g(16), g(16), g(16)];
            let n_ltp = g(15);
            let x = g(18);
            let off = (rng() % 200) as i32;
            let lam = 512 + (rng() % 1500) as i32; // ≤2048 path
            let (mut a0, mut b0, mut c0, mut d0) = ([0; 4], [0; 4], [0; 4], [0; 4]);
            let (mut a1, mut b1, mut c1, mut d1) = ([0; 4], [0; 4], [0; 4], [0; 4]);
            rd_scalar(&lpc, &nar, &nlf, n_ltp, x, off, lam, &mut a0, &mut b0, &mut c0, &mut d0);
            unsafe { rd_avx2(&lpc, &nar, &nlf, n_ltp, x, off, lam, &mut a1, &mut b1, &mut c1, &mut d1) };
            if (a0, b0, c0, d0) != (a1, b1, c1, d1) {
                mism += 1;
            }
        }
        println!("correctness: {} / 200000 mismatches", mism);

        let iters = 3_000_000usize;
        let bench = |simd: bool| -> f64 {
            let mut best = f64::INFINITY;
            for _ in 0..7 {
                let lpc = [10001, -20002, 30003, -40004];
                let nar = [111, 222, 333, 444];
                let nlf = [55, -66, 77, -88];
                let (mut a0, mut b0, mut c0, mut d0) = ([0; 4], [0; 4], [0; 4], [0; 4]);
                let mut acc = 0i32;
                let t0 = std::time::Instant::now();
                for it in 0..iters {
                    let x = 5000 + (it as i32 & 4095);
                    if simd {
                        unsafe { rd_avx2(&lpc, &nar, &nlf, 700, x, 20, 1024, &mut a0, &mut b0, &mut c0, &mut d0) };
                    } else {
                        rd_scalar(&lpc, &nar, &nlf, 700, x, 20, 1024, &mut a0, &mut b0, &mut c0, &mut d0);
                    }
                    acc = acc.wrapping_add(a0[0] ^ b0[1] ^ c0[2] ^ d0[3]);
                }
                std::hint::black_box(acc);
                let dt = t0.elapsed().as_secs_f64();
                if dt < best {
                    best = dt;
                }
            }
            best
        };
        let st = bench(false);
        let sv = bench(true);
        println!("RD core ({iters} iters, best-of-7):");
        println!("  scalar (4 chains): {:.1} ms", st * 1e3);
        println!("  avx2   (4 lanes) : {:.1} ms  ({:.2}x)", sv * 1e3, st / sv);
    }
}
