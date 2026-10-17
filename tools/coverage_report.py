#!/usr/bin/env python3
"""Summarise target/coverage_matrix/matrix.json (written by coverage_matrix.py).

Classes, most severe first:
  RANGE     libopus decodes our packets with a range-coder mismatch (illegal stream)
  DECODER   our decoder != libopus decoder on our stream, beyond a pure delay
  DELAY     our decoder == libopus decoder, but time-shifted
  GARBAGE   our stream decodes, but libopus's output of it is far below libopus's own
  GAP       libopus encodes the cell, we refuse it
  ROUTING   modal TOC (mode/bw/duration) differs from libopus (informational)

  python3 tools/coverage_report.py [matrix.json]
"""
import collections, json, math, os, sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
path = sys.argv[1] if len(sys.argv) > 1 else os.path.join(ROOT, 'target', 'coverage_matrix', 'matrix.json')
rows = json.load(open(path))


def key(r):
    return f"{r['rate'] // 1000}k {r['ch']}ch {r['app']:<8} {r['frame']:>4}ms bw={r['bw']:<4} {r['kbps']}k"


cls = collections.defaultdict(list)
for r in rows:
    if 'harness_error' in r:
        cls['HARNESS'].append(r)
        continue
    if not r.get('lib_ok') and not r.get('ours_ok'):
        cls['BOTH_REFUSE'].append(r)
        continue
    if not r.get('ours_ok'):
        cls['GAP' if r.get('lib_ok') else 'OURS_ONLY_FAIL'].append(r)
        continue
    if not r.get('range_ok'):
        cls['RANGE'].append(r)
    if not r.get('ourdec_ok'):
        cls['OURDEC'].append(r)
    md = r.get('dec_maxdiff', 0)
    if md > 2:
        lag = r.get('dec_lagdiff')
        if lag and (lag[1] <= 2 or lag[3] <= 2):
            cls['DELAY'].append(r)
        else:
            cls['DECODER'].append(r)
    so, sl = r.get('snr_ours'), r.get('snr_lib')
    if isinstance(so, (int, float)) and not math.isnan(so):
        if so < 1.0 and (not isinstance(sl, (int, float)) or math.isnan(sl) or sl > so + 6):
            cls['GARBAGE'].append(r)
    to, tl = r.get('toc_ours'), r.get('toc_lib')
    if to and tl and list(to) != list(tl):
        cls['ROUTING'].append(r)
    cls['PASS_CORE' if r.get('range_ok') and md <= 2 else 'x'].append(r)

print(f'{len(rows)} cells')
for c in ['HARNESS', 'RANGE', 'DECODER', 'OURDEC', 'DELAY', 'GARBAGE', 'GAP', 'OURS_ONLY_FAIL',
          'BOTH_REFUSE', 'ROUTING', 'PASS_CORE']:
    print(f'  {c:<15} {len(cls[c])}')

def show(c, fmt, limit=40):
    if not cls[c]:
        return
    print(f'\n=== {c} ===')
    for r in cls[c][:limit]:
        print('  ' + key(r) + '  ' + fmt(r))
    if len(cls[c]) > limit:
        print(f'  ... +{len(cls[c]) - limit}')

show('HARNESS', lambda r: r['harness_error'])
show('RANGE', lambda r: r.get('range_msg', ''))
show('DECODER', lambda r: f"maxdiff={r.get('dec_maxdiff')} first_bad={r.get('dec_first_bad')} lag={r.get('dec_lagdiff')} len={r.get('dec_len')}")
show('OURDEC', lambda r: '')
show('GARBAGE', lambda r: f"snr ours={r.get('snr_ours')} lib={r.get('snr_lib')} toc={r.get('toc_ours')}")

# GAP grouped by reason x frame (the shape of the gap, not 1000 lines)
g = collections.Counter((r['ours_err'], r['frame']) for r in cls['GAP'])
if g:
    print('\n=== GAP (libopus encodes, we refuse) by error x frame ===')
    for (e, f), n in sorted(g.items(), key=lambda x: (x[0][1], -x[1])):
        print(f'  {f:>4}ms  {n:4d}  {e}')
d = collections.Counter((r['rate'], r['dec_lagdiff'][2] if r.get('dec_lagdiff') else None,
                         r['toc_ours'][0] if r.get('toc_ours') else None) for r in cls['DELAY'])
if d:
    print('\n=== DELAY (bit-exact after shift) by rate x shift x mode ===')
    for k, n in sorted(d.items(), key=str):
        print(f'  {k}  {n}')
# routing confusion
rc = collections.Counter((tuple(r['toc_lib']), tuple(r['toc_ours'])) for r in cls['ROUTING'])
if rc:
    print('\n=== ROUTING lib -> ours (top 25) ===')
    for (l, o), n in rc.most_common(25):
        print(f'  {n:4d}  lib {l} -> ours {o}')
# forced-bandwidth honoured?
fb = collections.Counter()
for r in rows:
    if r.get('ours_ok') and r['bw'] != 'auto' and r.get('toc_ours') and r.get('toc_lib'):
        fb[(r['bw'], r['toc_ours'][1] == r['toc_lib'][1])] += 1
if fb:
    print('\n=== forced bandwidth: coded bw matches libopus? ===')
    for b in ['nb', 'mb', 'wb', 'swb', 'fb']:
        print(f'  {b:<4} match {fb[(b, True)]:4d}  differ {fb[(b, False)]:4d}')
