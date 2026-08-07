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

def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('--diff-only', default='')
    ap.add_argument('--baseline', default=BASELINE)
    a = ap.parse_args()

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
