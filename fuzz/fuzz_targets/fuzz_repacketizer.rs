#![no_main]
//! Packet utilities on attacker-controlled bytes: parsing, padding, unpadding,
//! and repacketizing a sequence of packets carved out of the input. None of
//! these may panic, and a successful merge must split back into its frames.

use libfuzzer_sys::fuzz_target;
use rusty_opus::repacketizer::{self, Repacketizer};

fuzz_target!(|data: &[u8]| {
    let _ = repacketizer::nb_frames(data);
    let _ = repacketizer::parse_packet(data, false);
    let _ = repacketizer::parse_packet(data, true);
    let _ = repacketizer::unpad_packet(data);

    if let Some((&first, rest)) = data.split_first() {
        let mut p = rest.to_vec();
        let _ = repacketizer::pad_packet(&mut p, usize::from(first) * 7);

        // Carve the rest into packets: each prefixed by a one-byte length.
        let mut rp = Repacketizer::new();
        let mut cursor = rest;
        while let Some((&len, tail)) = cursor.split_first() {
            let take = usize::from(len).min(tail.len());
            let _ = rp.cat(&tail[..take]);
            cursor = &tail[take..];
        }
        let n = rp.nb_frames();
        if let Ok(merged) = rp.out() {
            // A merged packet must re-parse with the same frame count.
            assert_eq!(repacketizer::nb_frames(&merged).ok(), Some(n as i32));
        }
        let _ = rp.out_self_delimited();
        if n > 0 {
            let _ = rp.out_range(usize::from(first) % n, n);
        }
    }
});
