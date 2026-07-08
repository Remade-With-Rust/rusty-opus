//! Path 2 decisive experiment: does cross-state SIMD of the NSQ warped shaping
//! filter (the recurrence-heavy 33% of NSQ) beat the 4-independent-chains scalar?
//!
//! The scalar processes 4 del-dec states as 4 independent serial recurrences —
//! the CPU's out-of-order engine interleaves them, hiding the per-tap latency.
//! The best-case SIMD keeps the 4 states as i64 lanes (no per-op narrow/permute,
//! unlike the reverted S1d hand-AVX2) and runs ONE vector chain. This isolates
//! the kernel to answer: is 4-wide cross-state SIMD worth a full NSQ rewrite?
//!
//!   cargo test --release --test nsq_shape_microbench -- --ignored --nocapture

const ORDER: usize = 24; // shaping_lpc_order at complexity ≥ 8
const NS: usize = 4;

#[inline(always)]
fn smlawb(a: i32, b: i32, c: i32) -> i32 {
    a.wrapping_add((((b as i64) * (c as i16 as i64)) >> 16) as i32)
}

/// Scalar: 4 independent per-state recurrences (the current NSQ shape).
fn shape_scalar(sar: &mut [[i32; NS]], diff: &[i32; NS], warp: i32, ar: &[i16], n_ar: &mut [i32; NS]) {
    for k in 0..NS {
        let mut tmp2 = smlawb(diff[k], sar[0][k], warp);
        let mut tmp1 = smlawb(sar[0][k], sar[1][k].wrapping_sub(tmp2), warp);
        sar[0][k] = tmp2;
        let mut acc = (ORDER as i32) >> 1;
        acc = smlawb(acc, tmp2, ar[0] as i32);
        let mut j = 2;
        while j < ORDER {
            tmp2 = smlawb(sar[j - 1][k], sar[j][k].wrapping_sub(tmp1), warp);
            sar[j - 1][k] = tmp1;
            acc = smlawb(acc, tmp1, ar[j - 1] as i32);
            tmp1 = smlawb(sar[j][k], sar[j + 1][k].wrapping_sub(tmp2), warp);
            sar[j][k] = tmp2;
            acc = smlawb(acc, tmp2, ar[j] as i32);
            j += 2;
        }
        sar[ORDER - 1][k] = tmp1;
        n_ar[k] = smlawb(acc, tmp1, ar[ORDER - 1] as i32);
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn shape_avx2(sar: &mut [[i32; NS]], diff: &[i32; NS], warp: i32, ar: &[i16], n_ar: &mut [i32; NS]) {
    use core::arch::x86_64::*;
    // 4 states as 4 i64 lanes (values in i32 range). smlawb keeps everything i64
    // (mul_epi32 reads low-32 of each lane) — no per-op narrow/permute.
    let wb = _mm256_set1_epi64x(warp as i64);
    let asr16 = |x: __m256i| {
        let s = _mm256_cmpgt_epi64(_mm256_setzero_si256(), x);
        _mm256_or_si256(_mm256_srli_epi64(x, 16), _mm256_slli_epi64(s, 48))
    };
    // smlawb_i64(a, b, c_broadcast): a + ((b*c)>>16), all i64 lanes.
    let smlawb_v = |a: __m256i, b: __m256i, cb: __m256i| {
        _mm256_add_epi64(a, asr16(_mm256_mul_epi32(b, cb)))
    };
    // load 4 i32 -> 4 i64 lanes
    let ldv = |p: &[i32; NS]| _mm256_cvtepi32_epi64(_mm_loadu_si128(p.as_ptr() as *const __m128i));
    let stv = |p: &mut [i32; NS], v: __m256i| {
        // narrow 4 i64 -> 4 i32 (low dword of each lane) and store
        let g = _mm256_permutevar8x32_epi32(v, _mm256_setr_epi32(0, 2, 4, 6, 0, 2, 4, 6));
        _mm_storeu_si128(p.as_mut_ptr() as *mut __m128i, _mm256_castsi256_si128(g));
    };
    let cbv = |c: i32| _mm256_set1_epi64x(c as i16 as i64);

    let vdiff = ldv(diff);
    let mut vsar: [__m256i; ORDER] = core::array::from_fn(|j| ldv(&sar[j]));
    let mut tmp2 = smlawb_v(vdiff, vsar[0], wb);
    let mut tmp1 = smlawb_v(vsar[0], _mm256_sub_epi64(vsar[1], tmp2), wb);
    vsar[0] = tmp2;
    let mut acc = smlawb_v(_mm256_set1_epi64x(((ORDER as i32) >> 1) as i64), tmp2, cbv(ar[0] as i32));
    let mut j = 2;
    while j < ORDER {
        tmp2 = smlawb_v(vsar[j - 1], _mm256_sub_epi64(vsar[j], tmp1), wb);
        vsar[j - 1] = tmp1;
        acc = smlawb_v(acc, tmp1, cbv(ar[j - 1] as i32));
        tmp1 = smlawb_v(vsar[j], _mm256_sub_epi64(vsar[j + 1], tmp2), wb);
        vsar[j] = tmp2;
        acc = smlawb_v(acc, tmp2, cbv(ar[j] as i32));
        j += 2;
    }
    vsar[ORDER - 1] = tmp1;
    acc = smlawb_v(acc, tmp1, cbv(ar[ORDER - 1] as i32));
    for j in 0..ORDER {
        stv(&mut sar[j], vsar[j]);
    }
    stv(n_ar, acc);
}

#[test]
#[ignore]
fn nsq_shape_microbench() {
    #[cfg(not(target_arch = "x86_64"))]
    {
        println!("x86_64 only");
        return;
    }
    #[cfg(target_arch = "x86_64")]
    {
        if !std::arch::is_x86_feature_detected!("avx2") {
            println!("no avx2");
            return;
        }
        // Deterministic random state.
        let mut s: u64 = 0x1234_5678_9abc_def1;
        let mut rng = || {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            (s >> 40) as i32 - (1 << 23)
        };
        let ar: Vec<i16> = (0..ORDER).map(|_| (rng() >> 8) as i16).collect();
        let warp = 13421; // typical warping_q16
        let iters = 2_000_000usize;

        // Correctness first: both must produce identical output over random states.
        for _ in 0..1000 {
            let mut sar_a = [[0i32; NS]; ORDER];
            for row in sar_a.iter_mut() {
                for v in row.iter_mut() {
                    *v = rng() >> 6;
                }
            }
            let mut sar_b = sar_a;
            let diff = [rng() >> 6, rng() >> 6, rng() >> 6, rng() >> 6];
            let mut na = [0i32; NS];
            let mut nb = [0i32; NS];
            shape_scalar(&mut sar_a, &diff, warp, &ar, &mut na);
            unsafe { shape_avx2(&mut sar_b, &diff, warp, &ar, &mut nb) };
            assert_eq!(na, nb, "n_ar mismatch");
            assert_eq!(sar_a, sar_b, "sar mismatch");
        }
        println!("correctness: AVX2 == scalar over 1000 random states ✓");

        let bench = |f: &dyn Fn(&mut [[i32; NS]], &[i32; NS], i32, &[i16], &mut [i32; NS])| -> f64 {
            let mut best = f64::INFINITY;
            for _ in 0..7 {
                let mut sar = [[123i32; NS]; ORDER];
                let diff = [7, 11, 13, 17];
                let mut n_ar = [0i32; NS];
                let t0 = std::time::Instant::now();
                for _ in 0..iters {
                    f(&mut sar, &diff, warp, &ar, &mut n_ar);
                    // feed n_ar back so the optimizer can't hoist the loop
                    sar[0][0] = sar[0][0].wrapping_add(n_ar[0] & 1);
                }
                let dt = t0.elapsed().as_secs_f64();
                std::hint::black_box(&sar);
                if dt < best {
                    best = dt;
                }
            }
            best
        };

        let scalar_t = bench(&shape_scalar);
        let avx2_t = bench(&|sar, diff, w, ar, n| unsafe { shape_avx2(sar, diff, w, ar, n) });
        println!(
            "shape filter ({ORDER} taps, {NS} states, {iters} iters, best-of-7):",
        );
        println!("  scalar (4 chains): {:.1} ms", scalar_t * 1e3);
        println!("  avx2 (i64 lanes) : {:.1} ms  ({:.2}x)", avx2_t * 1e3, scalar_t / avx2_t);
    }
}
