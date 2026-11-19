"""Shrink a libopus .ref (f32 PCM) into a committable .r16:
   "R16\0" u32 step  u64 fnv1a64(all f32 bytes)  u32 n_frames  u8 channels
   then every `step`-th frame (all channels) as i16 = round(x*32768) clamped.
"""
import struct, sys, os
src, dst, ch, step = sys.argv[1], sys.argv[2], int(sys.argv[3]), int(sys.argv[4])
raw = open(src, 'rb').read()
h = 0xcbf29ce484222325
for b in raw:
    h ^= b
    h = (h * 0x100000001b3) & 0xFFFFFFFFFFFFFFFF
f = struct.unpack('<%df' % (len(raw)//4), raw)
frames = len(f) // ch
out = bytearray(b'R16\0' + struct.pack('<IQIB', step, h, frames, ch))
for i in range(0, frames, step):
    for c in range(ch):
        v = round(f[i*ch + c] * 32768.0)
        out += struct.pack('<h', max(-32768, min(32767, v)))
open(dst, 'wb').write(out)
print(os.path.basename(dst), len(out))
