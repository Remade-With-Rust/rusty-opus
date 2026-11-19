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
//!
//! The decoder takes any RFC 7845 layout ([`ChannelLayout::new`], or the one
//! an `OpusHead` declares via [`ChannelLayout::from_opus_head`]), including
//! mapping family 255, and matches libopus `opus_multistream_decode_float`
//! (checked against libopus 1.6.1 in `tests/multistream_oracle.rs`).

use crate::Error;
use crate::repacketizer::{Repacketizer, parse_frames, samples_per_frame};
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

/// How the channels of a multistream signal map onto mono and stereo
/// (coupled) Opus streams (RFC 7845 channel mapping).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChannelLayout {
    /// Total output channels.
    pub nb_channels: usize,
    /// Number of Opus streams in each packet.
    pub nb_streams: usize,
    /// How many of those streams are stereo (coupled); they come first.
    pub nb_coupled_streams: usize,
    /// For each output channel, the decoded channel index it takes.
    pub mapping: Vec<u8>,
}

impl ChannelLayout {
    /// Standard layout for a channel count + mapping family (0 = mono/stereo,
    /// 1 = Vorbis surround for 1..=8 channels).
    ///
    /// # Errors
    ///
    /// [`Error::BadArg`] if `mapping_family` is not 0 or 1, or `channels` is out
    /// of range for it (1–2 for family 0, 1–8 for family 1).
    pub fn surround(channels: usize, mapping_family: i32) -> Result<Self, Error> {
        match mapping_family {
            0 => {
                if channels == 1 {
                    Ok(Self {
                        nb_channels: 1,
                        nb_streams: 1,
                        nb_coupled_streams: 0,
                        mapping: vec![0],
                    })
                } else if channels == 2 {
                    Ok(Self {
                        nb_channels: 2,
                        nb_streams: 1,
                        nb_coupled_streams: 1,
                        mapping: vec![0, 1],
                    })
                } else {
                    Err(Error::BadArg("family 0 supports only 1-2 channels"))
                }
            }
            1 => {
                if !(1..=8).contains(&channels) {
                    return Err(Error::BadArg("family 1 supports 1-8 channels"));
                }
                let (ns, nc, m) = VORBIS_MAPPINGS[channels - 1];
                Ok(Self {
                    nb_channels: channels,
                    nb_streams: ns,
                    nb_coupled_streams: nc,
                    mapping: m.to_vec(),
                })
            }
            _ => Err(Error::BadArg("unsupported mapping family")),
        }
    }

    /// An explicit layout: `nb_streams` streams per packet, the first
    /// `nb_coupled_streams` of them stereo, and for each of the `mapping.len()`
    /// output channels the decoded channel it takes (`255` = silent). Decoded
    /// channels number the coupled streams' left/right pairs first
    /// (`2*s`, `2*s+1`), then the mono streams (`nb_coupled_streams + s`).
    ///
    /// # Errors
    ///
    /// [`Error::BadArg`] unless the layout is valid per RFC 7845 §5.1.1 (and
    /// libopus `opus_multistream_decoder_init`): 1–255 channels, at least one
    /// stream, no more coupled streams than streams, at most 255 decoded
    /// channels (`nb_streams + nb_coupled_streams`), and every mapping entry
    /// either below that count or 255.
    pub fn new(
        nb_channels: usize,
        nb_streams: usize,
        nb_coupled_streams: usize,
        mapping: Vec<u8>,
    ) -> Result<Self, Error> {
        let layout = Self {
            nb_channels,
            nb_streams,
            nb_coupled_streams,
            mapping,
        };
        layout.validate()?;
        Ok(layout)
    }

    /// The layout an `OpusHead` identification header (RFC 7845 §5.1)
    /// declares: `head` is the whole header packet, starting with the
    /// `OpusHead` magic. Mapping family 0 implies the mono/stereo layout;
    /// families 1 (Vorbis surround, at most 8 channels) and 255 (application
    /// defined) carry an explicit stream count, coupled count and mapping table.
    ///
    /// # Errors
    ///
    /// [`Error::BadArg`] if `head` is not an `OpusHead` (bad magic, an
    /// incompatible major version, or truncated), declares an unsupported
    /// mapping family, or its layout is invalid (see [`ChannelLayout::new`]).
    pub fn from_opus_head(head: &[u8]) -> Result<Self, Error> {
        if head.len() < 19 || &head[..8] != b"OpusHead" {
            return Err(Error::BadArg("not an OpusHead"));
        }
        // RFC 7845 §5.1: the upper four bits are the major version; only 0
        // is defined, and a decoder must reject anything else.
        if head[8] >> 4 != 0 {
            return Err(Error::BadArg("unsupported OpusHead version"));
        }
        let channels = head[9] as usize;
        match head[18] {
            0 => Self::surround(channels, 0),
            family @ (1 | 255) => {
                if family == 1 && !(1..=8).contains(&channels) {
                    return Err(Error::BadArg("family 1 supports 1-8 channels"));
                }
                let table = head
                    .get(19..21 + channels)
                    .ok_or(Error::BadArg("OpusHead channel mapping table truncated"))?;
                Self::new(
                    channels,
                    table[0] as usize,
                    table[1] as usize,
                    table[2..].to_vec(),
                )
            }
            _ => Err(Error::BadArg("unsupported mapping family")),
        }
    }

    /// Check the invariants [`ChannelLayout::new`] documents (the fields are
    /// public, so a decoder re-checks what it is handed).
    fn validate(&self) -> Result<(), Error> {
        if !(1..=255).contains(&self.nb_channels) {
            return Err(Error::BadArg("channel count must be 1-255"));
        }
        if self.nb_streams < 1 || self.nb_coupled_streams > self.nb_streams {
            return Err(Error::BadArg("bad stream / coupled stream count"));
        }
        let decoded = self.nb_streams + self.nb_coupled_streams;
        if decoded > 255 {
            return Err(Error::BadArg("more than 255 decoded channels"));
        }
        if self.mapping.len() != self.nb_channels {
            return Err(Error::BadArg("mapping length differs from channel count"));
        }
        if self
            .mapping
            .iter()
            .any(|&m| m != 255 && m as usize >= decoded)
        {
            return Err(Error::BadArg("mapping entry names no decoded channel"));
        }
        Ok(())
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
    /// A surround encoder for `channels` channels with the standard layout of
    /// `mapping_family` (see [`ChannelLayout::surround`]).
    ///
    /// # Errors
    ///
    /// [`Error::BadArg`] for an unsupported channel count / mapping family, or any
    /// error from creating the per-stream encoders.
    pub fn new(
        sample_rate: i32,
        channels: usize,
        mapping_family: i32,
        application: Application,
    ) -> Result<Self, Error> {
        let layout = ChannelLayout::surround(channels, mapping_family)?;
        let mut encoders = Vec::with_capacity(layout.nb_streams);
        for s in 0..layout.nb_streams {
            let ch = if s < layout.nb_coupled_streams { 2 } else { 1 };
            encoders.push(OpusEncoder::new(sample_rate, ch, application)?);
        }
        let mut enc = Self {
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
        let per_unit = if units > 0 {
            total / units as i32
        } else {
            total
        };
        for (s, e) in self.encoders.iter_mut().enumerate() {
            e.bitrate_bps = if s < self.layout.nb_coupled_streams {
                per_unit * 2
            } else {
                per_unit
            };
        }
    }

    /// Number of Opus streams in each multistream packet.
    pub fn nb_streams(&self) -> usize {
        self.layout.nb_streams
    }

    /// Encode one frame of interleaved `input` (nb_channels per sample) into a
    /// multistream packet. `scratch` output is returned as a Vec.
    ///
    /// # Errors
    ///
    /// Any error from the per-stream [`crate::OpusEncoder::encode`] calls or from
    /// assembling the self-delimited multistream packet.
    pub fn encode(&mut self, input: &[f32], frame_size: usize) -> Result<Vec<u8>, Error> {
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
                    stream_buf[i * 2] = if l >= 0 {
                        input[i * nch + l as usize]
                    } else {
                        0.0
                    };
                    stream_buf[i * 2 + 1] = if r >= 0 {
                        input[i * nch + r as usize]
                    } else {
                        0.0
                    };
                }
            } else {
                let m = self.layout.mono_channel(s, -1);
                for i in 0..frame_size {
                    stream_buf[i] = if m >= 0 {
                        input[i * nch + m as usize]
                    } else {
                        0.0
                    };
                }
            }
            let n =
                self.encoders[s].encode(&stream_buf[..frame_size * sch], frame_size, &mut pkt)?;
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

    /// Input sampling rate in Hz.
    pub fn sample_rate(&self) -> i32 {
        self.sample_rate
    }
}

/// Multistream decoder: decode each stream and remux to the output channels.
pub struct OpusMSDecoder {
    layout: ChannelLayout,
    decoders: Vec<OpusDecoder>,
    sample_rate: i32,
    /// One stream's decoded samples (reused across packets).
    buf: Vec<f32>,
    /// Per-stream `(start, sd_field, next)` of the packet being decoded.
    spans: Vec<(usize, (usize, usize), usize)>,
    /// A self-delimited sub-packet rewritten in standard framing.
    packet: Vec<u8>,
}

impl OpusMSDecoder {
    /// A surround decoder for `channels` channels with the standard layout of
    /// `mapping_family` (see [`ChannelLayout::surround`]).
    ///
    /// # Errors
    ///
    /// [`Error::BadArg`] for an unsupported channel count / mapping family, or any
    /// error from creating the per-stream decoders.
    pub fn new(sample_rate: i32, channels: usize, mapping_family: i32) -> Result<Self, Error> {
        Self::with_layout(
            sample_rate,
            ChannelLayout::surround(channels, mapping_family)?,
        )
    }

    /// A decoder for an explicit channel layout — e.g. the one an `OpusHead`
    /// declares ([`ChannelLayout::from_opus_head`]), for mapping family 255 or
    /// a non-default family-1 table.
    ///
    /// # Errors
    ///
    /// [`Error::BadArg`] if the layout is invalid (see [`ChannelLayout::new`])
    /// or `sample_rate` is not an Opus rate.
    pub fn with_layout(sample_rate: i32, layout: ChannelLayout) -> Result<Self, Error> {
        layout.validate()?;
        let mut decoders = Vec::with_capacity(layout.nb_streams);
        for s in 0..layout.nb_streams {
            let ch = if s < layout.nb_coupled_streams { 2 } else { 1 };
            decoders.push(OpusDecoder::new(sample_rate, ch)?);
        }
        Ok(Self {
            layout,
            decoders,
            sample_rate,
            buf: Vec::new(),
            spans: Vec::new(),
            packet: Vec::new(),
        })
    }

    /// Decode a multistream packet into interleaved `output` (nb_channels per
    /// sample). Returns the number of samples per channel. `frame_size` is a
    /// capacity (as in libopus): the packet's own duration is decoded. An
    /// empty `packet` is a lost packet: every stream is concealed.
    ///
    /// Like libopus `opus_multistream_decode_float`, the whole packet is
    /// validated before any stream is decoded: every stream but the last uses
    /// the self-delimited framing (RFC 6716 appendix B), and all streams must
    /// carry the same duration.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidPacket`] if the multistream framing is malformed or the
    /// streams' durations differ; [`Error::BufferTooSmall`] if the packet is
    /// longer than `frame_size` or `output` cannot hold it; or any error from
    /// the per-stream [`crate::OpusDecoder::decode`] calls.
    pub fn decode(
        &mut self,
        packet: &[u8],
        frame_size: usize,
        output: &mut [f32],
    ) -> Result<usize, Error> {
        let nch = self.layout.nb_channels;
        let nb_streams = self.layout.nb_streams;

        // Split (and validate) first: `spans[s]` = (start, sd_field, next)
        // of stream s within `packet`.
        self.spans.clear();
        let mut samples = 0usize;
        if !packet.is_empty() {
            let mut off = 0usize;
            for s in 0..nb_streams {
                let rest = &packet[off..];
                if rest.is_empty() {
                    return Err(Error::InvalidPacket("multistream packet truncated"));
                }
                let last = s == nb_streams - 1;
                let f = parse_frames(rest, !last)?;
                let n = f.count * samples_per_frame(f.toc, self.sample_rate) as usize;
                if s > 0 && n != samples {
                    return Err(Error::InvalidPacket(
                        "multistream streams differ in duration",
                    ));
                }
                samples = n;
                let next = if last { rest.len() } else { f.next };
                self.spans.push((off, f.sd_field, next));
                off += next;
            }
            if samples > frame_size || output.len() < samples * nch {
                return Err(Error::BufferTooSmall("Output buffer too small"));
            }
        }

        let Self {
            layout,
            decoders,
            buf,
            spans,
            packet: standard,
            ..
        } = self;
        if buf.len() < frame_size * 2 {
            buf.resize(frame_size * 2, 0.0);
        }
        let mut produced = 0usize;
        for (s, dec) in decoders.iter_mut().enumerate() {
            let stream: &[u8] = match spans.get(s) {
                // Lost packet: conceal every stream.
                None => &[],
                Some(&(start, (sd_at, sd_len), next)) => {
                    let sub = &packet[start..start + next];
                    if sd_len == 0 {
                        sub
                    } else {
                        // Drop the self-delimiting length: the same frames in
                        // the standard framing `OpusDecoder::decode` parses.
                        standard.clear();
                        standard.extend_from_slice(&sub[..sd_at]);
                        standard.extend_from_slice(&sub[sd_at + sd_len..]);
                        standard
                    }
                }
            };
            let n = dec.decode(stream, frame_size, buf)?;
            if s == 0 {
                produced = n;
                if output.len() < n * nch {
                    return Err(Error::BufferTooSmall("Output buffer too small"));
                }
            } else if n != produced {
                return Err(Error::Internal(
                    "multistream streams decoded different lengths",
                ));
            }
            // Remux this stream's channel(s) to every output that takes them.
            let coupled = s < layout.nb_coupled_streams;
            for (c, &m) in layout.mapping.iter().enumerate() {
                let m = m as usize;
                let src = if coupled && m == 2 * s {
                    0
                } else if coupled && m == 2 * s + 1 {
                    1
                } else if !coupled && m == s + layout.nb_coupled_streams {
                    0
                } else {
                    continue;
                };
                let stride = if coupled { 2 } else { 1 };
                for i in 0..n {
                    output[i * nch + c] = buf[i * stride + src];
                }
            }
        }
        // Unmapped channels (mapping == 255) are silenced.
        for (c, &m) in layout.mapping.iter().enumerate() {
            if m == 255 {
                for i in 0..produced {
                    output[i * nch + c] = 0.0;
                }
            }
        }
        Ok(produced)
    }

    /// The channel layout this decoder outputs.
    pub fn layout(&self) -> &ChannelLayout {
        &self.layout
    }
}

impl OpusMSEncoder {
    /// Cap the audio bandwidth of every stream at once.
    pub fn set_max_bandwidth(&mut self, bw: Bandwidth) {
        for e in &mut self.encoders {
            e.max_bandwidth = bw;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn head(channels: u8, family: u8, table: &[u8]) -> Vec<u8> {
        let mut h = b"OpusHead\x01".to_vec();
        h.push(channels);
        h.extend_from_slice(&[0x38, 0x01, 0x80, 0xbb, 0, 0, 0, 0]);
        h.push(family);
        h.extend_from_slice(table);
        h
    }

    #[test]
    fn layout_from_opus_head() {
        let mono = ChannelLayout::from_opus_head(&head(1, 0, &[])).unwrap();
        assert_eq!(mono, ChannelLayout::surround(1, 0).unwrap());
        let five_one = ChannelLayout::from_opus_head(&head(6, 1, &[4, 2, 0, 4, 1, 2, 3, 5]));
        assert_eq!(five_one.unwrap(), ChannelLayout::surround(6, 1).unwrap());
        let custom = ChannelLayout::from_opus_head(&head(4, 255, &[2, 0, 1, 255, 0, 1])).unwrap();
        assert_eq!(
            custom,
            ChannelLayout::new(4, 2, 0, vec![1, 255, 0, 1]).unwrap()
        );
        // Family 255 allows any channel count, family 1 at most 8.
        let wide = head(10, 255, &[10, 0, 0, 1, 2, 3, 4, 5, 6, 7, 8, 9]);
        assert_eq!(
            ChannelLayout::from_opus_head(&wide).unwrap().nb_channels,
            10
        );
        let bad = head(10, 1, &[10, 0, 0, 1, 2, 3, 4, 5, 6, 7, 8, 9]);
        assert!(ChannelLayout::from_opus_head(&bad).is_err());
        // Truncated table, wrong magic, unknown major version, family 2.
        assert!(ChannelLayout::from_opus_head(&head(6, 1, &[4, 2, 0, 4, 1])).is_err());
        assert!(ChannelLayout::from_opus_head(b"OpusTags\x01\x02\0\0\0\0\0\0\0\0\0").is_err());
        let mut v16 = head(2, 0, &[]);
        v16[8] = 0x10;
        assert!(ChannelLayout::from_opus_head(&v16).is_err());
        assert!(ChannelLayout::from_opus_head(&head(4, 2, &[2, 2, 0, 1, 2, 3])).is_err());
        // Family 0 is mono/stereo only.
        assert!(ChannelLayout::from_opus_head(&head(3, 0, &[])).is_err());
    }

    #[test]
    fn layout_validation_follows_rfc_7845() {
        assert!(ChannelLayout::new(2, 1, 1, vec![0, 1]).is_ok());
        assert!(ChannelLayout::new(2, 1, 1, vec![255, 255]).is_ok());
        assert!(ChannelLayout::new(0, 1, 0, vec![]).is_err(), "no channels");
        assert!(ChannelLayout::new(1, 0, 0, vec![0]).is_err(), "no streams");
        assert!(ChannelLayout::new(2, 1, 2, vec![0, 1]).is_err(), "M > N");
        assert!(ChannelLayout::new(2, 1, 1, vec![0]).is_err(), "short map");
        assert!(
            ChannelLayout::new(2, 1, 1, vec![0, 2]).is_err(),
            "index >= N+M"
        );
        assert!(
            ChannelLayout::new(1, 200, 100, vec![0]).is_err(),
            "N+M > 255"
        );
        let mut layout = ChannelLayout::surround(2, 0).unwrap();
        layout.mapping[1] = 7;
        assert!(OpusMSDecoder::with_layout(48000, layout).is_err());
    }

    /// `n` 20 ms mono packets of a tone at 48 kHz.
    fn mono_packets(n: usize, freq: f32) -> Vec<Vec<u8>> {
        let mut enc = OpusEncoder::new(48000, 1, Application::Audio).unwrap();
        let mut buf = vec![0u8; 1500];
        (0..n)
            .map(|k| {
                let pcm: Vec<f32> = (0..960)
                    .map(|i| ((k * 960 + i) as f32 * freq).sin() * 0.3)
                    .collect();
                let len = enc.encode(&pcm, 960, &mut buf).unwrap();
                buf[..len].to_vec()
            })
            .collect()
    }

    /// Two mono streams decoded one by one: the reference for a 2-stream
    /// multistream decode.
    fn reference(a: &[Vec<u8>], b: &[Vec<u8>]) -> Vec<Vec<f32>> {
        let mut da = OpusDecoder::new(48000, 1).unwrap();
        let mut db = OpusDecoder::new(48000, 1).unwrap();
        let (mut oa, mut ob) = (vec![0f32; 960], vec![0f32; 960]);
        a.iter()
            .zip(b)
            .map(|(pa, pb)| {
                da.decode(pa, 960, &mut oa).unwrap();
                db.decode(pb, 960, &mut ob).unwrap();
                oa.iter().zip(&ob).flat_map(|(&x, &y)| [x, y]).collect()
            })
            .collect()
    }

    fn decode_ms(packets: &[Vec<u8>]) -> Vec<Vec<f32>> {
        let layout = ChannelLayout::new(2, 2, 0, vec![0, 1]).unwrap();
        let mut dec = OpusMSDecoder::with_layout(48000, layout).unwrap();
        let mut out = vec![0f32; 5760 * 2];
        packets
            .iter()
            .map(|p| {
                let n = dec.decode(p, 5760, &mut out).unwrap();
                out[..n * 2].to_vec()
            })
            .collect()
    }

    /// Code-0 `packet` as a self-delimited sub-packet, optionally with
    /// `pad` bytes of Opus padding (which forces code 3).
    fn self_delimited(packet: &[u8], pad: usize) -> Vec<u8> {
        assert_eq!(packet[0] & 3, 0);
        let payload = &packet[1..];
        let mut size = Vec::new();
        if payload.len() < 252 {
            size.push(payload.len() as u8);
        } else {
            let b0 = 252 + (payload.len() & 3);
            size.extend_from_slice(&[b0 as u8, ((payload.len() - b0) >> 2) as u8]);
        }
        let mut out = Vec::new();
        if pad == 0 {
            out.push(packet[0]);
            out.extend_from_slice(&size);
            out.extend_from_slice(payload);
        } else {
            assert!(pad < 255);
            out.extend_from_slice(&[packet[0] | 3, 0x41, pad as u8]);
            out.extend_from_slice(&size);
            out.extend_from_slice(payload);
            out.extend(std::iter::repeat_n(0u8, pad));
        }
        out
    }

    /// Every stream but the last is self-delimited; the decoder must strip
    /// that length (and skip any padding) before the stream's own decoder
    /// sees it. Before the fix the length byte was decoded as payload.
    #[test]
    fn self_delimited_streams_decode_like_single_streams() {
        let a = mono_packets(6, 0.031);
        let b = mono_packets(6, 0.077);
        let want = reference(&a, &b);
        for pad in [0, 9] {
            let packets: Vec<Vec<u8>> = a
                .iter()
                .zip(&b)
                .map(|(pa, pb)| {
                    let mut p = self_delimited(pa, pad);
                    p.extend_from_slice(pb);
                    p
                })
                .collect();
            assert_eq!(decode_ms(&packets), want, "pad {pad}");
        }
    }

    #[test]
    fn malformed_multistream_packets_are_rejected() {
        let a = mono_packets(1, 0.031);
        let layout = ChannelLayout::new(2, 2, 0, vec![0, 1]).unwrap();
        let mut dec = OpusMSDecoder::with_layout(48000, layout).unwrap();
        let mut out = vec![0f32; 5760 * 2];
        // One stream where two are declared.
        assert!(dec.decode(&a[0], 5760, &mut out).is_err());
        // Streams of different durations (20 ms + 10 ms TOC).
        let mut p = self_delimited(&a[0], 0);
        p.extend_from_slice(&[(a[0][0] & !0x18) | 0x10, 0xff]);
        assert!(matches!(
            dec.decode(&p, 5760, &mut out),
            Err(Error::InvalidPacket(_))
        ));
        // Output too small for the packet.
        let mut p = self_delimited(&a[0], 0);
        p.extend_from_slice(&a[0]);
        assert!(matches!(
            dec.decode(&p, 5760, &mut out[..100]),
            Err(Error::BufferTooSmall(_))
        ));
        // A lost packet conceals every stream.
        assert_eq!(dec.decode(&[], 960, &mut out).unwrap(), 960);
    }
}
