//! Port of libopus `src/repacketizer.c` + the packet helpers from `src/opus.c`:
//! split Opus packets into frames and recombine/re-frame/pad them WITHOUT
//! re-encoding. Used to merge several packets into a longer one, split a
//! multi-frame packet, or pad a packet to a target size (e.g. for CBR
//! transport). All frames in a repacketizer must share the same TOC config
//! (mode/bandwidth/frame-size); only the code (0..3) and framing change.

/// opus_packet_get_samples_per_frame(toc, Fs).
pub fn samples_per_frame(toc: u8, fs: i32) -> i32 {
    if toc & 0x80 != 0 {
        let a = ((toc >> 3) & 0x3) as i32;
        (fs << a) / 400
    } else if toc & 0x60 == 0x60 {
        if toc & 0x08 != 0 {
            fs / 50
        } else {
            fs / 100
        }
    } else {
        let a = ((toc >> 3) & 0x3) as i32;
        if a == 3 {
            fs * 60 / 1000
        } else {
            (fs << a) / 100
        }
    }
}

/// opus_packet_get_nb_frames.
pub fn nb_frames(packet: &[u8]) -> Result<i32, &'static str> {
    if packet.is_empty() {
        return Err("bad arg");
    }
    match packet[0] & 0x3 {
        0 => Ok(1),
        3 => {
            if packet.len() < 2 {
                Err("invalid packet")
            } else {
                Ok((packet[1] & 0x3f) as i32)
            }
        }
        _ => Ok(2),
    }
}

fn parse_size(data: &[u8]) -> (i32, i32) {
    // returns (bytes_consumed, size); size<0 => error
    if data.is_empty() {
        (-1, -1)
    } else if data[0] < 252 {
        (1, data[0] as i32)
    } else if data.len() < 2 {
        (-1, -1)
    } else {
        (2, data[1] as i32 * 4 + data[0] as i32)
    }
}

fn encode_size(size: i32, out: &mut Vec<u8>) {
    if size < 252 {
        out.push(size as u8);
    } else {
        let b0 = 252 + (size & 0x3);
        out.push(b0 as u8);
        out.push(((size - b0) >> 2) as u8);
    }
}

/// Split `data` into its frames. Returns (toc, frame byte-ranges, packet_offset).
/// `self_delimited` parses the trailing length prefix used by multistream.
#[allow(clippy::type_complexity)]
pub fn parse_packet(
    data: &[u8],
    self_delimited: bool,
) -> Result<(u8, Vec<(usize, usize)>, usize), &'static str> {
    if data.is_empty() {
        return Err("invalid packet");
    }
    let framesize = samples_per_frame(data[0], 48000);
    let toc = data[0];
    let mut pos = 1usize; // cursor into data
    let mut len = data.len() as i32 - 1;
    let mut cbr = false;
    let mut last_size = len;
    let mut sizes: Vec<i32> = Vec::new();

    let count: usize = match toc & 0x3 {
        0 => 1,
        1 => {
            cbr = true;
            if !self_delimited {
                if len & 1 != 0 {
                    return Err("invalid packet");
                }
                last_size = len / 2;
                sizes.push(last_size);
            }
            2
        }
        2 => {
            let (bytes, sz) = parse_size(&data[pos..]);
            if bytes < 0 {
                return Err("invalid packet");
            }
            len -= bytes;
            if sz < 0 || sz > len {
                return Err("invalid packet");
            }
            pos += bytes as usize;
            sizes.push(sz);
            last_size = len - sz;
            2
        }
        _ => {
            if len < 1 {
                return Err("invalid packet");
            }
            let ch = data[pos];
            pos += 1;
            len -= 1;
            let count = (ch & 0x3f) as usize;
            if count == 0 || framesize * count as i32 > 5760 {
                return Err("invalid packet");
            }
            if ch & 0x40 != 0 {
                // padding
                loop {
                    if len <= 0 {
                        return Err("invalid packet");
                    }
                    let p = data[pos];
                    pos += 1;
                    len -= 1;
                    let tmp = if p == 255 { 254 } else { p as i32 };
                    len -= tmp;
                    if p != 255 {
                        break;
                    }
                }
            }
            if len < 0 {
                return Err("invalid packet");
            }
            cbr = ch & 0x80 == 0;
            if !cbr {
                last_size = len;
                for _ in 0..count - 1 {
                    let (bytes, sz) = parse_size(&data[pos..]);
                    if bytes < 0 {
                        return Err("invalid packet");
                    }
                    len -= bytes;
                    if sz < 0 || sz > len {
                        return Err("invalid packet");
                    }
                    pos += bytes as usize;
                    sizes.push(sz);
                    last_size -= bytes + sz;
                }
                if last_size < 0 {
                    return Err("invalid packet");
                }
            } else if !self_delimited {
                last_size = len / count as i32;
                if last_size * count as i32 != len {
                    return Err("invalid packet");
                }
                for _ in 0..count - 1 {
                    sizes.push(last_size);
                }
            }
            count
        }
    };

    if self_delimited {
        let (bytes, sz) = parse_size(&data[pos..]);
        if bytes < 0 {
            return Err("invalid packet");
        }
        len -= bytes;
        if sz < 0 || sz > len {
            return Err("invalid packet");
        }
        pos += bytes as usize;
        if cbr {
            if sz * count as i32 > len {
                return Err("invalid packet");
            }
            sizes.clear();
            for _ in 0..count - 1 {
                sizes.push(sz);
            }
            sizes.push(sz);
        } else {
            if bytes + sz > last_size {
                return Err("invalid packet");
            }
            sizes.push(sz);
        }
    } else {
        if last_size > 1275 {
            return Err("invalid packet");
        }
        sizes.push(last_size);
    }

    // Frame byte-ranges start at `pos`.
    let mut frames = Vec::with_capacity(count);
    let mut off = pos;
    for &s in &sizes {
        if off + s as usize > data.len() {
            return Err("invalid packet");
        }
        frames.push((off, s as usize));
        off += s as usize;
    }
    let packet_offset = off; // for self-delimited multistream advancement
    Ok((toc, frames, packet_offset))
}

/// opus_repacketizer: accumulate frames from one or more same-config packets,
/// then emit them as a single re-framed packet.
#[derive(Default)]
pub struct Repacketizer {
    toc: u8,
    framesize: i32,
    frames: Vec<Vec<u8>>,
}

impl Repacketizer {
    pub fn new() -> Self {
        Repacketizer::default()
    }

    pub fn nb_frames(&self) -> usize {
        self.frames.len()
    }

    /// Append the frames of `data` (opus_repacketizer_cat). Errors if the TOC
    /// config differs from frames already held, or the 120 ms cap is exceeded.
    pub fn cat(&mut self, data: &[u8]) -> Result<(), &'static str> {
        self.cat_impl(data, false)
    }

    fn cat_impl(&mut self, data: &[u8], self_delimited: bool) -> Result<(), &'static str> {
        if data.is_empty() {
            return Err("invalid packet");
        }
        if self.frames.is_empty() {
            self.toc = data[0];
            self.framesize = samples_per_frame(data[0], 8000);
        } else if self.toc & 0xfc != data[0] & 0xfc {
            return Err("toc mismatch");
        }
        let curr = nb_frames(data)?;
        if curr < 1 {
            return Err("invalid packet");
        }
        if (curr as usize + self.frames.len()) as i32 * self.framesize > 960 {
            return Err("packet exceeds 120 ms");
        }
        let (_toc, ranges, _off) = parse_packet(data, self_delimited)?;
        for (o, l) in ranges {
            self.frames.push(data[o..o + l].to_vec());
        }
        Ok(())
    }

    /// Emit frames [begin, end) as one packet (opus_repacketizer_out_range).
    pub fn out_range(&self, begin: usize, end: usize) -> Result<Vec<u8>, &'static str> {
        self.out_range_impl(begin, end, None)
    }

    /// Emit all held frames (opus_repacketizer_out).
    pub fn out(&self) -> Result<Vec<u8>, &'static str> {
        self.out_range_impl(0, self.frames.len(), None)
    }

    fn out_range_impl(
        &self,
        begin: usize,
        end: usize,
        pad_to: Option<usize>,
    ) -> Result<Vec<u8>, &'static str> {
        self.out_range_full(begin, end, pad_to, false)
    }

    /// Emit all frames with the self-delimited framing multistream uses (the
    /// last frame's length is coded so the packet's total size is derivable).
    pub fn out_self_delimited(&self) -> Result<Vec<u8>, &'static str> {
        self.out_range_full(0, self.frames.len(), None, true)
    }

    fn out_range_full(
        &self,
        begin: usize,
        end: usize,
        pad_to: Option<usize>,
        self_delimited: bool,
    ) -> Result<Vec<u8>, &'static str> {
        if begin >= end || end > self.frames.len() {
            return Err("bad arg");
        }
        let count = end - begin;
        let lens: Vec<usize> = self.frames[begin..end].iter().map(|f| f.len()).collect();
        let mut out: Vec<u8> = Vec::new();

        if count == 1 {
            out.push(self.toc & 0xfc); // code 0
        } else if count == 2 && lens[0] == lens[1] {
            out.push((self.toc & 0xfc) | 0x1); // code 1
        } else if count == 2 {
            out.push((self.toc & 0xfc) | 0x2); // code 2
            encode_size(lens[0] as i32, &mut out);
        }

        let want_pad = pad_to.is_some();
        if count > 2 || (want_pad && count <= 2) {
            // Code 3 (needed for >2 frames, or to carry padding).
            out.clear();
            let vbr = lens.iter().any(|&l| l != lens[0]);
            if vbr {
                out.push((self.toc & 0xfc) | 0x3);
                out.push((count as u8) | 0x80);
            } else {
                out.push((self.toc & 0xfc) | 0x3);
                out.push(count as u8);
            }
            // Compute current size to know the padding amount.
            let mut tot = 2usize;
            if vbr {
                for &l in lens.iter().take(count - 1) {
                    tot += 1 + usize::from(l >= 252) + l;
                }
                tot += lens[count - 1];
            } else {
                tot += count * lens[0];
            }
            let pad_amount = pad_to.map(|n| n.saturating_sub(tot)).unwrap_or(0);
            if pad_amount != 0 {
                out[1] |= 0x40; // padding flag
                let nb_255s = (pad_amount - 1) / 255;
                for _ in 0..nb_255s {
                    out.push(255);
                }
                out.push((pad_amount - 255 * nb_255s - 1) as u8);
            }
            if vbr {
                for &l in lens.iter().take(count - 1) {
                    encode_size(l as i32, &mut out);
                }
            }
            if self_delimited {
                encode_size(lens[count - 1] as i32, &mut out);
            }
            for f in &self.frames[begin..end] {
                out.extend_from_slice(f);
            }
            if let Some(n) = pad_to {
                while out.len() < n {
                    out.push(0);
                }
            }
            return Ok(out);
        }

        if self_delimited {
            encode_size(lens[count - 1] as i32, &mut out);
        }
        for f in &self.frames[begin..end] {
            out.extend_from_slice(f);
        }
        Ok(out)
    }
}

/// opus_packet_pad: grow `packet` in place to `new_len` bytes by adding opus
/// padding (no re-encode). No-op if already `new_len`; errors if `new_len` is
/// smaller.
pub fn pad_packet(packet: &mut Vec<u8>, new_len: usize) -> Result<(), &'static str> {
    if packet.is_empty() {
        return Err("bad arg");
    }
    if packet.len() == new_len {
        return Ok(());
    }
    if packet.len() > new_len {
        return Err("bad arg");
    }
    let mut rp = Repacketizer::new();
    rp.cat(packet)?;
    let padded = rp.out_range_impl(0, rp.nb_frames(), Some(new_len))?;
    *packet = padded;
    Ok(())
}

/// opus_packet_unpad: strip opus padding, returning the minimal packet.
pub fn unpad_packet(packet: &[u8]) -> Result<Vec<u8>, &'static str> {
    if packet.is_empty() {
        return Err("bad arg");
    }
    let mut rp = Repacketizer::new();
    rp.cat(packet)?;
    rp.out_range_impl(0, rp.nb_frames(), None)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Build a synthetic code-3 VBR packet with 3 frames of distinct lengths,
    // split via out_range, and re-merge -> byte-identical (round-trip fidelity).
    #[test]
    fn split_merge_roundtrip() {
        // toc config 12 (hybrid SWB 10ms) stereo bit off, code 3.
        let toc = 12u8 << 3;
        let mut pkt = vec![toc | 0x3, 3 | 0x80]; // code 3, vbr, count 3
        let f0 = vec![0xAAu8; 3];
        let f1 = vec![0xBBu8; 5];
        let f2 = vec![0xCCu8; 4];
        encode_size(3, &mut pkt);
        encode_size(5, &mut pkt);
        pkt.extend_from_slice(&f0);
        pkt.extend_from_slice(&f1);
        pkt.extend_from_slice(&f2);

        let mut rp = Repacketizer::new();
        rp.cat(&pkt).unwrap();
        assert_eq!(rp.nb_frames(), 3);
        // out() must reproduce the exact same packet.
        assert_eq!(rp.out().unwrap(), pkt);
        // Splitting single frames yields code-0 packets with the frame bytes.
        let s0 = rp.out_range(0, 1).unwrap();
        assert_eq!(s0[0] & 0x3, 0);
        assert_eq!(&s0[1..], &f0[..]);
        let s1 = rp.out_range(1, 2).unwrap();
        assert_eq!(&s1[1..], &f1[..]);
    }

    #[test]
    fn pad_unpad_identity() {
        let toc = 8u8 << 3; // silk WB code 0
        let mut pkt = vec![toc];
        pkt.extend_from_slice(&[1, 2, 3, 4, 5]);
        let orig = pkt.clone();
        pad_packet(&mut pkt, orig.len() + 10).unwrap();
        assert_eq!(pkt.len(), orig.len() + 10);
        let back = unpad_packet(&pkt).unwrap();
        // frame bytes recovered
        let (_t, f, _) = parse_packet(&back, false).unwrap();
        assert_eq!(&back[f[0].0..f[0].0 + f[0].1], &orig[1..]);
    }

    #[test]
    fn cbr_merge_code1() {
        // Two equal-length frames merge to code 1.
        let toc = 8u8 << 3;
        let p = vec![toc, 9, 9, 9]; // code 0, 3-byte frame
        let mut rp = Repacketizer::new();
        rp.cat(&p).unwrap();
        rp.cat(&p).unwrap();
        let out = rp.out().unwrap();
        assert_eq!(out[0] & 0x3, 1); // code 1 (equal sizes)
        assert_eq!(rp.nb_frames(), 2);
    }
}

#[cfg(test)]
mod sd_tests {
    use super::*;
    #[test]
    fn self_delimited_roundtrip() {
        // 3-frame vbr packet -> self-delimited -> parse(self_delimited) recovers frames.
        let toc = 12u8 << 3;
        let mut rp = Repacketizer::new();
        let mut p = vec![toc | 0x3, 3 | 0x80];
        encode_size(3, &mut p); encode_size(5, &mut p);
        p.extend_from_slice(&[1u8;3]); p.extend_from_slice(&[2u8;5]); p.extend_from_slice(&[3u8;4]);
        rp.cat(&p).unwrap();
        let sd = rp.out_self_delimited().unwrap();
        // append trailing bytes to simulate concatenation; parse must stop at packet_offset
        let mut stream = sd.clone(); stream.extend_from_slice(&[0xEE;7]);
        let (t, frames, off) = parse_packet(&stream, true).unwrap();
        assert_eq!(t, toc | 0x3);
        assert_eq!(frames.len(), 3);
        assert_eq!(&stream[frames[0].0..frames[0].0+frames[0].1], &[1,1,1]);
        assert_eq!(&stream[frames[2].0..frames[2].0+frames[2].1], &[3,3,3,3]);
        assert_eq!(off, sd.len()); // packet ends exactly at the SD boundary
    }
}
