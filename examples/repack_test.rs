// Repacketizer round-trips: parse a .bit stream's 20ms packets, (a) merge
// consecutive pairs into 40ms code-3 packets, (b) pad each to +5 bytes then
// unpad, verifying frame identity. Writes the merged stream in opus_demo .bit
// framing for oracle decode.
use rusty_opus::repacketizer::{pad_packet, unpad_packet, Repacketizer, parse_packet};
use std::fs::File; use std::io::{Read, Write};
fn main() {
    let a: Vec<String> = std::env::args().collect();
    let mut data = Vec::new(); File::open(&a[1]).unwrap().read_to_end(&mut data).unwrap();
    let mut pkts: Vec<Vec<u8>> = Vec::new();
    let mut pos=0; while pos+8<=data.len() { let ln=u32::from_be_bytes([data[pos],data[pos+1],data[pos+2],data[pos+3]]) as usize; pos+=8; pkts.push(data[pos..pos+ln].to_vec()); pos+=ln; }
    // (b) pad/unpad identity on every packet
    let mut pad_fail=0;
    for p in &pkts {
        let mut q = p.clone();
        pad_packet(&mut q, p.len()+5).unwrap();
        let back = unpad_packet(&q).unwrap();
        // frames must match: parse both, compare frame bytes
        let (_t0, f0, _) = parse_packet(p, false).unwrap();
        let (_t1, f1, _) = parse_packet(&back, false).unwrap();
        let fr0: Vec<&[u8]> = f0.iter().map(|&(o,l)| &p[o..o+l]).collect();
        let fr1: Vec<&[u8]> = f1.iter().map(|&(o,l)| &back[o..o+l]).collect();
        if fr0 != fr1 { pad_fail+=1; }
    }
    // (a) merge consecutive pairs -> 40ms
    let mut merged = Vec::new(); let mut merge_fail=0;
    let mut i=0;
    while i+1 < pkts.len() {
        let mut rp = Repacketizer::new();
        if rp.cat(&pkts[i]).is_err() || rp.cat(&pkts[i+1]).is_err() { i+=2; continue; }
        match rp.out() {
            Ok(m) => { merged.push(m); }
            Err(_) => { merge_fail+=1; }
        }
        i+=2;
    }
    println!("packets={} pad/unpad_mismatches={pad_fail} merged={} merge_fail={merge_fail}", pkts.len(), merged.len());
    // write merged .bit (len + dummy range 0 + payload) for oracle decode
    let mut out = File::create(&a[2]).unwrap();
    for m in &merged {
        out.write_all(&(m.len() as u32).to_be_bytes()).unwrap();
        out.write_all(&0u32.to_be_bytes()).unwrap();
        out.write_all(m).unwrap();
    }
}
