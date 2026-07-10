//! Opus multistream (surround) — port of the core of
//! `src/opus_multistream_{encoder,decoder}.c`. Wraps N mono/coupled Opus
//! coders behind a channel-mapping layout so >2-channel audio (quad, 5.1,
//! 7.1) can be coded as a set of standard Opus streams concatenated with the
//! self-delimited framing.
//!
//! The channel bitrate allocation here is a simple even split across streams
//! (coupled streams get 2x a mono stream's share) — libopus adds a
//! surround-masking analysis on top, a quality refinement, not a conformance
//! requirement. The bitstream layout, mapping, and per-stream Opus coding are
//! standard, so streams interoperate with libopus.

use crate::repacketizer::{parse_packet, Repacketizer};
use crate::{Application, Bandwidth, OpusDecoder, OpusEncoder};

/// Vorbis channel layout for mapping family 1, channels 1..=8:
/// (nb_streams, nb_coupled_streams, channel_mapping).
const VORBIS_MAPPINGS: [(usize, usize, &[u8]); 8] = [
    (1, 0, &[0]),                      // mono
    (1, 1, &[0, 1]),                   // stereo
    (2, 1, &[0, 2, 1]),                // 1-d (3.0)
    (2, 2, &[0, 1, 2, 3]),             // quad
    (3, 2, &[0, 4, 1, 2, 3]),          // 5.0
    (4, 2, &[0, 4, 1, 2, 3, 5]),       // 5.1
    (4, 3, &[0, 4, 1, 2, 3, 5, 6]),    // 6.1
    (5, 3, &[0, 6, 1, 2, 3, 4, 5, 7]), // 7.1
];

#[derive(Clone)]
pub struct ChannelLayout {
    pub nb_channels: usize,
    pub nb_streams: usize,
    pub nb_coupled_streams: usize,
    pub mapping: Vec<u8>,
}

impl ChannelLayout {
    /// Standard layout for a channel count + mapping family (0 = mono/stereo,
    /// 1 = Vorbis surround for 1..=8 channels).
    pub fn surround(channels: usize, mapping_family: i32) -> Result<Self, &'static str> {
        match mapping_family {
            0 => {
                if channels == 1 {
                    Ok(ChannelLayout {
                        nb_channels: 1,
                        nb_streams: 1,
                        nb_coupled_streams: 0,
                        mapping: vec![0],
                    })
                } else if channels == 2 {
                    Ok(ChannelLayout {
                        nb_channels: 2,
                        nb_streams: 1,
                        nb_coupled_streams: 1,
                        mapping: vec![0, 1],
                    })
                } else {
                    Err("family 0 supports only 1-2 channels")
                }
            }
            1 => {
                if !(1..=8).contains(&channels) {
                    return Err("family 1 supports 1-8 channels");
                }
                let (ns, nc, m) = VORBIS_MAPPINGS[channels - 1];
                Ok(ChannelLayout {
                    nb_channels: channels,
                    nb_streams: ns,
                    nb_coupled_streams: nc,
                    mapping: m.to_vec(),
                })
            }
            _ => Err("unsupported mapping family"),
        }
    }

    fn left_channel(&self, stream_id: usize, prev: i32) -> i32 {
        let start = if prev < 0 { 0 } else { prev as usize + 1 };
        for (i, &m) in self.mapping.iter().enumerate().skip(start) {
            if m as usize == stream_id * 2 {
                return i as i32;
            }
        }
        -1
    }
    fn right_channel(&self, stream_id: usize, prev: i32) -> i32 {
        let start = if prev < 0 { 0 } else { prev as usize + 1 };
        for (i, &m) in self.mapping.iter().enumerate().skip(start) {
            if m as usize == stream_id * 2 + 1 {
                return i as i32;
            }
        }
        -1
    }
    fn mono_channel(&self, stream_id: usize, prev: i32) -> i32 {
        let start = if prev < 0 { 0 } else { prev as usize + 1 };
        for (i, &m) in self.mapping.iter().enumerate().skip(start) {
            if m as usize == stream_id + self.nb_coupled_streams {
                return i as i32;
            }
        }
        -1
    }
}

/// Multistream encoder: one Opus encoder per stream (coupled = stereo, the
/// rest mono), coded per the channel layout and concatenated self-delimited.
pub struct OpusMSEncoder {
    layout: ChannelLayout,
    encoders: Vec<OpusEncoder>,
    sample_rate: i32,
    /// Total target bitrate across all streams (split evenly, coupled=2x mono).
    pub bitrate_bps: i32,
}

impl OpusMSEncoder {
    pub fn new(
        sample_rate: i32,
        channels: usize,
        mapping_family: i32,
        application: Application,
    ) -> Result<Self, &'static str> {
        let layout = ChannelLayout::surround(channels, mapping_family)?;
        let mut encoders = Vec::with_capacity(layout.nb_streams);
        for s in 0..layout.nb_streams {
            let ch = if s < layout.nb_coupled_streams { 2 } else { 1 };
            encoders.push(OpusEncoder::new(sample_rate, ch, application)?);
        }
        let mut enc = OpusMSEncoder {
            layout,
            encoders,
            sample_rate,
            bitrate_bps: 64000 * channels as i32,
        };
        enc.set_bitrate(enc.bitrate_bps);
        Ok(enc)
    }

    /// Split the total bitrate across streams (each coupled stream gets 2x a
    /// mono stream's share, matching its 2 channels).
    pub fn set_bitrate(&mut self, total: i32) {
        self.bitrate_bps = total;
        let units = self.layout.nb_coupled_streams * 2
            + (self.layout.nb_streams - self.layout.nb_coupled_streams);
        let per_unit = if units > 0 { total / units as i32 } else { total };
        for (s, e) in self.encoders.iter_mut().enumerate() {
            e.bitrate_bps = if s < self.layout.nb_coupled_streams {
                per_unit * 2
            } else {
                per_unit
            };
        }
    }

    pub fn nb_streams(&self) -> usize {
        self.layout.nb_streams
    }

    /// Encode one frame of interleaved `input` (nb_channels per sample) into a
    /// multistream packet. `scratch` output is returned as a Vec.
    pub fn encode(&mut self, input: &[f32], frame_size: usize) -> Result<Vec<u8>, &'static str> {
        let nch = self.layout.nb_channels;
        let mut out: Vec<u8> = Vec::new();
        let mut stream_buf = vec![0f32; frame_size * 2];
        let mut pkt = vec![0u8; 1500 + frame_size];

        for s in 0..self.layout.nb_streams {
            let coupled = s < self.layout.nb_coupled_streams;
            let sch = if coupled { 2 } else { 1 };
            // Gather this stream's channels from the interleaved input.
            if coupled {
                let l = self.layout.left_channel(s, -1);
                let r = self.layout.right_channel(s, -1);
                for i in 0..frame_size {
                    stream_buf[i * 2] = if l >= 0 { input[i * nch + l as usize] } else { 0.0 };
                    stream_buf[i * 2 + 1] =
                        if r >= 0 { input[i * nch + r as usize] } else { 0.0 };
                }
            } else {
                let m = self.layout.mono_channel(s, -1);
                for i in 0..frame_size {
                    stream_buf[i] = if m >= 0 { input[i * nch + m as usize] } else { 0.0 };
                }
            }
            let n = self.encoders[s].encode(&stream_buf[..frame_size * sch], frame_size, &mut pkt)?;
            // All streams but the last are self-delimited so the decoder can
            // find each stream's boundary.
            if s != self.layout.nb_streams - 1 {
                let mut rp = Repacketizer::new();
                rp.cat(&pkt[..n])?;
                out.extend_from_slice(&rp.out_self_delimited()?);
            } else {
                out.extend_from_slice(&pkt[..n]);
            }
        }
        Ok(out)
    }

    pub fn sample_rate(&self) -> i32 {
        self.sample_rate
    }
}

/// Multistream decoder: decode each stream and remux to the output channels.
pub struct OpusMSDecoder {
    layout: ChannelLayout,
    decoders: Vec<OpusDecoder>,
}

impl OpusMSDecoder {
    pub fn new(
        sample_rate: i32,
        channels: usize,
        mapping_family: i32,
    ) -> Result<Self, &'static str> {
        let layout = ChannelLayout::surround(channels, mapping_family)?;
        let mut decoders = Vec::with_capacity(layout.nb_streams);
        for s in 0..layout.nb_streams {
            let ch = if s < layout.nb_coupled_streams { 2 } else { 1 };
            decoders.push(OpusDecoder::new(sample_rate, ch)?);
        }
        Ok(OpusMSDecoder { layout, decoders })
    }

    /// Decode a multistream packet into interleaved `output` (nb_channels per
    /// sample). Returns the number of samples per channel.
    pub fn decode(
        &mut self,
        packet: &[u8],
        frame_size: usize,
        output: &mut [f32],
    ) -> Result<usize, &'static str> {
        let nch = self.layout.nb_channels;
        let mut buf = vec![0f32; frame_size * 2];
        let mut data = packet;
        let mut produced = frame_size;

        for s in 0..self.layout.nb_streams {
            let coupled = s < self.layout.nb_coupled_streams;
            let last = s == self.layout.nb_streams - 1;
            // Determine this stream's byte slice.
            let (stream_slice, advance) = if last {
                (data, data.len())
            } else {
                let (_toc, _frames, off) = parse_packet(data, true)?;
                (&data[..off], off)
            };
            let n = self.decoders[s].decode(stream_slice, frame_size, &mut buf)?;
            produced = n;
            // Remux this stream's channel(s) to the output.
            if coupled {
                let mut prev = -1;
                loop {
                    let chan = self.layout.left_channel(s, prev);
                    if chan == -1 {
                        break;
                    }
                    for i in 0..n {
                        output[i * nch + chan as usize] = buf[i * 2];
                    }
                    prev = chan;
                }
                let mut prev = -1;
                loop {
                    let chan = self.layout.right_channel(s, prev);
                    if chan == -1 {
                        break;
                    }
                    for i in 0..n {
                        output[i * nch + chan as usize] = buf[i * 2 + 1];
                    }
                    prev = chan;
                }
            } else {
                let mut prev = -1;
                loop {
                    let chan = self.layout.mono_channel(s, prev);
                    if chan == -1 {
                        break;
                    }
                    for i in 0..n {
                        output[i * nch + chan as usize] = buf[i];
                    }
                    prev = chan;
                }
            }
            if !last {
                data = &data[advance..];
            }
        }
        // Unmapped channels (mapping == 255) are silenced.
        for c in 0..nch {
            if self.layout.mapping.get(c).copied() == Some(255) {
                for i in 0..produced {
                    output[i * nch + c] = 0.0;
                }
            }
        }
        Ok(produced)
    }
}

/// Bandwidth passthrough helper (so callers can cap all streams at once).
impl OpusMSEncoder {
    pub fn set_max_bandwidth(&mut self, bw: Bandwidth) {
        for e in &mut self.encoders {
            e.max_bandwidth = bw;
        }
    }
}
