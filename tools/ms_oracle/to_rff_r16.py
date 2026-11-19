"""libopus .ref -> the PCM rff must output for <name>.opus: pre-skip dropped,
cut at the final granule (pre_skip + input_len), channels in FFmpeg order
(family 1 reordered from Vorbis order). Written as .r16 (see to_r16.py)."""
import struct, sys, os
msp_path, ref_path, dst, step = sys.argv[1], sys.argv[2], sys.argv[3], int(sys.argv[4])
m = open(msp_path, 'rb').read()
ch, fam = m[8], m[11]
at = 12 + ch
preskip, input_len = struct.unpack('<HI', m[at:at+6])
ORDER = {1: [0], 2: [0, 1], 3: [0, 2, 1], 4: [0, 1, 2, 3], 5: [0, 2, 1, 3, 4],
         6: [0, 2, 1, 5, 3, 4], 7: [0, 2, 1, 6, 5, 3, 4], 8: [0, 2, 1, 7, 5, 6, 3, 4]}
order = ORDER[ch] if fam == 1 else list(range(ch))
raw = open(ref_path, 'rb').read()
f = struct.unpack('<%df' % (len(raw)//4), raw)
frames = input_len
pcm = []
for i in range(preskip, preskip + frames):
    for c in range(ch):
        pcm.append(f[i*ch + order[c]])
rawt = struct.pack('<%df' % len(pcm), *pcm)
h = 0xcbf29ce484222325
for b in rawt:
    h ^= b
    h = (h * 0x100000001b3) & 0xFFFFFFFFFFFFFFFF
out = bytearray(b'R16\0' + struct.pack('<IQIB', step, h, frames, ch))
for i in range(0, frames, step):
    for c in range(ch):
        v = round(pcm[i*ch + c] * 32768.0)
        out += struct.pack('<h', max(-32768, min(32767, v)))
open(dst, 'wb').write(out)
print(os.path.basename(dst), len(out))
