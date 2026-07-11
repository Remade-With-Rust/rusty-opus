// Decode a .bit stream (opus_demo framing) with simulated packet loss: every
// Nth packet is dropped (decoded as lost -> PLC). Reports energy continuity.
use rusty_opus::OpusDecoder;
use std::fs::File;
use std::io::{Read, Write};
fn main() {
    let a: Vec<String> = std::env::args().collect();
    let (rate, ch, drop_every): (i32, usize, usize) =
        (a[1].parse().unwrap(), a[2].parse().unwrap(), a[4].parse().unwrap());
    // Optional arg 5: write decoded s16 PCM (with concealment) here.
    let mut pcm_out = a.get(5).map(|p| std::io::BufWriter::new(File::create(p).unwrap()));
    // NODROP=1 decodes every packet (clean reference).
    let nodrop = std::env::var("NODROP").is_ok();
    let mut data = Vec::new();
    File::open(&a[3]).unwrap().read_to_end(&mut data).unwrap();
    let mut dec = OpusDecoder::new(rate, ch).unwrap();
    let fsz = (rate as usize / 50) * 1; // 20ms
    let mut out = vec![0f32; fsz * ch];
    let (mut pos, mut n, mut lost, mut concealed_energy, mut concealed_frames) = (0usize, 0usize, 0usize, 0f64, 0usize);
    let mut plc_silent = 0usize;
    while pos + 8 <= data.len() {
        let ln = u32::from_be_bytes([data[pos],data[pos+1],data[pos+2],data[pos+3]]) as usize;
        pos += 8;
        let pkt = &data[pos..pos+ln]; pos += ln;
        let drop = !nodrop && n % drop_every == (drop_every - 1);
        let r = if drop {
            lost += 1;
            dec.decode(&[], fsz, &mut out)
        } else {
            dec.decode(pkt, fsz, &mut out)
        };
        if r.is_err() { eprintln!("decode err at {}: {:?}", n, r); std::process::exit(1); }
        if drop {
            let e: f64 = out[..fsz*ch].iter().map(|&v| (v as f64)*(v as f64)).sum();
            concealed_energy += e; concealed_frames += 1;
            if e < 1e-6 { plc_silent += 1; }
        }
        if let Some(w) = pcm_out.as_mut() {
            for &v in &out[..fsz*ch] {
                let s = (v * 32768.0).clamp(-32768.0, 32767.0) as i16;
                w.write_all(&s.to_le_bytes()).unwrap();
            }
        }
        n += 1;
    }
    println!("packets={n} lost/concealed={lost} mean_concealed_energy={:.2} silent_conceals={plc_silent}/{concealed_frames}",
        concealed_energy / concealed_frames.max(1) as f64);
}
