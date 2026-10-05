#!/usr/bin/env python3
"""Per-cell transitions between two coverage_matrix.py runs (before -> after).

  python3 tools/coverage_delta.py target/coverage_matrix/matrix_baseline.json target/coverage_matrix/matrix_fixed.json
"""
import collections, json, sys

def status(r):
    if 'harness_error' in r: return 'harness'
    if not r.get('ours_ok'): return 'gap'
    if not r.get('range_ok'): return 'range'
    md = r.get('dec_maxdiff', 0)
    if md > 2:
        lag = r.get('dec_lagdiff')
        return 'delay' if lag and (lag[1] <= 2 or lag[3] <= 2) else 'decoder'
    return 'PASS'

k = lambda r: (r['rate'], r['ch'], r['app'], r['frame'], r['bw'], r['kbps'])
a = {k(r): r for r in json.load(open(sys.argv[1]))}
b = {k(r): r for r in json.load(open(sys.argv[2]))}
t = collections.Counter((status(a[x]), status(b[x])) for x in a if x in b)
for s in ('before', 'after'):
    src = a if s == 'before' else b
    c = collections.Counter(status(r) for r in src.values())
    print(f'{s:<7} ' + '  '.join(f'{n}={c[n]}' for n in ('PASS', 'delay', 'decoder', 'range', 'gap', 'harness')))
print('\ntransitions (before -> after):')
for (x, y), n in sorted(t.items(), key=lambda z: -z[1]):
    if x != y: print(f'  {x:>8} -> {y:<8} {n}')
reg = [x for x in a if x in b and status(a[x]) == 'PASS' and status(b[x]) != 'PASS']
print(f'\nREGRESSIONS (PASS -> not PASS): {len(reg)}')
for x in reg[:30]: print('  ', x, status(b[x]), b[x].get('range_msg', '')[:80])
