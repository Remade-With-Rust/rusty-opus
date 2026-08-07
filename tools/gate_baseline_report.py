#!/usr/bin/env python3
"""Great Gate P0 deliverable: the per-class baseline table.

Reports, per corpus clip, our ODG ladder against libopus's over the OVERLAPPING
actual-bitrate range (BD-ODG: mean ODG delta at matched rate, positive = we
win). Comparing at nominal rate would hand libopus its 15-20% VBR overshoot for
free, so everything here is interpolated on log(actual kbps).

  python tools/gate_baseline_report.py [ladder.csv]
"""
import csv, os, sys
import numpy as np

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
path = sys.argv[1] if len(sys.argv) > 1 else os.path.join(ROOT, 'target', 'ladder_baseline.csv')

rows = []
with open(path, newline='', encoding='utf-8-sig') as f:
    for r in csv.DictReader(f):
        rows.append((r['clip'], r['arm'], float(r['actual_kbps']), float(r['odg'])))

clips = sorted({r[0] for r in rows})

def curve(clip, arm):
    pts = sorted((k, o) for c, a, k, o in rows if c == clip and a == arm)
    return np.log([p[0] for p in pts]), np.array([p[1] for p in pts])

print(f'{"clip":22s} {"our ODG range":>18s} {"lib ODG range":>18s} {"BD-ODG":>8s}  verdict')
deltas = {}
for clip in clips:
    ro, qo = curve(clip, 'ours')
    rl, ql = curve(clip, 'lib')
    if len(ro) < 2 or len(rl) < 2:
        continue
    lo, hi = max(ro[0], rl[0]), min(ro[-1], rl[-1])
    if hi <= lo:
        print(f'{clip:22s} {"NO RATE OVERLAP — cannot compare":>40s}')
        continue
    x = np.linspace(lo, hi, 200)
    d = float(np.mean(np.interp(x, ro, qo) - np.interp(x, rl, ql)))
    deltas[clip] = d
    verdict = 'WE WIN' if d > 0.05 else ('parity' if d > -0.05 else 'libopus ahead')
    print(f'{clip:22s} {qo[0]:+7.2f}..{qo[-1]:+6.2f}    {ql[0]:+7.2f}..{ql[-1]:+6.2f}   '
          f'{d:>+8.3f}  {verdict}')

if deltas:
    vals = list(deltas.values())
    worst = min(deltas, key=deltas.get)
    best = max(deltas, key=deltas.get)
    print(f'\nclasses: {len(vals)}   mean {np.mean(vals):+.3f}   '
          f'best {best} {deltas[best]:+.3f}   worst {worst} {deltas[worst]:+.3f}')
    print('Reminder: the campaign finish line is WORST CLASS <= 0, never the mean.')
