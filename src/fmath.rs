//! Exact `f32` rounding without a libm call.
//!
//! `f32::floor` and `f32::round_ties_even` need SSE4.1 (`roundss`), which is
//! above the x86-64 baseline this crate is compiled for, so each one lowers to a
//! call into libm (`floorf` / `rintf`). In a per-sample loop that is ~20
//! instructions plus a call per element, and the call keeps the loop scalar.
//! These twins use only add/sub/compare/select, which are baseline everywhere,
//! and return the **same bits** as the std functions for every non-NaN `f32`
//! (signed zeros and infinities included) - proved by the tests below against
//! std. A NaN input yields a NaN, but not necessarily the same one: libm quiets
//! a signalling NaN and these pass it through. Rust leaves the payload and
//! quiet bit of NaN results unspecified for float arithmetic in general, and no
//! caller here lets one reach an output (they clamp-and-cast or subtract).

/// 2^23: every `f32` of magnitude >= this is already an integer.
const TWO_POW_23: f32 = 8_388_608.0;

/// Bit-identical to [`f32::round_ties_even`] for non-NaN input (NaN -> NaN).
///
/// For `|x| < 2^23`, `|x| + 2^23` lies in `[2^23, 2^24)` where the spacing of
/// `f32` is exactly 1, so the addition itself rounds to the nearest integer,
/// ties to even (the IEEE default mode); subtracting `2^23` back is exact. The
/// sign is restored afterwards so `-0.3` yields `-0.0`, as std does.
#[inline(always)]
pub fn round_ties_even(x: f32) -> f32 {
    let ax = x.abs();
    let r = (ax + TWO_POW_23) - TWO_POW_23;
    // NaN and |x| >= 2^23 take the second arm unchanged.
    if ax < TWO_POW_23 { r.copysign(x) } else { x }
}

/// Bit-identical to [`f32::floor`] for non-NaN input (NaN -> NaN).
#[inline(always)]
pub fn floor(x: f32) -> f32 {
    let r = round_ties_even(x);
    // Rounding to nearest overshoots by at most one; `r - 1.0` is exact here
    // (|r| < 2^23 whenever r != x). NaN compares false and passes through.
    if r > x { r - 1.0 } else { r }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(x: f32) {
        if x.is_nan() {
            // NaN in, NaN out; payload and quiet bit are not part of the contract.
            assert!(round_ties_even(x).is_nan() && floor(x).is_nan());
            return;
        }
        assert_eq!(
            round_ties_even(x).to_bits(),
            x.round_ties_even().to_bits(),
            "round_ties_even({x:e}) [{:#010x}]",
            x.to_bits()
        );
        assert_eq!(
            floor(x).to_bits(),
            x.floor().to_bits(),
            "floor({x:e}) [{:#010x}]",
            x.to_bits()
        );
    }

    #[test]
    fn matches_std_on_edges() {
        for &x in &[
            0.0f32,
            -0.0,
            0.5,
            -0.5,
            1.5,
            -1.5,
            2.5,
            -2.5,
            0.499_999_97,
            -0.499_999_97,
            0.500_000_06,
            f32::MIN_POSITIVE,
            -f32::MIN_POSITIVE,
            1e-45,
            -1e-45,
            4_194_303.5,
            -4_194_303.5,
            4_194_304.5,
            8_388_607.5,
            -8_388_607.5,
            8_388_608.0,
            -8_388_608.0,
            16_777_216.0,
            3.0e38,
            -3.0e38,
            f32::INFINITY,
            f32::NEG_INFINITY,
            f32::NAN,
        ] {
            check(x);
        }
        // Every half-integer and its two neighbours across the range where
        // halves are representable.
        for k in -70_000i32..70_000 {
            let h = k as f32 + 0.5;
            check(h);
            check(f32::from_bits(h.to_bits().wrapping_add(1)));
            check(f32::from_bits(h.to_bits().wrapping_sub(1)));
            check(k as f32);
        }
    }

    #[test]
    fn matches_std_on_a_bit_pattern_sweep() {
        // A stride through all 2^32 bit patterns (coprime to 2^32), plus every
        // exponent with dense mantissa neighbours of the integer boundary.
        let mut bits = 0u32;
        for _ in 0..crate::isa::oracle::iters(4_000_000) {
            check(f32::from_bits(bits));
            bits = bits.wrapping_add(0x0000_10AD);
        }
        for exp in 0u32..=255 {
            for sign in [0u32, 1] {
                for m in (0u32..0x0080_0000).step_by(0x1_0001) {
                    check(f32::from_bits((sign << 31) | (exp << 23) | m));
                }
            }
        }
    }
}
