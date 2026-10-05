#!/usr/bin/env python3
"""Regenerate fuzz/seeds/: real Opus packets (every mode, bandwidth, frame size,
mono/stereo, CBR/VBR, single- and multi-frame) for the decoder-side targets, and
short PCM excerpts for the encoder-side targets.

    python fuzz/make_seeds.py <dir with encode_bit[.exe]> <dir with in_<rate>_<ch>.sw>
"""
import os, struct, subprocess, sys

HERE = os.path.dirname(os.path.abspath(__file__))
EX, PCM = sys.argv[1], sys.argv[2]
ENC = os.path.join(EX, 'encode_bit.exe' if os.name == 'nt' else 'encode_bit')

CONFIGS = [  # rate, ch, kbps, frame_ms, app, vbr
    (8000, 1, 12, '20', 'voip', 1), (12000, 1, 16, '20', 'voip', 1), (16000, 1, 20, '20', 'voip', 0),
    (16000, 1, 16, '60', 'voip', 1), (24000, 1, 32, '20', 'audio', 1), (48000, 1, 24, '10', 'audio', 1),
    (48000, 1, 64, '20', 'audio', 1), (48000, 2, 128, '20', 'audio', 1), (48000, 2, 96, '2.5', 'lowdelay', 0),
    (48000, 2, 64, '5', 'lowdelay', 1), (48000, 2, 48, '40', 'audio', 1), (48000, 1, 32, '120', 'audio', 1),
    (24000, 2, 24, '60', 'voip', 0), (48000, 2, 24, '20', 'voip', 1),
]
DECODER_TARGETS = ['fuzz_decoder', 'fuzz_packet_parsing', 'fuzz_silk_decoder', 'fuzz_celt_decoder_boundary',
                   'fuzz_multi_frame', 'fuzz_repacketizer', 'fuzz_celt_large_frame']
ENCODER_TARGETS = ['fuzz_encoder', 'fuzz_encoder_boundary', 'fuzz_hybrid_mode', 'fuzz_overflow',
                   'fuzz_celt_encoder', 'fuzz_silk_encoder', 'fuzz_roundtrip']


def packets(path):
    data, pos, out = open(path, 'rb').read(), 0, []
    while pos + 8 <= len(data):
        n = struct.unpack('>I', data[pos:pos + 4])[0]
        out.append(data[pos + 8:pos + 8 + n])
        pos += 8 + n
    return out


def write(target, name, blob):
    d = os.path.join(HERE, 'seeds', target)
    os.makedirs(d, exist_ok=True)
    open(os.path.join(d, name), 'wb').write(blob)


os.makedirs(os.path.join(HERE, 'seeds'), exist_ok=True)
tmp = os.path.join(HERE, 'seeds', '_tmp.bit')
count = 0
for rate, ch, kbps, fr, app, vbr in CONFIGS:
    env = dict(os.environ, **({'VBR': '1'} if vbr else {}))
    subprocess.run([ENC, str(rate), str(ch), str(kbps * 1000), fr, app,
                    os.path.join(PCM, f'in_{rate}_{ch}.sw'), tmp], env=env, check=True, capture_output=True)
    pk = packets(tmp)
    for k in (2, len(pk) // 2):  # one early, one mid-stream packet per config
        tag = f'{rate}_{ch}_{kbps}k_{fr}ms_{app}_{k}'
        for t in DECODER_TARGETS:
            write(t, tag, pk[k])
        write('fuzz_multistream_decoder', tag, bytes([(ch - 1) | ((rate // 4000) << 3) & 0x78]) + pk[k])
        count += 1
os.remove(tmp)

# Encoder seeds: 20 ms of real PCM (s16le) at several rates.
for rate, ch in [(8000, 1), (16000, 1), (24000, 2), (48000, 1), (48000, 2)]:
    raw = open(os.path.join(PCM, f'in_{rate}_{ch}.sw'), 'rb').read()
    n = rate // 50 * ch * 2
    for t in ENCODER_TARGETS:
        write(t, f'pcm_{rate}_{ch}', raw[n * 10:n * 11])
print(f'{count} packet seeds x {len(DECODER_TARGETS) + 1} decoder targets; PCM seeds for {len(ENCODER_TARGETS)} encoder targets')
