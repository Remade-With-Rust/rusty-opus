#!/usr/bin/env python3
"""Report the mode/bandwidth histogram of an Opus stream from its TOC bytes.

The TOC's top 5 bits (config) declare mode + bandwidth for every packet, so a
histogram over a whole file says exactly how an encoder routed the content —
the cheapest possible cross-encoder comparison of a mode decision, and it needs
no instrumentation of the other encoder.

  python tools/opus_toc_stats.py file.opus [file2.opus ...]

Accepts Ogg-encapsulated Opus (skips the OpusHead/OpusTags pages).
"""
import sys, struct, collections

def ogg_packets(data):
    """Yield raw Opus packets from an Ogg stream."""
    pos, pending = 0, b''
    while pos + 27 <= len(data):
        if data[pos:pos + 4] != b'OggS':
            pos += 1
            continue
        nseg = data[pos + 26]
        segs = data[pos + 27:pos + 27 + nseg]
        body = pos + 27 + nseg
        for s in segs:
            pending += data[body:body + s]
            body += s
            if s < 255:
                yield pending
                pending = b''
        pos = body

def describe(config):
    if config < 12:
        mode = 'silk'
        bw = ['NB', 'MB', 'WB'][config // 4]
    elif config < 16:
        mode = 'hybrid'
        bw = ['SWB', 'FB'][(config - 12) // 2]
    else:
        mode = 'celt'
        bw = ['NB', 'WB', 'SWB', 'FB'][(config - 16) // 4]
    return f'{mode}/{bw}'

for path in sys.argv[1:]:
    data = open(path, 'rb').read()
    hist = collections.Counter()
    n = 0
    for pkt in ogg_packets(data):
        if not pkt or pkt[:8] in (b'OpusHead', b'OpusTags') or pkt[:8].startswith(b'OpusTag'):
            continue
        hist[describe(pkt[0] >> 3)] += 1
        n += 1
    top = ', '.join(f'{k} {v * 100 // max(n, 1)}%' for k, v in hist.most_common(4))
    print(f'{path}: {n} packets  {top}')
