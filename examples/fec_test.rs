// Decode with 1-in-N loss, recovering each lost frame via FEC from the NEXT
// packet (decode_fec), then decoding that packet normally. Compares the FEC
// reconstruction energy to plain PLC.
use rusty_opus::OpusDecoder;
use std::fs::File; use std::io::Read;
fn main() {
    let a: Vec<String> = std::env::args().collect();
    let (rate, ch, drop_every): (i32,usize,usize)=(a[1].parse().unwrap(),a[2].parse().unwrap(),a[4].parse().unwrap());
    let mut data=Vec::new(); File::open(&a[3]).unwrap().read_to_end(&mut data).unwrap();
    // parse all packets
    let mut pkts: Vec<Vec<u8>> = Vec::new();
    let mut pos=0; while pos+8<=data.len() { let ln=u32::from_be_bytes([data[pos],data[pos+1],data[pos+2],data[pos+3]]) as usize; pos+=8; pkts.push(data[pos..pos+ln].to_vec()); pos+=ln; }
    let fsz=(rate as usize/50)*1;
    let mut dec=OpusDecoder::new(rate,ch).unwrap();
    let mut out=vec![0f32; fsz*ch];
    let (mut fec_e, mut fec_n, mut fec_recovered)=(0f64,0usize,0usize);
    let mut i=0usize;
    while i<pkts.len() {
        let drop = i % drop_every == (drop_every-1);
        if drop {
            // recover frame i from packet i+1 via FEC (if exists)
            if i+1<pkts.len() {
                dec.decode_fec(&pkts[i+1], fsz, &mut out).unwrap();
                let e: f64 = out[..fsz*ch].iter().map(|&v|(v as f64)*(v as f64)).sum();
                fec_e+=e; fec_n+=1; if e>0.01 { fec_recovered+=1; }
            }
            // then decode packet i+1 normally, skip i
            if i+1<pkts.len() { dec.decode(&pkts[i+1], fsz, &mut out).unwrap(); }
            i+=2;
        } else {
            dec.decode(&pkts[i], fsz, &mut out).unwrap();
            i+=1;
        }
    }
    println!("fec_frames={fec_n} recovered_with_audio={fec_recovered} mean_fec_energy={:.3}", fec_e/fec_n.max(1) as f64);
}

// Primary allocator for this target: our rusty_alloc, the pure-Rust mimalloc
// remake. Codec hot paths allocate heavily and the system heap dominates the
// profile there (measured 1.38x end-to-end on AV2 decode). Per project
// convention this belongs in binary/bench/example roots, never in a library --
// a library that declares one hijacks every dependent's allocator choice.
#[global_allocator]
static RUSTY_ALLOC: rusty_alloc_api::RustyAlloc = rusty_alloc_api::RustyAlloc;
