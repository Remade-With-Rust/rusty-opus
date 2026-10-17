#![no_main]
//! Multistream (surround) decoding of attacker-controlled bytes for every
//! channel count and mapping family. Must never panic.

use libfuzzer_sys::fuzz_target;
use rusty_opus::multistream::OpusMSDecoder;

fuzz_target!(|data: &[u8]| {
    let Some((&cfg, packet)) = data.split_first() else { return };
    let channels = usize::from(cfg % 8) + 1;
    let family = if channels <= 2 && cfg & 0x80 == 0 { 0 } else { 1 };
    let rate = [8000, 12000, 16000, 24000, 48000][usize::from(cfg >> 3) % 5];
    let Ok(mut dec) = OpusMSDecoder::new(rate, channels, family) else { return };
    let frame = rate as usize * 120 / 1000;
    let mut out = vec![0f32; frame * channels];
    let _ = dec.decode(packet, frame, &mut out);
});
