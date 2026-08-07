#!/usr/bin/env python3
"""Great Gate P4: the one-run regression harness.

Re-runs the per-clip ladder (ours arm) on the gate corpus and diffs it against
the banked baseline per (clip, rate). CI rule: NO SIGN-FLIP on any tracked
class — a clip whose ODG regresses beyond the noise band on any rung fails the
run (worst class <= 0, never on average).

Usage:
  python tools/gate_regression.py                    # run ladder + diff
  python tools/gate_regression.py --diff-only new.csv  # diff an existing run
Exit codes: 0 clean, 1 regression (sign-flip), 2 harness error.

Noise band: |dODG| <= 0.05 counts as neutral (PEAQ_python rescoring jitter is
~0.01-0.03 on identical files; 0.05 adds margin — tighten once a null-arm
rescore distribution is banked).
"""
import argparse, csv, os, subprocess, sys

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
BASELINE = os.path.join(ROOT, 'target', 'ladder_baseline.csv')
NOISE = 0.05

def load(path, arm='ours'):
    rows = {}
    with open(path, newline='', encoding='utf-8-sig') as f:
        for r in csv.DictReader(f):
            if r['arm'] == arm:
                rows[(r['clip'], int(r['rate_kbps']))] = float(r['odg'])
    return rows

def load_curves(path, arm='ours'):
    """{clip: [(actual_kbps, odg), ...]} sorted by rate — for the BD comparison."""
    c = {}
    with open(path, newline='', encoding='utf-8-sig') as f:
        for r in csv.DictReader(f):
            if r['arm'] == arm:
                c.setdefault(r['clip'], []).append((float(r['actual_kbps']), float(r['odg'])))
    return {k: sorted(v) for k, v in c.items()}

def bd_compare(base_path, new_path):
    """Per-class BD-ODG: mean ODG delta over the OVERLAPPING actual-bitrate range.

    The per-rung view is wrong for any change that moves the bitrate — a brick
    that spends fewer bits at the same nominal target (the CELT silence flag) or
    more (the tonality VBR boost) would be scored on the bits it moved rather
    than on its efficiency. Interpolating both curves on log(actual kbps) prices
    the change at matched rate, which is the question that actually matters.
    """
    import math
    base, new = load_curves(base_path), load_curves(new_path)
    print(f'{"clip":22s} {"rate range kbps":>18s} {"BD-ODG":>9s}  verdict')
    worst, results = None, {}
    for clip in sorted(set(base) & set(new)):
        rb, rn = base[clip], new[clip]
        if len(rb) < 2 or len(rn) < 2:
            continue
        lo = max(rb[0][0], rn[0][0])
        hi = min(rb[-1][0], rn[-1][0])
        if hi <= lo:
            print(f'{clip:22s} {"NO RATE OVERLAP":>18s}      — skipped')
            continue
        xs = [math.exp(math.log(lo) + (math.log(hi) - math.log(lo)) * i / 199)
              for i in range(200)]
        def interp(curve, x):
            for i in range(1, len(curve)):
                if curve[i][0] >= x:
                    (x0, y0), (x1, y1) = curve[i - 1], curve[i]
                    t = 0.0 if x1 == x0 else (math.log(x / x0) / math.log(x1 / x0))
                    return y0 + t * (y1 - y0)
            return curve[-1][1]
        d = sum(interp(rn, x) - interp(rb, x) for x in xs) / len(xs)
        results[clip] = d
        if worst is None or d < worst[1]:
            worst = (clip, d)
        verdict = 'WIN' if d > NOISE else ('neutral' if d > -NOISE else 'REGRESSION')
        print(f'{clip:22s} {lo:7.1f}..{hi:7.1f}  {d:>+9.3f}  {verdict}')
    if results:
        vals = list(results.values())
        print(f'\nclasses {len(vals)}  mean {sum(vals)/len(vals):+.3f}  '
              f'worst {worst[0]} {worst[1]:+.3f}')
        if worst[1] < -NOISE:
            print(f'FAIL: {worst[0]} regressed {worst[1]:+.3f} at matched bitrate')
            return 1
        print('PASS: no class regressed at matched bitrate')
    return 0

def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('--diff-only', default='')
    ap.add_argument('--baseline', default=BASELINE)
    # Use for any brick that MOVES the bitrate; per-rung ODG would price the
    # bits it moved instead of the efficiency it gained.
    ap.add_argument('--bd', action='store_true',
                    help='rate-matched per-class BD-ODG instead of per-rung deltas')
    a = ap.parse_args()

    if a.bd:
        if not a.diff_only:
            sys.exit('--bd needs --diff-only <ladder.csv>')
        sys.exit(bd_compare(a.baseline, a.diff_only))

    if a.diff_only:
        new_csv = a.diff_only
    else:
        new_csv = os.path.join(ROOT, 'target', 'ladder_regression.csv')
        if os.path.exists(new_csv):
            os.remove(new_csv)
        r = subprocess.run([sys.executable, os.path.join(HERE, 'gate_ladder.py'),
                            '--arms', 'ours', '--out', new_csv, '--tag', 'regression'])
        if r.returncode != 0:
            sys.exit(2)

    base = load(a.baseline)
    new = load(new_csv)
    if not base or not new:
        print('missing baseline or new rows'); sys.exit(2)

    worst = {}
    missing = []
    for key, b_odg in sorted(base.items()):
        if key not in new:
            missing.append(key)
            continue
        d = new[key] - b_odg
        clip = key[0]
        if clip not in worst or d < worst[clip][1]:
            worst[clip] = (key[1], d)
    print(f'{"clip":22s} {"worst rung":>10s} {"dODG":>8s}  verdict')
    fails = 0
    for clip, (rate, d) in sorted(worst.items()):
        verdict = 'OK' if d >= -NOISE else 'REGRESSION'
        if d < -NOISE: fails += 1
        print(f'{clip:22s} {rate:>9}k {d:>+8.3f}  {verdict}')
    if missing:
        print(f'WARNING: {len(missing)} baseline points missing from new run: {missing[:5]}')
    if fails:
        print(f'\nFAIL: {fails} class(es) regressed beyond the {NOISE} noise band — sign-flip law violated')
        sys.exit(1)
    print('\nPASS: no class regressed beyond the noise band')

if __name__ == '__main__':
    main()
