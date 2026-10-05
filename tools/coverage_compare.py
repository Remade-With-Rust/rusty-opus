#!/usr/bin/env python3
"""RFC 6716 conformance gate for the matrix's DECODER cells (our decoder vs
libopus decoder differ by >2 LSB on OUR stream). Float decoders are not
required to be bit-exact; the normative criterion is `opus_compare`: libopus's
48 kHz decode is the reference, ours (at the cell rate) is the test.

Usage: OPUS_DEMO=... OPUS_COMPARE=... python tools/coverage_compare.py [matrix.json]
"""
import json, os, subprocess, sys
HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
CELLS = os.path.join(ROOT, 'target', 'coverage_matrix', 'cells')
EX = os.path.join(ROOT, 'target', 'release', 'examples')
DEC = os.environ.get('DEC_BIN', os.path.join(EX, 'decode_bit.exe' if os.name == 'nt' else 'decode_bit'))
DEMO, CMP = os.environ['OPUS_DEMO'], os.environ['OPUS_COMPARE']
path = sys.argv[1] if len(sys.argv) > 1 else os.path.join(ROOT, 'target', 'coverage_matrix', 'matrix.json')
rows = json.load(open(path))
bad = [r for r in rows if r.get('ours_ok') and r.get('range_ok') and r.get('dec_maxdiff', 0) > 2]
npass = 0
for r in bad:
    tag = f"{r['rate']}_{r['ch']}_{r['app']}_{r['frame']}_{r['bw']}_{r['kbps']}"
    bit = os.path.join(CELLS, tag + '.ours.bit')
    ref = os.path.join(CELLS, tag + '.ref48.sw')
    # opus_compare always reads the reference as 48 kHz STEREO (downmixing it
    # itself for a mono test), as the RFC test vectors are.
    subprocess.run([DEMO, '-d', '48000', '2', bit, ref], capture_output=True)
    test = os.path.join(CELLS, tag + '.oo.cmp.sw')
    subprocess.run([DEC, str(r['rate']), str(r['ch']), bit, test], capture_output=True)
    a = [CMP] + (['-s'] if r['ch'] == 2 else []) + ['-r', str(r['rate']), ref, test]
    p = subprocess.run(a, capture_output=True, text=True)
    out = (p.stdout + p.stderr).strip().splitlines()
    ok = p.returncode == 0 and any('PASS' in l for l in out)
    npass += ok
    print(f"{'PASS' if ok else 'FAIL'}  {tag:32s} maxdiff={r['dec_maxdiff']:5d}  {out[-1] if out else ''}")
print(f'\nopus_compare: {npass}/{len(bad)} DECODER cells conform')
