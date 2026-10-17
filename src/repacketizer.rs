//! Port of libopus `src/repacketizer.c` + the packet helpers from `src/opus.c`:
//! split Opus packets into frames and recombine/re-frame/pad them WITHOUT
//! re-encoding. Used to merge several packets into a longer one, split a
//! multi-frame packet, or pad a packet to a target size (e.g. for CBR
//! transport). All frames in a repacketizer must share the same TOC config
//! (mode/bandwidth/frame-size); only the code (0..3) and framing change.

use crate::Error;
/// opus_packet_get_samples_per_frame(toc, Fs).
pub fn samples_per_frame(toc: u8, fs: i32) -> i32 {
    if toc & 0x80 != 0 {
        let a = ((toc >> 3) & 0x3) as i32;
        (fs << a) / 400
    } else if toc & 0x60 == 0x60 {
        if toc & 0x08 != 0 { fs / 50 } else { fs / 100 }
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
///
/// # Errors
///
/// [`Error::BadArg`] for an empty packet; [`Error::InvalidPacket`] if the
/// frame-count byte of a code-3 packet is missing.
pub fn nb_frames(packet: &[u8]) -> Result<i32, Error> {
    if packet.is_empty() {
        return Err(Error::BadArg("bad arg"));
    }
    match packet[0] & 0x3 {
        0 => Ok(1),
        3 => {
            if packet.len() < 2 {
                Err(Error::InvalidPacket("invalid packet"))
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

#[cfg(test)]
fn encode_size(size: i32, out: &mut Vec<u8>) {
    if size < 252 {
        out.push(size as u8);
    } else {
        let b0 = 252 + (size & 0x3);
        out.push(b0 as u8);
        out.push(((size - b0) >> 2) as u8);
    }
}

/// Most frames one packet can carry: 120 ms of 2.5 ms frames (RFC 6716 3.2.5).
const MAX_FRAMES: usize = 48;

const TOO_SMALL: Error = Error::BufferTooSmall("output buffer too small for packet");

/// Bounds-checked writer over a caller's buffer.
struct Cursor<'a> {
    buf: &'a mut [u8],
    pos: usize,
}

impl Cursor<'_> {
    fn put(&mut self, b: u8) -> Result<(), Error> {
        *self.buf.get_mut(self.pos).ok_or(TOO_SMALL)? = b;
        self.pos += 1;
        Ok(())
    }

    fn put_all(&mut self, src: &[u8]) -> Result<(), Error> {
        let end = self.pos + src.len();
        self.buf
            .get_mut(self.pos..end)
            .ok_or(TOO_SMALL)?
            .copy_from_slice(src);
        self.pos = end;
        Ok(())
    }

    /// One- or two-byte frame length (RFC 6716 3.2.1).
    fn put_size(&mut self, size: usize) -> Result<(), Error> {
        if size < 252 {
            self.put(size as u8)
        } else {
            let b0 = 252 + (size & 0x3);
            self.put(b0 as u8)?;
            self.put(((size - b0) >> 2) as u8)
        }
    }
}

/// Fixed-capacity list of frame sizes (parsing allocates nothing).
#[derive(Clone, Copy)]
struct SizeList {
    v: [i32; MAX_FRAMES],
    n: usize,
}

impl SizeList {
    const fn new() -> Self {
        Self {
            v: [0; MAX_FRAMES],
            n: 0,
        }
    }

    fn push(&mut self, size: i32) -> Result<(), Error> {
        *self
            .v
            .get_mut(self.n)
            .ok_or(Error::InvalidPacket("invalid packet"))? = size;
        self.n += 1;
        Ok(())
    }

    fn clear(&mut self) {
        self.n = 0;
    }

    fn as_slice(&self) -> &[i32] {
        &self.v[..self.n]
    }
}

/// A parsed packet: TOC, frame byte-ranges, and where the packet ends.
pub(crate) struct Frames {
    pub(crate) toc: u8,
    pub(crate) count: usize,
    pub(crate) ranges: [(usize, usize); MAX_FRAMES],
    pub(crate) end: usize,
}

impl Frames {
    pub(crate) fn ranges(&self) -> &[(usize, usize)] {
        &self.ranges[..self.count]
    }
}

/// Split `data` into its frames. Returns (toc, frame byte-ranges, packet_offset).
/// `self_delimited` parses the trailing length prefix used by multistream.
#[allow(clippy::type_complexity)]
///
/// # Errors
///
/// [`Error::InvalidPacket`] if the packet violates RFC 6716 §3 framing
/// (truncated lengths, frame sizes beyond the payload, bad padding).
pub fn parse_packet(
    data: &[u8],
    self_delimited: bool,
) -> Result<(u8, Vec<(usize, usize)>, usize), Error> {
    let f = parse_frames(data, self_delimited)?;
    Ok((f.toc, f.ranges().to_vec(), f.end))
}

/// Allocation-free [`parse_packet`]: the frame table lives on the stack.
pub(crate) fn parse_frames(data: &[u8], self_delimited: bool) -> Result<Frames, Error> {
    if data.is_empty() {
        return Err(Error::InvalidPacket("invalid packet"));
    }
    let framesize = samples_per_frame(data[0], 48000);
    let toc = data[0];
    let mut pos = 1usize; // cursor into data
    let mut len = data.len() as i32 - 1;
    let mut cbr = false;
    let mut last_size = len;
    let mut sizes = SizeList::new();

    let count: usize = match toc & 0x3 {
        0 => 1,
        1 => {
            cbr = true;
            if !self_delimited {
                if len & 1 != 0 {
                    return Err(Error::InvalidPacket("invalid packet"));
                }
                last_size = len / 2;
                sizes.push(last_size)?;
            }
            2
        }
        2 => {
            let (bytes, sz) = parse_size(&data[pos..]);
            if bytes < 0 {
                return Err(Error::InvalidPacket("invalid packet"));
            }
            len -= bytes;
            if sz < 0 || sz > len {
                return Err(Error::InvalidPacket("invalid packet"));
            }
            pos += bytes as usize;
            sizes.push(sz)?;
            last_size = len - sz;
            2
        }
        _ => {
            if len < 1 {
                return Err(Error::InvalidPacket("invalid packet"));
            }
            let ch = data[pos];
            pos += 1;
            len -= 1;
            let count = (ch & 0x3f) as usize;
            if count == 0 || framesize * count as i32 > 5760 {
                return Err(Error::InvalidPacket("invalid packet"));
            }
            if ch & 0x40 != 0 {
                // padding
                loop {
                    if len <= 0 {
                        return Err(Error::InvalidPacket("invalid packet"));
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
                return Err(Error::InvalidPacket("invalid packet"));
            }
            cbr = ch & 0x80 == 0;
            if !cbr {
                last_size = len;
                for _ in 0..count - 1 {
                    let (bytes, sz) = parse_size(&data[pos..]);
                    if bytes < 0 {
                        return Err(Error::InvalidPacket("invalid packet"));
                    }
                    len -= bytes;
                    if sz < 0 || sz > len {
                        return Err(Error::InvalidPacket("invalid packet"));
                    }
                    pos += bytes as usize;
                    sizes.push(sz)?;
                    last_size -= bytes + sz;
                }
                if last_size < 0 {
                    return Err(Error::InvalidPacket("invalid packet"));
                }
            } else if !self_delimited {
                last_size = len / count as i32;
                if last_size * count as i32 != len {
                    return Err(Error::InvalidPacket("invalid packet"));
                }
                for _ in 0..count - 1 {
                    sizes.push(last_size)?;
                }
            }
            count
        }
    };

    if self_delimited {
        let (bytes, sz) = parse_size(&data[pos..]);
        if bytes < 0 {
            return Err(Error::InvalidPacket("invalid packet"));
        }
        len -= bytes;
        if sz < 0 || sz > len {
            return Err(Error::InvalidPacket("invalid packet"));
        }
        pos += bytes as usize;
        if cbr {
            if sz * count as i32 > len {
                return Err(Error::InvalidPacket("invalid packet"));
            }
            sizes.clear();
            for _ in 0..count - 1 {
                sizes.push(sz)?;
            }
            sizes.push(sz)?;
        } else {
            if bytes + sz > last_size {
                return Err(Error::InvalidPacket("invalid packet"));
            }
            sizes.push(sz)?;
        }
    } else {
        if last_size > 1275 {
            return Err(Error::InvalidPacket("invalid packet"));
        }
        sizes.push(last_size)?;
    }

    // Frame byte-ranges start at `pos`.
    let mut ranges = [(0usize, 0usize); MAX_FRAMES];
    let mut off = pos;
    for (slot, &s) in ranges.iter_mut().zip(sizes.as_slice()) {
        if off + s as usize > data.len() {
            return Err(Error::InvalidPacket("invalid packet"));
        }
        *slot = (off, s as usize);
        off += s as usize;
    }
    // `end` is where a self-delimited multistream packet's next stream begins.
    Ok(Frames {
        toc,
        count: sizes.as_slice().len(),
        ranges,
        end: off,
    })
}

/// opus_repacketizer: accumulate frames from one or more same-config packets,
/// then emit them as a single re-framed packet.
///
/// Frame payloads are held back to back in one buffer. [`Repacketizer::reset`]
/// keeps its capacity, so a reused repacketizer that writes with
/// [`Repacketizer::out_into`] performs no allocation once warmed up.
#[derive(Default)]
pub struct Repacketizer {
    toc: u8,
    framesize: i32,
    data: Vec<u8>,
    /// `(start, len)` of each frame in `data`.
    frames: Vec<(usize, usize)>,
}

impl Repacketizer {
    /// An empty repacketizer.
    pub fn new() -> Self {
        Self::default()
    }

    /// Drop all held frames, keeping allocated capacity (opus_repacketizer_init).
    pub fn reset(&mut self) {
        self.data.clear();
        self.frames.clear();
    }

    /// Number of frames added so far.
    pub fn nb_frames(&self) -> usize {
        self.frames.len()
    }

    fn frame(&self, i: usize) -> &[u8] {
        let (start, len) = self.frames[i];
        &self.data[start..start + len]
    }

    /// Append the frames of `data` (opus_repacketizer_cat). Errors if the TOC
    /// config differs from frames already held, or the 120 ms cap is exceeded.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidPacket`] if `data` is malformed, its TOC configuration
    /// differs from the packets already added, or the total would exceed 120 ms.
    pub fn cat(&mut self, data: &[u8]) -> Result<(), Error> {
        self.cat_impl(data, false)
    }

    fn cat_impl(&mut self, data: &[u8], self_delimited: bool) -> Result<(), Error> {
        if data.is_empty() {
            return Err(Error::InvalidPacket("invalid packet"));
        }
        if self.frames.is_empty() {
            self.toc = data[0];
            self.framesize = samples_per_frame(data[0], 8000);
        } else if self.toc & 0xfc != data[0] & 0xfc {
            return Err(Error::InvalidPacket("toc mismatch"));
        }
        let curr = nb_frames(data)?;
        if curr < 1 {
            return Err(Error::InvalidPacket("invalid packet"));
        }
        if (curr as usize + self.frames.len()) as i32 * self.framesize > 960 {
            return Err(Error::InvalidPacket("packet exceeds 120 ms"));
        }
        let parsed = parse_frames(data, self_delimited)?;
        for &(o, l) in parsed.ranges() {
            self.frames.push((self.data.len(), l));
            self.data.extend_from_slice(&data[o..o + l]);
        }
        Ok(())
    }

    /// Emit frames [begin, end) as one packet (opus_repacketizer_out_range).
    ///
    /// # Errors
    ///
    /// [`Error::BadArg`] if `begin..end` is empty or outside the stored frames.
    pub fn out_range(&self, begin: usize, end: usize) -> Result<Vec<u8>, Error> {
        self.out_vec(begin, end, None, false)
    }

    /// Emit all held frames (opus_repacketizer_out).
    ///
    /// # Errors
    ///
    /// [`Error::BadArg`] if no frames have been added.
    pub fn out(&self) -> Result<Vec<u8>, Error> {
        self.out_vec(0, self.frames.len(), None, false)
    }

    /// Emit all held frames into `out`, returning the packet length
    /// (opus_repacketizer_out with a caller buffer). Allocation-free.
    ///
    /// # Errors
    ///
    /// [`Error::BadArg`] if no frames have been added;
    /// [`Error::BufferTooSmall`] if the packet does not fit in `out`.
    pub fn out_into(&self, out: &mut [u8]) -> Result<usize, Error> {
        self.write_range(0, self.frames.len(), None, false, out)
    }

    /// Emit frames [begin, end) into `out`, returning the packet length.
    /// Allocation-free.
    ///
    /// # Errors
    ///
    /// [`Error::BadArg`] if `begin..end` is empty or outside the stored frames;
    /// [`Error::BufferTooSmall`] if the packet does not fit in `out`.
    pub fn out_range_into(&self, begin: usize, end: usize, out: &mut [u8]) -> Result<usize, Error> {
        self.write_range(begin, end, None, false, out)
    }

    /// Emit all held frames padded to `pad_to` bytes (when larger than the
    /// unpadded packet) into `out`. Allocation-free.
    pub(crate) fn out_padded_into(&self, pad_to: usize, out: &mut [u8]) -> Result<usize, Error> {
        self.write_range(0, self.frames.len(), Some(pad_to), false, out)
    }

    /// Emit all frames with the self-delimited framing multistream uses (the
    /// last frame's length is coded so the packet's total size is derivable).
    ///
    /// # Errors
    ///
    /// [`Error::BadArg`] if no frames have been added.
    pub fn out_self_delimited(&self) -> Result<Vec<u8>, Error> {
        self.out_vec(0, self.frames.len(), None, true)
    }

    /// `Vec` front end to [`Repacketizer::write_range`], sized to an upper bound.
    fn out_vec(
        &self,
        begin: usize,
        end: usize,
        pad_to: Option<usize>,
        self_delimited: bool,
    ) -> Result<Vec<u8>, Error> {
        if begin >= end || end > self.frames.len() {
            return Err(Error::BadArg("bad arg"));
        }
        let payload: usize = self.frames[begin..end].iter().map(|&(_, l)| l).sum();
        // TOC + count byte, two bytes per length field (plus the self-delimited
        // one), the payload, and any padding with its length bytes.
        let bound = 2 + 2 * (end - begin + 1) + payload + pad_to.map_or(0, |n| n + n / 255 + 1);
        let mut out = vec![0u8; bound];
        let n = self.write_range(begin, end, pad_to, self_delimited, &mut out)?;
        out.truncate(n);
        Ok(out)
    }

    /// The one packet writer behind every output method.
    fn write_range(
        &self,
        begin: usize,
        end: usize,
        pad_to: Option<usize>,
        self_delimited: bool,
        out: &mut [u8],
    ) -> Result<usize, Error> {
        if begin >= end || end > self.frames.len() {
            return Err(Error::BadArg("bad arg"));
        }
        let count = end - begin;
        let lens = &self.frames[begin..end];
        let len = |i: usize| lens[i].1;
        let mut w = Cursor { buf: out, pos: 0 };

        if count > 2 || pad_to.is_some() {
            // Code 3 (needed for >2 frames, or to carry padding).
            let vbr = lens.iter().any(|&(_, l)| l != len(0));
            w.put((self.toc & 0xfc) | 0x3)?;
            w.put(count as u8 | if vbr { 0x80 } else { 0 })?;
            // Current size, to know the padding amount.
            let mut tot = 2usize;
            if vbr {
                for &(_, l) in &lens[..count - 1] {
                    tot += 1 + usize::from(l >= 252) + l;
                }
                tot += len(count - 1);
            } else {
                tot += count * len(0);
            }
            let pad_amount = pad_to.map_or(0, |n| n.saturating_sub(tot));
            if pad_amount != 0 {
                w.buf[1] |= 0x40; // padding flag
                let nb_255s = (pad_amount - 1) / 255;
                for _ in 0..nb_255s {
                    w.put(255)?;
                }
                w.put((pad_amount - 255 * nb_255s - 1) as u8)?;
            }
            if vbr {
                for &(_, l) in &lens[..count - 1] {
                    w.put_size(l)?;
                }
            }
        } else if count == 1 {
            w.put(self.toc & 0xfc)?; // code 0
        } else if len(0) == len(1) {
            w.put((self.toc & 0xfc) | 0x1)?; // code 1
        } else {
            w.put((self.toc & 0xfc) | 0x2)?; // code 2
            w.put_size(len(0))?;
        }
        if self_delimited {
            w.put_size(len(count - 1))?;
        }
        for i in begin..end {
            w.put_all(self.frame(i))?;
        }
        if let Some(n) = pad_to {
            while w.pos < n {
                w.put(0)?;
            }
        }
        Ok(w.pos)
    }
}

/// opus_packet_pad: grow `packet` in place to `new_len` bytes by adding opus
/// padding (no re-encode). No-op if already `new_len`; errors if `new_len` is
/// smaller.
///
/// # Errors
///
/// [`Error::BadArg`] if `packet` is empty or `new_len` is smaller than it;
/// [`Error::InvalidPacket`] if `packet` cannot be parsed.
pub fn pad_packet(packet: &mut Vec<u8>, new_len: usize) -> Result<(), Error> {
    if packet.is_empty() {
        return Err(Error::BadArg("bad arg"));
    }
    if packet.len() == new_len {
        return Ok(());
    }
    if packet.len() > new_len {
        return Err(Error::BadArg("bad arg"));
    }
    let mut rp = Repacketizer::new();
    rp.cat(packet)?;
    let padded = rp.out_vec(0, rp.nb_frames(), Some(new_len), false)?;
    *packet = padded;
    Ok(())
}

/// opus_packet_unpad: strip opus padding, returning the minimal packet.
///
/// # Errors
///
/// [`Error::BadArg`] for an empty packet; [`Error::InvalidPacket`] if it
/// cannot be parsed.
pub fn unpad_packet(packet: &[u8]) -> Result<Vec<u8>, Error> {
    if packet.is_empty() {
        return Err(Error::BadArg("bad arg"));
    }
    let mut rp = Repacketizer::new();
    rp.cat(packet)?;
    rp.out()
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
        encode_size(3, &mut p);
        encode_size(5, &mut p);
        p.extend_from_slice(&[1u8; 3]);
        p.extend_from_slice(&[2u8; 5]);
        p.extend_from_slice(&[3u8; 4]);
        rp.cat(&p).unwrap();
        let sd = rp.out_self_delimited().unwrap();
        // append trailing bytes to simulate concatenation; parse must stop at packet_offset
        let mut stream = sd.clone();
        stream.extend_from_slice(&[0xEE; 7]);
        let (t, frames, off) = parse_packet(&stream, true).unwrap();
        assert_eq!(t, toc | 0x3);
        assert_eq!(frames.len(), 3);
        assert_eq!(&stream[frames[0].0..frames[0].0 + frames[0].1], &[1, 1, 1]);
        assert_eq!(
            &stream[frames[2].0..frames[2].0 + frames[2].1],
            &[3, 3, 3, 3]
        );
        assert_eq!(off, sd.len()); // packet ends exactly at the SD boundary
    }
}
