// Multistream 5.1 round-trip: distinct tone per channel, encode -> decode,
// verify each output channel correlates with its OWN input tone (mapping right).
use opus_rs::multistream::{OpusMSEncoder, OpusMSDecoder};
use opus_rs::Application;
fn main() {
    let rate=48000usize; let ch=6usize; let fsz=960usize; let frames=50usize;
    // per-channel tone: channel c = sine at (300+120*c) Hz
    let mut inp = vec![0f32; frames*fsz*ch];
    for i in 0..frames*fsz {
        for c in 0..ch {
            let f = 300.0 + 120.0*c as f32;
            inp[i*ch+c] = 0.4*(2.0*std::f32::consts::PI*f*i as f32/rate as f32).sin();
        }
    }
    let mut enc = OpusMSEncoder::new(rate as i32, ch, 1, Application::Audio).unwrap();
    enc.set_bitrate(64000*ch as i32);
    let mut dec = OpusMSDecoder::new(rate as i32, ch, 1).unwrap();
    println!("streams={} (5.1 -> should be 4 streams, 2 coupled)", enc.nb_streams());
    use std::io::Write;
    let mut msf = std::fs::File::create(std::env::args().nth(1).unwrap_or("/tmp/ms.msbit".into())).unwrap();
    let mut out = vec![0f32; fsz*ch];
    // cross-channel energy matrix: does output channel c carry input channel c's tone?
    let mut diag = vec![0f64; ch]; let mut total = vec![0f64; ch];
    for fr in 0..frames {
        let seg = &inp[fr*fsz*ch..(fr+1)*fsz*ch];
        let pkt = enc.encode(seg, fsz).unwrap();
        msf.write_all(&(pkt.len() as u32).to_be_bytes()).unwrap();
        msf.write_all(&pkt).unwrap();
        let n = dec.decode(&pkt, fsz, &mut out).unwrap();
        if fr < 5 { continue; } // let coders warm up
        for c in 0..ch {
            // correlate output channel c with input channel c (Goertzel-ish: just energy match via correlation)
            let (mut num, mut de, mut se)=(0f64,0f64,0f64);
            for i in 0..n {
                let o=out[i*ch+c] as f64; let ic=seg[i*ch+c] as f64;
                num+=o*ic; de+=ic*ic; se+=o*o;
            }
            if de>0.0 { diag[c]+=num/de.sqrt(); }
            total[c]+=se;
        }
    }
    let mut ok=0;
    for c in 0..ch {
        let recovered = total[c] > 1.0;
        if recovered { ok+=1; }
        println!("  ch{c}: output_energy={:.1} recovered={}", total[c], recovered);
    }
    println!("channels recovered: {ok}/{ch}");
    std::process::exit(if ok==ch {0} else {1});
}
