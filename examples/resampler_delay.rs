// Measure the SILK decode resampler's group delay for each internal rate -> 48 kHz.
// A correct decoder has a CONSTANT total delay across NB/MB/WB; if the per-rate
// delays differ, that difference is the bandwidth-transition alignment step that
// breaks conformance.
use rusty_opus::SilkResampler;

fn measure(fs_in: i32) -> f64 {
    let out_rate = 48000;
    let mut r = SilkResampler::default();
    r.init(fs_in, out_rate);
    let in_khz = (fs_in / 1000) as usize;
    let frame = in_khz * 20; // 20 ms input frame
    let ratio = out_rate / fs_in;
    let out_frame = frame * ratio as usize;

    // Feed a few silent priming frames, then a single impulse frame, then silence,
    // and locate the centroid of the |output| energy of the impulse response.
    let mut out = vec![0i16; out_frame + 16];
    // prime
    for _ in 0..4 {
        r.process(&mut out, &vec![0i16; frame], frame as i32);
    }
    // impulse at input sample 0 of this frame
    let mut imp = vec![0i16; frame];
    imp[0] = 10000;
    r.process(&mut out, &imp, frame as i32);
    let e: f64 = out[..out_frame].iter().map(|&x| (x as f64) * (x as f64)).sum();
    let centroid: f64 = out[..out_frame]
        .iter()
        .enumerate()
        .map(|(i, &x)| i as f64 * (x as f64) * (x as f64))
        .sum::<f64>()
        / e.max(1.0);
    centroid
}

fn main() {
    for &fs in &[8000, 12000, 16000] {
        let d = measure(fs);
        println!("in {:5} Hz -> 48000: impulse-energy centroid @ {:.2} output samples", fs, d);
    }
}
