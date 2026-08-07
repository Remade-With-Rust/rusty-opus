#!/usr/bin/env python3
"""Great Gate P2: build calculator harvest CSVs from the truth-table ladders.

Joins, per (clip, rate):
  - ladder_forced.csv  (tags forced-silk/celt/hybrid, arm=ours)  -> arm ODG
  - ladder_baseline.csv (tag baseline-*, arm=ours)               -> shipped ODG
  - gate_harvest_baseline.csv (per-frame tap rows)               -> signal means

Emits one calculator CSV per candidate arm (gate = "force <arm> instead of the
shipped auto choice"):  gain = ODG(forced arm) - ODG(shipped), unit = clip x
rate, clip = corpus clip, features = decision-time signal means + rate.
Split: stable name-keyed (sha1(clip) even/odd) so branch and leaf fits share it.

The speed pair (`work`, `cpu_ms`) is joined from target/p2_speedpair.csv when
present (produced by tools/gate_speedpair.ps1), differenced against arm=auto so
positive = work/time SAVED by firing the gate. Without that file the columns are
omitted and the calculator (correctly) marks the run HYPOTHESES ONLY — that
downgrade is the instrument audit working, not a gap to paper over.
"""
import csv, hashlib, os, sys
from collections import defaultdict

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
T = os.path.join(ROOT, 'target')

def load_ladder(path):
    rows = {}
    with open(path, newline='', encoding='utf-8-sig') as f:
        for r in csv.DictReader(f):
            if r['arm'] != 'ours':
                continue
            rows[(r['clip'], int(r['rate_kbps']), r['tag'])] = float(r['odg'])
    return rows

def signal_means(path):
    acc = defaultdict(lambda: defaultdict(float))
    cnt = defaultdict(int)
    with open(path, newline='', encoding='utf-8-sig') as f:
        for r in csv.DictReader(f):
            key = (r['clip'], round(int(r['bitrate']) / 1000))
            cnt[key] += 1
            a = acc[key]
            a['music_prob'] += float(r['music_prob'])
            a['tonality'] += float(r['tonality'])
            a['activity_prob'] += float(r['activity_prob'])
            a['voice_est'] += float(r['voice_est'])
            a['det_bw_fb'] += 1.0 if r['det_bw'] == '1105' else 0.0
            a['silk_frac'] += 1.0 if r['mode'] == 'silk' else 0.0
            a['hybrid_frac'] += 1.0 if r['mode'] == 'hybrid' else 0.0
    out = {}
    for key, a in acc.items():
        out[key] = {k: v / cnt[key] for k, v in a.items()}
    return out

def clip_split(clip, all_clips):
    """Stable name-keyed split that GUARANTEES both sides are populated.

    A pure hash split is stable but can land every clip on one side (it did:
    3 clips, all 'train'), and the calculator then correctly refuses the
    both-splits gate. Ranking the hashes and halving keeps the split stable
    per clip-set while making it usable. Both the branch fit and any symbolic
    leaf must share this function (great-gate.md §4, "same split, both halves").
    """
    ordered = sorted(all_clips, key=lambda c: hashlib.sha1(c.encode()).hexdigest())
    return 'train' if ordered.index(clip) < (len(ordered) + 1) // 2 else 'holdout'

def load_speedpair(path):
    """{(clip, rate, arm): (work, cpu_ms)} — raw per-arm cost."""
    if not os.path.exists(path):
        return {}
    out = {}
    with open(path, newline='', encoding='utf-8-sig') as f:
        for r in csv.DictReader(f):
            try:
                out[(r['clip'], int(r['rate_kbps']), r['arm'])] = (
                    float(r['work']), float(r['cpu_ms']))
            except ValueError:
                pass  # 'NA' from a failed probe: drop loudly below, never as 0
    return out

def main():
    forced = load_ladder(os.path.join(T, 'ladder_forced.csv'))
    base = load_ladder(os.path.join(T, 'ladder_baseline.csv'))
    sig = signal_means(os.path.join(T, 'gate_harvest_baseline.csv'))
    speed = load_speedpair(os.path.join(T, 'p2_speedpair.csv'))
    shipped = {(c, r): odg for (c, r, tag), odg in base.items() if tag.startswith('baseline')}
    if not speed:
        print('no p2_speedpair.csv — emitting quality-only (calculator will say '
              'HYPOTHESES ONLY)', file=sys.stderr)

    all_clips = sorted({c for (c, _r, _t) in forced})
    for arm in ('silk', 'celt', 'hybrid'):
        outp = os.path.join(T, f'p2_harvest_force_{arm}.csv')
        n = 0
        with open(outp, 'w', newline='') as f:
            w = csv.writer(f)
            head = ['clip', 'split', 'gain', 'clip_total', 'rate_kbps', 'music_prob', 'tonality',
                    'activity_prob', 'voice_est', 'det_bw_fb', 'silk_frac', 'hybrid_frac']
            if speed:
                head += ['work', 'cpu_ms']
            w.writerow(head)
            for (clip, rate, tag), odg in sorted(forced.items()):
                if tag != f'forced-{arm}' or (clip, rate) not in shipped:
                    continue
                s = sig.get((clip, rate))
                if s is None:
                    print(f'  no harvest signals for {clip}@{rate} — skipped', file=sys.stderr)
                    continue
                split = clip_split(clip, all_clips)
                gain = odg - shipped[(clip, rate)]
                # clip_total: the clip's own metric mass, so the calculator can
                # form the MACRO (per-clip) aggregation the ladder actually
                # reports. Each rung contributes equally within its clip, so the
                # mass is just the rung count.
                clip_total = sum(1 for (c, _r, t) in forced if c == clip and t == f'forced-{arm}')
                row = [clip, split, f'{gain:.4f}', clip_total, rate,
                       f"{s['music_prob']:.4f}", f"{s['tonality']:.4f}",
                       f"{s['activity_prob']:.4f}", f"{s['voice_est']:.1f}",
                       f"{s['det_bw_fb']:.4f}", f"{s['silk_frac']:.4f}",
                       f"{s['hybrid_frac']:.4f}"]
                if speed:
                    a, b = speed.get((clip, rate, arm)), speed.get((clip, rate, 'auto'))
                    if a is None or b is None:
                        print(f'  MISSING speed pair for {clip}@{rate} {arm} — row dropped '
                              '(a missing sample is not a tie)', file=sys.stderr)
                        continue
                    # positive = saved by firing the gate
                    row += [f'{b[0] - a[0]:.0f}', f'{b[1] - a[1]:.3f}']
                w.writerow(row)
                n += 1
        print(f'{outp}: {n} rows')

    truth_table(forced, shipped, sig, speed)

def truth_table(forced, shipped, sig, speed):
    """P1 deliverable: the per-class truth table — for each (clip, rate), the
    ODG of every forced arm against the shipped auto choice. The sign pattern
    ACROSS classes is the dispatch signal (great-gate.md: a negative outcome on
    one class is a dispatch, not a result)."""
    units = sorted({(c, r) for (c, r, _t) in forced})
    if not units:
        print('\nno forced-arm rows yet — truth table skipped'); return
    arms = ['silk', 'celt', 'hybrid']
    print('\n=== P1 truth table: dODG of each forced arm vs the shipped auto mode ===')
    print('(positive = that arm BEATS the shipped decision on this unit)')
    hdr = f'{"clip":22s} {"rate":>5s} {"shipped":>8s} ' + ' '.join(f'{a:>9s}' for a in arms)
    print(hdr + f' {"auto mode":>10s} {"det_bw=FB":>10s}')
    for clip, rate in units:
        if (clip, rate) not in shipped:
            continue
        base = shipped[(clip, rate)]
        cells = []
        for a in arms:
            odg = forced.get((clip, rate, f'forced-{a}'))
            cells.append(f'{odg - base:>+9.3f}' if odg is not None else f'{"—":>9s}')
        s = sig.get((clip, rate), {})
        mode = ('silk' if s.get('silk_frac', 0) > 0.5 else
                'hybrid' if s.get('hybrid_frac', 0) > 0.5 else 'celt')
        print(f'{clip:22s} {rate:>4}k {base:>+8.3f} ' + ' '.join(cells) +
              f' {mode:>10s} {s.get("det_bw_fb", float("nan")):>10.2f}')
    if speed:
        print('\n=== speed pair (work = stage calls, cpu_ms = pinned best-of-N; '
              'raw per arm) ===')
        for (clip, rate, arm), (wk, ms) in sorted(speed.items()):
            print(f'{clip:22s} {rate:>4}k {arm:>7s}  work={wk:>10.0f}  cpu_ms={ms:>8.3f}')

if __name__ == '__main__':
    main()
