#!/usr/bin/env python3
"""line census: lines.py <file.cg> <file-substr> [topN] [fn-substr]"""
import sys, re, collections
path, fsub = sys.argv[1], sys.argv[2]; topn = int(sys.argv[3]) if len(sys.argv) > 3 else 30
fnsub = sys.argv[4] if len(sys.argv) > 4 else None
names = {}
def nm(tok):
    m = re.match(r'\((\d+)\)(?: (.*))?$', tok)
    if not m: return tok
    if m.group(2) is not None: names[m.group(1)] = m.group(2)
    return names.get(m.group(1), '?')
cost = collections.Counter(); fn = cur = None; last = 0; pending = False
for line in open(path, errors='replace'):
    line = line.rstrip('\n')
    if not line: continue
    c = line[0]
    if c.isdigit() or c in '+-*':
        p = line.split()
        if p[0] == '*': ln = last
        elif p[0][0] in '+-': ln = last + int(p[0])
        else: ln = int(p[0])
        last = ln
        if pending: pending = False; continue
        if len(p) > 1 and cur and fsub in cur and (not fnsub or fnsub in (fn or '')): cost[(cur, ln)] += int(p[1])
        continue
    k, _, v = line.partition('=')
    if k == 'fn': fn = nm(v)
    elif k in ('fl', 'fi', 'fe'): cur = nm(v)
    elif k in ('cfn', 'cfi', 'cfl', 'ob', 'cob'): nm(v)
    elif k == 'calls': pending = True
src = {}
tot = sum(cost.values()); print(f"file~{fsub} fn~{fnsub}: {tot:,} Ir")
for (f, ln), c in cost.most_common(topn):
    if f not in src:
        try: src[f] = open(f, errors='replace').read().split('\n')
        except Exception: src[f] = []
    t = src[f][ln-1].strip()[:110] if 0 < ln <= len(src[f]) else ''
    print(f"{c:>12,} {f.split('/')[-1]}:{ln:<5} {t}")
