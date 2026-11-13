use crate::silk::biquad_alt::{silk_biquad_alt_stride1, silk_biquad_alt_stride2};
use crate::silk::macros::*;

const SILK_FIX_CONST_19: i32 =
    ((1.5 * std::f64::consts::PI / 1000.0) * (1 << 19) as f64 + 0.5) as i32;

#[inline(never)]
pub fn hp_cutoff(
    input: &[f32],
    cutoff_hz: i32,
    output: &mut [i16],
    hp_mem: &mut [i32],
    len: usize,
    channels: usize,
    fs: i32,
) {
    let mut b_q28 = [0i32; 3];
    let mut a_q28 = [0i32; 2];

    let fc_q19 = silk_div32_16(silk_smulbb(SILK_FIX_CONST_19, cutoff_hz), fs / 1000);

    let r_q28 = (1i32 << 28) - silk_mul(471, fc_q19);

    b_q28[0] = r_q28;
    b_q28[1] = -silk_lshift(r_q28, 1);
    b_q28[2] = r_q28;

    let r_q22 = silk_rshift(r_q28, 6);
    a_q28[0] = silk_smulww(r_q22, silk_smulww(fc_q19, fc_q19) - (2i32 << 22));
    a_q28[1] = silk_smulww(r_q22, r_q22);

    // The f32 -> i16 conversion goes through a small block scratch instead of
    // one frame-sized array: the old `[0i16; 11520]` (sized for 120 ms of 48 kHz
    // stereo) zero-filled 23 KB on every call to hold a 320..1920-sample frame.
    // The biquad is a per-sample recurrence carried in `s`, so running it block
    // by block is the same arithmetic in the same order.
    const BLOCK: usize = 480; // even, so a stereo block is whole sample pairs
    let mut block = [0i16; BLOCK];
    let convert = |src: &[f32], dst: &mut [i16]| {
        for (d, &x) in dst.iter_mut().zip(src) {
            // `fmath::floor` is bit-identical to `f32::floor` without the
            // per-sample libm call.
            *d = crate::fmath::floor(x * 32768.0 + 0.5).clamp(-32768.0, 32767.0) as i16;
        }
    };

    if channels == 1 {
        let s = &mut [hp_mem[0], hp_mem[1]];
        for (src, out) in input.chunks(BLOCK).zip(output.chunks_mut(BLOCK)) {
            let buf = &mut block[..src.len()];
            convert(src, buf);
            silk_biquad_alt_stride1(buf, &b_q28, &a_q28, s, out);
        }
        hp_mem[0] = s[0];
        hp_mem[1] = s[1];
    } else {
        let s = &mut [hp_mem[0], hp_mem[1], hp_mem[2], hp_mem[3]];
        let total = 2 * len;
        for (src, out) in input[..total]
            .chunks(BLOCK)
            .zip(output[..total].chunks_mut(BLOCK))
        {
            let buf = &mut block[..src.len()];
            convert(src, buf);
            silk_biquad_alt_stride2(buf, &b_q28, &a_q28, s, out, src.len() / 2);
        }
        hp_mem[0] = s[0];
        hp_mem[1] = s[1];
        hp_mem[2] = s[2];
        hp_mem[3] = s[3];
    }
}
